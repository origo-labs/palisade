//! M2 gate fixtures: the AST-backed gates, and the false positive
//! `EVIDENCE.md` §6 says cost the predecessor its measurement.
//!
//! Same three properties as M1 — fires, must not fire, and is a two-tree
//! check — plus one extra that matters more here than anywhere else: **a file
//! that does not parse must be `Untrustworthy`, never `Clean`**. A gate that
//! skips what it cannot read and returns "nothing found" has silently
//! downgraded a check, and the number it reports looks like a measurement.

use palisade_contract::{Gate, GateId, Primitive, Severity};
use palisade_gates::{GateContext, GateResult, registry};

fn gate(primitive: Primitive) -> Gate {
    let mut g = Gate::new(GateId::new("g").unwrap(), primitive);
    g.severity = Severity::Error;
    g
}

/// A fixed instant, so a gate that reads the clock is still a pure function.
const FIXED_NOW: i64 = 1_790_000_000;

fn ctx<'a>(gate: &'a Gate, obs: &'a palisade_observe::Observation) -> GateContext<'a> {
    GateContext {
        gate,
        observation: obs,
        now_unix: FIXED_NOW,
    }
}

fn two_tree(files: &[(&str, Option<&str>, Option<&str>)]) -> palisade_observe::Observation {
    let raw = palisade_git::ObservationInputs {
        base: Some("base".to_string()),
        status: vec![],
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
    palisade_observe::Observation::capture(&raw, palisade_observe::Budget::DEFAULT, true)
}

fn run(primitive: Primitive, obs: &palisade_observe::Observation) -> GateResult {
    let g = gate(primitive);
    registry::dispatch(primitive, &ctx(&g, obs))
}

fn findings(r: &GateResult) -> &[palisade_orchestrate::Finding] {
    match r {
        GateResult::Findings(f) => f,
        other => panic!("expected findings, got {other:?}"),
    }
}

fn assert_clean(primitive: Primitive, obs: &palisade_observe::Observation) {
    assert_eq!(run(primitive, obs), GateResult::Clean, "{primitive} fired");
}

fn assert_untrustworthy(primitive: Primitive, obs: &palisade_observe::Observation) {
    let r = run(primitive, obs);
    assert!(
        matches!(r, GateResult::Untrustworthy(_)),
        "{primitive} should not have been able to tell, got {r:?}"
    );
}

// ---- the false positive this gate exists to avoid --------------------------

/// `EVIDENCE.md` §6: "a new function returning `-> list[Note]` trips a
/// return-type check, which must compare against a baseline rather than look
/// for a pattern."
#[test]
fn a_new_function_returning_a_generic_type_is_not_an_api_break() {
    let base = "pub fn get() -> u8 { 1 }\n";
    let head = "pub fn get() -> u8 { 1 }\n\npub fn notes() -> Vec<String> { vec![] }\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    let r = run(Primitive::PublicApiUnchanged, &obs);
    // It *is* reported — a new public item is news — but at `warn`, never at
    // the gate's blocking severity. The M1 incident was a new signature
    // reading as a *block*.
    let f = findings(&r);
    assert!(
        f.iter().all(|x| x.severity == Severity::Warn),
        "an addition must not block: {f:?}"
    );
    assert!(f.iter().any(|x| x.message.contains("notes")));
}

#[test]
fn a_removed_function_is_a_block() {
    let base = "pub fn keep() {}\npub fn gone() {}\n";
    let head = "pub fn keep() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    let res = run(Primitive::PublicApiUnchanged, &obs);
    let f = findings(&res);
    assert!(
        f.iter()
            .any(|x| x.severity == Severity::Error && x.message.contains("gone")),
        "a removed public item must block: {f:?}"
    );
}

// ---- public_api_unchanged ---------------------------------------------------

#[test]
fn a_renamed_parameter_changes_the_signature() {
    let base = "pub fn f(a: i32) -> i32 { a }\n";
    let head = "pub fn f(b: i32) -> i32 { b }\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    let res = run(Primitive::PublicApiUnchanged, &obs);
    let f = findings(&res);
    assert!(
        f.iter().any(|x| x.severity == Severity::Error),
        "a renamed parameter is a signature change: {f:?}"
    );
}

#[test]
fn a_changed_return_type_is_a_block() {
    let base = "pub fn f() -> u8 { 1 }\n";
    let head = "pub fn f() -> u16 { 1 }\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert!(
        findings(&run(Primitive::PublicApiUnchanged, &obs))
            .iter()
            .any(|x| x.severity == Severity::Error)
    );
}

