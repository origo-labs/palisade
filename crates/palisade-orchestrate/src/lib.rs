//! `palisade-orchestrate` — the verdict algebra.
//!
//! Small enough to prove correct by exhaustion, which is how it is tested.
//! Knows nothing about TOML, tree-sitter, or processes (PLAN.md 2, boundary
//! 5); it consumes a validated contract and a set of gate outcomes.
//!
//! Four rules, each a bug that already happened once (PLAN.md 0):
//!
//! 1. A gate may only ever subtract. There is no input to `reduce` that
//!    yields `Accept` other than a conjunction of passes, declared-off gates,
//!    and non-blocking warnings.
//! 2. `error` is a first-class outcome, never a `block` and never a `pass`.
//! 3. Unknown is an error.
//! 4. Acceptance never comes from a judgement.

use std::fmt;

use palisade_contract::{Contract, GateId, Primitive, Severity};

/// Who produced a verdict.
///
/// `Delegated` is constructible only by `palisade-exec`, and `Judged` is not
/// in this enum at all: a judgement can only ever be a `Fail` at escalate
/// severity, so "a model said this is fine" is not expressible as a pass. That
/// is the type system, not a convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Palisade's own AST or diff analysis.
    Analyzed {
        /// Which primitive produced it.
        primitive: Primitive,
    },
    /// A trusted external process. Constructible only by `palisade-exec`.
    Delegated {
        /// The tool's identity, e.g. `cargo` or `slop-gate`.
        tool: ToolId,
        /// The tool version, so a finding is reproducible against a known
        /// implementation.
        version: String,
    },
}

/// Identifies the tool behind a `Delegated` verdict.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ToolId(String);

impl ToolId {
    /// Wrap a tool name.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    /// The tool name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A warning that is not a verdict. A gate whose own definition changed in
/// this diff is exactly that: worth saying out loud, never a reason to block,
/// and never a reason to accept.
/// A warning that is not a verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advisory {
    /// The gate the advisory concerns.
    pub gate_id: GateId,
    /// Human-readable explanation. Carries no fact absent from the fields
    /// it summarises.
    pub message: String,
}

/// Why no trustworthy verdict could be produced. Every variant forces
/// [`Verdict::Error`]; none of them is a failure of the work under review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UntrustworthyReason {
    /// The tool could not produce a verdict at all: missing, timed out,
    /// killed, or exited with a code it does not document.
    ToolCouldNotRun {
        /// What happened, in one sentence.
        detail: String,
    },
    /// A gate could not decide, e.g. a file it needed to parse did not parse.
    Indeterminate {
        /// What prevented a decision.
        detail: String,
    },
    /// A declared primitive has no implementation in this build. `judged` is
    /// one in v1 (PLAN.md 1.1): a project that declares it gets a loud
    /// failure, not a silent heuristic.
    Unimplemented {
        /// The primitive with no implementation.
        primitive: Primitive,
    },
    /// The contract itself did not validate, so there is no declared gate set
    /// that could have been satisfied.
    ContractInvalid {
        /// The validation error.
        detail: String,
    },
}

impl UntrustworthyReason {
    /// The human-readable detail, whichever variant carries it.
    pub fn detail(&self) -> &str {
        match self {
            Self::ToolCouldNotRun { detail }
            | Self::Indeterminate { detail }
            | Self::ContractInvalid { detail } => detail,
            Self::Unimplemented { primitive } => primitive.as_str(),
        }
    }
}

/// Why a gate did not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// `severity = "off"`. Reported as `off`, so a reader sees what was not
    /// checked. Participates in no conjunction and is *not* a pass.
    DeclaredOff,
    /// The contract or the observation says the gate does not apply, and the
    /// report says so. Still not a pass.
    NotApplicable {
        /// Why it does not apply.
        detail: String,
    },
}

