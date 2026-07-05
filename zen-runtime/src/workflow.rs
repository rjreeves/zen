use crate::capabilities::CapabilityGrant;
use crate::effects::{Effect, Effects, ProcessEffects};
use crate::events::{Event, EventSink};
use crate::process::{parse_duration, ExecRequest};
use crate::values::{
    eq_vals, json_to_value, secret_reference_name, value_to_echo_string, value_to_json, Value,
};
use crate::workflow_host::WorkflowHost;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

struct WorkflowSpec {
    name: String,
    steps: Vec<WorkflowStep>,
}

struct WorkflowStep {
    name: String,
    command: WorkflowStepCommand,
    condition: Option<WorkflowCondition>,
    timeout: Option<Duration>,
    env: HashMap<String, WorkflowEnvValue>,
    save_as: Option<String>,
    artifacts: Vec<WorkflowArtifactSpec>,
    retry: RetryPolicy,
    on_failure: Vec<WorkflowAction>,
    rollback: Vec<WorkflowAction>,
    finally: Vec<WorkflowAction>,
    checkpoint: Option<String>,
}

/// A workflow step's env value is either a literal string or a symbolic
/// reference to a secret by name (YAML `{ secret: "name" }`). The secret
/// variant carries only the name until it's resolved from the trusted
/// secret store right before spawning the child process - the plaintext
/// value never gets stored on `WorkflowStep` or persisted anywhere.
enum WorkflowEnvValue {
    Literal(String),
    Secret(String),
}

#[derive(Clone)]
struct WorkflowArtifactSpec {
    name: Option<String>,
    path: String,
}

enum WorkflowStepCommand {
    Shell(String),
    Zen(String),
}

struct WorkflowStepRunResult {
    output: Value,
    succeeded: bool,
    error: Option<String>,
}

struct RetryPolicy {
    attempts: usize,
    delay: Duration,
}

struct WorkflowCondition {
    left: Vec<String>,
    op: WorkflowConditionOp,
    right: Value,
}

enum WorkflowConditionOp {
    Eq,
    Neq,
}

#[derive(Clone)]
struct CompletedWorkflowStep {
    name: String,
    rollback: Vec<WorkflowAction>,
    checkpoint: Option<String>,
    output: Value,
}

#[derive(Clone)]
enum WorkflowAction {
    Run(String),
    Emit(String),
}

struct WorkflowPersistence {
    conn: Connection,
    run_id: String,
    resume_checkpoints: HashSet<String>,
    resume_step_outputs: HashMap<String, Value>,
}

impl EventSink for WorkflowPersistence {
    fn emit(&mut self, event: &Event) -> Result<(), String> {
        self.conn
            .execute(
                "insert into workflow_events (run_id, event, workflow, step, attempt, created_at)
                 values (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    self.run_id,
                    event.name,
                    event.workflow,
                    event.step,
                    event.attempt.map(|attempt| attempt as i64),
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(|error| format!("Failed to save workflow event: {}", error))?;
        Ok(())
    }
}

/// Runs `workflow.run` workflows against a `WorkflowHost` instead of the
/// concrete `.fg` interpreter. Holds a single `&mut dyn WorkflowHost`
/// (rather than separate `ScriptRunner`/`SecretStore`/permission handles)
/// because those are all implemented by the same host object, and Rust
/// won't let us hold three aliasing `&mut` borrows of it at once.
pub struct WorkflowEngine<'a> {
    host: &'a mut dyn WorkflowHost,
}

impl<'a> WorkflowEngine<'a> {
    pub fn new(host: &'a mut dyn WorkflowHost) -> Self {
        Self { host }
    }

    pub fn run(&mut self, value: Value) -> Result<Value, String> {
        let spec = workflow_spec_from_value(value)?;
        self.workflow_execute_spec(spec, None)
    }

    pub fn run_persisted(&mut self, value: Value, source: &str) -> Result<Value, String> {
        let spec = workflow_spec_from_value(value)?;
        let mut persistence = self.workflow_persistence(&spec.name, source, None)?;
        self.workflow_execute_spec(spec, Some(&mut persistence))
    }

    pub fn resume_persisted(
        &mut self,
        value: Value,
        source: &str,
        run_id: &str,
    ) -> Result<Value, String> {
        let spec = workflow_spec_from_value(value)?;
        let mut persistence = self.workflow_persistence(&spec.name, source, Some(run_id))?;
        self.workflow_execute_spec(spec, Some(&mut persistence))
    }

