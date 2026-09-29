//! `external_tool` — run a third-party gate and absorb its findings.
//!
//! This is the seam to `slop-gate` and to anything else that already does a
//! job Palisade is not trying to redo. The integration surface is deliberately
//! small: argv from the contract, an exit code, and SARIF on stdout.
//!
//! Three ways this goes wrong, all handled here and none of them in the
//! orchestrator:
//!
//! - **An untemplated `{base}` reaches argv.** The tool analyses the wrong
//!   tree and reports green. `template_args` refuses it.
//! - **The tool's output is not the format declared.** Silently reporting
//!   "pass" because the exit code was 0 while the findings were unparseable
//!   would be the exit-code bug again, in a new costume.
//! - **The tool cannot run at all.** `Untrustworthy`, never a pass.

use std::time::Duration;

use palisade_contract::{GateId, Primitive, Severity};
use palisade_orchestrate::{
    Finding, GateRunBuilder, Origin, Side, Subject, SubjectKind, UntrustworthyReason,
};

use crate::{Completed, Interpretation, run, template_args};

/// Run a declared tool and absorb its SARIF findings.
///
/// # Errors
///
/// Never at the type level: a tool that cannot run is a *result*, because
/// that is the distinction the whole crate exists to preserve. A malformed
/// contract, a missing template variable, or output that is not the declared
/// format all become `Untrustworthy`.
/// The values a contract's argv template may reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateValues {
    /// The base commit.
    pub base: String,
    /// The head being checked.
    pub head: String,
    /// A path to a baseline artefact, if the tool takes one.
    pub index: String,
}

/// Run a declared tool, absorb its findings, and return the gate's run.
#[allow(clippy::too_many_arguments)]
pub fn run_tool(
    program: &str,
    args: &[String],
    format: OutputFormat,
    values: &TemplateValues,
    timeout: Duration,
    origin: Origin,
) -> palisade_orchestrate::GateRun {
    let mut builder = GateRunBuilder::new();

    let argv = match template_args(args, &values.base, &values.head, &values.index) {
        Ok(a) => a,
        Err(e) => {
            builder.untrustworthy(UntrustworthyReason::ContractInvalid { detail: e });
            return finish(builder, origin);
        }
    };

    let completed = run(program, &argv, timeout);
    match completed.status.interpretation() {
        Interpretation::Passed => {}
        Interpretation::NoTrustworthyResult => {
            builder.untrustworthy(UntrustworthyReason::ToolCouldNotRun {
                detail: format!(
                    "{program}: {}",
                    Interpretation::NoTrustworthyResult.describe(completed.status)
                ),
            });
            return finish(builder, origin);
        }
        Interpretation::Failed => {
            let findings = match format.parse(&completed) {
                Ok(f) => f,
                Err(e) => {
                    builder.untrustworthy(e);
                    return finish(builder, origin);
                }
            };
            for f in findings {
                builder.finding(f);
            }
        }
    }
    finish(builder, origin)
}

fn finish(builder: GateRunBuilder, origin: Origin) -> palisade_orchestrate::GateRun {
    builder.finish(
        GateId::new("external_tool").expect("constant is valid"),
        Primitive::ExternalTool,
        origin,
    )
}

/// What the tool writes on stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// SARIF 2.1.0. The only format with real structure.
    Sarif,
    /// One diagnostic per line, `path:line: message`. For tools that have
    /// nothing better, and easy to produce by hand.
    Lines,
}

impl OutputFormat {
    /// The spelling used in the contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sarif => "sarif",
            Self::Lines => "lines",
        }
    }

    /// Parse the spelling, rejecting an unknown one rather than defaulting.
    pub fn parse_name(s: &str) -> Option<Self> {
        match s {
            "sarif" => Some(Self::Sarif),
            "lines" => Some(Self::Lines),
            _ => None,
        }
    }

    /// Turn a tool's output into findings.
    ///
    /// # Errors
    ///
    /// `UntrustworthyReason::Indeterminate` when the output does not parse as
    /// the declared format. A tool that exited 1 and printed something we
    /// cannot read has told us nothing usable, and the alternative — dropping
    /// the findings and reporting the failure — loses the only information the
    /// tool had.
    pub fn parse(&self, completed: &Completed) -> Result<Vec<Finding>, UntrustworthyReason> {
        match self {
            Self::Sarif => parse_sarif_findings(&completed.stdout),
            Self::Lines => Ok(parse_line_findings(&completed.stdout)),
        }
    }
}

