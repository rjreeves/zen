use crate::ast::{Expr, PipeStage, Stmt};
use crate::audit::{current_timestamp, write_entry, AuditEntry};
use crate::interrupt;
use crate::lexer::Lexer;
use std::time::Instant;

use crate::parser::Parser;
use crate::permissions::PermissionSet;
use crate::runtime::executor::Executor;
use crate::runtime::values::Value;
use crate::terminal;
use crate::Commands;
use reedline::{
    default_emacs_keybindings, ColumnarMenu, Completer, EditCommand, Emacs, FileBackedHistory,
    KeyCode, KeyModifiers, Keybindings, MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch,
    Reedline, ReedlineEvent, ReedlineMenu, Signal, Span, Suggestion, ValidationResult, Validator,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub fn handle_command(cmd: Commands) -> Result<(), String> {
    match cmd {
        Commands::Run { script, yes } => run_script(&script, yes),
        Commands::Repl { yes } => run_repl(yes),
        Commands::Explain { script } => explain_script(&script),
        Commands::Audit => show_audit(),
        Commands::Runs => show_workflow_runs(),
        Commands::RunStatus { run_id } => show_workflow_run_status(&run_id),
        Commands::Resume { run_id, yes } => resume_workflow_run(&run_id, yes),
        Commands::Version => show_version(),
        Commands::OneOff(parts) => run_one_off(parts),
    }
}

fn run_script(path: &str, auto_yes: bool) -> Result<(), String> {
    let start = Instant::now();

    if is_workflow_path(path) {
        return run_workflow_file(path, auto_yes, start);
    }

    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read script '{}': {}", path, e))?;

    run_source(&src, path, auto_yes, start)
}

fn is_workflow_path(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("yaml" | "yml")
    )
}

fn run_workflow_file(path: &str, auto_yes: bool, start: Instant) -> Result<(), String> {
    run_workflow_file_with_run_id(path, auto_yes, start, None)
}

fn run_workflow_file_with_run_id(
    path: &str,
    auto_yes: bool,
    start: Instant,
    run_id: Option<&str>,
) -> Result<(), String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read workflow '{}': {}", path, e))?;
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(&src).map_err(|e| format!("Failed to parse workflow YAML: {}", e))?;
    let workflow = yaml_to_value(yaml)?;
    let permissions = workflow_permissions(&workflow)?;
    let permission_list = permissions.list();

    println!("Workflow requires:");
    for p in &permission_list {
        println!("  - {}", p);
    }

    if !auto_yes {
        print!("\nAllow? (y/n): ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();

        if input.trim().to_lowercase() != "y" {
            return Err("Execution denied by user".into());
        }
    } else {
        println!("✔ Auto-approved (--yes)");
    }

    let mut executor = Executor::new_with_permissions(permissions);
    let result = if let Some(run_id) = run_id {
        executor.workflow_resume_persisted(workflow, path, run_id)
    } else {
        executor.workflow_run_persisted(workflow, path)
    };
    let duration = start.elapsed().as_millis();

    let audit_entry = AuditEntry {
        timestamp: current_timestamp(),
        script: path.to_string(),
        permissions: permission_list,
        status: if result.is_ok() {
            "success".into()
        } else {
            "error".into()
        },
        duration_ms: duration,
        error: result.clone().err(),
    };

    let _ = write_entry(audit_entry);

    let result = result?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value_to_json(&result))
            .unwrap_or_else(|_| format!("{:?}", result))
    );
    Ok(())
}

fn resume_workflow_run(run_id: &str, auto_yes: bool) -> Result<(), String> {
    let conn = open_runtime_db()?;
    let source: String = conn
        .query_row(
            "select source from workflow_runs where id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Failed to query workflow run '{}': {}", run_id, e))?
        .ok_or_else(|| format!("Workflow run '{}' was not found", run_id))?;

    run_workflow_file_with_run_id(&source, auto_yes, Instant::now(), Some(run_id))
}

fn show_workflow_runs() -> Result<(), String> {
    let Some(conn) = open_runtime_db_if_exists()? else {
        println!("No workflow runs found.");
        return Ok(());
    };

    let mut stmt = conn
        .prepare(
            "select id, name, source, status, created_at, updated_at
             from workflow_runs
             order by updated_at desc",
        )
        .map_err(|e| format!("Failed to query workflow runs: {}", e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| format!("Failed to read workflow runs: {}", e))?;

    println!("Workflow runs:");
    let mut any = false;
    for row in rows {
        let (id, name, source, status, created_at, updated_at) =
            row.map_err(|e| format!("Failed to read workflow run: {}", e))?;
        any = true;
        println!(
            "  {}  {}  {}  source={}  created={}  updated={}",
            id, status, name, source, created_at, updated_at
        );
    }

    if !any {
        println!("  (none)");
    }
    Ok(())
}

fn show_workflow_run_status(run_id: &str) -> Result<(), String> {
    let conn = open_runtime_db()?;
    let run: Option<(String, String, String, String, String, String)> = conn
        .query_row(
            "select id, name, source, status, created_at, updated_at
             from workflow_runs
             where id = ?1",
            params![run_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|e| format!("Failed to query workflow run '{}': {}", run_id, e))?;
    let Some((id, name, source, status, created_at, updated_at)) = run else {
        return Err(format!("Workflow run '{}' was not found", run_id));
    };

    println!("Run: {}", id);
    println!("Name: {}", name);
    println!("Source: {}", source);
    println!("Status: {}", status);
    println!("Created: {}", created_at);
    println!("Updated: {}", updated_at);

    println!("\nSteps:");
    let mut stmt = conn
        .prepare(
            "select name, status, attempts, checkpoint, error, updated_at
             from workflow_steps
             where run_id = ?1
             order by updated_at asc",
        )
        .map_err(|e| format!("Failed to query workflow steps: {}", e))?;
    let steps = stmt
        .query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| format!("Failed to read workflow steps: {}", e))?;
    let mut any_steps = false;
    for step in steps {
        let (name, status, attempts, checkpoint, error, updated_at) =
            step.map_err(|e| format!("Failed to read workflow step: {}", e))?;
        any_steps = true;
        println!(
            "  {}  {}  attempts={}  checkpoint={}  updated={}",
            name,
            status,
            attempts,
            checkpoint.unwrap_or_else(|| "-".into()),
            updated_at
        );
        if let Some(error) = error {
            println!("    error: {}", error);
        }
    }
    if !any_steps {
        println!("  (none)");
    }

    println!("\nEvents:");
    let mut stmt = conn
        .prepare(
            "select event, step, attempt, created_at
             from workflow_events
             where run_id = ?1
             order by id asc",
        )
        .map_err(|e| format!("Failed to query workflow events: {}", e))?;
    let events = stmt
        .query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| format!("Failed to read workflow events: {}", e))?;
    let mut any_events = false;
    for event in events {
        let (event, step, attempt, created_at) =
            event.map_err(|e| format!("Failed to read workflow event: {}", e))?;
        any_events = true;
        println!(
            "  {}  event={}  step={}  attempt={}",
            created_at,
            event,
            step.unwrap_or_else(|| "-".into()),
            attempt
                .map(|attempt| attempt.to_string())
                .unwrap_or_else(|| "-".into())
        );
    }
    if !any_events {
        println!("  (none)");
    }

    Ok(())
}

fn open_runtime_db_if_exists() -> Result<Option<Connection>, String> {
    let path = runtime_db_path();
    if !path.is_file() {
        return Ok(None);
    }
    Connection::open(&path)
        .map(Some)
        .map_err(|e| format!("Failed to open '{}': {}", path.display(), e))
}

