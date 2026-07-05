use std::collections::HashSet;

/// A request to use a capability. `kind` is the same dotted string Zen has
/// always used for permissions (e.g. `"proc.exec"`, `"time"`, `"rand"`).
/// `resource` is reserved for finer-grained checks a future caller (Flux, or
/// a later Zen change) may want - e.g. a specific path for `fs.read` - and is
/// unused by today's coarse-grained `PermissionSet` checks.
pub struct CapabilityRequest {
    pub kind: String,
    pub resource: Option<String>,
}

impl CapabilityRequest {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            resource: None,
        }
    }
}

pub enum CapabilityDecision {
    Granted,
    Denied(String),
    /// Reserved for a caller that wants to interactively confirm rather than
    /// hard-deny. `PermissionSet` never produces this today - Zen's own
    /// prompt-then-grant flow (see `cli.rs::confirm_repl_permissions`) lives
    /// a layer above `Capabilities::check`, calling `grant` itself once the
    /// user approves rather than expecting `check` to prompt internally.
    Prompt,
}

/// A capability being added to the grant set. Same `kind`/`resource` shape
/// as `CapabilityRequest`; `resource` is likewise unused by `PermissionSet`
/// today.
pub struct CapabilityGrant {
    pub kind: String,
    pub resource: Option<String>,
}

impl CapabilityGrant {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            resource: None,
        }
    }
}

/// The set of capability kinds currently granted. Thin wrapper (not a bare
/// `HashSet<String>`) so `Capabilities::granted` has a stable return type
/// independent of the underlying collection.
#[derive(Debug, Default)]
pub struct GrantSet(HashSet<String>);

impl GrantSet {
    pub fn contains(&self, kind: &str) -> bool {
        self.0.contains(kind)
    }

    pub fn insert(&mut self, kind: String) {
        self.0.insert(kind);
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.0.iter()
    }
}

/// Checks and grants for a single kind of ambient authority (a "capability").
/// Exposed so either a static checker (Flux, at compile time) or a runtime
/// prompt (Zen, at execution time) can drive the same grant model - see
/// `RUNTIME-INTERFACES.md` §3 in the Flux docs.
pub trait Capabilities {
    fn check(&self, req: &CapabilityRequest) -> CapabilityDecision;
    fn grant(&mut self, grant: CapabilityGrant);
    fn granted(&self) -> &GrantSet;
}
