//! Which primitive is implemented by which function, and what happens to one
//! that is not.
//!
//! The mapping is exhaustive over [`ALL_PRIMITIVES`] and a `match` with no
//! wildcard, so adding a primitive to the contract vocabulary is a compile
//! error here until somebody decides what it does. That is the point: a
//! primitive nobody has thought about must not quietly become a gate that
//! always passes.

use rulebound_contract::{ALL_PRIMITIVES, Primitive};
use rulebound_orchestrate::{Origin, UntrustworthyReason};

use crate::{AnalyzedGate, GateContext, GateResult};

/// The `Analyzed` implementation of `p`, or `None` if it lands in a later
/// milestone.
pub fn analyzed(p: Primitive) -> Option<AnalyzedGate> {
    match p {
        Primitive::DependencySurfaceUnchanged => Some(crate::dependency_surface::run),
        Primitive::TestsNotDeleted => Some(crate::tests_not_deleted::run),
        Primitive::PathsUnchanged => Some(crate::paths_unchanged::run),
        Primitive::PublicApiUnchanged => Some(crate::public_api::run),
        Primitive::UnsafeSurfaceUnchanged => Some(crate::unsafe_surface::run),
        Primitive::SuppressionsNotWidened => Some(crate::suppressions::run),
        Primitive::SecretAbsent => None, // M1.5
        Primitive::ChecksGreen | Primitive::ExternalTool => None, // M3, Delegated
        Primitive::Judged => None,       // M6, escalate-only
        Primitive::ContractNotLoosened => Some(crate::contract_not_loosened::run),
        Primitive::ContractReviewStale => Some(crate::contract_review_stale::run),
    }
}

/// Run a gate by primitive.
///
/// An unimplemented primitive yields `Untrustworthy`, never `Clean`. A project
/// that declares `public_api_unchanged` in an M1 build gets a loud failure
/// saying the primitive is not implemented here, which is a true statement,
/// rather than a green run that implies a check nobody performed.
pub fn dispatch(primitive: Primitive, ctx: &GateContext<'_>) -> GateResult {
    match analyzed(primitive) {
        Some(run) => run(ctx),
        None => GateResult::Untrustworthy(UntrustworthyReason::Unimplemented { primitive }),
    }
}

/// Primitives this build can actually run.
///
/// Excludes `SecretAbsent` on purpose: it is declared, parseable, and refused,
/// so a project that wants it gets a loud `Untrustworthy` rather than a
/// pattern scan nobody calibrated.
pub fn implemented() -> Vec<Primitive> {
    ALL_PRIMITIVES
        .iter()
        .copied()
        .filter(|p| analyzed(*p).is_some())
        .collect()
}

/// The origin a `Delegated` primitive would carry. Lives here so the
/// *unimplemented* path can still report who was supposed to produce the
/// verdict, which is more useful than "nobody".
pub fn unimplemented_origin(primitive: Primitive) -> Origin {
    Origin::Analyzed { primitive }
}
