# Reference: slop-gate, and what to take from it

Source: <https://github.com/mrorigo/slop-gate> (README, fetched 2026-09-28).
Author: mrorigo. Rust only at time of writing. Read this as a competitor-free
prior art: it solves one slice of the problem in this PRD, and it solves it
well enough that the design should start from what it already got right.

## Why it matters here

`slop-gate` is a deterministic, model-free, network-free CI gate for exactly
the rule class that no classifier tested in this project could handle: "keep
the architecture clean." Its README opens with "It runs locally, uses no model,
and makes no network request during analysis."

That is the thesis of this project, already implemented and shipped in one
ecosystem. It is also the strongest available evidence that the thesis is not
a demo.

## What it catches

| Rule | Question | Measurement |
| --- | --- | --- |
| `function-mass` | Did this change add too much function-level complexity? | `CC × √SLOC`, and the delta from the base function |
| `near-clone` | Did this change add an implementation that already exists? | token-shingle **and** AST-shingle similarity, both thresholds required |
| `lint-suppression-growth` | Did this change weaken a compiler or Clippy diagnostic? | added or broadened `allow` / `expect` |
| `unsafe-surface-growth` | Did this change add unsafe surface? | unsafe blocks, declarations, impls, extern blocks |
| `dependency-surface-growth` | Did this change expand production dependency surface? | new direct edges, added features, default features |
| `structural-erosion` | Is complexity concentrating in already-complex functions? | high-complexity mass ÷ total function mass |

`dependency-surface-growth` is our rule 1, generalised past Python and past
"did the manifest change" to "did the production dependency *surface* change",
which catches added features and default features that a manifest diff misses.

## Seven design decisions worth stealing

### 1. A baseline artifact, keyed to everything that affects the answer

You cannot ask "did this get worse" without a "before". `slop-gate index`
builds an artifact from a trusted commit; `check` evaluates the candidate
against it. The artifact is bound to the exact base commit, the active policy,
the tool version, and a build-script fingerprint of the analyzer sources. Any
of those changing is a hard error that names both versions:

```text
slop-gate check: invalid artifact tool version: index was built by slop-gate
0.4.0, this is slop-gate 0.5.0; rebuild the index with slop-gate index
```

This is reproducibility as an enforced property, not a promise. A gate that
cannot name the version of itself that produced its baseline is not auditable.

### 2. Unknown keys are errors

"Unknown keys are errors, so a misspelled rule cannot silently weaken a gate."

This is the single most important line in the README for our purposes. A
configuration typo that silently disables a gate is the exact failure mode that
turns a gate suite into theatre. This should be non-negotiable in the gate
language of this project.

### 3. Suppressions require a reason

A suppression must name a supported rule, use a repository-relative path, and
include a reason. The README advises using them sparingly and preferring a
refactor.

This is the audit trail for rule-waivers. Without it, a gate suite erodes one
innocent-looking exception at a time and nobody can reconstruct the intent.

### 4. `warn` by default, and a calibration protocol before promoting to `error`

Missing configuration uses warning-only defaults. Severity is
`off | warn | error`. There is a documented calibration protocol to follow
*before* changing a rule from `warn` to `error`.

This is the discipline this project needed and did not have. We repeatedly
learned that a threshold fitted on the states it was measured against is not a
threshold. `slop-gate` institutionalises "do not make a gate blocking until you
have calibrated it" instead of leaving it to judgement.

### 5. Exit code 2 is not exit code 1

| Code | Meaning |
| --- | --- |
| 0 | completed, no error-severity findings |
| 1 | completed, at least one error-severity finding |
| 2 | configuration, Git, artifact, or analyzer failure prevented a trustworthy result |

This is a distinction the preceding project got wrong and had to be taught. A
verifier that could not start returned `returncode: None`, which mapped onto
"tests failed" — the two states that must never be confused. Code that cannot
produce a trustworthy verdict must say so in a way that cannot be mistaken for a
verdict. See `EVIDENCE.md`.

### 6. Meta-gates against gate-gaming

`lint-suppression-growth` detects a change that weakens the compiler or Clippy
diagnostics — added or broadened `allow` / `expect` attributes.

This is the answer to the Goodhart objection, and it is elegant: the tool
audits the *other* tools' suppressions. A worker optimising against a gate
suite rather than against intent will start adding suppressions, and this rule
sees it. The generalisation for this project is a required gate that fails when
the gate configuration itself is loosened without a recorded reason.

### 7. Thresholds chosen from a measured curve, and published

Block detection defaults to 120 tokens, justified with a table of how many
findings survive at 40, 60, 80, 120 and 160 tokens on a 9.3k-line repository.
The stated reason: the default sits at the knee, keeping the recall that
motivates the rule while reporting only duplication substantial enough to
extract.

Publishing the curve behind a threshold is the discipline that would have saved
this project several times. Every threshold in the preceding supervisor was a
round number that no measured population could reach; `EVIDENCE.md` is largely a
post-mortem on that.

## Two more details worth copying

**Report the actionable quantity, not the raw one.** A clone-family summary
reports `duplicate_mass` — the family total minus its largest member — as the
recoverable mass, alongside the raw `family_mass`. "Reporting the family total
overstates the available refactor by roughly the size of the function that would
be kept."

**Risk-tier findings, and a stated scope.** Clone findings carry `clone_scope`
(whole-function or block), `clone_risk` (high when both sides are production
logic, medium when one is a constructor, low when either is a test or an
accessor), `left_role` / `right_role`, and `same_file`. A test-to-test clone is
not the same event as a copy-pasted parser.

## Deliberate boundaries it draws

> "It does not prove semantic equivalence, assess security, or replace a linter."

and a table dividing labour with `cargo fmt --check`, `cargo clippy` and
`cargo audit`. It positions as a complement, not a replacement, and says so in
the README rather than in a footnote.

This project should adopt the same discipline. Every component we have built has
overstated what it could do; the honest version of this PRD has to be explicit
about the seams.

## Known limits and what is missing

- **Rust only.** The gate language is Rust; parsing is `tree-sitter-rust-orchard`.
  It rejects files with parser error nodes rather than applying a
  macro-specific recovery workaround. Porting is the obvious next step and the
  obvious ask.
- **Not semantic.** "Keep the architecture clean" is approximated by six
  measurable proxies. Two clean architectures will be flagged; one clean-looking
  mess will not.
- **Findings need a human or a review tool.** Output is `human`, `json` or
  `sarif`. It does not make decisions; it emits evidence.

## What this means for the PRD

The deterministic-gate thesis is not speculative. It is shipped, in Rust, by
one person, and it is measurably better at "did this change make the codebase
worse" than any model we tested. The PRD should be read as generalising that
tool to a cross-language gate contract with an orchestrator around it — not as
proposing an idea that needs defending.
