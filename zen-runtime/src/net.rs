//! Request shape for `effects::Effect::Net`. Unlike `process.rs` (and the
//! `Fs` operations in `effects.rs`), this module holds no implementation -
//! performing a real network request needs an HTTP client dependency, and
//! `EXTRACTION-PLAN.md`'s dependency-footprint guardrail places that at the
//! language/surface layer (`ureq` already lives in the `zen` crate for
//! `dropbox.rs`, not here). Each caller that wants `Effect::Net` to
//! actually do something implements its own `Effects` performer using its
//! own HTTP client crate - see `flux-lang/src/effects.rs`'s `NetEffects`
//! for the first one.

use std::time::Duration;

pub struct NetRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub timeout: Option<Duration>,
}
