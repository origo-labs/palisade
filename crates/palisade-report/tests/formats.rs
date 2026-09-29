//! What each report has to guarantee, and the two properties that make a gate
//! suite calibratable at all.

use palisade_contract::{Gate, GateId, JudgementSection, Primitive, Severity};
use palisade_orchestrate::{
    Finding, GateRun, Origin, Side, Subject, SubjectKind, UntrustworthyReason, Verdict,
};
use palisade_report::Report;

fn gate(name: &str, primitive: Primitive) -> Gate {
    let mut g = Gate::new(GateId::new(name).unwrap(), primitive);
    g.severity = Severity::Error;
    g
}

fn contract(gates: Vec<Gate>, not_covered: Vec<&str>) -> palisade_contract::Contract {
    palisade_contract::Contract {
        version: 1,
        gates,
        budget_observation_bytes: 131_072,
        baseline_ref: Some("HEAD".to_string()),
        judgement: JudgementSection {
            not_covered: not_covered.iter().map(|s| (*s).to_string()).collect(),
            reviewed: Some("2026-09-28".to_string()),
        },
        suppressions: Vec::new(),
        changes: Vec::new(),
    }
}

fn added_finding(name: &str, observed: &str) -> Finding {
    Finding::new(
        GateId::new("no_new_dependencies").unwrap(),
        Primitive::DependencySurfaceUnchanged,
        Severity::Error,
        Subject::new(SubjectKind::Dependency, name),
        Some("Cargo.toml".into()),
        None,
        Side::Absent,
        Side::value(observed),
        format!("production dependency `{name}` was added"),
        Origin::Analyzed {
            primitive: Primitive::DependencySurfaceUnchanged,
        },
    )
}

fn sample() -> (palisade_contract::Contract, Vec<GateRun>) {
    let contract = contract(
        vec![
            gate("no_new_dependencies", Primitive::DependencySurfaceUnchanged),
            gate("tests_not_deleted", Primitive::TestsNotDeleted),
            gate("checks_green", Primitive::ChecksGreen),
        ],
        vec!["whether the design is the right one"],
    );
    let runs = vec![
        GateRun::with_findings(
            GateId::new("no_new_dependencies").unwrap(),
            Primitive::DependencySurfaceUnchanged,
            vec![
                added_finding("tokio", "1"),
                added_finding("serde_json", "1"),
            ],
            Origin::Analyzed {
                primitive: Primitive::DependencySurfaceUnchanged,
            },
        ),
        GateRun::passed(
            GateId::new("tests_not_deleted").unwrap(),
            Primitive::TestsNotDeleted,
            Origin::Analyzed {
                primitive: Primitive::TestsNotDeleted,
            },
        ),
        GateRun::with_findings(
            GateId::new("checks_green").unwrap(),
            Primitive::ChecksGreen,
            vec![Finding::new(
                GateId::new("checks_green_clippy").unwrap(),
                Primitive::ChecksGreen,
                Severity::Error,
                Subject::new(SubjectKind::Contract, "cargo clippy"),
                None,
                None,
                Side::value("clean"),
                Side::value("error: unused import"),
                "`cargo clippy` failed",
                Origin::Delegated {
                    tool: palisade_orchestrate::ToolId::new("cargo"),
                    version: "1.98.0".to_string(),
                },
            )],
            Origin::Delegated {
                tool: palisade_orchestrate::ToolId::new("cargo"),
                version: "1.98.0".to_string(),
            },
        ),
    ];
    (contract, runs)
}

// ---- the three targets stay separate ----------------------------------------

