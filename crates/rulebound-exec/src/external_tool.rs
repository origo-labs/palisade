//! `external_tool` — run a third-party gate and absorb its findings.
//!
//! This is the seam to `slop-gate` and to anything else that already does a
//! job Rulebound is not trying to redo. The integration surface is deliberately
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

use rulebound_contract::{GateId, Primitive, Severity};
use rulebound_orchestrate::{
    Finding, GateRunBuilder, Origin, Side, Subject, SubjectKind, ToolId, UntrustworthyReason,
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
    gate_id: &GateId,
    program: &str,
    args: &[String],
    format: OutputFormat,
    values: &TemplateValues,
    timeout: Duration,
    origin: Origin,
) -> rulebound_orchestrate::GateRun {
    let mut builder = GateRunBuilder::new();

    let argv = match template_args(args, &values.base, &values.head, &values.index) {
        Ok(a) => a,
        Err(e) => {
            builder.untrustworthy(UntrustworthyReason::ContractInvalid { detail: e });
            return finish(builder, gate_id, origin);
        }
    };

    let completed = run(program, &argv, timeout);
    match completed
        .status
        .interpretation_with(crate::cargo_exit_codes())
    {
        Interpretation::NoTrustworthyResult => {
            builder.untrustworthy(UntrustworthyReason::ToolCouldNotRun {
                detail: format!(
                    "{program}: {}",
                    Interpretation::NoTrustworthyResult.describe(completed.status)
                ),
            });
            return finish(builder, gate_id, origin);
        }
        // **Exit 0 does not mean "no findings".**
        //
        // `slop-gate` reports its findings on stdout and exits 0 when the
        // findings are at `warning` level, exiting non-zero only for
        // `error`. So a tool that reports problems without blocking reports
        // them *and* says "I passed", and a gate that branches on the exit code
        // alone discards every one of them.
        //
        // Found by running it: the first live invocation absorbed zero
        // findings from a tool that had one, and the report said `pass`. The
        // exit code answers "did the tool fail to do its job", and only the
        // output answers "did the tool find something". A tool's exit code is
        // a statement about the *tool*, never about the code it inspected.
        Interpretation::Passed | Interpretation::Failed => {
            let (tool_id, version) = match &origin {
                Origin::Delegated { tool, version } => (tool.clone(), version.clone()),
                Origin::Analyzed { .. } => (ToolId::new("external"), String::new()),
            };
            let findings = match format.parse(&completed, &tool_id, &version) {
                Ok(f) => f,
                Err(e) => {
                    // Unparseable output is a real problem even when the tool
                    // exited 0: something said "I passed" and then said
                    // nothing we can read.
                    builder.untrustworthy(e);
                    return finish(builder, gate_id, origin);
                }
            };
            for f in findings {
                builder.finding(f);
            }
            if completed
                .status
                .interpretation_with(crate::cargo_exit_codes())
                == Interpretation::Failed
                && builder_is_empty(&builder)
            {
                // The tool failed and told us nothing about why. That is not
                // a clean run, and not a set of findings either.
                builder.untrustworthy(UntrustworthyReason::Indeterminate {
                    detail: format!(
                        "{program} exited {} and reported no diagnostic we could \
                         read",
                        exit_of(completed.status)
                    ),
                });
            }
        }
    }
    finish(builder, gate_id, origin)
}

fn exit_of(status: crate::Outcome) -> i32 {
    match status {
        crate::Outcome::Exited(c) => c,
        _ => -1,
    }
}

fn builder_is_empty(b: &GateRunBuilder) -> bool {
    !b.has_findings()
}

/// Stamp the run with the gate the *contract* declared.
///
/// Not the primitive's name. An earlier version hard-coded `external_tool`,
/// and the report's reordering pass — which matches runs to contract entries
/// by id — silently dropped the run. A gate that vanishes from its own report
/// is the worst failure mode available: no error, no finding, and a verdict
/// that looks earned.
fn finish(
    builder: GateRunBuilder,
    gate_id: &GateId,
    origin: Origin,
) -> rulebound_orchestrate::GateRun {
    builder.finish(gate_id.clone(), Primitive::ExternalTool, origin)
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

    /// Turn a tool's output into findings, attributed to `tool`.
    ///
    /// # Errors
    ///
    /// `UntrustworthyReason::Indeterminate` when the output does not parse as
    /// the declared format. A tool that exited 1 and printed something we
    /// cannot read has told us nothing usable, and the alternative — dropping
    /// the findings and reporting the failure — loses the only information the
    /// tool had.
    pub fn parse(
        &self,
        completed: &Completed,
        tool: &ToolId,
        version: &str,
    ) -> Result<Vec<Finding>, UntrustworthyReason> {
        // Every absorbed finding carries the *real* tool, not a placeholder.
        // A finding attributed to "external" is unattributable, and saying
        // which third party produced a verdict is the single most useful thing
        // the origin field carries.
        let origin = Origin::Delegated {
            tool: tool.clone(),
            version: version.to_string(),
        };
        match self {
            Self::Sarif => parse_sarif_findings(&completed.stdout, &origin),
            Self::Lines => Ok(parse_line_findings(&completed.stdout, &origin)),
        }
    }
}

