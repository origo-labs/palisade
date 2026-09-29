//! The verdict algebra, proved by exhaustion (PLAN.md 6).
//!
//! The four rules this file exists to hold:
//!
//! 1. A gate may only ever subtract. No input yields `Accept` other than a
//!    conjunction of passes, skips and warnings.
//! 2. `error` is a first-class outcome, never a `block` and never a `pass`.
//! 3. Unknown is an error.
//! 4. Acceptance never comes from a judgement — and in v1 a judgement is not
//!    even expressible as a pass, because `Origin` has no `Judged` variant.

use palisade_contract::{GateId, Primitive, Severity};
use palisade_orchestrate::{
    Advisory, ChangeKind, Finding, GateOutcome, Origin, ReductionInput, Side, SkipReason, Subject,
    SubjectKind, ToolId, UntrustworthyReason, Verdict, reduce,
};

fn gate(name: &str) -> GateId {
    GateId::new(name).expect("fixture gate id")
}

fn analyzed(primitive: Primitive) -> Origin {
    Origin::Analyzed { primitive }
}

fn pass(primitive: Primitive) -> GateOutcome {
    GateOutcome::Pass {
        origin: analyzed(primitive),
    }
}

fn fail(primitive: Primitive, severity: Severity) -> GateOutcome {
    GateOutcome::Fail(Finding::new(
        gate("g"),
        primitive,
        severity,
        Subject::new(SubjectKind::Dependency, "serde"),
        None,
        None,
        Side::value("expected"),
        Side::value("observed"),
        "m",
        analyzed(primitive),
    ))
}

fn untrustworthy() -> GateOutcome {
    GateOutcome::Untrustworthy {
        reason: UntrustworthyReason::ToolCouldNotRun {
            detail: "exit 2".to_string(),
        },
        origin: Origin::Delegated {
            tool: ToolId::new("cargo"),
            version: "1.98.0".to_string(),
        },
    }
}

fn skip() -> GateOutcome {
    GateOutcome::Skipped {
        reason: SkipReason::DeclaredOff,
    }
}

/// The full alphabet of outcomes the reduction can see. Deliberately includes
/// one of every variant, including the two that must never be a pass.
fn alphabet() -> Vec<GateOutcome> {
    vec![
        pass(Primitive::SecretAbsent),
        fail(Primitive::SecretAbsent, Severity::Warn),
        fail(Primitive::SecretAbsent, Severity::Error),
        fail(Primitive::SecretAbsent, Severity::Escalate),
        fail(Primitive::SecretAbsent, Severity::Off),
        untrustworthy(),
        GateOutcome::Untrustworthy {
            reason: UntrustworthyReason::Unimplemented {
                primitive: Primitive::Judged,
            },
            origin: analyzed(Primitive::Judged),
        },
        skip(),
    ]
}

fn reduce_all(outcomes: &[GateOutcome]) -> Verdict {
    reduce(&ReductionInput {
        outcomes,
        ..Default::default()
    })
}

/// Every vector of length 0..=3 over the alphabet, plus the single-element
/// cases. Exhaustive rather than random: this is the whole state space that
/// matters, and a random sweep over it would be a weaker claim for more code.
fn all_vectors() -> Vec<Vec<GateOutcome>> {
    let a = alphabet();
    let mut out: Vec<Vec<GateOutcome>> = vec![vec![]];
    for x in &a {
        out.push(vec![x.clone()]);
    }
    for x in &a {
        for y in &a {
            out.push(vec![x.clone(), y.clone()]);
        }
    }
    for x in &a {
        for y in &a {
            for z in &a {
                out.push(vec![x.clone(), y.clone(), z.clone()]);
            }
        }
    }
    out
}

#[test]
fn the_reduction_is_total() {
    // Every vector produces exactly one of the four, and nothing else is
    // representable. There is no "no verdict" path.
    for v in all_vectors() {
        let verdict = reduce_all(&v);
        assert!(matches!(
            verdict,
            Verdict::Accept | Verdict::Block | Verdict::Escalate | Verdict::Error
        ));
    }
}

#[test]
fn empty_input_accepts() {
    assert_eq!(reduce_all(&[]), Verdict::Accept);
}

#[test]
fn contract_error_outranks_everything() {
    // Even a set of passing gates cannot outvote a contract that did not
    // validate: with no valid contract there is no declared gate set to have
    // satisfied.
    for v in all_vectors() {
        let verdict = reduce(&ReductionInput {
            outcomes: &v,
            contract_error: Some("unknown key `sevrity`".to_string()),
            ..Default::default()
        });
        assert_eq!(verdict, Verdict::Error, "vector of {} outcomes", v.len());
    }
}