#[test]
fn json_has_the_three_targets_as_separate_keys() {
    // EVIDENCE.md 5. Conflating "does this break a rule" with "should the
    // supervisor intervene" is what confounded every model comparison in the
    // predecessor programme, and separating them in the schema is what stops a
    // consumer recombining them by accident.
    let (c, runs) = sample();
    let v: serde_json::Value = serde_json::from_str(
        &Report {
            contract: &c,
            runs: &runs,
            verdict: Verdict::Block,
            tool_version: "0.1.0",
        }
        .json(),
    )
    .expect("valid json");
    for key in ["acceptance", "rule_violations", "residual_judgement"] {
        assert!(v.get(key).is_some(), "missing target `{key}`");
    }
    assert_eq!(v["acceptance"]["verdict"], "block");
    assert_eq!(v["rule_violations"]["count"], 3);
    // The judgement target exists, says it is not implemented, and is empty —
    // so a consumer does not have to infer v1's decision (PLAN.md 1.1) from an
    // absent key, and cannot mistake an empty list for "nothing to report".
    assert_eq!(v["residual_judgement"]["implemented"], false);
    assert!(
        v["residual_judgement"]["findings"]
            .as_array()
            .expect("array")
            .is_empty(),
        "v1 has no judgement tier, so there can be no judgement findings"
    );
    // The gap travels with it, so a consumer sees what is unmodelled.
    assert!(
        !v["residual_judgement"]["not_covered"]
            .as_array()
            .expect("array")
            .is_empty()
    );
}

#[test]
fn every_gate_carries_its_provenance() {
    // DoD item 3. A report that cannot say whose verdict it is cannot be
    // audited, and the whole point of `Origin` is that it survives to here.
    let (c, runs) = sample();
    let v: serde_json::Value = serde_json::from_str(
        &Report {
            contract: &c,
            runs: &runs,
            verdict: Verdict::Block,
            tool_version: "0.1.0",
        }
        .json(),
    )
    .expect("valid json");
    let gates = v["acceptance"]["gates"].as_array().expect("array");
    assert_eq!(gates.len(), 3);
    for g in gates {
        assert!(g["origin"].is_object(), "gate without provenance: {g}");
    }
    // A skipped gate has no producer, and says so rather than inventing one.
    let runs_with_skip = vec![GateRun::skipped(
        GateId::new("off_gate").unwrap(),
        Primitive::SecretAbsent,
    )];
    let v: serde_json::Value = serde_json::from_str(
        &Report {
            contract: &c,
            runs: &runs_with_skip,
            verdict: Verdict::Accept,
            tool_version: "0.1.0",
        }
        .json(),
    )
    .expect("valid json");
    assert!(v["acceptance"]["gates"][0]["origin"].is_null());
    assert_eq!(v["acceptance"]["gates"][0]["outcome"], "skipped");
}

// ---- the expected/observed contract reaches the output ---------------------

#[test]
fn absence_is_null_in_json_not_a_string() {
    let (c, runs) = sample();
    let v: serde_json::Value = serde_json::from_str(
        &Report {
            contract: &c,
            runs: &runs,
            verdict: Verdict::Block,
            tool_version: "0.1.0",
        }
        .json(),
    )
    .expect("valid json");
    // Findings are sorted by gate id, so pick the dependency one by subject
    // rather than by position.
    let all = v["rule_violations"]["findings"].as_array().expect("array");
    let f = all
        .iter()
        .find(|f| f["subject"]["kind"] == "dependency")
        .expect("a dependency finding");
    assert!(f["expected"].is_null(), "{f}");
    assert!(f["observed"].is_string(), "{f}");
    // The change kind is carried, and it is derived from the pair.
    assert_eq!(f["change"], "added");
}

#[test]
fn sarif_carries_the_fingerprint_and_the_pair() {
    let (c, runs) = sample();
    let v: serde_json::Value = serde_json::from_str(
        &Report {
            contract: &c,
            runs: &runs,
            verdict: Verdict::Block,
            tool_version: "0.1.0",
        }
        .sarif(),
    )
    .expect("valid sarif");
    assert_eq!(v["version"], "2.1.0");
    let results = v["runs"][0]["results"].as_array().expect("array");
    assert_eq!(results.len(), 3);
    for r in results {
        let fp = &r["partialFingerprints"]["palisadeFingerprint/v1"];
        assert!(fp.is_string(), "no fingerprint: {r}");
        assert_eq!(r["properties"]["change"], r["properties"]["change"]);
        // The pair is present as properties, because SARIF has no native
        // expected/observed and inventing a non-standard field would be worse.
        assert!(r["properties"].get("expected").is_some());
        assert!(r["properties"].get("observed").is_some());
    }
}

