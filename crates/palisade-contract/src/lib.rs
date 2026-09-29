//! `palisade-contract` — the parsed, validated `palisade.toml`.
//!
//! Pulled forward from M3: no configurable gate can exist without it, so
//! every M1 gate would otherwise be unrunnable. `palisade-orchestrate` stays
//! free of TOML regardless (PLAN.md 2, boundary 4) — the parser lives here and
//! the orchestrator consumes a validated [`Contract`].

pub mod generate;
pub mod parse;

use std::fmt;

/// PRD 5. Non-negotiable, taken from `slop-gate`:
///
/// > Severities are `off | warn | error | escalate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Declared but not run. Participates in no conjunction and is *not* a
    /// pass: a reader of the report must be able to see what was not checked.
    Off,
    /// Recorded, does not block. The default for a new gate.
    Warn,
    /// Blocks. Promotion from `warn` requires a recorded calibration.
    Error,
    /// Routes to a human. Does not block.
    Escalate,
}

impl Severity {
    /// The exact spelling used in `palisade.toml` and in the report.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Escalate => "escalate",
        }
    }

    /// New gates default to `warn` (PRD 5, contract language rules).
    pub const DEFAULT: Self = Self::Warn;
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A gate's identifier. Deliberately a newtype over `String` rather than a
/// bare string: `gate_id` appears in every `Finding` and in every suppression
/// record, and a typo'd id is a gate that can never be matched or suppressed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GateId(String);

/// Why a string is not a usable [`GateId`]. All three are contract validation
/// errors: a gate id that cannot round-trip into a SARIF `ruleId` or a
/// suppression entry is a gate that cannot be named, matched, or waived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateIdError {
    /// The id was the empty string.
    Empty,
    /// The id exceeded [`GateId::MAX_LEN`].
    TooLong(
        /// The offending length, in bytes.
        usize,
    ),
    /// The id contained a character outside `[a-z0-9_-]`, or began with a
    /// digit.
    IllegalChar {
        /// The first offending character.
        found: char,
    },
}

impl GateId {
    /// The contract is TOML, and a gate id travels into SARIF `ruleId` and
    /// into `palisade.toml` suppression entries. Keep it to an identifier
    /// shape so it round-trips without quoting surprises.
    pub const MAX_LEN: usize = 64;

    /// Build a gate id, rejecting anything that would not round-trip.
    ///
    /// # Errors
    ///
    /// See [`GateIdError`]. All three cases are contract validation errors,
    /// never warnings: a gate whose id cannot be named is a gate that cannot
    /// be matched by a suppression or reported in a SARIF `ruleId`.
    pub fn new(s: impl Into<String>) -> Result<Self, GateIdError> {
        let s = s.into();
        if s.is_empty() {
            return Err(GateIdError::Empty);
        }
        if s.len() > Self::MAX_LEN {
            return Err(GateIdError::TooLong(s.len()));
        }
        if let Some(found) = s
            .chars()
            .find(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && *c != '_' && *c != '-')
        {
            return Err(GateIdError::IllegalChar { found });
        }
        if s.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(GateIdError::IllegalChar {
                found: s.chars().next().expect("len checked non-empty"),
            });
        }
        Ok(Self(s))
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The gate primitives of PRD 6.
///
/// The split matters and is enforced by the crate boundary (PLAN.md 2):
/// `Analyzed` primitives are pure functions of an `Observation`; `Delegated`
/// primitives are constructed only by `palisade-exec` and carry a tool's
/// verdict plus its evidence. `Judged` is v1-unimplemented by decision
/// (PLAN.md 1.1) and its `NotImplemented` outcome forces `error`, not `pass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Primitive {
    // ---- Analyzed: no I/O, pure over an Observation --------------------
    /// Did the declared production dependency surface change? PRD 6.
    DependencySurfaceUnchanged,
    /// Were tests, benches or fuzz targets removed or skipped? PRD 6.
    TestsNotDeleted,
    /// Were frozen paths touched? PRD 6.
    PathsUnchanged,
    /// Did a public item's signature change against the base tree? PRD 6.
    PublicApiUnchanged,
    /// Was unsafe surface added? PRD 6.
    UnsafeSurfaceUnchanged,
    /// Were diagnostic suppressions broadened? PRD 6.
    SuppressionsNotWidened,
    /// Were credentials introduced? PRD 6.
    SecretAbsent,

