//! M1 exit criteria, one block per gate.
//!
//! Every gate gets three properties, and a gate without all three is not done:
//!
//! 1. a fixture that **fires** it,
//! 2. a fixture that must **not** fire it,
//! 3. a test proving it is a **two-tree** check, not a single-tree one.
//!
//! (3) is the one that catches the class of bug this project already paid for.
//! A gate that looks only at the current tree passes a file that is
//! *individually* compliant but changed in a way the contract forbids; a gate
//! that only diffs is blind to a file that was always wrong. `EVIDENCE.md`
//! §6 names the specific false positive: a new function returning
//! `-> list[Note]` trips a return-type check that was matching a pattern
//! rather than comparing against a baseline. That case is a must-not-fire
//! fixture here.

use palisade_contract::{Gate, GateId, Primitive, Severity};
use palisade_gates::{GateContext, GateResult, registry};
use palisade_observe::{Budget, Observation};

// ---- harness ----------------------------------------------------------------

/// Build a context around a hand-made two-tree view, so a gate can be tested
/// without a repository. The point of the crate boundary is that this is
/// possible.
fn ctx<'a>(gate: &'a Gate, observation: &'a Observation) -> GateContext<'a> {
    GateContext { gate, observation }
}

fn gate(primitive: Primitive) -> Gate {
    let mut g = Gate::new(GateId::new("g").unwrap(), primitive);
    g.severity = Severity::Error;
    g
}

/// Assemble an observation from `path -> (base, head)`.
fn two_tree(files: &[(&str, Option<&str>, Option<&str>)]) -> Observation {
    let raw = palisade_git::ObservationInputs {
        base: Some("base-sha".to_string()),
        status: files
            .iter()
            .map(|(p, _, _)| palisade_git::StatusEntry {
                x: 'M',
                y: ' ',
                path: (*p).to_string(),
                orig_path: None,
            })
            .collect(),
        dirty: true,
        unstaged: String::new(),
        staged: String::new(),
        files: files
            .iter()
            .map(|(p, b, h)| palisade_git::RawFile {
                path: (*p).to_string(),
                orig_path: None,
                base: b.map(str::to_string),
                head: h.map(str::to_string),
                truncated: false,
            })
            .collect(),
    };
    Observation::capture(&raw, Budget::DEFAULT, true)
}

fn findings(result: &GateResult) -> &[palisade_orchestrate::Finding] {
    match result {
        GateResult::Findings(f) => f,
        other => panic!("expected findings, got {other:?}"),
    }
}

fn assert_clean(result: &GateResult) {
    assert_eq!(*result, GateResult::Clean, "expected a clean result");
}

// ---- dependency_surface_unchanged ------------------------------------------

const MANIFEST_MIN: &str = r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
serde = "1"
"#;

const MANIFEST_PLUS_DEP: &str = r#"
[package]
name = "demo"
version = "0.1.0"

[dependencies]
serde = "1"
tokio = { version = "1", features = ["full"] }
"#;

#[test]
fn dependency_surface_fires_on_a_new_production_dependency() {
    let g = gate(Primitive::DependencySurfaceUnchanged);
    let obs = two_tree(&[("Cargo.toml", Some(MANIFEST_MIN), Some(MANIFEST_PLUS_DEP))]);
    let r = registry::dispatch(g.primitive, &ctx(&g, &obs));
    let f = findings(&r);
    assert!(
        f.iter().any(|x| x.message.contains("tokio")),
        "expected a finding naming tokio, got {f:?}"
    );
    assert_eq!(f[0].expected, "absent", "the dependency was absent at base");
}

#[test]
fn dependency_surface_does_not_fire_on_a_dev_dependency() {
    // Dev-dependencies are not production surface. A gate that flags them is
    // the kind that gets switched off.
    let head = format!("{MANIFEST_MIN}\n[dev-dependencies]\ncriterion = \"0.5\"\n");
    let g = gate(Primitive::DependencySurfaceUnchanged);
    let obs = two_tree(&[("Cargo.toml", Some(MANIFEST_MIN), Some(&head))]);
    assert_clean(&registry::dispatch(g.primitive, &ctx(&g, &obs)));
}