#[test]
fn sarif_rules_exist_even_when_nothing_fired() {
    // A code-scanning UI with an empty rule set cannot distinguish "no rule
    // fired" from "no rule exists".
    let c = contract(
        vec![gate("tests_not_deleted", Primitive::TestsNotDeleted)],
        vec![],
    );
    let runs = vec![GateRun::passed(
        GateId::new("tests_not_deleted").unwrap(),
        Primitive::TestsNotDeleted,
        Origin::Analyzed {
            primitive: Primitive::TestsNotDeleted,
        },
    )];
    let v: serde_json::Value = serde_json::from_str(
        &Report {
            contract: &c,
            runs: &runs,
            verdict: Verdict::Accept,
            tool_version: "0.1.0",
        }
        .sarif(),
    )
    .expect("valid sarif");
    let rules = v["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .expect("array");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["id"], "tests_not_deleted");
    assert!(
        v["runs"][0]["results"]
            .as_array()
            .expect("array")
            .is_empty()
    );
}

#[test]
fn sarif_regions_are_end_exclusive_as_the_spec_requires() {
    // Ours are inclusive; SARIF's `endLine` is exclusive. Getting this wrong
    // puts a highlight one line past the finding.
    let f = Finding::new(
        GateId::new("g").unwrap(),
        Primitive::SecretAbsent,
        Severity::Error,
        Subject::new(SubjectKind::Path, "src/lib.rs"),
        Some("src/lib.rs".into()),
        Some(palisade_orchestrate::HunkRef { start: 10, end: 12 }),
        Side::Absent,
        Side::value("key"),
        "m",
        Origin::Analyzed {
            primitive: Primitive::SecretAbsent,
        },
    );
    let c = contract(vec![gate("g", Primitive::SecretAbsent)], vec![]);
    let runs = vec![GateRun::with_findings(
        GateId::new("g").unwrap(),
        Primitive::SecretAbsent,
        vec![f],
        Origin::Analyzed {
            primitive: Primitive::SecretAbsent,
        },
    )];
    let v: serde_json::Value = serde_json::from_str(
        &Report {
            contract: &c,
            runs: &runs,
            verdict: Verdict::Block,
            tool_version: "0.1.0",
        }
        .sarif(),
    )
    .expect("valid sarif");
    let region = &v["runs"][0]["results"][0]["locations"][0]["region"];
    assert_eq!(region["startLine"], 10);
    assert_eq!(region["endLine"], 13);
}

// ---- determinism ------------------------------------------------------------

#[test]
fn two_runs_over_the_same_findings_are_byte_identical() {
    // A supervisor whose own output is nondeterministic cannot be calibrated,
    // and a CI baseline that churns is one nobody reads. So: no timestamps, no
    // wall-clock, no hash-map iteration order, everything sorted.
    let (c, runs) = sample();
    for format in ["json", "sarif", "human"] {
        let render = |r: &Report<'_>| match format {
            "json" => r.json(),
            "sarif" => r.sarif(),
            _ => r.human(),
        };
        let first = render(&Report {
            contract: &c,
            runs: &runs,
            verdict: Verdict::Block,
            tool_version: "0.1.0",
        });
        let second = render(&Report {
            contract: &c,
            runs: &runs,
            verdict: Verdict::Block,
            tool_version: "0.1.0",
        });
        assert_eq!(first, second, "{format} is not deterministic");
    }
}

