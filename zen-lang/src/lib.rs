//! Zen language front end and runtime: lexer, parser, AST, and the .fg
//! interpreter (executor, plugins). Decoupled from the CLI (`zen-cli`) and
//! from the durable workflow engine internals (`zen-runtime`).

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod terminal;

// `crate::permissions` and `crate::interrupt` are referenced throughout
// executor.rs and the plugins/ tree (originally reachable there only
// because the old root crate's main.rs did `use zen_runtime::{interrupt,
// permissions};` at the crate root, and descendant modules can see an
// ancestor's private `use`). Re-declare that same trick here so those
// call sites keep working unchanged after the move.
use zen_runtime::interrupt;
use zen_runtime::permissions;
