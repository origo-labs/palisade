//! `rulebound-report` — the only place a report is produced, in any format.
//!
//! Three formats, and the reason for each:
//!
//! - `human` — a PR comment. The default, because a supervisor's first
//!   audience is a person deciding whether to read the rest.
//! - `json` — the machine contract, with **the three targets of
//!   `EVIDENCE.md` §5 as separate top-level keys**. Conflating "does this
//!   break a stated rule" with "should the supervisor intervene" is what
//!   confounded every model comparison in the predecessor programme, and
//!   separating them in the schema makes recombining them a reporting bug
//!   rather than a measurement bug.
//! - `sarif` — SARIF 2.1.0, so a finding lands in GitHub code scanning with no
//!   extra work.
//!
//! **Determinism is a product property, not a nicety.** Two runs on an
//! unchanged tree must produce identical bytes, because a gate suite whose own
//! output moves cannot be calibrated and a CI baseline that churns is one
//! nobody reads. So: no timestamps, no wall-clock, no hash-map iteration
//! order, and every collection that reaches the output is sorted.

use rulebound_contract::{Contract, Severity};
use rulebound_orchestrate::{Finding, GateOutcome, GateRun, Origin, Side, Verdict};
use serde::Serialize;

/// The tool name written into every artefact.
pub const TOOL_NAME: &str = "rulebound";
/// The SARIF version emitted.
pub const SARIF_VERSION: &str = "2.1.0";
/// The partialFingerprint key carrying [`Finding::fingerprint`].
pub const FINGERPRINT_KEY: &str = "ruleboundFingerprint/v1";

/// The three targets, as top-level JSON keys.
///
/// Named here so the serialiser and the documentation cannot drift, and so the
/// set reads as a commitment: a report that grows a fourth key is making a
/// claim somebody has to evaluate.
pub const JSON_TARGET_KEYS: [&str; 3] = ["acceptance", "rule_violations", "residual_judgement"];

/// Everything a report is made of.
#[derive(Debug, Clone)]
pub struct Report<'a> {
    /// The contract that was run.
    pub contract: &'a Contract,
    /// One entry per declared gate, in declaration order.
    pub runs: &'a [GateRun],
    /// The verdict.
    pub verdict: Verdict,
    /// The tool's own version, for reproducibility.
    pub tool_version: &'a str,
}