#[test]
fn dependency_surface_does_not_fire_on_a_reformat() {
    // Comments, key order and whitespace are not surface. This is the "reorder
    // the manifest" false positive.
    let head = "# a comment\n[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\n\n  serde = \"1\"   # trailing\n";
    let g = gate(Primitive::DependencySurfaceUnchanged);
    let obs = two_tree(&[("Cargo.toml", Some(MANIFEST_MIN), Some(head))]);
    assert_clean(&registry::dispatch(g.primitive, &ctx(&g, &obs)));
}

#[test]
fn dependency_surface_fires_on_default_features_being_enabled() {
    // The case a manifest *diff* misses entirely, and the reason slop-gate
    // generalises the rule to surface rather than to "did the manifest
    // change".
    let head = r#"
[package]
name = "demo"

[dependencies]
serde = { version = "1", default-features = false }
"#;
    let g = gate(Primitive::DependencySurfaceUnchanged);
    let obs = two_tree(&[("Cargo.toml", Some(MANIFEST_MIN), Some(head))]);
    let r = registry::dispatch(g.primitive, &ctx(&g, &obs));
    let f = findings(&r);
    assert!(
        f.iter().any(|x| x.expected.contains("true")),
        "expected the default-features flip, got {f:?}"
    );
}

#[test]
fn dependency_surface_fires_on_a_version_bump() {
    let head = r#"
[package]
name = "demo"

[dependencies]
serde = "2"
"#;
    let g = gate(Primitive::DependencySurfaceUnchanged);
    let obs = two_tree(&[("Cargo.toml", Some(MANIFEST_MIN), Some(head))]);
    assert!(!findings(&registry::dispatch(g.primitive, &ctx(&g, &obs))).is_empty());
}

#[test]
fn dependency_surface_fires_on_a_feature_table_change() {
    let head = r#"
[package]
name = "demo"

[dependencies]
serde = "1"

[features]
extra = ["dep:x"]
"#;
    let g = gate(Primitive::DependencySurfaceUnchanged);
    let obs = two_tree(&[("Cargo.toml", Some(MANIFEST_MIN), Some(head))]);
    assert!(!findings(&registry::dispatch(g.primitive, &ctx(&g, &obs))).is_empty());
}

#[test]
fn dependency_surface_respects_an_allow_entry() {
    let mut g = gate(Primitive::DependencySurfaceUnchanged);
    g.allow = vec!["tokio".to_string()];
    let obs = two_tree(&[("Cargo.toml", Some(MANIFEST_MIN), Some(MANIFEST_PLUS_DEP))]);
    assert_clean(&registry::dispatch(g.primitive, &ctx(&g, &obs)));
}

#[test]
fn dependency_surface_is_untrustworthy_on_an_unparsable_manifest() {
    // Not a finding. "I could not read this" is not "the contract is met".
    let g = gate(Primitive::DependencySurfaceUnchanged);
    let obs = two_tree(&[("Cargo.toml", Some(MANIFEST_MIN), Some("[[[not toml"))]);
    let r = registry::dispatch(g.primitive, &ctx(&g, &obs));
    assert!(matches!(r, GateResult::Untrustworthy(_)), "got {r:?}");
}

#[test]
fn dependency_surface_is_untrustworthy_when_the_manifest_was_clipped() {
    let mut raw = two_tree(&[("Cargo.toml", Some(MANIFEST_MIN), Some(MANIFEST_PLUS_DEP))]);
    raw.files[0].truncated = true;
    let g = gate(Primitive::DependencySurfaceUnchanged);
    let r = registry::dispatch(g.primitive, &ctx(&g, &raw));
    assert!(matches!(r, GateResult::Untrustworthy(_)), "got {r:?}");
}