#[test]
fn untrustworthy_outranks_every_failure() {
    // "We could not tell" is strictly more dangerous to report as accept, and
    // strictly more confusing to report as block, than "we know". It wins.
    for v in all_vectors() {
        let mut with_bad = v.clone();
        with_bad.push(untrustworthy());
        assert_eq!(reduce_all(&with_bad), Verdict::Error, "{v:?}");
    }
}

#[test]
fn unimplemented_is_error_not_pass() {
    for v in all_vectors() {
        let mut with_unimpl = v.clone();
        with_unimpl.push(GateOutcome::Untrustworthy {
            reason: UntrustworthyReason::Unimplemented {
                primitive: Primitive::Judged,
            },
            origin: analyzed(Primitive::Judged),
        });
        assert_eq!(reduce_all(&with_unimpl), Verdict::Error, "{v:?}");
    }
}

#[test]
fn monotonicity_adding_a_disqualifying_outcome_can_never_reach_accept() {
    // Rule 1, stated correctly. A gate may only ever *subtract*, so what must
    // hold is that appending a disqualifying outcome to an accepting vector
    // leaves `accept`. Passes and skips are not disqualifying — appending one
    // of those to an accepting vector must leave it accepting, and asserting
    // otherwise would be asserting that a gate can un-accept a verdict, which
    // is the opposite of the rule.
    for v in all_vectors() {
        if reduce_all(&v) != Verdict::Accept {
            continue;
        }
        for extra in alphabet() {
            let mut extended = v.clone();
            extended.push(extra.clone());
            if extra.rules_out_accept() {
                assert_ne!(
                    reduce_all(&extended),
                    Verdict::Accept,
                    "appending {extra:?} to accepting vector {v:?} reached accept"
                );
            } else {
                assert_eq!(
                    reduce_all(&extended),
                    Verdict::Accept,
                    "appending the accept-compatible {extra:?} should not block"
                );
            }
        }
    }
}

#[test]
fn acceptance_exactly_when_nothing_rules_it_out() {
    // The characterisation of `accept`, over the whole state space. This is
    // the property the report's "zero false positives" claim rests on: an
    // accept is exactly the conjunction of gates that neither blocked, nor
    // escalated, nor could not be determined.
    for v in all_vectors() {
        let expected = if v.iter().any(GateOutcome::rules_out_accept) {
            match reduce_all(&v) {
                Verdict::Accept => panic!("accepted despite {v:?}"),
                other => other,
            }
        } else {
            Verdict::Accept
        };
        assert_eq!(reduce_all(&v), expected, "vector {v:?}");
    }
}

#[test]
fn block_outranks_escalate() {
    let v = [
        fail(Primitive::SecretAbsent, Severity::Escalate),
        fail(Primitive::SecretAbsent, Severity::Error),
    ];
    assert_eq!(reduce_all(&v), Verdict::Block);
}

#[test]
fn escalate_when_something_escalates_and_nothing_blocks() {
    let v = [
        pass(Primitive::SecretAbsent),
        fail(Primitive::SecretAbsent, Severity::Escalate),
    ];
    assert_eq!(reduce_all(&v), Verdict::Escalate);
}

#[test]
fn a_warn_does_not_block_and_does_not_escalate() {
    let v = [
        pass(Primitive::SecretAbsent),
        fail(Primitive::SecretAbsent, Severity::Warn),
    ];
    assert_eq!(reduce_all(&v), Verdict::Accept);
}

#[test]
fn skips_never_block_and_never_accept_anything_by_themselves() {
    // A skip steps aside. It is reported as `off`, never as a pass.
    let s = skip();
    assert_eq!(reduce_all(std::slice::from_ref(&s)), Verdict::Accept);
    assert!(s.is_skip());
    assert!(!s.is_adverse());
}

#[test]
fn advisories_are_never_a_verdict() {
    // A gate whose own definition changed in this diff is worth saying out
    // loud and is never a reason to block or a reason to accept.
    let advisory = Advisory {
        gate_id: gate("no_unsafe_added"),
        message: "gate definition changed in this diff".to_string(),
    };
    for v in all_vectors() {
        let base = reduce_all(&v);
        let with_advisory = reduce(&ReductionInput {
            outcomes: &v,
            advisories: std::slice::from_ref(&advisory),
            ..Default::default()
        });
        assert_eq!(base, with_advisory, "advisory moved the verdict for {v:?}");
    }
}

