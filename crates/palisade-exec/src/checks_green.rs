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

/// Run the checks and build the gate's run, plus the test inventory it
/// provides to `tests_not_deleted`.
///
/// `timeout` applies per command, not to the gate as a whole. A suite with a
/// slow test suite should not have its `fmt` check time out because `test` did.
pub fn run_checks(
    origin: Origin,
    timeout: Duration,
) -> (
    palisade_orchestrate::GateRun,
    crate::test_inventory::TestInventory,
) {
    let mut builder = GateRunBuilder::new();
    for check in Check::ALL {
        apply(
            &mut builder,
            check,
            run("cargo", &check.argv(), timeout),
            &origin,
        );
    }
    let run = builder.finish(
        palisade_contract::GateId::new("checks_green").expect("constant is valid"),
        palisade_contract::Primitive::ChecksGreen,
        origin,
    );
    (run, list_tests(timeout))
}

/// The test inventory: `cargo test -- --list`.
///
/// `--no-run` is deliberately absent. The inventory is a *list*, and running
/// the suite as well would double the cost of every commit to obtain data the
/// `--list` output already contains. A supervisor that runs a test suite it was
/// not asked to run is a different product with a different failure mode — and
/// `checks_green` runs the suite anyway, as its own job.
fn list_tests(timeout: Duration) -> crate::test_inventory::TestInventory {
    let completed = run(
        "cargo",
        &["test".to_string(), "--".to_string(), "--list".to_string()],
        timeout,
    );
    // An inventory we could not read is an empty one, and the consuming gate
    // has to be able to tell that from a build with no tests. `checks_green`
    // already reported the failure against the `test` check; the gate that
    // consumes this is told not to claim coverage it does not have.
    if completed
        .status
        .interpretation_with(crate::cargo_exit_codes())
        != Interpretation::Passed
    {
        return crate::test_inventory::TestInventory::empty();
    }
    crate::test_inventory::TestInventory::parse(&completed.stdout).0
}

