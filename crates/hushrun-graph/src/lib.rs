//! \[ADR 0145\] hushrun's static map of Rust: which sources each test function can reach (C-147 to
//! C-151), so that a test that already passed is reused when nothing it reaches changed, with no
//! coverage and no tracing.
//!
//! - [`items`] splits a `.rs` file into items with tree-sitter's Rust grammar.
//! - [`scip`] reads rust-analyzer's SCIP index, and [`symbol`] its symbols.
//! - [`external`] reads what a `macro_rules!` of another crate can name where it expands.
//! - [`graph`] links the items by the index's occurrences, closes on the safe side what the index
//!   can't show, and walks from a test to what it reaches, or says why it can't.
//!
//! The crate starts no tool: the caller runs rust-analyzer and hands over its index
//! (`hushrun::units::cargo_map`).

pub mod external;
pub mod graph;
pub mod items;
pub mod scip;
pub mod symbol;
