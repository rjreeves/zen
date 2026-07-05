use crate::script_runner::ScriptRunner;
use crate::secret_store::SecretStore;
use std::path::{Path, PathBuf};

/// The surface the workflow engine needs from its host. Narrower than
/// `PluginHost` (which also carries the `.fg` interpreter's `Expr`-based
/// builtin dispatch, used by the general plugin system) - the workflow
/// engine never evaluates an `Expr` itself, so it only needs permission
/// checks, path resolution, and the two path roots it plans commands
/// against. `ScriptRunner`/`SecretStore` are supertraits so the engine can
/// hold a single `&mut dyn WorkflowHost` instead of three separate `&mut`
/// borrows of the same host.
pub trait WorkflowHost: ScriptRunner + SecretStore {
    fn check_permission(&self, permission: &str) -> Result<(), String>;
    fn resolve_workspace_path(&self, path: &str) -> Result<PathBuf, String>;
    fn resolve_local_write_path(&self, path: &str) -> Result<PathBuf, String>;
    fn workspace_root_path(&self) -> &Path;
    fn cwd_path(&self) -> &Path;
}
