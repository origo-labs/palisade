# Palisade — build plan

Derived from `PRD.md` (what), `EVIDENCE.md` (what we know and what failed),
`REFERENCE-slop-gate.md` (prior art worth copying). This document is *how*:
decisions that close the PRD's open questions, module layout, the verdict
algebra, milestones with exit criteria, and the test strategy that keeps the
claims honest.

Every design choice below traces to something in those three documents. Where
this plan adds a decision the PRD left open, it is marked **[decision]** and
carries the reason, so it can be argued with rather than rediscovered.

---

## 0. Ground rules

These are not aspirations; each is a bug that already happened once.

1. **A gate may only ever subtract.** There is no code path from any gate
   result — deterministic or judged — to `accept`. Acceptance is the
   conjunction of declared, enabled, non-`off` gates returning `pass`.
   Enforced by a unit test that asserts the type system makes it unrepresentable
   to construct an `accept` from a `Judged` value.
2. **`error` is a first-class outcome, not a `block` and not a `pass`.** A gate
   that cannot produce a trustworthy verdict says so. Predecessor bug: a
   verifier that failed to start returned `returncode: None`, which the caller
   mapped onto "tests failed". See `EVIDENCE.md` §5.
3. **Unknown is an error.** Unknown contract keys, unknown check names, unknown
   severities, unknown enum variants in gate output. Misspelling must never
   weaken a contract. (`slop-gate` decision 2.)
4. **Observations are collected on a dirty tree.** `git diff` reads unstaged
   changes. If a state is committed before observation, the diff is empty and
   the whole system scores at chance while reporting success. This was the
   single most expensive bug in the predecessor programme. See `EVIDENCE.md`
   "apparatus bugs" and §3 — the classifier's worst failures were on states
   with empty diffs. **There is a regression test for this, in M0, before any
   gate exists.**
5. **Three targets, never conflated.** Acceptance ("is it tested"), rule
   violations ("does this break a stated rule"), and residual judgement
   ("is this on-topic") are three separate fields in the output schema with
   three separate producers. `EVIDENCE.md` §5. The output type must make
   combining them a compile error, or a reporting bug will do it for us.
6. **Deterministic does not mean complete.** Every emitted artefact states what
   it did not cover. `judgement.not_covered` is mandatory, may be empty, and
   must carry a `reviewed` date.

---

## 1. Decisions closing the PRD's open questions

### 1.1 Judgement tier: not in v1 **[decision]**

PRD §10 asks whether the judgement tier earns its place. It does not yet.
Evidence for it is 55% recall at 100% specificity on 16 states, with the framing
tuned on those same states (`EVIDENCE.md` §2, §9), and recall 55% is
explicitly "not adequate for one that blocks unattended work" — though
`escalate` is exactly a tier that does not block.

**v1 ships with no model in the loop at all.** The `judged` check exists in the
schema, is `escalate`-only, and its implementation returns
`GateOutcome::NotImplemented` — which by rule 2 is `error`, not `pass`. A
project that declares one gets a loud, correct failure instead of a silent
regression to a heuristic.

The design is built so this is additive. `Origin` already has exactly the two
variants a judgement could not extend, the verdict algebra already routes
`escalate` separately, and M6 adds a third gate kind — one that can only ever
produce a `Fail` — without touching the reduction or either existing kind.
Shipping the seam now and the body later is strictly better than shipping the
body and discovering the seam was load-bearing.

**Exit gate for M6** (must all hold before the tier is enabled by default):
a fresh holdout set, never used for framing, with recall and specificity
published alongside the AUC table. If recall < 80% on that set, the tier stays
opt-in and documented as such.

### 1.2 Baselines: no bespoke artifact in v1 **[decision]**

PRD §10 flags the baseline tradeoff. `slop-gate` needs an artifact keyed to
commit + policy + tool version + build fingerprint. Reproducibility demands it;
adoption cost argues against asking contributors to manage one.

**v1 uses the git object store as the baseline.** `base` is a commit-ish;
gates read the base tree with `git show <base>:<path>` and the head state from
the working tree. The base commit is immutable, content-addressed, already
versioned, and already subject to the repository's own retention policy. This
is the "derive the baseline from CI cache storage" answer generalised one step
further: git already *is* the cache.

This makes most v1 gates **two-tree diff gates** (compare base tree to worktree
tree) rather than single-tree checks, which is both more correct and one less
moving part. A bespoke `palisade index` artifact is deferred until a
measurement shows `git show` is the bottleneck — not before. We do not build
reproducibility machinery for a performance problem we have not measured.

Exception: `external_tool` gates hand `{base}`/`{head}` to `slop-gate`, which
maintains its own artifact. That boundary is explicit; we do not wrap it.

### 1.3 Who owns the contract: make staleness mechanical **[decision]**

PRD §10: "a file nobody reviews is worse than no file". A `reviewed` date that
nothing reads is a comment.

Two built-in gates ship in M4, neither configurable, both default `warn`:

| Built-in | Fires when |
| --- | --- |
| `contract_review_stale` | `[judgement].reviewed` is older than `review_interval_days` (default 180) |
| `contract_not_loosened` | the contract file itself changed in this diff, and a gate was disabled, downgraded `error`→`warn`, had a suppression added, or had a threshold loosened — without a `reason` field in the same diff |