    fn workflow_execute_spec(
        &mut self,
        spec: WorkflowSpec,
        mut persistence: Option<&mut WorkflowPersistence>,
    ) -> Result<Value, String> {
        let mut events = Vec::new();
        let mut step_results = Vec::new();
        let mut rollback_stack = Vec::new();
        let mut outputs = HashMap::new();
        let mut artifacts = Vec::new();
        let mut workflow_success = true;

        push_workflow_event(
            &mut events,
            persistence.as_deref_mut(),
            "workflow.started",
            &spec.name,
            None,
            None,
        )?;

        for step in spec.steps {
            if !workflow_condition_matches(step.condition.as_ref(), &outputs)? {
                push_workflow_event(
                    &mut events,
                    persistence.as_deref_mut(),
                    "step.skipped",
                    &spec.name,
                    Some(&step.name),
                    None,
                )?;
                step_results.push(workflow_step_result(
                    &step.name,
                    "skipped",
                    0,
                    Value::Null,
                    None,
                ));
                persist_workflow_step(
                    persistence.as_deref_mut(),
                    &step.name,
                    "skipped",
                    0,
                    step.checkpoint.as_deref(),
                    step_results.last().unwrap(),
                    None,
                )?;
                continue;
            }

            if step.checkpoint.as_ref().is_some_and(|checkpoint| {
                persistence
                    .as_ref()
                    .is_some_and(|persistence| persistence.resume_checkpoints.contains(checkpoint))
            }) {
                push_workflow_event(
                    &mut events,
                    persistence.as_deref_mut(),
                    "step.skipped",
                    &spec.name,
                    Some(&step.name),
                    None,
                )?;
                let resumed_output = persistence
                    .as_ref()
                    .and_then(|persistence| persistence.resume_step_outputs.get(&step.name))
                    .map(workflow_step_output_from_result)
                    .unwrap_or(Value::Null);
                step_results.push(workflow_step_result(
                    &step.name,
                    "skipped",
                    0,
                    resumed_output.clone(),
                    None,
                ));
                record_workflow_output(&mut outputs, &step, resumed_output);
                artifacts.extend(self.workflow_artifact_summaries(&step)?);
                rollback_stack.push(CompletedWorkflowStep {
                    name: step.name,
                    rollback: step.rollback,
                    checkpoint: step.checkpoint,
                    output: step_results.last().cloned().unwrap_or(Value::Null),
                });
                continue;
            }

            push_workflow_event(
                &mut events,
                persistence.as_deref_mut(),
                "step.started",
                &spec.name,
                Some(&step.name),
                None,
            )?;
            persist_workflow_step(
                persistence.as_deref_mut(),
                &step.name,
                "running",
                0,
                step.checkpoint.as_deref(),
                &Value::Null,
                None,
            )?;

            let mut status = "failed".to_string();
            let mut last_output = Value::Null;
            let mut error = None;
            let mut attempt_count = 0usize;

            for attempt in 1..=step.retry.attempts {
                attempt_count = attempt;
                let run_result = self.workflow_run_step(&step)?;
                last_output = run_result.output;
                error = run_result.error;

                if run_result.succeeded {
                    status = "succeeded".into();
                    push_workflow_event(
                        &mut events,
                        persistence.as_deref_mut(),
                        "step.succeeded",
                        &spec.name,
                        Some(&step.name),
                        Some(attempt),
                    )?;
                    break;
                }

                if attempt < step.retry.attempts {
                    status = "retrying".into();
                    push_workflow_event(
                        &mut events,
                        persistence.as_deref_mut(),
                        "step.retrying",
                        &spec.name,
                        Some(&step.name),
                        Some(attempt),
                    )?;
                    persist_workflow_step(
                        persistence.as_deref_mut(),
                        &step.name,
                        "retrying",
                        attempt,
                        step.checkpoint.as_deref(),
                        &last_output,
                        error.as_deref(),
                    )?;
                    if !step.retry.delay.is_zero() {
                        thread::sleep(step.retry.delay);
                    }
                }
            }

            if status != "succeeded" {
                workflow_success = false;
                status = "failed".into();
                push_workflow_event(
                    &mut events,
                    persistence.as_deref_mut(),
                    "step.failed",
                    &spec.name,
                    Some(&step.name),
                    Some(attempt_count),
                )?;
                for action in &step.on_failure {
                    if let Err(action_error) = self.workflow_run_action(
                        action,
                        &spec.name,
                        &step.name,
                        &mut events,
                        persistence.as_deref_mut(),
                    ) {
                        error = Some(action_error);
                    }
                }
                for action in &step.rollback {
                    if let Err(action_error) = self.workflow_run_rollback_action(
                        action,
                        &spec.name,
                        &step.name,
                        &mut events,
                        persistence.as_deref_mut(),
                    ) {
                        error = Some(action_error);
                    }
                }
                if let Err(action_error) = self.workflow_rollback_completed_steps(
                    &spec.name,
                    &mut rollback_stack,
                    &mut step_results,
                    &mut events,
                    persistence.as_deref_mut(),
                ) {
                    error = Some(action_error);
                }
            }

            for action in &step.finally {
                if let Err(action_error) = self.workflow_run_action(
                    action,
                    &spec.name,
                    &step.name,
                    &mut events,
                    persistence.as_deref_mut(),
                ) {
                    workflow_success = false;
                    status = "failed".into();
                    error = Some(action_error);
                }
            }

            step_results.push(workflow_step_result(
                &step.name,
                &status,
                attempt_count,
                last_output.clone(),
                error,
            ));
            let persisted_output = step_results.last().cloned().unwrap_or(Value::Null);
            persist_workflow_step(
                persistence.as_deref_mut(),
                &step.name,
                &status,
                attempt_count,
                step.checkpoint.as_deref(),
                &persisted_output,
                None,
            )?;

            if status == "succeeded" {
                record_workflow_output(&mut outputs, &step, last_output);
                artifacts.extend(self.workflow_artifact_summaries(&step)?);
                rollback_stack.push(CompletedWorkflowStep {
                    name: step.name,
                    rollback: step.rollback,
                    checkpoint: step.checkpoint,
                    output: persisted_output,
                });
            }

            if status != "succeeded" {
                break;
            }
        }

        push_workflow_event(
            &mut events,
            persistence.as_deref_mut(),
            if workflow_success {
                "workflow.completed"
            } else {
                "workflow.failed"
            },
            &spec.name,
            None,
            None,
        )?;

        let mut output = HashMap::new();
        output.insert("name".into(), Value::String(spec.name));
        output.insert("success".into(), Value::Bool(workflow_success));
        output.insert(
            "status".into(),
            Value::String(
                if workflow_success {
                    "completed"
                } else {
                    "failed"
                }
                .into(),
            ),
        );
        output.insert("steps".into(), Value::List(step_results));
        output.insert("outputs".into(), Value::Object(outputs));
        output.insert("artifacts".into(), Value::List(artifacts));
        output.insert("events".into(), Value::List(events));
        if let Some(persistence) = persistence {
            persist_workflow_run_status(
                &persistence.conn,
                &persistence.run_id,
                if workflow_success {
                    "completed"
                } else {
                    "failed"
                },
            )?;
            output.insert("run_id".into(), Value::String(persistence.run_id.clone()));
        }

        Ok(Value::Object(output))
    }

