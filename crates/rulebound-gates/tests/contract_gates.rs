//! M4: the gates about gates.
//!
//! The Goodhart defence. A worker optimising against the gate suite rather
//! than against intent will not remove a requirement — it will set
//! `severity = "off"`, add a name to `allow`, delete a gate, or quietly drop
//! an entry from `not_covered`. All of those are diff-visible, which is the
//! only reason they are checkable at all.
//!
//! These are the tests where a false positive costs the most, because a
//! `contract_not_loosened` finding that fires on a legitimate contract edit
//! teaches a team to disable the gate that audits the contract.

use rulebound_contract::{Gate, GateId, Primitive, Severity};
use rulebound_gates::{GateContext, GateResult, registry};

const NOW: i64 = 1_790_000_000; // 2026-09-28

/// The test inventory a gate sees. `None` here: these fixtures are about the
/// contract, not about whether a test runs.
fn inventory() -> Option<&'static rulebound_exec::test_inventory::TestInventory> {
    None
}

/// A contract with one gate, as the head or the base of a comparison.
fn contract(gate_severity: &str, extra: &str, judgement: &str) -> String {
    format!(
        "version = 1
[[gates]]
id = \"no_unsafe_added\"
check = \"unsafe_surface_unchanged\"
severity = \"{gate_severity}\"
{extra}
{judgement}"
    )
}

const PLAIN_JUDGEMENT: &str =
    "[judgement]\nreviewed = \"2026-09-20\"\nnot_covered = [\"design quality\"]";

fn two_tree(base: &str, head: &str) -> rulebound_observe::Observation {
    let raw = rulebound_git::ObservationInputs {
        base: Some("base".to_string()),
        status: vec![],
        dirty: true,
        unstaged: String::new(),
        staged: String::new(),
        files: vec![rulebound_git::RawFile {
            path: "rulebound.toml".to_string(),
            orig_path: None,
            base: Some(base.to_string()),
            head: Some(head.to_string()),
            truncated: false,
        }],
    };
    rulebound_observe::Observation::capture(&raw, rulebound_observe::Budget::DEFAULT, true)
}

fn gate(primitive: Primitive) -> Gate {
    let mut g = Gate::new(GateId::new("contract_hygiene").unwrap(), primitive);
    g.severity = Severity::Error;
    g
}

fn run_not_loosened(base: &str, head: &str) -> GateResult {
    let g = gate(Primitive::ContractNotLoosened);
    let obs = two_tree(base, head);
    registry::dispatch(
        g.primitive,
        &GateContext {
            gate: &g,
            observation: &obs,
            test_inventory: inventory(),
            now_unix: NOW,
        },
    )
}

fn findings(r: &GateResult) -> &[rulebound_orchestrate::Finding] {
    match r {
        GateResult::Findings(f) => f,
        other => panic!("expected findings, got {other:?}"),
    }
}

fn assert_clean(r: &GateResult) {
    assert_eq!(*r, GateResult::Clean, "expected clean, got {r:?}");
}

// ---- contract_not_loosened: every way to weaken a contract -----------------

#[test]
fn downgrading_a_severity_is_a_loosening() {
    let base = contract("error", "", PLAIN_JUDGEMENT);
    let head = contract("warn", "", PLAIN_JUDGEMENT);
    let r = run_not_loosened(&base, &head);
    let f = findings(&r);
    assert!(f.iter().any(|x| x.message.contains("downgraded")), "{f:?}");
}

#[test]
fn switching_a_gate_off_is_a_loosening() {
    let base = contract("error", "", PLAIN_JUDGEMENT);
    let head = contract("off", "", PLAIN_JUDGEMENT);
    let r = run_not_loosened(&base, &head);
    assert!(!findings(&r).is_empty());
}

#[test]
fn deleting_a_gate_is_a_loosening() {
    let base = contract("error", "", PLAIN_JUDGEMENT);
    let head = "version = 1\n[judgement]\nreviewed = \"2026-09-20\"\n";
    let r = run_not_loosened(&base, head);
    let f = findings(&r);
    assert!(f.iter().any(|x| x.message.contains("deleted")), "{f:?}");
}

