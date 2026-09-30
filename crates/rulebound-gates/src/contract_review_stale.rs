//! `contract_review_stale` — is the review date still fresh?
//!
//! A file nobody reviews is worse than no file, because it manufactures
//! confidence (PRD §10). A `reviewed` date that nothing reads is a comment, so
//! this gate reads it.
//!
//! Two rules, and the second is the one that matters:
//!
//! 1. A review older than the interval is a finding.
//! 2. **A missing or unparseable date is also a finding.** An unparseable date
//!    is not a fresh one, and a gate that only checked the interval would let
//!    `[judgement] reviewed = "soon"` pass forever.
//!
//! The interval is a constant and not a contract key, on purpose: a mechanism
//!    whose trigger the project can set to `never` is a comment.

use rulebound_contract::JudgementSection;
use rulebound_orchestrate::{Finding, Side, Subject, SubjectKind};

use crate::{GateContext, GateResult};

/// The gate.
pub fn run(ctx: &GateContext<'_>) -> GateResult {
    let interval = JudgementSection::DEFAULT_REVIEW_INTERVAL_DAYS;

    // The contract is part of the observation, and its `judgement` section is
    // the only thing this gate needs. Read it from the head side.
    let head = match ctx.observation.file("rulebound.toml") {
        Some(view) => match &view.head {
            Some(src) => src,
            None => return GateResult::Clean, // contract deleted; another gate's business
        },
        None => return GateResult::Clean,
    };
    let Ok(contract) = rulebound_contract::parse::parse_contract(head) else {
        // An invalid contract is already `error` from the check that loaded
        // it. Saying it twice, with a different reason, would be noise.
        return GateResult::Clean;
    };

    let section = &contract.judgement;
    let subject = || Subject::new(SubjectKind::Contract, "[judgement].reviewed");

    let Some(days) = section.staleness_days(ctx.now_unix) else {
        return GateResult::findings(vec![Finding::new(
            ctx.gate.id.clone(),
            ctx.gate.primitive,
            ctx.gate.severity,
            subject(),
            Some("rulebound.toml".into()),
            None,
            Side::value("a YYYY-MM-DD review date"),
            Side::value(
                section
                    .reviewed
                    .clone()
                    .unwrap_or_else(|| "(absent)".to_string()),
            ),
            // A missing date never reaches here: it is rejected when the
            // contract loads, so the whole run is `error`. This branch is the
            // one a load-time check cannot catch.
            "the contract's review date is not a YYYY-MM-DD date. An \
             unparseable date is not a fresh one: a gate that only checked the \
             interval would let `reviewed = \"soon\"` pass forever.",
            ctx.origin(),
        )]);
    };

    if days <= interval {
        return GateResult::Clean;
    }

    GateResult::findings(vec![Finding::new(
        ctx.gate.id.clone(),
        ctx.gate.primitive,
        ctx.gate.severity,
        subject(),
        Some("rulebound.toml".into()),
        None,
        Side::value(format!("reviewed within {interval} days")),
        Side::value(format!("reviewed {days} days ago")),
        format!(
            "the contract was last reviewed {days} days ago, beyond the \\
             {interval}-day interval. A gate suite nobody re-reads stops \\
             matching the codebase it guards."
        ),
        ctx.origin(),
    )])
}