/// `path:line: message` lines.
fn parse_line_findings(text: &str, origin: &Origin) -> Vec<Finding> {
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
                line_no.map(|start| rulebound_orchestrate::HunkRef { start, end: start }),
                // A third-party tool's finding is not a comparison against a
                // baseline we hold, so neither side carries a value.
                Side::Absent,
                Side::Absent,
                format!("{}: {message}", origin_label(origin)),
                origin.clone(),
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
fn parse_sarif_findings(text: &str, origin: &Origin) -> Result<Vec<Finding>, UntrustworthyReason> {
    // A tool may print a progress line before its report. Find the JSON object
    // rather than requiring it to be the whole stream: a leading
    // `Fetching index` is not a reason to throw away a valid SARIF document,
    // and "did not parse" is a much less actionable message than the output it
    // failed on. Bounded so a huge non-JSON stream is not scanned forever.
    const SEARCH_WINDOW: usize = 64 * 1024;
    let trimmed = text.trim_start();
    let candidate = if trimmed.starts_with('{') {
        trimmed
    } else {
        match trimmed.find('{') {
            Some(i) if i < SEARCH_WINDOW => &trimmed[i..],
            _ => trimmed,
        }
    };
    let value: serde_json::Value =
        serde_json::from_str(candidate).map_err(|e| UntrustworthyReason::Indeterminate {
            detail: format!(
                "declared format is sarif but the output did not parse: {e}. \\
                 First bytes were: {:?}",
                &text[..text.len().min(200)]
            ),
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
                line.map(|start| rulebound_orchestrate::HunkRef { start, end: start }),
                Side::Absent,
                Side::Absent,
                format!("{} `{rule_id}`: {message}", origin_label(origin)),
                origin.clone(),
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

/// A readable name for the producing tool, for a message.
fn origin_label(origin: &Origin) -> String {
    match origin {
        Origin::Delegated { tool, .. } => format!("external tool `{tool}`"),
        Origin::Analyzed { primitive } => format!("gate `{primitive}`"),
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
        let f = OutputFormat::Sarif
            .parse(&completed, &ToolId::new("slop-gate"), "0.5.0")
            .expect("parses");
        assert_eq!(f.len(), 2);
        // The message names the tool that produced it, not a placeholder.
        assert_eq!(
            f[0].message,
            "external tool `slop-gate` `near-clone`: duplicate implementation"
        );
        assert!(
            matches!(&f[0].origin, Origin::Delegated { tool, .. } if tool.as_str() == "slop-gate"),
            "an absorbed finding must be attributed to the real tool: {:?}",
            f[0].origin
        );
        assert_eq!(
            f[0].path.as_deref().map(camino::Utf8Path::as_str),
            Some("src/parse.rs")
        );
        assert_eq!(
            f[0].hunk,
            Some(rulebound_orchestrate::HunkRef { start: 42, end: 42 })
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
        let f = OutputFormat::Sarif
            .parse(&completed, &ToolId::new("slop-gate"), "0.5.0")
            .expect("parses");
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
        let err = OutputFormat::Sarif
            .parse(&completed, &ToolId::new("slop-gate"), "0.5.0")
            .unwrap_err();
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
        let f = OutputFormat::Lines
            .parse(&completed, &ToolId::new("slop-gate"), "0.5.0")
            .expect("parses");
        assert_eq!(f.len(), 2);
        assert_eq!(
            f[0].hunk,
            Some(rulebound_orchestrate::HunkRef { start: 10, end: 10 })
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
            &GateId::new("t").unwrap(),
            "rulebound-no-such-tool-xyz",
            &[],
            OutputFormat::Lines,
            &values(),
            Duration::from_secs(1),
            crate::origin("x", "1"),
        );
        assert!(run.findings.is_empty());
        assert!(matches!(
            run.outcome,
            rulebound_orchestrate::GateOutcome::Untrustworthy { .. }
        ));
    }

    #[test]
    fn an_untemplated_argument_is_refused_before_anything_runs() {
        let run = run_tool(
            &GateId::new("t").unwrap(),
            "sh",
            &["--base".to_string(), "{bases}".to_string()],
            OutputFormat::Lines,
            &values(),
            Duration::from_secs(1),
            crate::origin("x", "1"),
        );
        match run.outcome {
            rulebound_orchestrate::GateOutcome::Untrustworthy {
                reason: UntrustworthyReason::ContractInvalid { detail },
                ..
            } => assert!(detail.contains("{bases}"), "{detail}"),
            other => panic!("expected a contract error, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod absorbed_findings_tests {
    use super::*;
    use crate::Outcome;

    fn values() -> TemplateValues {
        TemplateValues {
            base: "base".into(),
            head: "head".into(),
            index: "idx.json".into(),
        }
    }

    /// A tool that reports its findings and exits **0**, which is what
    /// `slop-gate` does for anything at `warning` level.
    const SARIF_WITH_FINDING: &str = r#"{
      "version": "2.1.0",
      "runs": [{ "tool": {"driver": {"rules": [{"id": "structural-erosion"}]}},
        "results": [{"ruleId": "structural-erosion", "level": "warning",
          "message": {"text": "structural erosion at 67.42%"},
          "locations": [{"physicalLocation": {
            "artifactLocation": {"uri": "src/search/hybrid.rs"},
            "region": {"startLine": 156}}}]}]}]}"#;

    /// A real program that behaves exactly that way, so the test is not
    /// asserting against a stub. It writes a SARIF document and exits **0**,
    /// which is the whole point: a tool that finds problems without blocking.
    /// A unique name per test so two tests writing the same path cannot race.
    fn tool_script(tag: &str, payload: &str) -> String {
        let body = format!("#!/bin/sh\ncat <<'RULEBOUND_EOF'\n{payload}\nRULEBOUND_EOF\nexit 0\n");
        let dir = std::env::temp_dir();
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join(format!("rulebound-et-{}-{tag}", std::process::id()));
        std::fs::write(&path, body).expect("write tool");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn exit_zero_with_findings_is_not_a_pass() {
        // The bug this test exists for. `slop-gate` reports its findings on
        // stdout and exits 0 at warning level, so a gate that branched on the
        // exit code alone reported `pass` while absorbing nothing. The first
        // live invocation did exactly that: a tool with one finding, and a
        // report that said `pass`.
        let tool = tool_script("exit0", SARIF_WITH_FINDING);
        let run = run_tool(
            &GateId::new("slop_gate").unwrap(),
            &tool,
            &[],
            OutputFormat::Sarif,
            &values(),
            Duration::from_secs(10),
            crate::origin("slop-gate", "0.5.0"),
        );
        assert_eq!(run.findings.len(), 1, "the finding must survive: {run:?}");
        assert!(run.findings[0].message.contains("structural erosion"));
    }

    #[test]
    fn the_run_carries_the_gate_the_contract_declared() {
        // Not the primitive's name. An earlier version hard-coded
        // `external_tool`, and the report's reordering pass -- which matches
        // runs to contract entries by id -- silently dropped the run. A gate
        // that vanishes from its own report is the worst failure available: no
        // error, no finding, a verdict that looks earned.
        let run = run_tool(
            &GateId::new("slop_gate").unwrap(),
            "sh",
            &["-c".into(), "exit 0".into()],
            OutputFormat::Lines,
            &values(),
            Duration::from_secs(10),
            crate::origin("slop-gate", "0.5.0"),
        );
        assert_eq!(run.gate_id.as_str(), "slop_gate");
    }

    #[test]
    fn a_progress_line_before_the_report_does_not_lose_it() {
        // A tool that prints `Fetching index` before its JSON should not have
        // its report thrown away, and the error should quote the bytes it
        // could not read.
        let text = format!("Fetching index\n{SARIF_WITH_FINDING}");
        let completed = Completed {
            status: Outcome::Exited(0),
            stdout: text,
            stderr: String::new(),
        };
        let f = OutputFormat::Sarif
            .parse(&completed, &ToolId::new("slop-gate"), "0.5.0")
            .expect("should find the report");
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn genuinely_unparseable_output_quotes_what_it_saw() {
        let completed = Completed {
            status: Outcome::Exited(0),
            stdout: "src/a.rs:1: some human output, not json".to_string(),
            stderr: String::new(),
        };
        let err = OutputFormat::Sarif
            .parse(&completed, &ToolId::new("slop-gate"), "0.5.0")
            .unwrap_err();
        let detail = err.detail();
        assert!(
            detail.contains("some human output"),
            "the error must quote the output: {detail}"
        );
    }
}