    fn workflow_persistence(
        &self,
        workflow_name: &str,
        source: &str,
        run_id: Option<&str>,
    ) -> Result<WorkflowPersistence, String> {
        let runtime_dir = self.host.workspace_root_path().join(".zen");
        fs::create_dir_all(&runtime_dir).map_err(|error| {
            format!(
                "Failed to create runtime directory '{}': {}",
                runtime_dir.display(),
                error
            )
        })?;
        let db_path = runtime_dir.join("runtime.db");
        let conn = Connection::open(&db_path)
            .map_err(|error| format!("Failed to open '{}': {}", db_path.display(), error))?;
        init_workflow_db(&conn)?;

        let run_id = if let Some(run_id) = run_id {
            conn.query_row(
                "select id from workflow_runs where id = ?1",
                params![run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("Failed to query workflow run: {}", error))?
            .ok_or_else(|| format!("Workflow run '{}' was not found", run_id))?
        } else {
            format!(
                "{}-{}",
                workflow_slug(workflow_name),
                Utc::now().timestamp_millis()
            )
        };

        conn.execute(
            "insert into workflow_runs (id, name, source, status, created_at, updated_at)
             values (?1, ?2, ?3, 'running', ?4, ?4)
             on conflict(id) do update set status = 'running', updated_at = excluded.updated_at",
            params![run_id, workflow_name, source, Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("Failed to save workflow run: {}", error))?;

        let mut stmt = conn
            .prepare(
                "select name, checkpoint, output_json from workflow_steps
                 where run_id = ?1 and status = 'succeeded' and checkpoint is not null",
            )
            .map_err(|error| format!("Failed to query workflow checkpoints: {}", error))?;
        let mut rows = stmt
            .query(params![run_id])
            .map_err(|error| format!("Failed to read workflow checkpoints: {}", error))?;
        let mut resume_checkpoints = HashSet::new();
        let mut resume_step_outputs = HashMap::new();
        while let Some(row) = rows
            .next()
            .map_err(|error| format!("Failed to read workflow checkpoint: {}", error))?
        {
            let name = row
                .get::<_, String>(0)
                .map_err(|error| format!("Failed to read workflow checkpoint name: {}", error))?;
            let checkpoint = row
                .get::<_, String>(1)
                .map_err(|error| format!("Failed to read workflow checkpoint marker: {}", error))?;
            let output_json = row
                .get::<_, Option<String>>(2)
                .map_err(|error| format!("Failed to read workflow checkpoint output: {}", error))?;
            resume_checkpoints.insert(checkpoint);
            if let Some(output_json) = output_json {
                let json: JsonValue = serde_json::from_str(&output_json).map_err(|error| {
                    format!("Failed to decode workflow checkpoint output: {}", error)
                })?;
                resume_step_outputs.insert(name, json_to_value(json));
            }
        }
        drop(rows);
        drop(stmt);

        Ok(WorkflowPersistence {
            conn,
            run_id,
            resume_checkpoints,
            resume_step_outputs,
        })
    }

    fn workflow_run_step(&mut self, step: &WorkflowStep) -> Result<WorkflowStepRunResult, String> {
        match &step.command {
            WorkflowStepCommand::Shell(command) => {
                let output = self.workflow_run_command(command, step.timeout, &step.env)?;
                let succeeded = exec_value_success(&output);
                let error = (!succeeded).then(|| workflow_error_from_exec(&output));
                Ok(WorkflowStepRunResult {
                    output,
                    succeeded,
                    error,
                })
            }
            WorkflowStepCommand::Zen(source) => match self.workflow_run_zen(source) {
                Ok(output) => Ok(WorkflowStepRunResult {
                    output,
                    succeeded: true,
                    error: None,
                }),
                Err(error) => Ok(WorkflowStepRunResult {
                    output: workflow_error_output(&error),
                    succeeded: false,
                    error: Some(error),
                }),
            },
        }
    }

    fn workflow_run_command(
        &mut self,
        command: &str,
        timeout: Option<Duration>,
        env: &HashMap<String, WorkflowEnvValue>,
    ) -> Result<Value, String> {
        self.host.check_permission("proc.exec")?;
        let (resolved_env, secret_values) = self.resolve_workflow_env(env)?;
        let request = ExecRequest {
            command: command.into(),
            argv: None,
            attempts: 1,
            timeout,
            wait_children: false,
            workdir: Some(self.host.cwd_path().to_string_lossy().into_owned()),
            env: resolved_env,
            secret_values,
        };
        ProcessEffects.perform(Effect::Process(request), &CapabilityGrant::new("proc.exec"))
    }

    /// Resolves literal env values as-is and secret references from the
    /// trusted secret store. Resolution happens here, right before the
    /// child process is spawned, so the plaintext value never lives on
    /// `WorkflowStep`/`WorkflowSpec` and is never in scope when workflow
    /// state gets persisted, logged, or echoed as an event. Returns the
    /// resolved secret plaintext values alongside the env map so the caller
    /// can mask them out of captured process output too.
    fn resolve_workflow_env(
        &self,
        env: &HashMap<String, WorkflowEnvValue>,
    ) -> Result<(HashMap<String, String>, Vec<String>), String> {
        let mut resolved = HashMap::new();
        let mut secret_values = Vec::new();
        for (key, value) in env {
            let value = match value {
                WorkflowEnvValue::Literal(value) => value.clone(),
                WorkflowEnvValue::Secret(name) => {
                    self.host.check_permission("secrets.read")?;
                    let secret = self
                        .host
                        .read_secret(name)?
                        .ok_or_else(|| format!("Secret '{}' was not found", name))?;
                    secret_values.push(secret.clone());
                    secret
                }
            };
            resolved.insert(key.clone(), value);
        }
        Ok((resolved, secret_values))
    }

    fn workflow_run_zen(&mut self, source: &str) -> Result<Value, String> {
        self.host.run_capture(source)
    }

    fn workflow_rollback_completed_steps(
        &mut self,
        workflow_name: &str,
        rollback_stack: &mut Vec<CompletedWorkflowStep>,
        step_results: &mut [Value],
        events: &mut Vec<Value>,
        mut persistence: Option<&mut WorkflowPersistence>,
    ) -> Result<(), String> {
        let mut rollback_error = None;

        while let Some(step) = rollback_stack.pop() {
            if step.rollback.is_empty() {
                continue;
            }

            push_workflow_event(
                events,
                persistence.as_deref_mut(),
                "step.rollback_started",
                workflow_name,
                Some(&step.name),
                None,
            )?;

            let mut rolled_back = true;
            let mut step_error = None;
            for action in &step.rollback {
                if let Err(error) = self.workflow_run_rollback_action(
                    action,
                    workflow_name,
                    &step.name,
                    events,
                    persistence.as_deref_mut(),
                ) {
                    rolled_back = false;
                    step_error = Some(error.clone());
                    rollback_error = Some(error);
                }
            }

            let status = if rolled_back {
                "rolled_back"
            } else {
                "rollback_failed"
            };
            set_workflow_step_result_status(step_results, &step.name, status);
            persist_workflow_step(
                persistence.as_deref_mut(),
                &step.name,
                status,
                0,
                step.checkpoint.as_deref(),
                &step.output,
                step_error.as_deref(),
            )?;
            push_workflow_event(
                events,
                persistence.as_deref_mut(),
                if rolled_back {
                    "step.rolled_back"
                } else {
                    "step.rollback_failed"
                },
                workflow_name,
                Some(&step.name),
                None,
            )?;
        }

        match rollback_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn workflow_run_action(
        &mut self,
        action: &WorkflowAction,
        workflow_name: &str,
        step_name: &str,
        events: &mut Vec<Value>,
        mut persistence: Option<&mut WorkflowPersistence>,
    ) -> Result<(), String> {
        self.workflow_run_named_action(
            action,
            workflow_name,
            step_name,
            events,
            persistence.as_deref_mut(),
            "action",
        )
    }

    fn workflow_run_rollback_action(
        &mut self,
        action: &WorkflowAction,
        workflow_name: &str,
        step_name: &str,
        events: &mut Vec<Value>,
        mut persistence: Option<&mut WorkflowPersistence>,
    ) -> Result<(), String> {
        self.workflow_run_named_action(
            action,
            workflow_name,
            step_name,
            events,
            persistence.as_deref_mut(),
            "rollback",
        )
    }

    fn workflow_run_named_action(
        &mut self,
        action: &WorkflowAction,
        workflow_name: &str,
        step_name: &str,
        events: &mut Vec<Value>,
        mut persistence: Option<&mut WorkflowPersistence>,
        event_prefix: &str,
    ) -> Result<(), String> {
        match action {
            WorkflowAction::Run(command) => {
                push_workflow_event(
                    events,
                    persistence.as_deref_mut(),
                    &format!("{}.started", event_prefix),
                    workflow_name,
                    Some(step_name),
                    None,
                )?;
                let output = self.workflow_run_command(command, None, &HashMap::new())?;
                if exec_value_success(&output) {
                    push_workflow_event(
                        events,
                        persistence.as_deref_mut(),
                        &format!("{}.succeeded", event_prefix),
                        workflow_name,
                        Some(step_name),
                        None,
                    )?;
                    Ok(())
                } else {
                    push_workflow_event(
                        events,
                        persistence.as_deref_mut(),
                        &format!("{}.failed", event_prefix),
                        workflow_name,
                        Some(step_name),
                        None,
                    )?;
                    Err(workflow_error_from_exec(&output))
                }
            }
            WorkflowAction::Emit(name) => {
                push_workflow_event(
                    events,
                    persistence.as_deref_mut(),
                    name,
                    workflow_name,
                    Some(step_name),
                    None,
                )?;
                Ok(())
            }
        }
    }

    fn workflow_artifact_summaries(&self, step: &WorkflowStep) -> Result<Vec<Value>, String> {
        step.artifacts
            .iter()
            .map(|artifact| self.workflow_artifact_summary(step, artifact))
            .collect()
    }

    fn workflow_artifact_summary(
        &self,
        step: &WorkflowStep,
        artifact: &WorkflowArtifactSpec,
    ) -> Result<Value, String> {
        let path = self.host.resolve_workspace_path(&artifact.path)?;
        let metadata = fs::metadata(&path).ok();
        let mut map = HashMap::new();
        map.insert("step".into(), Value::String(step.name.clone()));
        map.insert("path".into(), Value::String(artifact.path.clone()));
        map.insert(
            "absolute_path".into(),
            Value::String(path.to_string_lossy().into_owned()),
        );
        map.insert("exists".into(), Value::Bool(metadata.is_some()));
        if let Some(name) = &artifact.name {
            map.insert("name".into(), Value::String(name.clone()));
        }
        if let Some(metadata) = metadata {
            map.insert("size".into(), Value::Number(metadata.len() as f64));
            map.insert("directory".into(), Value::Bool(metadata.is_dir()));
        }
        Ok(Value::Object(map))
    }
}

pub fn validate(value: Value) -> Result<(), String> {
    validate_workflow_value(&value)
}

pub fn runtime_db_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".zen").join("runtime.db")
}

fn workflow_spec_from_value(value: Value) -> Result<WorkflowSpec, String> {
    validate_workflow_value(&value)?;
    let Value::Object(map) = value else {
        return Err("workflow.run expects a workflow object".into());
    };

    let name = workflow_string_field(&map, "name")?;
    let steps_value = map.get("steps").ok_or("workflow.run expects steps")?;
    let Value::List(step_values) = steps_value else {
        return Err("workflow steps must be a list".into());
    };

    let mut steps = Vec::new();
    for step_value in step_values {
        steps.push(workflow_step_from_value(step_value.clone())?);
    }

    Ok(WorkflowSpec { name, steps })
}

fn validate_workflow_value(value: &Value) -> Result<(), String> {
    let mut errors = Vec::new();
    let Value::Object(map) = value else {
        return Err("Workflow validation failed:\n  workflow must be an object".into());
    };

    validate_required_string(map, "name", "name", &mut errors);
    match map.get("steps") {
        Some(Value::List(steps)) if steps.is_empty() => {
            errors.push("steps must contain at least one step".into());
        }
        Some(Value::List(steps)) => {
            for (index, step) in steps.iter().enumerate() {
                validate_workflow_step_value(step, &format!("steps[{}]", index), &mut errors);
            }
        }
        Some(_) => errors.push("steps must be a list".into()),
        None => errors.push("steps is required".into()),
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Workflow validation failed:\n  {}",
            errors.join("\n  ")
        ))
    }
}

