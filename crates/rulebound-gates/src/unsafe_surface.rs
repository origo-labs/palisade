//! `unsafe_surface_unchanged` — was unsafe surface added?
//!
//! Four kinds, reported separately because they have different consequences:
//! `unsafe` blocks, `unsafe fn`, `unsafe impl` and `extern` blocks. A count
//! that lumps them together hides which one grew, and "we added three unsafe
//! things" is not an actionable review comment.
//!
//! Growth is the trigger, not presence. A file that already had four unsafe
//! blocks and still has four is not a finding; a file that had four and now
//! has five is. `slop-gate` calls this `unsafe-surface-growth` for the same
//! reason: the question worth asking is *did this get worse*, which needs a
//! baseline.
//!
//! A file that does not parse is `Untrustworthy`, never a pass. A text-based
//! counter would either miss `unsafe` written oddly or, worse, count things
//! that are not code — and M1's `is_attr` bug is the argument against
//! counting by text when a parser is available.

use std::collections::BTreeMap;

use rulebound_ast::{ParseCache, ParsedFile, UnsafeKind};
use rulebound_orchestrate::{Finding, Side, Subject, SubjectKind, UntrustworthyReason};

use crate::{GateContext, GateResult, truncated};

/// The gate.
pub fn run(ctx: &GateContext<'_>) -> GateResult {
    let cache = ParseCache::new();
    let mut findings = Vec::new();

    for view in &ctx.observation.files {
        if !view.path.ends_with(".rs") {
            continue;
        }
        if view.truncated {
            return GateResult::Untrustworthy(truncated(&view.path));
        }
        let Some(head_src) = &view.head else { continue };
        let Ok(head) = parse(&cache, head_src, &view.path) else {
            return parse_failure(&cache, head_src, &view.path);
        };
        let head_counts = counts(&head);
        let base_counts = match view.base.as_deref() {
            Some(src) => {
                let Ok(base) = parse(&cache, src, &view.path) else {
                    return parse_failure(&cache, src, &view.path);
                };
                counts(&base)
            }
            // A new file with unsafe in it is all new surface.
            None => BTreeMap::new(),
        };

        for (kind, head_n) in &head_counts {
            let base_n = base_counts.get(kind).copied().unwrap_or(0);
            if *head_n <= base_n {
                continue;
            }
            findings.push(Finding::new(
                ctx.gate.id.clone(),
                ctx.gate.primitive,
                ctx.gate.severity,
                Subject::new(SubjectKind::UnsafeSurface, plural(*kind, *head_n)),
                Some(view.path.clone().into()),
                None,
                Side::counted(kind.as_str(), base_n),
                Side::counted(kind.as_str(), *head_n),
                format!(
                    "{} unsafe {} added to `{}`",
                    head_n - base_n,
                    plural(*kind, head_n - base_n),
                    view.path
                ),
                ctx.origin(),
            ));
        }
    }

    // A file with no `.rs` counterpart in the observation means no `.rs` file
    // changed between the two trees, so there is nothing to compare. Sound for
    // the same reason as the equivalent branch in
    // `dependency_surface_unchanged`: the view is built from the diff of the
    // *same two trees* this gate compares, so absence means "unchanged" rather
    // than "unexamined". An earlier version reported `Untrustworthy` here, and
    // a repository with a `target/` directory full of build output then failed
    // every run with "no .rs file in the observation" — a confident, wrong
    // error built on an absence that meant nothing.
    GateResult::findings(findings)
}

/// How many of each kind, for both sides of the comparison.
fn counts(f: &ParsedFile) -> BTreeMap<UnsafeKind, usize> {
    let mut out: BTreeMap<UnsafeKind, usize> = BTreeMap::new();
    for site in f.unsafe_sites() {
        *out.entry(site.kind).or_insert(0) += 1;
    }
    out
}

fn plural(kind: UnsafeKind, n: usize) -> String {
    if n == 1 {
        kind.as_str().to_string()
    } else {
        format!("{}s", kind.as_str())
    }
}

/// Parse a file, turning a refusal into the gate's `Untrustworthy` outcome.
///
/// A file with parser error nodes is not counted. Counting what happened to
/// parse is how a gate reports a number that looks measured and is not.
fn parse(cache: &ParseCache, source: &str, path: &str) -> Result<ParsedFile, UntrustworthyReason> {
    match cache.parse(source).as_ref() {
        Ok(f) => Ok(f.clone()),
        Err(e) => Err(UntrustworthyReason::Indeterminate {
            detail: format!("{path}: {e}"),
        }),
    }
}

/// The gate outcome for a file that could not be parsed.
fn parse_failure(cache: &ParseCache, source: &str, path: &str) -> GateResult {
    match parse(cache, source, path) {
        Ok(_) => GateResult::Untrustworthy(UntrustworthyReason::Indeterminate {
            detail: format!("{path}: inconsistent parse result"),
        }),
        Err(e) => GateResult::Untrustworthy(e),
    }
}