`contract_not_loosened` is the generalised `slop-gate` decision 6: the tool
audits its own configuration against the Goodhart attack. A worker optimising
against the gate suite rather than the intent will start loosening gates, and
that is a diff-visible event. Making it a gate rather than a convention is the
whole point.

Both are `warn` in v1. Promotion to `error` is a *project's* decision after
calibration, per the contract language rules.

### 1.4 Naming: `Palisade` stands for v1 **[decision]**

Renaming costs a spec rewrite and buys nothing measurable. Revisit at first
external release.

### 1.5 Rule mass: the measurement, not a feature **[decision]**

PRD §9 criterion 1 ("a majority of stated rules transcribe into gates") is the
product-vs-demo test. It is not a build task; it is M5, and it requires
corpora we do not control. M5 is scheduled as if it might fail, because if it
fails, the PRD's thesis is wrong and that is worth knowing in month four rather
than month eighteen.

### 1.6 Observation budget: reserve the marker inside the budget **[decision]**

PRD §7 already says it. Made concrete because getting it wrong is a silent
correctness bug, not a crash: budget N bytes, allocate N − len(marker), clip,
append marker. Total never exceeds N.

---

## 2. Repository layout

```
palisade/
  Cargo.toml                    # workspace
  .github/workflows/ci.yml      # fmt, clippy, test, boundary, no-network, deny
  scripts/check-boundary.sh     # "only git and exec may spawn", enforced
  crates/
    palisade-contract/          # palisade.toml: parse, validate, deny-unknown
    palisade-git/               # the only crate that shells out to git
    palisade-observe/           # bounded observation capture (dirty tree!)
    palisade-ast/               # tree-sitter-rust-orchard wrapper, cached parse
    palisade-gates/             # Analyzed gates: one module per primitive, no I/O
    palisade-exec/              # Delegated gates: the only crate that spawns
                                #   non-git processes. Sole constructor of
                                #   Origin::Delegated.
    palisade-orchestrate/       # verdict algebra, precedence, exit codes — pure
    palisade-report/            # human | json | sarif 2.1.0
    palisade-cli/               # binary: `palisade check`, `palisade observe`
    palisade-testkit/           # fixture repo builder. Dev-dependency only.
  docs/
    CONTRACT.md                 # generated from the schema, checked in CI
```

Integration tests live in the crate that owns the behaviour, not in a
top-level `tests/`: `crates/palisade-orchestrate/tests/verdict_algebra.rs`,
`crates/palisade-observe/tests/dirty_tree_regression.rs` and `.../budget.rs`.
A single top-level directory would mean the tests could not see the crates'
private internals, and more importantly it would blur which boundary each test
is exercising.

`palisade-testkit` is a tenth crate rather than a `tests/fixtures/` directory
of committed repositories. It builds fixtures deterministically, which means
"here is how to make a dirty tree" is written down exactly once — and that
particular knowledge is the thing the predecessor programme got wrong, so it
gets one home rather than a copy per test file.

Six boundaries that matter, each enforced by a lint or a test rather than
convention:

- **`palisade-git` and `palisade-exec` are the only crates that spawn
  processes.** Everything else takes data. This is what makes gates
  unit-testable against fixtures with no repo and no subprocess.
- **Gates come in two kinds, and the type says whose verdict it is.**
  `Analyzed` gates are Palisade's own AST and diff analysis and perform no I/O:
  `fn(&Observation) -> Vec<Finding>`. `Delegated` gates are a trusted external
  process's verdict plus its evidence, constructed only by `palisade-exec`:
  `checks_green` (`cargo fmt`/`clippy`/`test`) and `external_tool`. The
  distinction is epistemic — "we diffed it" versus "cargo said so" — and it
  belongs in the type rather than in a field someone has to remember to fill
  in. Both flow into the same reduction, which stays pure.
- **`palisade-gates` has no `std::process` in its dependency graph.** Enforced
  by a `cargo-deny`/`cargo-machete` style check in CI, not by review. This
  invariant is what keeps analyzed gates deterministic and calibratable, and
  it is the reason the crate split exists.
- **`palisade-ast` caches parses per file content hash.** Gates re-parse
  otherwise, and the per-commit budget dies.
- **`palisade-orchestrate` knows nothing about TOML, tree-sitter, or
  processes.** It consumes a validated `Contract` and a `Vec<Finding>` and
  returns a `Verdict`. Pure function, fully property-testable, and it stays
  that way *because* process execution lives in `palisade-exec`.
- **`palisade-report` is the only place SARIF is produced.** One serialiser, no
  ad-hoc JSON in gates.
- **No model client crate exists in v1.** Not stubbed, not feature-gated —
  absent. Adding it in M6 is a visible diff.

### 2.1 Dependencies (v1, pinned)

| Crate | Why |
| --- | --- |
| `tree-sitter` + `tree-sitter-rust-orchard` | same parser as `slop-gate`; findings line up across the two tools |
| `serde`, `serde_json` | contract + report |
| `toml` (0.8+, with span info) | span-carrying parse errors are the difference between "line 41" and a wall of text |
| `camino` | git paths are UTF-8 on the wire; on disk they are not |
| `blake3` | content-addressed parse cache + observation fingerprints |
| `clap` (derive) | CLI |
| `rayon` | gates are embarrassingly parallel and independent |

