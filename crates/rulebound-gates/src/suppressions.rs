//! `suppressions_not_widened` — were diagnostic suppressions broadened?
//!
//! This is the gate that audits the other tools. A worker optimising against
//! the gate suite rather than against intent will not remove a requirement —
//! it will add `#[allow]`, and this is the rule that sees it. `slop-gate`
//! calls it `lint-suppression-growth` and treats it as the elegant answer to
//! the Goodhart objection.
//!
//! The trigger is **widening**, not presence. A suppression that is already
//! there is not a finding; a suppression that gained a lint, or gained scope,
//! is. A new `#[allow]` on a function that had none is treated as widening
//! from nothing, which is the common case and the one worth catching.
//!
//! Broadening *to a wildcard* — `clippy::all`, `warnings` — is reported
//! separately from broadening a specific list, because it is a different
//! magnitude of event. Turning off every Clippy lint for a module is not the
//! same as adding one lint to an existing allow, and a report that conflates
//! them is a report nobody triages.
//!
//! Narrowing, and removing a suppression entirely, are improvements and are
//! never findings. A gate that fires on those is a gate that makes the code
//! worse.

use std::collections::BTreeMap;

use rulebound_ast::{ParseCache, ParsedFile, Suppression};
use rulebound_orchestrate::{Finding, Side, Subject, SubjectKind, UntrustworthyReason};

use crate::{GateContext, GateResult, truncated};

/// A suppression keyed by where it sits, for comparing two trees.
type Surface = BTreeMap<SuppressionKey, Suppression>;

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
        let head_surface = surface(&head);
        let base_surface = match view.base.as_deref() {
            Some(src) => {
                let Ok(base) = parse(&cache, src, &view.path) else {
                    return parse_failure(&cache, src, &view.path);
                };
                surface(&base)
            }
            None => BTreeMap::new(),
        };

        // Added or widened.
        for (key, head_s) in &head_surface {
            let Some(base_s) = base_surface.get(key) else {
                findings.push(Finding::new(
                    ctx.gate.id.clone(),
                    ctx.gate.primitive,
                    ctx.gate.severity,
                    // The subject names the suppression, not the map key. The
                    // key exists to match two trees together and leaks
                    // implementation shape; a subject of `::allow` tells a
                    // reader nothing, and the message would then be about the
                    // file while the subject was about the key.
                    Subject::new(SubjectKind::Suppression, render(head_s)),
                    Some(view.path.clone().into()),
                    None,
                    Side::Absent,
                    Side::value(render(head_s)),
                    message_for_addition(&view.path, head_s),
                    ctx.origin(),
                ));
                continue;
            };
            if let Some(grown) = widened(base_s, head_s) {
                findings.push(Finding::new(
                    ctx.gate.id.clone(),
                    ctx.gate.primitive,
                    ctx.gate.severity,
                    Subject::new(SubjectKind::Suppression, render(head_s)),
                    Some(view.path.clone().into()),
                    None,
                    Side::value(render(base_s)),
                    Side::value(render(head_s)),
                    grown,
                    ctx.origin(),
                ));
            }
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

/// How a suppression was widened, or `None` if it was not.
///
/// Two ways to widen, checked in order of severity: more lints, or the same
/// lints but a broader attribute. Going from `#[allow(clippy::foo)]` to
/// `#[expect(clippy::foo, bar)]` is widening; going to a *narrower* list is
/// not, and neither is dropping the attribute.
fn widened(base: &Suppression, head: &Suppression) -> Option<String> {
    // A wildcard subsumes everything, so it is the widest state.
    if head.is_wildcard() && !base.is_wildcard() {
        let target = if head.module_wide {
            "a whole module"
        } else {
            "an item"
        };
        return Some(format!(
            "a diagnostic suppression was broadened to a whole tool's lints on {target}"
        ));
    }
    let added: Vec<&String> = head
        .lints
        .iter()
        .filter(|l| !base.lints.contains(l))
        .collect();
    if !added.is_empty() {
        let names = added
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Some(format!(
            "a diagnostic suppression was widened to include {names}"
        ));
    }
    // Same lints, wider scope.
    if head.module_wide && !base.module_wide {
        return Some("a diagnostic suppression was moved onto a whole module".to_string());
    }
    None
}

/// The message names the suppression, not just its category. A reader who has
/// to cross-reference the subject field to learn *which* allow was added is
/// doing the report's job by hand.
fn message_for_addition(path: &str, s: &Suppression) -> String {
    if s.is_wildcard() {
        format!(
            "a wildcard diagnostic suppression `{}` was added to `{path}`",
            render(s)
        )
    } else {
        format!(
            "a diagnostic suppression `{}` was added to `{path}`",
            render(s)
        )
    }
}

/// Kept for the match between two trees. Not a subject name: this is a lookup
/// key, and a reader should never see it.
type SuppressionKey = String;

/// The lookup key for a suppression: where it sits, and which attribute it is.
fn suppression_key(path: &str, attribute: &str) -> SuppressionKey {
    if path.is_empty() {
        attribute.to_string()
    } else {
        format!("{path}::{attribute}")
    }
}

/// Parse a file, turning a refusal into the gate's `Untrustworthy` outcome.
fn parse(cache: &ParseCache, source: &str, path: &str) -> Result<ParsedFile, UntrustworthyReason> {
    match cache.parse(source).as_ref() {
        Ok(f) => Ok(f.clone()),
        Err(e) => Err(UntrustworthyReason::Indeterminate {
            detail: format!("{path}: {e}"),
        }),
    }
}

/// The gate outcome for a file that could not be parsed.
///
/// A file with error nodes has no dependable answer, so the gate says it could
/// not tell rather than counting the suppressions it happened to see.
fn parse_failure(cache: &ParseCache, source: &str, path: &str) -> GateResult {
    match parse(cache, source, path) {
        Ok(_) => GateResult::Untrustworthy(UntrustworthyReason::Indeterminate {
            detail: format!("{path}: inconsistent parse result"),
        }),
        Err(e) => GateResult::Untrustworthy(e),
    }
}

/// Keyed by path *and* attribute, so `#[allow]` and `#[expect]` on the same
/// item are two suppressions rather than one overwriting the other.
fn surface(f: &ParsedFile) -> Surface {
    f.suppressions()
        .iter()
        .map(|s| (suppression_key(&s.path, &s.attribute), s.clone()))
        .collect()
}

fn render(s: &Suppression) -> String {
    format!(
        "#[{}({})]{}",
        s.attribute,
        s.lints.join(", "),
        if s.module_wide { " on a module" } else { "" }
    )
}
