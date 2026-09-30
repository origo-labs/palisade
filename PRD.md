# Rulebound — PRD

**A supervisor for coding agents that runs a project's declared quality
contract, and escalates only what the contract could not cover.**

Status: specification. Nothing is built. This document and `EVIDENCE.md` are
meant to be sufficient for a fresh implementation, in Rust, from scratch.

Rust is the primary ecosystem for v1. The justification is in §3 and it is not
primarily a language preference.

---

## 1. Problem

Coding agents are good at writing code and bad at knowing when to stop. Every
supervisor built to help them shares a flaw: **it requires trusting a
judgement**.

A measurement programme tried to fix that with models and failed. A 400M span
classifier and a frontier LLM were both asked whether a diff contradicted a
project's own rules file, on a repository neither had seen. Results in
`EVIDENCE.md`: the classifier flagged rules on states with an *empty diff*, and
the same head saturated at 1.00 for both models. Meanwhile the rules in
question turned out to be trivially machine-checkable, and checks for them were
correct on four of four violations at zero cost.

The lesson is not "models are bad". It is that the judgement was never the
problem to solve. The problem is that quality rules live in prose, where
nothing can enforce them.

## 2. Thesis

**Move the trust from the supervisor to the project.**

A project declares its quality bar as an executable contract. The supervisor
runs the contract and reports pass/fail with evidence. It does not judge
whether work is good, because the project has already said what good is.

This is a better security property than a validated model. "We tested the model
on a held-out set" decays as models, prompts and repositories move. "The
project's declared gates all passed, and here is what they did not cover" does
not.

It also relocates the hard engineering from *make the supervisor smarter* — which
could not be done — to *make the project specify its quality bar executably*,
which is a specification problem, and specification problems fail visibly.

## 3. Why Rust first

Three reasons, in order of weight.

**3.1 An excellent gate already exists, and integrating beats rebuilding.**
[`slop-gate`](REFERENCE-slop-gate.md) is a deterministic, model-free,
network-free CI gate for the hardest rule class — "did this change make the
codebase worse" — with cyclomatic-complexity mass, near-clone detection over
token and AST shingles, lint-suppression growth, unsafe-surface growth,
dependency-surface growth, and structural erosion. It has published threshold
curves, SARIF output, a calibration protocol, and a baseline-artifact scheme
keyed to commit, policy, tool version and build fingerprint. Rebuilding that in
another language would be a strictly worse version of a solved problem.

**3.2 Rust is cheap to check, precisely.** Every check Rulebound needs in v1 is an
AST or a build-graph question: unsafe blocks, attribute widening, `mod` and
visibility changes, feature and dependency surface, public API signatures,
generic bounds, trait implementations. `tree-sitter-rust` parses it, and
`cargo` answers the rest faster than any check can be reasoned about. The
economics only work in an ecosystem where "run every gate" is a sub-second
affair.

**3.3 The gate has to run in CI, not in a sidecar.** A supervisor that adds a
language runtime, a virtualenv and a network dependency to every commit is a
supervisor teams turn off. A single static binary that shells out to `cargo` and
`slop-gate` is not.

Cross-language support is deliberately deferred. The contract format in §5 is
designed to admit it — every primitive names an ecosystem adapter — but v1
ships Rust-only, and the reason is recorded here so it is a decision rather than
a deferral: the integration surface with `slop-gate` is a Rust binary, SARIF,
and a shared notion of "public API surface", and that is worth getting right in
one language before it is abstracted across four.

## 4. Product

A single binary. Given a repository and a coding agent's output, it produces
exactly one of:

| Outcome | Meaning |
| --- | --- |
| `accept` | every declared gate passed |
| `block` | at least one gate failed, with evidence for each |
| `escalate` | a declared judgement call was inconclusive |
| `error` | the contract or environment is broken; **no verdict is implied** |

`error` is distinct from `block` and never collapses into it. A gate that could
not run has not passed. This distinction was learned the hard way in the
predecessor project, where a verifier that failed to start and a verifier whose
tests failed were indistinguishable to the caller.

## 5. The gate contract

A versioned TOML file in the repository root. Two required sections.

