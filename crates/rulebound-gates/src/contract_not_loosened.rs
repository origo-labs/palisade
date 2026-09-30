//! `contract_not_loosened` — did this diff weaken the contract without saying
//! why?
//!
//! This is the Goodhart defence, and it is the generalisation of `slop-gate`
//! decision 6. `slop-gate` audits the *other* tools' suppressions; this audits
//! *our own* configuration. A worker optimising against the gate suite rather
//! than against intent will not remove a requirement — it will set
//! `severity = "off"`, or add a name to `allow`, or delete the gate. All of
//! those are diff-visible, so all of them are checkable.
//!
//! What counts as a loosening, enumerated so the gate cannot quietly grow:
//!
//! | Change | Why it is a loosening |
//! | --- | --- |
//! | a gate deleted | the rule is gone |
//! | severity went down | `error` → `warn` is a rule that stopped blocking |
//! | a new `allow` entry | a documented exemption that is not documented |
//! | a new suppression | a silent exemption |
//! | a `not_covered` entry removed | the contract now claims *more* than before |
//! | severity raised to `error` with no `calibration` | an unmeasured claim promoted to blocking |
//!
//! The last row is the promotion guard, and it is the reason the analysis is
//! two-tree rather than a static check. Writing `severity = "error"` in a new
//! contract is an author making a claim; *promoting* a gate that was `warn` to
//! `error` in a diff is a claim that has to arrive with the measurement behind
//! it. A static rule would reject every contract until M5 exists, and would be
//! switched off before then.
//!
//! Every finding here is silenceable by a `[[change]]` record in the same
//! commit naming the gate and giving a non-blank reason. That is PRD 5: "A gate
//! may not be loosened without a reason in the same commit."

use std::collections::BTreeMap;

use rulebound_contract::parse::parse_contract;
use rulebound_contract::{Contract, Gate, GateId, Primitive, Severity};
use rulebound_orchestrate::{Finding, Side, Subject, SubjectKind, UntrustworthyReason};

use crate::{GateContext, GateResult};

/// Where the contract lives. Fixed, because a contract that can move is a
/// contract that can be escaped.
const CONTRACT_PATH: &str = "rulebound.toml";

/// Strictness order. A decrease in this number is a loosening; an increase to
/// `Error` is a promotion, which has its own rule.
///
/// `escalate` sits above `warn` because it routes to a human rather than
/// merely being recorded, and below `error` because it does not block.
const fn strictness(s: Severity) -> u8 {
    match s {
        Severity::Off => 0,
        Severity::Warn => 1,
        Severity::Escalate => 2,
        Severity::Error => 3,
    }
}

