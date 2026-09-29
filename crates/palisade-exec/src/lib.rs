//! `palisade-exec` — the `Delegated` gate kind, and the only crate in the
//! workspace permitted to spawn a process other than `palisade-git`.
//!
//! The whole crate exists to own one table, and the table is in code rather
//! than in prose so that widening it means editing the thing that implements
//! it.
//!
//! | Process outcome | Outcome |
//! | --- | --- |
//! | exit 0 | `Pass` |
//! | exit 1 | `Fail`, one finding per reported diagnostic |
//! | exit 2 | `Untrustworthy` — no trustworthy result |
//! | not found on `PATH` | `Untrustworthy` |
//! | timed out | `Untrustworthy` |
//! | killed by signal | `Untrustworthy` |
//! | stdout unparseable as the declared format | `Untrustworthy` |
//! | any other non-zero code | `Untrustworthy`, **not** `Fail` |
//!
//! Only 0 and 1 mean anything, and only because the invoked tools document
//! that. Everything else is "no trustworthy verdict", which is `Error`.
//!
//! Why this got its own crate: `EVIDENCE.md` §5 records a predecessor whose
//! verifier could not start and whose tests could fail being
//! indistinguishable to the caller, and which confounded every model
//! comparison in that programme. The distinction is correct here, it is
//! tested one row at a time, and it is not going in the least-tested layer.

pub mod checks_green;
pub mod external_tool;

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use palisade_orchestrate::{Origin, ToolId, UntrustworthyReason};

/// The signal that killed a child, where the platform tells us.
fn signal_of(status: &std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        std::os::unix::process::ExitStatusExt::signal(status).unwrap_or(-1)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        -1
    }
}

/// Poll interval while waiting for a child. Ten milliseconds is short enough
/// that a finished tool is not noticeably delayed, and long enough that a
/// run spawning several processes does not spend its time in `sleep`.
const POLL: Duration = Duration::from_millis(10);

/// The captured result of one delegated process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completed {
    /// How the process finished, before any interpretation.
    pub status: Outcome,
    /// Whatever the tool wrote to stdout, which is its evidence.
    pub stdout: String,
    /// Whatever it wrote to stderr, which is usually where diagnostics land.
    pub stderr: String,
}

/// How a process finished, before any interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The process ran to completion and exited with this code.
    Exited(
        /// The exit code.
        i32,
    ),
    /// The process was killed by a signal. `-1` where the platform does not
    /// say which.
    Signalled(
        /// The signal number, or `-1`.
        i32,
    ),
    /// The process outlived its budget and was killed.
    TimedOut,
    /// The process could not be started at all.
    SpawnFailed,
}

impl Outcome {
    /// The table's verdict for this outcome.
    ///
    /// The one function in the crate that decides pass from fail, and the one
    /// place a future contributor has to look to widen the rules.
    pub const fn interpretation(self) -> Interpretation {
        match self {
            // Only 0 and 1 carry a verdict, and only because the tools we
            // invoke document that. Exit 2 is slop-gate's "I could not produce
            // a trustworthy result"; cargo's convention is not documented, so
            // anything else is treated the same way: unknown is not a verdict.
            Self::Exited(0) => Interpretation::Passed,
            Self::Exited(1) => Interpretation::Failed,
            Self::Exited(_) => Interpretation::NoTrustworthyResult,
            Self::Signalled(_) | Self::TimedOut | Self::SpawnFailed => {
                Interpretation::NoTrustworthyResult
            }
        }
    }
}

/// What a process outcome means, once the table has been applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpretation {
    /// The tool ran and said everything is fine.
    Passed,
    /// The tool ran and reported findings. Its output says what they are.
    Failed,
    /// The tool did not produce a verdict. Never a pass, and never a failure
    /// of the work under review.
    NoTrustworthyResult,
}

impl Interpretation {
    /// A sentence for a finding or an error.
    pub fn describe(self, outcome: Outcome) -> String {
        match (self, outcome) {
            (Self::Passed, o) => format!("tool exited {o:?} and reported no findings"),
            (Self::Failed, o) => format!("tool exited {o:?} and reported findings"),
            (Self::NoTrustworthyResult, Outcome::Exited(c)) => format!(
                "tool exited {c}, which is not a documented verdict. Treating it \
                 as a failure would report a tool problem as a problem with the \
                 work under review."
            ),
            (Self::NoTrustworthyResult, Outcome::Signalled(s)) => {
                format!("tool was killed by signal {s}")
            }
            (Self::NoTrustworthyResult, Outcome::TimedOut) => {
                "tool did not finish within its timeout".to_string()
            }
            (Self::NoTrustworthyResult, Outcome::SpawnFailed) => {
                "tool could not be started; it is not installed or not executable".to_string()
            }
        }
    }
}

