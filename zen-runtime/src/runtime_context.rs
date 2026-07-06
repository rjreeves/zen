use crate::effects::Effects;
use crate::journal::Journal;
use crate::plugin_host::PluginHost;

/// A language surface's entire view of the runtime, bundled behind one
/// handle - see `RUNTIME-INTERFACES.md`'s closing section in the Flux docs.
///
/// The illustrative shape there holds `caps`, `effects`, `journal`,
/// `plugins`, `events` as five *independent* `&mut dyn Trait` fields. That
/// doesn't construct for any concrete type backing more than one of the
/// bundled traits itself - which `Executor` does: `plugins: &mut dyn
/// PluginHost` needs to borrow the whole `Executor`, conflicting with a
/// separate `caps: &mut dyn Capabilities` field pointing at
/// `self.permissions`, which is *part of* that same `Executor`. A genuine
/// Rust aliasing conflict, not a style choice - the same category of
/// problem `WorkflowHost` already solved by using supertraits instead of
/// separate fields.
///
/// `PluginHost` (Stage 5) already bundles `use_capability`/`emit`/`secret`
/// behind one handle, so `caps` and `events` collapse into `plugins` here
/// rather than staying separate fields. `journal` and `effects` stay
/// separate because they're genuinely backed by different objects
/// (`WorkflowPersistence`, a stateless `ProcessEffects`) that never alias
/// whatever backs `plugins` - `journal` is `None` when a run isn't
/// persisted, matching `WorkflowEngine::run` vs `run_persisted` today.
pub struct RuntimeContext<'a> {
    pub plugins: &'a mut dyn PluginHost,
    pub effects: &'a mut dyn Effects,
    pub journal: Option<&'a mut dyn Journal>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::CapabilityRequest;
    use crate::effects::ProcessEffects;
    use crate::events::Event;
    use crate::secret_store::SecretStore;

    struct StubHost;

    impl SecretStore for StubHost {
        fn read_secret(&self, _name: &str) -> Result<Option<String>, String> {
            Ok(None)
        }
    }

    impl crate::events::EventSink for StubHost {
        fn emit(&mut self, _event: &Event) -> Result<(), String> {
            Ok(())
        }
    }

    impl PluginHost for StubHost {
        fn use_capability(
            &mut self,
            cap: &CapabilityRequest,
        ) -> Result<crate::capabilities::CapabilityGrant, String> {
            Ok(crate::capabilities::CapabilityGrant {
                kind: cap.kind.clone(),
                resource: cap.resource.clone(),
            })
        }
    }

    #[test]
    fn runtime_context_constructs_without_aliasing_conflict() {
        let mut host = StubHost;
        let mut effects = ProcessEffects;

        let mut ctx = RuntimeContext {
            plugins: &mut host,
            effects: &mut effects,
            journal: None,
        };

        let grant = ctx
            .plugins
            .use_capability(&CapabilityRequest::new("proc.exec"))
            .unwrap();
        assert_eq!(grant.kind, "proc.exec");
        assert!(ctx.journal.is_none());
    }
}
