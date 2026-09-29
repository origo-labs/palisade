//! Contract generation — what `palisade init` writes.
//!
//! ## Why this is the product's main act
//!
//! The retrofit story asks a project to author a quality contract before it has
//! ever seen the tool. A screen of twelve popular Rust repositories found that
//! none of them write down code-quality rules at all, so that first run is:
//! install, discover you have no contract, spend a day writing one, find most
//! of it is `not_covered`, give up. That is a bad funnel, and it is a *typical*
//! one rather than a run of bad luck.
//!
//! So the contract is generated, and the project edits it. The artefact nobody
//! has — a dated, owned statement of what the bar is and what it does not
//! cover — is produced for them, and `not_covered` is filled in honestly
//! rather than left empty.
//!
//! ## Every gate at `warn`, and why
//!
//! A generated contract proposes; it does not judge. `warn` means "this is
//! reported and nothing blocks", which is exactly the right posture for gates
//! nobody has calibrated. A fresh contract at `error` would be a claim nobody
//! has measured, and the promotion guard added in M4 would (correctly) refuse
//! it.
//!
//! The generated file says so at the top, because a reader who assumes
//! otherwise will assume the gates are advisory over everybody's code.

use crate::{GateId, Primitive, Severity};

/// The conventional frozen paths a greenfield Rust project probably has.
///
/// Seeded so `paths_unchanged` does something meaningful on day one. A path
/// that is not present costs nothing: the gate only fires on paths that are
/// actually touched.
pub const CONVENTIONAL_FROZEN_PATHS: &[&str] = &["fixtures", "testdata", "benches", "examples"];

/// The gap a generated contract admits to on a greenfield project.
///
/// Populated rather than left empty, because PRD 9.4 treats an empty list as a
/// claim the tool does not make, and a generated contract that started by
/// claiming completeness would be the manufactured confidence the whole
/// `judgement` section exists to prevent. Each entry is a real thing this
/// suite cannot check, and each is a thing a human still has to look at.
pub const HONEST_GAPS: &[&str] = &[
    "whether the chosen design is the right one for the problem",
    "whether error messages are good enough for an end user",
    "whether an added benchmark is a fair one",
    "whether the code is understandable to someone who did not write it",
    "whether a test asserts the behaviour it claims to assert",
];

/// A gate the generated contract includes, and the comment above it.
struct Entry {
    primitive: Primitive,
    /// Why a project might turn it off, or what it costs. Rendered into the
    /// generated file so nobody has to run the tool to find out.
    note: &'static str,
}

/// The full menu, in report order.
fn menu() -> Vec<Entry> {
    vec![
        Entry {
            primitive: Primitive::ChecksGreen,
            note: "runs `cargo fmt`, `clippy` and `test`. The most expensive \
                   gate in the suite; it costs about what your CI already \
                   spends on the same three commands",
        },
        Entry {
            primitive: Primitive::DependencySurfaceUnchanged,
            note: "a declared production dependency changed. Also needs \
                   `checks_green` if you want the new dependency to build",
        },
        Entry {
            primitive: Primitive::TestsNotDeleted,
            note: "a test disappeared, or gained `#[ignore]`. Add \
                   `consumes = [\"checks_green\"]` to also catch a test the \
                   source has but the build does not run",
        },
        Entry {
            primitive: Primitive::PublicApiUnchanged,
            note: "a public item's signature changed or was removed. \
                   Additions are reported at `warn` and never block",
        },
        Entry {
            primitive: Primitive::UnsafeSurfaceUnchanged,
            note: "`unsafe` blocks, functions, impls or extern blocks were \
                   added. Growth is the trigger, not presence",
        },
        Entry {
            primitive: Primitive::SuppressionsNotWidened,
            note: "an `#[allow]` was added or broadened, including to a \
                   whole tool's lints with `clippy::all`",
        },
        Entry {
            primitive: Primitive::PathsUnchanged,
            note: "a conventionally frozen path was touched. Edit `paths` \
                   below to change what is frozen",
        },
        // `secret_absent` and `external_tool` are deliberately NOT in the
        // default contract. A generated file that declares an unimplemented
        // primitive cannot pass its own first run: the gate reports
        // `Untrustworthy` and the verdict is `error` before anybody has
        // changed a line. That is the contract claiming a completeness it
        // does not have, shipped by the generator that exists to be honest.
        //
        // They are listed in the file as *commented examples* instead, so the
        // gap is visible and a project can turn one on knowing it will report
        // `error` until it is implemented.
    ]
}

