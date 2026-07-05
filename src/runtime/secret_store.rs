/// Interface for looking up a secret by name.
///
/// The workflow engine's `{ secret: "name" }` env references used to call
/// `crate::runtime::plugins::secrets::read_secret` directly, which pulled
/// the whole secrets plugin module (and its `ZenPlugin`/`Executor` coupling)
/// into the workflow engine's dependency surface. Going through this trait
/// instead means workflow code depends on "something that can look up a
/// secret by name", not the concrete secrets module.
pub trait SecretStore {
    fn read_secret(&self, name: &str) -> Result<Option<String>, String>;
}