#[test]
fn a_widened_generic_bound_is_a_block() {
    let base = "pub fn f<T: Clone>(t: T) {}\n";
    let head = "pub fn f<T: Clone + Send>(t: T) {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert!(
        findings(&run(Primitive::PublicApiUnchanged, &obs))
            .iter()
            .any(|x| x.severity == Severity::Error)
    );
}

#[test]
fn a_new_trait_impl_is_reported() {
    let base = "pub struct S;\n";
    let head = "pub struct S;\nimpl Clone for S { fn clone(&self) -> Self { S } }\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert!(
        !findings(&run(Primitive::PublicApiUnchanged, &obs)).is_empty(),
        "a new trait impl is new public surface"
    );
}

#[test]
fn changing_only_a_body_is_not_an_api_change() {
    // A gate that fires here fires on every line of every implementation edit,
    // and gets switched off within a week.
    let base = "pub fn f() -> i32 { 1 }\n";
    let head = "pub fn f() -> i32 { 1 + 1 }\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert_clean(Primitive::PublicApiUnchanged, &obs);
}

#[test]
fn running_rustfmt_is_not_an_api_change() {
    let base = "pub fn f(a: i32, b: i32) -> i32 { a + b }\n";
    let head = "pub fn f(\n    a: i32,\n    b: i32,\n) -> i32 {\n    a + b\n}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert_clean(Primitive::PublicApiUnchanged, &obs);
}

#[test]
fn private_items_are_not_public_api() {
    let base = "fn hidden() {}\n";
    let head = "fn hidden() {}\nfn also_hidden() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert_clean(Primitive::PublicApiUnchanged, &obs);
}

#[test]
fn crate_visible_items_are_not_public_api() {
    // `pub(crate)` is not public API, and treating it as such would make half
    // a real codebase's internals look like a compatibility surface.
    let base = "pub(crate) fn internal() {}\n";
    let head = "pub(crate) fn internal() {}\npub(crate) fn added() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert_clean(Primitive::PublicApiUnchanged, &obs);
}

#[test]
fn an_unparsable_file_is_untrustworthy_not_clean() {
    // The property that separates this gate from a text counter. A recovered
    // parse is a guess, and a guess reported as a measurement is worse than
    // no answer.
    let base = "pub fn f() {}\n";
    let head = "pub fn f( {\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert_untrustworthy(Primitive::PublicApiUnchanged, &obs);
}

#[test]
fn a_clipped_file_is_untrustworthy() {
    let mut obs = two_tree(&[("src/lib.rs", Some("pub fn f() {}"), Some("pub fn f() {}"))]);
    obs.files[0].truncated = true;
    assert_untrustworthy(Primitive::PublicApiUnchanged, &obs);
}

#[test]
fn a_deleted_file_declaring_public_api_is_a_removal() {
    let base = "pub fn gone() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), None)]);
    let res = run(Primitive::PublicApiUnchanged, &obs);
    let f = findings(&res);
    assert!(
        f.iter().any(|x| x.severity == Severity::Error),
        "a deleted file with public items is a removal: {f:?}"
    );
}

// ---- unsafe_surface_unchanged ----------------------------------------------