fn open_runtime_db() -> Result<Connection, String> {
    open_runtime_db_if_exists()?.ok_or_else(|| {
        format!(
            "Workflow runtime database was not found at '{}'",
            runtime_db_path().display()
        )
    })
}

fn runtime_db_path() -> PathBuf {
    Executor::new_with_permissions(PermissionSet::new(&Vec::new())).workflow_runtime_db_path()
}

fn yaml_to_value(value: serde_yaml::Value) -> Result<Value, String> {
    match value {
        serde_yaml::Value::Null => Ok(Value::Null),
        serde_yaml::Value::Bool(value) => Ok(Value::Bool(value)),
        serde_yaml::Value::Number(value) => value
            .as_f64()
            .map(Value::Number)
            .ok_or_else(|| "Unsupported YAML number".into()),
        serde_yaml::Value::String(value) => Ok(Value::String(value)),
        serde_yaml::Value::Sequence(items) => items
            .into_iter()
            .map(yaml_to_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        serde_yaml::Value::Mapping(entries) => {
            let mut object = std::collections::HashMap::new();
            for (key, value) in entries {
                let serde_yaml::Value::String(key) = key else {
                    return Err("Workflow YAML object keys must be strings".into());
                };
                object.insert(key, yaml_to_value(value)?);
            }
            Ok(Value::Object(object))
        }
        serde_yaml::Value::Tagged(tagged) => yaml_to_value(tagged.value),
    }
}

fn workflow_permissions(workflow: &Value) -> Result<PermissionSet, String> {
    let mut required = Vec::new();
    let permission_probe = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));

    required.extend(workflow_explicit_permissions(workflow)?);

    if workflow_has_run(workflow) {
        required.push(("proc".into(), "exec".into()));
    }
    for command in workflow_zen_commands(workflow) {
        if let Some(permissions) = permission_probe.command_permissions(&command) {
            for permission in permissions {
                if let Some((left, right)) = permission.split_once('.') {
                    required.push((left.into(), right.into()));
                }
            }
        }
    }

    Ok(PermissionSet::new(&required))
}

fn workflow_explicit_permissions(workflow: &Value) -> Result<Vec<(String, String)>, String> {
    let Value::Object(map) = workflow else {
        return Ok(Vec::new());
    };
    let Some(requires) = map.get("requires") else {
        return Ok(Vec::new());
    };
    let Value::List(items) = requires else {
        return Err("Workflow requires must be a list of permission strings".into());
    };

    items
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let Some(permission) = value.as_string() else {
                return Err(format!("Workflow requires[{}] must be a string", index));
            };
            workflow_permission_from_string(permission)
                .map_err(|error| format!("Workflow requires[{}] {}", index, error))
        })
        .collect()
}

fn workflow_permission_from_string(permission: &str) -> Result<(String, String), String> {
    if permission.is_empty() {
        return Err("must be non-empty".into());
    }
    let parts: Vec<_> = permission.split('.').collect();
    if parts.len() != 2 || parts.iter().any(|part| part.is_empty()) {
        return Err(format!(
            "must use service.action format like postgres.read, got '{}'",
            permission
        ));
    }
    Ok((parts[0].into(), parts[1].into()))
}

fn workflow_has_run(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.contains_key("run") || map.values().any(workflow_has_run),
        Value::List(items) => items.iter().any(workflow_has_run),
        _ => false,
    }
}

fn workflow_zen_commands(value: &Value) -> Vec<String> {
    let mut commands = Vec::new();
    collect_workflow_zen_commands(value, &mut commands);
    commands
}

fn collect_workflow_zen_commands(value: &Value, commands: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(source) | Value::Secret(source)) = map.get("zen") {
                if let Some(command) = source.split_whitespace().next() {
                    commands.push(command.into());
                }
            }
            for value in map.values() {
                collect_workflow_zen_commands(value, commands);
            }
        }
        Value::List(items) => {
            for item in items {
                collect_workflow_zen_commands(item, commands);
            }
        }
        _ => {}
    }
}

fn run_one_off(parts: Vec<String>) -> Result<(), String> {
    if parts.is_empty() {
        return Err("Expected Zen expression to run".into());
    }

    let mut auto_yes = false;
    let parts: Vec<String> = parts
        .into_iter()
        .filter(|part| {
            if part == "--yes" || part == "-y" {
                auto_yes = true;
                false
            } else {
                true
            }
        })
        .collect();

    if parts.is_empty() {
        return Err("Expected Zen expression to run".into());
    }

    let src = format!("{}\n", parts.join(" "));
    run_source(&src, "<inline>", auto_yes, Instant::now())
}

fn run_source(src: &str, audit_label: &str, auto_yes: bool, start: Instant) -> Result<(), String> {
    let tokens = Lexer::new(src).tokenize()?;
    let mut parser = Parser::new(tokens, src);
    let program = parser.parse_program()?;

    let permissions = PermissionSet::new(&program.requires);

    let permission_list = permissions.list();

    if !permission_list.is_empty() {
        println!("Script requires:");
        for p in &permission_list {
            println!("  - {}", p);
        }

        if !auto_yes {
            print!("\nAllow? (y/n): ");
            use std::io::{self, Write};
            io::stdout().flush().unwrap();

            let mut input = String::new();
            io::stdin().read_line(&mut input).unwrap();

            if input.trim().to_lowercase() != "y" {
                return Err("Execution denied by user".into());
            }
        } else {
            println!("✔ Auto-approved (--yes)");
        }
    }

    let permissions_list = permissions.list();

    let mut executor = Executor::new_with_permissions(permissions);

    let result = executor.execute(program);

    let duration = start.elapsed().as_millis();

    let audit_entry = AuditEntry {
        timestamp: current_timestamp(),
        script: audit_label.to_string(),
        permissions: permissions_list,
        status: if result.is_ok() {
            "success".into()
        } else {
            "error".into()
        },
        duration_ms: duration,
        error: result.clone().err(),
    };

    let _ = write_entry(audit_entry);

    result
}

fn run_repl(auto_yes: bool) -> Result<(), String> {
    interrupt::install_ctrl_c_handler()?;
    interrupt::clear_interrupt();

    let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
    print_repl_banner(&executor);

    let mut history = Vec::new();
    let completion_state = Arc::new(Mutex::new(ZenCompletionState::default()));
    let mut editor = build_reedline_editor(Arc::clone(&completion_state))?;
    let prompt = ZenPrompt;
    let persistent_history = repl_history_path();

    let mut startup_files = run_startup_files(&mut executor, auto_yes);

    loop {
        refresh_completion_state(&completion_state, &executor);
        let line = match editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => line,
            Ok(Signal::CtrlC) | Ok(Signal::ExternalBreak(_)) => {
                interrupt::clear_interrupt();
                println!("Interrupted.");
                continue;
            }
            Ok(Signal::CtrlD) => {
                println!();
                break;
            }
            Ok(_) => continue,
            Err(e) => return Err(format!("Failed to read input: {}", e)),
        };

        if interrupt::take_interrupt() {
            println!("Interrupted.");
            continue;
        }

        let trimmed = line.trim();

        if trimmed == "." {
            break;
        }

        if trimmed.starts_with(':') {
            match handle_repl_command(
                trimmed,
                &mut executor,
                &mut history,
                &mut startup_files,
                auto_yes,
            ) {
                Ok(ReplCommandAction::Continue) => continue,
                Ok(ReplCommandAction::Quit) => break,
                Err(e) => {
                    eprintln!("{}", e);
                    continue;
                }
            }
        }

        let input = ensure_trailing_newline(line);
        if let Err(e) = execute_repl_source(&mut executor, &input, auto_yes) {
            eprintln!("{}", e);
            continue;
        }

        history.push(input);
    }

    if let Some(path) = &persistent_history {
        if let Err(e) = editor.sync_history() {
            eprintln!("Failed to save REPL history to '{}': {}", path.display(), e);
        }
    }

    Ok(())
}

