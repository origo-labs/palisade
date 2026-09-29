//! `palisade-gates` — the `Analyzed` gate kind. M0 stub. **Empty on purpose.**
//!
//! An `Analyzed` gate is a pure function of an `Observation`:
//!
//! ```text
//! fn(&Observation) -> Vec<Finding>
//! ```
//!
//! No I/O, no process, no clock, no network. That is what makes it
//! calibratable — a gate whose output depends on when it ran or on what else
//! the machine was doing has no meaningful false-positive rate, and a
//! false-positive rate is the number that earns a gate the right to block.
//!
//! M1 lands the two-tree diff gates; M2 the AST-backed ones.
//!
//! This crate has no `std::process` in its dependency graph, and CI fails the
//! build if it ever does.

use palisade_orchestrate::Finding;

/// The signature every `Analyzed` gate implements. Not a trait in M0: the
/// registry is empty and a trait with no implementors is a guess. M1 lands it
/// with the first two-tree gates, when there is something to be a trait *of*.
pub type AnalyzedGate = fn(&palisade_observe::Observation) -> Vec<Finding>;
