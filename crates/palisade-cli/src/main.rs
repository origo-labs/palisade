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

use palisade_contract::{Contract, Severity};
use palisade_gates::{GateContext, GateResult, registry};
use palisade_git::Repo;
use palisade_observe::{Budget, Observation};
use palisade_orchestrate::{GateOutcome, ReductionInput, Verdict, reduce};

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
    /// Run the declared contract and return a verdict.
    Check {
        /// Repository to inspect. Defaults to the working directory.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Baseline commit-ish. Overrides `[baseline].ref` in the contract.
        #[arg(long)]
        base: Option<String>,
        /// Observation budget in bytes. Overrides `[budget]`.
        #[arg(long)]
        budget: Option<usize>,
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
    let (path, cli_base, cli_budget, expect_diff) = match cli.cmd {
        Cmd::Check { path, base, budget } => (path, base, budget, true),
        Cmd::Observe {
            path,
            base,
            budget,
            expect_diff,
        } => (path, Some(base), Some(budget), expect_diff),
    };

    let start: PathBuf = path.unwrap_or_else(|| PathBuf::from("."));
    let start = start
        .into_os_string()
        .into_string()
        .map_err(|p| format!("path is not UTF-8: {p:?}"))?;
    let start = Utf8PathBuf::from(start);
    let repo = Repo::open(&start).map_err(|e| e.to_string())?;

    // `observe` is a debugging tool and deliberately contract-free: its job is
    // to show what a supervisor would see, including on a repository that has
    // not declared a contract yet.
    if observing {
        let base = cli_base.unwrap_or_else(|| "HEAD".to_string());
        let budget = resolve_budget(cli_budget, None)?;
        let obs = capture(&repo, &base, budget, expect_diff)?;
        print_observation(&obs);
        println!("verdict: {}", Verdict::Accept);
        return Ok(Verdict::Accept);
    }

    // `check` is the product. A missing or invalid contract is `error`, and
    // never `block`: a contract that could not be read has not been violated,
    // and conflating the two is the bug this project exists to avoid.
    let contract = load_contract(repo.root())?;
    let base = cli_base
        .or_else(|| contract.baseline_ref.clone())
        .unwrap_or_else(|| "HEAD".to_string());
    let budget = resolve_budget(cli_budget, Some(&contract))?;

    let obs = capture(&repo, &base, budget, true)?;
    let reports: Vec<GateReport> = contract.gates.iter().map(|g| run_gate(g, &obs)).collect();
    let outcomes: Vec<GateOutcome> = reports.iter().map(|r| r.outcome.clone()).collect();
    let verdict = reduce(&ReductionInput {
        outcomes: &outcomes,
        ..Default::default()
    });

    print_report(&contract, &reports, verdict);
    Ok(verdict)
}

fn capture(
    repo: &Repo,
    base: &str,
    budget: Budget,
    expect_diff: bool,
) -> Result<Observation, String> {
    let inputs = repo
        .observation_inputs(Some(base))
        .map_err(|e| format!("could not observe: {e}"))?;
    Ok(Observation::capture(&inputs, budget, expect_diff))
}

fn resolve_budget(cli: Option<usize>, contract: Option<&Contract>) -> Result<Budget, String> {
    let bytes = cli.unwrap_or_else(|| {
        contract.map_or(palisade_contract::DEFAULT_OBSERVATION_BYTES, |c| {
            c.budget_observation_bytes
        })
    });
    Budget::new(bytes).ok_or_else(|| {
        format!(
            "observation budget {bytes} is below the minimum of {}; a truncated \
             observation that small is not evidence of anything",
            Budget::MIN
        )
    })
}

/// Read and validate `palisade.toml` from the repository root.
fn load_contract(root: &Utf8PathBuf) -> Result<Contract, String> {
    let path = root.join(palisade_contract::parse::CONTRACT_FILENAME);
    let source = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "could not read {}: {e}. A repository without a contract has no \
             declared quality bar, and a supervisor cannot invent one.",
            path
        )
    })?;
    palisade_contract::parse::parse(&source).map_err(|e| format!("{path}: {e}"))
}