#[test]
fn unsafe_surface_fires_on_a_new_unsafe_block() {
    let base = "pub fn f() { let _x = 1; }\n";
    let head = "pub fn f() { unsafe { let _x = 1; } }\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    let res = run(Primitive::UnsafeSurfaceUnchanged, &obs);
    let f = findings(&res);
    assert!(
        f.iter().any(|x| x.message.contains("unsafe block")),
        "{f:?}"
    );
}

#[test]
fn unsafe_surface_fires_on_a_new_unsafe_fn() {
    let base = "pub fn safe() {}\n";
    let head = "pub unsafe fn danger() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    let res = run(Primitive::UnsafeSurfaceUnchanged, &obs);
    let f = findings(&res);
    assert!(
        f.iter().any(|x| x.message.contains("unsafe function")),
        "{f:?}"
    );
}

#[test]
fn unsafe_surface_does_not_fire_when_unsafe_is_unchanged() {
    // Growth, not presence. A file that already had two unsafe blocks still
    // having two is not a finding.
    let both = "pub unsafe fn a() {}\npub unsafe fn b() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(both), Some(both))]);
    assert_clean(Primitive::UnsafeSurfaceUnchanged, &obs);
}

#[test]
fn unsafe_surface_does_not_fire_when_unsafe_is_removed() {
    let base = "pub unsafe fn a() {}\npub unsafe fn b() {}\n";
    let head = "pub unsafe fn a() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert_clean(Primitive::UnsafeSurfaceUnchanged, &obs);
}

#[test]
fn unsafe_surface_does_not_fire_on_the_word_unsafe_in_a_comment() {
    // The text-counting failure, pinned. `unsafe` in a doc comment is prose.
    let base = "//! does not use unsafe\npub fn f() {}\n";
    let head = "//! really does not use unsafe code\npub fn f() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert_clean(Primitive::UnsafeSurfaceUnchanged, &obs);
}

#[test]
fn unsafe_surface_is_untrustworthy_on_an_unparsable_file() {
    let obs = two_tree(&[("src/lib.rs", Some("pub fn f() {}"), Some("fn ("))]);
    assert_untrustworthy(Primitive::UnsafeSurfaceUnchanged, &obs);
}

// ---- suppressions_not_widened ---------------------------------------------

#[test]
fn suppressions_fire_on_a_new_allow() {
    let base = "pub fn f() {}\n";
    let head = "#[allow(clippy::needless_range_loop)]\npub fn f() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert!(
        !findings(&run(Primitive::SuppressionsNotWidened, &obs)).is_empty(),
        "a new suppression is a new suppression"
    );
}

#[test]
fn suppressions_fire_when_a_wildcard_is_added() {
    let base = "pub fn f() {}\n";
    let head = "#[allow(clippy::all)]\npub fn f() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    let res = run(Primitive::SuppressionsNotWidened, &obs);
    let f = findings(&res);
    assert!(
        f.iter().any(|x| x.message.contains("wildcard")),
        "a whole-tool suppression is a different magnitude of event: {f:?}"
    );
}

#[test]
fn suppressions_fire_when_an_existing_allow_gains_a_lint() {
    let base = "#[allow(clippy::a)]\npub fn f() {}\n";
    let head = "#[allow(clippy::a, clippy::b)]\npub fn f() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    let res = run(Primitive::SuppressionsNotWidened, &obs);
    let f = findings(&res);
    assert!(
        f.iter().any(|x| x.message.contains("widened")),
        "adding a lint to an existing allow is widening: {f:?}"
    );
}

#[test]
fn suppressions_do_not_fire_when_the_list_narrows() {
    // Narrowing is an improvement. A gate that fires here makes the code worse.
    let base = "#[allow(clippy::a, clippy::b)]\npub fn f() {}\n";
    let head = "#[allow(clippy::a)]\npub fn f() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert_clean(Primitive::SuppressionsNotWidened, &obs);
}

#[test]
fn suppressions_do_not_fire_when_a_suppression_is_removed() {
    let base = "#[allow(clippy::a)]\npub fn f() {}\n";
    let head = "pub fn f() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert_clean(Primitive::SuppressionsNotWidened, &obs);
}