/// The gate id a primitive gets in a generated contract.
///
/// Snake-cased, because a gate id is read by people in pull requests and
/// typed into suppression records.
pub fn gate_id_for(primitive: Primitive) -> String {
    primitive.as_str().to_string()
}

/// The generated contract, as TOML text.
///
/// # Arguments
///
/// `reviewed` is the review date, supplied by the caller so this function
/// stays pure and a test can pin it. `is_new` distinguishes a project that has
/// never run a gate suite from one that has, which changes only the wording
/// of the header.
pub fn generate(reviewed: &str, workspace_members: usize) -> String {
    let mut out = String::new();
    out.push_str(GENERATED_HEADER);
    if workspace_members > 1 {
        out.push_str(&format!(
            "\n# Detected a workspace with {workspace_members} members. The gates below apply to\n\
             # the whole tree; a workspace often wants a stricter contract per member, which\n\
             # means splitting this file and running the suite per directory.\n"
        ));
    }
    out.push_str(GENERATED_SEVERITY_NOTE);
    out.push_str("\nversion = 1\n");
    out.push_str("\n# The commit every two-tree gate compares against. Override per run with\n# `palisade check --base <ref>`.\n");
    out.push_str("baseline = { ref = \"HEAD\" }\n");
    out.push_str("\n[budget]\n# Bound on the observation. Below 1024 is a validation error: a truncated\n# observation that small is not evidence of anything.\nobservation_bytes = 131072\n");

    for entry in menu() {
        let id = gate_id_for(entry.primitive);
        out.push_str(&format!("\n[[gates]]\n# {}\nid = \"{id}\"\n", entry.note));
        out.push_str(&format!("check = \"{}\"\n", entry.primitive));
        // Every gate starts at `warn`, and the reason is in the file rather
        // than in a comment in this generator.
        out.push_str(&format!("severity = \"{}\"\n", Severity::Warn));
        if entry.primitive == Primitive::PathsUnchanged {
            let paths: Vec<String> = CONVENTIONAL_FROZEN_PATHS
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect();
            out.push_str(&format!("paths = [{}]\n", paths.join(", ")));
        }
    }

    out.push_str(UNIMPLEMENTED_NOTE);
    out.push_str("\n# -------------------------------------------------------------------------\n");
    out.push_str("# The gap. Mandatory, and deliberately not empty: these are real things this\n");
    out.push_str("# suite cannot check, and a human still has to look at them. A contract that\n");
    out.push_str(
        "# claimed completeness would be worse than no contract, because it manufactures\n",
    );
    out.push_str("# confidence. Edit freely -- this list is the honest statement of what is not\n");
    out.push_str("# covered, and it is printed on every run.\n");
    // Quoted: an unquoted `2026-09-29` is an inline table to TOML, and the
    // generated file would not parse. Caught by the round-trip test, which is
    // the only reason it is worth having.
    out.push_str("[judgement]\nreviewed = \"");
    out.push_str(reviewed);
    out.push('"');
    out.push_str("\nnot_covered = [\n");
    for (i, gap) in HONEST_GAPS.iter().enumerate() {
        let comma = if i + 1 == HONEST_GAPS.len() { "" } else { "," };
        out.push_str(&format!("  \"{gap}\"{comma}\n"));
    }
    out.push_str("]\n");
    out
}

/// A gate id that is valid, for the contract we are about to write.
pub fn valid_id(p: Primitive) -> GateId {
    GateId::new(p.as_str()).expect("primitive names are valid gate ids")
}

const GENERATED_HEADER: &str = "\
# palisade.toml -- generated by `palisade init`.
#
# This is a proposal, not a verdict. Every gate below is at `warn`: it reports
# and nothing blocks, because a contract written today has been calibrated on
# nothing. That is the honest posture and it is the point.
#
# To make a gate block, promote it to `error` and say why, in a `[[changes]]`
# record naming the measurement behind it. A gate promoted without a
# `calibration` is reported by the built-in `contract_not_loosened` gate, which
# you cannot switch off -- it is part of the supervisor rather than this file.
";

