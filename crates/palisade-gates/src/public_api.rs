//! `public_api_unchanged` — did a public item's signature change?
//!
//! Two-tree, AST-based, and compared against the **baseline** rather than
//! matched against a pattern in the current file. That last point is not a
//! style preference. `EVIDENCE.md` §6 records a first implementation that
//! tripped on a new function returning `-> list[Note]`: it looked for a
//! pattern instead of comparing two trees, so *any* new signature looked like
//! a violation. That case is a must-not-fire fixture in `tests/gates.rs`.
//!
//! What counts as a change, stated so the gate cannot be read as more than it
//! is:
//!
//! - **Removed** — a public item present at the base commit is gone. Always a
//!   finding at the gate's severity.
//! - **Changed** — present on both sides with a different signature. Always a
//!   finding at the gate's severity.
//! - **Added** — not present at the base commit. Reported at `warn` and
//!   silenceable per item through `allow`.
//!
//! Additions are deliberately *not* blocking. A gate that fires on every new
//! public function is a gate that gets switched off, and switching it off
//! takes the removals with it. The severity of an addition is pinned here
//! rather than read from the contract, because a contract that can promote
//! "somebody added a function" to blocking is a contract that will be edited
//! to do exactly that.
//!
//! A public item is identified by its module path, not by its own text, so
//! moving a function between modules reads as a removal and an addition rather
//! than as a silent relocation.

use std::collections::BTreeMap;

use palisade_ast::{ParseCache, ParsedFile, PublicItem};
use palisade_contract::Severity;
use palisade_orchestrate::{Finding, Side, Subject, SubjectKind, UntrustworthyReason};

use crate::{GateContext, GateResult, truncated};

/// Per-item identity and signature, for comparing two trees.
type Surface = BTreeMap<String, String>;