#[test]
fn findings_are_sorted_so_interleaving_cannot_change_the_output() {
    // Two gates that each find several things: the order they finished in
    // must not reach the artefact.
    let c = contract(
        vec![
            gate("a_gate", Primitive::TestsNotDeleted),
            gate("b_gate", Primitive::SecretAbsent),
        ],
        vec![],
    );
    let make = |gate_name: &str, primitive: Primitive, names: &[&str]| {
        let findings = names
            .iter()
            .map(|n| {
                Finding::new(
                    GateId::new(gate_name).unwrap(),
                    primitive,
                    Severity::Error,
                    Subject::new(SubjectKind::Test, *n),
                    None,
                    None,
                    Side::Absent,
                    Side::value(*n),
                    format!("{n} added"),
                    Origin::Analyzed { primitive },
                )
            })
            .collect();
        GateRun::with_findings(
            GateId::new(gate_name).unwrap(),
            primitive,
            findings,
            Origin::Analyzed { primitive },
        )
    };
    let forwards = vec![
        make("a_gate", Primitive::TestsNotDeleted, &["t1", "t2"]),
        make("b_gate", Primitive::SecretAbsent, &["s1", "s2"]),
    ];
    let backwards = vec![
        make("b_gate", Primitive::SecretAbsent, &["s1", "s2"]),
        make("a_gate", Primitive::TestsNotDeleted, &["t1", "t2"]),
    ];
    let render = |runs: &[GateRun]| {
        Report {
            contract: &c,
            runs,
            verdict: Verdict::Block,
            tool_version: "0.1.0",
        }
        .json()
    };
    assert_eq!(render(&forwards), render(&backwards));
}

// ---- the gap is in the artefact ---------------------------------------------

#[test]
fn the_human_report_always_states_what_was_not_covered() {
    // PRD 8. A contract that checks dependencies and green tests will pass a
    // change that is compliant and strategically wrong. That lives in
    // `judgement.not_covered`, and the report has to say so every time.
    let (c, runs) = sample();
    let text = Report {
        contract: &c,
        runs: &runs,
        verdict: Verdict::Block,
        tool_version: "0.1.0",
    }
    .human();
    assert!(
        text.contains("whether the design is the right one"),
        "{text}"
    );
    assert!(text.contains("reviewed: 2026-09-28"), "{text}");
}

#[test]
fn an_empty_not_covered_is_called_out_rather_than_omitted() {
    let c = contract(
        vec![gate("g", Primitive::SecretAbsent)],
        vec![], // empty
    );
    let text = Report {
        contract: &c,
        runs: &[],
        verdict: Verdict::Accept,
        tool_version: "0.1.0",
    }
    .human();
    assert!(
        text.contains("claims more than it delivers"),
        "an empty gap list must be visible: {text}"
    );
}

#[test]
fn the_gap_is_in_the_sarif_too() {
    // A consumer reading only the code-scanning artefact should not have to
    // open the contract to learn what was not checked.
    let (c, runs) = sample();
    let v: serde_json::Value = serde_json::from_str(
        &Report {
            contract: &c,
            runs: &runs,
            verdict: Verdict::Block,
            tool_version: "0.1.0",
        }
        .sarif(),
    )
    .expect("valid sarif");
    let not_covered = v["runs"][0]["properties"]["notCovered"]
        .as_array()
        .expect("array");
    assert_eq!(not_covered.len(), 1);
    assert_eq!(v["runs"][0]["properties"]["verdict"], "block");
}

#[test]
fn an_untrustworthy_gate_is_visible_in_every_format() {
    let c = contract(
        vec![gate("api_unchanged", Primitive::PublicApiUnchanged)],
        vec![],
    );
    let runs = vec![GateRun::untrustworthy(
        GateId::new("api_unchanged").unwrap(),
        Primitive::PublicApiUnchanged,
        UntrustworthyReason::Unimplemented {
            primitive: Primitive::PublicApiUnchanged,
        },
        Origin::Analyzed {
            primitive: Primitive::PublicApiUnchanged,
        },
    )];
    let report = Report {
        contract: &c,
        runs: &runs,
        verdict: Verdict::Error,
        tool_version: "0.1.0",
    };
    assert!(report.human().contains("ERROR"), "{}", report.human());
    let v: serde_json::Value = serde_json::from_str(&report.json()).expect("valid json");
    assert_eq!(v["acceptance"]["verdict"], "error");
    assert_eq!(v["acceptance"]["exit_code"], 2);
    assert_eq!(v["acceptance"]["gates"][0]["outcome"], "untrustworthy");
}