fn print_repl_banner(executor: &Executor) {
    println!("{}", version_banner());
    println!("Workspace: {}", executor.workspace_root_path().display());
    println!("Type :help for help, :quit to exit.");
}

fn version_banner() -> String {
    format!("{}  (c) SyntrA 2026", version_label())
}

fn version_label() -> String {
    format!(
        "Zen v{}.{}",
        env!("CARGO_PKG_VERSION"),
        env!("ZEN_BUILD_NUMBER")
    )
}

fn build_reedline_editor(
    completion_state: Arc<Mutex<ZenCompletionState>>,
) -> Result<Reedline, String> {
    let mut keybindings = default_emacs_keybindings();
    add_completion_keybindings(&mut keybindings);

    let completion_menu = Box::new(
        ColumnarMenu::default()
            .with_name("completion_menu")
            .with_columns(4)
            .with_column_padding(2),
    );

    let mut editor = Reedline::create()
        .with_validator(Box::new(ZenInputValidator))
        .with_completer(Box::new(ZenCompleter::new(completion_state)))
        .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
        .with_edit_mode(Box::new(Emacs::new(keybindings)))
        .with_quick_completions(true)
        .with_partial_completions(true);

    if let Some(path) = repl_history_path() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create REPL history directory: {}", e))?;
        }

        match FileBackedHistory::with_file(1000, path.clone()) {
            Ok(history) => {
                editor = editor.with_history(Box::new(history));
            }
            Err(e) => {
                eprintln!(
                    "Warning: failed to open REPL history at '{}': {}",
                    path.display(),
                    e
                );
            }
        }
    }

    Ok(editor)
}

fn add_completion_keybindings(keybindings: &mut Keybindings) {
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    keybindings.add_binding(
        KeyModifiers::ALT,
        KeyCode::Enter,
        ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
    );
}

struct ZenPrompt;

impl Prompt for ZenPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _prompt_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("zen> ")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("...> ")
    }

    fn render_prompt_history_search_indicator(
        &self,
        _history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Borrowed("search> ")
    }
}

struct ZenInputValidator;

impl Validator for ZenInputValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        if repl_input_complete(line) {
            ValidationResult::Complete
        } else {
            ValidationResult::Incomplete
        }
    }
}

fn ensure_trailing_newline(mut input: String) -> String {
    if !input.ends_with('\n') {
        input.push('\n');
    }
    input
}

#[derive(Default)]
struct ZenCompletionState {
    commands: Vec<String>,
    variables: Vec<String>,
}

struct ZenCompleter {
    state: Arc<Mutex<ZenCompletionState>>,
}

impl ZenCompleter {
    fn new(state: Arc<Mutex<ZenCompletionState>>) -> Self {
        Self { state }
    }
}

impl Completer for ZenCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let pos = pos.min(line.len());
        let before_cursor = &line[..pos];

        if let Some((start, prefix)) = variable_completion_prefix(before_cursor) {
            let variables = self
                .state
                .lock()
                .map(|state| state.variables.clone())
                .unwrap_or_default();
            return completion_suggestions(variables, prefix, start, pos, false);
        }

        if let Some((start, prefix, quote)) = repl_path_completion_prefix(before_cursor) {
            return path_completion_suggestions(prefix, start, pos, quote);
        }

        let (start, prefix) = token_completion_prefix(before_cursor);
        let choices = if before_cursor.trim_start().starts_with(':') {
            repl_meta_commands()
                .into_iter()
                .map(str::to_string)
                .collect()
        } else {
            self.state
                .lock()
                .map(|state| state.commands.clone())
                .unwrap_or_default()
        };

        completion_suggestions(choices, prefix, start, pos, true)
    }
}

fn refresh_completion_state(state: &Arc<Mutex<ZenCompletionState>>, executor: &Executor) {
    let mut commands = collect_completion_commands(executor);
    commands.sort();
    commands.dedup();

    let mut variables: Vec<_> = executor
        .variables()
        .into_iter()
        .map(|(name, _)| name.to_string())
        .collect();
    variables.sort();
    variables.dedup();

    if let Ok(mut state) = state.lock() {
        state.commands = commands;
        state.variables = variables;
    }
}

fn collect_completion_commands(executor: &Executor) -> Vec<String> {
    let mut commands = vec![
        "cd".to_string(),
        "clear".to_string(),
        "echo".to_string(),
        "fields".to_string(),
        "from-json".to_string(),
        "get".to_string(),
        "pick".to_string(),
        "save".to_string(),
        "select".to_string(),
        "table".to_string(),
        "to-json".to_string(),
        "where".to_string(),
    ];

    if let Ok(Value::List(plugins)) = executor.plugin_inventory() {
        for plugin in plugins {
            let Value::Object(plugin) = plugin else {
                continue;
            };
            let Some(Value::List(plugin_commands)) = plugin.get("commands") else {
                continue;
            };
            commands.extend(
                plugin_commands
                    .iter()
                    .filter_map(|value| value.as_string().map(str::to_string)),
            );
        }
    }

    commands
}

fn repl_meta_commands() -> Vec<&'static str> {
    vec![
        ".",
        ":clear",
        ":commands",
        ":d",
        ":doctor",
        ":exit",
        ":help",
        ":history",
        ":load",
        ":p",
        ":permissions",
        ":plugins",
        ":quit",
        ":r",
        ":reload",
        ":reset",
        ":s",
        ":save",
        ":startup",
        ":var",
        ":vars",
    ]
}

fn variable_completion_prefix(input: &str) -> Option<(usize, &str)> {
    let dollar = input.rfind('$')?;
    let prefix = &input[dollar + 1..];
    if prefix.chars().all(is_variable_name_char) {
        Some((dollar + 1, prefix))
    } else {
        None
    }
}

fn repl_path_completion_prefix(input: &str) -> Option<(usize, &str, Option<char>)> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with(":load") && !trimmed.starts_with(":save") {
        return None;
    }

    let arg_start = input.find(char::is_whitespace)?;
    let after_command = &input[arg_start..];
    let leading_space = after_command.len() - after_command.trim_start().len();
    let path_start = arg_start + leading_space;
    let path = &input[path_start..];

    match path.chars().next() {
        Some('"') => Some((path_start + 1, &path[1..], Some('"'))),
        Some('\'') => Some((path_start + 1, &path[1..], Some('\''))),
        _ => Some((path_start, path, None)),
    }
}

fn token_completion_prefix(input: &str) -> (usize, &str) {
    let start = input
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace() || matches!(ch, '|' | '{' | '}'))
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    (start, &input[start..])
}

fn completion_suggestions(
    choices: Vec<String>,
    prefix: &str,
    start: usize,
    end: usize,
    append_whitespace: bool,
) -> Vec<Suggestion> {
    let mut seen = HashSet::new();
    let mut suggestions: Vec<_> = choices
        .into_iter()
        .filter(|choice| choice.starts_with(prefix))
        .filter(|choice| seen.insert(choice.clone()))
        .map(|choice| Suggestion {
            value: choice,
            span: Span { start, end },
            append_whitespace,
            ..Default::default()
        })
        .collect();
    suggestions.sort_by(|left, right| left.value.cmp(&right.value));
    suggestions
}