fn validate_workflow_step_value(value: &Value, path: &str, errors: &mut Vec<String>) {
    let Value::Object(map) = value else {
        errors.push(format!("{} must be an object", path));
        return;
    };

    validate_required_string(map, "name", &format!("{}.name", path), errors);
    let has_run = map.contains_key("run");
    let has_zen = map.contains_key("zen");
    match (has_run, has_zen) {
        (true, false) => validate_required_string(map, "run", &format!("{}.run", path), errors),
        (false, true) => validate_required_string(map, "zen", &format!("{}.zen", path), errors),
        (false, false) => errors.push(format!("{} must contain either run or zen", path)),
        (true, true) => errors.push(format!("{} must contain only one of run or zen", path)),
    }
    validate_optional_string(map, "checkpoint", &format!("{}.checkpoint", path), errors);
    if let Some(condition) = map.get("if") {
        validate_workflow_condition_value(condition, &format!("{}.if", path), errors);
    }
    if let Some(timeout) = map.get("timeout") {
        validate_workflow_duration_value(timeout, &format!("{}.timeout", path), errors);
    }
    if let Some(env) = map.get("env") {
        validate_workflow_env_value(env, &format!("{}.env", path), errors);
    }
    validate_optional_string(map, "save_as", &format!("{}.save_as", path), errors);
    if let Some(artifacts) = map.get("artifacts") {
        validate_workflow_artifacts_value(artifacts, &format!("{}.artifacts", path), errors);
    }

    if let Some(retry) = map.get("retry") {
        validate_workflow_retry_value(retry, &format!("{}.retry", path), errors);
    }
    for field in ["on_failure", "rollback", "finally"] {
        if let Some(actions) = map.get(field) {
            validate_workflow_actions_value(actions, &format!("{}.{}", path, field), errors);
        }
    }
}