#[test]
fn suppressions_do_not_fire_on_a_derive() {
    // Otherwise this gate fires on every derive in the codebase.
    let base = "pub struct A;\n";
    let head = "#[derive(Debug)]\npub struct A;\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert_clean(Primitive::SuppressionsNotWidened, &obs);
}

#[test]
fn suppressions_do_not_fire_on_a_doc_comment_mentioning_allow() {
    let base = "//! docs\npub fn f() {}\n";
    let head = "//! Use #[allow(clippy::x)] to suppress.\npub fn f() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    assert_clean(Primitive::SuppressionsNotWidened, &obs);
}

#[test]
fn suppressions_are_untrustworthy_on_an_unparsable_file() {
    let obs = two_tree(&[("src/lib.rs", Some("pub fn f() {}"), Some("#[allow("))]);
    assert_untrustworthy(Primitive::SuppressionsNotWidened, &obs);
}

// ---- the M5 dry-run false positive, frozen ---------------------------------

#[test]
fn splitting_a_file_into_a_module_is_not_a_test_deletion() {
    // The M5 dry run over `warmplane` reported `17 test(s) removed from
    // src/http_v1.rs` on a merge where that file had been split into a module:
    // it became `mod.rs` plus eight new files and the tests moved into
    // `tests.rs`. Verified 17 before, 17 after, none deleted.
    //
    // A file-to-module split is an ordinary refactor that a competent agent
    // performs constantly, so a gate that reports it is a gate that gets
    // disabled — and disabling it takes the real findings with it. This is the
    // same shape as the `EVIDENCE.md` 6 return-type false positive: one
    // file's view of the world is not the repository's.
    let base = "#[test]\nfn alpha() {}\n\n#[test]\nfn beta() {}\n";
    // The same two tests, now in a different file, plus a `mod.rs` that
    // declares the module.
    let head_mod = "mod tests;\n";
    let head_tests = "#[test]\nfn alpha() {}\n\n#[test]\nfn beta() {}\n";

    let obs = two_tree(&[
        ("src/http_v1.rs", Some(base), Some(head_mod)),
        ("src/http_v1/tests.rs", None, Some(head_tests)),
    ]);
    assert_clean(Primitive::TestsNotDeleted, &obs);
}

#[test]
fn a_genuine_deletion_alongside_a_move_is_still_caught() {
    // The fix must not swallow real findings: a test that moved is fine, a test
    // that exists nowhere is not, and both can happen in one diff.
    let base = "#[test]\nfn alpha() {}\n\n#[test]\nfn beta() {}\n";
    let head = "#[test]\nfn alpha() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    let r = run(Primitive::TestsNotDeleted, &obs);
    let f = findings(&r);
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].message.contains("beta"), "{f:?}");
}

#[test]
fn same_named_tests_in_different_modules_stay_distinct() {
    // Identity is module + name, so two tests called `works` in two modules are
    // two tests and deleting one is a finding. Dropping the path from the
    // identity must not mean dropping the module from it.
    let base = "mod a { #[test] fn works() {} }\nmod b { #[test] fn works() {} }\n";
    let head = "mod a { #[test] fn works() {} }\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(head))]);
    let r = run(Primitive::TestsNotDeleted, &obs);
    let f = findings(&r);
    assert_eq!(f.len(), 1, "one of the two was removed: {f:?}");
    assert!(f[0].message.contains("b::works"), "{f:?}");
}

#[test]
fn a_file_the_observation_cannot_read_both_sides_is_untrustworthy() {
    // A partial tree cannot support "nothing was removed": the tests it did
    // not read might be the ones that went. Same refusal as an unparseable
    // file, and for the same reason.
    let base = "#[test]\nfn alpha() {}\n";
    let obs = two_tree(&[("src/lib.rs", Some(base), Some(base))]);
    let mut obs = obs;
    obs.files[0].truncated = true;
    assert_untrustworthy(Primitive::TestsNotDeleted, &obs);
}