/// A gate's result. There is no bare boolean anywhere in this crate, by
/// construction: a gate that cannot produce a trustworthy verdict has to say
/// which of the non-pass variants it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// The gate ran and found nothing it reports. Compatible with accept.
    Pass {
        /// Who produced it.
        origin: Origin,
    },
    /// The gate ran and found something. Whether that blocks is the
    /// finding's severity, not this variant.
    Fail(
        /// The evidence, always structured (PRD 7).
        Finding,
    ),
    /// The gate could not produce a trustworthy verdict. Forces
    /// [`Verdict::Error`] and is never a pass.
    Untrustworthy {
        /// Why no verdict was possible.
        reason: UntrustworthyReason,
        /// Who was supposed to produce it.
        origin: Origin,
    },
    /// The gate did not run. Reported as `off`, never as a pass.
    Skipped {
        /// Why it did not run.
        reason: SkipReason,
    },
}

impl GateOutcome {
    /// Whether the gate declined to run. A skip steps aside and is reported;
    /// it neither blocks nor accepts.
    pub fn is_skip(&self) -> bool {
        matches!(self, Self::Skipped { .. })
    }

    /// Whether this outcome carries news: something was found, or something
    /// could not be determined. Every such outcome is reported.
    ///
    /// A `Pass` is not news and a `Skipped` is not news, though the `Skipped`
    /// is still reported — as `off`, never as a pass.
    pub const fn is_adverse(&self) -> bool {
        matches!(self, Self::Fail(_) | Self::Untrustworthy { .. })
    }

    /// Whether this outcome is incompatible with [`Verdict::Accept`].
    ///
    /// Strictly stronger than [`GateOutcome::is_adverse`]: a `warn` finding is
    /// adverse but fully compatible with acceptance, which is what `warn`
    /// means and why PRD 5 makes it the default severity. Only an
    /// `error`-severity failure or anything untrustworthy rules out accept.
    pub const fn rules_out_accept(&self) -> bool {
        match self {
            Self::Untrustworthy { .. } => true,
            Self::Fail(f) => f.is_blocking(),
            Self::Pass { .. } | Self::Skipped { .. } => false,
        }
    }
}

/// A content-addressed identifier for a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint(
    /// Raw blake3 output; see [`Fingerprint::of`] for the derivation.
    pub [u8; 32],
);

impl Fingerprint {
    /// Stable across runs, so CI can tell "the same known finding" from "a new
    /// one" and so a suppression can be scoped to a finding rather than a
    /// file. Derived from the *normalised* content, never from a diff hunk's
    /// line numbers — those move under you on any unrelated edit.
    pub fn of(parts: &[&str]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"palisade.finding.v1\0");
        for p in parts {
            h.update(p.as_bytes());
            h.update(b"\0");
        }
        let bytes = *h.finalize().as_bytes();
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Self(out)
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0[..8] {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// PRD 7: "A block without the diff hunk, the gate id, the expected and
/// observed value, and a stable fingerprint is a bug." So this is a struct,
/// not a formatted string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The gate that produced it.
    pub gate_id: GateId,
    /// The check that ran.
    pub primitive: Primitive,
    /// What it does to the verdict.
    pub severity: Severity,
    /// What is being compared, in a form a consumer can branch on. Without it
    /// the two sides are two sentences and a reader has to guess what kind of
    /// thing is being talked about.
    pub subject: Subject,
    /// Repository-relative location, when the finding has one.
    pub path: Option<camino::Utf8PathBuf>,
    /// Line range in the observed file, when the finding is located.
    pub hunk: Option<HunkRef>,
    /// The contract's or the baseline's side of the comparison.
    pub expected: Side,
    /// The worktree's side of the comparison.
    pub observed: Side,
    /// A human sentence naming the subject and the change. Carries nuance a
    /// generic renderer cannot — why an addition is not a break, how to
    /// silence one — but is a *hint*, not the source of truth: everything a
    /// consumer needs is in `subject`, `expected` and `observed`. A test
    /// asserts every message names its subject, so the prose cannot drift away
    /// from the fields it describes.
    pub message: String,
    /// Stable across runs and across unrelated line movement.
    pub fingerprint: Fingerprint,
    /// Who produced it, derived from the producing outcome.
    pub origin: Origin,
}

/// What a finding is about.
///
/// Present so a report or a SARIF consumer can branch on the kind without
/// parsing prose, and so both sides of a comparison are unambiguously about
/// the same thing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Subject {
    /// What sort of thing this is.
    pub kind: SubjectKind,
    /// Which one.
    pub name: String,
}

