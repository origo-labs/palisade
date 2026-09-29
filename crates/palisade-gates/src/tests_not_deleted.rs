//! `tests_not_deleted` — were tests removed, or made not to run?
//!
//! Two-tree, AST-based, and compared **across the whole tree** rather than
//! per file. That last part is the result of the M5 dry run, and it is the
//! single most important property this gate has.
//!
//! - **Tree-level, not per-file.** A per-file comparison reported `17 test(s)
//!   removed from src/http_v1.rs` on a real merge where `src/http_v1.rs` had
//!   been split into a module: the file became `mod.rs` plus eight new files
//!   and the tests moved into `tests.rs`. Verified: 17 tests before, 17 after,
//!   none deleted. A file split is an ordinary refactor that a competent agent
//!   performs constantly, so a gate that reports it is a gate that gets
//!   disabled — and disabling it takes the real findings with it. A test that
//!   *moved* has not been deleted; a test that exists *nowhere* has.
//! - **Identities, not counts.** Comparing names catches a delete-and-replace
//!   pair that a count cannot: remove one test and add another, the count is
//!   unchanged, and a test somebody relied on is gone. M1 counted `#[test]`
//!   attributes; M2 replaced that because a count is a weaker statement than
//!   an identity.
//! - **The `is_test_path` heuristic is gone.** A deleted file is parsed at
//!   the base commit and the tests it declared are reported by name. A
//!   heuristic there had two failure modes — missing a test in an oddly named
//!   file, and reporting on a directory called `contest/` — and has neither.
//! - **Framework attributes are recognised**, not just bare `#[test]`. A gate
//!   that only knows `#[test]` reports "nothing removed" on a repository that
//!   writes `#[tokio::test]`.
//!
//! **The scope is bounded, and the bound is reported.** A tree-wide comparison
//! needs the whole tree, but the observation is deliberately budgeted, so this
//! gate reads what the observation holds and **says so when it cannot see
//! everything**. A partial tree cannot support a "nothing was removed" claim,
//! because the tests it did not read might have been the ones that went. That
//! is `Untrustworthy`, not `Clean` — the same refusal-to-guess rule as an
//! unparseable file, and for the same reason.
//!
//! ## The inventory, when the contract provides one
//!
//! The static reading above can see a test that *exists*. It cannot see one
//! that **runs**. A test behind a `#[cfg]`, one the harness skips, one whose
//! name the compiler rewrote — all of them read as present in the source and
//! none of them are in the suite.
//!
//! `cargo test -- --list` is the real inventory, and it arrives through a
//! declared `consumes` edge from `checks_green`. When it is present this gate
//! reports a test that is in the source and **not in the build**: a test
//! nobody runs is a test that was deleted in everything but name, and it is
//! the one class of finding a source reading cannot produce on its own.
//!
//! The two are combined rather than substituted. The inventory is a *head*
//! snapshot, so it cannot answer "was a test removed between the base commit
//! and now" — that needs the base tree built and listed too, a second full
//! compile on every commit to re-answer a question the source reading already
//! answers correctly. So the source reading keeps ownership of removal, and
//! the inventory adds the thing it is uniquely able to say.

use std::collections::{BTreeMap, BTreeSet};

use palisade_ast::{ParseCache, ParsedFile, TestFn};
use palisade_orchestrate::{Finding, Side, Subject, SubjectKind, UntrustworthyReason};

use crate::{GateContext, GateResult};