/// Run a program, capturing its output, with a hard timeout.
///
/// # Errors
///
/// Never in the "return `Err`" sense: a process that cannot start, times out
/// or dies is a *result*, because distinguishing it from a failure is the
/// entire purpose of this crate. Only a programming error panics.
pub fn run(program: &str, args: &[String], timeout: Duration) -> Completed {
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A delegated tool's colour codes end up inside a finding's `observed`
        // field, and a JSON or SARIF artefact full of escape sequences is not
        // machine-readable. Colour is a function of the terminal, and a report
        // is not a terminal.
        .env("CARGO_TERM_COLOR", "never")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .env("TERM", "dumb")
        // An argument list is data, never a shell. `Command` does not use one,
        // and setting this makes that explicit to anything downstream.
        .env("SHELL", "/bin/sh")
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            return Completed {
                status: Outcome::SpawnFailed,
                stdout: String::new(),
                stderr: String::new(),
            };
        }
    };

    // The pipes must be drained on other threads, or a tool that writes more
    // than a buffer's worth of diagnostics blocks forever and we would report
    // it as a timeout. That is a false `Untrustworthy` caused by our own
    // reading, which is the exact failure mode this crate exists to avoid.
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    let deadline = Instant::now() + timeout;
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(s)) => {
                let outcome = if s.success() {
                    Outcome::Exited(0)
                } else {
                    // `ExitStatus::code()` is `None` exactly when the child
                    // died from a signal, which is the distinction between
                    // "ran and said no" and "did not finish".
                    match s.code() {
                        Some(c) => Outcome::Exited(c),
                        // `code()` is `None` exactly when the child died from
                        // a signal. The number is worth having in the message,
                        // and std exposes it per-platform.
                        None => Outcome::Signalled(signal_of(&s)),
                    }
                };
                break outcome;
            }
            Ok(None) => {}
            Err(_) => break Outcome::SpawnFailed,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break Outcome::TimedOut;
        }
        std::thread::sleep(POLL);
    };

    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();

    Completed {
        status: outcome,
        stdout,
        stderr,
    }
}

/// Substitute `{base}`, `{head}` and `{index}` in a configured argument list.
///
/// A template variable that does not resolve is an error, never passed through
/// literally. A literal `{base}` in an argv is a gate that analysed the wrong
/// tree and reported green, which is the same class of bug as collapsing exit
/// code 2 into a failure: a plausible-looking result that means nothing.
///
/// # Errors
///
/// Returns a description of the first unresolved variable.
pub fn template_args(
    args: &[String],
    base: &str,
    head: &str,
    index: &str,
) -> Result<Vec<String>, String> {
    args.iter()
        .map(|a| substitute(a, base, head, index))
        .collect()
}

/// The variables the contract may reference.
const VARIABLES: [(&str, &str); 3] = [("base", ""), ("head", ""), ("index", "")];