```toml
version = 1

# ---- deterministic gates -------------------------------------------------
[[gates]]
id = "project_checks_green"
severity = "error"
check = "checks_green"          # cargo fmt/clippy/test via the adapter

[[gates]]
id = "no_complexity_growth"
severity = "error"
check = "external_tool"
tool = "slop-gate"
args = ["check", "--base", "{base}", "--head", "{head}", "--index", "{index}"]
format = "sarif"                # findings merged into our evidence

[[gates]]
id = "no_new_dependencies"
severity = "error"
check = "dependency_surface_unchanged"

[[gates]]
id = "tests_not_deleted"
severity = "error"
check = "tests_not_deleted"

[[gates]]
id = "public_api_unchanged"
severity = "error"
check = "public_api_unchanged"
allow = []                      # documented, reviewed additions

[[gates]]
id = "no_unsafe_added"
severity = "error"
check = "unsafe_surface_unchanged"

[[gates]]
id = "no_suppression_widening"
severity = "error"
check = "suppressions_not_widened"   # #[allow] / #[expect] broadening
# slop-gate covers this too; declared here so the contract is self-describing.

# ---- judgement gates: opt-in, off by default ----------------------------
[[gates]]
id = "on_topic"
severity = "escalate"
check = "judged"
question = """
Pick exactly one label for the whole change:
  no_change | on_topic | off_topic | contradicts_rules
"""

# ---- the gap, declared --------------------------------------------------
[judgement]
not_covered = [
  "whether the chosen design is the right one for the problem",
  "whether error messages are good enough for an end user",
  "whether the benchmark added is a fair one",
]
reviewed = "2026-09-28"
```

### The judgement section is mandatory

Not "consider declaring the gap" — **required, may be empty, must carry a review
date.** A stale review is itself a finding.

The reasoning: an undeclared judgement call is a risk nobody is looking at.
Writing it down costs nothing and turns the gap into an artefact the team owns
and reviews. It also stops the deterministic story from quietly becoming a
claim of completeness, which is how every component in the predecessor project
overstated itself.

### Contract language rules

Non-negotiable, taken from `slop-gate`, which has already learned them the hard
way:

- **Unknown keys, unknown checks and unknown severities are errors.** A
  misspelled gate must never silently weaken the contract.
- **Suppressions require a reason**, a repository-relative path and a named
  gate, and are recorded in the output artefact. Use sparingly; prefer a
  refactor.
- **Severities are `off | warn | error | escalate`.** New gates default to
  `warn`. Promotion to `error` requires a recorded calibration: a run over a
  corpus with the resulting false-positive rate written down. `slop-gate`
  documents this as a protocol; follow it.
- **Thresholds are published with the curve they came from.** Every
  `slop-gate` default is justified by a table of what survives at 40, 60, 80,
  120 and 160 tokens. A threshold with no curve behind it is a guess, and every
  threshold in the predecessor project was a round number no measured population
  could reach.
- **A gate may not be loosened without a reason in the same commit.** This is
  the Goodhart defence: a worker optimising against the gate suite rather than
  the intent will start loosening gates, and that is measurable.

## 6. Gate primitives

Ecosystem-neutral primitives, driven by a Rust adapter. Each returns a verdict
plus structured evidence; none of them returns a bare boolean.

| Primitive | Question | Evidence |
| --- | --- | --- |
| `checks_green` | do the project's own checks pass? | `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, per adapter |
| `dependency_surface_unchanged` | did production dependency surface change? | `Cargo.toml` direct deps, features, default-features |
| `tests_not_deleted` | were tests, benches or fuzz targets removed or skipped? | AST scan for `#[test]`/`#[ignore]` and `cargo test -- --list` delta |
| `paths_unchanged` | were frozen paths touched? | `git status`, `git diff` |
| `public_api_unchanged` | did a public item's signature change? | AST + `cargo public-api`-style diff against baseline |
| `unsafe_surface_unchanged` | was unsafe surface added? | `unsafe` blocks, `impl`/`extern` blocks, `unsafe fn` |
| `suppressions_not_widened` | were diagnostic suppressions broadened? | `#[allow]`, `#[expect]` added or widened |
| `secret_absent` | were credentials introduced? | pattern scan over the diff |
| `external_tool` | did a third-party gate pass? | argv, exit code, SARIF findings |
| `judged` | residual judgement | opt-in, `escalate` severity only |

`checks_green` is the adoption escape hatch and the reason v1 is not blocked on
adapter completeness: a repository that already runs `cargo clippy` in CI gets
that coverage on day one with no Rulebound-specific configuration at all.
Everything else is the delta above that floor.

## 7. Orchestration

The loop is thin, and the thinness is the feature.

```text
worker events → bounded observation → run every declared gate
                                        │
                     ┌──────────────────┼──────────────────┐
                     ▼                  ▼                  ▼
                  accept              block             escalate
              (all gates green)  (evidence, always)   (judgement call)
```

Rules, each one a lesson from the predecessor project:

- **A gate may only block. Nothing a gate or a model says can accept work.**
  Acceptance is the conjunction of declared gates passing. There is no code path
  by which model output reaches `accept`.
- **Evidence is mandatory.** A block without the diff hunk, the gate id, the
  expected and observed value, and a stable fingerprint is a bug.
- **A gate that cannot produce a trustworthy verdict returns `error`,** never
  `pass` and never `block`.
- **No worker command is ever constructed from model output.** The command is
  operator- or project-owned.
