//! `dependency_surface_unchanged` — did the declared production dependency
//! surface change?
//!
//! The base side of this comparison is `Cargo.toml` at the base commit, parsed
//! as TOML. **Not** a regex over diff text, and the reason is recorded:
//! `EVIDENCE.md` §6 describes a first implementation that scored 38% and missed
//! a dependency addition outright, because it stripped the diff's `+`/`-`
//! markers and then anchored its patterns on them.
//!
//! Scope, stated precisely because "dependency surface" is a slippery phrase.
//! This gate compares what the manifest *declares*:
//!
//! - `[dependencies]` and `[target.*.dependencies]` — production.
//! - `[features]` and each dependency's `default-features` flag.
//!
//! It deliberately does **not** compare `[dev-dependencies]`, which are not
//! production surface, nor the resolved graph in `Cargo.lock`. A direct TOML
//! parse is also what keeps this gate `Analyzed`: `cargo metadata` would be a
//! build-graph dependency and a subprocess, and the declared direct surface is
//! the thing a reviewer can actually see in a diff. If M5's corpus shows the
//! direct parse misses real production dependency changes, that is a
//! delegated addition via `palisade-exec` with a recorded calibration.

use std::collections::BTreeMap;

use palisade_orchestrate::{Finding, HunkRef, Side, Subject, SubjectKind, UntrustworthyReason};

use crate::{GateContext, GateResult, truncated};

/// One dependency as declared. Ordinal so a `Finding` is deterministic, which
/// is what lets the report be byte-stable across runs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Declared {
    /// Crate name as written in the manifest.
    name: String,
    /// Version requirement text, or the source specifier.
    requirement: String,
    /// Whether `default-features = false` was set.
    default_features: bool,
    /// The declared features.
    features: Vec<String>,
}

/// The production dependency surface of one manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Surface {
    dependencies: BTreeMap<String, Declared>,
    features: BTreeMap<String, Vec<String>>,
}

impl Surface {
    /// Flatten `[dependencies]` and `[target.*.dependencies]` into one map.
    fn parse(manifest: &str, path: &str) -> Result<Self, UntrustworthyReason> {
        let value: toml::Value =
            toml::from_str(manifest).map_err(|e| UntrustworthyReason::Indeterminate {
                detail: format!("{path} is not valid TOML: {e}"),
            })?;

        let mut surface = Self::default();
        if let Some(deps) = value.get("dependencies").and_then(toml::Value::as_table) {
            surface.dependencies.extend(declared_from(deps));
        }
        if let Some(features) = value.get("features").and_then(toml::Value::as_table) {
            for (name, value) in features {
                let list = value.as_array().map(|a| {
                    a.iter()
                        .filter_map(toml::Value::as_str)
                        .map(str::to_string)
                        .collect()
                });
                if let Some(list) = list {
                    surface.features.insert(name.clone(), list);
                }
            }
        }
        // `[target.*.dependencies]` is production surface for that target, so
        // it merges into the same map. `[target.*.dev-dependencies]` does not,
        // and is skipped by construction: only `dependencies` is read.
        if let Some(targets) = value.get("target").and_then(toml::Value::as_table) {
            for body in targets.values() {
                if let Some(deps) = body.get("dependencies").and_then(toml::Value::as_table) {
                    surface.dependencies.extend(declared_from(deps));
                }
            }
        }
        Ok(surface)
    }
}

