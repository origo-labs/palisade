//! `palisade-report` — the only place SARIF is produced. M0 stub.
//!
//! M3 lands `human`, `json` and `sarif` (2.1.0, so a Palisade finding lands in
//! GitHub code scanning with no extra work). One serialiser, no ad-hoc JSON in
//! gates (PLAN.md 2, boundary 6).
//!
//! The report schema carries the three targets of `EVIDENCE.md` 5 as three
//! separate top-level keys — acceptance, rule violations, residual judgement —
//! because `ok=False` conflating "does this break a rule" with "should the
//! supervisor intervene" is what confounded every model comparison in the
//! predecessor programme. Separating them in the type makes combining them a
//! reporting bug rather than a measurement bug.

/// Top-level keys of the JSON report. One constant, so the schema cannot
/// drift between the serialiser and the documentation.
pub const JSON_TARGET_KEYS: [&str; 3] = ["acceptance", "rule_violations", "residual_judgement"];