#[test]
fn adding_a_suppression_is_a_loosening() {
    let base = contract("error", "", PLAIN_JUDGEMENT);
    let head = contract(
        "error",
        "",
        &format!(
            "{PLAIN_JUDGEMENT}\n[[suppressions]]\ngate = \"no_unsafe_added\"\npath = \"src/lib.rs\"\nreason = \"temporary\""
        ),
    );
    let r = run_not_loosened(&base, &head);
    let f = findings(&r);
    assert!(f.iter().any(|x| x.message.contains("suppression")), "{f:?}");
}

#[test]
fn adding_an_allow_entry_is_a_loosening() {
    let base = contract("error", "", PLAIN_JUDGEMENT);
    let head = contract("error", "allow = [\"src/generated.rs\"]", PLAIN_JUDGEMENT);
    let r = run_not_loosened(&base, &head);
    let f = findings(&r);
    assert!(f.iter().any(|x| x.message.contains("exemption")), "{f:?}");
}

#[test]
fn dropping_a_not_covered_entry_is_a_loosening() {
    // The subtlest one, and the most corrosive: nothing about the *code*
    // changed, the contract just stopped admitting to a gap.
    let base = contract(
        "error",
        "",
        "[judgement]\nreviewed = \"2026-09-20\"\nnot_covered = [\"design quality\", \"error messages\"]",
    );
    let head = contract(
        "error",
        "",
        "[judgement]\nreviewed = \"2026-09-20\"\nnot_covered = [\"design quality\"]",
    );
    let r = run_not_loosened(&base, &head);
    let f = findings(&r);
    assert!(
        f.iter().any(|x| x.message.contains("no longer admits")),
        "{f:?}"
    );
}

#[test]
fn swapping_what_a_gate_checks_is_a_loosening() {
    let base = contract("error", "", PLAIN_JUDGEMENT);
    let head = "version = 1\n[[gates]]\nid = \"no_unsafe_added\"\ncheck = \"secret_absent\"\nseverity = \"error\"\n"
        .to_string()
        + PLAIN_JUDGEMENT
        + "\n";
    let r = run_not_loosened(&base, &head);
    let f = findings(&r);
    assert!(
        f.iter().any(|x| x.message.contains("what it checks")),
        "{f:?}"
    );
}

#[test]
fn a_recorded_reason_downgrades_rather_than_silences() {
    // Silencing entirely would make a justified loosening indistinguishable
    // from a change nobody noticed. The change still happened, and a reader
    // should see that it was deliberate.
    let base = contract("error", "", PLAIN_JUDGEMENT);
    let head = format!(
        "{}\n[[changes]]\ngate = \"no_unsafe_added\"\nreason = \"vendor audit approved the allowance in PR 412\"\n",
        contract("warn", "", PLAIN_JUDGEMENT)
    );
    let r = run_not_loosened(&base, &head);
    let f = findings(&r);
    let loosened = f
        .iter()
        .find(|x| x.message.contains("downgraded"))
        .expect("the loosening is still reported");
    assert_eq!(loosened.severity, Severity::Warn);
    assert!(loosened.message.contains("PR 412"), "{}", loosened.message);
}

// ---- the promotion guard ----------------------------------------------------

#[test]
fn promoting_a_gate_to_error_without_a_calibration_is_reported() {
    let base = contract("warn", "", PLAIN_JUDGEMENT);
    let head = contract("error", "", PLAIN_JUDGEMENT);
    let r = run_not_loosened(&base, &head);
    let f = findings(&r);
    let promotion = f
        .iter()
        .find(|x| x.message.contains("promoted"))
        .unwrap_or_else(|| panic!("expected a promotion finding, got {f:?}"));
    // Blocking. This was pinned to `warn` on the reasoning that the gate is
    // usually advisory, but the gate is no longer declarable, so it never is —
    // and a promotion to `error` without a measurement is exactly what PRD 5
    // forbids. It has to be able to block.
    assert_eq!(promotion.severity, Severity::Error);
    assert!(
        promotion.message.contains("calibration"),
        "{}",
        promotion.message
    );
}

#[test]
fn promoting_a_gate_to_error_with_a_calibration_is_fine() {
    let base = contract("warn", "", PLAIN_JUDGEMENT);
    let head = contract(
        "error",
        "calibration = \"curve:2026-11-rs-corpus-n420\"",
        PLAIN_JUDGEMENT,
    );
    let r = run_not_loosened(&base, &head);
    assert_clean(&r);
}