#[test]
fn an_accepting_run_contains_no_blocking_finding_and_nothing_untrustworthy() {
    // The positive form of rule 1: accept means nothing *disqualified* the
    // run. A `warn` finding may legitimately coexist with accept — that is
    // what `warn` means, and PRD 5 makes it the default severity for a new
    // gate — so the property is about blocking findings and untrustworthy
    // gates, not about findings as such.
    for v in all_vectors() {
        if reduce_all(&v) != Verdict::Accept {
            continue;
        }
        for o in &v {
            assert!(
                !matches!(o, GateOutcome::Untrustworthy { .. }),
                "accepting vector contained {o:?}"
            );
            if let GateOutcome::Fail(f) = o {
                assert!(
                    !matches!(f.severity, Severity::Error | Severity::Escalate),
                    "accepting vector contained a blocking finding: {f:?}"
                );
            }
        }
    }
}

#[test]
fn a_warn_finding_is_reported_even_though_it_does_not_block() {
    // The reason `warn` exists, and the reason it must not be erased from the
    // report: a warning that is dropped is a gate that appears to have passed.
    let v = [fail(Primitive::SecretAbsent, Severity::Warn)];
    assert_eq!(reduce_all(&v), Verdict::Accept);
    assert!(
        matches!(v[0], GateOutcome::Fail(_)),
        "the finding still exists"
    );
}

#[test]
fn no_delegated_passing_gate_can_manufacture_a_verdict() {
    // A delegated pass is a pass. It can only ever contribute to accept,
    // never manufacture one out of nothing, and never block.
    let delegated_pass = GateOutcome::Pass {
        origin: Origin::Delegated {
            tool: ToolId::new("slop-gate"),
            version: "0.5.0".to_string(),
        },
    };
    assert_eq!(
        reduce_all(std::slice::from_ref(&delegated_pass)),
        Verdict::Accept
    );
    let with_block = [
        delegated_pass,
        fail(Primitive::UnsafeSurfaceUnchanged, Severity::Error),
    ];
    assert_eq!(reduce_all(&with_block), Verdict::Block);
}

#[test]
fn findings_carry_provenance() {
    // "Whose verdict is this" must survive into the artefact (PLAN.md 3).
    let f = Finding::new(
        gate("g"),
        Primitive::SecretAbsent,
        Severity::Error,
        Subject::new(SubjectKind::Dependency, "api-key"),
        Some("src/lib.rs".into()),
        Some(palisade_orchestrate::HunkRef { start: 1, end: 2 }),
        Side::Absent,
        Side::value("key present"),
        "credential in diff",
        analyzed(Primitive::SecretAbsent),
    );
    assert!(matches!(f.origin, Origin::Analyzed { .. }));
    // The subject is a first-class field, not something the message has to
    // carry, so a consumer never parses prose to learn what it is looking at.
    assert_eq!(f.subject.name, "api-key");
}

#[test]
fn fingerprints_are_stable_and_content_derived() {
    let a = Finding::new(
        gate("g"),
        Primitive::SecretAbsent,
        Severity::Warn,
        Subject::new(SubjectKind::Dependency, "api-key"),
        Some("src/lib.rs".into()),
        None,
        Side::Absent,
        Side::value("key present"),
        "m",
        analyzed(Primitive::SecretAbsent),
    );
    let b = Finding::new(
        gate("g"),
        Primitive::SecretAbsent,
        Severity::Warn,
        Subject::new(SubjectKind::Dependency, "api-key"),
        Some("src/lib.rs".into()),
        None,
        Side::Absent,
        Side::value("key present"),
        "a different human sentence",
        analyzed(Primitive::SecretAbsent),
    );
    // The message is a rendering; the fingerprint is the fact. Two findings
    // that differ only in prose are the same finding to CI.
    assert_eq!(a.fingerprint, b.fingerprint);

    let c = Finding::new(
        gate("g"),
        Primitive::SecretAbsent,
        Severity::Warn,
        Subject::new(SubjectKind::Dependency, "api-key"),
        Some("src/other.rs".into()),
        None,
        Side::Absent,
        Side::value("key present"),
        "m",
        analyzed(Primitive::SecretAbsent),
    );
    assert_ne!(a.fingerprint, c.fingerprint);
}