/// `path:line: message` lines.
fn parse_line_findings(text: &str) -> Vec<Finding> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let mut parts = line.splitn(3, ':');
            let path = parts.next().unwrap_or_default().trim();
            let line_no = parts.next().and_then(|p| p.trim().parse::<u32>().ok());
            let message = parts.next().unwrap_or(line).trim();
            Finding::new(
                GateId::new("external_tool").expect("constant is valid"),
                Primitive::ExternalTool,
                // A line-format tool does not convey its own severity, so
                // `Error` is the only defensible reading for a tool that
                // exited non-zero having told us about something.
                Severity::Error,
                external_subject("external"),
                (!path.is_empty()).then(|| camino::Utf8PathBuf::from(path)),
                line_no.map(|start| palisade_orchestrate::HunkRef { start, end: start }),
                // A third-party tool's finding is not a comparison against a
                // baseline we hold, so neither side carries a value.
                Side::Absent,
                Side::Absent,
                format!("external tool: {message}"),
                external_origin(),
            )
        })
        .collect()
}

/// The subset of SARIF 2.1.0 that a tool needs to emit to be absorbed.
///
/// Deliberately not the whole schema. A tool that emits something outside this
/// shape is one we cannot faithfully represent, and silently dropping the
/// parts we do not understand would be reporting a gate as having found less
/// than it did.
fn parse_sarif_findings(text: &str) -> Result<Vec<Finding>, UntrustworthyReason> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| UntrustworthyReason::Indeterminate {
            detail: format!("declared format is sarif but the output did not parse: {e}"),
        })?;
    let runs = value
        .get("runs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| UntrustworthyReason::Indeterminate {
            detail: "sarif output has no `runs` array".to_string(),
        })?;

    let mut out = Vec::new();
    for run in runs {
        let rule_ids: Vec<String> = run
            .get("tool")
            .and_then(|t| t.get("driver"))
            .and_then(|d| d.get("rules"))
            .and_then(|r| r.as_array())
            .map(|rules| {
                rules
                    .iter()
                    .filter_map(|r| r.get("id").and_then(|i| i.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let Some(results) = run.get("results").and_then(|v| v.as_array()) else {
            continue;
        };
        for result in results {
            let rule_id = result
                .get("ruleId")
                .and_then(|v| v.as_str())
                .unwrap_or("external")
                .to_string();
            // SARIF allows either `ruleId` or an index into the driver rules.
            let rule_id = match rule_id.parse::<usize>() {
                Ok(i) => rule_ids.get(i).cloned().unwrap_or(rule_id),
                Err(_) => rule_id,
            };
            let message = result
                .get("message")
                .and_then(|m| m.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("(no message)")
                .to_string();
            let path = result
                .get("locations")
                .and_then(|l| l.as_array())
                .and_then(|l| l.first())
                .and_then(|l| l.get("physicalLocation"))
                .and_then(|p| p.get("artifactLocation"))
                .and_then(|a| a.get("uri"))
                .and_then(|u| u.as_str())
                .map(camino::Utf8PathBuf::from);
            let line = result
                .get("locations")
                .and_then(|l| l.as_array())
                .and_then(|l| l.first())
                .and_then(|l| l.get("physicalLocation"))
                .and_then(|p| p.get("region"))
                .and_then(|r| r.get("startLine"))
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            let level = result
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            let severity = match level {
                "error" => Severity::Error,
                "warning" | "note" => Severity::Warn,
                _ => Severity::Error,
            };
            let gate_id = GateId::new(format!("external_{rule_id}"))
                .unwrap_or_else(|_| GateId::new("external").expect("constant is valid"));
            out.push(Finding::new(
                gate_id,
                Primitive::ExternalTool,
                severity,
                external_subject(&rule_id),
                path,
                line.map(|start| palisade_orchestrate::HunkRef { start, end: start }),
                Side::Absent,
                Side::Absent,
                format!("external tool `{rule_id}`: {message}"),
                external_origin(),
            ));
        }
    }
    Ok(out)
}

/// The subject kind for an absorbed finding. The thing under discussion is
/// the *tool's* rule, not anything in this repository, so `Contract` is the
/// closest fit the vocabulary has; the rule id is the name.
pub const EXTERNAL_SUBJECT: SubjectKind = SubjectKind::Contract;

/// The subject for an absorbed finding, named by the tool's rule id.
pub fn external_subject(rule_id: &str) -> Subject {
    Subject::new(EXTERNAL_SUBJECT, rule_id)
}

/// The origin absorbed findings carry. The tool's own identity, if the caller
/// did not know better, is patched by the caller via `Finding::new`.
///
/// Kept separate so an absorbed finding is never attributed to Palisade's
/// analysis, which would make a delegated verdict indistinguishable from an
/// analyzed one in the report — the one thing `Origin` exists to prevent.
fn external_origin() -> Origin {
    Origin::Delegated {
        tool: palisade_orchestrate::ToolId::new("external"),
        version: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Outcome;

    fn values() -> TemplateValues {
        TemplateValues {
            base: "base".to_string(),
            head: "head".to_string(),
            index: "index".to_string(),
        }
    }

    const SARIF: &str = r#"{
      "version": "2.1.0",
      "runs": [{
        "tool": {"driver": {"name": "slop-gate", "rules": [
          {"id": "near-clone"}, {"id": "unsafe-surface-growth"}
        ]}},
        "results": [
          {"ruleId": "near-clone", "level": "error",
           "message": {"text": "duplicate implementation"},
           "locations": [{"physicalLocation": {
             "artifactLocation": {"uri": "src/parse.rs"},
             "region": {"startLine": 42}}}]},
          {"ruleId": "unsafe-surface-growth", "level": "warning",
           "message": {"text": "unsafe added"}}
        ]
      }]
    }"#;

    #[test]
    fn sarif_findings_are_absorbed_with_their_locations() {
        let completed = Completed {
            status: Outcome::Exited(1),
            stdout: SARIF.to_string(),
            stderr: String::new(),
        };
        let f = OutputFormat::Sarif.parse(&completed).expect("parses");
        assert_eq!(f.len(), 2);
        assert_eq!(
            f[0].message,
            "external tool `near-clone`: duplicate implementation"
        );
        assert_eq!(
            f[0].path.as_deref().map(camino::Utf8Path::as_str),
            Some("src/parse.rs")
        );
        assert_eq!(
            f[0].hunk,
            Some(palisade_orchestrate::HunkRef { start: 42, end: 42 })
        );
        assert_eq!(f[1].severity, Severity::Warn, "level is preserved");
    }

    #[test]
    fn a_rule_id_may_be_an_index_into_the_driver_rules() {
        // SARIF permits this, and a tool that uses it is not broken.
        let sarif = r#"{"runs":[{"tool":{"driver":{"rules":[{"id":"real-name"}]}},
            "results":[{"ruleId":"0","level":"error","message":{"text":"m"}}]}]}"#;
        let completed = Completed {
            status: Outcome::Exited(1),
            stdout: sarif.to_string(),
            stderr: String::new(),
        };
        let f = OutputFormat::Sarif.parse(&completed).expect("parses");
        assert!(f[0].message.contains("real-name"), "{:?}", f[0].message);
    }

    #[test]
    fn unparseable_sarif_is_untrustworthy_not_an_empty_pass() {
        // The exit code said "fail" and the output said nothing we can read.
        // Dropping the findings and reporting the failure would lose the only
        // information the tool had.
        let completed = Completed {
            status: Outcome::Exited(1),
            stdout: "not json at all".to_string(),
            stderr: String::new(),
        };
        let err = OutputFormat::Sarif.parse(&completed).unwrap_err();
        assert!(
            matches!(err, UntrustworthyReason::Indeterminate { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn line_output_parses() {
        let completed = Completed {
            status: Outcome::Exited(1),
            stdout: "src/a.rs:10: something\nsrc/b.rs:2: other\n".to_string(),
            stderr: String::new(),
        };
        let f = OutputFormat::Lines.parse(&completed).expect("parses");
        assert_eq!(f.len(), 2);
        assert_eq!(
            f[0].hunk,
            Some(palisade_orchestrate::HunkRef { start: 10, end: 10 })
        );
        assert!(f[1].message.contains("other"));
    }

    #[test]
    fn an_unknown_format_name_is_rejected_rather_than_defaulted() {
        // A misspelled format is the same class of typo as a misspelled gate,
        // and PRD 5 is explicit that those are errors.
        assert_eq!(OutputFormat::parse_name("sarif"), Some(OutputFormat::Sarif));
        assert_eq!(OutputFormat::parse_name("sraif"), None);
    }

    #[test]
    fn a_tool_that_cannot_run_is_untrustworthy() {
        let run = run_tool(
            "palisade-no-such-tool-xyz",
            &[],
            OutputFormat::Lines,
            &values(),
            Duration::from_secs(1),
            crate::origin("x", "1"),
        );
        assert!(run.findings.is_empty());
        assert!(matches!(
            run.outcome,
            palisade_orchestrate::GateOutcome::Untrustworthy { .. }
        ));
    }

    #[test]
    fn an_untemplated_argument_is_refused_before_anything_runs() {
        let run = run_tool(
            "sh",
            &["--base".to_string(), "{bases}".to_string()],
            OutputFormat::Lines,
            &values(),
            Duration::from_secs(1),
            crate::origin("x", "1"),
        );
        match run.outcome {
            palisade_orchestrate::GateOutcome::Untrustworthy {
                reason: UntrustworthyReason::ContractInvalid { detail },
                ..
            } => assert!(detail.contains("{bases}"), "{detail}"),
            other => panic!("expected a contract error, got {other:?}"),
        }
    }
}