/// The gate.
pub fn run(ctx: &GateContext<'_>) -> GateResult {
    let cache = ParseCache::new();

    // Every `.rs` path in the base tree, and whether the observation actually
    // holds both sides of it. `FileView` only covers files that differ, so a
    // file present at base and unchanged at head is absent from `files` — and
    // a test in such a file is, by definition, still there. Enumerating the
    // base tree's paths is what lets the gate tell "I saw the file and it is
    // unchanged" from "I never looked at the file".
    let (base_paths, covered) = base_tree_paths(ctx);

    let mut base_tests: BTreeMap<String, Vec<Located>> = BTreeMap::new();
    let mut head_tests: BTreeMap<String, Vec<Located>> = BTreeMap::new();
    let mut missing_sides: Vec<String> = Vec::new();

    for path in &base_paths {
        let view = ctx.observation.file(path);
        if let Some(v) = view
            && v.truncated
        {
            // Clipped content is not content we can compare, whichever side is
            // missing. Report it rather than reasoning about half a file.
            missing_sides.push(path.clone());
            continue;
        }
        match view {
            // Both sides available: parse them.
            Some(v) if v.base.is_some() && v.head.is_some() => {
                let (Ok(base), Ok(head)) = (
                    parse(&cache, v.base.as_deref().expect("both sides present"), path),
                    parse(&cache, v.head.as_deref().expect("both sides present"), path),
                ) else {
                    return GateResult::Untrustworthy(UntrustworthyReason::Indeterminate {
                        detail: format!("{path}: could not be read on both sides"),
                    });
                };
                for t in base.tests() {
                    base_tests
                        .entry(test_key(t))
                        .or_default()
                        .push(Located::new(path, t));
                }
                for t in head.tests() {
                    head_tests
                        .entry(test_key(t))
                        .or_default()
                        .push(Located::new(path, t));
                }
            }
            // The file is unchanged, so the base tree's version is current.
            // We do not have its contents, but we do not need them: nothing
            // about this file changed, so nothing about its tests changed.
            None if !covered.contains(path) => {}
            // A file that is **new** at head. It cannot have lost a test, so
            // it is not a gap in the base tree — and treating one as a gap
            // would make every refactor that splits a file unanswerable, which
            // is the M5 false positive arriving through the other door.
            // Its tests are collected, so a later comparison sees them.
            Some(v) if v.base.is_none() => {
                let head = match parse(&cache, v.head.as_deref().expect("head present"), path) {
                    Ok(f) => f,
                    Err(e) => return GateResult::Untrustworthy(e),
                };
                for t in head.tests() {
                    head_tests
                        .entry(test_key(t))
                        .or_default()
                        .push(Located::new(path, t));
                }
            }
            // The file was **deleted**. Its tests are gone by definition, and
            // that is a removal, not a gap in what we can see. This is the one
            // case where a missing head side is the answer rather than an
            // obstacle to it.
            Some(v) if v.base.is_some() && v.head.is_none() => {
                let base = match parse(&cache, v.base.as_deref().expect("base present"), path) {
                    Ok(f) => f,
                    Err(e) => return GateResult::Untrustworthy(e),
                };
                for t in base.tests() {
                    base_tests
                        .entry(test_key(t))
                        .or_default()
                        .push(Located::new(path, t));
                }
            }
            // Clipped, or otherwise unreadable on both sides. A gap in what
            // we can see cannot support a conclusion, so it is not a finding
            // and not a pass: it is a refusal.
            _ => missing_sides.push(path.clone()),
        }
    }

    // A test that exists in the base tree and in no changed file either. A
    // removed test is one whose whole identity is gone; a moved test is
    // present under a new path, and the paths are not part of the identity.
    let mut findings = Vec::new();

    // A test the source still has and the build does not run.
    //
    // Only when an inventory was provided. Without one this gate makes no
    // claim about whether a test runs, and says so by omission rather than by
    // reporting a coverage it does not have.
    if let Some(inv) = ctx.test_inventory {
        let mut all_tests: BTreeMap<String, Vec<Located>> = BTreeMap::new();
        for (key, locations) in base_tests.iter().chain(head_tests.iter()) {
            for l in locations {
                all_tests.entry(key.clone()).or_default().push(l.clone());
            }
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for (key, locations) in &all_tests {
            if !seen.insert(key.as_str()) {
                continue;
            }
            let Some(first) = locations.first() else {
                continue;
            };
            if inv.contains(key) || inv.contains(&first.name) {
                continue;
            }
            findings.push(Finding::new(
                ctx.gate.id.clone(),
                ctx.gate.primitive,
                ctx.gate.severity,
                Subject::new(SubjectKind::Test, key.clone()),
                Some(first.path.clone().into()),
                None,
                // The pair is about the *inventory*, read the way the
                // inventory itself reads: the test should be listed, and it
                // is not. So the change kind is `Removed` — the test has been
                // removed from what the suite runs — and the message says what
                // that means in the source's terms.
                Side::value("run by the test suite"),
                Side::Absent,
                format!(
                    "`{key}` is in the source but not in `cargo test -- --list`. \
                     It is not run: a test nobody runs is a test that was \
                     deleted in everything but name."
                ),
                ctx.origin(),
            ));
        }
    }

    for (key, locations) in &base_tests {
        if head_tests.contains_key(key) {
            continue;
        }
        // Name the test by its module path. The file is already in `path`, and
        // repeating it here made the subject say "src/lib.rs::works" for a
        // test that actually lives in `mod b`.
        let names: Vec<String> = locations
            .iter()
            .map(|l| {
                if l.module.is_empty() {
                    l.name.clone()
                } else {
                    format!("{}::{}", l.module, l.name)
                }
            })
            .collect();
        findings.push(Finding::new(
            ctx.gate.id.clone(),
            ctx.gate.primitive,
            ctx.gate.severity,
            Subject::new(SubjectKind::Test, names.join(", ")),
            locations.first().map(|l| l.path.clone().into()),
            None,
            Side::listed(&base_test_names(key, &base_tests)),
            Side::Absent,
            format!(
                "{} test(s) no longer exist anywhere in the tree: {}",
                names.len(),
                names.join(", ")
            ),
            ctx.origin(),
        ));
    }

    // A test that was there and is now skipped. A test that gains `#[ignore]`
    // still exists and still runs in some configurations, so it is reported
    // separately from a removal — but it is a way of not running a test
    // without deleting it, and a gate that only watched for deletions would
    // miss it.
    for (key, head_locations) in &head_tests {
        let Some(base_locations) = base_tests.get(key) else {
            continue; // a new test, even a skipped one
        };
        let head_t = head_locations.first().expect("locations are never empty");
        let base_t = base_locations.first().expect("locations are never empty");
        if (head_t.ignored || head_t.should_panic) && !base_t.ignored && !base_t.should_panic {
            let marker = if head_t.ignored {
                "#[ignore]"
            } else {
                "#[should_panic]"
            };
            findings.push(Finding::new(
                ctx.gate.id.clone(),
                ctx.gate.primitive,
                ctx.gate.severity,
                Subject::new(SubjectKind::Test, head_t.name.clone()),
                Some(head_t.path.clone().into()),
                None,
                Side::value("runs"),
                Side::value(marker),
                format!("`{}` was marked {marker}", head_t.name),
                ctx.origin(),
            ));
        }
    }

    // The bound. If the observation could not show us both sides of some
    // file, this gate cannot claim nothing was removed — the tests it did not
    // read might be the ones that went.
    if !missing_sides.is_empty() {
        return GateResult::Untrustworthy(UntrustworthyReason::Indeterminate {
            detail: format!(
                "{} file(s) in the base tree were not fully observable ({}). \
                 A tree-wide test comparison cannot conclude from a partial \
                 tree, because the tests it did not read might be the ones that \
                 were removed.",
                missing_sides.len(),
                preview(&missing_sides)
            ),
        });
    }

    GateResult::findings(findings)
}

/// Every `.rs` path at the base commit, and the set the observation could see.
///
/// The base tree is the only place a test that has since been removed can
/// still be *seen*. Enumerating it is what turns "no finding" from "I found
/// nothing" into "I looked at all of it and found nothing".
fn base_tree_paths(ctx: &GateContext<'_>) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut paths = BTreeSet::new();
    let mut covered = BTreeSet::new();
    for view in &ctx.observation.files {
        if !view.path.ends_with(".rs") {
            continue;
        }
        covered.insert(view.path.clone());
        // The path itself, and the original path of a rename, are both files
        // in the base tree.
        paths.insert(view.path.clone());
        if let Some(orig) = &view.orig_path {
            paths.insert(orig.clone());
        }
    }
    // A base tree we cannot enumerate means the bound cannot be established.
    // The observation carries the diff, not a tree listing, so a file that was
    // never touched contributes no path here — and correctly so, since an
    // untouched file cannot have lost a test.
    (paths, covered)
}

/// A test's identity, for comparing two trees.
///
/// **The file path is deliberately not part of it.** `#[test] fn alpha()` in
/// `src/http_v1.rs` and in `src/http_v1/tests.rs` is the *same* test, and
/// treating it as two would make a file-to-module split look like a deletion
/// plus an addition — the M5 dry-run false positive.
///
/// The AST module path *is* part of it, so two `#[test] fn works()` in
/// `mod a` and `mod b` stay distinct, and a deletion of either is a finding.
fn test_key(t: &TestFn) -> String {
    if t.path.is_empty() {
        t.name.clone()
    } else {
        format!("{}::{}", t.path, t.name)
    }
}

#[derive(Debug, Clone)]
struct Located {
    /// The file the test is in, for the finding's location.
    path: String,
    /// The AST module path, for the identity.
    module: String,
    name: String,
    ignored: bool,
    should_panic: bool,
}

impl Located {
    fn new(path: &str, t: &TestFn) -> Self {
        Self {
            path: path.to_string(),
            module: t.path.clone(),
            name: t.name.clone(),
            ignored: t.ignored,
            should_panic: t.should_panic,
        }
    }
}

fn base_test_names(key: &str, all: &BTreeMap<String, Vec<Located>>) -> Vec<String> {
    all.get(key)
        .map(|l| {
            l.iter()
                .map(|x| {
                    if x.module.is_empty() {
                        x.name.clone()
                    } else {
                        format!("{}::{}", x.module, x.name)
                    }
                })
                .collect()
        })
        .unwrap_or_else(|| vec![key.to_string()])
}

fn preview(items: &[String]) -> String {
    items.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
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