/// One gate's outcome *and* every finding it produced.
///
/// The two are separate because one `GateOutcome` carries one finding, and
/// reducing several to the most severe is fine for a verdict and fatal for a
/// report. PRD 7 is explicit: "a block without the diff hunk, the gate id, the
/// expected and observed value, and a stable fingerprint is a bug" — and a
/// report that shows one of three findings is a block with the other two
/// missing. An earlier version of this file collapsed and dropped, with a
/// comment claiming the rest were printed. They were not.
struct GateReport {
    gate_id: String,
    primitive: &'static str,
    outcome: GateOutcome,
    findings: Vec<palisade_orchestrate::Finding>,
}

fn run_gate(gate: &palisade_contract::Gate, obs: &Observation) -> GateReport {
    if gate.severity == Severity::Off {
        // Reported as `off`, never as a pass: a reader must be able to see what
        // was not checked.
        return GateReport {
            gate_id: gate.id.to_string(),
            primitive: gate.primitive.as_str(),
            outcome: GateOutcome::Skipped {
                reason: palisade_orchestrate::SkipReason::DeclaredOff,
            },
            findings: Vec::new(),
        };
    }
    let result = registry::dispatch(
        gate.primitive,
        &GateContext {
            gate,
            observation: obs,
        },
    );
    let origin = palisade_orchestrate::Origin::Analyzed {
        primitive: gate.primitive,
    };
    let (outcome, findings) = match result {
        GateResult::Clean => (GateOutcome::Pass { origin }, Vec::new()),
        GateResult::Findings(f) => {
            let worst = worst_of(&f);
            (GateOutcome::Fail(worst), f)
        }
        GateResult::Untrustworthy(reason) => {
            (GateOutcome::Untrustworthy { reason, origin }, Vec::new())
        }
    };
    GateReport {
        gate_id: gate.id.to_string(),
        primitive: gate.primitive.as_str(),
        outcome,
        findings,
    }
}

/// The finding that decides the verdict for a gate: the most severe one.
///
/// Every finding is still reported by [`print_report`]; this only picks which
/// single one the verdict algebra sees.
fn worst_of(findings: &[palisade_orchestrate::Finding]) -> palisade_orchestrate::Finding {
    let mut worst = findings[0].clone();
    for f in findings {
        if severity_rank(f.severity) > severity_rank(worst.severity) {
            worst = f.clone();
        }
    }
    worst
}

fn severity_rank(s: Severity) -> u8 {
    match s {
        Severity::Warn => 0,
        Severity::Escalate => 1,
        Severity::Error => 2,
        Severity::Off => 0,
    }
}

fn print_report(contract: &Contract, reports: &[GateReport], verdict: Verdict) {
    println!(
        "contract:  {} gate(s), version {}",
        contract.gates.len(),
        contract.version
    );
    println!();
    for report in reports {
        match &report.outcome {
            GateOutcome::Pass { .. } => {
                println!("  pass           {}", report.primitive);
            }
            GateOutcome::Skipped { .. } => {
                println!("  off            (declared off, not checked)");
            }
            GateOutcome::Untrustworthy { reason, .. } => {
                println!("  ERROR          {}: {}", report.primitive, reason.detail());
            }
            GateOutcome::Fail(_) => {
                // Every finding, not just the one that decided the verdict.
                for f in &report.findings {
                    println!("  {:<8} [{}] {}", f.severity, report.gate_id, f.message);
                    println!(
                        "            {} -> {}   [{}]",
                        f.expected, f.observed, f.fingerprint
                    );
                }
            }
        }
    }
    println!();
    println!("verdict: {verdict}");
    // PRD 8: the gap is the artefact somebody owns. It is printed on every run
    // so it cannot quietly stop being true.
    if contract.judgement.not_covered.is_empty() {
        println!("not_covered: (empty — the contract claims more than it delivers)");
    } else {
        println!("not_covered:");
        for item in &contract.judgement.not_covered {
            println!("  - {item}");
        }
    }
    println!(
        "reviewed: {}",
        contract.judgement.reviewed.as_deref().unwrap_or("<none>")
    );
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
    println!("files:         {}", obs.files.len());
    for f in &obs.files {
        println!(
            "  {} (base: {}, head: {})",
            f.path,
            if f.base.is_some() { "yes" } else { "no" },
            if f.head.is_some() { "yes" } else { "no" }
        );
    }
    println!("--- diff ---\n{}", obs.diff.text);
}