impl Report<'_> {
    /// Whether the run should block.
    pub fn verdict(&self) -> Verdict {
        self.verdict
    }

    /// Every finding from every gate, in declaration order.
    pub fn findings(&self) -> impl Iterator<Item = &Finding> {
        self.runs.iter().flat_map(|r| r.findings.iter())
    }

    /// The human report.
    pub fn human(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "contract: {} gate(s), version {}\n\n",
            self.contract.gates.len(),
            self.contract.version
        ));
        for run in self.runs {
            match &run.outcome {
                GateOutcome::Pass { .. } => {
                    out.push_str(&format!("  pass    {}\n", run.primitive));
                }
                GateOutcome::Skipped { .. } => {
                    out.push_str("  off     (declared off, not checked)\n");
                }
                GateOutcome::Untrustworthy { reason, .. } => {
                    out.push_str(&format!(
                        "  ERROR   {}: {}\n",
                        run.primitive,
                        reason.detail()
                    ));
                }
                GateOutcome::Fail(_) => {
                    // Every finding, not just the one that decided the verdict.
                    //
                    // Two lines per finding, and they answer different
                    // questions. The first is the gate's own sentence, which
                    // carries nuance a derived label cannot — why an addition
                    // is not a break, how to silence one. The second is the
                    // structured pair, which is what a consumer reads and
                    // what cannot be relabelled by careless prose.
                    for f in &run.findings {
                        out.push_str(&format!(
                            "  {:<7} [{}] {}\n",
                            f.severity, run.gate_id, f.message
                        ));
                        out.push_str(&format!(
                            "           {} {}: {} -> {}   [{}]\n",
                            f.subject.kind.noun(),
                            f.change().verb(),
                            f.expected.render(),
                            f.observed.render(),
                            f.fingerprint
                        ));
                    }
                }
            }
        }
        out.push('\n');
        out.push_str(&format!("verdict: {}\n", self.verdict));
        // PRD 8: the gap is the artefact somebody owns, so it is printed on
        // every run and cannot quietly stop being true.
        if self.contract.judgement.not_covered.is_empty() {
            out.push_str("not_covered: (empty — the contract claims more than it delivers)\n");
        } else {
            out.push_str("not_covered:\n");
            for item in &self.contract.judgement.not_covered {
                out.push_str(&format!("  - {item}\n"));
            }
        }
        out.push_str(&format!(
            "reviewed: {}\n",
            self.contract
                .judgement
                .reviewed
                .as_deref()
                .unwrap_or("<none>")
        ));
        out
    }

    /// The JSON report. The three targets are separate top-level keys.
    pub fn json(&self) -> String {
        let value = self.json_value();
        // `to_string_pretty` on a `serde_json::Value` built from sorted
        // structures is stable, so this is byte-reproducible.
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
    }

    /// The JSON report as a value, for callers that want to post-process it.
    pub fn json_value(&self) -> serde_json::Value {
        let mut rule_violations: Vec<&Finding> = self.findings().collect();
        // Sorted by gate, then subject, then fingerprint: three runs over the
        // same tree produce the same order regardless of how the gates
        // happened to interleave.
        rule_violations.sort_by(|a, b| {
            a.gate_id
                .as_str()
                .cmp(b.gate_id.as_str())
                .then_with(|| a.subject.name.cmp(&b.subject.name))
                .then_with(|| a.fingerprint.0.cmp(&b.fingerprint.0))
        });

        let mut gates: Vec<GateJson> = self
            .runs
            .iter()
            .map(|r| GateJson {
                gate_id: r.gate_id.to_string(),
                primitive: r.primitive.to_string(),
                outcome: outcome_name(&r.outcome),
                origin: r.outcome.origin().map(origin_json),
                finding_count: r.findings.len(),
            })
            .collect();
        gates.sort_by(|a, b| a.gate_id.cmp(&b.gate_id));

        serde_json::json!({
            // ---- target 1: acceptance -------------------------------------
            // "Is it tested." The verifier's exit code. No model.
            JSON_TARGET_KEYS[0]: {
                "verdict": self.verdict.to_string(),
                "exit_code": self.verdict.exit_code(),
                "gates": gates,
                "budget_bytes": self.contract.budget_observation_bytes,
                "baseline": self.contract.baseline_ref,
            },
            // ---- target 2: rule violations ---------------------------------
            // "Does this break a stated rule." Deterministic per-rule checks.
            // No model.
            JSON_TARGET_KEYS[1]: {
                "count": rule_violations.len(),
                "findings": rule_violations.iter().map(|f| finding_json(f)).collect::<Vec<_>>(),
            },
            // ---- target 3: residual judgement ------------------------------
            // "Is this work on-topic for the job." The only place a model
            // belongs. Empty in v1 by decision (PLAN.md 1.1), and present so a
            // consumer that expects the key finds it rather than assuming.
            JSON_TARGET_KEYS[2]: {
                "implemented": false,
                "findings": [],
                "not_covered": self.contract.judgement.not_covered,
                "reviewed": self.contract.judgement.reviewed,
            },
        })
    }

    /// The SARIF 2.1.0 report.
    pub fn sarif(&self) -> String {
        let value = self.sarif_value();
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
    }

    /// The SARIF report as a value.
    pub fn sarif_value(&self) -> serde_json::Value {
        let mut rules: Vec<SarifRule> = Vec::new();
        let mut results: Vec<serde_json::Value> = Vec::new();
        let mut seen: Vec<String> = Vec::new();

        let mut findings: Vec<&Finding> = self.findings().collect();
        findings.sort_by(|a, b| {
            a.gate_id
                .as_str()
                .cmp(b.gate_id.as_str())
                .then_with(|| a.subject.name.cmp(&b.subject.name))
                .then_with(|| a.fingerprint.0.cmp(&b.fingerprint.0))
        });

        for f in findings {
            let rule_id = f.gate_id.to_string();
            let rule_index = match seen.iter().position(|r| *r == rule_id) {
                Some(i) => i,
                None => {
                    seen.push(rule_id.clone());
                    rules.push(SarifRule {
                        id: rule_id.clone(),
                        // `ShortDescription.text` is required by SARIF.
                        short_description: SarifMessage {
                            text: format!("{} ({})", f.primitive, f.subject.kind.noun()),
                        },
                    });
                    rules.len() - 1
                }
            };
            results.push(result_json(f, &rule_id, rule_index));
        }

        // A clean run must still report its rules. A code-scanning UI with an
        // empty rule set shows nothing to attribute a future result to, so the
        // reader cannot tell "no rule fired" from "no rule exists".
        for run in self.runs {
            let id = run.gate_id.to_string();
            if seen.contains(&id) {
                continue;
            }
            seen.push(id.clone());
            rules.push(SarifRule {
                id,
                short_description: SarifMessage {
                    text: format!("{} ({})", run.primitive, outcome_name(&run.outcome)),
                },
            });
        }

        serde_json::json!({
            "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
            "version": SARIF_VERSION,
            "runs": [{
                "tool": {"driver": {
                    "name": TOOL_NAME,
                    "version": self.tool_version,
                    "informationUri": "https://github.com/rulebound",
                    "rules": rules,
                }},
                "results": results,
                // The gap, in the artefact, so a code-scanning consumer sees
                // what was not checked without opening the contract.
                "properties": {
                    "verdict": self.verdict.to_string(),
                    "notCovered": self.contract.judgement.not_covered,
                    "reviewed": self.contract.judgement.reviewed,
                    // The gates that audit the contract are not in the
                    // contract. Saying so in the artefact stops a reader
                    // wondering where they were configured.
                    "builtInGates": ["contract_not_loosened", "contract_review_stale"],
                },
            }],
        })
    }
}

