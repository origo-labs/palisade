//! The M1 gates, end to end, through a real repository.
//!
//! The unit tests in `gates.rs` build two-tree views by hand, which proves the
//! gate logic. This file proves the *plumbing*: that `palisade-observe` really
//! does hand a gate both sides of a change, on a real dirty tree, including
//! the cases M0 discovered the hard way (untracked content, staged-only work,
//! a commit made before the observation).
//!
//! A gate tested only against a hand-built view is a gate tested against a
//! fiction.

use palisade_contract::{Gate, GateId, Primitive, Severity};
use palisade_gates::{GateContext, GateResult, registry};
use palisade_observe::{Budget, Observation};
use palisade_testkit::{FixtureRepo, TempDir};

/// A fixed instant, so a gate that reads the clock is still a pure function.
const FIXED_NOW: i64 = 1_790_000_000;

fn gate(primitive: Primitive) -> Gate {
    let mut g = Gate::new(GateId::new("g").unwrap(), primitive);
    g.severity = Severity::Error;
    // `paths_unchanged` with no paths declared reports `Untrustworthy` rather
    // than passing, so a configured gate needs its paths. Configuring it here
    // keeps the false-positive floor honest: it is measuring the gate, not a
    // misconfiguration.
    if primitive == Primitive::PathsUnchanged {
        g.paths = vec!["fixtures".to_string()];
    }
    g
}

fn observe(repo: &FixtureRepo, base: &str) -> Observation {
    let inputs = repo
        .open()
        .observation_inputs(Some(base))
        .expect("observation inputs");
    Observation::capture(&inputs, Budget::DEFAULT, true)
}

fn findings(r: GateResult) -> Vec<palisade_orchestrate::Finding> {
    match r {
        GateResult::Findings(f) => f,
        GateResult::Clean => Vec::new(),
        GateResult::Untrustworthy(e) => panic!("untrustworthy: {e:?}"),
    }
}

const MANIFEST: &str =
    "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n";

fn seeded(label: &str) -> (TempDir, FixtureRepo, String) {
    let dir = TempDir::new(label).expect("temp dir");
    let repo = FixtureRepo::create_at("repo", dir.path()).expect("fixture");
    repo.write("Cargo.toml", MANIFEST).expect("manifest");
    repo.write("src/lib.rs", "pub fn f() {}\n").expect("lib");
    let base = repo.commit("initial").expect("commit");
    (dir, repo, base)
}

#[test]
fn a_real_manifest_edit_is_seen_by_the_dependency_gate() {
    let (_d, repo, base) = seeded("e2e-dep");
    repo.write(
        "Cargo.toml",
        &MANIFEST.replace("serde = \"1\"", "serde = \"1\"\ntokio = \"1\""),
    )
    .expect("edit");

    let obs = observe(&repo, &base);
    let g = gate(Primitive::DependencySurfaceUnchanged);
    let f = findings(registry::dispatch(
        g.primitive,
        &GateContext {
            gate: &g,
            observation: &obs,
            now_unix: FIXED_NOW,
        },
    ));
    assert!(
        f.iter().any(|x| x.message.contains("tokio")),
        "expected tokio, got {f:?}"
    );
    // And the base side really came from the base commit, not the worktree.
    let view = obs
        .file("Cargo.toml")
        .expect("Cargo.toml in the observation");
    assert!(view.base.as_deref().is_some_and(|b| b.contains("serde")));
    assert!(view.head.as_deref().is_some_and(|h| h.contains("tokio")));
}

#[test]
fn an_untracked_manifest_is_still_observed() {
    // M0's second finding: `git diff` does not include untracked files, so a
    // gate that only read the diff would be blind here. This is the end-to-end
    // proof that the two-tree view fixed it.
    let (_d, repo, base) = seeded("e2e-untracked");
    repo.write("vendor/Cargo.toml", "package = \"x\"\n")
        .expect("untracked manifest");

    let obs = observe(&repo, &base);
    let view = obs
        .file("vendor/Cargo.toml")
        .expect("untracked file in view");
    assert!(view.base.is_none(), "it did not exist at the base commit");
    assert!(
        view.head.as_deref().is_some_and(|h| h.contains("package")),
        "untracked content must reach the gate: {view:?}"
    );
}