impl Subject {
    /// Name a subject.
    pub fn new(kind: SubjectKind, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
        }
    }
}

/// What sort of thing a finding is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubjectKind {
    /// A declared production dependency.
    Dependency,
    /// A `[features]` entry.
    Feature,
    /// A test function, by module path and name.
    Test,
    /// A public item, by module path and name.
    PublicItem,
    /// A kind of `unsafe` surface, by count.
    UnsafeSurface,
    /// A diagnostic suppression.
    Suppression,
    /// A whole file, for a deletion where the unit is the file.
    File,
    /// A path, for a frozen-path policy check.
    Path,
    /// A delegated check, e.g. `cargo fmt`. Distinct from `Contract`: a check
    /// is something Palisade ran, not something the project declared.
    Check,
    /// The contract itself.
    Contract,
}

impl SubjectKind {
    /// The word a message uses for this kind of subject.
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Dependency => "dependency",
            Self::Feature => "feature",
            Self::Test => "test",
            Self::PublicItem => "public item",
            Self::UnsafeSurface => "unsafe surface",
            Self::Suppression => "suppression",
            Self::File => "file",
            Self::Path => "path",
            Self::Check => "check",
            Self::Contract => "contract",
        }
    }
}

/// One side of a comparison.
///
/// `Absent` is a value, not the string `"absent"`. Three gates had invented
/// three different spellings of it, and a consumer had no way to tell "not
/// present" from "present and equal to the word absent".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// The thing was not present on this side of the comparison.
    Absent,
    /// The thing was present, with this rendering.
    Value(String),
}

impl Side {
    /// A present value.
    pub fn value(s: impl Into<String>) -> Self {
        Self::Value(s.into())
    }

    /// A named set, rendered the same way wherever it appears.
    ///
    /// A shared renderer is what makes the two sides comparable. One gate
    /// rendered its before-side as a list and its after-side as a count, which
    /// is why its report line read `2 test(s): a, b -> 1 test(s)`.
    pub fn listed(items: &[String]) -> Self {
        match items.len() {
            0 => Self::Absent,
            1 => Self::Value(items[0].clone()),
            n => Self::Value(format!("{n} ({})", items.join(", "))),
        }
    }

    /// A count with a unit, rendered the same way on both sides.
    pub fn counted(unit: &str, n: usize) -> Self {
        Self::Value(format!("{n} {unit}{}", if n == 1 { "" } else { "s" }))
    }

    /// The rendering, for a report.
    pub fn render(&self) -> String {
        match self {
            Self::Absent => "(absent)".to_string(),
            Self::Value(v) => v.clone(),
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

/// What kind of change a finding describes.
///
/// **Derived from the two sides, never stated in prose.** This is the property
/// that makes the pair worth having: a consumer classifies a finding by
/// comparing `expected` and `observed`, and cannot be misled by a message that
/// says "widened" on a pair that is actually an addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChangeKind {
    /// Present in the worktree, absent from the baseline or the contract.
    Added,
    /// Present in the baseline, absent from the worktree.
    Removed,
    /// Present on both sides, with different values.
    Changed,
}

impl ChangeKind {
    /// The word a message and a report use.
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
        }
    }
}

impl fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.verb())
    }
}

/// Serialised as the same word the human report and SARIF use, rather than the
/// variant name. A consumer that reads `"Added"` here and `"added"` in the
/// prose has to normalise before it can join the two, and it will not.
impl serde::Serialize for ChangeKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.verb())
    }
}

/// A line range in the observed file, 1-based and inclusive.
///
/// Excluded from [`Fingerprint::of`]: line numbers move under any unrelated
/// edit, so a fingerprint that included them would report a known finding as
/// new on every nearby change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HunkRef {
    /// First line, 1-based inclusive.
    pub start: u32,
    /// Last line, 1-based inclusive.
    pub end: u32,
}

