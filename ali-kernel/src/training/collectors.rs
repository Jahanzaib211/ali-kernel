//! Training data collectors — the flywheel that improves the local model over time.
//!
//! # Architecture (compiler query system pattern)
//!
//! | Compiler | Ali Kernel Training |
//! |---|---|
//! | Query cache (input hash → result) | Tool pair cache (tool+args_hash → output) |
//! | Incremental recompilation | Cached tool executions skip re-logging |
//! | DefPathHash (stable across sessions) | Content-addressed training pairs |
//! | Self-hosting (rustc builds rustc) | Local model generates synthetic data for itself |
//!
//! # Collectors
//!
//! - **tool_pairs**: Every tool execution logged as (task, tool, args, result). Teach when/how to call tools.
//! - **eval_pairs**: Eval failures → local model generates gold answer → DPO pair. Teach from mistakes.
//! - **synthetic**: Local model generates diverse training pairs for weak domains. Teach new capabilities.
//!
//! # Flywheel
//!
//! ```text
//! Tool execution → tool_pairs → training pool
//! Eval failures → eval_pairs → training pool (DPO)
//! Weak domains → synthetic → training pool (SFT)
//!                                    ↓
//!                          Local llama.cpp model
//!                                    ↓
//!                          Better tool execution + eval scores
//! ```

pub mod eval_pairs;
pub mod synthetic;
pub mod tool_pairs;

pub use eval_pairs::EvalPairCollector;
pub use synthetic::SyntheticCollector;
pub use tool_pairs::ToolPairCollector;