No async runtime. No HTTP client. No model SDK. If a dependency in this table
ever needs the network at build or run time, that is a PRD amendment, not a
Cargo change.

---

## 3. The verdict algebra

This is the heart, and it is small enough to prove correct by exhaustion.

```rust
// Deliberately has NO `Judged` variant. A judgement can only ever be a `Fail`
// at escalate severity, so "a model said this is fine" is not expressible as
// a pass. That is the type system, not a convention.
enum Origin {
    Analyzed { primitive: Primitive },            // Palisade's own analysis
    Delegated { tool: ToolId, version: String }, // a trusted process said so
}

enum GateOutcome {
    Pass { origin: Origin },
    Fail(Finding),          // Finding carries its own Origin
    Untrustworthy { reason: UntrustworthyReason, origin: Origin },
    Skipped { reason: SkipReason },
    // `NotImplemented` is a variant of UntrustworthyReason, not its own
    // outcome: an unimplemented primitive produces no trustworthy verdict,
    // which is precisely what `Untrustworthy` means.
}

enum Verdict { Accept, Block, Escalate, Error }
```

A `Finding`'s `origin` is not a free field to be filled in — it is
constructed from the `Origin` on the `GateOutcome` that produced it, by
`Analyzed` gates carrying `Primitive` and by `palisade-exec` carrying
`ToolId`+`version`. There is no constructor that lets a gate claim a
provenance it did not earn.

Two predicates on `GateOutcome`, kept distinct because conflating them is
itself a bug this milestone hit:

- `is_adverse` — the outcome carries news: a `Fail` or an `Untrustworthy`.
  Every adverse outcome is reported.
- `rules_out_accept` — the outcome is *incompatible* with `Accept`. Strictly
  stronger: a `warn` finding is adverse and perfectly compatible with
  acceptance, which is what `warn` means and why PRD 5 makes it the default
  severity. Only `error`/`escalate` findings and untrustworthy gates rule out
  accept.

The second is the one monotonicity is stated over, and stating it over the
first is wrong — it would assert that a gate can *un*-accept a verdict, which
is the opposite of the rule.

### 3.0 The two gate kinds, and why they are separated

`checks_green` and `external_tool` are not the same kind of thing as
`dependency_surface_unchanged`. The first two are "run this and read the exit
code"; their trustworthiness is inherited from a third party. The rest are
Palisade's own AST and diff analysis; their trustworthiness is ours. Folding
them into one shape would mean either a `Vec<Finding>` nested inside a
`GateOutcome` with a second flattening path, or a fake "one gate, three
sub-results" abstraction that both primitive kinds have to distort themselves
to fit.

Instead the two kinds have separate shapes — `Analyzed` returns
`Vec<Finding>` from a pure function, `Delegated` is constructed only by
`palisade-exec` — and both reduce through the same pure function. The payoff
is that the exit-code-2 → `Untrustworthy` rule, the single most
safety-critical piece of code in the tool, gets its own crate and its own
exhaustive test table. `EVIDENCE.md` §5 is that exact rule's failure mode: a
verifier that could not start and a verifier whose tests failed were
indistinguishable to the caller, and it confounded an entire measurement
programme. It does not get a home in the least-tested layer.

`Skipped` is deliberately **not** `Pass`. A gate skipped because it is `off`
does not participate in the conjunction, and is reported in the output as
`off` — a reader can always see what was not checked. A gate skipped for any
other reason (missing tool, unparseable file) is `Untrustworthy` and forces
`Error`.

Precedence, and it is total:

```text
any Untrustworthy           -> Error        (a gate that could not run has not passed)
any NotImplemented declared -> Error
any contract parse/validate failure -> Error
any gate changed this diff  -> Warning      (non-blocking advisory, never a verdict)
any Fail with severity=error       -> Block
any Fail with severity=escalate    -> Escalate
any Fail with severity=warn        -> (nothing)   # recorded, still Accept
otherwise                          -> Accept
```

Two properties this buys, both tested in `verdict_algebra.rs`:

- **Monotonicity.** Adding a new failing gate can never move `Block` to
  `Accept`. Adding a new *untrustworthy* gate can never move any verdict to
  `Accept`. Tested as a property over generated gate vectors.
- **No acceptance from a model.** `Verdict::Accept` is only constructible in
  `orchestrate` from a conjunction over `GateOutcome` variants that carry no
  model provenance. In M6, `Judged` becomes a `Fail` with
  `severity: escalate` and a forced `provenance: Judged` marker; the property
  test asserts no input containing that marker yields `Accept`.
- **Provenance is total.** Every `Finding` and every `Pass` has an `Origin`,
  `Origin::Delegated` is constructible only by `palisade-exec`, and `Origin`
  has no `Judged` variant at all. A test asserts the JSON and SARIF outputs
  both carry it, because a report that cannot say whose verdict it is cannot be
  audited.

### 3.1 Exit codes

Aligned with `slop-gate` decision 5, extended by one:

| Code | Verdict | Meaning |
| --- | --- | --- |
| 0 | `Accept` | every enabled gate passed |
| 1 | `Block` | ≥1 error-severity failure |
| 2 | `Error` | no trustworthy verdict could be produced |
| 3 | `Escalate` | ≥1 escalate-severity finding, none blocking |