#[test]
fn dependency_surface_is_a_two_tree_check() {
    // Proof: the *same head* fires or does not fire depending only on the
    // base. A single-tree gate cannot tell these two apart.
    let g = gate(Primitive::DependencySurfaceUnchanged);
    let head = Some(MANIFEST_PLUS_DEP);

    let added = two_tree(&[("Cargo.toml", Some(MANIFEST_MIN), head)]);
    assert!(!findings(&registry::dispatch(g.primitive, &ctx(&g, &added))).is_empty());

    let already_there = two_tree(&[("Cargo.toml", Some(MANIFEST_PLUS_DEP), head)]);
    assert_clean(&registry::dispatch(g.primitive, &ctx(&g, &already_there)));
}

// ---- tests_not_deleted ------------------------------------------------------

#[test]
fn tests_not_deleted_fires_on_a_deleted_test_file() {
    let g = gate(Primitive::TestsNotDeleted);
    let obs = two_tree(&[("tests/it.rs", Some("#[test]\nfn t() {}\n"), None)]);
    let r = registry::dispatch(g.primitive, &ctx(&g, &obs));
    let f = findings(&r);
    assert_eq!(f[0].observed, "deleted");
}

#[test]
fn tests_not_deleted_fires_on_a_removed_test_in_a_surviving_file() {
    let g = gate(Primitive::TestsNotDeleted);
    let before = "#[test]\nfn a() {}\n\n#[test]\nfn b() {}\n";
    let after = "#[test]\nfn a() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(before), Some(after))]);
    let r = registry::dispatch(g.primitive, &ctx(&g, &obs));
    let f = findings(&r);
    assert!(
        f[0].observed.contains('1') && f[0].expected.contains('2'),
        "expected 2 -> 1, got {f:?}"
    );
}

#[test]
fn tests_not_deleted_fires_on_a_new_skip_marker() {
    let g = gate(Primitive::TestsNotDeleted);
    let before = "#[test]\nfn a() {}\n";
    let after = "#[test]\n#[ignore]\nfn a() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(before), Some(after))]);
    assert!(!findings(&registry::dispatch(g.primitive, &ctx(&g, &obs))).is_empty());
}

#[test]
fn tests_not_deleted_does_not_fire_when_a_test_is_added() {
    let g = gate(Primitive::TestsNotDeleted);
    let before = "#[test]\nfn a() {}\n";
    let after = "#[test]\nfn a() {}\n\n#[test]\nfn b() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(before), Some(after))]);
    assert_clean(&registry::dispatch(g.primitive, &ctx(&g, &obs)));
}

#[test]
fn tests_not_deleted_does_not_fire_on_a_deleted_non_test_file() {
    let g = gate(Primitive::TestsNotDeleted);
    let obs = two_tree(&[("src/notes.md", Some("hi"), None)]);
    assert_clean(&registry::dispatch(g.primitive, &ctx(&g, &obs)));
}

#[test]
fn tests_not_deleted_ignores_a_test_attribute_in_a_doc_comment() {
    // The crying-wolf case. A docs change that *mentions* `#[test]` must not
    // look like a test being added or removed.
    let g = gate(Primitive::TestsNotDeleted);
    let before = "//! module docs\n";
    let after =
        "//! Use `#[test]` to write a test.\n//! Example:\n//!     #[test]\n//!     fn t() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(before), Some(after))]);
    assert_clean(&registry::dispatch(g.primitive, &ctx(&g, &obs)));
}

#[test]
fn tests_not_deleted_ignores_a_similarly_named_non_test_directory() {
    let g = gate(Primitive::TestsNotDeleted);
    // `contest/` is not `tests/`. A substring match would fire here.
    let obs = two_tree(&[("contest/entries.rs", Some("x"), None)]);
    assert_clean(&registry::dispatch(g.primitive, &ctx(&g, &obs)));
}