/// The gate.
pub fn run(ctx: &GateContext<'_>) -> GateResult {
    let Some(view) = ctx.observation.file(CONTRACT_PATH) else {
        // The contract did not change, so nothing was loosened. Sound for the
        // same reason as every other absence in this suite: the observation is
        // built from the diff of the two trees being compared.
        return GateResult::Clean;
    };
    if view.truncated {
        return GateResult::Untrustworthy(UntrustworthyReason::Indeterminate {
            detail: format!("{CONTRACT_PATH} exceeds the per-file observation cap"),
        });
    }

    // Both sides. A contract that only exists at the head is new, not
    // loosened, and a contract that only exists at the base was deleted, which
    // is the largest loosening available.
    let (Some(base_src), Some(head_src)) = (&view.base, &view.head) else {
        return if view.base.is_some() {
            GateResult::findings(vec![Finding::new(
                ctx.gate.id.clone(),
                ctx.gate.primitive,
                ctx.gate.severity,
                Subject::new(SubjectKind::Contract, CONTRACT_PATH),
                Some(CONTRACT_PATH.into()),
                None,
                Side::value("a declared quality bar"),
                Side::Absent,
                "the contract was deleted. Every gate it declared is gone.",
                ctx.origin(),
            )])
        } else {
            // A new contract is the author writing the bar down.
            GateResult::Clean
        };
    };

    let base = parse_contract(base_src);
    let head = parse_contract(head_src);
    let (Ok(base), Ok(head)) = (base, head) else {
        // One side does not parse. Naming which, because "the contract is
        // broken" and "the base commit's contract is broken" call for different
        // fixes.
        let detail = match (parse_contract(base_src), parse_contract(head_src)) {
            (Err(e), Ok(_)) => format!("the contract at the base commit is invalid: {e}"),
            (Ok(_), Err(e)) => format!("the contract is invalid: {e}"),
            (Err(b), Err(h)) => format!("both sides are invalid: base: {b}; head: {h}"),
            (Ok(_), Ok(_)) => unreachable!("the match above already found a failure"),
        };
        return GateResult::Untrustworthy(UntrustworthyReason::Indeterminate { detail });
    };

    let justified: BTreeMap<&str, &str> = head
        .changes
        .iter()
        .map(|c| (c.gate.as_str(), c.reason.as_str()))
        .collect();

    let base_gates: BTreeMap<&str, &Gate> = base.gates.iter().map(|g| (g.id.as_str(), g)).collect();
    let head_gates: BTreeMap<&str, &Gate> = head.gates.iter().map(|g| (g.id.as_str(), g)).collect();

    let mut findings = Vec::new();

    // A gate that disappeared.
    for (id, gate) in &base_gates {
        if head_gates.contains_key(id) {
            continue;
        }
        findings.push(loosened(
            ctx,
            id,
            format!("{} at {}", gate.primitive, gate.severity),
            "still declared",
            &justified,
            &format!("gate `{id}` was deleted from the contract"),
        ));
    }

    // A gate that changed.
    for (id, head_gate) in &head_gates {
        let Some(base_gate) = base_gates.get(id) else {
            continue;
        };
        findings.extend(compare(ctx, id, base_gate, head_gate, &justified));
    }

    // A gate that got stricter without evidence. The promotion guard.
    for (id, head_gate) in &head_gates {
        let Some(base_gate) = base_gates.get(id) else {
            continue;
        };
        if strictness(head_gate.severity) <= strictness(base_gate.severity) {
            continue;
        }
        if head_gate.severity == Severity::Error
            && head_gate.calibration.is_none()
            && !justified.contains_key(id)
        {
            findings.push(Finding::new(
                ctx.gate.id.clone(),
                ctx.gate.primitive,
                // This gate's own severity, which for the built-in is `error`.
                // An earlier version pinned this to `warn` on the reasoning
                // that the gate is usually advisory — but the gate is no longer
                // declarable, so it never is, and a promotion to `error`
                // without a measurement is precisely what PRD 5 forbids. It
                // must be able to block.
                ctx.gate.severity,
                Subject::new(SubjectKind::Contract, format!("gate `{id}`")),
                Some(CONTRACT_PATH.into()),
                None,
                Side::Absent,
                Side::Absent,
                format!(
                    "gate `{id}` was promoted from {} to `error` with no \
                     `calibration` naming the measurement behind it. PRD 5 \
                     requires a recorded calibration before a gate blocks.",
                    base_gate.severity
                ),
                ctx.origin(),
            ));
        }
    }

    // A gate that gained exemptions.
    for (id, head_gate) in &head_gates {
        let Some(base_gate) = base_gates.get(id) else {
            continue;
        };
        let added: Vec<&String> = head_gate
            .allow
            .iter()
            .filter(|a| !base_gate.allow.contains(a))
            .collect();
        if added.is_empty() {
            continue;
        }
        findings.push(loosened(
            ctx,
            id,
            format!("allow = [{}]", base_gate.allow.join(", ")),
            format!("allow = [{}]", head_gate.allow.join(", ")),
            &justified,
            &format!(
                "gate `{id}` gained {} exemption(s): {}",
                added.len(),
                added
                    .iter()
                    .map(|a| a.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }

    // A gap entry that disappeared. Removing one from `not_covered` is the
    // contract quietly claiming coverage it never had.
    for gone in base
        .judgement
        .not_covered
        .iter()
        .filter(|g| !head.judgement.not_covered.contains(g))
    {
        findings.push(Finding::new(
            ctx.gate.id.clone(),
            ctx.gate.primitive,
            ctx.gate.severity,
            Subject::new(SubjectKind::Contract, format!("not_covered: {gone}")),
            Some(CONTRACT_PATH.into()),
            None,
            Side::value(gone),
            Side::Absent,
            format!(
                "`not_covered` no longer admits to `{gone}`. Removing a gap \
                 entry makes the contract claim more than it delivers, which is \
                 the failure PRD 5 calls manufacturing confidence."
            ),
            ctx.origin(),
        ));
    }

    // A new suppression. Silent by construction, which is the problem.
    for added in head
        .suppressions
        .iter()
        .filter(|s| !base.suppressions.contains(s))
    {
        findings.push(loosened(
            ctx,
            added.gate_id.as_str(),
            "no suppression",
            added.reason.clone(),
            &justified,
            &format!(
                "a suppression was added for `{}` on `{}`: {}",
                added.gate_id, added.path, added.reason
            ),
        ));
    }

    GateResult::findings(findings)
}

/// Findings for one gate whose declaration changed.
fn compare(
    ctx: &GateContext<'_>,
    id: &str,
    base: &Gate,
    head: &Gate,
    justified: &BTreeMap<&str, &str>,
) -> Vec<Finding> {
    let mut out = Vec::new();

    // A loosening of severity.
    if strictness(head.severity) < strictness(base.severity) {
        out.push(loosened(
            ctx,
            id,
            base.severity.to_string(),
            head.severity.to_string(),
            justified,
            &format!(
                "gate `{id}` was downgraded from {} to {}",
                base.severity, head.severity
            ),
        ));
    }

    // A primitive swapped out from under an id.
    if base.primitive != head.primitive {
        out.push(loosened(
            ctx,
            id,
            base.primitive.to_string(),
            head.primitive.to_string(),
            justified,
            &format!(
                "gate `{id}` changed what it checks: {} -> {}",
                base.primitive, head.primitive
            ),
        ));
    }

    // Frozen paths narrowed.
    let narrowed: Vec<&String> = base
        .paths
        .iter()
        .filter(|p| !head.paths.contains(p))
        .collect();
    if !narrowed.is_empty() && !justified.contains_key(id) {
        out.push(Finding::new(
            ctx.gate.id.clone(),
            ctx.gate.primitive,
            ctx.gate.severity,
            Subject::new(SubjectKind::Contract, format!("gate `{id}`")),
            Some(CONTRACT_PATH.into()),
            None,
            Side::listed(&base.paths.iter().map(|p| (*p).clone()).collect::<Vec<_>>()),
            Side::listed(&head.paths.iter().map(|p| (*p).clone()).collect::<Vec<_>>()),
            format!(
                "gate `{id}` stopped covering {} frozen path(s): {}",
                narrowed.len(),
                narrowed
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ctx.origin(),
        ));
    }

    out
}

/// Build a loosening finding, silenced by a recorded reason.
fn loosened(
    ctx: &GateContext<'_>,
    gate_id: &str,
    expected: impl Into<String>,
    observed: impl Into<String>,
    justified: &BTreeMap<&str, &str>,
    message: &str,
) -> Finding {
    let mut finding = Finding::new(
        ctx.gate.id.clone(),
        ctx.gate.primitive,
        ctx.gate.severity,
        Subject::new(SubjectKind::Contract, format!("gate `{gate_id}`")),
        Some(CONTRACT_PATH.into()),
        None,
        Side::value(expected),
        Side::value(observed),
        message,
        ctx.origin(),
    );
    if let Some(reason) = justified.get(gate_id) {
        // A recorded reason downgrades rather than silences: the change still
        // happened, and a reader should see that it was deliberate. Silencing
        // it entirely would make a justified loosening indistinguishable from
        // a change nobody noticed.
        finding.severity = Severity::Warn;
        finding.message = format!("{message} — recorded as: {reason}");
    }
    finding
}

/// The gate's own declaration, used when the report needs to name it.
pub fn primitive() -> Primitive {
    Primitive::ContractNotLoosened
}

/// Look up a gate by id, for a report that wants to name it.
pub fn gate_of<'a>(contract: &'a Contract, id: &str) -> Option<&'a GateId> {
    contract
        .gates
        .iter()
        .find(|g| g.id.as_str() == id)
        .map(|g| &g.id)
}