`Error` is 2, not 1. That ordering is the whole lesson from the predecessor
project and it is the one thing in this design most likely to be "simplified"
away by a future contributor. It gets a comment, a test, and a line in
`docs/CONTRACT.md`.

`palisade-exec` owns the whole table, and it is the reason the crate exists:

| Process outcome | `GateOutcome` |
| --- | --- |
| exit 0 | `Pass` |
| exit 1 | `Fail`, one finding per reported diagnostic |
| exit 2 | `Untrustworthy` — the tool could not produce a trustworthy result |
| not found on `PATH` | `Untrustworthy` |
| timed out | `Untrustworthy` |
| killed by signal | `Untrustworthy` |
| stdout not parseable as the declared `format` | `Untrustworthy` |
| any other non-zero code | `Untrustworthy`, not `Fail` |

The last row is the belt-and-braces version of the same lesson: an exit code
Palisade does not recognise is not a verdict it is entitled to interpret.
Only 0 and 1 mean anything, and only because the tools we invoke document
that; everything else is "no trustworthy verdict", which is `Error`.

### 3.2 Finding: what a block is made of

PRD §7: "A block without the diff hunk, the gate id, the expected and observed
value, and a stable fingerprint is a bug." So `Finding` is not a free-form
string:

```rust
struct Finding {
    gate_id: GateId,
    primitive: Primitive,
    severity: Severity,
    path: Utf8PathBuf,          // repo-relative
    hunk: Option<HunkRef>,      // line range in the *observed* file
    expected: String,           // what the contract/base said
    observed: String,           // what was found
    message: String,            // human sentence, no facts not in the fields above
    fingerprint: Fingerprint,   // blake3(gate_id, path, rule params, normalized finding)
    origin: Origin,             // Analyzed | Delegated { tool, version }
}
```

`fingerprint` is what lets CI tell "the same known finding" from "a new one",
and what lets suppression be scoped to a finding rather than a file.
`provenance` is what keeps the three targets from being conflated downstream,
and makes "was this caught by a model" answerable in the artefact itself. It is
derived from the producing `GateOutcome`, never supplied independently — see
§3.

Report formats: `human` (default, for a PR comment), `json` (machine, the
three targets as separate top-level keys), `sarif` (2.1.0, so it lands in GitHub
code scanning with no extra work). SARIF `partialFingerprints` carries
`fingerprint`; `properties` carries `expected`/`observed`/`provenance`.

---

## 4. The contract

`palisade.toml` at repo root. Strict `toml` with `deny_unknown_fields`
everywhere, custom `Deserialize` for every enum (so `"erorr"` is a parse error
naming the field, not a default), and span-carrying errors.

Schema is what PRD §5 shows, plus four additions this plan makes:

1. **`[budget] observation_bytes = 131072`** — explicit, because "bounded" with
   no number is unbounded in practice.
2. **`[baseline] ref = "origin/main"`** — makes the two-tree model legible in
   the file. `{base}` templating resolves from here.
3. **Per-gate `reason` is required when the gate changed** — feeds
   `contract_not_loosened` (§1.3).
4. **`[[gates]] consumes` / `provides`** — declared edges between a `Delegated`
   gate and the `Analyzed` gates that read its output. `tests_not_deleted`
   needs `cargo test -- --list` from `checks_green`, so it must declare it.
   Pretending the gate list is an unordered set would hide a real dependency
   on a subprocess result. `provides`/`consumes` are validated as a DAG at
   contract load, and an undeclared edge is an error rather than a
   nondeterministic read of an unpopulated slot.

`docs/CONTRACT.md` is generated from the schema types and **diffed in CI**.
Documentation that can drift from the parser is worse than none; the CI check
makes the drift a build failure.

Thresholds: every threshold the contract can set must name the curve it came
from (`calibration = "curve:2026-11-rs-corpus-n420"`), per PRD §5.
`cargo metadata` is available at runtime, so a contract that requires a
threshold with no recorded calibration is a **validation error**, not a
warning. This is stricter than `slop-gate` and it is deliberate: we have already
paid for round-number thresholds once (`EVIDENCE.md` §7, §8).

---

## 5. Milestones

Each milestone ends with a checkable exit criterion. No milestone starts before
the previous one exits, and M0's regression test is written before any gate
code exists.

### M0 — Skeleton and the apparatus bug — **shipped**
- Workspace, all ten crates, stubbed boundaries. `palisade-exec` ships empty
  in M0 and is first populated in M3; its existence from the start is what
  keeps the boundary from being retrofitted later.
- `palisade-git` with exactly the operations gates need: `rev_parse`, `status
  --porcelain -z`, `diff` (unstaged **and** staged, and untracked file
  contents), `show`, `merge_base`.
- `palisade-observe` with a hard byte budget and the marker-inside-the-budget
  rule.
- **`crates/palisade-observe/tests/dirty_tree_regression.rs`**: a fixture repo
  with one unstaged edit. Asserts the observation is non-empty and contains
  the edit, and that the same fixture *committed* yields an explicitly-flagged
  empty diff rather than a silent success. This is the `EVIDENCE.md` bug,
  frozen as a test, before it can recur.
- `palisade-orchestrate` verdict algebra with no gates registered.
- CI: fmt, clippy `-D warnings`, test, boundary check, no-network-in-tree,
  `cargo deny`.

