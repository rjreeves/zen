use crate::interrupt;
use crate::runtime::values::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use sysinfo::System;

pub struct ExecRequest {
    pub command: String,
    pub argv: Option<Vec<String>>,
    pub attempts: usize,
    pub timeout: Option<Duration>,
    pub wait_children: bool,
    pub workdir: Option<String>,
    pub env: HashMap<String, String>,
}

struct ExecOutput {
    status_code: i32,
    stdout: String,
    stderr: String,
    timed_out: bool,
    cancelled: bool,
}

pub fn exec_command(request: ExecRequest) -> Result<Value, String> {
    let mut last_output = None;

    for attempt in 1..=request.attempts {
        let output = run_command_once(&request)?;

        if output.status_code == 0 {
            return Ok(exec_output_to_value(output, attempt));
        }

        last_output = Some((output, attempt));
    }

    let (output, attempt) = last_output.ok_or("exec did not run")?;
    Ok(exec_output_to_value(output, attempt))
}

fn run_command_once(request: &ExecRequest) -> Result<ExecOutput, String> {
    run_command_spawned(request)
}

fn run_command_spawned(request: &ExecRequest) -> Result<ExecOutput, String> {
    let label = request.command.as_str();
    let mut command = command_builder(request)?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(original_error)
            if request.argv.is_some() && original_error.kind() == ErrorKind::NotFound =>
        {
            let mut fallback =
                command_builder_shell(&request.command, request.workdir.as_deref(), &request.env)?;
            fallback.stdout(Stdio::piped()).stderr(Stdio::piped());
            fallback.spawn().map_err(|fallback_error| {
                format!(
                    "Failed to execute command '{}': {}; shell fallback failed: {}",
                    label, original_error, fallback_error
                )
            })?
        }
        Err(error) => return Err(format!("Failed to execute command '{}': {}", label, error)),
    };

    let root_pid = child.id();
    let mut known_descendants = HashSet::new();
    let start = Instant::now();
    let mut main_status = None;

    loop {
        let live_descendants = if request.wait_children {
            live_descendant_pids(root_pid, &mut known_descendants)
        } else {
            HashSet::new()
        };

        if main_status.is_none() {
            main_status = child
                .try_wait()
                .map_err(|e| format!("Failed to wait for command '{}': {}", label, e))?;
        }

        if main_status.is_some() && live_descendants.is_empty() {
            let output = child
                .wait_with_output()
                .map_err(|e| format!("Failed to collect command output '{}': {}", label, e))?;

            return Ok(ExecOutput {
                status_code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                timed_out: false,
                cancelled: false,
            });
        }

        if request
            .timeout
            .is_some_and(|timeout| start.elapsed() >= timeout)
        {
            let _ = child.kill();
            if request.wait_children {
                kill_processes(&known_descendants);
            }
            let output = child
                .wait_with_output()
                .map_err(|e| format!("Failed to collect timed out command '{}': {}", label, e))?;

            return Ok(ExecOutput {
                status_code: -1,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                timed_out: true,
                cancelled: false,
            });
        }

        if interrupt::is_interrupted() {
            let _ = child.kill();
            if request.wait_children {
                kill_processes(&known_descendants);
            }
            let output = child
                .wait_with_output()
                .map_err(|e| format!("Failed to collect interrupted command '{}': {}", label, e))?;

            return Ok(ExecOutput {
                status_code: -1,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                timed_out: false,
                cancelled: true,
            });
        }

        thread::sleep(Duration::from_millis(25));
    }
}

fn live_descendant_pids(root_pid: u32, known_descendants: &mut HashSet<u32>) -> HashSet<u32> {
    let mut system = System::new_all();
    system.refresh_all();

    let mut parent_by_pid = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            parent_by_pid.insert(pid.as_u32(), parent.as_u32());
        }
    }

    let mut descendants = HashSet::new();
    loop {
        let before = descendants.len();
        for (pid, parent) in &parent_by_pid {
            if *parent == root_pid
                || descendants.contains(parent)
                || known_descendants.contains(parent)
            {
                descendants.insert(*pid);
            }
        }

        if descendants.len() == before {
            break;
        }
    }

    known_descendants.extend(descendants.iter().copied());
    descendants
}

fn kill_processes(pids: &HashSet<u32>) {
    if pids.is_empty() {
        return;
    }

    let mut system = System::new_all();
    system.refresh_all();

    for (pid, process) in system.processes() {
        if pids.contains(&pid.as_u32()) {
            let _ = process.kill();
        }
    }
}

fn command_builder(request: &ExecRequest) -> Result<Command, String> {
    if let Some(argv) = request.argv.as_deref() {
        command_builder_argv(argv, request.workdir.as_deref(), &request.env)
    } else {
        command_builder_shell(&request.command, request.workdir.as_deref(), &request.env)
    }
}

fn command_builder_argv(
    argv: &[String],
    workdir: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<Command, String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("External command resolved to an empty argv".into());
    };

    let program = resolve_argv_program(program, workdir);
    let mut cmd = Command::new(program);
    cmd.args(args);
    apply_command_options(&mut cmd, workdir, env)?;
    Ok(cmd)
}

