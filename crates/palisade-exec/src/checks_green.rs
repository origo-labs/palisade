//! `checks_green` — do the project's own checks pass?
//!
//! The adoption escape hatch, and the reason v1 is not blocked on adapter
//! completeness: a repository that already runs `cargo clippy` in CI gets that
//! coverage on day one with no Palisade-specific configuration at all.
//!
//! Three commands, three independent results, three findings. A `fmt` failure
//! is not a `test` failure and the report must not make a reader work out
//! which happened — so each gets its own finding, and the verdict is the worst
//! of the three.
//!
//! The fan-out is the reason `GateRun` holds a list of outcomes rather than
//! one. Collapsing these to a single result throws away exactly the evidence
//! PRD 7 requires a block to carry.

use std::time::Duration;

use palisade_orchestrate::{Finding, GateRunBuilder, Origin, Side, Subject, SubjectKind};

use crate::{Completed, Interpretation, run};

/// One of the three checks, and what its name is in a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// `cargo fmt --check`.
    Fmt,
    /// `cargo clippy --all-targets -- -D warnings`.
    Clippy,
    /// `cargo test`.
    Test,
}

impl Check {
    /// The word used in a finding's subject and message.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fmt => "fmt",
            Self::Clippy => "clippy",
            Self::Test => "test",
        }
    }

    /// The argv for a check, with no template variables.
    fn argv(self) -> Vec<String> {
        let parts: &[&str] = match self {
            Self::Fmt => &["fmt", "--check"],
            Self::Clippy => &["clippy", "--all-targets", "--", "-D", "warnings"],
            Self::Test => &["test"],
        };
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    /// The checks, in the order they run. Cheap first: a formatting failure
    /// makes the rest irrelevant, and a user waiting on a supervisor should
    /// not watch `cargo test` run to learn their braces are wrong.
    pub const ALL: [Check; 3] = [Check::Fmt, Check::Clippy, Check::Test];
}

/// Run the checks and build the gate's run.
///
/// `timeout` applies per command, not to the gate as a whole. A suite with a
/// slow test suite should not have its `fmt` check time out because `test` did.
pub fn run_checks(origin: Origin, timeout: Duration) -> palisade_orchestrate::GateRun {
    let mut builder = GateRunBuilder::new();
    for check in Check::ALL {
        apply(
            &mut builder,
            check,
            run("cargo", &check.argv(), timeout),
            &origin,
        );
    }
    builder.finish(
        palisade_contract::GateId::new("checks_green").expect("constant is valid"),
        palisade_contract::Primitive::ChecksGreen,
        origin,
    )
}

/// Fold one command's result into the run.
fn apply(builder: &mut GateRunBuilder, check: Check, completed: Completed, origin: &Origin) {
    match completed.status.interpretation() {
        Interpretation::Passed => {}
        Interpretation::NoTrustworthyResult => {
            // The row that matters. A tool that could not run has not passed,
            // and reporting that as a clippy failure would send a user hunting
            // for a lint that does not exist.
            builder.untrustworthy(palisade_orchestrate::UntrustworthyReason::ToolCouldNotRun {
                detail: format!(
                    "cargo {}: {}",
                    check.as_str(),
                    Interpretation::NoTrustworthyResult.describe(completed.status)
                ),
            });
        }
        Interpretation::Failed => {
            let diagnostics = first_diagnostics(&completed, 5);
            let observed = if diagnostics.is_empty() {
                "failed, with no diagnostic on stdout or stderr".to_string()
            } else {
                diagnostics.join(" | ")
            };
            // No value on either side: there is no baseline formatting to
            // compare against, so inventing one and calling the result a
            // "change" would be a fiction. The diagnostic is the message.
            builder.finding(Finding::new(
                palisade_contract::GateId::new(format!("checks_green_{}", check.as_str()))
                    .expect("suffix is valid"),
                palisade_contract::Primitive::ChecksGreen,
                palisade_contract::Severity::Error,
                Subject::new(SubjectKind::Check, format!("cargo {}", check.as_str())),
                None,
                None,
                Side::Absent,
                Side::Absent,
                format!("`cargo {}` failed: {observed}", check.as_str()),
                origin.clone(),
            ));
        }
    }
}