**Exit — met.** A commit with no changes to the fixture yields an explicitly
attributable empty observation; an unstaged change yields a non-empty one. The
verdict algebra passes over every vector of length ≤3 drawn from a
nine-element alphabet (585 vectors), plus a characterisation test that
`accept` holds exactly when nothing rules it out. 53 tests, clippy clean under
`-D warnings`, `cargo fmt --check` clean.

**Two findings from building it, both worth more than the code:**

1. **`git diff` does not include untracked files, however you spell the flag.**
   `-u`/`-U` is unified context width; `diff` has no `--untracked-files`. A
   supervisor whose observation omits new files is observing nothing where the
   work usually is, and every gate reading a new file's content would be
   silently blind. The first implementation of `diff_unstaged` carried a
   comment asserting otherwise, and the regression test caught it. Untracked
   content is now synthesised with `git diff --no-index` and labelled, so its
   provenance stays visible in the diff itself.

2. **Anchoring to a base commit makes the predecessor's harness bug a
   non-event.** `git diff <base>` is base-vs-worktree and still sees committed
   work; `git diff` alone is worktree-vs-index and goes blind the moment
   someone commits. The regression test was originally written asserting the
   *buggy* behaviour was expected, which was wrong in a useful way — the
   two-tree model from §1.2 means the mistake is survivable, and the remaining
   hazard (an index-anchored run) is reported as `diff_expected_but_absent`
   rather than as a clean bill of health.

### M1 — Two-tree diff gates — **shipped**
Three gates, chosen because they need no AST and no subprocess, so they
validate the whole pipeline cheaply. 95 tests, 36 of them the M1 exit
criteria.

**The blocker M0 left open, and how it was resolved.** A two-tree gate needs
the base side of a file, and `git show <base>:<path>` is a subprocess — which
gates may not spawn. M0's `Observation` carried only diff *text*, so no
two-tree gate could run at all. The fix: **the two-tree view is materialised
into the observation** during capture. `FileView` carries `path`, `orig_path`,
`base`, `head` and a `truncated` flag, capped per file. The fetch happens once,
in `palisade-git`; the *result* is what a gate receives. This is why the
boundary holds and why gates stay testable against a hand-built view.

The second consequence: a gate that needs an uncapped file must return
`Untrustworthy` rather than reason about half a manifest, because half a
`Cargo.toml` is a wrong answer rather than a partial one.

Gates:
- `dependency_surface_unchanged` — parses `Cargo.toml` as TOML and diffs the
  declared production surface: `[dependencies]`, `[target.*.dependencies]`,
  `[features]`, `default-features` flags and per-dependency version
  requirements. Deliberately **not** `[dev-dependencies]` (not production
  surface) and **not** the resolved graph in `Cargo.lock`. The `default-features`
  and version checks are the part a manifest *diff* misses, which is why
  `slop-gate` generalises the rule to surface rather than to "did the manifest
  change". `cargo metadata` remains the escalation if M5's corpus shows the
  direct parse misses real changes; it is a build-graph dependency and would
  make this a `Delegated` gate.
- `tests_not_deleted` — **M1's text counter, replaced in M2 by the AST.** See
  the M2 note below; comparing test *identities* rather than a count catches a
  delete-and-replace pair that a count structurally cannot, and removing the
  `is_test_path` heuristic removed two false-positive modes at once.
- `paths_unchanged` — component-wise prefix matching, so `fixtures_extra/` is
  not inside `fixtures/`. Declaring no paths is `Untrustworthy`, not a pass.

**Three findings from building it:**

1. **`git status` is the wrong command for the file set.** The two-tree view
   was first built from `git status --porcelain`, which is worktree-versus-index
   and therefore *blind to committed work* — exactly the case the two-tree
   model exists to cover. The end-to-end tests caught it: after a commit, the
   view was empty and every gate saw nothing. It is now built from
   `git diff --name-status -z <base>` (which also carries rename pairs) with
   untracked paths unioned in from status. This is the same *class* as M0's
   untracked-file finding: asking git the wrong question and reading the empty
   answer as "there is nothing here".

2. **Attribute matching that fails silently makes a gate look calibrated.**
   `is_attr` compared the text after `#[` against the bare name, so for
   `#[test]` it compared `"test]"` to `"test"` and matched *nothing*. Every
   count was 0→0, and the "a test was added" test passed **vacuously**. The
   attribute path is now parsed properly, so `#[test]`, `#[test = "x"]` and
   `#[tokio::test]` match and `#[testify]` does not. A gate that counts zero of
   everything is indistinguishable from a gate that works, which is exactly why
   the must-not-fire fixtures exist.

3. **"Not in the view" means "unchanged" — and only because of (1).** The
   dependency gate returns `Clean` for an absent `Cargo.toml` rather than
   `Untrustworthy`, which is sound precisely because the view is derived from
   the diff of the same two trees the gate compares. Under the old,
   status-derived view that same branch made the gate report `Untrustworthy` on
   every repository whose manifest the current edit had not touched. The
   argument is written out at the branch, and
   `an_unchanged_repository_produces_no_findings_from_any_m1_gate` pins it.

**Scope changes, both forced and both recorded:**

- **The contract TOML parser moved up from M3.** No configurable gate can
  exist without it — `paths_unchanged` needs frozen paths and
  `dependency_surface_unchanged` needs an allow-list. M1 would otherwise have
  been a library nothing could run.
