//! `tests_not_deleted` — were tests removed, or made not to run?
//!
//! Two-tree and AST-based. The set of test **identities** is compared, not a
//! count, and that is the whole point:
//!
//! - Comparing names catches a **delete-and-replace** pair that a count
//!   cannot. Remove one test and add another and the count is unchanged, while
//!   a test somebody was relying on is gone. This gate existed in M1 counting
//!   `#[test]` attributes, and M2 replaced it because a count is a weaker
//!   statement than an identity and the text matcher underneath it was doing
//!   an AST gate's job.
//! - The `is_test_path` heuristic is **gone**. A deleted file is parsed at
//!   the base commit and the tests it declared are reported by name, so
//!   "is this a test file" no longer has to be guessed. A heuristic here had
//!   two failure modes — missing a test in an oddly named file, and reporting
//!   on a directory called `contest/` — and now has neither.
//! - Framework attributes are recognised, not just bare `#[test]`. A gate that
//!   only knows `#[test]` reports "nothing removed" on a repository that
//!   writes `#[tokio::test]`, which is the failure mode of a check that has
//!   never met the codebase it runs on.
//!
//! `cargo test -- --list` gives the real inventory — names as the compiler
//! sees them, including macro-generated tests — and arrives in M3 as a
//! `consumes` edge from `checks_green`. Until then this is a static reading of
//! the source, and the report says so rather than implying full coverage.

use std::collections::BTreeSet;

use palisade_ast::{ParseCache, ParsedFile, TestFn};
use palisade_orchestrate::{Finding, Side, Subject, SubjectKind, UntrustworthyReason};

use crate::{GateContext, GateResult, truncated};

/// The gate.
pub fn run(ctx: &GateContext<'_>) -> GateResult {
    let cache = ParseCache::new();
    let mut findings = Vec::new();
    let mut saw_rust = false;

    for view in &ctx.observation.files {
        if !view.path.ends_with(".rs") {
            continue;
        }
        saw_rust = true;
        if view.truncated {
            return GateResult::Untrustworthy(truncated(&view.path));
        }

        // Both sides. A deleted file has no head, and every test it declared
        // is a removal — which falls out of the set comparison for free.
        let base = match view.base.as_deref() {
            Some(src) => match parse(&cache, src, &view.path) {
                Ok(f) => Some(f),
                Err(e) => return GateResult::Untrustworthy(e),
            },
            None => None,
        };
        let head = match view.head.as_deref() {
            Some(src) => match parse(&cache, src, &view.path) {
                Ok(f) => Some(f),
                Err(e) => return GateResult::Untrustworthy(e),
            },
            None => None,
        };

        // A file that was only added cannot have lost a test.
        let Some(base) = base.as_ref() else { continue };

        let base_ids: BTreeSet<String> = base.tests().iter().map(TestFn::id).collect();
        // A deleted file has no head, so it has no tests, so every test it
        // declared is a removal. This is the case the `is_test_path` heuristic
        // used to guess at, and getting it from the parse instead means the
        // guess is gone rather than merely improved.
        let head_ids: BTreeSet<String> = head
            .as_ref()
            .map(|h| h.tests().iter().map(TestFn::id).collect())
            .unwrap_or_default();

        // Removals.
        let removed: Vec<&TestFn> = base
            .tests()
            .iter()
            .filter(|t| !head_ids.contains(&t.id()))
            .collect();
        if !removed.is_empty() {
            // Both sides rendered the same way, so the pair is comparable. The
            // previous version printed a list before and a count after, which
            // is why the report read `2 test(s): a, b -> 1 test(s)`.
            let before: Vec<String> = base.tests().iter().map(TestFn::id).collect();
            let after: Vec<String> = head
                .as_ref()
                .map(|h| h.tests().iter().map(TestFn::id).collect())
                .unwrap_or_default();
            let names: Vec<String> = removed.iter().map(|t| t.id()).collect();
            findings.push(Finding::new(
                ctx.gate.id.clone(),
                ctx.gate.primitive,
                ctx.gate.severity,
                Subject::new(SubjectKind::Test, names.join(", ")),
                Some(view.path.clone().into()),
                None,
                Side::listed(&before),
                Side::listed(&after),
                format!(
                    "{} test(s) removed from `{}`: {}",
                    removed.len(),
                    view.path,
                    names.join(", ")
                ),
                ctx.origin(),
            ));
        }

        // Newly skipped. A test that gains `#[ignore]` still exists and still
        // runs in some configurations, so it is reported separately from a
        // removal — but it is a way of not running a test without deleting
        // it, and a gate that only watched for deletions would miss it.
        let Some(head) = head.as_ref() else { continue };
        for t in head.tests() {
            let id = t.id();
            if !base_ids.contains(&id) {
                continue; // a new test, even a skipped one
            }
            let Some(base_t) = base.tests().iter().find(|b| b.id() == id) else {
                continue;
            };
            if (t.ignored || t.should_panic) && !base_t.ignored && !base_t.should_panic {
                let marker = if t.ignored {
                    "#[ignore]"
                } else {
                    "#[should_panic]"
                };
                findings.push(Finding::new(
                    ctx.gate.id.clone(),
                    ctx.gate.primitive,
                    ctx.gate.severity,
                    Subject::new(SubjectKind::Test, id.clone()),
                    Some(view.path.clone().into()),
                    None,
                    Side::value("runs"),
                    Side::value(marker),
                    format!("`{id}` in `{}` was marked {marker}", view.path),
                    ctx.origin(),
                ));
            }
        }
    }

    // No `.rs` file in the observation means no `.rs` file changed, which
    // means no test changed. Sound for the same reason as the equivalent
    // branch in `dependency_surface_unchanged`: the view is built from the diff
    // of the same two trees this gate compares, so absence means "unchanged"
    // rather than "unexamined". It would be unsound under a status-derived
    // view, which is why the reasoning is written down rather than assumed.
    let _ = saw_rust;

    GateResult::findings(findings)
}

/// Parse a file, turning a refusal into the gate's `Untrustworthy` outcome.
///
/// A file that does not parse is not counted. A static reading of a broken
/// file is a reading of whatever happened to be legible, and a gate that
/// reports a confident count from it is worse than a gate that declines.
fn parse(cache: &ParseCache, source: &str, path: &str) -> Result<ParsedFile, UntrustworthyReason> {
    match cache.parse(source).as_ref() {
        Ok(f) => Ok(f.clone()),
        Err(e) => Err(UntrustworthyReason::Indeterminate {
            detail: format!("{path}: {e}"),
        }),
    }
}