fn validate_workflow_condition_value(value: &Value, path: &str, errors: &mut Vec<String>) {
    match value {
        Value::String(value) if !value.is_empty() => {
            if let Err(error) = parse_workflow_condition(value) {
                errors.push(format!("{} {}", path, error));
            }
        }
        Value::String(_) => errors.push(format!("{} must be a non-empty string", path)),
        _ => errors.push(format!("{} must be a string", path)),
    }
}

fn validate_workflow_retry_value(value: &Value, path: &str, errors: &mut Vec<String>) {
    let Value::Object(map) = value else {
        errors.push(format!("{} must be an object", path));
        return;
    };

    if let Some(value) = map.get("attempts") {
        match value {
            Value::Number(value) if *value >= 1.0 && value.fract() == 0.0 => {}
            Value::Number(_) => errors.push(format!("{}.attempts must be an integer >= 1", path)),
            _ => errors.push(format!("{}.attempts must be a number", path)),
        }
    }

    if let Some(value) = map.get("delay") {
        validate_workflow_duration_value(value, &format!("{}.delay", path), errors);
    }
}

fn validate_workflow_duration_value(value: &Value, path: &str, errors: &mut Vec<String>) {
    match value {
        Value::String(value) => {
            if let Err(error) = parse_duration(value) {
                errors.push(format!("{} {}", path, error));
            }
        }
        Value::Number(value) if *value >= 0.0 => {}
        Value::Number(_) => errors.push(format!("{} must be >= 0", path)),
        _ => errors.push(format!("{} must be a duration string or number", path)),
    }
}

fn validate_workflow_env_value(value: &Value, path: &str, errors: &mut Vec<String>) {
    let Value::Object(map) = value else {
        errors.push(format!("{} must be an object", path));
        return;
    };

    for (key, value) in map {
        if key.is_empty() {
            errors.push(format!("{} contains an empty key", path));
        }
        match value {
            Value::String(_) | Value::Secret(_) => {}
            Value::Object(entry)
                if entry.len() == 1
                    && matches!(entry.get("secret"), Some(Value::String(name)) if !name.is_empty()) => {}
            _ => errors.push(format!(
                "{}.{} must be a string or {{ secret: \"name\" }}",
                path, key
            )),
        }
    }
}

fn validate_workflow_artifacts_value(value: &Value, path: &str, errors: &mut Vec<String>) {
    match value {
        Value::String(value) if !value.is_empty() => {}
        Value::String(_) => errors.push(format!("{} must be a non-empty string", path)),
        Value::List(items) => {
            for (index, item) in items.iter().enumerate() {
                validate_workflow_artifact_value(item, &format!("{}[{}]", path, index), errors);
            }
        }
        _ => errors.push(format!(
            "{} must be a string or list of artifact strings/objects",
            path
        )),
    }
}

fn validate_workflow_artifact_value(value: &Value, path: &str, errors: &mut Vec<String>) {
    match value {
        Value::String(value) if !value.is_empty() => {}
        Value::String(_) => errors.push(format!("{} must be a non-empty string", path)),
        Value::Object(map) => {
            validate_required_string(map, "path", &format!("{}.path", path), errors);
            validate_optional_string(map, "name", &format!("{}.name", path), errors);
        }
        _ => errors.push(format!("{} must be a string or artifact object", path)),
    }
}

