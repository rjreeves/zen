use crate::ast::{Expr, FunctionCall};
#[cfg(test)]
use crate::runtime::executor::Executor;
use crate::runtime::plugin::{CommandDoc, PluginHost, PluginResult, ZenPlugin};
use crate::runtime::plugins::secrets::resolve_env_config;
use crate::runtime::values::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use zen_runtime::process::{exec_command, ExecRequest};

pub struct PostgresPlugin;

static POSTGRES_DOCS: &[CommandDoc] = &[
    CommandDoc {
        command: "pg.version",
        summary: "Shows the installed PostgreSQL client version.",
        usage: "pg.version",
        examples: &["pg.version"],
    },
    CommandDoc {
        command: "pg.query",
        summary: "Runs SQL with psql and returns stdout, stderr, and exitcode.",
        usage: "pg.query <database-url-or-name> <sql>",
        examples: &[
            "pg.query main \"select now()\"",
            "\"select count(*) from users\" | pg.query main",
            "pg.query \"$DATABASE_URL\" \"select 1\" { env: { PGPASSWORD: $pass } }",
        ],
    },
    CommandDoc {
        command: "pg.auth.passwordless",
        summary: "Checks whether psql can connect without any password source.",
        usage: "pg.auth.passwordless <database-url-or-name>",
        examples: &["pg.auth.passwordless postgres", "pg.auth.passwordless \"$DATABASE_URL\""],
    },
    CommandDoc {
        command: "pg.dump",
        summary: "Runs pg_dump for a database, optionally writing to a file.",
        usage: "pg.dump <database-url-or-name> [output-path]",
        examples: &["pg.dump main", "pg.dump main \"backup.sql\""],
    },
    CommandDoc {
        command: "pg.restore",
        summary: "Runs pg_restore into a PostgreSQL database.",
        usage: "pg.restore <database-url-or-name> <dump-path>",
        examples: &["pg.restore main \"backup.dump\""],
    },
    CommandDoc {
        command: "pg.pass.path",
        summary: "Returns the PostgreSQL password file path for this user.",
        usage: "pg.pass.path",
        examples: &["pg.pass.path"],
    },
    CommandDoc {
        command: "pg.pass.set",
        summary: "Creates or updates one PostgreSQL pgpass entry.",
        usage: "pg.pass.set <host> <port> <database> <user> <password>",
        examples: &[
            "pg.pass.set localhost 5432 fireworks postgres $password",
            "pg.pass.set \"*\" \"*\" fireworks postgres secrets.get \"postgres.fireworks.password\"",
        ],
    },
];

impl ZenPlugin for PostgresPlugin {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn commands(&self) -> &'static [&'static str] {
        &[
            "pg.version",
            "pg.query",
            "pg.auth.passwordless",
            "pg.dump",
            "pg.restore",
            "pg.pass.path",
            "pg.pass.set",
        ]
    }

    fn command_permissions(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("pg.query", "postgres.read"),
            ("pg.auth.passwordless", "postgres.read"),
            ("pg.dump", "postgres.read"),
            ("pg.restore", "postgres.write"),
            ("pg.pass.set", "postgres.write"),
        ]
    }

    fn command_docs(&self) -> &'static [CommandDoc] {
        POSTGRES_DOCS
    }

    fn call(
        &self,
        executor: &mut dyn PluginHost,
        call: &FunctionCall,
        input: &Value,
    ) -> Result<PluginResult, String> {
        match call.name.join(".").as_str() {
            "pg.version" => pg_version(executor, call).map(PluginResult::handled),
            "pg.query" => pg_query(executor, call, input).map(PluginResult::handled),
            "pg.auth.passwordless" => {
                pg_auth_passwordless(executor, call).map(PluginResult::handled)
            }
            "pg.dump" => pg_dump(executor, call).map(PluginResult::handled),
            "pg.restore" => pg_restore(executor, call).map(PluginResult::handled),
            "pg.pass.path" => pg_pass_path(executor, call).map(PluginResult::handled),
            "pg.pass.set" => pg_pass_set(executor, call).map(PluginResult::handled),
            _ => Ok(PluginResult::unhandled()),
        }
    }
}

