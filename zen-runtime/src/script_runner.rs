use crate::values::Value;

/// Interface for running a `.fg` script and capturing its result.
///
/// The workflow engine's `zen:` steps used to call `Lexer`/`Parser`/
/// `Executor::execute_capture` directly, which is the one place workflow
/// execution reached into the interpreter. Going through this trait instead
/// means workflow code depends on "something that can run a script", not on
/// the concrete interpreter, which is the seam a future `zen-runtime` crate
/// would need.
pub trait ScriptRunner {
    fn run_capture(&mut self, source: &str) -> Result<Value, String>;
}
