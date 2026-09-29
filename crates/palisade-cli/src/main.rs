//! `palisade` — the single binary.
//!
//! M0 has no gates registered, so `check` returns `accept` and says so plainly
//! rather than implying the repository was inspected. `observe` is the
//! milestone's real deliverable: it prints the bounded observation a supervisor
//! would act on, which is how the empty-diff class of bug becomes visible
//! during development rather than after a measurement programme has run on it.
//!
//! M3 adds `init`, the real contract loader, and the report formats.

// Writing to stdout is this crate's product, not its logging: `palisade
// observe` *is* an observation printer, and `palisade check` emits a report
// designed to be piped. `print_stdout` is denied workspace-wide for exactly
// that reason and re-permitted here and only here, where the output is the
// artefact rather than a debugging habit.
#![allow(clippy::print_stdout)]

use std::path::PathBuf;
use std::process::ExitCode;

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

use palisade_git::Repo;
use palisade_observe::{Budget, Observation};
use palisade_orchestrate::{ReductionInput, Verdict, reduce};

#[derive(Debug, Parser)]
#[command(
    name = "palisade",
    about = "A supervisor for coding agents that runs a project's declared quality contract.",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run the declared contract. M0: no gates registered yet.
    Check {
        /// Repository to inspect. Defaults to the working directory.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Baseline commit-ish for the two-tree diff.
        #[arg(long, default_value = "HEAD")]
        base: String,
        /// Observation budget in bytes.
        #[arg(long, default_value_t = Budget::DEFAULT.bytes())]
        budget: usize,
    },
    /// Print the bounded observation. Exists so the empty-diff failure mode
    /// is visible during development, not after it has been measured on.
    Observe {
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, default_value = "HEAD")]
        base: String,
        #[arg(long, default_value_t = Budget::DEFAULT.bytes())]
        budget: usize,
        /// Assert that a change is expected, so an empty observation is
        /// reported as a fault rather than as a clean tree.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        expect_diff: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(verdict) => ExitCode::from(verdict.exit_code() as u8),
        Err(msg) => {
            // A startup failure is `error`, never `block`: a verifier that
            // could not start and a verifier that failed are different
            // events, and conflating them is the bug this project exists to
            // avoid (EVIDENCE.md 5, PRD 4).
            eprintln!("palisade: {msg}");
            ExitCode::from(Verdict::Error.exit_code() as u8)
        }
    }
}

fn run(cli: Cli) -> Result<Verdict, String> {
    // `expect_diff` defaults to true for `check`: a supervisor that was asked
    // to review a change and observed nothing must say so, not report a clean
    // tree. `observe --expect-diff=false` is the escape hatch for inspecting
    // a repository with no pending work.
    let observing = matches!(cli.cmd, Cmd::Observe { .. });
    let (path, base, budget, expect_diff) = match cli.cmd {
        Cmd::Check { path, base, budget } => (path, base, budget, true),
        Cmd::Observe {
            path,
            base,
            budget,
            expect_diff,
        } => (path, base, budget, expect_diff),
    };

    let start: PathBuf = path.unwrap_or_else(|| PathBuf::from("."));
    let start = start
        .into_os_string()
        .into_string()
        .map_err(|p| format!("path is not UTF-8: {p:?}"))?;
    let start = Utf8PathBuf::from(start);
    let repo = Repo::open(&start).map_err(|e| e.to_string())?;

    let Some(budget) = Budget::new(budget) else {
        return Err(format!(
            "observation budget {budget} is below the minimum of {}; a truncated \
             observation that small is not evidence of anything",
            Budget::MIN
        ));
    };

    let inputs = repo
        .observation_inputs(Some(&base))
        .map_err(|e| format!("could not observe: {e}"))?;
    let obs = Observation::capture(&inputs, budget, expect_diff);

    let verdict = if observing {
        {
            print_observation(&obs);
            Verdict::Accept
        }
    } else {
        {
            // M0 registers no gates. Say so on stdout rather than letting a
            // green result imply the repository was checked: "zero false
            // positives" and "checked nothing" must never look alike.
            eprintln!(
                "palisade: no gates registered in this build (M0). \
                 This is not an inspection of the repository."
            );
            reduce(&ReductionInput::default())
        }
    };

    println!("verdict: {verdict}");
    Ok(verdict)
}

fn print_observation(obs: &Observation) {
    println!("base:          {}", obs.base.as_deref().unwrap_or("<none>"));
    println!("budget:        {} bytes", obs.budget.bytes());
    println!(
        "observation:   {}{}",
        if obs.diff.truncated {
            "truncated"
        } else {
            "intact"
        },
        match obs.empty {
            Some(reason) => format!(" (empty: {reason})"),
            None => String::new(),
        }
    );
    println!("status entries: {}", obs.status.len());
    for entry in &obs.status {
        println!("  {:>2} {:<2} {}", entry.x, entry.y, entry.path);
        if let Some(orig) = &entry.orig_path {
            println!("       (from {orig})");
        }
    }
    println!("--- diff ---\n{}", obs.diff.text);
}