fn pg_version(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    if !call.args.is_empty() {
        return Err("pg.version expects no arguments".into());
    }

    let (env, secret_values) = resolve_env_config(executor, call.config.clone())?;
    run_postgres_command("psql", &["--version".into()], env, secret_values)
}

fn pg_query(executor: &mut dyn PluginHost, call: &FunctionCall, input: &Value) -> Result<Value, String> {
    executor.check_permission("postgres.read")?;

    let args = arg_strings(executor, call.args.clone())?;
    let (database, sql) = match args.as_slice() {
        [database, sql] => (database.clone(), sql.clone()),
        [database] if !matches!(input, Value::Null) => {
            (database.clone(), value_to_string(input.clone()))
        }
        _ => return Err("pg.query expects <database-url-or-name> <sql>".into()),
    };

    let (env, secret_values) = resolve_env_config(executor, call.config.clone())?;
    run_postgres_command(
        "psql",
        &[
            database,
            "--no-align".into(),
            "--tuples-only".into(),
            "--quiet".into(),
            "--command".into(),
            sql,
        ],
        env,
        secret_values,
    )
}

fn pg_auth_passwordless(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("postgres.read")?;

    let args = arg_strings(executor, call.args.clone())?;
    let [database] = args.as_slice() else {
        return Err("pg.auth.passwordless expects <database-url-or-name>".into());
    };

    let (env, secret_values) = resolve_env_config(executor, call.config.clone())?;
    let output = run_postgres_command(
        "psql",
        &[
            "-w".into(),
            "--no-align".into(),
            "--tuples-only".into(),
            "--quiet".into(),
            "--command".into(),
            "select 1".into(),
            database.clone(),
        ],
        passwordless_auth_env(env),
        secret_values,
    )?;

    Ok(passwordless_auth_result(database, output))
}

fn pg_dump(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("postgres.read")?;

    let args = arg_strings(executor, call.args.clone())?;
    let command_args = match args.as_slice() {
        [database] => vec![database.clone()],
        [database, output_path] => vec![database.clone(), "--file".into(), output_path.clone()],
        _ => return Err("pg.dump expects <database-url-or-name> [output-path]".into()),
    };

    let (env, secret_values) = resolve_env_config(executor, call.config.clone())?;
    run_postgres_command("pg_dump", &command_args, env, secret_values)
}

fn pg_restore(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("postgres.write")?;

    let args = arg_strings(executor, call.args.clone())?;
    let [database, dump_path] = args.as_slice() else {
        return Err("pg.restore expects <database-url-or-name> <dump-path>".into());
    };

    let (env, secret_values) = resolve_env_config(executor, call.config.clone())?;
    run_postgres_command(
        "pg_restore",
        &["--dbname".into(), database.clone(), dump_path.clone()],
        env,
        secret_values,
    )
}

fn pg_pass_path(_executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    if !call.args.is_empty() {
        return Err("pg.pass.path expects no arguments".into());
    }

    Ok(Value::String(pgpass_path()?.display().to_string()))
}

fn pg_pass_set(executor: &mut dyn PluginHost, call: &FunctionCall) -> Result<Value, String> {
    executor.check_permission("postgres.write")?;

    let args = arg_strings(executor, call.args.clone())?;
    let [host, port, database, user, password] = args.as_slice() else {
        return Err("pg.pass.set expects <host> <port> <database> <user> <password>".into());
    };

    let path = pgpass_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create '{}': {}", parent.display(), e))?;
    }

    let existing = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("Failed to read '{}': {}", path.display(), error)),
    };

    let result = upsert_pgpass_entry(&existing, host, port, database, user, password);
    fs::write(&path, result.content)
        .map_err(|e| format!("Failed to write '{}': {}", path.display(), e))?;

    let mut map = HashMap::new();
    map.insert("path".into(), Value::String(path.display().to_string()));
    map.insert("updated".into(), Value::Bool(result.updated));
    map.insert("added".into(), Value::Bool(!result.updated));
    Ok(Value::Object(map))
}

fn arg_strings(executor: &mut dyn PluginHost, args: Vec<Expr>) -> Result<Vec<String>, String> {
    args.into_iter()
        .map(|expr| executor.plugin_arg_value(expr).map(value_to_string))
        .collect()
}