impl Finding {
    /// Build a finding, deriving the fingerprint from its content so it is
    /// stable and cannot drift from what it describes.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        gate_id: GateId,
        primitive: Primitive,
        severity: Severity,
        subject: Subject,
        path: Option<camino::Utf8PathBuf>,
        hunk: Option<HunkRef>,
        expected: Side,
        observed: Side,
        message: impl Into<String>,
        origin: Origin,
    ) -> Self {
        let path_s = path.as_deref().map_or("", camino::Utf8Path::as_str);
        let fingerprint = Fingerprint::of(&[
            gate_id.as_str(),
            primitive.as_str(),
            path_s,
            subject.kind.noun(),
            &subject.name,
            &expected.render(),
            &observed.render(),
        ]);
        Self {
            gate_id,
            primitive,
            severity,
            subject,
            path,
            hunk,
            expected,
            observed,
            message: message.into(),
            fingerprint,
            origin,
        }
    }

    /// What kind of change this finding describes, derived from the two sides.
    ///
    /// The one property that makes `expected`/`observed` worth having over two
    /// free-text fields: a consumer classifies the finding by comparing them,
    /// so a message that says "widened" cannot make an addition look like a
    /// broadening.
    pub fn change(&self) -> ChangeKind {
        match (&self.expected, &self.observed) {
            (Side::Absent, Side::Value(_)) => ChangeKind::Added,
            (Side::Value(_), Side::Absent) => ChangeKind::Removed,
            (Side::Value(_), Side::Value(_)) => ChangeKind::Changed,
            // A subject that is on neither side is a check that fired without
            // a comparison, e.g. a policy gate. Reported as `Changed` because
            // something about it is not as declared.
            (Side::Absent, Side::Absent) => ChangeKind::Changed,
        }
    }

    /// A sentence naming the subject and the change.
    ///
    /// The fallback rendering, used where a gate has no nuance to add. A
    /// generated sentence can never disagree with the fields it is generated
    /// from, which is why it exists alongside the free-form `message` rather
    /// than instead of it.
    pub fn describe(&self) -> String {
        format!(
            "{} `{}` {}",
            self.subject.kind.noun(),
            self.subject.name,
            self.change().verb()
        )
    }

    /// Whether this finding should reach a human rather than block. Judged
    /// findings are pinned to escalate in M6 regardless of the contract: a
    /// contract must not be able to turn a model into an acceptance criterion.
    pub const fn is_escalating(&self) -> bool {
        matches!(self.severity, Severity::Escalate)
    }

    /// Whether this finding is incompatible with `accept`. `error` blocks;
    /// `escalate` routes to a human; neither coexists with acceptance.
    pub const fn is_blocking(&self) -> bool {
        matches!(self.severity, Severity::Error | Severity::Escalate)
    }
}

/// The single answer a run produces. Exactly one, never none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verdict {
    /// Every enabled gate passed. The only way to get here.
    Accept,
    /// At least one error-severity finding, with evidence for each.
    Block,
    /// At least one escalate-severity finding, and nothing blocking.
    Escalate,
    /// No trustworthy verdict could be produced. **No verdict is implied**,
    /// and this never collapses into `Block` (PRD 4).
    Error,
}

impl Verdict {
    /// Distinct exit codes, and `Error` is 2 rather than 1. That ordering is
    /// the lesson from the predecessor project and is the thing most likely to
    /// be "simplified" away. It gets its own crate (palisade-exec), a comment
    /// naming the incident, and a test that fails if the two are unified.
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Accept => 0,
            Self::Block => 1,
            Self::Error => 2,
            Self::Escalate => 3,
        }
    }

    /// The exact spelling used in reports and on the CLI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Block => "block",
            Self::Escalate => "escalate",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything the reduction is allowed to see. Note what is absent: no
/// `GateOutcome` can carry model provenance, and no field here can turn a
/// block into an accept.
/// Everything the reduction is allowed to see.
#[derive(Debug, Clone, Default)]
pub struct ReductionInput<'a> {
    /// One entry per gate the run considered.
    pub outcomes: &'a [GateOutcome],
    /// Warnings that are not verdicts.
    pub advisories: &'a [Advisory],
    /// Set when the contract itself failed to validate. Checked first and
    /// unconditionally, because a contract that did not parse cannot be said
    /// to have been satisfied.
    pub contract_error: Option<String>,
}

