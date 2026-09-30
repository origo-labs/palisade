//! The M0 regression test, and the reason this milestone precedes every gate.
//!
//! > **Observations must be collected on a dirty tree.** `git diff` reads
//! > unstaged changes, so committing each state first yields an empty diff
//! > everywhere and the whole benchmark silently scores at chance.
//! > — EVIDENCE.md, apparatus bugs
//!
//! That bug cost a measurement programme: a 400M classifier and a frontier
//! LLM were both blamed for a harness that had committed the states before
//! observing them, and the classifier's worst outputs were on states whose
//! diff was empty. Nothing to violate, and it flagged rules anyway.
//!
//! So this test is written before any gate exists, and it asserts both
//! directions: a dirty tree yields a non-empty observation, and the *same*
//! change committed yields an observation that is explicitly empty with a
//! stated reason — never a clean bill of health.

use rulebound_observe::{Budget, EmptyReason, Observation};
use rulebound_testkit::{FixtureRepo, TempDir};

const LIB_RS: &str = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
const LIB_RS_EDITED: &str = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn sub(a: i32, b: i32) -> i32 {\n    a - b\n}\n";

/// A minimal repo with one committed file, plus the baseline SHA.
fn seeded(label: &str) -> (TempDir, FixtureRepo, String) {
    let dir = TempDir::new(label).expect("temp dir");
    let repo = FixtureRepo::create_at("repo", dir.path()).expect("fixture repo");
    repo.write("src/lib.rs", LIB_RS).expect("write lib");
    repo.write("Cargo.toml", "[package]\nname = \"demo\"\n")
        .expect("write manifest");
    let base = repo.commit("initial").expect("commit");
    (dir, repo, base)
}

#[test]
fn a_dirty_tree_yields_a_non_empty_observation() {
    let (_dir, repo, base) = seeded("dirty");
    // The edit is UNSTAGED. This is the state a supervisor actually sees.
    repo.write("src/lib.rs", LIB_RS_EDITED).expect("edit");

    let inputs = repo
        .open()
        .observation_inputs(Some(&base))
        .expect("observation inputs");

    // First, the raw fact the bug turns on: git really does see it.
    assert!(
        inputs.unstaged.contains("pub fn sub"),
        "git diff should see the unstaged edit, got: {}",
        inputs.unstaged
    );
    assert!(inputs.dirty, "status should report a dirty worktree");

    let obs = Observation::capture(&inputs, Budget::DEFAULT, true);
    assert!(!obs.is_empty(), "an unstaged edit must be observable");
    assert_eq!(obs.empty, None);
    assert!(obs.diff.text.contains("pub fn sub"));
    assert!(!obs.diff.truncated);
}

#[test]
fn an_untracked_file_is_observed_with_its_content() {
    // A supervisor's observation of a brand-new file with no content is an
    // observation of nothing. This is the case that made a whole model
    // evaluation meaningless.
    let (_dir, repo, base) = seeded("untracked");
    repo.write("src/new.rs", "pub fn brand_new() {}\n")
        .expect("new file");

    let inputs = repo
        .open()
        .observation_inputs(Some(&base))
        .expect("observation inputs");
    let obs = Observation::capture(&inputs, Budget::DEFAULT, true);

    assert!(!obs.is_empty());
    assert!(
        inputs
            .status
            .iter()
            .any(|s| s.path == "src/new.rs" && s.is_untracked()),
        "status should list the untracked file: {:?}",
        inputs.status
    );
    assert!(repo.open().worktree_file("src/new.rs").is_some());
}

#[test]
fn a_staged_change_is_observed_even_though_a_bare_diff_is_silent() {
    // The other half of the same bug: `git status` says the file changed
    // while a plain `git diff` says nothing. Only reading both gives the
    // true observation.
    let (_dir, repo, base) = seeded("staged");
    repo.write("src/lib.rs", LIB_RS_EDITED).expect("edit");
    repo.git(&["add", "src/lib.rs"]).expect("stage");

    let r = repo.open();
    let bare = r.diff_unstaged(None).expect("bare diff");
    assert!(
        !bare.contains("pub fn sub"),
        "a bare `git diff` is silent about a staged change; that is exactly the trap"
    );

    let inputs = r.observation_inputs(Some(&base)).expect("inputs");
    let obs = Observation::capture(&inputs, Budget::DEFAULT, true);
    assert!(!obs.is_empty(), "the staged change must still be observed");
    assert!(obs.diff.text.contains("+pub fn sub"));
}

