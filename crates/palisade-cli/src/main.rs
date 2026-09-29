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

use std::time::Duration;

use palisade_contract::{Contract, Gate, GateId, Primitive, Severity};
use palisade_gates::{GateContext, GateResult, registry};
use palisade_git::Repo;
use palisade_observe::{Budget, Observation};
use palisade_orchestrate::{GateRun, ReductionInput, Verdict, reduce};
use palisade_report::Report;

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
        /// Report format.
        #[arg(long, value_enum, default_value_t = Format::Human)]
        format: Format,
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

/// The report format. `human` is the default because a supervisor's first
/// audience is a person deciding whether to read the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Format {
    /// A PR comment.
    Human,
    /// The machine contract, with the three targets as separate top-level keys.
    Json,
    /// SARIF 2.1.0, for code scanning.
    Sarif,
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
    let format = match cli.cmd {
        Cmd::Check { format, .. } => format,
        Cmd::Observe { .. } => Format::Human,
    };
    let (path, cli_base, cli_budget, expect_diff) = match cli.cmd {
        Cmd::Check {
            path, base, budget, ..
        } => (path, base, budget, true),
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
    let (cargo_version, tool_version) = tool_versions();
    let mut runs: Vec<GateRun> = contract
        .gates
        .iter()
        .map(|g| run_gate(g, &obs, &base, &cargo_version, &tool_version))
        .collect();
    // The gates that audit the contract are not in the contract. A gate that
    // could be declared could also be deleted, and a gate that a worker can
    // switch off is not a gate — so these are appended here, unconditionally,
    // and `parse` refuses to accept a contract that tries to declare them.
    runs.extend(built_in_gates(&obs, &cargo_version));
    let outcomes: Vec<palisade_orchestrate::GateOutcome> =
        runs.iter().map(|r| r.outcome.clone()).collect();
    let verdict = reduce(&ReductionInput {
        outcomes: &outcomes,
        ..Default::default()
    });

    let report = Report {
        contract: &contract,
        runs: &runs,
        verdict,
        tool_version: env!("CARGO_PKG_VERSION"),
    };
    // The report goes to stdout so it can be piped; anything a human needs
    // that is *not* the report goes to stderr.
    print!(
        "{}",
        match format {
            Format::Human => report.human(),
            Format::Json => report.json(),
            Format::Sarif => report.sarif(),
        }
    );
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
    palisade_contract::parse::parse_contract(&source).map_err(|e| format!("{path}: {e}"))
}

/// The gates the supervisor runs whether or not anybody asked for them.
///
/// Their severity is fixed in code and not configurable. That is a deliberate
/// asymmetry: every other gate can be softened, because a project that
/// disagrees can record why. These cannot be softened at all, because the
/// thing they detect is a project softening its own gates.
///
/// Within them, a *justified* loosening is still reported, at `warn`. So the
/// force is not "you may never change the contract" — it is "you may never
/// change it silently".
fn built_in_gates(obs: &Observation, cargo_version: &str) -> Vec<GateRun> {
    [
        Primitive::ContractNotLoosened,
        Primitive::ContractReviewStale,
    ]
    .into_iter()
    .map(|p| {
        let mut g = Gate::new(
            GateId::new(p.as_str()).expect("primitive name is a valid id"),
            p,
        );
        g.severity = Severity::Error;
        // The gate already decides per finding whether a recorded
        // `[[changes]]` reason downgrades it, so there is no global
        // downgrade here. An earlier version applied one if *any* finding
        // was justified, which meant one recorded reason silently unblocked
        // every other loosening in the same diff.
        run_gate(&g, obs, "HEAD", cargo_version, "unused")
    })
    .collect()
}

/// Run one declared gate and collect everything it produced.
///
/// An `Analyzed` gate is a pure function of the observation and cannot fail to
/// run. A `Delegated` gate is the only kind that can fail to run, and it is
/// the only kind that spawns anything.
fn run_gate(
    gate: &Gate,
    obs: &Observation,
    base: &str,
    cargo_version: &str,
    tool_version: &str,
) -> GateRun {
    if gate.severity == Severity::Off {
        // Reported as `off`, never as a pass: a reader must be able to see what
        // was not checked.
        return GateRun::skipped(gate.id.clone(), gate.primitive);
    }

    match gate.primitive {
        Primitive::ChecksGreen => {
            let origin = palisade_exec::origin("cargo", cargo_version);
            palisade_exec::checks_green::run_checks(origin, timeout(gate))
        }
        Primitive::ExternalTool => {
            let origin = palisade_exec::origin("external", tool_version);
            let values = palisade_exec::external_tool::TemplateValues {
                base: base.to_string(),
                head: "HEAD".to_string(),
                index: String::new(),
            };
            let format = palisade_exec::external_tool::OutputFormat::Sarif;
            palisade_exec::external_tool::run_tool(
                "slop-gate",
                &Vec::new(),
                format,
                &values,
                timeout(gate),
                origin,
            )
        }
        other => {
            let result = registry::dispatch(
                other,
                &GateContext {
                    gate,
                    observation: obs,
                    // The only clock read in a gate, and it is here rather
                    // than inside `contract_review_stale` so that gate stays a
                    // pure function of its inputs.
                    now_unix: unix_now(),
                },
            );
            let origin = palisade_orchestrate::Origin::Analyzed { primitive: other };
            match result {
                GateResult::Clean => GateRun::passed(gate.id.clone(), other, origin),
                GateResult::Findings(f) => {
                    GateRun::with_findings(gate.id.clone(), other, f, origin)
                }
                GateResult::Untrustworthy(reason) => {
                    GateRun::untrustworthy(gate.id.clone(), other, reason, origin)
                }
            }
        }
    }
}

/// Unix seconds, once per invocation.
///
/// The only clock in the workspace. Every gate that needs the time is handed
/// it, so that no gate's output depends on when it happened to run.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A gate's timeout, defaulting generously.
///
/// `cargo test` on a large repository is minutes, and a supervisor that
/// declares a test suite is opting into that cost. The default is a ceiling on
/// a runaway, not a target.
fn timeout(gate: &Gate) -> Duration {
    Duration::from_secs(gate.timeout_seconds.unwrap_or(900))
}

/// The versions of the tools we delegate to, resolved once per invocation.
///
/// Asking a tool its version is running a process, so it goes through
/// `palisade-exec`. The CLI used to shell out inline and
/// `scripts/check-boundary.sh` caught it, which is the check earning its
/// place.
fn tool_versions() -> (String, String) {
    use std::sync::OnceLock;
    static CACHE: OnceLock<(String, String)> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            (
                palisade_exec::tool_version("cargo"),
                palisade_exec::tool_version("slop-gate"),
            )
        })
        .clone()
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
