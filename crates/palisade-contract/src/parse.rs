//! Parsing and validation of `palisade.toml`.
//!
//! The rule that matters most here is `slop-gate`'s, quoted in full in
//! `REFERENCE-slop-gate.md`:
//!
//! > "Unknown keys are errors, so a misspelled rule cannot silently weaken a
//! > gate."
//!
//! A configuration typo that silently disables a gate is the exact failure
//! mode that turns a gate suite into theatre, and it is the failure mode this
//! project already paid for once (`PLAN.md` 0, rule 3). So every struct here
//! is `deny_unknown_fields`, every enum is parsed by hand rather than by serde
//! derive, and every error carries a span.

use std::fmt;

use serde::Deserialize;

use crate::{
    Contract, Gate, GateChange, GateId, JudgementSection, Primitive, Severity, Suppression,
};

pub use crate::ALL_PRIMITIVES;

/// The contract filename, at the repository root.
pub const CONTRACT_FILENAME: &str = "palisade.toml";

/// A contract that could not be parsed or could not be validated.
///
/// `Unknown key` is the interesting one: it names the key and its line, so
/// "my gate stopped running" is answerable from the error alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// The file did not parse as TOML.
    Syntax {
        /// 1-based line the parser pointed at.
        line: Option<usize>,
        /// The parser's message, trimmed.
        message: String,
    },
    /// A key the parser does not know. An error, never a warning.
    UnknownKey {
        /// The offending key.
        key: String,
        /// 1-based line, when the parser could attribute it.
        line: Option<usize>,
    },
    /// A value that is not valid for its key.
    InvalidValue {
        /// The key whose value is wrong.
        key: String,
        /// What would have been accepted.
        expected: String,
        /// What was written.
        found: String,
        /// 1-based line, when known.
        line: Option<usize>,
    },
    /// The contract parsed but does not describe a runnable gate suite.
    Validation(String),
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { line, message } => {
                write_line(f, line)?;
                write!(f, "could not parse: {message}")
            }
            Self::UnknownKey { key, line } => {
                write_line(f, line)?;
                write!(
                    f,
                    "unknown key `{key}`. A misspelled key must never silently \
                     weaken the contract, so this is an error rather than a warning."
                )
            }
            Self::InvalidValue {
                key,
                expected,
                found,
                line,
            } => {
                write_line(f, line)?;
                write!(f, "`{key}`: expected {expected}, found `{found}`")
            }
            Self::Validation(m) => write!(f, "invalid contract: {m}"),
        }
    }
}

/// 1-based line number of a byte offset. Clamped to the source so a span
/// pointing at end-of-input reports the last line rather than failing.
fn line_of(source: &str, offset: usize) -> usize {
    let end = offset.min(source.len());
    let end = (0..end)
        .rev()
        .find(|i| source.is_char_boundary(*i))
        .unwrap_or(0);
    source[..end].matches('\n').count() + 1
}

fn write_line(f: &mut fmt::Formatter<'_>, line: &Option<usize>) -> fmt::Result {
    match line {
        Some(l) => write!(f, "line {l}: "),
        None => Ok(()),
    }
}

impl std::error::Error for ContractError {}

/// Parse and validate a `palisade.toml`.
///
/// # Errors
///
/// Returns [`ContractError`] on a syntax error, an unknown key, an invalid
/// value, or a contract that parses but describes an unrunnable gate suite.
/// Every one of those is a hard failure: a contract that could not be fully
/// understood has not been satisfied.
///
/// # Panics
///
/// Never. This is the one function in the workspace that must not panic, since
/// a panic here is a supervisor crash on a malformed file.
pub fn parse_contract(source: &str) -> Result<Contract, ContractError> {
    let raw: RawContract = toml::from_str(source).map_err(|e| {
        // `toml` hands back a byte offset, not a line. Converting here rather
        // than reporting "at byte 412" is the difference between an error a
        // person can act on and an error they have to count lines to find.
        let offset = e.span().map_or(0, |r| r.start);
        ContractError::Syntax {
            line: Some(line_of(source, offset)),
            message: e.message().trim().to_string(),
        }
    })?;
    raw.validate()
}

// ---- The wire format -------------------------------------------------------
//
// Separate from the domain types on purpose. These structs own the parsing
// rules, including hand-written `Deserialize` for every enum so that a typo
// is an error naming the field rather than a silently accepted default. The
// domain types in `crate` stay free of serde.

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContract {
    /// Contract schema version. Only 1 exists.
    version: u32,
    #[serde(default)]
    gates: Vec<RawGate>,
    #[serde(default)]
    budget: Option<RawBudget>,
    #[serde(default)]
    baseline: Option<RawBaseline>,
    /// Mandatory. May be empty, but the section must be present.
    judgement: RawJudgement,
    #[serde(default)]
    suppressions: Vec<RawSuppression>,
    /// Loosening records for this commit.
    #[serde(default)]
    changes: Vec<RawChange>,
}