fn path_completion_suggestions(
    prefix: &str,
    start: usize,
    end: usize,
    quote: Option<char>,
) -> Vec<Suggestion> {
    let path = PathBuf::from(prefix);
    let (dir, name_prefix) = if prefix.ends_with(['/', '\\']) {
        (path, "")
    } else {
        (
            path.parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".")),
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(""),
        )
    };

    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut suggestions = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(name_prefix) {
            continue;
        }

        let mut value = dir.join(name).to_string_lossy().replace('\\', "/");
        if value.starts_with("./") {
            value = value.trim_start_matches("./").to_string();
        }
        if entry.path().is_dir() {
            value.push('/');
        } else if let Some(quote) = quote {
            value.push(quote);
        }

        suggestions.push(Suggestion {
            value,
            span: Span { start, end },
            append_whitespace: entry.path().is_file() && quote.is_none(),
            ..Default::default()
        });
    }
    suggestions.sort_by(|left, right| left.value.cmp(&right.value));
    suggestions
}

fn is_variable_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'
}

fn repl_history_path() -> Option<PathBuf> {
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .or_else(dirs::home_dir)
        .map(|base| base.join("zen").join("zen_history.txt"))
}

enum ReplCommandAction {
    Continue,
    Quit,
}

fn handle_repl_command(
    trimmed: &str,
    executor: &mut Executor,
    history: &mut Vec<String>,
    startup_files: &mut Vec<StartupFile>,
    auto_yes: bool,
) -> Result<ReplCommandAction, String> {
    match trimmed {
        "" => Ok(ReplCommandAction::Continue),
        "." | ":quit" | ":exit" => Ok(ReplCommandAction::Quit),
        ":help" => {
            print_repl_help_overview();
            Ok(ReplCommandAction::Continue)
        }
        ":commands" => {
            print_repl_help_overview();
            Ok(ReplCommandAction::Continue)
        }
        ":clear" => {
            terminal::clear_screen()?;
            Ok(ReplCommandAction::Continue)
        }
        ":var" | ":vars" => {
            print_repl_vars(executor);
            Ok(ReplCommandAction::Continue)
        }
        ":history" => {
            print_repl_history(history);
            Ok(ReplCommandAction::Continue)
        }
        ":plugins" => {
            print_repl_plugins(executor)?;
            Ok(ReplCommandAction::Continue)
        }
        ":permissions" | ":perms" | ":p" => {
            print_repl_permissions(executor);
            Ok(ReplCommandAction::Continue)
        }
        ":doctor" | ":d" => {
            print_repl_doctor(executor, history, startup_files);
            Ok(ReplCommandAction::Continue)
        }
        ":reset" => {
            executor.reset_session();
            executor.reload_plugins();
            *startup_files = reload_startup_files(executor, auto_yes, startup_files);
            print_repl_doctor(executor, history, startup_files);
            Ok(ReplCommandAction::Continue)
        }
        ":startup" | ":s" => {
            print_startup_files(startup_files);
            Ok(ReplCommandAction::Continue)
        }
        ":reload" | ":r" => {
            executor.reload_plugins();
            *startup_files = reload_startup_files(executor, auto_yes, startup_files);
            print_startup_files(startup_files);
            print_repl_plugins(executor)?;
            Ok(ReplCommandAction::Continue)
        }
        _ => {
            if let Some(topic) = repl_command_arg(trimmed, ":help") {
                print_repl_help_topic(topic)?;
                return Ok(ReplCommandAction::Continue);
            }

            if let Some(path) = repl_command_arg(trimmed, ":load") {
                load_repl_script(path, executor, history, auto_yes)?;
                return Ok(ReplCommandAction::Continue);
            }

            if let Some(path) = repl_command_arg(trimmed, ":save") {
                save_repl_history(path, history)?;
                return Ok(ReplCommandAction::Continue);
            }

            Err(format!("Unknown REPL command '{}'", trimmed))
        }
    }
}

fn repl_command_arg<'a>(trimmed: &'a str, command: &str) -> Option<&'a str> {
    let rest = trimmed.strip_prefix(command)?;
    let arg = rest.trim_start();
    if rest.len() == arg.len() || arg.is_empty() {
        return None;
    }
    Some(arg)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StartupFile {
    path: PathBuf,
    status: StartupStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StartupStatus {
    Loaded,
    Missing,
    Failed(String),
}

fn run_startup_files(executor: &mut Executor, auto_yes: bool) -> Vec<StartupFile> {
    run_startup_file_paths(executor, auto_yes, repl_startup_paths())
}

fn reload_startup_files(
    executor: &mut Executor,
    auto_yes: bool,
    startup_files: &[StartupFile],
) -> Vec<StartupFile> {
    let paths: Vec<_> = startup_files.iter().map(|file| file.path.clone()).collect();
    if paths.is_empty() {
        run_startup_files(executor, auto_yes)
    } else {
        run_startup_file_paths(executor, auto_yes, paths)
    }
}

fn run_startup_file_paths(
    executor: &mut Executor,
    auto_yes: bool,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Vec<StartupFile> {
    let mut files = Vec::new();

    for path in paths {
        if !path.is_file() {
            files.push(StartupFile {
                path,
                status: StartupStatus::Missing,
            });
            continue;
        }

        let label = path.to_string_lossy();
        let status = match std::fs::read_to_string(&path) {
            Ok(input) => match execute_repl_source(executor, &input, auto_yes) {
                Ok(()) => StartupStatus::Loaded,
                Err(e) => {
                    eprintln!("Startup file '{}' failed:\n{}", label, e);
                    StartupStatus::Failed(e)
                }
            },
            Err(e) => {
                let message = format!("Failed to read startup file '{}': {}", label, e);
                eprintln!("{}", message);
                StartupStatus::Failed(message)
            }
        };

        files.push(StartupFile { path, status });
    }

    files
}

fn repl_startup_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(base) = dirs::config_dir()
        .or_else(dirs::data_local_dir)
        .or_else(dirs::data_dir)
        .or_else(dirs::home_dir)
    {
        paths.push(base.join("zen").join("startup.fg"));
    }

    if let Ok(cwd) = std::env::current_dir() {
        paths.push(cwd.join(".zen").join("startup.fg"));
    }

    paths.sort();
    paths.dedup();
    paths
}

fn execute_repl_source(executor: &mut Executor, input: &str, auto_yes: bool) -> Result<(), String> {
    interrupt::clear_interrupt();

    if input
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim_start().starts_with(':'))
    {
        return Err(
            "REPL commands such as :var, :history, :load, and :save cannot be parsed as Zen code."
                .into(),
        );
    }

    let tokens = Lexer::new(input).tokenize()?;
    let mut parser = Parser::new(tokens, input);
    let program = parser.parse_program()?;

    if !program.requires.is_empty() {
        let permissions = PermissionSet::new(&program.requires);
        let permission_list = permissions.list();

        println!("Session requires:");
        for p in &permission_list {
            println!("  - {}", p);
        }

        if !auto_yes && !confirm_repl_permissions()? {
            return Err("Permissions were not granted.".into());
        }

        executor.grant_permissions(&program.requires);
    }

    let result = executor.execute(program);
    if interrupt::take_interrupt() {
        return Err("Interrupted.".into());
    }
    result
}

fn load_repl_script(
    path: &str,
    executor: &mut Executor,
    history: &mut Vec<String>,
    auto_yes: bool,
) -> Result<(), String> {
    let input = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read script '{}': {}", path, e))?;

    execute_repl_source(executor, &input, auto_yes)?;
    history.push(input);
    println!("Loaded {}", path);
    Ok(())
}

fn save_repl_history(path: &str, history: &[String]) -> Result<(), String> {
    let mut output = String::new();

    for entry in history {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(entry.trim_end());
        output.push('\n');
    }

    std::fs::write(path, output)
        .map_err(|e| format!("Failed to save REPL history to '{}': {}", path, e))?;
    println!("Saved {}", path);
    Ok(())
}

