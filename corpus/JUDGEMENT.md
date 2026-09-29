# Findings judgement, M5

Judged 2026-09-29, from `results/*.txt`. 253 merges replayed across four
repositories, generated contracts, eight analyzed gates.

## Read this before the numbers

**I judged these, and I wrote the gates.** That is the self-review problem
PRD §9 criterion 5 exists to remove, and it is not removed by my having been
careful. Two consequences, stated rather than buried:

- **This is not criterion 3.** Criterion 3 needs a person who did not build the
  gates. What follows is a first pass whose *categories* are reliable and whose
  *verdicts* are one opinion.
- **The bias is not symmetric.** For "is this finding structurally absurd" —
  a branch sync reported as a mass deletion — my judgement is nearly as good
  as a stranger's, because the answer is checkable against the repository. For
  "would a maintainer find this noisy" it is not, and that is the question a
  false-positive rate actually turns on.

What I can do reliably is find findings that are *provably* wrong and fix the
cause. That is what this pass produced, and it is worth having.

## The headline

| | |
| --- | --- |
| Merges replayed | 253 |
| Errors (`Untrustworthy`) | **0** |
| Branch syncs skipped | 23 (not changes under review) |
| Findings | 924 |
| **Findings that are a blocking-severity false positive** | **0** |

Every finding is `warn`. That is the M2 design holding: additions never
block, a justified loosening never blocks, and the two contract-hygiene gates
never fired because no diff loosened a contract. **A team running this would
never be blocked by a false positive**, which is the property PRD 3.3 asks for
and the one this measurement can support.

## By gate

| Gate | Findings | Judgement |
| --- | --- | --- |
| `public_api_unchanged` | 905 | 98% of the total, **all additions at `warn`, by design.** Not false positives — a new public item is real news — but noise at that volume. Now collapsed to one finding per file: **905 → 57 on `warmplane`**, and removals are *not* collapsed, because those are the ones a reader must not miss. |
| `suppressions_not_widened` | 11 | **All real.** Every one is a genuine `#[allow]` added. `gliner2-candle` adds nine across two merges; `warmplane` one `#[allow(dead_code)]`. This gate is working. |
| `tests_not_deleted` | 8 | **4 real, 4 were harness false positives** now fixed. See below. |

## Three gate defects, all found by running on real code

**Additions were not collapsed.** Fixed: one finding per file, carrying the
count and a bounded sample of names, with the overflow stated rather than
silently truncated. Removals and signature changes are untouched.

**`checks_green` reported cargo's download noise instead of the lint.** On
`pearls` the finding read "Updating crates.io index | Locking 242 packages"
while the five real clippy errors began seven lines later. A finding that
names the dependency download is not evidence, and a gate whose evidence is
routinely about something else is a gate nobody reads findings of. Now selects
diagnostic *heads* (`error: …`, `warning: …`), excludes cargo's own
`could not compile` summary, and counts overflow in diagnostics rather than in
lines of source excerpt.

**Doc comments counted as signature.** On `pearls`, **7 of 8** "signature
changed" findings differed only in `///` prose. A doc comment is not the API.
Now stripped before comparison, and the three remaining findings are all
genuine: a serde field renamed `compact_threshold_days` → `max_priority` (a
breaking change for anyone serialising) and an added error type.

Stripping it exposed a latent bug: `strip_doc_comments` matched `//` inside
`///` and then searched for a block-comment close in what was really a line
comment, swallowing the rest of the declaration. **An empty signature reads as
"no change", so the gate had been silently disabled for every documented
item** — which is most of a real codebase. Fixed, and the fix is why the
new fixtures exist.

## Two harness defects, both found by this pass

**1. Branch syncs reported as mass test deletions.** 12 of `adk-rust`'s 98
merges are `Merge remote-tracking branch 'origin/main' into fix/...`. Replaying
`diff(first_parent, merge)` for one reports every file the *other* branch had
not yet merged as deleted by this change. Four of them produced
`tests::test_approval_flow` and three siblings as "no longer exist anywhere in
the tree."

The test existed in the first parent and not the second, so the diff was
correct and the *conclusion* was nonsense. Now skipped by subject-line match,
with a test that the pattern catches a branch sync and not a PR merge.

This is the second time a merge-replay artefact has produced a confident false
positive, after the M5 file-split bug. **The pattern is: a two-tree comparison
is only as sound as the pair of trees it is given.** Both bugs were in the
pairing, not the comparison.

**2. `init` froze `benches`,** which fired on four `warmplane` merges that
*added* Criterion benchmarks. Fixed; `benches` and `examples` are no longer
seeded frozen.

## The one real substantive finding

`adk-rust` PR #434 (`feat/cooperative-cancellation`) removed four tests:

```
- async fn test_approval_flow()
- async fn test_denial_flow()
- async fn test_timeout_flow()
- async fn test_resolve_unknown_id()
```

Verified: the test existed at the first parent, does not exist at the merge, and
was not moved to another file. The PR added *other* tests
(`test_context_is_cancelled_defaults_false`, `test_runner_interrupt_stops_agent_stream`,
`test_timeout_terminates_background_descendants`) so it looks like a
consolidation — but four named tests from `adk-acp/src/server/permission.rs`
are simply gone.

**This is in a repository whose `CONTRIBUTING.md` says "Every PR must pass
these checks."** That is the gate doing exactly what the project's own stated
rule asks for, on a project that wrote the rule down, in code written with
coding agents. It is the strongest single piece of evidence in this corpus
that the product works.

## What I cannot tell you

**Whether 905 `public_api_unchanged` additions are acceptable noise.** They are
correct — each is a real new public item — and they are all `warn`. But a
report that opens with 391 findings on one merge is a report nobody reads, and
that is a design problem I can see and a judgement problem I cannot. Three
options, none of which I can choose alone:

1. Collapse additions to a per-merge count. Loses detail, fixes the skim.
2. Suppress them unless the contract asks for them. Makes the default quiet,
   which is a real opinion about what a project wants.
3. Leave them. The information is real; a team that finds it noisy can set
   `allow` per path, which the contract already supports.

I lean to (1) with a `--verbose` escape, but that is a taste call and the
volume is the fact that should drive it.

## Coverage gaps in this measurement

- **`checks_green` was run on three small repositories** (`duiker`,
  `pearls`, `rust-okf`: 6 merges, 18s cold on the smallest) and found real
  clippy failures. It is still unmeasured on anything large, because
  `cargo test --workspace` over 38 members is minutes per merge.
  `external_tool` remains unmeasured entirely.
- **No finding was judged by a second person.** Every number above is one
  opinion.
- **`awakenworks/awaken` contributed nothing measurable** — 783 files, three
  rules files, zero merge commits. It contributes rules to read, not data.
- **Every merge in the corpus is recent.** Both dated repositories are from
  2026. There is no multi-year agent-written history, which is a fact about
  the population rather than about the tool.