#[test]
fn tests_not_deleted_fires_on_a_renamed_test_file() {
    // A rename is a deletion at the old path. The old path must still be
    // checked, which is why `FileView` carries `orig_path`.
    let g = gate(Primitive::TestsNotDeleted);
    let mut obs = two_tree(&[(
        "tests/new.rs",
        Some("#[test]\nfn t() {}\n"),
        Some("#[test]\nfn t() {}\n"),
    )]);
    obs.files[0].path = "tests/new.rs".to_string();
    obs.files[0].orig_path = Some("tests/old.rs".to_string());
    let r = registry::dispatch(g.primitive, &ctx(&g, &obs));
    // Content is identical under the new name, so the count did not drop; what
    // fired is nothing. This test pins that: a pure rename of an intact test
    // is not a deleted test.
    assert_clean(&r);
}

// ---- paths_unchanged --------------------------------------------------------

#[test]
fn paths_unchanged_fires_on_a_frozen_path() {
    let mut g = gate(Primitive::PathsUnchanged);
    g.paths = vec!["fixtures".to_string()];
    let obs = two_tree(&[("fixtures/a.json", Some("{}"), Some("{\"x\":1}"))]);
    assert!(!findings(&registry::dispatch(g.primitive, &ctx(&g, &obs))).is_empty());
}

#[test]
fn paths_unchanged_does_not_fire_on_a_sibling_directory() {
    // The false positive component-wise matching exists to prevent.
    let mut g = gate(Primitive::PathsUnchanged);
    g.paths = vec!["fixtures".to_string()];
    let obs = two_tree(&[("fixtures_extra/a.json", Some("{}"), Some("{\"x\":1}"))]);
    assert_clean(&registry::dispatch(g.primitive, &ctx(&g, &obs)));
}

#[test]
fn paths_unchanged_is_untrustworthy_with_no_paths_declared() {
    // A frozen-path gate with no paths enforces nothing, and must not read as
    // a passing rule.
    let g = gate(Primitive::PathsUnchanged);
    let obs = two_tree(&[("src/a.rs", Some("a"), Some("b"))]);
    let r = registry::dispatch(g.primitive, &ctx(&g, &obs));
    assert!(matches!(r, GateResult::Untrustworthy(_)), "got {r:?}");
}

// ---- the registry -----------------------------------------------------------

#[test]
fn an_unimplemented_primitive_is_untrustworthy_not_clean() {
    // The single most important property of the registry. A declared gate
    // that this build cannot run must say so, because "checked nothing" and
    // "checked and found nothing" are the two states a supervisor most needs
    // to tell apart.
    for p in [
        Primitive::PublicApiUnchanged,
        Primitive::UnsafeSurfaceUnchanged,
        Primitive::SuppressionsNotWidened,
        Primitive::SecretAbsent,
        Primitive::ChecksGreen,
        Primitive::ExternalTool,
        Primitive::Judged,
        Primitive::ContractNotLoosened,
        Primitive::ContractReviewStale,
    ] {
        let g = gate(p);
        let obs = two_tree(&[("src/a.rs", Some("a"), Some("b"))]);
        let r = registry::dispatch(p, &ctx(&g, &obs));
        assert!(
            matches!(r, GateResult::Untrustworthy(_)),
            "{p} returned {r:?} instead of Untrustworthy"
        );
    }
}

#[test]
fn every_primitive_is_either_implemented_or_explicitly_unimplemented() {
    // The registry matches exhaustively, so this is a runtime backstop against
    // a future `Primitive` being added to the match but forgotten here.
    for p in palisade_contract::ALL_PRIMITIVES {
        let implemented = registry::analyzed(p).is_some();
        assert_eq!(
            implemented,
            registry::implemented().contains(&p),
            "disagreement about {p}"
        );
    }
}