- **Judgement gates are opt-in per gate, default to `escalate`,** and a project
  that declares none runs with no model in the loop at all.
- **The worker's environment is allowlisted.** "Secrets never reach the model"
  is only true if secrets never reach the worker. An empty allowlist means no
  inherited variables, not all of them.
- **Bounded observation.** The diff, including untracked files, is clipped to a
  declared budget, and the clip is a hard bound — reserve the truncation marker
  inside the budget, do not append it after the split.

## 8. Explicit non-claims

Carried forward, because every component of the predecessor project overstated
what it could do:

- Rulebound does **not** judge whether code is good. It runs what the project
  declared.
- It does **not** prove semantic equivalence, assess security, or replace a
  linter — no more than `slop-gate` does, which says so in its own README.
- Deterministic does not mean complete. A contract checking dependencies and
  green tests will pass a change that is compliant and strategically wrong. That
  lives in `judgement.not_covered`, which is why that section is mandatory.
- A gate that checks form instead of intent is theatre. The contract's value is
  bounded by the discipline of the people writing it, and no tool fixes that.
- Zero false positives is a measured claim about a corpus, never a property of
  the software.

## 9. Success criteria — **AMENDED 2026-09-29**

> This section replaces criterion 1 and the three-repository validation design.
> The reasoning is in `PLAN.md` §7. In short: a screen of 12 popular Rust
> repositories found that **none of them write down code-quality rules** — the
> large contribution guides are about build environments, editor setup and
> release process. Criterion 1 therefore had a near-zero denominator on exactly
> the repositories it was meant to be measured on, and the 4-of-4 result in
> `EVIDENCE.md` §6 turns out to have been circular: those fixtures were written
> *as* gates and then transcribed back out of gate form.
>
> The product is unchanged. The *question* is different, and the question is
> the one the evidence can actually answer.

The amended claim: **Rulebound is a standard for building Rust projects with
agents, and its contract is the artefact.** The gates are commodity — `cargo
test`, `clippy` and `fmt` are everywhere, and `slop-gate` already exists. What
does not exist in any repository we examined is a single dated, owned artefact
saying what this project's bar is and what it does not cover. That is the
product; the gates are the content it holds.

Validated when all of these hold:

1. **A generated contract finds real problems in real agent-authored changes,
   at an acceptable false-positive rate.** This replaces transcription rate,
   which is tautological when Rulebound writes the contract. The experiment:
   generate a contract for a repository the tool has never seen, apply it to
   real merges, and have a human judge each finding. A finding is a false
   positive only if a human says the code was fine; no ground-truth labels are
   needed, which is what makes this measurable at all.
2. The full suite runs on every commit inside the repository's existing CI
   budget, with no model in the loop.
3. **Zero false positives at `error` severity** over the corpus, measured and
   published with the calibration. This is now the *primary* criterion rather
   than one of four, because with a generated contract there is no
   transcription question left to answer.
4. `judgement.not_covered` is non-empty, specific, and reviewed. An empty list
   means the contract claims more than it delivers, and the tool should warn.
5. **The standard survives contact with at least two projects the author did
   not write.** This is new, and it is the criterion that decides whether this
   is a product or a personal tool. Every repository available to us is
   agent-written by one author, who is also the person choosing the gates and
   judging the findings. A false positive on our own corpus is a bug report; on
   someone else's project it is the reason they turn it off.

Criterion 5 cannot be satisfied by us alone and is named here so it is not
quietly dropped.

## 10. Open questions

- **How much real-world rule mass is gateable?** The measurement we have least
  of, and the one criterion 1 exists to get.
- **Baseline artefacts.** `slop-gate` needs one, keyed to commit, policy, tool
  version and build fingerprint. Reproducibility demands it; adoption cost
  argues against it. A gate with no baseline can only ask "is this bad", never
  "did this get worse" — and "did this get worse" is the question worth asking.
  Likely answer: derive the baseline from CI cache storage, as `slop-gate`
  already does, rather than asking contributors to manage it.
- **Does the judgement tier earn its place?** It is the only part that can fail
  in a new way, and in the evidence it reached 55% recall at 100% specificity.
  It may be correct to ship without it.
- **Who owns the contract?** A file nobody reviews is worse than no file,
  because it manufactures confidence. The `reviewed` date is the only forcing
  function in the design; it may not be enough.
- **Naming.** `Rulebound` is a placeholder. `foreman` is taken. Other candidates:
  `Attest`, `Warden`, `Cordon`, `Rampart`.

## 11. Inputs for whoever builds this

- `EVIDENCE.md` — every measurement, including the negative ones, and the
  harness bug that confounded them.
- `REFERENCE-slop-gate.md` — the prior art, and seven design decisions to take
  from it directly.
- `results/` — raw per-question scores for each run.