fn run_postgres_command(
    program: &str,
    args: &[String],
    env: HashMap<String, String>,
    secret_values: Vec<String>,
) -> Result<Value, String> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(program.to_string());
    argv.extend(args.iter().cloned());

    exec_command(ExecRequest {
        command: format!("{} {}", program, args.join(" ")),
        argv: Some(argv),
        attempts: 1,
        timeout: None,
        wait_children: false,
        workdir: None,
        env,
        secret_values,
    })
}

fn passwordless_auth_env(mut env: HashMap<String, String>) -> HashMap<String, String> {
    env.insert("PGPASSWORD".into(), String::new());
    env.insert(
        "PGPASSFILE".into(),
        std::env::temp_dir()
            .join("zen-no-pgpass")
            .display()
            .to_string(),
    );
    env.entry("PGCONNECT_TIMEOUT".into())
        .or_insert_with(|| "5".into());
    env
}

fn passwordless_auth_result(database: &str, output: Value) -> Value {
    let Value::Object(output_map) = output else {
        return output;
    };

    let passwordless = matches!(output_map.get("success"), Some(Value::Bool(true)));
    let stdout = output_map
        .get("stdout")
        .and_then(Value::as_string)
        .unwrap_or("");
    let stderr = output_map
        .get("stderr")
        .and_then(Value::as_string)
        .unwrap_or("");
    let reason = if passwordless {
        "passwordless connection succeeded".into()
    } else {
        postgres_auth_reason(stderr, stdout)
    };

    let mut map = HashMap::new();
    map.insert("database".into(), Value::String(database.into()));
    map.insert("passwordless".into(), Value::Bool(passwordless));
    map.insert("success".into(), Value::Bool(passwordless));
    map.insert("reason".into(), Value::String(reason));

    for field in ["status", "exitcode", "stdout", "stderr"] {
        if let Some(value) = output_map.get(field) {
            map.insert(field.into(), value.clone());
        }
    }

    Value::Object(map)
}

fn postgres_auth_reason(stderr: &str, stdout: &str) -> String {
    let message = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };

    if message.contains("no password supplied") {
        return "no password supplied".into();
    }
    if message.contains("password authentication failed") {
        return "password authentication failed".into();
    }
    if message.is_empty() {
        return "psql connection failed".into();
    }

    message.lines().last().unwrap_or(message).trim().into()
}

fn value_to_string(value: Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) | Value::Secret(value) => value,
        other => format!("{:?}", other),
    }
}

fn pgpass_path() -> Result<PathBuf, String> {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return Ok(PathBuf::from(appdata)
            .join("postgresql")
            .join("pgpass.conf"));
    }

    if let Some(home) = dirs::home_dir() {
        return Ok(home
            .join("AppData")
            .join("Roaming")
            .join("postgresql")
            .join("pgpass.conf"));
    }

    Err("Could not resolve Windows pgpass path; APPDATA is not set".into())
}

struct PgPassUpsert {
    content: String,
    updated: bool,
}

fn upsert_pgpass_entry(
    existing: &str,
    host: &str,
    port: &str,
    database: &str,
    user: &str,
    password: &str,
) -> PgPassUpsert {
    let entry = format!(
        "{}:{}:{}:{}:{}",
        escape_pgpass_field(host),
        escape_pgpass_field(port),
        escape_pgpass_field(database),
        escape_pgpass_field(user),
        escape_pgpass_field(password)
    );
    let mut updated = false;
    let mut lines = Vec::new();

    for line in existing.lines() {
        if !updated && pgpass_line_matches(line, host, port, database, user) {
            lines.push(entry.clone());
            updated = true;
        } else {
            lines.push(line.to_string());
        }
    }

    if !updated {
        lines.push(entry);
    }

    PgPassUpsert {
        content: format!("{}\n", lines.join("\n")),
        updated,
    }
}

fn pgpass_line_matches(line: &str, host: &str, port: &str, database: &str, user: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }

    let fields = split_pgpass_line(line);
    fields.len() >= 5
        && fields[0] == host
        && fields[1] == port
        && fields[2] == database
        && fields[3] == user
}

fn escape_pgpass_field(value: &str) -> String {
    value.replace('\\', "\\\\").replace(':', "\\:")
}

