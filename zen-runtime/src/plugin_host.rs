use crate::capabilities::{CapabilityGrant, CapabilityRequest};
use crate::events::EventSink;
use crate::secret_store::SecretStore;

/// What a plugin is allowed to ask of its host, per `RUNTIME-INTERFACES.md`
/// §2 in the Flux docs - `use_capability`, `emit` (already `EventSink`), and
/// `secret` (already `SecretStore`) bundled behind one handle, `EventSink`/
/// `SecretStore` as supertraits so a caller holds a single `&mut dyn
/// PluginHost` instead of three separate borrows (same pattern
/// `WorkflowHost` already uses).
///
/// This is a *different* trait from Zen's own `runtime::plugin::PluginHost`
/// (the `Expr`-based, ~50-method interface `ZenPlugin::call` already takes) -
/// an unfortunate name collision across two crates, not the same type. That
/// trait stays exactly as it is; none of Zen's plugins call this one. This
/// one exists so a future Flux plugin caller has a narrow, Flux-shaped
/// interface to depend on, proven here only against `Executor`.
pub trait PluginHost: EventSink + SecretStore {
    fn use_capability(&mut self, cap: &CapabilityRequest) -> Result<CapabilityGrant, String>;
}