#[test]
fn a_staged_only_change_reaches_the_gates() {
    let (_d, repo, base) = seeded("e2e-staged");
    repo.write("src/lib.rs", "pub fn f() {}\npub fn g() {}\n")
        .expect("edit");
    repo.git(&["add", "src/lib.rs"]).expect("stage");

    let obs = observe(&repo, &base);
    let view = obs.file("src/lib.rs").expect("in view");
    assert!(
        view.head.as_deref().is_some_and(|h| h.contains("pub fn g")),
        "a staged change must be observed: {view:?}"
    );
}

#[test]
fn a_test_removed_after_being_committed_is_still_caught() {
    // The two-tree model earns its keep here: the work was committed before
    // the observation, which is the mistake that blinded the predecessor's
    // benchmark, and the gate still sees it because it diffs against a commit
    // rather than against the index.
    //
    // The baseline is the commit that *added* the two tests, not the fixture's
    // initial commit. That distinction is the whole gate: relative to the
    // initial commit there is no deletion at all, and a gate that reported one
    // would be a gate reporting on a tree nobody is reviewing.
    let (_d, repo, _initial) = seeded("e2e-committed");
    repo.write("tests/a.rs", "#[test]\nfn a() {}\n\n#[test]\nfn b() {}\n")
        .expect("add tests");
    let base = repo.commit("add tests").expect("commit");
    repo.write("tests/a.rs", "#[test]\nfn a() {}\n")
        .expect("remove one");
    repo.commit("remove one").expect("commit");

    let obs = observe(&repo, &base);
    let g = gate(Primitive::TestsNotDeleted);
    let f = findings(registry::dispatch(
        g.primitive,
        &GateContext {
            gate: &g,
            observation: &obs,
            now_unix: FIXED_NOW,
        },
    ));
    let removed = f
        .iter()
        .find(|x| x.subject.kind == palisade_orchestrate::SubjectKind::Test)
        .unwrap_or_else(|| panic!("no test finding, got {f:?}"));
    // The subject names the test by its module path, not its file: the file is
    // already the finding location, and putting it in the identity is what made
    // a file-to-module split look like a deletion.
    assert_eq!(removed.subject.name, "b");
    assert_eq!(
        removed.path.as_deref().map(|p| p.as_str()),
        Some("tests/a.rs")
    );
    // A removal is the test on the before side and absent after, so the change
    // kind is derived rather than stated.
    assert!(removed.expected.render().contains('b'));
    assert_eq!(removed.observed, palisade_orchestrate::Side::Absent);
    assert_eq!(removed.change(), palisade_orchestrate::ChangeKind::Removed);
}

#[test]
fn a_deleted_test_file_is_caught_on_a_real_tree() {
    let (_d, repo, _initial) = seeded("e2e-deleted");
    repo.write("tests/a.rs", "#[test]\nfn a() {}\n")
        .expect("add");
    let base = repo.commit("add").expect("commit");
    repo.remove("tests/a.rs").expect("delete");
    repo.commit("delete").expect("commit");

    let obs = observe(&repo, &base);
    let g = gate(Primitive::TestsNotDeleted);
    let f = findings(registry::dispatch(
        g.primitive,
        &GateContext {
            gate: &g,
            observation: &obs,
            now_unix: FIXED_NOW,
        },
    ));
    assert_eq!(f.len(), 1, "expected exactly one finding, got {f:?}");
    // The gate names the test rather than reporting "the file was deleted",
    // because the base tree is parsed and the tests it declared are known.
    assert!(
        f[0].message.contains('a'),
        "the deleted test should be named: {f:?}"
    );
}

#[test]
fn a_renamed_test_file_keeps_its_base_content() {
    // M0's rename handling: the base side lives at the original path, so a
    // pure rename must not look like a test appearing from nowhere.
    let (_d, repo, _initial) = seeded("e2e-rename");
    repo.write("tests/old.rs", "#[test]\nfn a() {}\n")
        .expect("add");
    let base = repo.commit("add").expect("commit");
    repo.git(&["mv", "tests/old.rs", "tests/new.rs"])
        .expect("rename");
    repo.commit("rename").expect("commit");

    let obs = observe(&repo, &base);
    let view = obs.file("tests/new.rs").expect("renamed file in view");
    assert_eq!(view.orig_path.as_deref(), Some("tests/old.rs"));
    assert!(
        view.base.as_deref().is_some_and(|b| b.contains("fn a")),
        "the base side must come from the original path: {view:?}"
    );
}