/// Substitute the exact variables in one argument, and refuse near-misses.
///
/// Scanning before substituting matters. A blanket `replace("{base}", base)`
/// turns `{base_sha}` into `abc_sha` — a path nobody asked for, passed to a
/// tool that will treat it as a real ref and quietly analyse the wrong tree.
/// So each brace-delimited run is classified: an exact variable is replaced, a
/// near-miss is an error, and anything else is the caller's own literal.
fn substitute(arg: &str, base: &str, head: &str, index: &str) -> Result<String, String> {
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        // An unbalanced brace is not a placeholder, so it is the caller's.
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return Ok(out);
        };
        let inner = &after[..close];
        let name = inner.trim();
        let value = match name {
            "base" => Some(base),
            "head" => Some(head),
            "index" => Some(index),
            _ => None,
        };
        if let Some(v) = value {
            out.push_str(v);
        } else if is_near_miss(name) {
            return Err(format!(
                "argument `{arg}` contains `{{{name}}}`, which is not one of \
                 {{base}}, {{head}} or {{index}}. Silently passing it through \
                 would make the gate analyse the wrong tree."
            ));
        } else {
            out.push_str(&rest[open..=open + 1 + close]);
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Whether an unknown placeholder is plausibly a mistyped variable, as opposed
/// to a literal brace pair the caller meant.
fn is_near_miss(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    VARIABLES.iter().any(|(known, _)| lower.contains(known))
}

/// The reason a completed process did not yield a verdict, if it did not.
pub fn untrustworthy_reason(
    tool: &ToolId,
    program: &str,
    completed: &Completed,
) -> Option<UntrustworthyReason> {
    let interp = completed.status.interpretation();
    if interp != Interpretation::NoTrustworthyResult {
        return None;
    }
    let detail = format!("{program}: {}", interp.describe(completed.status));
    let _ = tool;
    Some(UntrustworthyReason::ToolCouldNotRun { detail })
}

/// The origin a delegated verdict carries.
pub fn origin(tool: &str, version: &str) -> Origin {
    Origin::Delegated {
        tool: ToolId::new(tool),
        version: version.to_string(),
    }
}

/// A tool's version string, for the provenance of a delegated verdict.
///
/// Lives here rather than in the CLI because asking a tool its version is
/// running a process, and this is the only crate allowed to do that. The
/// `scripts/check-boundary.sh` check caught a CLI that did it inline, which is
/// the check doing exactly the job it was written for.
///
/// A hard-coded version string in a report would be a lie the moment the tool
/// is upgraded under it, and a missing tool reports `unknown` rather than
/// nothing — an absent version is worse than an honest one.
pub fn tool_version(program: &str) -> String {
    let completed = run(program, &["--version".to_string()], Duration::from_secs(10));
    if completed.status.interpretation() != Interpretation::Passed {
        return "unknown".to_string();
    }
    completed
        .stdout
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .nth(1)
        .unwrap_or("unknown")
        .to_string()
}

/// Whether an executable is findable, without running it. Used to report a
/// missing tool as a missing tool rather than as a spawn failure.
pub fn is_available(program: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate: &Path = &dir.join(program);
        candidate.is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sh(script: &str) -> Completed {
        run(
            "sh",
            &["-c".to_string(), script.to_string()],
            Duration::from_secs(10),
        )
    }

    #[test]
    fn exit_zero_is_a_pass() {
        assert_eq!(sh("exit 0").status.interpretation(), Interpretation::Passed);
    }

    #[test]
    fn exit_one_is_a_failure_and_its_output_is_the_evidence() {
        let c = sh("echo 'src/lib.rs:1:1: something is wrong' >&2; exit 1");
        assert_eq!(c.status.interpretation(), Interpretation::Failed);
        assert!(
            c.stderr.contains("something is wrong"),
            "the diagnostic must survive: {:?}",
            c.stderr
        );
    }

    #[test]
    fn exit_two_is_not_a_failure() {
        // The row that cost the predecessor a measurement programme.
        assert_eq!(
            sh("exit 2").status.interpretation(),
            Interpretation::NoTrustworthyResult
        );
    }

    #[test]
    fn an_undocumented_exit_code_is_not_a_failure() {
        // cargo does not document an exit code convention, so 101 — which
        // `cargo test` really does return when a test fails — is as
        // undocumented to us as 250. It is a tool problem until proven
        // otherwise, and the gate says so instead of inventing a verdict.
        for code in [2, 3, 101, 127, 250] {
            let c = sh(&format!("exit {code}"));
            assert_eq!(
                c.status.interpretation(),
                Interpretation::NoTrustworthyResult,
                "exit {code}"
            );
        }
    }

    #[test]
    fn a_missing_binary_is_not_a_failure() {
        let c = run("palisade-no-such-binary-xyz", &[], Duration::from_secs(5));
        assert_eq!(c.status, Outcome::SpawnFailed);
        assert_eq!(
            c.status.interpretation(),
            Interpretation::NoTrustworthyResult
        );
    }

    #[test]
    fn a_hang_is_a_timeout_not_a_failure() {
        let c = run(
            "sh",
            &["-c".to_string(), "sleep 30".to_string()],
            Duration::from_millis(200),
        );
        assert_eq!(c.status, Outcome::TimedOut);
        assert_eq!(
            c.status.interpretation(),
            Interpretation::NoTrustworthyResult
        );
    }

    #[test]
    fn a_signal_death_is_not_a_failure() {
        let c = sh("kill -9 $$");
        assert_eq!(c.status, Outcome::Signalled(9));
        assert_eq!(
            c.status.interpretation(),
            Interpretation::NoTrustworthyResult
        );
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_is_not_a_timeout() {
        // The failure this crate could easily have shipped: a tool that writes
        // more than 64 KiB blocks on a full pipe, never exits, and we report a
        // timeout. That is an `Untrustworthy` caused by our own reading of a
        // tool that actually succeeded.
        let c = sh("for i in $(seq 1 20000); do echo 'a diagnostic line of some length'; done");
        assert_eq!(c.status.interpretation(), Interpretation::Passed);
        assert!(
            c.stdout.len() > 600_000,
            "expected a full pipe buffer worth, got {} bytes",
            c.stdout.len()
        );
    }

    #[test]
    fn templates_substitute_and_leftovers_are_an_error() {
        let args = vec![
            "check".to_string(),
            "--base".to_string(),
            "{base}".to_string(),
            "--head".to_string(),
            "{head}".to_string(),
        ];
        let out = template_args(&args, "abc", "def", "ghi").expect("substitutes");
        assert_eq!(out, vec!["check", "--base", "abc", "--head", "def"]);
    }

    #[test]
    fn an_unresolved_template_is_an_error_not_a_literal() {
        // A literal `{base}` in an argv is a gate that analysed the wrong tree
        // and reported green. A *typo* of the variable is the same bug with an
        // extra step, so `{bases}` and `{Base}` have to fail too — otherwise
        // the one mistake a user is most likely to make is the one mistake
        // that passes silently.
        for typo in ["{bases}", "{Base}", "{BASE}", "{base_sha}", "{INDEX}"] {
            let args = vec!["--base".to_string(), typo.to_string()];
            let err = template_args(&args, "abc", "def", "ghi").unwrap_err();
            assert!(err.contains(typo), "{typo} should be rejected, got {err}");
        }
    }

    #[test]
    fn a_literal_brace_that_is_not_a_template_is_left_alone() {
        // Not every `{...}` is a typo, and refusing them all would make the
        // gate unusable for tools that take JSON or format strings.
        let args = vec!["--label".to_string(), "{literal}".to_string()];
        let out = template_args(&args, "abc", "def", "ghi").expect("left alone");
        assert_eq!(out[1], "{literal}");
    }
}