    // ---- Delegated: constructed only by palisade-exec -----------------
    /// Do the project's own checks pass? `cargo fmt`/`clippy`/`test`.
    ChecksGreen,
    /// Did a third-party gate pass? argv, exit code, SARIF findings.
    ExternalTool,

    // ---- Judged: escalate-only, unimplemented in v1 --------------------
    /// Residual judgement. Opt-in, escalate-only, `NotImplemented` in v1.
    Judged,

    // ---- Built-in, non-configurable (PLAN.md 1.3) ----------------------
    /// Did this diff loosen the contract without a recorded reason?
    ContractNotLoosened,
    /// Is `[judgement].reviewed` older than the review interval?
    ContractReviewStale,
}

impl Primitive {
    /// The exact spelling used as the contract's `check = "..."` value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DependencySurfaceUnchanged => "dependency_surface_unchanged",
            Self::TestsNotDeleted => "tests_not_deleted",
            Self::PathsUnchanged => "paths_unchanged",
            Self::PublicApiUnchanged => "public_api_unchanged",
            Self::UnsafeSurfaceUnchanged => "unsafe_surface_unchanged",
            Self::SuppressionsNotWidened => "suppressions_not_widened",
            Self::SecretAbsent => "secret_absent",
            Self::ChecksGreen => "checks_green",
            Self::ExternalTool => "external_tool",
            Self::Judged => "judged",
            Self::ContractNotLoosened => "contract_not_loosened",
            Self::ContractReviewStale => "contract_review_stale",
        }
    }

    /// Which gate kind this primitive belongs to (PLAN.md 3.0).
    /// Which gate kind this primitive belongs to, and therefore which crate
    /// is allowed to implement it.
    pub const fn kind(self) -> PrimitiveKind {
        match self {
            Self::ChecksGreen | Self::ExternalTool => PrimitiveKind::Delegated,
            Self::Judged => PrimitiveKind::Judged,
            _ => PrimitiveKind::Analyzed,
        }
    }
}

impl fmt::Display for Primitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The epistemic distinction, as a type. "We diffed it" versus "cargo said
/// so". This is the distinction that must survive into the artefact, so it is
/// not a field someone has to remember to fill in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrimitiveKind {
    /// A pure function of an observation. `palisade-gates`, no I/O.
    Analyzed,
    /// A trusted external process's verdict. `palisade-exec`, the only crate
    /// permitted to spawn anything.
    Delegated,
    /// Escalate-only and unimplemented in v1.
    Judged,
}

/// A suppression record (PLAN.md M4, and `slop-gate` decision 3): gate,
/// repository-relative path, and a reason. All three are required. A gate
/// suite erodes one innocent-looking exception at a time otherwise, and
/// nobody can reconstruct the intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suppression {
    /// The gate this waives.
    pub gate_id: GateId,
    /// Repository-relative path the waiver applies to.
    pub path: String,
    /// Why the waiver is justified. Required, and emitted in the report.
    pub reason: String,
}

/// The declared, validated contract, as consumed by `palisade-orchestrate`.
/// M0 is the vocabulary; field population and TOML parsing arrive in M3.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contract {
    /// Contract schema version. `1` for v1.
    pub version: u32,
    /// The declared gates, in declaration order.
    pub gates: Vec<Gate>,
    /// Bound on the observation, in bytes. Below `palisade_observe::Budget::MIN`
    /// is a validation error.
    pub budget_observation_bytes: usize,
    /// The commit-ish every two-tree gate compares against.
    pub baseline_ref: Option<String>,
    /// PRD 5. Mandatory. May be empty. Must carry a review date.
    pub judgement: JudgementSection,
    /// Rule waivers, each with a gate, a path, and a reason.
    pub suppressions: Vec<Suppression>,
    /// Loosening records for this commit. Empty in most diffs.
    pub changes: Vec<GateChange>,
}