/// A recorded loosening, in the same commit as the loosening.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawChange {
    /// The gate that was loosened.
    gate: String,
    /// Why. Required, and must not be blank.
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGate {
    id: String,
    /// Which check to run. The only way to name a primitive, and an unknown
    /// spelling is an error rather than a gate that does nothing.
    check: String,
    /// Defaults to `warn` when absent (PRD 5).
    #[serde(default)]
    severity: Option<String>,
    /// Frozen paths, for `paths_unchanged`.
    #[serde(default)]
    paths: Vec<String>,
    /// Reviewed, documented additions. For `dependency_surface_unchanged` and
    /// `public_api_unchanged`.
    #[serde(default)]
    allow: Vec<String>,
    /// Required when a gate changed in the diff; feeds `contract_not_loosened`.
    #[serde(default)]
    reason: Option<String>,
    /// Gates this one supplies delegated output to.
    #[serde(default)]
    provides: Vec<String>,
    /// Gates this one reads delegated output from.
    #[serde(default)]
    consumes: Vec<String>,
    /// Ceiling on a Delegated gate's runtime, in seconds.
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// The published measurement behind this severity.
    #[serde(default)]
    calibration: Option<String>,
    /// The program an `external_tool` gate runs.
    #[serde(default)]
    tool: Option<String>,
    /// Its argv, with `{base}`, `{head}` and `{index}` placeholders.
    #[serde(default)]
    args: Vec<String>,
    /// What the tool writes on stdout: `sarif` or `lines`.
    #[serde(default)]
    format: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBudget {
    observation_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBaseline {
    #[serde(rename = "ref")]
    ref_: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJudgement {
    #[serde(default)]
    not_covered: Vec<String>,
    reviewed: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSuppression {
    gate: String,
    path: String,
    reason: String,
}

impl RawContract {
    fn validate(self) -> Result<Contract, ContractError> {
        if self.version != 1 {
            return Err(ContractError::Validation(format!(
                "version {} is not supported; this build understands version 1",
                self.version
            )));
        }

        let mut gates = Vec::with_capacity(self.gates.len());
        let mut seen_ids: Vec<GateId> = Vec::new();
        for raw in &self.gates {
            let gate = raw.to_gate()?;
            // Two gates with one id would make a finding unattributable and a
            // suppression ambiguous. A duplicate is an error, not a merge.
            if seen_ids.contains(&gate.id) {
                return Err(ContractError::Validation(format!(
                    "duplicate gate id `{}`; a finding must name exactly one gate",
                    gate.id
                )));
            }
            seen_ids.push(gate.id.clone());
            gates.push(gate);
        }

        // `provides`/`consumes` must form a DAG, and every name must resolve
        // to a declared gate. An undeclared edge is a load error rather than a
        // nondeterministic read of a slot nobody filled.
        for gate in &gates {
            for edge in gate.provides.iter().chain(gate.consumes.iter()) {
                if !seen_ids.iter().any(|id| id.as_str() == edge) {
                    return Err(ContractError::Validation(format!(
                        "gate `{}` refers to `{edge}`, which is not a declared gate",
                        gate.id
                    )));
                }
            }
        }
        if let Some(cycle) = find_cycle(&gates) {
            return Err(ContractError::Validation(format!(
                "provides/consumes contains a cycle: {}",
                cycle.join(" -> ")
            )));
        }

        let judgement = JudgementSection {
            not_covered: self.judgement.not_covered,
            // Mandatory. A missing review date is not a default; it is the
            // finding `contract_review_stale` exists to report.
            reviewed: Some(self.judgement.reviewed.ok_or_else(|| {
                ContractError::Validation(
                    "[judgement].reviewed is required. A review date is the only \
                     forcing function the design has for who owns this contract."
                        .to_string(),
                )
            })?),
        };

        let suppressions = self
            .suppressions
            .iter()
            .map(|s| {
                Ok(Suppression {
                    gate_id: GateId::new(s.gate.clone())
                        .map_err(|e| ContractError::Validation(format!("suppression: {e:?}")))?,
                    path: s.path.clone(),
                    reason: s.reason.clone(),
                })
            })
            .collect::<Result<Vec<_>, ContractError>>()?;

        let changes = self
            .changes
            .iter()
            .map(|c| {
                if c.reason.trim().is_empty() {
                    return Err(ContractError::Validation(format!(
                        "change record for gate `{}` has a blank reason. A \
                         reason nobody wrote is not a reason.",
                        c.gate
                    )));
                }
                if !gates.iter().any(|g| g.id.as_str() == c.gate) {
                    return Err(ContractError::Validation(format!(
                        "change record names `{}`, which is not a declared gate",
                        c.gate
                    )));
                }
                Ok(GateChange {
                    gate: c.gate.clone(),
                    reason: c.reason.clone(),
                })
            })
            .collect::<Result<Vec<_>, ContractError>>()?;

        Ok(Contract {
            version: self.version,
            gates,
            budget_observation_bytes: self
                .budget
                .and_then(|b| b.observation_bytes)
                .unwrap_or(crate::DEFAULT_OBSERVATION_BYTES),
            baseline_ref: self.baseline.and_then(|b| b.ref_),
            judgement,
            suppressions,
            changes,
        })
    }
}

impl RawGate {
    fn to_gate(&self) -> Result<Gate, ContractError> {
        let id = GateId::new(self.id.clone())
            .map_err(|e| ContractError::Validation(format!("gate id: {e:?}")))?;
        let primitive =
            parse_primitive(&self.check).ok_or_else(|| ContractError::InvalidValue {
                key: format!("gates.{id}.check"),
                expected: format!("one of: {}", known_primitives().join(", ")),
                found: self.check.clone(),
                line: None,
            })?;
        // The gates that audit the contract are part of the supervisor, not
        // the project's configuration. A contract that declares one is
        // mistaken, and a contract that could *remove* one would be a gate
        // that can be switched off by the thing it guards — which is the
        // Goodhart attack this whole milestone exists to stop, aimed at the
        // defence itself.
        if matches!(
            primitive,
            Primitive::ContractNotLoosened | Primitive::ContractReviewStale
        ) {
            return Err(ContractError::Validation(format!(
                "gate `{id}` declares `{primitive}`, which is always on and \
                 cannot be declared, given a severity, or removed. It is a \
                 property of the supervisor, not of the contract."
            )));
        }

        let severity = match &self.severity {
            None => Severity::DEFAULT,
            Some(s) => parse_severity(s).ok_or_else(|| ContractError::InvalidValue {
                key: format!("gates.{id}.severity"),
                expected: "one of: off, warn, error, escalate".to_string(),
                found: s.clone(),
                line: None,
            })?,
        };

        // Keys that only mean something for one primitive. A gate carrying
        // `paths` but not being `paths_unchanged` is a mistake that would
        // otherwise read as a setting that does nothing.
        if !self.paths.is_empty() && primitive != Primitive::PathsUnchanged {
            return Err(ContractError::Validation(format!(
                "gate `{id}` declares `paths`, which only `paths_unchanged` uses"
            )));
        }
        if primitive == Primitive::ExternalTool {
            // `tool` and `args` are the whole gate. Without them there is
            // nothing to run, and a declared `external_tool` with no program
            // would be a gate that silently does nothing -- the same class of
            // hole as an unimplemented primitive, and the reason the
            // unimplemented ones are refused outright.
            let Some(tool) = self.tool.as_deref() else {
                return Err(ContractError::Validation(format!(
                    "gate `{id}` is `external_tool` but declares no `tool`"
                )));
            };
            if self.args.is_empty() {
                return Err(ContractError::Validation(format!(
                    "gate `{id}` is `external_tool` but declares no `args`. An \
                     empty argv would run `{tool}` with no arguments, which is \
                     not a gate."
                )));
            }
            match self.format.as_deref() {
                None => {}
                Some("sarif") | Some("lines") => {}
                Some(other) => {
                    return Err(ContractError::Validation(format!(
                        "gate `{id}` declares format `{other}`; expected `sarif` or `lines`"
                    )));
                }
            }
        } else if self.tool.is_some() || !self.args.is_empty() || self.format.is_some() {
            return Err(ContractError::Validation(format!(
                "gate `{id}` declares `tool`/`args`/`format`, which only \
                 `external_tool` uses"
            )));
        }

        Ok(Gate {
            id,
            primitive,
            severity,
            reason: self.reason.clone(),
            provides: self.provides.clone(),
            consumes: self.consumes.clone(),
            paths: self.paths.clone(),
            allow: self.allow.clone(),
            tool: self.tool.clone(),
            args: self.args.clone(),
            format: self.format.clone(),
            timeout_seconds: self.timeout_seconds,
            calibration: self.calibration.clone(),
        })
    }
}

/// Hand-parsed so a misspelling is an error naming the key, not a default
/// that silently weakens the contract.
fn parse_severity(s: &str) -> Option<Severity> {
    match s {
        "off" => Some(Severity::Off),
        "warn" => Some(Severity::Warn),
        "error" => Some(Severity::Error),
        "escalate" => Some(Severity::Escalate),
        _ => None,
    }
}

fn parse_primitive(s: &str) -> Option<Primitive> {
    ALL_PRIMITIVES.iter().copied().find(|p| p.as_str() == s)
}

fn known_primitives() -> Vec<&'static str> {
    ALL_PRIMITIVES.iter().map(|p| p.as_str()).collect()
}

/// Cycle detection over `provides`/`consumes`, reported as a path so the
/// error names the loop rather than just its existence.
fn find_cycle(gates: &[Gate]) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unvisited,
        InProgress,
        Done,
    }
    let mut marks = vec![Mark::Unvisited; gates.len()];
    let mut stack: Vec<usize> = Vec::new();

    // Iterative so a pathological contract cannot blow the stack.
    for start in 0..gates.len() {
        if marks[start] != Mark::Unvisited {
            continue;
        }
        let mut work = vec![(start, 0usize)];
        while let Some((node, edge)) = work.pop() {
            if edge == 0 {
                if marks[node] == Mark::Done {
                    continue;
                }
                if marks[node] == Mark::InProgress {
                    continue;
                }
                marks[node] = Mark::InProgress;
                stack.push(node);
            }
            let edges: Vec<&str> = gates[node]
                .provides
                .iter()
                .chain(gates[node].consumes.iter())
                .map(String::as_str)
                .collect();
            if edge < edges.len() {
                work.push((node, edge + 1));
                if let Some(next) = gates.iter().position(|g| edges[edge] == g.id.as_str()) {
                    match marks[next] {
                        Mark::InProgress => {
                            let mut cycle = vec![gates[next].id.as_str().to_string()];
                            for &i in stack.iter().skip_while(|&&i| i != next) {
                                cycle.push(gates[i].id.as_str().to_string());
                            }
                            return Some(cycle);
                        }
                        Mark::Unvisited => work.push((next, 0)),
                        Mark::Done => {}
                    }
                }
            } else {
                marks[node] = Mark::Done;
                stack.pop();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
version = 1

[[gates]]
id = "no_new_dependencies"
check = "dependency_surface_unchanged"

[judgement]
reviewed = "2026-09-28"
not_covered = ["whether the design is the right one"]
"#;

    #[test]
    fn a_minimal_contract_parses() {
        let c = parse_contract(MINIMAL).expect("minimal contract");
        assert_eq!(c.version, 1);
        assert_eq!(c.gates.len(), 1);
        // PRD 5: new gates default to warn.
        assert_eq!(c.gates[0].severity, Severity::Warn);
        assert_eq!(c.judgement.reviewed.as_deref(), Some("2026-09-28"));
    }

    #[test]
    fn a_misspelled_key_is_an_error() {
        // The single most important line in slop-gate's README, and the reason
        // a typo can never silently disable a gate.
        let src = MINIMAL.replace("check = ", "chek = ");
        let err = parse_contract(&src).unwrap_err();
        assert!(
            matches!(err, ContractError::Syntax { .. }),
            "expected a parse failure, got {err:?}"
        );
        assert!(err.to_string().to_lowercase().contains("chek"));
    }

    #[test]
    fn an_unknown_top_level_key_is_an_error() {
        let src = format!("{MINIMAL}\nbudgett = 1\n");
        let err = parse_contract(&src).unwrap_err();
        assert!(err.to_string().contains("budgett"), "got {err}");
    }

    #[test]
    fn an_unknown_check_name_is_an_error() {
        let src = MINIMAL.replace("dependency_surface_unchanged", "dependancy_surface");
        let err = parse_contract(&src).unwrap_err();
        assert!(matches!(err, ContractError::InvalidValue { .. }), "{err:?}");
        assert!(err.to_string().contains("dependancy_surface"));
    }

    #[test]
    fn a_misspelled_severity_is_an_error_not_a_default() {
        let src = MINIMAL.replace(
            "check = \"dependency_surface_unchanged\"",
            "check = \"dependency_surface_unchanged\"\nseverity = \"erorr\"",
        );
        let err = parse_contract(&src).unwrap_err();
        assert!(matches!(err, ContractError::InvalidValue { .. }), "{err:?}");
        assert!(err.to_string().contains("erorr"));
    }

    #[test]
    fn a_missing_review_date_is_an_error() {
        let src = "version = 1\n[judgement]\nnot_covered = []\n";
        let err = parse_contract(src).unwrap_err();
        assert!(err.to_string().contains("reviewed"), "{err}");
    }

    #[test]
    fn a_duplicate_gate_id_is_an_error() {
        let src = format!(
            "{MINIMAL}\n[[gates]]\nid = \"no_new_dependencies\"\ncheck = \"secret_absent\"\n"
        );
        let err = parse_contract(&src).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn an_undeclared_edge_is_an_error() {
        let src = MINIMAL.replace(
            "check = \"dependency_surface_unchanged\"",
            "check = \"dependency_surface_unchanged\"\nconsumes = [\"checks_green\"]",
        );
        let err = parse_contract(&src).unwrap_err();
        assert!(err.to_string().contains("not a declared gate"), "{err}");
    }

    #[test]
    fn a_provides_cycle_is_rejected() {
        let src = r#"
version = 1
[[gates]]
id = "a"
check = "tests_not_deleted"
consumes = ["b"]
[[gates]]
id = "b"
check = "tests_not_deleted"
consumes = ["a"]
[judgement]
reviewed = "2026-09-28"
"#;
        let err = parse_contract(src).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    #[test]
    fn paths_on_the_wrong_primitive_is_an_error() {
        let src = MINIMAL.replace(
            "check = \"dependency_surface_unchanged\"",
            "check = \"dependency_surface_unchanged\"\npaths = [\"fixtures/\"]",
        );
        let err = parse_contract(&src).unwrap_err();
        assert!(err.to_string().contains("paths_unchanged"), "{err}");
    }

    #[test]
    fn an_unsupported_version_is_rejected() {
        let src = MINIMAL.replace("version = 1", "version = 2");
        let err = parse_contract(&src).unwrap_err();
        assert!(err.to_string().contains("version 2"), "{err}");
    }
}

#[cfg(test)]
mod external_tool_config_tests {
    use super::*;

    fn contract_with(gate: &str) -> Result<Contract, ContractError> {
        parse_contract(&format!(
            "version = 1\n[[gates]]\nid = \"g\"\ncheck = \"external_tool\"\n{gate}\n\
             [judgement]\nreviewed = \"2026-09-28\"\n"
        ))
    }

    #[test]
    fn an_external_tool_gate_needs_a_tool_and_args() {
        // Without them there is nothing to run, and a declared
        // `external_tool` with no program would be a gate that silently does
        // nothing -- the same hole as an unimplemented primitive, and the
        // reason those are refused outright.
        let err = contract_with("severity = \"warn\"").unwrap_err();
        assert!(err.to_string().contains("no `tool`"), "{err}");
        let err = contract_with("tool = \"slop-gate\"").unwrap_err();
        assert!(err.to_string().contains("no `args`"), "{err}");
    }

    #[test]
    fn a_well_formed_external_tool_gate_parses() {
        let c = contract_with(
            "tool = \"slop-gate\"\n\
             args = [\"check\", \"--base\", \"{base}\", \"--head\", \"{head}\"]\n\
             format = \"sarif\"\nseverity = \"warn\"",
        )
        .expect("valid");
        let g = &c.gates[0];
        assert_eq!(g.tool.as_deref(), Some("slop-gate"));
        assert_eq!(g.format.as_deref(), Some("sarif"));
        // The placeholders survive parsing; substitution happens at run time
        // and an unresolved one is an error there.
        assert!(g.args.contains(&"{base}".to_string()));
    }

    #[test]
    fn an_unknown_output_format_is_rejected() {
        // A typo here is a gate that silently parses nothing and looks clean.
        let err = contract_with(
            "tool = \"t\"\nargs = [\"check\"]\nformat = \"sraif\"\nseverity = \"warn\"",
        )
        .unwrap_err();
        assert!(err.to_string().contains("sraif"), "{err}");
    }

    #[test]
    fn tool_keys_on_another_primitive_are_rejected() {
        // A gate carrying `tool` but not being `external_tool` is a mistake
        // that would otherwise read as a setting that does nothing.
        let err = parse_contract(
            "version = 1\n[[gates]]\nid = \"g\"\ncheck = \"tests_not_deleted\"\n\
             tool = \"slop-gate\"\n[judgement]\nreviewed = \"2026-09-28\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("only `external_tool`"), "{err}");
    }
}