fn resolve_argv_program(program: &str, workdir: Option<&str>) -> PathBuf {
    let path = Path::new(program);
    if path.is_absolute() || !is_path_like_program(program) {
        return path.to_path_buf();
    }

    workdir
        .map(|dir| Path::new(dir).join(path))
        .unwrap_or_else(|| path.to_path_buf())
}

fn is_path_like_program(program: &str) -> bool {
    program.starts_with('.')
        || program.starts_with('/')
        || program.starts_with('\\')
        || program.contains('/')
        || program.contains('\\')
}

fn command_builder_shell(
    command: &str,
    workdir: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<Command, String> {
    let mut cmd = if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.arg("/S").arg("/C");
        push_windows_raw_arg(&mut cmd, &windows_shell_safe_command(command));
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    };

    apply_command_options(&mut cmd, workdir, env)?;
    Ok(cmd)
}

/// `cmd /C`'s quote handling only preserves quotes on the command line when
/// it has exactly two quote characters total (a single bare quoted
/// executable name, no quoted arguments). Otherwise, if the line starts with
/// a quote character, cmd strips that leading quote and the *last* quote
/// anywhere on the line - not its match - which corrupts a quoted path that
/// itself contains a space once any further quoted argument is present
/// (e.g. `"C:\Program Files\app.exe" --flag "value with space"`).
/// Prefixing with `call` keeps the line from starting with a quote at all,
/// so none of that stripping logic triggers.
fn windows_shell_safe_command(command: &str) -> String {
    if command.starts_with('"') {
        format!("call {}", command)
    } else {
        command.to_string()
    }
}

#[cfg(windows)]
fn push_windows_raw_arg(cmd: &mut Command, arg: &str) {
    use std::os::windows::process::CommandExt;

    cmd.raw_arg(arg);
}

#[cfg(not(windows))]
fn push_windows_raw_arg(_cmd: &mut Command, _arg: &str) {}