#[test]
fn an_unchanged_repository_produces_no_findings_from_any_m1_gate() {
    // The false-positive floor. A repository that was not changed cannot
    // produce a finding, and the number that matters is this one.
    let (_d, repo, base) = seeded("e2e-noop");
    let obs = observe(&repo, &base);
    for p in registry::implemented() {
        let g = gate(p);
        let r = registry::dispatch(
            p,
            &GateContext {
                gate: &g,
                observation: &obs,
                now_unix: FIXED_NOW,
            },
        );
        assert_eq!(r, GateResult::Clean, "{p} fired on an unchanged repository");
    }
}

// ---- the AST-backed gates, on a real tree ---------------------------------

#[test]
fn a_real_removed_public_function_is_caught() {
    let (_d, repo, _initial) = seeded("e2e-api");
    repo.write("src/lib.rs", "pub fn keep() {}\npub fn gone() {}\n")
        .expect("write");
    let base = repo.commit("two functions").expect("commit");
    repo.write("src/lib.rs", "pub fn keep() {}\n")
        .expect("remove one");
    repo.commit("remove one").expect("commit");

    let obs = observe(&repo, &base);
    let g = gate(Primitive::PublicApiUnchanged);
    let f = findings(registry::dispatch(
        g.primitive,
        &GateContext {
            gate: &g,
            observation: &obs,
            now_unix: FIXED_NOW,
        },
    ));
    assert!(
        f.iter().any(|x| x.message.contains("gone")),
        "expected the removal, got {f:?}"
    );
}

#[test]
fn a_real_wildcard_suppression_is_caught() {
    // `seeded` already committed a plain `pub fn f`, so that commit is the
    // baseline. Re-committing identical content would be an empty commit.
    let (_d, repo, base) = seeded("e2e-suppress");
    repo.write("src/lib.rs", "#[allow(clippy::all)]\npub fn f() {}\n")
        .expect("widen");
    repo.commit("widen").expect("commit");

    let obs = observe(&repo, &base);
    let g = gate(Primitive::SuppressionsNotWidened);
    let f = findings(registry::dispatch(
        g.primitive,
        &GateContext {
            gate: &g,
            observation: &obs,
            now_unix: FIXED_NOW,
        },
    ));
    assert!(f.iter().any(|x| x.message.contains("wildcard")), "{f:?}");
}

#[test]
fn a_real_unsafe_block_is_caught() {
    let (_d, repo, _initial) = seeded("e2e-unsafe");
    repo.write("src/lib.rs", "pub fn f() { let _x = 1; }\n")
        .expect("write");
    let base = repo.commit("safe").expect("commit");
    repo.write("src/lib.rs", "pub fn f() { unsafe { let _x = 1; } }\n")
        .expect("add unsafe");
    repo.commit("add unsafe").expect("commit");

    let obs = observe(&repo, &base);
    let g = gate(Primitive::UnsafeSurfaceUnchanged);
    let f = findings(registry::dispatch(
        g.primitive,
        &GateContext {
            gate: &g,
            observation: &obs,
            now_unix: FIXED_NOW,
        },
    ));
    assert!(f.iter().any(|x| x.observed.render().contains('1')), "{f:?}");
}

#[test]
fn an_unrelated_edit_produces_no_ast_findings() {
    // The false-positive floor for the AST gates, on a real repository. A new
    // public function is a `warn`, never a block, and everything else is
    // silent.
    let (_d, repo, base) = seeded("e2e-ast-noop");
    repo.write(
        "src/lib.rs",
        "pub fn f() { let _x = 1; }\n\npub fn helper() -> u8 { 7 }\n",
    )
    .expect("edit");

    let obs = observe(&repo, &base);
    for p in [
        Primitive::PublicApiUnchanged,
        Primitive::UnsafeSurfaceUnchanged,
        Primitive::SuppressionsNotWidened,
    ] {
        let g = gate(p);
        let r = registry::dispatch(
            p,
            &GateContext {
                gate: &g,
                observation: &obs,
                now_unix: FIXED_NOW,
            },
        );
        match r {
            GateResult::Clean => {}
            GateResult::Findings(f) => assert!(
                f.iter().all(|x| x.severity == Severity::Warn),
                "{p} blocked on an unrelated edit: {f:?}"
            ),
            GateResult::Untrustworthy(e) => panic!("{p} could not tell: {e:?}"),
        }
    }
}