/// Fold one command's result into the run.
fn apply(builder: &mut GateRunBuilder, check: Check, completed: Completed, origin: &Origin) {
    match completed
        .status
        .interpretation_with(crate::cargo_exit_codes())
    {
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

    // Skip cargo's *progress* and count the diagnostics instead of taking the
    // first N lines.
    //
    // Found by running the gate on `pearls`: the reported evidence was
    // "Updating crates.io index | Locking 242 packages" while the real clippy
    // errors began seven lines later. A finding that names the dependency
    // download instead of the lint is not evidence, and a gate whose evidence
    // is routinely about something else is a gate nobody reads the findings of.
    // A *head* is a line that opens a diagnostic: `error: ...` or
    // `warning: ...`. The `| 187 | ...` source excerpt and the `-->` pointer
    // belong to a head, so counting them would overstate the number of
    // problems by an order of magnitude.
    let heads: Vec<&str> = source.iter().copied().filter(|l| is_head(l)).collect();
    let chosen: Vec<&str> = if heads.is_empty() {
        // Nothing recognisable. Report the head of the output rather than
        // nothing, because "it failed and we cannot tell why" is itself the
        // information.
        source.iter().copied().take(n).collect()
    } else {
        heads.iter().copied().take(n).collect()
    };

    let mut kept: Vec<String> = chosen
        .iter()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect();
    if heads.len() > kept.len() {
        kept.push(format!("(+{} more)", heads.len() - kept.len()));
    }
    kept
}

/// Whether a line *opens* a diagnostic.
fn is_head(line: &str) -> bool {
    let t = line.trim_start();
    if is_cargo_summary(t) {
        return false;
    }
    t.starts_with("error: ") || t.starts_with("warning: ")
}

/// Whether a line is cargo's own summary of a failure rather than a diagnostic
/// about the code. These are the lines that made a `pearls` finding name the
/// dependency download instead of the lint.
fn is_cargo_summary(t: &str) -> bool {
    t.starts_with("error: could not compile")
        || t.starts_with("error: build failed")
        || t.starts_with("error: aborting")
        || t.starts_with("error: failed to select a version")
        || t.starts_with("error: failed to get")
        || t.starts_with("error: no such command")
        || t.starts_with("For more information about")
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
        // Realistic shape: a compiler diagnostic opens with `error: `, and
        // cargo's own `error: could not compile` summary is not one of them.
        let noisy = Completed {
            status: crate::Outcome::Exited(1),
            stdout: String::new(),
            stderr: (0..500)
                .map(|i| format!("error: lint {i} fired here"))
                .chain(std::iter::once(
                    "error: could not compile `demo` (lib)".to_string(),
                ))
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let kept = first_diagnostics(&noisy, 3);
        assert_eq!(kept.len(), 4, "three plus the overflow marker: {kept:?}");
        // 500 diagnostics, not 501: cargo's summary is not a diagnostic, and
        // counting it would overstate the number of problems.
        assert!(kept[3].contains("+497 more"), "{kept:?}");
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

#[cfg(test)]
mod diagnostic_selection_tests {
    use super::*;
    use crate::Outcome;

    /// cargo's real output, condensed. The progress lines come first and the
    /// diagnostics start at line 7.
    const CLIPPY: &str = "\
    Updating crates.io index
     Locking 242 packages to latest compatible versions
    Adding fastrand v0.2.1 (was not in lockfile)
     Adding getrandom v0.2.15 (was not in lockfile)
    Compiling cfg-if v1.0.4
     Compiling memchr v2.8.2
    error: consider using `sort_by_key`
     --> src/lib.rs:187:9
      |
    187 |         _ => pearls.sort_by(|a, b| b.updated_at.cmp(&a.updated_at)),
      |         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
      |
    help: try
    187 -         _ => pearls.sort_by(|a, b| b.updated_at.cmp(&a.updated_at)),
    187 +         _ => pears.sort_by_key(|a| std::cmp::Reverse(a.updated_at)),
    |
    error: consider using `sort_by_key`
     --> src/other.rs:12:5
    error: could not compile `pearls-app` (lib) due to 5 previous errors
    ";

    fn clippy_output() -> Completed {
        Completed {
            status: Outcome::Exited(1),
            stdout: String::new(),
            stderr: CLIPPY.to_string(),
        }
    }

    #[test]
    fn the_evidence_names_the_lint_not_the_dependency_download() {
        // Found by running the gate on `pearls`: the finding said "Updating
        // crates.io index" while the real clippy errors started seven lines
        // later. A finding that names the download instead of the lint is not
        // evidence.
        let kept = first_diagnostics(&clippy_output(), 5);
        assert!(
            kept.iter().any(|l| l.contains("sort_by_key")),
            "the diagnostic must be the evidence: {kept:?}"
        );
        assert!(
            !kept.iter().any(|l| l.contains("Updating crates.io")),
            "cargo progress must not be reported as a diagnostic: {kept:?}"
        );
    }

    #[test]
    fn cargo_s_own_summary_is_not_counted_as_a_diagnostic() {
        let kept = first_diagnostics(&clippy_output(), 20);
        assert!(
            !kept.iter().any(|l| l.contains("could not compile")),
            "cargo's summary line is not a finding: {kept:?}"
        );
    }

    #[test]
    fn the_overflow_count_is_of_diagnostics_not_of_all_output() {
        let kept = first_diagnostics(&clippy_output(), 1);
        assert_eq!(kept.len(), 2, "one diagnostic plus the marker: {kept:?}");
        assert!(kept[1].contains("+1 more"), "{kept:?}");
    }

    #[test]
    fn unrecognisable_output_is_reported_rather_than_dropped() {
        // "It failed and we cannot tell why" is itself the information. An
        // empty `observed` would read as a pass.
        let output = Completed {
            status: Outcome::Exited(1),
            stdout: String::new(),
            stderr: "something went wrong and it is not a diagnostic\n".to_string(),
        };
        let kept = first_diagnostics(&output, 5);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].contains("something went wrong"));
    }

    #[test]
    fn an_empty_failure_still_reports_something() {
        let output = Completed {
            status: Outcome::Exited(1),
            stdout: String::new(),
            stderr: String::new(),
        };
        let kept = first_diagnostics(&output, 5);
        assert!(kept.is_empty());
        // And the caller says so rather than emitting a blank observation.
        let msg = if kept.is_empty() {
            "failed, with no diagnostic on stdout or stderr".to_string()
        } else {
            kept.join(" | ")
        };
        assert!(msg.contains("no diagnostic"), "{msg}");
    }
}