fn apply_command_options(
    cmd: &mut Command,
    workdir: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<(), String> {
    if let Some(path) = workdir {
        let metadata =
            fs::metadata(path).map_err(|e| format!("Invalid exec workdir '{}': {}", path, e))?;
        if !metadata.is_dir() {
            return Err(format!("Invalid exec workdir '{}': not a directory", path));
        }
        cmd.current_dir(path);
    }

    for (key, value) in env {
        cmd.env(key, value);
    }

    Ok(())
}

fn exec_output_to_value(output: ExecOutput, attempt: usize) -> Value {
    let exitcode = output.status_code;

    let mut map = HashMap::new();
    map.insert("success".into(), Value::Bool(exitcode == 0));
    map.insert("status".into(), Value::Number(exitcode as f64));
    map.insert("exitcode".into(), Value::Number(exitcode as f64));
    map.insert("attempts".into(), Value::Number(attempt as f64));
    map.insert("timed_out".into(), Value::Bool(output.timed_out));
    map.insert("cancelled".into(), Value::Bool(output.cancelled));
    map.insert("stdout".into(), Value::String(output.stdout));
    map.insert("stderr".into(), Value::String(output.stderr));

    Value::Object(map)
}

pub fn parse_duration(raw: &str) -> Result<Duration, String> {
    if let Some(ms) = raw.strip_suffix("ms") {
        return ms
            .parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|_| format!("Invalid timeout duration '{}'", raw));
    }

    if let Some(seconds) = raw.strip_suffix('s') {
        return seconds
            .parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|_| format!("Invalid timeout duration '{}'", raw));
    }

    if let Some(minutes) = raw.strip_suffix('m') {
        return minutes
            .parse::<u64>()
            .map(|minutes| Duration::from_secs(minutes * 60))
            .map_err(|_| format!("Invalid timeout duration '{}'", raw));
    }

    raw.parse::<u64>()
        .map(Duration::from_secs)
        .map_err(|_| format!("Invalid timeout duration '{}'", raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn argv_exec_resolves_relative_program_against_workdir() {
        let current_exe = env::current_exe().unwrap();
        let workdir = current_exe.parent().unwrap().to_string_lossy().into_owned();
        let program = format!("./{}", current_exe.file_name().unwrap().to_string_lossy());

        let output = exec_command(ExecRequest {
            command: format!("{} --help", program),
            argv: Some(vec![program, "--help".into()]),
            attempts: 1,
            timeout: Some(Duration::from_secs(10)),
            wait_children: false,
            workdir: Some(workdir),
            env: HashMap::new(),
        })
        .unwrap();

        let Value::Object(map) = output else {
            panic!("Expected exec output object");
        };

        assert!(matches!(map.get("success"), Some(Value::Bool(true))));
    }

    #[test]
    fn argv_exec_keeps_first_program_token_with_spaces_intact() {
        let current_exe = env::current_exe().unwrap();
        let workdir_path = env::temp_dir().join(format!("zen argv space {}", std::process::id()));
        fs::create_dir_all(&workdir_path).unwrap();

        let program_name = if cfg!(windows) {
            "my app.exe"
        } else {
            "my app"
        };
        let copied_exe = workdir_path.join(program_name);
        fs::copy(&current_exe, &copied_exe).unwrap();

        let output = exec_command(ExecRequest {
            command: format!("\"./{}\" --help", program_name),
            argv: Some(vec![format!("./{}", program_name), "--help".into()]),
            attempts: 1,
            timeout: Some(Duration::from_secs(10)),
            wait_children: false,
            workdir: Some(workdir_path.to_string_lossy().into_owned()),
            env: HashMap::new(),
        })
        .unwrap();

        let Value::Object(map) = output else {
            panic!("Expected exec output object");
        };

        assert!(matches!(map.get("success"), Some(Value::Bool(true))));
        let _ = fs::remove_file(copied_exe);
        let _ = fs::remove_dir(workdir_path);
    }

    #[cfg(windows)]
    #[test]
    fn argv_exec_shell_fallback_preserves_spaced_args_for_cmd_helpers() {
        let workdir_path = env::temp_dir().join(format!("zen cmd fallback {}", std::process::id()));
        fs::create_dir_all(&workdir_path).unwrap();
        let helper = workdir_path.join("msgbox.cmd");
        fs::write(
            &helper,
            "@echo off\r\necho first=[%~1]\r\necho second=[%~2]\r\necho third=[%~3]\r\n",
        )
        .unwrap();

        // argv[0] stays a bare, extension-less name on purpose: CreateProcess
        // can't resolve it directly (no PATHEXT-style extension search of its
        // own), so the argv-direct attempt below is guaranteed to fail and
        // trigger the shell fallback this test exists to exercise.
        //
        // The `command` string below (used only by that fallback) references
        // the helper by its full path instead of the bare name "msgbox".
        // cmd.exe's own bare-name resolution also searches its current
        // directory, but that implicit search is disabled on any machine with
        // NoDefaultCurrentDirectoryInExePath set - using an explicit path
        // sidesteps that entirely and can't collide with an unrelated
        // same-named executable elsewhere on PATH.
        //
        // This path starts with a quote character (it contains a space), so
        // it also exercises windows_shell_safe_command's `call` guard -
        // without it, cmd's /C quote handling would strip the leading quote
        // and the last quote anywhere on the line, corrupting the path.
        let command = format!("\"{}\" \"a b\" \"D \"", helper.display());
        let output = exec_command(ExecRequest {
            command,
            argv: Some(vec!["msgbox".into(), "a b".into(), "D ".into()]),
            attempts: 1,
            timeout: Some(Duration::from_secs(10)),
            wait_children: false,
            workdir: Some(workdir_path.to_string_lossy().into_owned()),
            env: HashMap::new(),
        })
        .unwrap();

        let Value::Object(map) = output else {
            panic!("Expected exec output object");
        };

        assert!(matches!(map.get("success"), Some(Value::Bool(true))));
        match map.get("stdout") {
            Some(Value::String(value)) => assert!(
                value.contains("first=[a b]") && value.contains("second=[D"),
                "expected spaced args to stay intact, got {value:?}"
            ),
            other => panic!("Expected stdout string, got {:?}", other),
        }

        let _ = fs::remove_file(helper);
        let _ = fs::remove_dir(workdir_path);
    }

    #[cfg(windows)]
    #[test]
    fn shell_command_preserves_quoted_path_with_quoted_argument() {
        // Real-world shape this protects: a `run:` step with no structured
        // argv at all (e.g. `run: "C:\Program Files\app.exe" --flag "value
        // with space"`), which goes straight to command_builder_shell -
        // never through the argv-fallback path the test above exercises.
        let workdir_path = env::temp_dir().join(format!("zen shell quote {}", std::process::id()));
        fs::create_dir_all(&workdir_path).unwrap();
        let helper = workdir_path.join("msgbox.cmd");
        fs::write(&helper, "@echo off\r\necho first=[%~1]\r\n").unwrap();

        let command = format!("\"{}\" \"a b\"", helper.display());
        let output = exec_command(ExecRequest {
            command,
            argv: None,
            attempts: 1,
            timeout: Some(Duration::from_secs(10)),
            wait_children: false,
            workdir: Some(workdir_path.to_string_lossy().into_owned()),
            env: HashMap::new(),
        })
        .unwrap();

        let Value::Object(map) = output else {
            panic!("Expected exec output object");
        };

        assert!(matches!(map.get("success"), Some(Value::Bool(true))));
        match map.get("stdout") {
            Some(Value::String(value)) => assert!(
                value.contains("first=[a b]"),
                "expected quoted path with a quoted argument to run correctly, got {value:?}"
            ),
            other => panic!("Expected stdout string, got {:?}", other),
        }

        let _ = fs::remove_file(helper);
        let _ = fs::remove_dir(workdir_path);
    }
}