/// One declared gate: an id, a primitive, and what a finding from it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// Unique within the contract.
    pub id: GateId,
    /// Which check to run.
    pub primitive: Primitive,
    /// What a finding from it does to the verdict.
    pub severity: Severity,
    /// Required by `contract_not_loosened` when this gate changed in the
    /// diff (PLAN.md 4.3).
    /// Mandatory when this gate changed in the diff; feeds
    /// `contract_not_loosened`.
    pub reason: Option<String>,
    /// Gates this one supplies delegated output to.
    pub provides: Vec<String>,
    /// Gates this one reads delegated output from.
    pub consumes: Vec<String>,
    /// Frozen paths. Only meaningful for [`Primitive::PathsUnchanged`], and
    /// rejected on any other primitive rather than read as a setting that
    /// silently does nothing.
    pub paths: Vec<String>,
    /// Documented, reviewed additions. Not blanket permission: every entry is
    /// a specific, reviewable exception.
    pub allow: Vec<String>,
    /// Ceiling on a `Delegated` gate's runtime, in seconds. A ceiling on a
    /// runaway, not a target: `cargo test` on a large repository is minutes.
    pub timeout_seconds: Option<u64>,
    /// The program an `external_tool` gate runs.
    pub tool: Option<String>,
    /// Its argv, with `{base}`, `{head}` and `{index}` substituted.
    pub args: Vec<String>,
    /// What the tool writes on stdout.
    pub format: Option<String>,
    /// The published measurement that justifies this gate's severity.
    ///
    /// Required to *promote* a gate to `error` in a diff; a gate written as
    /// `error` in a fresh contract is the author making a claim, not a
    /// promotion. See `contract_not_loosened`.
    pub calibration: Option<String>,
}

/// A recorded, justified change to a gate, in the same commit as the change.
///
/// PRD 5: "A gate may not be loosened without a reason in the same commit."
/// This is where the reason lives, and it is a separate record rather than a
/// field on the gate because a *removed* gate has no gate left to hold one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateChange {
    /// The gate that was loosened.
    pub gate: String,
    /// Why. Required and non-empty: a blank reason is not a reason, and a
    /// reason nobody wrote is indistinguishable from a gate nobody reviewed.
    pub reason: String,
}

impl Gate {
    /// A new gate defaults to `warn`; promotion to `error` requires a recorded
    /// calibration (PLAN.md 4, PRD 5).
    pub fn new(id: GateId, primitive: Primitive) -> Self {
        Self {
            id,
            primitive,
            severity: Severity::DEFAULT,
            reason: None,
            provides: Vec::new(),
            consumes: Vec::new(),
            paths: Vec::new(),
            allow: Vec::new(),
            tool: None,
            args: Vec::new(),
            format: None,
            timeout_seconds: None,
            calibration: None,
        }
    }
}

/// Every primitive, in a fixed order so error messages, documentation and the
/// gate registry all agree. The registry matches on it exhaustively, so
/// adding a variant here is a compile error until somebody decides what it
/// does — which is the intended friction.
pub const ALL_PRIMITIVES: [Primitive; 12] = [
    Primitive::ChecksGreen,
    Primitive::DependencySurfaceUnchanged,
    Primitive::ExternalTool,
    Primitive::Judged,
    Primitive::PathsUnchanged,
    Primitive::PublicApiUnchanged,
    Primitive::SecretAbsent,
    Primitive::SuppressionsNotWidened,
    Primitive::TestsNotDeleted,
    Primitive::UnsafeSurfaceUnchanged,
    Primitive::ContractNotLoosened,
    Primitive::ContractReviewStale,
];

/// Default observation budget when `[budget]` is absent. Matches
/// `palisade_observe::Budget::DEFAULT`, duplicated as a literal so that
/// `palisade-contract` stays free of a dependency on the observer.
pub const DEFAULT_OBSERVATION_BYTES: usize = 128 * 1024;

