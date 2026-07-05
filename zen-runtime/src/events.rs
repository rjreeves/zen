/// A single point-in-time occurrence a language surface or the runtime wants
/// published - e.g. a workflow's `step.succeeded`. Kept plain data so any
/// `EventSink` (in-memory log, SQLite-backed journal, a future bus) can
/// decide what to do with it.
pub struct Event {
    pub name: String,
    pub workflow: String,
    pub step: Option<String>,
    pub attempt: Option<usize>,
}

/// The transition/signal bus a language surface publishes onto. See
/// `RUNTIME-INTERFACES.md` §6 in the Flux docs - this is also how durable
/// suspensions get woken (a signal is an event), though nothing in this repo
/// wires suspension/wakeup yet.
pub trait EventSink {
    fn emit(&mut self, event: &Event) -> Result<(), String>;
}