fn split_pgpass_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ':' => {
                fields.push(current);
                current = String::new();
            }
            _ => current.push(ch),
        }
    }

    fields.push(current);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermissionSet;

    #[test]
    fn postgres_plugin_ignores_unknown_calls() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = PostgresPlugin
            .call(
                &mut executor,
                &FunctionCall {
                    name: vec!["not_pg".into()],
                    args: Vec::new(),
                    config: None,
                },
                &Value::Null,
            )
            .unwrap();

        assert!(!result.is_handled());
    }

    #[test]
    fn pg_query_requires_postgres_read_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = PostgresPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["pg".into(), "query".into()],
                args: vec![Expr::String("main".into()), Expr::String("select 1".into())],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("postgres.read")),
            Ok(_) => panic!("Expected pg.query to require postgres.read"),
        }
    }

    #[test]
    fn pg_auth_passwordless_requires_postgres_read_permission() {
        let mut executor = Executor::new_with_permissions(PermissionSet::new(&Vec::new()));
        let result = PostgresPlugin.call(
            &mut executor,
            &FunctionCall {
                name: vec!["pg".into(), "auth".into(), "passwordless".into()],
                args: vec![Expr::String("postgres".into())],
                config: None,
            },
            &Value::Null,
        );

        match result {
            Err(err) => assert!(err.contains("postgres.read")),
            Ok(_) => panic!("Expected pg.auth.passwordless to require postgres.read"),
        }
    }

    #[test]
    fn passwordless_auth_env_disables_password_sources() {
        let mut env = HashMap::new();
        env.insert("PGPASSWORD".into(), "secret".into());
        env.insert("PGPASSFILE".into(), "real-pgpass".into());

        let env = passwordless_auth_env(env);

        assert_eq!(env.get("PGPASSWORD"), Some(&String::new()));
        assert!(env
            .get("PGPASSFILE")
            .is_some_and(|path| path.contains("zen-no-pgpass")));
        assert_eq!(env.get("PGCONNECT_TIMEOUT"), Some(&"5".into()));
    }

    #[test]
    fn passwordless_auth_result_summarizes_no_password() {
        let mut output = HashMap::new();
        output.insert("success".into(), Value::Bool(false));
        output.insert("exitcode".into(), Value::Number(2.0));
        output.insert("stdout".into(), Value::String(String::new()));
        output.insert(
            "stderr".into(),
            Value::String(
                "psql: error: connection to server failed: fe_sendauth: no password supplied\n"
                    .into(),
            ),
        );

        let result = passwordless_auth_result("postgres", Value::Object(output));
        let Value::Object(map) = result else {
            panic!("Expected object result");
        };

        assert!(matches!(map.get("passwordless"), Some(Value::Bool(false))));
        assert!(matches!(map.get("success"), Some(Value::Bool(false))));
        assert_eq!(
            map.get("reason").and_then(Value::as_string),
            Some("no password supplied")
        );
        assert_eq!(
            map.get("database").and_then(Value::as_string),
            Some("postgres")
        );
    }

    #[test]
    fn pgpass_escapes_colons_and_backslashes() {
        assert_eq!(escape_pgpass_field(r"a:b\c"), r"a\:b\\c");
    }

    #[test]
    fn pgpass_split_unescapes_fields() {
        assert_eq!(
            split_pgpass_line(r"local\:host:5432:fireworks:post\:gres:p\\w"),
            vec!["local:host", "5432", "fireworks", "post:gres", r"p\w"]
        );
    }

    #[test]
    fn pgpass_upsert_adds_new_entry() {
        let result = upsert_pgpass_entry("", "localhost", "5432", "fireworks", "postgres", "pw");

        assert!(!result.updated);
        assert_eq!(result.content, "localhost:5432:fireworks:postgres:pw\n");
    }

    #[test]
    fn pgpass_upsert_replaces_matching_entry() {
        let existing = "localhost:5432:fireworks:postgres:old\nother:5432:db:user:pw\n";
        let result = upsert_pgpass_entry(
            existing,
            "localhost",
            "5432",
            "fireworks",
            "postgres",
            "new",
        );

        assert!(result.updated);
        assert_eq!(
            result.content,
            "localhost:5432:fireworks:postgres:new\nother:5432:db:user:pw\n"
        );
    }
}