/// The two primitives that exist in the vocabulary but not in the build, shown
/// commented out so their absence is deliberate and visible rather than an
/// oversight.
const UNIMPLEMENTED_NOTE: &str = "\
# Not in this contract, on purpose. Both exist in the vocabulary and neither is
# implemented in this build, and a gate that is declared but unimplemented
# reports `error` rather than passing -- so shipping one by default would
# make a fresh project fail its own first run. Uncomment either when it is
# implemented, or when you want the run to say so loudly.
#
# [[gates]]
# id = \"secret_absent\"
# check = \"secret_absent\"
# severity = \"warn\"
#
# [[gates]]
# id = \"slop_gate\"
# check = \"external_tool\"
# tool = \"slop-gate\"
# args = [\"check\", \"--base\", \"{base}\", \"--head\", \"{head}\"]
# format = \"sarif\"
# severity = \"warn\"
";

const GENERATED_SEVERITY_NOTE: &str = "\
# Severity: `off` skips a gate, `warn` reports, `error` blocks, `escalate` asks a
# human. Promoting `warn` -> `error` is a claim that this gate is worth
# blocking on, and it is the one claim here that needs evidence behind it.
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_contract;

    #[test]
    fn the_generated_contract_parses() {
        // The first thing that has to be true: `init` writes a file the tool
        // can read. A generator that emits plausible TOML which the parser
        // rejects is worse than no generator.
        let c = parse_contract(&generate("2026-09-29", 0)).expect("generated contract is valid");
        assert_eq!(c.version, 1);
    }

    #[test]
    fn every_gate_starts_at_warn() {
        // A generated contract proposes. If any gate starts at `error`, the
        // project is being handed a claim nobody has measured.
        let c = parse_contract(&generate("2026-09-29", 0)).expect("valid");
        assert!(!c.gates.is_empty());
        for g in &c.gates {
            assert_eq!(
                g.severity,
                Severity::Warn,
                "gate `{}` should start at warn",
                g.id
            );
        }
    }

    #[test]
    fn the_gap_is_populated_and_reviewed() {
        // PRD 9.4: an empty list means the contract claims more than it
        // delivers. A generated contract that started by claiming completeness
        // would manufacture the confidence this section exists to prevent.
        let c = parse_contract(&generate("2026-09-29", 0)).expect("valid");
        assert!(!c.judgement.not_covered.is_empty());
        assert_eq!(c.judgement.reviewed.as_deref(), Some("2026-09-29"));
    }

    #[test]
    fn the_frozen_paths_gate_is_seeded() {
        // `paths_unchanged` with no paths reports `Untrustworthy` -- it
        // enforces nothing. A generated contract must not ship a gate that
        // cannot run.
        let c = parse_contract(&generate("2026-09-29", 0)).expect("valid");
        let paths = c
            .gates
            .iter()
            .find(|g| g.primitive == Primitive::PathsUnchanged)
            .expect("the paths gate is in the menu");
        assert!(!paths.paths.is_empty());
    }

    #[test]
    fn no_unimplemented_primitive_is_declared_by_default() {
        // A generated contract that declares an unimplemented primitive
        // cannot pass its own first run: the gate reports `Untrustworthy` and
        // the verdict is `error` before anyone has changed a line. Found by
        // running `init` on a real repository.
        let c = parse_contract(&generate("2026-09-29", 0)).expect("valid");
        for p in [Primitive::SecretAbsent, Primitive::ExternalTool] {
            assert!(
                !c.gates.iter().any(|g| g.primitive == p),
                "{p} is unimplemented and must not be declared by default"
            );
        }
        // And every gate the contract does declare can actually run.
        for g in &c.gates {
            assert_ne!(
                g.primitive.kind(),
                crate::PrimitiveKind::Judged,
                "the judgement tier is never in a generated contract"
            );
        }
    }

    #[test]
    fn the_absent_primitives_are_named_in_the_file_anyway() {
        // Omitting them silently would be its own dishonesty. The file says
        // they exist and why they are not on.
        let text = generate("2026-09-29", 0);
        assert!(text.contains("secret_absent"), "the gap should be named");
        assert!(text.contains("external_tool"));
        assert!(text.contains("Not in this contract, on purpose"));
    }

    #[test]
    fn generation_is_deterministic() {
        // A generated file that differs between two runs of the same command
        // is a file nobody can diff in review, which is the one moment a
        // generated artefact is actually read.
        assert_eq!(generate("2026-09-29", 0), generate("2026-09-29", 0));
    }

    #[test]
    fn a_workspace_is_announced_because_the_gates_do_not_apply_per_member() {
        let single = generate("2026-09-29", 0);
        let multi = generate("2026-09-29", 4);
        assert!(!single.contains("Detected a workspace"));
        assert!(multi.contains("Detected a workspace with 4 members"));
        // And both are still valid contracts.
        assert!(parse_contract(&multi).is_ok());
    }
}