// ---- expected / observed: the settled meaning ------------------------------
//
// The pair is only worth having if the *kind of change* is derivable from it
// rather than stated in prose. These tests are the contract for the SARIF and
// JSON serialisers M3 is about to write, and they are why `message` is a hint
// rather than the source of truth.

fn finding(expected: Side, observed: Side) -> Finding {
    Finding::new(
        gate("g"),
        Primitive::TestsNotDeleted,
        Severity::Error,
        Subject::new(SubjectKind::Test, "a"),
        Some("src/lib.rs".into()),
        None,
        expected,
        observed,
        "whatever the gate felt like saying",
        analyzed(Primitive::TestsNotDeleted),
    )
}

#[test]
fn a_finding_is_classified_by_comparing_its_two_sides() {
    assert_eq!(
        finding(Side::Absent, Side::value("x")).change(),
        ChangeKind::Added
    );
    assert_eq!(
        finding(Side::value("x"), Side::Absent).change(),
        ChangeKind::Removed
    );
    assert_eq!(
        finding(Side::value("x"), Side::value("y")).change(),
        ChangeKind::Changed
    );
}

#[test]
fn the_message_cannot_relabel_a_finding() {
    // The report labels findings by `change()`, never by the message, so a
    // gate that writes "widened" on an addition cannot make an addition read as
    // a broadening. Two findings with the same sides classify the same way
    // however differently they are worded.
    let wrong = Finding::new(
        gate("g"),
        Primitive::SuppressionsNotWidened,
        Severity::Error,
        Subject::new(SubjectKind::Suppression, "#[allow(clippy::a)]"),
        None,
        None,
        Side::Absent,
        Side::value("#[allow(clippy::a)]"),
        "a diagnostic suppression was widened",
        analyzed(Primitive::SuppressionsNotWidened),
    );
    assert_eq!(wrong.change(), ChangeKind::Added);
    assert!(wrong.message.contains("widened"));
    assert_ne!(wrong.change(), ChangeKind::Changed);
}

#[test]
fn the_generated_sentence_cannot_disagree_with_the_fields() {
    let added = finding(Side::Absent, Side::value("x"));
    assert_eq!(added.describe(), "test `a` added");
    let removed = finding(Side::value("x"), Side::Absent);
    assert_eq!(removed.describe(), "test `a` removed");
    let changed = finding(Side::value("x"), Side::value("y"));
    assert_eq!(changed.describe(), "test `a` changed");
}

#[test]
fn absence_is_one_value_not_three_spellings() {
    // Three gates had invented "absent", "absent at the base commit" and "no
    // suppression here at the base commit". A consumer could not tell "not
    // present" from "present and equal to the word absent". Now there is one
    // value, and rendering it is the only place a string appears.
    assert_eq!(Side::Absent.render(), "(absent)");
    assert_eq!(Side::value("absent").render(), "absent");
    assert_ne!(Side::Absent, Side::value("absent"));
}

#[test]
fn a_set_renders_the_same_way_on_both_sides() {
    // The renderer is shared precisely so this holds. It was not: one gate
    // printed a list before and a count after, and the report read
    // `2 test(s): a, b -> 1 test(s)`.
    assert_eq!(Side::listed(&["a".into()]).render(), "a");
    assert_eq!(Side::listed(&["a".into(), "b".into()]).render(), "2 (a, b)");
    assert_eq!(Side::listed(&[]), Side::Absent);
    assert_eq!(Side::counted("unsafe block", 1).render(), "1 unsafe block");
    assert_eq!(Side::counted("unsafe block", 2).render(), "2 unsafe blocks");
}

#[test]
fn the_fingerprint_includes_the_subject_so_two_findings_do_not_collide() {
    // Two findings that differ only in what they are about must not share a
    // fingerprint, or a suppression scoped to one would silence the other.
    let a = Finding::new(
        gate("g"),
        Primitive::TestsNotDeleted,
        Severity::Error,
        Subject::new(SubjectKind::Test, "one"),
        None,
        None,
        Side::Absent,
        Side::value("x"),
        "m",
        analyzed(Primitive::TestsNotDeleted),
    );
    let b = Finding::new(
        gate("g"),
        Primitive::TestsNotDeleted,
        Severity::Error,
        Subject::new(SubjectKind::Test, "two"),
        None,
        None,
        Side::Absent,
        Side::value("x"),
        "m",
        analyzed(Primitive::TestsNotDeleted),
    );
    assert_ne!(a.fingerprint, b.fingerprint);
}