fn repl_input_complete(input: &str) -> bool {
    let mut depth = 0i32;

    for ch in input.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
    }

    depth <= 0
}

fn confirm_repl_permissions() -> Result<bool, String> {
    print!("\nAllow for this REPL session? (y/n): ");
    io::stdout()
        .flush()
        .map_err(|e| format!("Failed to flush prompt: {}", e))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read input: {}", e))?;

    Ok(input.trim().eq_ignore_ascii_case("y"))
}

fn print_repl_help_overview() {
    println!("Commands:");
    println!("  :help        Show this help");
    println!("  :help TOPIC  Show help for a REPL topic");
    println!("  :commands    Show REPL commands");
    println!("  :clear       Clear pending multiline input");
    println!("  :history     Show executed Zen inputs");
    println!("  :plugins     Show loaded plugins and owned commands");
    println!("  :permissions Show granted session permissions (:perms, :p)");
    println!("  :doctor      Show a compact session health summary (:d)");
    println!("  :reset       Clear session state and re-run startup files");
    println!("  :startup     Show startup files and load status (:s)");
    println!("  :reload      Re-run startup files in this session (:r)");
    println!("  :load PATH   Load and run a Zen script in this session");
    println!("  :save PATH   Save executed Zen inputs to a script");
    println!("  :vars        Show session variables (:var also works)");
    println!("  .            Exit the REPL");
    println!("  :quit        Exit the REPL");
    println!();
    println!("Examples:");
    println!("  let now = time.now");
    println!("  echo now");
    println!("  time.since \"2026-05-01\" | select days, human");
    println!("  requires {{");
    println!("    proc.exec");
    println!("  }}");
    println!("  exec cargo --version");
    println!();
    println!("Try :help keys for REPL keyboard shortcuts.");
}

fn print_repl_help_topic(topic: &str) -> Result<(), String> {
    let topic = repl_help_topic_key(topic);

    match topic.as_str() {
        "commands" | "repl" => {
            print_repl_help_overview();
            Ok(())
        }
        "keys" | "keyboard" | "shortcuts" => {
            println!("Keyboard Shortcuts:");
            println!("  Tab          Complete commands, paths, and variables");
            println!("  Enter        Submit complete input");
            println!("  Enter        Continue multiline input when braces are open");
            println!("  Alt+Enter    Insert an explicit newline");
            println!("  Up/Down      Browse history");
            println!("  Left/Right   Move the cursor");
            println!("  Ctrl+A       Move to start of line");
            println!("  Ctrl+E       Move to end of line");
            println!("  Ctrl+U       Clear before the cursor");
            println!("  Ctrl+K       Clear after the cursor");
            println!("  Ctrl+W       Delete the previous word");
            println!("  Ctrl+C       Cancel the current edit and return to the prompt");
            println!("  Ctrl+D       Exit the REPL");
            println!("  Ctrl+R       Search history");
            println!();
            println!("Completions:");
            println!("  :            REPL commands such as :help, :load, and :save");
            println!("  command      Loaded Zen and plugin command names");
            println!("  :load PATH   Workspace paths");
            println!("  :save PATH   Workspace paths");
            println!("  $            Session variables");
            Ok(())
        }
        "help" => {
            println!("Help:");
            println!("  Use :help to show all REPL commands.");
            println!("  Use :help TOPIC for topic help, such as :help startup or :help permissions.");
            println!("  Use help <command> for Zen command docs, such as help dropbox.status.");
            Ok(())
        }
        "load" => {
            println!("Load:");
            println!("  Usage: :load PATH");
            println!("  Loads and runs a Zen script in the current REPL session.");
            println!("  Loaded script input is added to the REPL history.");
            Ok(())
        }
        "save" => {
            println!("Save:");
            println!("  Usage: :save PATH");
            println!("  Saves executed Zen inputs from this REPL session to a script file.");
            println!("  REPL meta commands such as :vars and :startup are not saved.");
            Ok(())
        }
        "vars" | "var" => {
            println!("Variables:");
            println!("  Use :vars or :var to show variables set in the current REPL session.");
            Ok(())
        }
        "history" => {
            println!("History:");
            println!("  Use :history to show executed Zen inputs in this REPL session.");
            println!("  Use :save PATH to write those inputs to a script file.");
            Ok(())
        }
        "clear" => {
            println!("Clear:");
            println!("  Use :clear to clear the terminal screen.");
            println!("  If multiline input is pending, :clear also discards it.");
            Ok(())
        }
        "quit" | "exit" | "." => {
            println!("Quit:");
            println!("  Use :quit, :exit, or . to leave the REPL.");
            Ok(())
        }
        "startup" => {
            println!("Startup files:");
            println!("  Zen checks a user startup file and .zen/startup.fg when the REPL starts.");
            println!("  Use :startup or :s to show loaded, missing, and failed files.");
            println!("  Use :reload or :r to re-run the same startup files.");
            Ok(())
        }
        "permissions" | "perms" => {
            println!("Permissions:");
            println!("  Use requires {{ ... }} to grant permissions for the current REPL session.");
            println!("  Use :permissions, :perms, or :p to show granted permissions.");
            println!("  Permission errors include a Try: requires {{ ... }} suggestion.");
            Ok(())
        }
        "plugins" => {
            println!("Plugins:");
            println!("  Use :plugins to show loaded plugins and their commands.");
            println!("  Use plugins.list for structured plugin metadata in pipelines.");
            println!("  Use help <command> for detailed command docs when available.");
            Ok(())
        }
        "doctor" => {
            println!("Doctor:");
            println!("  Use :doctor or :d to show cwd, startup status, permission count, variables, and history.");
            Ok(())
        }
        "reset" => {
            println!("Reset:");
            println!("  Use :reset to clear variables, permissions, and mocked time, then re-run startup files.");
            Ok(())
        }
        "aliases" => {
            println!("Aliases:");
            println!("  :p  -> :permissions");
            println!("  :s  -> :startup");
            println!("  :r  -> :reload");
            println!("  :d  -> :doctor");
            Ok(())
        }
        _ => Err(format!(
            "Unknown help topic '{}'. Try: :help commands, :help keys, :help :save, :help startup, :help permissions, :help plugins, :help aliases, :help doctor, or :help reset",
            topic
        )),
    }
}

fn repl_help_topic_key(topic: &str) -> String {
    topic
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_start_matches(':')
        .to_ascii_lowercase()
}

fn print_repl_vars(executor: &Executor) {
    let vars = executor.variables();

    if vars.is_empty() {
        println!("No session variables.");
        return;
    }

    for (name, value) in vars {
        println!("{} = {}", name, format_repl_value(value));
    }
}

fn print_repl_history(history: &[String]) {
    if history.is_empty() {
        println!("No REPL history.");
        return;
    }

    for (index, entry) in history.iter().enumerate() {
        println!("{}: {}", index + 1, entry.trim_end());
    }
}

