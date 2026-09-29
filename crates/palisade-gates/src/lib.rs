//! `palisade-gates` — the `Analyzed` gate kind.
//!
//! An `Analyzed` gate is a pure function of an [`Observation`]:
//!
//! ```text
//! fn(&GateContext) -> GateResult
//! ```
//!
//! No I/O, no process, no clock, no network. That is what makes it
//! calibratable — a gate whose output depends on when it ran, or on what else
//! the machine was doing, has no meaningful false-positive rate, and a
//! false-positive rate is the number that earns a gate the right to block.
//!
//! The two-tree comparison is the reason the observation carries file
//! contents on both sides. `git show <base>:<path>` is a subprocess, and a
//! gate may not spawn one, so the fetch happens once during capture and the
//! *result* is part of what the gate receives.
//!
//! This crate has no `std::process` in its dependency graph, and CI fails the
//! build if it ever does.

pub mod dependency_surface;
pub mod paths_unchanged;
pub mod registry;
pub mod tests_not_deleted;

use palisade_contract::Gate;
use palisade_observe::Observation;
use palisade_orchestrate::{Finding, UntrustworthyReason};

/// What a gate returns.
///
/// Three cases, and the third is the one a gate author has to think hardest
/// about: a gate that cannot tell must say so rather than returning an empty
/// finding list, which is indistinguishable from "checked, found nothing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateResult {
    /// The gate ran and found nothing to report.
    Clean,
    /// The gate ran and found these. The verdict depends on their severities.
    Findings(Vec<Finding>),
    /// The gate could not reach a trustworthy conclusion.
    Untrustworthy(UntrustworthyReason),
}

impl GateResult {
    /// Convenience for the common case of no findings.
    pub fn findings(findings: Vec<Finding>) -> Self {
        if findings.is_empty() {
            Self::Clean
        } else {
            Self::Findings(findings)
        }
    }
}

/// Everything one gate invocation is allowed to see.
#[derive(Debug, Clone, Copy)]
pub struct GateContext<'a> {
    /// The gate's own declaration: id, severity, and whatever it was
    /// configured with.
    pub gate: &'a Gate,
    /// The bounded, two-tree observation.
    pub observation: &'a Observation,
}

impl<'a> GateContext<'a> {
    /// The origin every finding from this gate carries. Always
    /// `Analyzed`: a gate that is not analyzed should not be in this crate.
    pub fn origin(&self) -> palisade_orchestrate::Origin {
        palisade_orchestrate::Origin::Analyzed {
            primitive: self.gate.primitive,
        }
    }
}

/// The signature every `Analyzed` gate implements.
pub type AnalyzedGate = fn(&GateContext<'_>) -> GateResult;

/// Gates that need file contents to be uncapped, because a half-parsed
/// manifest is a wrong answer rather than a partial one. They return
/// `Untrustworthy` when the observation says a side was clipped.
pub(crate) fn truncated(path: &str) -> UntrustworthyReason {
    UntrustworthyReason::Indeterminate {
        detail: format!(
            "{path} exceeds the per-file observation cap, so this gate cannot \
             compare it. Raise the cap, or narrow the gate to a smaller surface."
        ),
    }
}