fn outcome_name(o: &GateOutcome) -> &'static str {
    match o {
        GateOutcome::Pass { .. } => "pass",
        GateOutcome::Fail(_) => "fail",
        GateOutcome::Untrustworthy { .. } => "untrustworthy",
        GateOutcome::Skipped { .. } => "skipped",
    }
}

fn origin_json(origin: &Origin) -> serde_json::Value {
    match origin {
        Origin::Analyzed { primitive } => serde_json::json!({
            "kind": "analyzed",
            "primitive": primitive.as_str(),
        }),
        Origin::Delegated { tool, version } => serde_json::json!({
            "kind": "delegated",
            "tool": tool.as_str(),
            "version": version,
        }),
    }
}

fn finding_json(f: &Finding) -> serde_json::Value {
    serde_json::json!({
        "gateId": f.gate_id.to_string(),
        "primitive": f.primitive.to_string(),
        "severity": f.severity.to_string(),
        "subject": {"kind": f.subject.kind.noun(), "name": f.subject.name},
        "change": f.change(),
        "expected": side_json(&f.expected),
        "observed": side_json(&f.observed),
        "path": f.path.as_ref().map(|p| p.to_string()),
        "hunk": f.hunk.map(|h| serde_json::json!({"start": h.start, "end": h.end})),
        "message": f.message,
        "fingerprint": f.fingerprint.to_string(),
        "origin": origin_json(&f.origin),
    })
}

/// Absence is `null`, not the string `"absent"`. A consumer can therefore
/// branch on the pair, which is the whole reason `expected`/`observed` are
/// typed.
fn side_json(s: &Side) -> serde_json::Value {
    match s {
        Side::Absent => serde_json::Value::Null,
        Side::Value(v) => serde_json::Value::String(v.clone()),
    }
}

fn result_json(f: &Finding, rule_id: &str, rule_index: usize) -> serde_json::Value {
    let level = match f.severity {
        Severity::Error | Severity::Escalate => "error",
        Severity::Warn => "warning",
        Severity::Off => "none",
    };
    let mut location = serde_json::json!({});
    if let Some(path) = &f.path {
        location["artifactLocation"] = serde_json::json!({"uri": path.as_str()});
    }
    if let Some(h) = f.hunk {
        location["region"] = serde_json::json!({
            "startLine": h.start,
            // SARIF regions are end-exclusive; ours are inclusive.
            "endLine": h.end + 1,
        });
    }
    serde_json::json!({
        "ruleId": rule_id,
        "ruleIndex": rule_index,
        "level": level,
        "message": {"text": f.message},
        "locations": [location],
        // The key GitHub code scanning uses to recognise a finding it has
        // already reported, so a known finding stays known across runs.
        "partialFingerprints": {FINGERPRINT_KEY: f.fingerprint.to_string()},
        "properties": {
            "gateId": f.gate_id.to_string(),
            "subjectKind": f.subject.kind.noun(),
            "subject": f.subject.name,
            "change": f.change(),
            "expected": side_json(&f.expected),
            "observed": side_json(&f.observed),
            "origin": origin_json(&f.origin),
            "hunkInclusiveEnd": f.hunk.map(|h| h.end),
        },
    })
}

#[derive(Debug, Serialize)]
struct SarifRule {
    id: String,
    #[serde(rename = "shortDescription")]
    short_description: SarifMessage,
}

#[derive(Debug, Serialize)]
struct SarifMessage {
    text: String,
}

#[derive(Debug, Serialize)]
struct GateJson {
    #[serde(rename = "gateId")]
    gate_id: String,
    primitive: String,
    outcome: &'static str,
    /// `None` for a skip, which has no producer.
    origin: Option<serde_json::Value>,
    #[serde(rename = "findingCount")]
    finding_count: usize,
}