fn validate_workflow_actions_value(value: &Value, path: &str, errors: &mut Vec<String>) {
    let Value::List(actions) = value else {
        errors.push(format!("{} must be a list of actions", path));
        return;
    };

    for (index, action) in actions.iter().enumerate() {
        let action_path = format!("{}[{}]", path, index);
        let Value::Object(map) = action else {
            errors.push(format!("{} must be an object", action_path));
            continue;
        };

        let has_run = map.contains_key("run");
        let has_emit = map.contains_key("emit");
        match (has_run, has_emit) {
            (true, false) => {
                validate_required_string(map, "run", &format!("{}.run", action_path), errors)
            }
            (false, true) => {
                validate_required_string(map, "emit", &format!("{}.emit", action_path), errors)
            }
            (false, false) => {
                errors.push(format!("{} must contain either run or emit", action_path))
            }
            (true, true) => errors.push(format!(
                "{} must contain only one of run or emit",
                action_path
            )),
        }
    }
}

fn validate_required_string(
    map: &HashMap<String, Value>,
    field: &str,
    path: &str,
    errors: &mut Vec<String>,
) {
    match map.get(field) {
        Some(Value::String(value)) if !value.is_empty() => {}
        Some(Value::Secret(value)) if !value.is_empty() => {}
        Some(_) => errors.push(format!("{} must be a non-empty string", path)),
        None => errors.push(format!("{} is required", path)),
    }
}

fn validate_optional_string(
    map: &HashMap<String, Value>,
    field: &str,
    path: &str,
    errors: &mut Vec<String>,
) {
    match map.get(field) {
        Some(Value::String(value)) if value.is_empty() => {
            errors.push(format!("{} must be a non-empty string", path))
        }
        Some(Value::String(_)) | Some(Value::Secret(_)) | None => {}
        Some(_) => errors.push(format!("{} must be a string", path)),
    }
}

fn workflow_step_from_value(value: Value) -> Result<WorkflowStep, String> {
    let Value::Object(map) = value else {
        return Err("workflow step must be an object".into());
    };

    Ok(WorkflowStep {
        name: workflow_string_field(&map, "name")?,
        command: workflow_step_command_from_value(&map)?,
        condition: match map.get("if") {
            Some(value) => {
                let condition = workflow_string_value(value, "if")?;
                Some(parse_workflow_condition(&condition)?)
            }
            None => None,
        },
        timeout: workflow_timeout_from_value(map.get("timeout"))?,
        env: workflow_env_from_value(map.get("env"))?,
        save_as: match map.get("save_as") {
            Some(value) => Some(workflow_string_value(value, "save_as")?),
            None => None,
        },
        artifacts: workflow_artifacts_from_value(map.get("artifacts"))?,
        retry: workflow_retry_from_value(map.get("retry"))?,
        on_failure: workflow_actions_from_value(map.get("on_failure"))?,
        rollback: workflow_actions_from_value(map.get("rollback"))?,
        finally: workflow_actions_from_value(map.get("finally"))?,
        checkpoint: match map.get("checkpoint") {
            Some(value) => Some(workflow_string_value(value, "checkpoint")?),
            None => None,
        },
    })
}

fn workflow_step_command_from_value(
    map: &HashMap<String, Value>,
) -> Result<WorkflowStepCommand, String> {
    match (map.get("run"), map.get("zen")) {
        (Some(value), None) => Ok(WorkflowStepCommand::Shell(workflow_string_value(
            value, "run",
        )?)),
        (None, Some(value)) => Ok(WorkflowStepCommand::Zen(workflow_string_value(
            value, "zen",
        )?)),
        (None, None) => Err("workflow step expects run or zen".into()),
        (Some(_), Some(_)) => Err("workflow step expects only one of run or zen".into()),
    }
}

fn init_workflow_db(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "
        create table if not exists workflow_runs (
            id text primary key,
            name text not null,
            source text not null,
            status text not null,
            created_at text not null,
            updated_at text not null
        );
        create table if not exists workflow_steps (
            run_id text not null,
            name text not null,
            status text not null,
            attempts integer not null,
            checkpoint text,
            output_json text,
            error text,
            updated_at text not null,
            primary key (run_id, name)
        );
        create table if not exists workflow_events (
            id integer primary key autoincrement,
            run_id text not null,
            event text not null,
            workflow text not null,
            step text,
            attempt integer,
            created_at text not null
        );
        ",
    )
    .map_err(|error| format!("Failed to initialize workflow store: {}", error))
}

fn persist_workflow_run_status(conn: &Connection, run_id: &str, status: &str) -> Result<(), String> {
    conn.execute(
        "update workflow_runs set status = ?1, updated_at = ?2 where id = ?3",
        params![status, Utc::now().to_rfc3339(), run_id],
    )
    .map_err(|error| format!("Failed to update workflow run: {}", error))?;
    Ok(())
}

fn persist_workflow_step(
    persistence: Option<&mut WorkflowPersistence>,
    name: &str,
    status: &str,
    attempts: usize,
    checkpoint: Option<&str>,
    output: &Value,
    error: Option<&str>,
) -> Result<(), String> {
    let Some(persistence) = persistence else {
        return Ok(());
    };
    let output_json = serde_json::to_string(&value_to_json(output))
        .map_err(|error| format!("Failed to encode workflow step output: {}", error))?;
    persistence
        .conn
        .execute(
            "insert into workflow_steps (run_id, name, status, attempts, checkpoint, output_json, error, updated_at)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             on conflict(run_id, name) do update set
                status = excluded.status,
                attempts = excluded.attempts,
                checkpoint = excluded.checkpoint,
                output_json = excluded.output_json,
                error = excluded.error,
                updated_at = excluded.updated_at",
            params![
                persistence.run_id,
                name,
                status,
                attempts as i64,
                checkpoint,
                output_json,
                error,
                Utc::now().to_rfc3339()
            ],
        )
        .map_err(|error| format!("Failed to save workflow step: {}", error))?;
    Ok(())
}