/// The verdict plus the evidence that produced it. `reduce` is total: it
/// always returns one of the four, never nothing.
pub fn reduce(input: &ReductionInput<'_>) -> Verdict {
    // Contract failure dominates: with no valid contract there is no declared
    // gate set to have passed.
    if input.contract_error.is_some() {
        return Verdict::Error;
    }
    // A gate that could not run has not passed. This outranks every failure,
    // because "we could not tell" is strictly more dangerous to report as
    // accept, and strictly more confusing to report as block, than "we know".
    if input
        .outcomes
        .iter()
        .any(|o| matches!(o, GateOutcome::Untrustworthy { .. }))
    {
        return Verdict::Error;
    }

    let mut escalate = false;
    for outcome in input.outcomes {
        if let GateOutcome::Fail(f) = outcome {
            match f.severity {
                Severity::Error => return Verdict::Block,
                Severity::Escalate => escalate = true,
                Severity::Warn | Severity::Off => {}
            }
        }
    }
    if escalate {
        Verdict::Escalate
    } else {
        Verdict::Accept
    }
}

/// Gates declared `off` produce a `Skipped`, not a `Pass`, and the reduction
/// does not care either way. This exists so a reader of the report can always
/// see what was not checked.
pub fn skipped_off(gate_id: &GateId) -> GateOutcome {
    let _ = gate_id;
    GateOutcome::Skipped {
        reason: SkipReason::DeclaredOff,
    }
}

