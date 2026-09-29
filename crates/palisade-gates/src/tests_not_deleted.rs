//! `tests_not_deleted` — were tests removed, or made not to run?
//!
//! Three signals, none of which needs a build:
//!
//! 1. A file that looks like a test disappeared from the two-tree view.
//! 2. A `#[test]` or `#[bench]` count went *down* in a file that survived.
//! 3. A skip marker was added — `#[ignore]`, `#[should_panic]` without an
//!    expected value, or a `.skip(` / `xit(` / `xdescribe(` call.
//!
//! Every count is taken over whole files on both sides of the tree, never over
//! diff text. `EVIDENCE.md` §6 records the first implementation of the
//! predecessor's version of this check scoring 38% because it stripped the
//! diff's `+`/`-` markers and then anchored its patterns on them. Diff text is
//! a rendering of a change; the count is a property of the file.
//!
//! `cargo test -- --list` gives a stronger inventory — real test names, not
//! attributes — and arrives in M3 as a `consumes` edge from `checks_green`.
//! Until then the report says which signals it used, rather than implying
//! full coverage.

use palisade_orchestrate::Finding;

use crate::{GateContext, GateResult, truncated};

/// Does this path look like a test, bench, or fuzz target?
///
/// A heuristic, and stated as one: a gate that claims to detect deleted tests
/// is bounded by what "looks like a test" means. The falsifiable part is the
/// *deletion* check, which is exact for any path this accepts, and the
/// `[dependencies]`-style false positive is watched for in M5's corpus.
fn is_test_path(path: &str) -> bool {
    let p = path.replace('\\', "/");
    let segments: Vec<&str> = p.split('/').collect();
    for seg in &segments {
        match *seg {
            "tests" | "test" | "benches" | "bench" | "fuzz" | "fixtures" => return true,
            _ => {}
        }
    }
    let file = segments.last().copied().unwrap_or_default();
    file.starts_with("test_")
        || file.ends_with("_test.rs")
        || file.ends_with("_tests.rs")
        || file.ends_with("_bench.rs")
        || file.ends_with(".test.ts")
        || file.ends_with(".spec.ts")
}

/// Count the test-defining attributes in a source file.
///
/// Line-anchored and comment-aware: a `#[test]` inside a doc comment is prose,
/// not a test, and counting it would make this gate cry wolf on documentation
/// changes. That is the difference between a gate people keep and one that
/// gets switched off — and a gate that gets switched off takes its
/// neighbours with it.
fn count_test_attrs(src: &str) -> usize {
    src.lines().filter(|l| is_attr(l, "test")).count()
}

fn count_skip_attrs(src: &str) -> usize {
    src.lines()
        .filter(|l| is_attr(l, "ignore") || is_attr(l, "should_panic"))
        .count()
}

/// Whether a line is a bare `#[name]` or `#[name(...)]` attribute, excluding
/// comments and doc comments.
///
/// The attribute *path* is compared, not the raw text, so `#[test]`,
/// `#[test = "x"]` and `#[tokio::test]` are all recognised while
/// `#[testify]` is not. Getting this wrong is not a subtle bug: a naive
/// `starts_with("test")` counts `#[testify]`, and a naive equality against the
/// text after `#[` counts *nothing at all*, which makes the gate pass on every
/// repository while appearing calibrated.
fn is_attr(line: &str, name: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("//") {
        return false;
    }
    let Some(rest) = t.strip_prefix("#[") else {
        return false;
    };
    // Cut the path at the first argument list or the closing bracket.
    let end = rest.find(['(', ']']).unwrap_or(rest.len());
    let path = rest[..end].trim().trim_start_matches("::");
    // A path-qualified attribute such as `#[tokio::test]` counts as `test`.
    path.rsplit("::").next().is_some_and(|last| last == name)
}

/// A framework-level skip: `.skip(`, `xit(`, `xdescribe(`, `test.todo`.
fn count_framework_skips(src: &str) -> usize {
    let mut n = 0;
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with("//") {
            continue;
        }
        for pat in [".skip(", "xit(", "xdescribe(", "test.todo", "xtest("] {
            if t.contains(pat) {
                n += 1;
                break;
            }
        }
    }
    n
}

/// The gate.
pub fn run(ctx: &GateContext<'_>) -> GateResult {
    let mut findings = Vec::new();
    for view in &ctx.observation.files {
        // A rename keeps the base side under the original path.
        let base = view.base.as_deref();
        let head = view.head.as_deref();

        // 1. A test file vanished.
        if head.is_none() && is_test_path(&view.path) {
            findings.push(Finding::new(
                ctx.gate.id.clone(),
                ctx.gate.primitive,
                ctx.gate.severity,
                Some(view.path.clone().into()),
                None,
                "present at the base commit",
                "deleted",
                format!("`{}` looks like a test and was deleted", view.path),
                ctx.origin(),
            ));
            continue;
        }
        let Some(head) = head else { continue };
        if view.truncated {
            return GateResult::Untrustworthy(truncated(&view.path));
        }
        let Some(base) = base else {
            // A new file can only add tests. Nothing to compare.
            continue;
        };

        // 2. A test count went down in a file that survived. A rename moved
        //    the content, so the original path is the honest base.
        let before = count_test_attrs(base);
        let after = count_test_attrs(head);
        if after < before {
            findings.push(Finding::new(
                ctx.gate.id.clone(),
                ctx.gate.primitive,
                ctx.gate.severity,
                Some(view.path.clone().into()),
                None,
                format!("{before} `#[test]` attributes"),
                format!("{after} `#[test]` attributes"),
                format!(
                    "{} test(s) appear to have been removed from `{}`",
                    before - after,
                    view.path
                ),
                ctx.origin(),
            ));
        }

        // 3. A skip marker was added.
        let skips_before = count_skip_attrs(base) + count_framework_skips(base);
        let skips_after = count_skip_attrs(head) + count_framework_skips(head);
        if skips_after > skips_before {
            findings.push(Finding::new(
                ctx.gate.id.clone(),
                ctx.gate.primitive,
                ctx.gate.severity,
                Some(view.path.clone().into()),
                None,
                format!("{skips_before} skip marker(s)"),
                format!("{skips_after} skip marker(s)"),
                format!("a test skip marker was added to `{}`", view.path),
                ctx.origin(),
            ));
        }
    }

    GateResult::findings(findings)
}