/// The first `n` lines that look like diagnostics, from stderr first.
///
/// Bounded because `cargo test` can print thousands of lines of test output
/// and a finding whose `observed` field is a megabyte is not evidence anybody
/// can read. The bound is inside the value, not applied after it.
fn first_diagnostics(completed: &Completed, n: usize) -> Vec<String> {
    let stderr: Vec<&str> = completed
        .stderr
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let source = if stderr.is_empty() {
        completed.stdout.lines().collect::<Vec<_>>()
    } else {
        stderr
    };
    let mut kept: Vec<String> = source
        .iter()
        .take(n)
        .map(|l| l.trim().to_string())
        .collect();
    if source.len() > n {
        kept.push(format!("(+{} more lines)", source.len() - n));
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_are_ordered_cheapest_first() {
        assert_eq!(Check::ALL, [Check::Fmt, Check::Clippy, Check::Test]);
    }

    #[test]
    fn a_failing_check_produces_one_named_finding() {
        let mut b = GateRunBuilder::new();
        apply(
            &mut b,
            Check::Clippy,
            Completed {
                status: crate::Outcome::Exited(1),
                stdout: String::new(),
                stderr: "error: unused variable `x`\n  --> src/lib.rs:2:9\n".to_string(),
            },
            &crate::origin("cargo", "1.98.0"),
        );
        let run = b.finish(
            palisade_contract::GateId::new("g").unwrap(),
            palisade_contract::Primitive::ChecksGreen,
            crate::origin("cargo", "1.98.0"),
        );
        assert_eq!(run.findings.len(), 1);
        assert!(run.findings[0].message.contains("clippy"));
        assert!(
            run.findings[0].message.contains("unused variable"),
            "the diagnostic must survive: {:?}",
            run.findings[0].message
        );
        // A failed check is not a comparison, so neither side carries a value.
        // Faking a baseline would make the report say "check changed", which
        // is a fiction: there is no baseline formatting to have changed from.
        assert_eq!(
            run.findings[0].change(),
            palisade_orchestrate::ChangeKind::Failed
        );
    }

    #[test]
    fn a_check_that_cannot_run_is_untrustworthy_not_a_finding() {
        let mut b = GateRunBuilder::new();
        apply(
            &mut b,
            Check::Test,
            Completed {
                status: crate::Outcome::SpawnFailed,
                stdout: String::new(),
                stderr: String::new(),
            },
            &crate::origin("cargo", "1.98.0"),
        );
        let run = b.finish(
            palisade_contract::GateId::new("g").unwrap(),
            palisade_contract::Primitive::ChecksGreen,
            crate::origin("cargo", "1.98.0"),
        );
        // Not a finding: reporting "cargo test failed" when cargo is not
        // installed sends a user hunting for a test failure that never
        // happened.
        assert!(run.findings.is_empty());
        assert!(matches!(
            run.outcome,
            palisade_orchestrate::GateOutcome::Untrustworthy { .. }
        ));
    }

    #[test]
    fn diagnostics_are_bounded_and_say_so() {
        let noisy = Completed {
            status: crate::Outcome::Exited(1),
            stdout: String::new(),
            stderr: (0..500)
                .map(|i| format!("error line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let kept = first_diagnostics(&noisy, 3);
        assert_eq!(kept.len(), 4, "three plus the overflow marker: {kept:?}");
        assert!(kept[3].contains("+497 more"));
    }

    #[test]
    fn a_failing_check_with_no_output_still_says_so() {
        // Silence from a tool is not a pass, and reporting it as one would be
        // the same bug as the exit-code one in a different costume.
        let mut b = GateRunBuilder::new();
        apply(
            &mut b,
            Check::Fmt,
            Completed {
                status: crate::Outcome::Exited(1),
                stdout: String::new(),
                stderr: String::new(),
            },
            &crate::origin("cargo", "1.98.0"),
        );
        let run = b.finish(
            palisade_contract::GateId::new("g").unwrap(),
            palisade_contract::Primitive::ChecksGreen,
            crate::origin("cargo", "1.98.0"),
        );
        assert!(run.findings[0].message.contains("no diagnostic"));
        // A failed check is not a diff, so neither side carries a value.
        assert_eq!(
            run.findings[0].change(),
            palisade_orchestrate::ChangeKind::Failed
        );
    }
}