#[test]
fn committing_the_state_before_observing_is_caught_by_the_base() {
    // The regression, and a correction to the obvious way of writing it.
    //
    // The naive expectation is that committing the work empties the diff. It
    // does not — but only because the gate is anchored to a *base*, not to
    // the index. `git diff <base>` is base-vs-worktree, so it still sees
    // committed work. `git diff` alone is worktree-vs-index, and that is the
    // one that goes blind.
    //
    // This is exactly why PLAN.md 1.2 chose the two-tree model over a bespoke
    // baseline artifact: anchoring to a content-addressed commit makes the
    // "someone committed the state first" mistake a non-event rather than a
    // catastrophic one.
    let (_dir, repo, base) = seeded("committed");
    repo.write("src/lib.rs", LIB_RS_EDITED).expect("edit");
    let head = repo.commit("the work").expect("commit the work");
    assert_ne!(head, base, "the work is a new commit");

    let r = repo.open();

    // The buggy form: worktree vs index. Silent, even though the work exists.
    let bare = r.diff_unstaged(None).expect("bare diff");
    assert!(
        !bare.contains("pub fn sub"),
        "worktree-vs-index is blind to committed work; that is the trap"
    );

    // The form Rulebound uses: base vs worktree. Sees the work.
    let inputs = r.observation_inputs(Some(&base)).expect("inputs");
    assert!(
        inputs.unstaged.contains("pub fn sub"),
        "got: {}",
        inputs.unstaged
    );
    assert!(
        !inputs.dirty,
        "the tree is clean, yet the change is observed"
    );

    let obs = Observation::capture(&inputs, Budget::DEFAULT, true);
    assert!(!obs.is_empty(), "committed work must still be observed");
    assert_eq!(obs.empty, None);
}

#[test]
fn committing_the_state_and_using_no_base_is_reported_as_a_fault() {
    // The remaining hazard: an index-anchored observation of a repository
    // whose work is already committed. Nothing is dirty and nothing is
    // diffed, so the observation must be *attributable* rather than looking
    // like a clean bill of health. "There was nothing to check" and "I
    // checked and it was fine" have to be different strings.
    let (_dir, repo, _base) = seeded("committed-nobase");
    repo.write("src/lib.rs", LIB_RS_EDITED).expect("edit");
    repo.commit("the work").expect("commit the work");

    let inputs = repo.open().observation_inputs(None).expect("inputs");
    assert!(inputs.unstaged.is_empty());
    assert!(!inputs.dirty);

    // A caller that expected a change gets the fault named.
    let obs = Observation::capture(&inputs, Budget::DEFAULT, true);
    assert_eq!(obs.empty, Some(EmptyReason::DiffExpectedButAbsent));
    assert!(obs.is_empty());

    // A caller that did not is told the tree is clean, and nothing more.
    let honest = Observation::capture(&inputs, Budget::DEFAULT, false);
    assert_eq!(honest.empty, Some(EmptyReason::CleanWorktree));
}

#[test]
fn an_unchanged_tree_against_a_base_is_honestly_empty() {
    let (_dir, repo, base) = seeded("unchanged");
    let inputs = repo.open().observation_inputs(Some(&base)).expect("inputs");
    assert!(!inputs.dirty);
    let obs = Observation::capture(&inputs, Budget::DEFAULT, true);
    assert_eq!(obs.empty, Some(EmptyReason::DiffExpectedButAbsent));
}

#[test]
fn a_clean_tree_with_no_base_is_clean_worktree() {
    let (_dir, repo, _base) = seeded("nobase");
    let inputs = repo.open().observation_inputs(None).expect("inputs");
    let obs = Observation::capture(&inputs, Budget::DEFAULT, false);
    assert_eq!(obs.empty, Some(EmptyReason::CleanWorktree));
}

#[test]
fn a_deletion_is_observed() {
    let (_dir, repo, base) = seeded("delete");
    repo.remove("src/lib.rs").expect("remove");
    let inputs = repo.open().observation_inputs(Some(&base)).expect("inputs");
    let obs = Observation::capture(&inputs, Budget::DEFAULT, true);
    assert!(!obs.is_empty());
    assert!(inputs.status.iter().any(|s| s.path == "src/lib.rs"));
}

#[test]
fn a_path_with_a_space_and_a_quote_survives_the_observation() {
    // If the parse of `git status` were quoting-based, this would corrupt a
    // gate's view of which files moved. `-z` is what makes it safe.
    let (_dir, repo, base) = seeded("weird");
    let weird = "src/has space/it's \"quoted\".rs";
    repo.write(weird, "pub fn odd() {}\n").expect("weird path");
    let inputs = repo.open().observation_inputs(Some(&base)).expect("inputs");
    assert!(
        inputs.status.iter().any(|s| s.path == weird),
        "got {:?}",
        inputs.status
    );
    let obs = Observation::capture(&inputs, Budget::DEFAULT, true);
    assert!(!obs.is_empty());
}

#[test]
fn a_rename_is_observed_with_both_paths() {
    let (_dir, repo, base) = seeded("rename");
    repo.git(&["mv", "src/lib.rs", "src/renamed.rs"])
        .expect("rename");
    let inputs = repo.open().observation_inputs(Some(&base)).expect("inputs");
    let entry = inputs
        .status
        .iter()
        .find(|s| s.path == "src/renamed.rs")
        .expect("renamed entry");
    assert_eq!(entry.orig_path.as_deref(), Some("src/lib.rs"));
}