fn print_repl_plugins(executor: &Executor) -> Result<(), String> {
    let plugins = executor.plugin_inventory()?;

    let Value::List(entries) = plugins else {
        return Err("plugins.list returned unexpected data".into());
    };

    if entries.is_empty() {
        println!("No plugins loaded.");
        return Ok(());
    }

    println!("Plugins:");
    for entry in entries {
        let Value::Object(map) = entry else {
            continue;
        };
        let name = map
            .get("name")
            .and_then(Value::as_string)
            .unwrap_or("unknown");
        let kind = map
            .get("kind")
            .and_then(Value::as_string)
            .unwrap_or("builtin");
        let commands = map
            .get("commands")
            .and_then(|value| match value {
                Value::List(commands) => Some(commands),
                _ => None,
            })
            .map(|commands| {
                commands
                    .iter()
                    .filter_map(Value::as_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let description = map.get("description").and_then(Value::as_string);
        let version = map.get("version").and_then(Value::as_string);
        let author = map.get("author").and_then(Value::as_string);
        let homepage = map.get("homepage").and_then(Value::as_string);
        let label = version
            .map(|version| format!("{} v{}", name, version))
            .unwrap_or_else(|| name.to_string());

        if kind == "external" {
            let source = map
                .get("source")
                .and_then(Value::as_string)
                .unwrap_or("unknown source");
            println!("  {:<16} {:<8} {}", label, kind, commands);
            if let Some(description) = description {
                println!("  {:<16} {:<8} {}", "", "about", description);
            }
            if let Some(author) = author {
                println!("  {:<16} {:<8} {}", "", "author", author);
            }
            if let Some(homepage) = homepage {
                println!("  {:<16} {:<8} {}", "", "home", homepage);
            }
            println!("  {:<16} {:<8} {}", "", "source", source);
        } else {
            println!("  {:<16} {:<8} {}", label, kind, commands);
        }
    }

    let diagnostics = executor.external_plugin_diagnostics();
    if diagnostics.root.is_some() || !diagnostics.failed.is_empty() {
        println!();
        println!("External plugins:");
        match diagnostics.root {
            Some(root) => println!("  root   {}", root),
            None => println!("  root   none found"),
        }
        println!("  loaded {}", diagnostics.loaded.len());
        println!("  failed {}", diagnostics.failed.len());
        for loaded in diagnostics.loaded {
            println!(
                "  ok     {} [{}]: {}",
                loaded.name,
                loaded.commands.join(", "),
                loaded.source
            );
        }
        for failure in diagnostics.failed {
            println!("  error  {}: {}", failure.source, failure.error);
        }
    }

    Ok(())
}

fn print_repl_permissions(executor: &Executor) {
    let mut permissions = executor.permissions();
    permissions.sort();

    if permissions.is_empty() {
        println!("No session permissions.");
        return;
    }

    println!("Session permissions:");
    for permission in permissions {
        println!("  {}", permission);
    }
}

fn print_repl_doctor(executor: &Executor, history: &[String], startup_files: &[StartupFile]) {
    let startup_loaded = startup_files
        .iter()
        .filter(|file| matches!(file.status, StartupStatus::Loaded))
        .count();
    let startup_missing = startup_files
        .iter()
        .filter(|file| matches!(file.status, StartupStatus::Missing))
        .count();
    let startup_failed = startup_files
        .iter()
        .filter(|file| matches!(file.status, StartupStatus::Failed(_)))
        .count();

    println!("Zen doctor:");
    println!("  cwd          {}", executor.cwd().display());
    println!(
        "  startup     {} loaded, {} missing, {} failed",
        startup_loaded, startup_missing, startup_failed
    );
    println!("  permissions {} granted", executor.permissions().len());
    println!("  variables   {} set", executor.variables().len());
    println!("  history     {} entries", history.len());

    let diagnostics = executor.external_plugin_diagnostics();
    println!(
        "  plugins     {} external loaded, {} failed",
        diagnostics.loaded.len(),
        diagnostics.failed.len()
    );
    if let Some(root) = diagnostics.root {
        println!("  plugin root {}", root);
    }
    for loaded in diagnostics.loaded {
        println!(
            "  plugin ok   {} [{}]: {}",
            loaded.name,
            loaded.commands.join(", "),
            loaded.source
        );
    }
    for failure in diagnostics.failed {
        println!("  plugin err  {}: {}", failure.source, failure.error);
    }
}

fn print_startup_files(files: &[StartupFile]) {
    println!("Startup files:");

    if files.is_empty() {
        println!("  none");
        return;
    }

    for file in files {
        let status = match &file.status {
            StartupStatus::Loaded => "loaded",
            StartupStatus::Missing => "missing",
            StartupStatus::Failed(_) => "failed",
        };

        println!("  {:<7} {}", status, file.path.display());
    }
}

fn format_repl_value(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => format!("{:?}", value),
        Value::Secret(_) => value.redacted().into(),
        Value::Object(_) | Value::List(_) => {
            serde_json::to_string(&value_to_json(value)).unwrap_or_else(|_| format!("{:?}", value))
        }
    }
}

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(value) => serde_json::Value::Bool(*value),
        Value::Number(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::String(value) => serde_json::Value::String(value.clone()),
        Value::Secret(_) => serde_json::Value::String(value.redacted().into()),
        Value::List(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Object(fields) => {
            let mut object = serde_json::Map::new();
            for (key, value) in fields {
                object.insert(key.clone(), value_to_json(value));
            }
            serde_json::Value::Object(object)
        }
    }
}

fn explain_script(path: &str) -> Result<(), String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read script '{}': {}", path, e))?;

    if is_workflow_path(path) {
        return explain_workflow(path, &src);
    }

    let tokens = Lexer::new(&src).tokenize()?;
    let mut parser = Parser::new(tokens, &src);
    let program = parser.parse_program()?;

    println!("Required permissions:");
    for (left, right) in program.requires {
        println!("  {}.{}", left, right);
    }

    println!("\nStatements:");
    for stmt in &program.statements {
        print_statement(stmt, 0);
        println!();
    }

    Ok(())
}

fn explain_workflow(path: &str, src: &str) -> Result<(), String> {
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(src).map_err(|e| format!("Failed to parse workflow YAML: {}", e))?;
    let workflow = yaml_to_value(yaml)?;
    let permissions = workflow_permissions(&workflow)?;
    Executor::workflow_validate(workflow.clone())?;

    println!("Workflow: {}", workflow_name(&workflow).unwrap_or(path));
    println!("\nRequired permissions:");
    for permission in permissions.list() {
        println!("  {}", permission);
    }

    println!("\nSteps:");
    for step in workflow_steps(&workflow) {
        println!("  - {}", step);
    }

    println!("\nValidation: ok");

    Ok(())
}

fn workflow_name(workflow: &Value) -> Option<&str> {
    let Value::Object(map) = workflow else {
        return None;
    };
    map.get("name").and_then(Value::as_string)
}

fn workflow_steps(workflow: &Value) -> Vec<&str> {
    let Value::Object(map) = workflow else {
        return Vec::new();
    };
    let Some(Value::List(steps)) = map.get("steps") else {
        return Vec::new();
    };

    steps
        .iter()
        .filter_map(|step| {
            let Value::Object(map) = step else {
                return None;
            };
            map.get("name").and_then(Value::as_string)
        })
        .collect()
}

fn print_statement(stmt: &Stmt, indent: usize) {
    let pad = "  ".repeat(indent);

    match stmt {
        Stmt::Requires(caps) => {
            println!("{}Requires: {:?}", pad, caps);
        }

        Stmt::Assignment { name, expr } => {
            println!("{}Assignment:", pad);
            println!("{}  name: {}", pad, name);
            println!("{}  value:", pad);
            print_expr(expr, indent + 2);
        }

        Stmt::If {
            condition,
            then_branch,
            else_branch,
        } => {
            println!("{}If:", pad);
            println!("{}  condition:", pad);
            print_expr(condition, indent + 2);
            println!("{}  then:", pad);
            for stmt in then_branch {
                print_statement(stmt, indent + 2);
            }
            if !else_branch.is_empty() {
                println!("{}  else:", pad);
                for stmt in else_branch {
                    print_statement(stmt, indent + 2);
                }
            }
        }

        Stmt::Try {
            try_branch,
            catch_name,
            catch_branch,
            finally_branch,
        } => {
            println!("{}Try:", pad);
            for stmt in try_branch {
                print_statement(stmt, indent + 1);
            }
            if !catch_branch.is_empty() {
                match catch_name {
                    Some(name) => println!("{}  catch {}:", pad, name),
                    None => println!("{}  catch:", pad),
                }
                for stmt in catch_branch {
                    print_statement(stmt, indent + 2);
                }
            }
            if !finally_branch.is_empty() {
                println!("{}  finally:", pad);
                for stmt in finally_branch {
                    print_statement(stmt, indent + 2);
                }
            }
        }

        Stmt::Pipeline(pipeline) => {
            println!("{}Pipeline:", pad);
            println!("{}  base:", pad);
            print_expr(&pipeline.base, indent + 2);

            println!("{}  stages:", pad);

            for stage in &pipeline.stages {
                print_stage(stage, indent + 2);
            }
        }

        Stmt::Expr(expr) => {
            println!("{}Expression:", pad);
            print_expr(expr, indent + 1);
        }
    }
}

fn print_stage(stage: &PipeStage, indent: usize) {
    let pad = "  ".repeat(indent);

    match stage {
        PipeStage::Where { expr } => {
            println!("{}Where:", pad);
            print_expr(expr, indent + 1);
        }

        PipeStage::Select { fields } => {
            println!("{}Select: {:?}", pad, fields);
        }

        PipeStage::Fields { fields } => {
            println!("{}Fields: {:?}", pad, fields);
        }

        PipeStage::Get { field } => {
            println!("{}Get: {}", pad, field);
        }

        PipeStage::ToJson => {
            println!("{}ToJson", pad);
        }

        PipeStage::FromJson => {
            println!("{}FromJson", pad);
        }

        PipeStage::Table => {
            println!("{}Table", pad);
        }

        PipeStage::Save { path } => {
            println!("{}Save: {}", pad, path);
        }

        PipeStage::Sort { field, descending } => {
            println!(
                "{}Sort: {} ({})",
                pad,
                field,
                if *descending { "desc" } else { "asc" }
            );
        }

        PipeStage::Limit { count } => {
            println!("{}Limit: {}", pad, count);
        }

        PipeStage::Count => {
            println!("{}Count", pad);
        }

        PipeStage::Sum { field } => {
            println!("{}Sum: {}", pad, field);
        }

        PipeStage::Avg { field } => {
            println!("{}Avg: {}", pad, field);
        }

        PipeStage::Max { field } => {
            println!("{}Max: {}", pad, field);
        }

        PipeStage::Min { field } => {
            println!("{}Min: {}", pad, field);
        }

        PipeStage::Distinct { field } => {
            println!("{}Distinct: {}", pad, field);
        }

        PipeStage::Call(call) => {
            println!("{}Call: {:?}", pad, call.name);
        }
    }
}

fn print_expr(expr: &Expr, indent: usize) {
    let pad = "  ".repeat(indent);

    match expr {
        Expr::Ident(name) => {
            println!("{}Ident({})", pad, name);
        }
        Expr::Variable(name) => {
            println!("{}Variable(${})", pad, name);
        }
        Expr::Literal(lit) => {
            println!("{}Literal({:?})", pad, lit);
        }
        Expr::Number(lit) => {
            println!("{}Number({:?})", pad, lit);
        }
        Expr::String(lit) => {
            println!("{}String({:?})", pad, lit);
        }
        Expr::Bool(lit) => {
            println!("{}Bool({:?})", pad, lit);
        }
        Expr::List(items) => {
            println!("{}List", pad);
            for item in items {
                print_expr(item, indent + 1);
            }
        }
        Expr::Object(entries) => {
            println!("{}Object", pad);
            for (key, value) in entries {
                println!("{}  Key({})", pad, key);
                print_expr(value, indent + 2);
            }
        }
        Expr::Unary { op, expr } => {
            println!("{}Unary {:?}", pad, op);
            print_expr(expr, indent + 1);
        }

        Expr::Call(call) => {
            println!("{}Call({:?})", pad, call.name);
        }

        Expr::Binary { left, op, right } => {
            println!("{}Binary({:?})", pad, op);
            print_expr(left, indent + 1);
            print_expr(right, indent + 1);
        }
    }
}

fn show_audit() -> Result<(), String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    let path = home.join(".zen").join("audit.log");

    if !path.exists() {
        println!("No audit log found.");
        return Ok(());
    }

    let content =
        std::fs::read_to_string(path).map_err(|e| format!("Failed to read audit log: {}", e))?;

    let mut entries = Vec::new();

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<AuditEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(_) => continue,
        }
    }

    if entries.is_empty() {
        println!("Audit log is empty.");
        return Ok(());
    }

    render_audit_table(&entries);

    Ok(())
}