/// Read a `[dependencies]`-shaped table into declarations.
///
/// A free function rather than a method so the caller can `extend` without
/// holding two mutable borrows of the same `Surface`.
fn declared_from(table: &toml::Table) -> BTreeMap<String, Declared> {
    let mut out = BTreeMap::new();
    {
        for (name, spec) in table {
            let (requirement, default_features, features) = match spec {
                toml::Value::String(v) => (v.clone(), true, Vec::new()),
                toml::Value::Table(t) => (
                    t.get("version")
                        .and_then(toml::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    t.get("default-features")
                        .and_then(toml::Value::as_bool)
                        .unwrap_or(true),
                    t.get("features")
                        .and_then(toml::Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(toml::Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
                // An inline table reaches here only from a programmatically
                // built value; a parsed manifest always yields `Table`.
                other => (other.to_string(), true, Vec::new()),
            };
            out.insert(
                name.clone(),
                Declared {
                    name: name.clone(),
                    requirement,
                    default_features,
                    features,
                },
            );
        }
    }
    out
}

/// The gate.
pub fn run(ctx: &GateContext<'_>) -> GateResult {
    let manifest_path = "Cargo.toml";
    let Some(view) = ctx.observation.file(manifest_path) else {
        // Not in the two-tree view, therefore identical in both trees,
        // therefore the production dependency surface did not change.
        //
        // That reasoning is only sound because the view is built from the diff
        // of the *same two trees* this gate compares — `git diff --name-status
        // <base>`, plus untracked paths. Absence means "unchanged", not
        // "unexamined". It would be unsound if the view were built from
        // `git status`, which is worktree-versus-index and blind to committed
        // work; an earlier version did exactly that, and this branch then
        // reported `Untrustworthy` on every repository whose manifest had not
        // been touched by the current edit. The end-to-end test
        // `an_unchanged_repository_produces_no_findings_from_any_m1_gate`
        // pins the difference.
        return GateResult::Clean;
    };
    if view.truncated {
        return GateResult::Untrustworthy(truncated(manifest_path));
    }

    let head = match &view.head {
        Some(h) => h,
        None => return GateResult::Clean, // deleted: not an addition
    };
    let head_surface = match Surface::parse(head, manifest_path) {
        Ok(s) => s,
        Err(e) => return GateResult::Untrustworthy(e),
    };

    // No base side means the manifest is new. Its whole surface is new, and a
    // brand-new project is not a dependency-surface *change* to be blocked on
    // — but it is still worth reporting, so a new manifest is reported once
    // rather than once per dependency.
    let Some(base) = &view.base else {
        return GateResult::findings(vec![Finding::new(
            ctx.gate.id.clone(),
            ctx.gate.primitive,
            ctx.gate.severity,
            Subject::new(SubjectKind::File, manifest_path),
            Some(manifest_path.into()),
            None,
            Side::Absent,
            Side::counted("production dependency", head_surface.dependencies.len()),
            "Cargo.toml is new; its whole dependency surface is unreviewed",
            ctx.origin(),
        )]);
    };

    let base_surface = match Surface::parse(base, manifest_path) {
        Ok(s) => s,
        Err(e) => return GateResult::Untrustworthy(e),
    };

    let mut findings = Vec::new();
    // `allow` entries are exact matches, not globs: a blanket `"*"` would be
    // indistinguishable from having no gate at all.
    let allowed: Vec<&str> = ctx.gate.allow.iter().map(String::as_str).collect();

    // Additions and removals.
    for (name, declared) in &head_surface.dependencies {
        if !base_surface.dependencies.contains_key(name) {
            if allowed.contains(&name.as_str()) {
                continue;
            }
            findings.push(dep_finding(
                ctx,
                manifest_path,
                name,
                Side::Absent,
                Side::value(describe(declared)),
                &format!("production dependency `{name}` was added"),
            ));
        }
    }
    for (name, declared) in &base_surface.dependencies {
        if !head_surface.dependencies.contains_key(name) {
            findings.push(dep_finding(
                ctx,
                manifest_path,
                name,
                Side::value(describe(declared)),
                Side::Absent,
                &format!("production dependency `{name}` was removed"),
            ));
        }
    }

    // Changes to an existing dependency: version, default-features, features.
    // This is the part a manifest *diff* misses, which is why `slop-gate`
    // generalises the rule to surface rather than to "did the manifest
    // change" (REFERENCE-slop-gate.md).
    for (name, head_decl) in &head_surface.dependencies {
        let Some(base_decl) = base_surface.dependencies.get(name) else {
            continue;
        };
        for (what, before, after) in differences(base_decl, head_decl) {
            if allowed
                .iter()
                .any(|a| *a == format!("{name}:{what}").as_str())
            {
                continue;
            }
            findings.push(dep_finding(
                ctx,
                manifest_path,
                name,
                Side::value(before),
                Side::value(after),
                &format!("production dependency `{name}` changed its {what}"),
            ));
        }
    }

    // Feature table changes.
    for (name, head_list) in &head_surface.features {
        match base_surface.features.get(name) {
            None => findings.push(feature_finding(
                ctx,
                manifest_path,
                name,
                Side::Absent,
                Side::value(render(head_list)),
            )),
            Some(base_list) if base_list != head_list => findings.push(feature_finding(
                ctx,
                manifest_path,
                name,
                Side::value(render(base_list)),
                Side::value(render(head_list)),
            )),
            Some(_) => {}
        }
    }
    for (name, base_list) in &base_surface.features {
        if !head_surface.features.contains_key(name) {
            findings.push(feature_finding(
                ctx,
                manifest_path,
                name,
                Side::value(render(base_list)),
                Side::Absent,
            ));
        }
    }

    GateResult::findings(findings)
}

/// Field-level differences between two declarations of the same dependency.
fn differences(base: &Declared, head: &Declared) -> Vec<(&'static str, String, String)> {
    let mut out = Vec::new();
    if base.requirement != head.requirement {
        out.push((
            "version requirement",
            base.requirement.clone(),
            head.requirement.clone(),
        ));
    }
    if base.default_features != head.default_features {
        out.push((
            "default-features",
            base.default_features.to_string(),
            head.default_features.to_string(),
        ));
    }
    if base.features != head.features {
        out.push(("features", render(&base.features), render(&head.features)));
    }
    out
}

fn describe(d: &Declared) -> String {
    format!(
        "{}{}{}",
        d.requirement,
        if d.default_features {
            " (default features)"
        } else {
            ""
        },
        if d.features.is_empty() {
            String::new()
        } else {
            format!(" features={}", render(&d.features))
        }
    )
}

fn render(features: &[String]) -> String {
    if features.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", features.join(", "))
    }
}

fn dep_finding(
    ctx: &GateContext<'_>,
    path: &str,
    name: &str,
    expected: Side,
    observed: Side,
    message: &str,
) -> Finding {
    Finding::new(
        ctx.gate.id.clone(),
        ctx.gate.primitive,
        ctx.gate.severity,
        Subject::new(SubjectKind::Dependency, name),
        Some(path.into()),
        Some(HunkRef { start: 0, end: 0 }),
        expected,
        observed,
        message,
        ctx.origin(),
    )
}

fn feature_finding(
    ctx: &GateContext<'_>,
    path: &str,
    name: &str,
    expected: Side,
    observed: Side,
) -> Finding {
    Finding::new(
        ctx.gate.id.clone(),
        ctx.gate.primitive,
        ctx.gate.severity,
        Subject::new(SubjectKind::Feature, format!("features.{name}")),
        Some(path.into()),
        Some(HunkRef { start: 0, end: 0 }),
        expected,
        observed,
        format!("[features].{name} {}", "changed"),
        ctx.origin(),
    )
}