/// PRD 5. Mandatory, may be empty, must carry a review date. The section that
/// decides whether a contract is a tool you can rely on or one that
/// manufactures confidence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JudgementSection {
    /// Mandatory. An empty list is legal but the tool warns (PRD 9.4).
    pub not_covered: Vec<String>,
    /// Mandatory, `YYYY-MM-DD`. A stale review is itself a finding.
    pub reviewed: Option<String>,
}

impl JudgementSection {
    /// PLAN.md 1.3. Neither the interval nor the enforcement is configurable:
    /// a mechanism whose trigger the project can set to `never` is a comment.
    pub const DEFAULT_REVIEW_INTERVAL_DAYS: i64 = 180;

    /// Days since the recorded review date, or `None` if undated or unparseable.
    /// No clock dependency: the caller passes "now" so tests are deterministic.
    pub fn staleness_days(&self, now_unix: i64) -> Option<i64> {
        let reviewed = self.reviewed.as_deref()?;
        // `YYYY-MM-DD`, the only date format that does not need a parser crate
        // in M0. Malformed input is `None`, and `contract_review_stale` treats
        // `None` as a finding: an unparseable date is not a fresh one.
        let mut parts = reviewed.split('-');
        let y = parts.next()?.parse::<i64>().ok()?;
        let m = parts.next()?.parse::<i64>().ok()?;
        let d = parts.next()?.parse::<i64>().ok()?;
        if parts.next().is_some() {
            return None;
        }
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        // Days from civil, Howard Hinnant's algorithm. Proleptic Gregorian,
        // no chrono dependency, exact for the range we care about.
        let days = days_from_civil(y, m, d);
        Some(now_unix.div_euclid(86_400) - days)
    }
}

/// Howard Hinnant's `days_from_civil`, for `m` in 1..=12.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_id_shape_is_enforced() {
        assert!(GateId::new("no_unsafe_added").is_ok());
        assert!(GateId::new("public-api-unchanged").is_ok());
        assert_eq!(GateId::new(""), Err(GateIdError::Empty));
        assert_eq!(
            GateId::new("2fast"),
            Err(GateIdError::IllegalChar { found: '2' })
        );
        assert_eq!(
            GateId::new("has space"),
            Err(GateIdError::IllegalChar { found: ' ' })
        );
        assert!(matches!(
            GateId::new("x".repeat(65)),
            Err(GateIdError::TooLong(65))
        ));
    }

    #[test]
    fn new_gates_default_to_warn() {
        let g = Gate::new(GateId::new("t").unwrap(), Primitive::SecretAbsent);
        assert_eq!(g.severity, Severity::Warn);
    }

    #[test]
    fn primitive_kinds_match_the_crate_boundary() {
        assert_eq!(Primitive::SecretAbsent.kind(), PrimitiveKind::Analyzed);
        assert_eq!(Primitive::ChecksGreen.kind(), PrimitiveKind::Delegated);
        assert_eq!(Primitive::ExternalTool.kind(), PrimitiveKind::Delegated);
        assert_eq!(Primitive::Judged.kind(), PrimitiveKind::Judged);
    }

    #[test]
    fn staleness_counts_days() {
        let j = JudgementSection {
            not_covered: vec![],
            reviewed: Some("2026-09-28".to_string()),
        };
        // 2026-09-28 is day 20706 of the civil epoch; +10 days is 2026-10-08.
        let then = (days_from_civil(2026, 9, 28) + 10) * 86_400;
        assert_eq!(j.staleness_days(then), Some(10));
    }

    #[test]
    fn undated_or_malformed_review_is_not_fresh() {
        let undated = JudgementSection::default();
        assert_eq!(undated.staleness_days(0), None);

        for bad in ["", "2026", "2026-13", "2026-09-99", "not-a-date"] {
            let j = JudgementSection {
                not_covered: vec![],
                reviewed: Some(bad.to_string()),
            };
            assert_eq!(j.staleness_days(0), None, "{bad} should not parse");
        }
    }

    #[test]
    fn epoch_is_the_unix_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }
}