fn render_audit_table(entries: &[AuditEntry]) {
    let headers = ["timestamp", "script", "status", "duration", "permissions"];

    // Compute column widths
    let mut w_timestamp = headers[0].len();
    let mut w_script = headers[1].len();
    let mut w_status = headers[2].len();
    let mut w_duration = headers[3].len();
    let mut w_perms = headers[4].len();

    for e in entries {
        w_timestamp = w_timestamp.max(e.timestamp.len());
        w_script = w_script.max(e.script.len());
        w_status = w_status.max(e.status.len());
        w_duration = w_duration.max(format!("{}ms", e.duration_ms).len());
        w_perms = w_perms.max(e.permissions.join(",").len());
    }

    // Header
    println!(
        "{:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}",
        headers[0],
        headers[1],
        headers[2],
        headers[3],
        headers[4],
        w1 = w_timestamp,
        w2 = w_script,
        w3 = w_status,
        w4 = w_duration,
        w5 = w_perms,
    );

    println!(
        "{:-<w1$}  {:-<w2$}  {:-<w3$}  {:-<w4$}  {:-<w5$}",
        "",
        "",
        "",
        "",
        "",
        w1 = w_timestamp,
        w2 = w_script,
        w3 = w_status,
        w4 = w_duration,
        w5 = w_perms,
    );

    // Rows
    for e in entries {
        println!(
            "{:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}",
            e.timestamp,
            e.script,
            e.status,
            format!("{}ms", e.duration_ms),
            e.permissions.join(","),
            w1 = w_timestamp,
            w2 = w_script,
            w3 = w_status,
            w4 = w_duration,
            w5 = w_perms,
        );
    }
}