fn workflow_slug(value: &str) -> String {
    let slug: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    slug.trim_matches('-').to_string()
}

fn workflow_retry_from_value(value: Option<&Value>) -> Result<RetryPolicy, String> {
    let Some(Value::Object(map)) = value else {
        return Ok(RetryPolicy {
            attempts: 1,
            delay: Duration::ZERO,
        });
    };

    let attempts = match map.get("attempts") {
        Some(Value::Number(value)) => (*value as usize).max(1),
        Some(other) => {
            return Err(format!(
                "workflow retry.attempts must be a number, got {}",
                value_to_echo_string(other.clone())
            ))
        }
        None => 1,
    };
    let delay = match map.get("delay") {
        Some(Value::String(value)) => parse_duration(value)?,
        Some(Value::Number(value)) => Duration::from_secs(*value as u64),
        Some(other) => {
            return Err(format!(
                "workflow retry.delay must be a duration string or number, got {}",
                value_to_echo_string(other.clone())
            ))
        }
        None => Duration::ZERO,
    };

    Ok(RetryPolicy { attempts, delay })
}

fn workflow_timeout_from_value(value: Option<&Value>) -> Result<Option<Duration>, String> {
    match value {
        Some(Value::String(value)) => parse_duration(value).map(Some),
        Some(Value::Number(value)) => Ok(Some(Duration::from_secs(*value as u64))),
        Some(_) => Err("workflow timeout must be a duration string or number".into()),
        None => Ok(None),
    }
}

fn workflow_env_from_value(
    value: Option<&Value>,
) -> Result<HashMap<String, WorkflowEnvValue>, String> {
    let Some(value) = value else {
        return Ok(HashMap::new());
    };
    let Value::Object(map) = value else {
        return Err("workflow env must be an object".into());
    };

    let mut env = HashMap::new();
    for (key, value) in map {
        env.insert(key.clone(), workflow_env_value_from_value(value, key)?);
    }
    Ok(env)
}

fn workflow_env_value_from_value(value: &Value, key: &str) -> Result<WorkflowEnvValue, String> {
    match value {
        Value::String(value) | Value::Secret(value) => Ok(WorkflowEnvValue::Literal(value.clone())),
        Value::Object(entry) if entry.len() == 1 && entry.contains_key("secret") => {
            match secret_reference_name(value) {
                Some(name) => Ok(WorkflowEnvValue::Secret(name.to_string())),
                None => Err(format!(
                    "workflow env '{}' secret reference must be {{ secret: \"name\" }}",
                    key
                )),
            }
        }
        _ => Err(format!(
            "workflow env '{}' must be a string or {{ secret: \"name\" }}",
            key
        )),
    }
}

fn parse_workflow_condition(raw: &str) -> Result<WorkflowCondition, String> {
    let (left, op, right) = if let Some((left, right)) = raw.split_once("==") {
        (left, WorkflowConditionOp::Eq, right)
    } else if let Some((left, right)) = raw.split_once("!=") {
        (left, WorkflowConditionOp::Neq, right)
    } else {
        return Err("must contain == or !=".into());
    };

    let left = left.trim();
    if left.is_empty() {
        return Err("left side must be a path".into());
    }
    let left: Vec<String> = left.split('.').map(str::trim).map(str::to_string).collect();
    if left.iter().any(|part| part.is_empty()) {
        return Err("left side path contains an empty segment".into());
    }
    if left.first().map(String::as_str) != Some("outputs") {
        return Err("left side path must start with outputs".into());
    }

    Ok(WorkflowCondition {
        left,
        op,
        right: parse_workflow_condition_literal(right.trim())?,
    })
}

fn parse_workflow_condition_literal(raw: &str) -> Result<Value, String> {
    if raw.is_empty() {
        return Err("right side literal is required".into());
    }

    if raw == "true" {
        return Ok(Value::Bool(true));
    }
    if raw == "false" {
        return Ok(Value::Bool(false));
    }
    if raw == "null" {
        return Ok(Value::Null);
    }
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        return Ok(Value::String(raw[1..raw.len() - 1].into()));
    }
    if let Ok(number) = raw.parse::<f64>() {
        return Ok(Value::Number(number));
    }

    Err("right side must be a string, boolean, number, or null literal".into())
}

fn workflow_condition_matches(
    condition: Option<&WorkflowCondition>,
    outputs: &HashMap<String, Value>,
) -> Result<bool, String> {
    let Some(condition) = condition else {
        return Ok(true);
    };

    let left = workflow_condition_path_value(&condition.left, outputs)?;
    let matches = eq_vals(&left, &condition.right);
    Ok(match condition.op {
        WorkflowConditionOp::Eq => matches,
        WorkflowConditionOp::Neq => !matches,
    })
}

fn workflow_condition_path_value(
    path: &[String],
    outputs: &HashMap<String, Value>,
) -> Result<Value, String> {
    let Some(first) = path.first() else {
        return Err("workflow condition path is empty".into());
    };
    if first != "outputs" {
        return Err("workflow condition path must start with outputs".into());
    }

    let mut current = Value::Object(outputs.clone());
    for part in &path[1..] {
        let Value::Object(map) = current else {
            return Ok(Value::Null);
        };
        current = map.get(part).cloned().unwrap_or(Value::Null);
    }
    Ok(current)
}

fn workflow_actions_from_value(value: Option<&Value>) -> Result<Vec<WorkflowAction>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Value::List(items) = value else {
        return Err("workflow actions must be a list".into());
    };

    items
        .iter()
        .map(|item| {
            let Value::Object(map) = item else {
                return Err("workflow action must be an object".into());
            };
            if let Some(value) = map.get("run") {
                return Ok(WorkflowAction::Run(workflow_string_value(value, "run")?));
            }
            if let Some(value) = map.get("emit") {
                return Ok(WorkflowAction::Emit(workflow_string_value(value, "emit")?));
            }
            Err("workflow action expects run or emit".into())
        })
        .collect()
}