- **`secret_absent` moved out to its own milestone (M1.5).** A pattern-matching
  secret gate is the classic crying-wolf liability, and calibrating it honestly
  means measuring its false positives on a corpus rather than averaging it into
  a milestone that has measured nothing. It stays a declared primitive, so a
  project that wants it gets `Untrustworthy` — never a silent pass.
- **`unsafe_surface_unchanged` and `suppressions_not_widened` moved to M2.** The
  plan claimed they are "pure diff comparisons"; they are not. Counting `unsafe`
  blocks or `#[allow]` sets correctly needs the AST, and doing it by text would
  have put the same silently-wrong counting as finding (2) into a
  blocking gate.

**Exit — met.** Every gate has a firing fixture, a must-not-fire fixture, and a
two-tree test proving it is not a single-tree check; the must-not-fire set
includes the doc-comment `#[test]`, the `fixtures_extra/` sibling, the
`contest/` sibling, the dev-dependency, the manifest reformat, and an unchanged
repository. **Zero false positives across 36 gate fixtures.** The registry
returns `Untrustworthy` for all nine unimplemented primitives, tested
individually.

### M1.5 — `secret_absent`, and only after a corpus
A milestone of one gate, deliberately. A pattern-matching secret gate is the
classic crying-wolf liability: it fires on documentation, on test fixtures, on
lockfiles, and on base64 that merely looks like a key. A gate like that gets
switched off, and a project that switches off one gate has learned that
gates are advisory, which costs every other gate its authority too.

So this gate is not shipped until it has been measured:
- A pattern list where **every pattern ships with a documented
  false-positive example drawn from a real repository.** A pattern with no
  known false positive is a pattern nobody has tried to break.
- A measured false-positive rate over the M5 corpus, published with the
  pattern that produced each one.
- `warn` by default regardless of the result. Promotion to `error` is the
  project's decision, and per PRD 5 it needs a recorded calibration.

Scope, stated so the gate cannot be read as more than it is: it scans the
bounded observation for credential-shaped strings. It is not an entropy scan,
not a git-history scan, and not a replacement for a secret-scanning service. A
secret committed in an earlier commit is outside what it can see, and that
limitation belongs in `judgement.not_covered` rather than in a README.

**Exit:** a false-positive rate measured on a corpus, published, with every
pattern's worst known false positive named.

### M2 — AST and API surface — **shipped**
- `palisade-ast`: the `tree-sitter-rust-orchard` wrapper, the content-hash
  parse cache, and the refusal to guess on parser error nodes. 14 tests, and
  they pin the properties the gates rest on.
- `public_api_unchanged`, `unsafe_surface_unchanged`,
  `suppressions_not_widened` — the two M1 deferred, plus the API gate. 27
  fixtures, four of them end-to-end on a real repository.

**The false positive this milestone was built around.** `EVIDENCE.md` §6
records a new function returning `-> list[Note]` tripping a return-type check,
"which must compare against a baseline rather than look for a pattern". The
gate does compare against a baseline, and the fixture is
`a_new_function_returning_a_generic_type_is_not_an_api_break`. It also
resolves a question the PRD left open: **a new public item is reported at
`warn`, pinned in code, and cannot be promoted to blocking by the contract.**
A gate that blocks on every new public function gets switched off within a
week, and switching it off takes the removals with it. Removals and signature
changes stay at the contract's severity.

Other decisions taken here, each of which would otherwise have been a false
positive:
- **A function signature excludes its body.** Otherwise every implementation
  edit is an API-change finding.
- **Signatures are whitespace-normalised and trailing-comma-stripped.**
  Otherwise every `rustfmt` run is an API change. Pinned by
  `running_rustfmt_is_not_an_api_change`.
- **`pub(crate)` is not public API.** Half a real codebase's internals are
  crate-visible.
- **Items are identified by module path**, so moving a function between
  modules reads as a removal and an addition rather than a silent relocation.
- **Unsafe surface is four counts, not one.** Blocks, `fn`, `impl` and `extern`
  blocks are reported separately, and the trigger is *growth* — a file that
  already had two unsafe blocks and still has two is not a finding.
- **Suppression is about widening, not presence.** Narrowing and removing are
  improvements and never fire. Broadening *to a wildcard* (`clippy::all`) is
  reported separately from adding one lint, because it is a different
  magnitude of event.

**Two findings from building it:**

1. **A parser is not a given; the grammar has three shapes for the same
   fact.** `unsafe fn f()` parses as a direct `unsafe` token, but
   `pub unsafe fn f()` wraps it in a `function_modifiers` node. A modifier
   check that only looked for the direct form would have missed *every
   `pub unsafe fn`* — the most interesting case — and reported a confidently
   wrong number. Found by the AST fixture, not by review. The same thing
   happened with `extern` (which is `foreign_mod_item`, not
   `extern_modifier`) and with attributes (which hang off an `attributes`
   wrapper that a "do not descend into declarations" rule skips entirely).

2. **One-pass normalisation is not enough.** Collapsing whitespace turns
   `fn f(\n a: i32,\n)` into `fn f( a: i32, )`, which is a different string
   from `fn f(a: i32)` and therefore a false API change on every reformatted
   function. It takes two passes, because "is this comma trailing?" needs a
   character the first pass has not reached. Writing the obvious one-pass
   version and having a fixture catch it is the argument for
   must-not-fire fixtures all over again.