fn show_version() -> Result<(), String> {
    println!("{}", version_banner());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_startup_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        std::env::temp_dir()
            .join("zen-tests")
            .join(format!("startup-{unique}.fg"))
    }

    fn plugin_inventory_has(value: &Value, name: &str) -> bool {
        let Value::List(entries) = value else {
            return false;
        };

        entries.iter().any(|entry| {
            let Value::Object(map) = entry else {
                return false;
            };
            map.get("name").and_then(Value::as_string) == Some(name)
        })
    }

    fn workflow_from_yaml(src: &str) -> Value {
        yaml_to_value(serde_yaml::from_str(src).unwrap()).unwrap()
    }

    #[test]
    fn workflow_permissions_merge_explicit_and_inferred_permissions() {
        let workflow = workflow_from_yaml(
            r#"
name: permission-smoke
requires:
  - postgres.read
steps:
  - name: auth
    zen: pg.auth.passwordless postgres
  - name: dump
    run: echo dump
"#,
        );

        let permissions = workflow_permissions(&workflow).unwrap().list();

        assert!(permissions.contains(&"postgres.read".into()));
        assert!(permissions.contains(&"proc.exec".into()));
    }

    #[test]
    fn workflow_permissions_reject_malformed_requires() {
        let workflow = workflow_from_yaml(
            r#"
name: bad-permissions
requires:
  - postgres
steps:
  - name: one
    zen: echo ok
"#,
        );

        let err = workflow_permissions(&workflow).unwrap_err();

        assert!(err.contains("Workflow requires[0]"));
        assert!(err.contains("service.action"));
    }

    #[test]
    fn startup_file_smoke_loads_session_variables() {
        let _guard = interrupt::lock_for_test();
        let path = temp_startup_path();
        let missing_path = path.with_file_name("missing-startup.fg");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "let startup_name = \"zen\"\nlet startup_count = 2 + 3\n",
        )
        .unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let startup_files = run_startup_file_paths(
            &mut executor,
            false,
            vec![path.clone(), missing_path.clone()],
        );

        let vars = executor.variables();
        assert_eq!(
            vars.iter()
                .find(|(name, _)| *name == "startup_name")
                .and_then(|(_, value)| value.as_string()),
            Some("zen")
        );
        assert_eq!(
            vars.iter()
                .find(|(name, _)| *name == "startup_count")
                .and_then(|(_, value)| value.as_number()),
            Some(5.0)
        );
        assert_eq!(
            startup_files,
            vec![
                StartupFile {
                    path: path.clone(),
                    status: StartupStatus::Loaded,
                },
                StartupFile {
                    path: missing_path,
                    status: StartupStatus::Missing,
                },
            ]
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reload_startup_file_updates_session_variables() {
        let _guard = interrupt::lock_for_test();
        let path = temp_startup_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "let reload_value = \"before\"\n").unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let mut startup_files = run_startup_file_paths(&mut executor, false, vec![path.clone()]);

        std::fs::write(&path, "let reload_value = \"after\"\n").unwrap();
        let action = handle_repl_command(
            ":reload",
            &mut executor,
            &mut Vec::new(),
            &mut startup_files,
            false,
        )
        .unwrap();

        assert!(matches!(action, ReplCommandAction::Continue));
        assert_eq!(
            executor
                .variables()
                .iter()
                .find(|(name, _)| *name == "reload_value")
                .and_then(|(_, value)| value.as_string()),
            Some("after")
        );
        assert_eq!(
            startup_files,
            vec![StartupFile {
                path: path.clone(),
                status: StartupStatus::Loaded,
            }]
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reload_refreshes_external_plugins() {
        let _guard = interrupt::lock_for_test();
        let plugin_name = format!("reload_test_{}", std::process::id());
        let plugin_command = format!("{plugin_name}.reply");
        let plugin_dir = std::env::current_dir()
            .unwrap()
            .join(".zen")
            .join("plugins")
            .join(&plugin_name);
        let _ = std::fs::remove_dir_all(&plugin_dir);
        std::fs::create_dir_all(&plugin_dir).unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let before = executor.plugin_inventory().unwrap();
        assert!(!plugin_inventory_has(&before, &plugin_name));

        std::fs::write(
            plugin_dir.join("plugin.toml"),
            format!(
                r#"
name = "{plugin_name}"

[[commands]]
name = "{plugin_command}"
run = "echo hello"
"#
            ),
        )
        .unwrap();

        let mut startup_files = Vec::new();
        let action = handle_repl_command(
            ":reload",
            &mut executor,
            &mut Vec::new(),
            &mut startup_files,
            false,
        )
        .unwrap();

        assert!(matches!(action, ReplCommandAction::Continue));
        let after = executor.plugin_inventory().unwrap();
        assert!(plugin_inventory_has(&after, &plugin_name));

        let _ = std::fs::remove_dir_all(&plugin_dir);
    }

    #[test]
    fn startup_requires_grant_session_permissions() {
        let _guard = interrupt::lock_for_test();
        let path = temp_startup_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "requires {\n  proc.exec\n  dropbox.read\n}\n").unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let startup_files = run_startup_file_paths(&mut executor, true, vec![path.clone()]);
        let mut permissions = executor.permissions();
        permissions.sort();

        assert_eq!(
            startup_files,
            vec![StartupFile {
                path: path.clone(),
                status: StartupStatus::Loaded,
            }]
        );
        assert_eq!(
            permissions,
            vec!["dropbox.read".to_string(), "proc.exec".to_string()]
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reset_clears_session_state_then_reruns_startup() {
        let _guard = interrupt::lock_for_test();
        let path = temp_startup_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "requires {\n  proc.exec\n}\nlet startup_value = \"ready\"\n",
        )
        .unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let mut startup_files = run_startup_file_paths(&mut executor, true, vec![path.clone()]);
        execute_repl_source(
            &mut executor,
            "requires {\n  dropbox.read\n}\nlet dirty_value = \"remove me\"\n",
            true,
        )
        .unwrap();

        let action = handle_repl_command(
            ":reset",
            &mut executor,
            &mut Vec::new(),
            &mut startup_files,
            true,
        )
        .unwrap();

        let vars = executor.variables();
        let mut permissions = executor.permissions();
        permissions.sort();

        assert!(matches!(action, ReplCommandAction::Continue));
        assert_eq!(
            vars.iter()
                .find(|(name, _)| *name == "startup_value")
                .and_then(|(_, value)| value.as_string()),
            Some("ready")
        );
        assert!(vars.iter().all(|(name, _)| *name != "dirty_value"));
        assert_eq!(permissions, vec!["proc.exec".to_string()]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn repl_command_aliases_are_recognized() {
        let _guard = interrupt::lock_for_test();
        let path = temp_startup_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "let alias_value = \"ready\"\n").unwrap();

        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let mut startup_files = run_startup_file_paths(&mut executor, false, vec![path.clone()]);
        let mut history = Vec::new();

        for command in [":p", ":s", ":r", ":d", ":plugins", ":commands"] {
            let action = handle_repl_command(
                command,
                &mut executor,
                &mut history,
                &mut startup_files,
                false,
            )
            .unwrap();
            assert!(matches!(action, ReplCommandAction::Continue));
        }

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn repl_help_topics_accept_known_topics() {
        for topic in [
            "commands",
            "keys",
            "keyboard",
            "shortcuts",
            ":help",
            ":load PATH",
            ":save PATH",
            ":vars",
            ":history",
            ":clear",
            ":quit",
            "startup",
            "permissions",
            "plugins",
            "aliases",
            "doctor",
            "reset",
        ] {
            print_repl_help_topic(topic).unwrap();
        }

        let err = print_repl_help_topic("nope").unwrap_err();
        assert!(err.contains("Unknown help topic"));
    }

    #[test]
    fn repl_completes_meta_commands_plugin_commands_and_variables() {
        let _guard = interrupt::lock_for_test();
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        execute_repl_source(&mut executor, "let pass = \"postgres1\"\n", true).unwrap();

        let state = Arc::new(Mutex::new(ZenCompletionState::default()));
        refresh_completion_state(&state, &executor);
        let mut completer = ZenCompleter::new(state);

        let meta: Vec<_> = completer
            .complete(":co", 3)
            .into_iter()
            .map(|suggestion| suggestion.value)
            .collect();
        assert!(meta.contains(&":commands".to_string()));

        let commands: Vec<_> = completer
            .complete("dropbox.st", 10)
            .into_iter()
            .map(|suggestion| suggestion.value)
            .collect();
        assert!(commands.contains(&"dropbox.status".to_string()));

        let variables: Vec<_> = completer
            .complete("$pa", 3)
            .into_iter()
            .map(|suggestion| suggestion.value)
            .collect();
        assert_eq!(variables, vec!["pass".to_string()]);
    }

    #[test]
    fn permission_errors_include_try_suggestion() {
        let _guard = interrupt::lock_for_test();
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let err =
            execute_repl_source(&mut executor, "workspace.find \"README\"\n", true).unwrap_err();

        assert!(err.contains("Try: requires { workspace.read }"));
    }
}