#[test]
fn writing_a_new_contract_at_error_is_not_a_promotion() {
    // The author making a claim is not a promotion. A static rule here would
    // reject every contract until M5 exists, and would be switched off first.
    let head = contract("error", "", PLAIN_JUDGEMENT);
    let base = "version = 1\n[judgement]\nreviewed = \"2026-09-20\"\n";
    let r = run_not_loosened(base, &head);
    assert_clean(&r);
}

// ---- the must-not-fires, which matter more here than anywhere -------------

#[test]
fn an_ordinary_contract_edit_is_not_a_loosening() {
    // Adding a gate, tightening a severity, adding a covered path, updating
    // the review date. All legitimate, all strengthening.
    let base = contract("error", "", PLAIN_JUDGEMENT);
    let head = "version = 1
[[gates]]
id = \"no_unsafe_added\"
check = \"unsafe_surface_unchanged\"
severity = \"error\"
[[gates]]
id = \"tests_not_deleted\"
check = \"tests_not_deleted\"
severity = \"error\"
[judgement]
reviewed = \"2026-09-28\"
not_covered = [\"design quality\"]
";
    let r = run_not_loosened(&base, head);
    assert_clean(&r);
}

#[test]
fn covering_more_frozen_paths_is_not_a_loosening() {
    // Fewer frozen paths *is* a loosening; more is not. The direction is the
    // whole gate, so the gate has to be one that can take paths.
    let contract_with_paths = |paths: &str| {
        format!(
            "version = 1
[[gates]]
id = \"fixtures_frozen\"
check = \"paths_unchanged\"
severity = \"error\"
paths = [{paths}]
{PLAIN_JUDGEMENT}"
        )
    };
    let r = run_not_loosened(
        &contract_with_paths("\"a\""),
        &contract_with_paths("\"a\", \"b\""),
    );
    assert_clean(&r);
}

#[test]
fn covering_fewer_frozen_paths_is_a_loosening() {
    let contract_with_paths = |paths: &str| {
        format!(
            "version = 1
[[gates]]
id = \"fixtures_frozen\"
check = \"paths_unchanged\"
severity = \"error\"
paths = [{paths}]
{PLAIN_JUDGEMENT}"
        )
    };
    let r = run_not_loosened(
        &contract_with_paths("\"a\", \"b\""),
        &contract_with_paths("\"a\""),
    );
    let f = findings(&r);
    assert!(
        f.iter().any(|x| x.message.contains("stopped covering")),
        "{f:?}"
    );
}

#[test]
fn an_unchanged_contract_is_clean() {
    let c = contract("error", "", PLAIN_JUDGEMENT);
    let r = run_not_loosened(&c, &c);
    assert_clean(&r);
}

#[test]
fn a_contract_absent_from_the_observation_is_clean() {
    // Absence from the diff means unchanged, not unexamined.
    let raw = rulebound_git::ObservationInputs {
        base: Some("base".to_string()),
        status: vec![],
        dirty: true,
        unstaged: String::new(),
        staged: String::new(),
        files: vec![rulebound_git::RawFile {
            path: "src/lib.rs".to_string(),
            orig_path: None,
            base: Some("a".to_string()),
            head: Some("b".to_string()),
            truncated: false,
        }],
    };
    let obs =
        rulebound_observe::Observation::capture(&raw, rulebound_observe::Budget::DEFAULT, true);
    let g = gate(Primitive::ContractNotLoosened);
    assert_clean(&registry::dispatch(
        g.primitive,
        &GateContext {
            gate: &g,
            observation: &obs,
            test_inventory: inventory(),
            now_unix: NOW,
        },
    ));
}

#[test]
fn an_invalid_contract_says_which_side_is_broken() {
    // "The contract is broken" and "the base commit's contract is broken" call
    // for different fixes, so the message names the side.
    let base = contract("error", "", PLAIN_JUDGEMENT);
    let head = "this is not a contract";
    let r = run_not_loosened(&base, head);
    match r {
        GateResult::Untrustworthy(reason) => {
            let detail = reason.detail();
            assert!(detail.contains("invalid"), "{detail}");
        }
        other => panic!("expected untrustworthy, got {other:?}"),
    }
}

#[test]
fn deleting_the_contract_entirely_is_the_largest_loosening() {
    let base = contract("error", "", PLAIN_JUDGEMENT);
    let raw = rulebound_git::ObservationInputs {
        base: Some("base".to_string()),
        status: vec![],
        dirty: true,
        unstaged: String::new(),
        staged: String::new(),
        files: vec![rulebound_git::RawFile {
            path: "rulebound.toml".to_string(),
            orig_path: None,
            base: Some(base),
            head: None,
            truncated: false,
        }],
    };
    let obs =
        rulebound_observe::Observation::capture(&raw, rulebound_observe::Budget::DEFAULT, true);
    let g = gate(Primitive::ContractNotLoosened);
    let r = registry::dispatch(
        g.primitive,
        &GateContext {
            gate: &g,
            observation: &obs,
            test_inventory: inventory(),
            now_unix: NOW,
        },
    );
    let f = findings(&r);
    assert!(f[0].message.contains("deleted"), "{f:?}");
}

// ---- contract_review_stale --------------------------------------------------

fn stale_gate() -> Gate {
    gate(Primitive::ContractReviewStale)
}

fn with_reviewed(reviewed: &str) -> GateResult {
    let head =
        format!("version = 1\n[judgement]\nreviewed = \"{reviewed}\"\nnot_covered = [\"x\"]\n");
    let g = stale_gate();
    let obs = two_tree(&head, &head);
    registry::dispatch(
        g.primitive,
        &GateContext {
            gate: &g,
            observation: &obs,
            test_inventory: inventory(),
            now_unix: NOW,
        },
    )
}

#[test]
fn a_fresh_review_is_clean() {
    assert_clean(&with_reviewed("2026-09-20"));
}

#[test]
fn a_stale_review_is_a_finding() {
    // NOW is 2026-09-28, so 2025-01-01 is well beyond 180 days.
    let r = with_reviewed("2025-01-01");
    let f = findings(&r);
    assert!(f[0].message.contains("last reviewed"), "{}", f[0].message);
}

#[test]
fn an_unparseable_review_date_is_a_finding_not_a_pass() {
    // A gate that only checked the interval would let `reviewed = "soon"` pass
    // forever, which is the failure this rule exists to prevent.
    for bad in ["soon", "2026-13", "not-a-date", ""] {
        let r = with_reviewed(bad);
        let f = findings(&r);
        assert!(
            f[0].message.contains("not a YYYY-MM-DD"),
            "`{bad}` should be reported, got {:?}",
            f[0].message
        );
    }
}

#[test]
fn a_missing_review_date_never_reaches_this_gate() {
    // A division of labour worth stating: a *missing* date is rejected when the
    // contract is loaded, so the whole run is `error` and no gate runs. This
    // gate only has to catch the two cases a load-time check cannot: a date
    // that is too old, and a date that is present but not a date.
    let head = "version = 1\n[judgement]\nnot_covered = [\"x\"]\n";
    assert!(
        rulebound_contract::parse::parse_contract(head).is_err(),
        "a contract with no review date must not load"
    );
}

// ---- the gates that audit the contract cannot themselves be audited away ---

#[test]
fn the_contract_hygiene_gates_cannot_be_declared() {
    // The flaw this milestone found in itself. These two gates were originally
    // declared in the contract like any other, and a worker that deleted the
    // declaration silenced the gate that would have noticed — a gate that can
    // be switched off by the thing it guards is not a gate, and the Goodhart
    // attack aimed at the defence is the one that matters.
    for check in ["contract_not_loosened", "contract_review_stale"] {
        let src = format!(
            "version = 1\n[[gates]]\nid = \"hygiene\"\ncheck = \"{check}\"\n\
             severity = \"off\"\n[judgement]\nreviewed = \"2026-09-28\"\n"
        );
        let err = rulebound_contract::parse::parse_contract(&src)
            .unwrap_err()
            .to_string();
        assert!(err.contains("always on"), "{check}: {err}");
        // And not merely ignored — an error, because a contract that thinks it
        // can turn it off has been told something it needed to know.
        assert!(err.contains("property of the supervisor"), "{check}: {err}");
    }
}