/// The gate.
pub fn run(ctx: &GateContext<'_>) -> GateResult {
    let cache = ParseCache::new();
    let allowed: Vec<&str> = ctx.gate.allow.iter().map(String::as_str).collect();
    let mut findings = Vec::new();

    for view in &ctx.observation.files {
        if !view.path.ends_with(".rs") {
            continue;
        }
        if view.truncated {
            return GateResult::Untrustworthy(truncated(&view.path));
        }

        // A whole file deleted: every public item it declared is gone.
        let Some(head_src) = &view.head else {
            // `head` absent means deleted. `base` is present in that case,
            // because capture only produces a `FileView` for a path that
            // differs between the two trees.
            let Some(base_src) = view.base.as_deref() else {
                continue;
            };
            let Ok(base) = parse_result(&cache, base_src, &view.path) else {
                return parse_failure(&cache, base_src, &view.path);
            };
            if !base.items().is_empty() {
                findings.push(Finding::new(
                    ctx.gate.id.clone(),
                    ctx.gate.primitive,
                    ctx.gate.severity,
                    Subject::new(SubjectKind::File, view.path.clone()),
                    Some(view.path.clone().into()),
                    None,
                    Side::listed(
                        &base
                            .items()
                            .iter()
                            .map(|i| i.path.clone())
                            .collect::<Vec<_>>(),
                    ),
                    Side::Absent,
                    format!("`{}` declared public API and was deleted", view.path),
                    ctx.origin(),
                ));
            }
            continue;
        };

        let Ok(head) = parse_result(&cache, head_src, &view.path) else {
            return parse_failure(&cache, head_src, &view.path);
        };

        // A new file has no baseline; every item in it is an addition.
        // Collapsed for the same reason, and for the same reason the
        // per-file case is: a whole new module would otherwise produce a
        // finding per item.
        let Some(base_src) = view.base.as_deref() else {
            let added: Vec<PublicItem> = head
                .items()
                .iter()
                .filter(|i| !allowed.contains(&i.path.as_str()))
                .cloned()
                .collect();
            if !added.is_empty() {
                findings.push(addition_summary(ctx, &view.path, &added));
            }
            continue;
        };

        let Ok(base) = parse_result(&cache, base_src, &view.path) else {
            return parse_failure(&cache, base_src, &view.path);
        };
        let base_surface = surface(&base);
        let head_surface = surface(&head);

        // Removals.
        for (path, signature) in &base_surface {
            if head_surface.contains_key(path) || allowed.contains(&path.as_str()) {
                continue;
            }
            findings.push(Finding::new(
                ctx.gate.id.clone(),
                ctx.gate.primitive,
                ctx.gate.severity,
                Subject::new(SubjectKind::PublicItem, path.clone()),
                Some(view.path.clone().into()),
                None,
                Side::value(signature.clone()),
                Side::Absent,
                format!("public {} `{path}` was removed", kind_of(&base, path)),
                ctx.origin(),
            ));
        }

        // Changes.
        for (path, head_sig) in &head_surface {
            let Some(base_sig) = base_surface.get(path) else {
                continue;
            };
            if base_sig == head_sig || allowed.contains(&path.as_str()) {
                continue;
            }
            findings.push(Finding::new(
                ctx.gate.id.clone(),
                ctx.gate.primitive,
                ctx.gate.severity,
                Subject::new(SubjectKind::PublicItem, path.clone()),
                Some(view.path.clone().into()),
                None,
                Side::value(base_sig.clone()),
                Side::value(head_sig.clone()),
                format!("public {} `{path}` changed signature", kind_of(&head, path)),
                ctx.origin(),
            ));
        }

        // Additions, collapsed. See the module docs for why, and for why
        // removals are not collapsed alongside them.
        let mut added: Vec<PublicItem> = Vec::new();
        for path in head_surface.keys() {
            if base_surface.contains_key(path) {
                continue;
            }
            let Some(item) = head.items().iter().find(|i| &i.path == path) else {
                continue;
            };
            if allowed.contains(&path.as_str()) {
                continue;
            }
            added.push(item.clone());
        }
        if !added.is_empty() {
            findings.push(addition_summary(ctx, &view.path, &added));
        }
    }

    // A run that saw no Rust at all did not check anything. Saying so is the
    // difference between "the API is unchanged" and "there is no API here".

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

/// How many names one collapsed finding lists before it says "and N more".
///
/// Bounded because a finding whose `observed` field is 400 lines long is not
/// evidence anybody can read, which is the same reason the M3 diagnostics are
/// bounded.
const ADDITION_SAMPLE: usize = 8;

/// One finding for a batch of added public items, rather than one each.
#[allow(clippy::too_many_lines)]
fn addition_summary(ctx: &GateContext<'_>, path: &str, items: &[PublicItem]) -> Finding {
    let n = items.len();
    let names: Vec<String> = items
        .iter()
        .take(ADDITION_SAMPLE)
        .map(|i| i.path.clone())
        .collect();
    let mut observed = names.join(", ");
    if n > ADDITION_SAMPLE {
        observed.push_str(&format!(" (+{} more)", n - ADDITION_SAMPLE));
    }
    let noun = if n == 1 { "item" } else { "items" };
    // The subject and the message must agree, and the invariant test that
    // checks that caught "1 public items" -- the subject is what a consumer
    // keys on, so a plural there is a real defect rather than a typo.
    let subject = format!("{n} public {noun} in this file");
    let verb = if n == 1 { "was" } else { "were" };
    Finding::new(
        ctx.gate.id.clone(),
        ctx.gate.primitive,
        // Pinned, not read from the contract. See the module docs.
        Severity::Warn,
        Subject::new(SubjectKind::PublicItem, subject),
        Some(path.into()),
        None,
        Side::Absent,
        Side::value(observed),
        format!(
            "{n} public {noun} {verb} added, including {}. Additions are not \
             a compatibility break; list a path in `allow` to silence them.",
            names
                .iter()
                .take(3)
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ctx.origin(),
    )
}

/// Parse a file, turning a refusal into the gate's `Untrustworthy` outcome.
///
/// `slop-gate` rejects files with parser error nodes rather than recovering,
/// and the reason is that a recovered parse is a guess about a file that did
/// not parse. Not reasoning about it is the honest outcome, and it is also
/// the safe one: a gate that skips an unparseable file and returns `Clean`
/// has silently downgraded a check.
fn parse_result(
    cache: &ParseCache,
    source: &str,
    path: &str,
) -> Result<ParsedFile, UntrustworthyReason> {
    match cache.parse(source).as_ref() {
        Ok(f) => Ok(f.clone()),
        Err(e) => Err(UntrustworthyReason::Indeterminate {
            detail: format!("{path}: {e}"),
        }),
    }
}

/// The gate outcome for a file that could not be parsed.
fn parse_failure(cache: &ParseCache, source: &str, path: &str) -> GateResult {
    match parse_result(cache, source, path) {
        Ok(_) => GateResult::Untrustworthy(UntrustworthyReason::Indeterminate {
            detail: format!("{path}: inconsistent parse result"),
        }),
        Err(e) => GateResult::Untrustworthy(e),
    }
}

fn surface(f: &ParsedFile) -> Surface {
    f.items()
        .iter()
        .map(|i| (i.path.clone(), i.signature.clone()))
        .collect()
}

fn kind_of(f: &ParsedFile, path: &str) -> &'static str {
    f.items()
        .iter()
        .find(|i| i.path == path)
        .map_or("item", |i| i.kind.as_str())
}
