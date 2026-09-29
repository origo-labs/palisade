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

use palisade_ast::{ParseCache, ParsedFile, Suppression};
use palisade_orchestrate::{Finding, UntrustworthyReason};

use crate::{GateContext, GateResult, truncated};

/// A suppression keyed by where it sits, for comparing two trees.
type Surface = BTreeMap<String, Suppression>;

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
                    Some(view.path.clone().into()),
                    None,
                    "no suppression here at the base commit",
                    render(head_s),
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
                    Some(view.path.clone().into()),
                    None,
                    render(base_s),
                    render(head_s),
                    grown,
                    ctx.origin(),
                ));
            }
        }
    }

    if !saw_rust && !ctx.observation.files.is_empty() {
        return GateResult::Untrustworthy(UntrustworthyReason::Indeterminate {
            detail: "no `.rs` file in the observation, so no suppressions to compare".to_string(),
        });
    }

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

fn message_for_addition(path: &str, s: &Suppression) -> String {
    if s.is_wildcard() {
        format!("a wildcard diagnostic suppression was added to `{path}`")
    } else {
        format!("a diagnostic suppression was added to `{path}`")
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
        .map(|s| (format!("{}::{}", s.path, s.attribute), s.clone()))
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