fn workflow_artifacts_from_value(
    value: Option<&Value>,
) -> Result<Vec<WorkflowArtifactSpec>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };

    match value {
        Value::String(path) | Value::Secret(path) => Ok(vec![WorkflowArtifactSpec {
            name: None,
            path: path.clone(),
        }]),
        Value::List(items) => items.iter().map(workflow_artifact_from_value).collect(),
        _ => Err("workflow artifacts must be a string or list".into()),
    }
}

fn workflow_artifact_from_value(value: &Value) -> Result<WorkflowArtifactSpec, String> {
    match value {
        Value::String(path) | Value::Secret(path) => Ok(WorkflowArtifactSpec {
            name: None,
            path: path.clone(),
        }),
        Value::Object(map) => Ok(WorkflowArtifactSpec {
            name: match map.get("name") {
                Some(value) => Some(workflow_string_value(value, "artifact.name")?),
                None => None,
            },
            path: workflow_string_field(map, "path")?,
        }),
        _ => Err("workflow artifact must be a string or object".into()),
    }
}

fn workflow_string_field(map: &HashMap<String, Value>, field: &str) -> Result<String, String> {
    let value = map
        .get(field)
        .ok_or_else(|| format!("workflow object missing '{}'", field))?;
    workflow_string_value(value, field)
}

fn workflow_string_value(value: &Value, field: &str) -> Result<String, String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Secret(value) => Ok(value.clone()),
        _ => Err(format!("workflow '{}' must be a string", field)),
    }
}

fn exec_value_success(value: &Value) -> bool {
    match value {
        Value::Object(map) => matches!(map.get("success"), Some(Value::Bool(true))),
        _ => false,
    }
}

fn workflow_error_from_exec(value: &Value) -> String {
    let Value::Object(map) = value else {
        return "workflow command failed".into();
    };
    let stderr = map
        .get("stderr")
        .and_then(Value::as_string)
        .unwrap_or("")
        .trim();
    if !stderr.is_empty() {
        return stderr.into();
    }
    let stdout = map
        .get("stdout")
        .and_then(Value::as_string)
        .unwrap_or("")
        .trim();
    if !stdout.is_empty() {
        return stdout.into();
    }
    let exitcode = map
        .get("exitcode")
        .map(|value| value_to_echo_string(value.clone()))
        .unwrap_or_else(|| "unknown".into());
    format!("command exited with status {}", exitcode)
}

fn workflow_error_output(error: &str) -> Value {
    let mut map = HashMap::new();
    map.insert("success".into(), Value::Bool(false));
    map.insert("error".into(), Value::String(error.into()));
    Value::Object(map)
}

fn workflow_step_result(
    name: &str,
    status: &str,
    attempts: usize,
    output: Value,
    error: Option<String>,
) -> Value {
    let mut map = HashMap::new();
    map.insert("name".into(), Value::String(name.into()));
    map.insert("status".into(), Value::String(status.into()));
    map.insert("attempts".into(), Value::Number(attempts as f64));
    map.insert("output".into(), output);
    if let Some(error) = error {
        map.insert("error".into(), Value::String(error));
    }
    Value::Object(map)
}

fn record_workflow_output(outputs: &mut HashMap<String, Value>, step: &WorkflowStep, output: Value) {
    let key = step.save_as.as_ref().unwrap_or(&step.name).clone();
    outputs.insert(key, output);
}

fn workflow_step_output_from_result(value: &Value) -> Value {
    let Value::Object(map) = value else {
        return Value::Null;
    };
    map.get("output").cloned().unwrap_or(Value::Null)
}

fn set_workflow_step_result_status(step_results: &mut [Value], name: &str, status: &str) {
    for result in step_results.iter_mut().rev() {
        let Value::Object(map) = result else {
            continue;
        };
        if map.get("name").and_then(Value::as_string) == Some(name) {
            map.insert("status".into(), Value::String(status.into()));
            return;
        }
    }
}

fn push_workflow_event(
    events: &mut Vec<Value>,
    persistence: Option<&mut WorkflowPersistence>,
    name: &str,
    workflow_name: &str,
    step_name: Option<&str>,
    attempt: Option<usize>,
) -> Result<(), String> {
    let event = Event {
        name: name.to_string(),
        workflow: workflow_name.to_string(),
        step: step_name.map(str::to_string),
        attempt,
    };

    let mut map = HashMap::new();
    map.insert("event".into(), Value::String(event.name.clone()));
    map.insert("workflow".into(), Value::String(event.workflow.clone()));
    if let Some(step_name) = &event.step {
        map.insert("step".into(), Value::String(step_name.clone()));
    }
    if let Some(attempt) = event.attempt {
        map.insert("attempt".into(), Value::Number(attempt as f64));
    }
    events.push(Value::Object(map));
    if let Some(persistence) = persistence {
        persistence.emit(&event)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_persistence() -> WorkflowPersistence {
        let conn = Connection::open_in_memory().unwrap();
        init_workflow_db(&conn).unwrap();
        conn.execute(
            "insert into workflow_runs (id, name, source, status, created_at, updated_at)
             values ('run-1', 'wf', 'test', 'running', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        WorkflowPersistence {
            conn,
            run_id: "run-1".into(),
            resume_checkpoints: HashSet::new(),
            resume_step_outputs: HashMap::new(),
        }
    }

    #[test]
    fn event_sink_persists_emitted_event() {
        let mut persistence = test_persistence();
        persistence
            .emit(&Event {
                name: "step.succeeded".into(),
                workflow: "wf".into(),
                step: Some("build".into()),
                attempt: Some(2),
            })
            .unwrap();

        let (event, step, attempt): (String, Option<String>, Option<i64>) = persistence
            .conn
            .query_row(
                "select event, step, attempt from workflow_events where run_id = 'run-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(event, "step.succeeded");
        assert_eq!(step.as_deref(), Some("build"));
        assert_eq!(attempt, Some(2));
    }
}