/// The gates a contract declares, with the `off` ones already turned into
/// skips. M0 registers no gates; this is the seam M1 fills.
pub fn planned_outcomes(contract: &Contract) -> Vec<GateOutcome> {
    contract
        .gates
        .iter()
        .filter(|g| g.severity == Severity::Off)
        .map(|g| skipped_off(&g.id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_distinct_and_error_is_two() {
        let all = [
            Verdict::Accept,
            Verdict::Block,
            Verdict::Escalate,
            Verdict::Error,
        ];
        let mut codes: Vec<i32> = all.iter().map(|v| v.exit_code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 4, "every verdict needs its own code");
        assert_eq!(Verdict::Error.exit_code(), 2);
        assert_eq!(Verdict::Block.exit_code(), 1);
    }

    #[test]
    fn an_unimplemented_primitive_is_an_error_not_a_pass() {
        // PLAN.md 1.1: a project that declares `judged` in v1 gets a loud,
        // correct failure rather than a silent regression to a heuristic.
        let o = GateOutcome::Untrustworthy {
            reason: UntrustworthyReason::Unimplemented {
                primitive: Primitive::Judged,
            },
            origin: Origin::Analyzed {
                primitive: Primitive::Judged,
            },
        };
        let v = reduce(&ReductionInput {
            outcomes: std::slice::from_ref(&o),
            ..Default::default()
        });
        assert_eq!(v, Verdict::Error);
    }

    #[test]
    fn skipped_off_is_not_a_pass() {
        assert!(skipped_off(&GateId::new("x").unwrap()).is_skip());
        assert!(!skipped_off(&GateId::new("x").unwrap()).is_adverse());
        assert!(!skipped_off(&GateId::new("x").unwrap()).rules_out_accept());
        // But a skip also never blocks.
        let v = reduce(&ReductionInput {
            outcomes: &[skipped_off(&GateId::new("x").unwrap())],
            ..Default::default()
        });
        assert_eq!(v, Verdict::Accept);
    }
}

/// One declared gate's run: every outcome it produced, and every finding.
///
/// A gate is not one outcome. `checks_green` runs three commands and can fail
/// two of them; a `Delegated` gate can be asked to run a tool that reports
/// forty findings. Collapsing those to one — which the CLI did, and which the
/// M2 follow-up had to undo — throws away the evidence PRD 7 requires. So the
/// run holds all of it, and the *reduction* is a separate, explicit step.
///
/// This lives here rather than in the CLI because `palisade-exec` needs to
/// build one too, and a shape duplicated across two crates is a shape that
/// drifts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRun {
    /// The declared gate.
    pub gate_id: GateId,
    /// The check that ran.
    pub primitive: Primitive,
    /// Every finding, uncollapsed and in the order the gate produced them.
    pub findings: Vec<Finding>,
    /// The single outcome the verdict algebra sees.
    pub outcome: GateOutcome,
}

impl GateRun {
    /// A gate that passed.
    pub fn passed(gate_id: GateId, primitive: Primitive, origin: Origin) -> Self {
        Self {
            gate_id,
            primitive,
            findings: Vec::new(),
            outcome: GateOutcome::Pass { origin },
        }
    }

    /// A gate that was declared `off` and so did not run. Reported, never a
    /// pass.
    pub fn skipped(gate_id: GateId, primitive: Primitive) -> Self {
        Self {
            gate_id,
            primitive,
            findings: Vec::new(),
            outcome: GateOutcome::Skipped {
                reason: SkipReason::DeclaredOff,
            },
        }
    }

    /// A gate that could not produce a trustworthy verdict.
    pub fn untrustworthy(
        gate_id: GateId,
        primitive: Primitive,
        reason: UntrustworthyReason,
        origin: Origin,
    ) -> Self {
        Self {
            gate_id,
            primitive,
            findings: Vec::new(),
            outcome: GateOutcome::Untrustworthy { reason, origin },
        }
    }

    /// A gate that found things. The outcome is the most severe finding; the
    /// rest stay in `findings` and are reported.
    pub fn with_findings(
        gate_id: GateId,
        primitive: Primitive,
        findings: Vec<Finding>,
        origin: Origin,
    ) -> Self {
        let outcome = match findings.iter().max_by_key(|f| severity_rank(f.severity)) {
            Some(worst) => GateOutcome::Fail(worst.clone()),
            None => GateOutcome::Pass { origin },
        };
        Self {
            gate_id,
            primitive,
            findings,
            outcome,
        }
    }

    /// Whether this run carries evidence a reader can act on.
    pub fn has_findings(&self) -> bool {
        !self.findings.is_empty()
    }
}

/// Severity ordering, for picking the finding that decides a verdict.
fn severity_rank(s: Severity) -> u8 {
    match s {
        Severity::Warn => 0,
        Severity::Escalate => 1,
        Severity::Error => 2,
        Severity::Off => 0,
    }
}

/// Accumulates a run, so a fan-out gate does not have to re-derive the
/// precedence rule at every `push`.
///
/// The precedence is the interesting part: an untrustworthy sub-result beats
/// any number of failures, because "we could not tell" is more dangerous to
/// report as a pass and more confusing to report as a block than "we know".
#[derive(Debug, Default)]
pub struct GateRunBuilder {
    findings: Vec<Finding>,
    untrustworthy: Option<UntrustworthyReason>,
}

impl GateRunBuilder {
    /// A builder with nothing in it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a finding. Does not clear an untrustworthy already recorded.
    pub fn finding(&mut self, f: Finding) {
        self.findings.push(f);
    }

    /// Record that part of the gate could not be determined. The first one
    /// wins, because it is the earliest thing that went wrong.
    pub fn untrustworthy(&mut self, reason: UntrustworthyReason) {
        if self.untrustworthy.is_none() {
            self.untrustworthy = Some(reason);
        }
    }

    /// Collapse to a run.
    pub fn finish(self, gate_id: GateId, primitive: Primitive, origin: Origin) -> GateRun {
        if let Some(reason) = self.untrustworthy {
            return GateRun {
                gate_id,
                primitive,
                findings: self.findings,
                outcome: GateOutcome::Untrustworthy { reason, origin },
            };
        }
        GateRun::with_findings(gate_id, primitive, self.findings, origin)
    }
}

impl GateOutcome {
    /// The origin behind this outcome, when it has one.
    ///
    /// A skip has none: it is the absence of a verdict, not a verdict, and
    /// giving it a producer would make "we did not check" indistinguishable
    /// from "we checked and found nothing" in the report.
    pub fn origin(&self) -> Option<&Origin> {
        match self {
            Self::Pass { origin } | Self::Untrustworthy { origin, .. } => Some(origin),
            Self::Fail(f) => Some(&f.origin),
            Self::Skipped { .. } => None,
        }
    }
}