**`tests_not_deleted` moved here too.** M1 shipped it as a text counter, and
that was the one gate in the suite doing an AST gate's job. Replacing it was
not a like-for-like swap, it was strictly stronger in three ways:

1. **Identities, not counts.** The set of test names is compared across two
   trees, so removing `original` and adding `replacement` is caught even
   though the count is unchanged. A count cannot express that, and
   `tests_not_deleted_catches_a_delete_and_replace_that_keeps_the_count`
   pins it.
2. **The `is_test_path` heuristic is gone.** A deleted file is parsed at the
   base commit and the tests it declared are reported *by name*. "Is this a
   test file" no longer has to be guessed, which removes both the miss (a test
   in an oddly named file) and the false positive (a directory called
   `contest/`).
3. **Framework attributes are recognised** — `#[tokio::test]`,
   `#[test_case]`, `#[rstest]`, `#[bench]`, not just bare `#[test]`. A gate
   that only knows `#[test]` reports "nothing removed" on a repository that
   writes anything else, which is the failure mode of a check that has never
   met the codebase it runs on.

`cargo test -- --list` remains the stronger inventory — real names as the
compiler sees them, including macro-generated tests — and lands in M3 as a
`consumes` edge from `checks_green`.

**A bug this exposed, which was not in the gate at all.** The CLI reduced
each gate's findings to the single most severe one to feed the verdict algebra
and then printed only that one, with a comment asserting the rest were
printed. They were not. So a run that found three things reported one, and
PRD 7's "a block without the evidence is a bug" applied to the whole report
rather than to one finding. The CLI now carries a `GateReport` holding the
outcome *and* every finding, and
`several_findings_from_one_gate_all_survive` pins it. Worth recording because
the comment was confidently false, and a false comment about evidence is
worse than no comment.

**Exit — met.** The public-API gate is demonstrated against all four real
signature changes the plan named (renamed parameter, changed return type,
widened bound, new trait impl) and against seven benign ones (body change,
reformat, private items, crate-visible items, a new function returning
`Vec<String>`, a deleted file with no public items, and a whole unrelated
edit). **Zero false positives across 27 fixtures plus 4 end-to-end tests.**
Every AST gate returns `Untrustworthy` on a file with parser error nodes —
pinned by a test per gate, because a gate that skips what it cannot read and
returns "nothing found" has silently downgraded a check while still reporting
a number.

### M3 — `palisade-exec`, the delegated gates, and the report
- `palisade-exec`: process spawn, per-tool argv from the contract, timeout,
  capture, and the §3.1 exit-code table. `Delegated` is constructible nowhere
  else in the workspace; a test asserts this by building `Delegated` from a
  recording fake and by having CI run a check that no other crate references
  `std::process::Command`.
- `checks_green`: `cargo fmt --check`, `cargo clippy -- -D warnings`,
  `cargo test`, each producing its own finding so a fmt failure is not
  indistinguishable from a test failure. Feeds `tests_not_deleted` its
  `cargo test -- --list` inventory over a declared `provides` edge.
- `external_tool`: argv templating from `{base}`/`{head}`/`{index}`, SARIF
  ingestion. A template variable that does not resolve is `Untrustworthy`, never
  a literal `{base}` in an argv — a silently untemplated path is a gate that
  reports on the wrong tree and looks green.
- `palisade-report`: human, json (three separate top-level target keys), sarif
  2.1.0.
- `palisade init`: generate a starter `palisade.toml` seeded from the project's
  detected ecosystem, with every gate at `warn`. It is an adoption tool and
  must be impossible to run destructively.

**Exit:** end-to-end `palisade check` on both fixture repos produces
byte-stable SARIF across two runs on an unchanged tree (determinism check —
a supervisor whose own output is nondeterministic is uncalibratable), and exit
codes 0/1/2/3 are each reachable in a test. Every row of the §3.1 exit-code
table has a test, and the JSON output for a delegated failure names the tool and
its version.

### M4 — The gates about gates
- `contract_not_loosened`, `contract_review_stale` (§1.3).
- Suppression records: `{gate_id, path, reason}` required, emitted in the
  output artefact, counted in a report.
- Severity-promotion guard: promoting a gate `warn`→`error` without a
  `calibration` reference is a validation error (§4).
- SARIF upload path, documented for GitHub Actions and GitLab CI.

**Exit:** a commit that disables a gate without a reason blocks; with a reason
it is a warning. A gate cannot be promoted to `error` without a named
calibration corpus.

### M5 — Validation on repositories we did not write
PRD §9. Not optional and not parallelisable with anything else.

- Three real Rust repositories, none authored by us, no tuning allowed.
- For each: transcribe the project's *stated* rules into gates, recording
  every rule that could not be transcribed and why. The transcription rate is
  the primary number.
- Corpus of real pull requests for false positives at `error` severity.
  Published: N, FP count, and the threshold curve each threshold came from.
- Publish the full result, including the criterion that failed, in `EVAL.md`.

**Exit:** criteria 1–4 of PRD §9 measured and written down. If criterion 1
fails, the plan stops and the PRD is amended — that is the decision this
milestone exists to force, and it is a legitimate outcome, not a project
failure.

