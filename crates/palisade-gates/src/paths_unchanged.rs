//! `paths_unchanged` — were frozen paths touched?
//!
//! The narrowest gate in the suite, and the one with the fewest ways to be
//! wrong. It exists because "don't touch `fixtures/`" is a rule projects state
//! and cannot enforce, and a frozen directory that drifts is a benchmark that
//! quietly stops measuring anything.
//!
//! Path matching is prefix-based on `/`-separated components, not string
//! prefixes. `fixtures_extra/` is not inside `fixtures/`, and a gate that
//! cannot tell the difference is a gate that reports a false positive the
//! first time somebody has a similarly-named directory.

use crate::{GateContext, GateResult};

/// Whether `path` is `frozen` or inside it.
fn covered_by(path: &str, frozen: &str) -> bool {
    let p = normalise(path);
    let f = normalise(frozen);
    if f.is_empty() {
        return false;
    }
    p == f || p.starts_with(&format!("{f}/"))
}

fn normalise(p: &str) -> String {
    p.replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string()
}

/// The gate.
pub fn run(ctx: &GateContext<'_>) -> GateResult {
    if ctx.gate.paths.is_empty() {
        // A frozen-path gate with no paths declared is a configuration that
        // looks like a rule and enforces nothing. It says so rather than
        // passing.
        return GateResult::Untrustworthy(
            palisade_orchestrate::UntrustworthyReason::Indeterminate {
                detail: format!(
                    "gate `{}` is `paths_unchanged` but declares no paths, so it \
                 enforces nothing",
                    ctx.gate.id
                ),
            },
        );
    }

    let mut findings = Vec::new();
    for view in &ctx.observation.files {
        // A rename touches both the source and the destination.
        let mut candidates: Vec<&str> = vec![view.path.as_str()];
        if let Some(orig) = &view.orig_path {
            candidates.push(orig.as_str());
        }
        for path in candidates {
            for frozen in &ctx.gate.paths {
                if covered_by(path, frozen) {
                    findings.push(palisade_orchestrate::Finding::new(
                        ctx.gate.id.clone(),
                        ctx.gate.primitive,
                        ctx.gate.severity,
                        Some(path.into()),
                        None,
                        format!("`{frozen}` is frozen"),
                        "modified",
                        format!("frozen path `{path}` was touched"),
                        ctx.origin(),
                    ));
                }
            }
        }
    }

    GateResult::findings(findings)
}

#[cfg(test)]
mod tests {
    use super::covered_by;

    #[test]
    fn prefix_matching_is_component_wise() {
        assert!(covered_by("fixtures/a.rs", "fixtures"));
        assert!(covered_by("fixtures/deep/a.rs", "fixtures/"));
        assert!(covered_by("fixtures", "fixtures/"));
        // The false positive this exists to prevent.
        assert!(!covered_by("fixtures_extra/a.rs", "fixtures"));
        assert!(!covered_by("src/fixtures/a.rs", "fixtures"));
        assert!(!covered_by("a.rs", ""));
    }
}