### M6 — Judgement tier (opt-in, gated on §1.1)
Only after M5. Fresh holdout, forced choice over
`no_change | on_topic | off_topic | contradicts_rules`, `escalate` severity
only, and the acceptance-impossibility test extended: since `Origin` has no
`Judged` variant, the guarantee is structural, and M6's job is to keep it that
way rather than to prove it.

---

## 6. Test strategy

Property and unit tests carry the invariants; fixtures carry the gates. Model
tests do not exist in v1 (no model).

| Layer | What | How |
| --- | --- | --- |
| Verdict algebra | rules 1, 2, 5, and the three-target separation | Exhaustive enumeration: every vector of length ≤3 over a nine-element alphabet, plus a characterisation test. A random sweep would be a strictly weaker claim for more code, and this state space is small enough to be closed |
| `palisade-exec` | the §3.1 exit-code table, one row one test | A recording fake process; asserts timeout, missing binary, signal, and unknown exit code all yield `Untrustworthy` and `provenance: Delegated` naming the tool |
| Provenance | "whose verdict is this" survives to the artefact | Test that `Origin::Delegated` is unconstructible outside `palisade-exec`, plus `scripts/check-boundary.sh` in CI |
| `is_adverse` vs `rules_out_accept` | conflating them asserts a gate can un-accept a verdict | Both properties tested separately over the whole state space |
| Gates | each fires, each stays silent | Two fixture repos, planted violations, committed; each gate gets a firing and a **must-not-fire** fixture |
| Diff machinery | `EVIDENCE.md` apparatus bugs | Named regression tests, one per recorded bug (§1 of `EVIDENCE.md`, §4, §6) |
| Observers | bounded observation | Property: for any input and any budget, output length ≤ budget, marker present exactly once |
| Contract | `deny_unknown_fields` | Golden test: every mutation of a valid contract either parses identically or errors with a span. Property: no valid mutation exists that weakens a gate silently |
| Contract | `provides`/`consumes` graph | Cycles rejected at load; an undeclared edge is a load error, not a nondeterministic read |
| Report | determinism | Two runs on an unchanged tree produce identical bytes |
| End-to-end | all four verdicts, all four exit codes | Fixture repos driven through the CLI |

**Corpus discipline.** The predecessor's headline number (0/5 false positives
for the LLM) is real but was framed on the same 16 states it was measured
against (`EVIDENCE.md` §9). Therefore: any number Palisade publishes is
tagged with the corpus it came from, the corpus is committed, and **no
threshold is fitted on the same corpus it is reported against.** This is
`slop-gate` decision 7 and the single most transferable habit in this
project.

---

## 7. Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Criterion 1 fails — real rules do not transcribe into gates | The thesis is wrong | M5 is early and blocking. It is the reason the plan stops and reopens the PRD rather than shipping M6 |
| Per-commit budget blown by `cargo test` | Teams disable the tool | `checks_green` is a declared `Delegated` gate, so its cost is visible in the contract and disableable without touching code. The cost is the project's own CI cost, and the budget is measured and published from the start (criterion 2) |
| Exit code collapsed into "failure" | The predecessor's most expensive bug returns | The §3.1 table is a crate of its own with one test per row; unknown codes are `Untrustworthy`; a CI check forbids process execution outside `palisade-exec` |
| `secret_absent` cries wolf | Gate gets switched off, and everything with it | Every pattern ships with a documented false-positive example; `warn` default; FP counted in the M5 corpus like any other gate |
| Contract becomes a ritual document | Manufactures confidence — the exact failure the PRD names | `contract_review_stale` + `contract_not_loosened`; both `warn` until a project has measured them |
| Gate-gaming, Goodhart-style | Gates get loosened to pass work | `contract_not_loosened` is a diff-visible check, not a policy. Plus `slop-gate`'s suppression-growth gate as a second, independent witness |
| Deterministic output drifts between versions | A passing run is not reproducible | Fingerprints in every finding; tool version in the report header; SARIF `partialFingerprints` |
| The `Error`/`Block` collapse gets "simplified" away | The predecessor's worst bug returns | Distinct exit code, a comment naming the incident, a crate whose entire job is the exit-code table, and a test that fails if the two are ever unified |
| Scope creep into a linter | Becomes the thing it complements | PRD §8 non-claims are quoted verbatim in the README; `checks_green` is the deliberate integration point, not a starting point |

---

## 8. Definition of done for v1

1. `palisade check` runs a declared contract on a Rust repository and returns
   exactly one of `accept | block | escalate | error`, with exit code 0/1/3/2.
2. Every primitive in PRD §6 is implemented, or is `NotImplemented` and forces
   `Error`. No primitive silently passes.
3. Every finding and every `Pass` carries an `Origin`, present in the JSON and
   SARIF output. `Origin::Delegated` is constructible only by `palisade-exec`,
   `Origin` has no `Judged` variant, and CI fails if any crate outside
   `palisade-git`/`palisade-exec` spawns a process.
4. Zero false positives across the negative fixture suite, published with the
   count.
5. Every artefact states what it did not cover, from `judgement.not_covered`.
6. M5's four criteria measured on three external repositories, published,
   including any criterion that failed.
7. `README.md` contains the PRD §8 non-claims verbatim.
8. No network request at analysis time. No model in the loop. Single binary.

Criterion 5 is the one that decides whether this is a tool you can rely on. The
gates are the easy part; the gap being an owned, dated artefact is the part
that is worth building.
