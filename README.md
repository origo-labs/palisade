# Palisade

A supervisor for coding agents. It runs a project's **declared quality
contract** and escalates only what that contract could not cover.

**Status: beta.** Ten of the twelve gates run — eight you declare, plus two
that audit the contract itself. The other two report `error` rather than
passing. It has run against 253 real merges in ten repositories. It has *not*
been reviewed by anyone who did not write it — see
[What we have not measured](#what-we-have-not-measured), which is the most
important section here.

## Try it

```sh
cargo install --git https://github.com/origo-labs/palisade palisade-cli
cd your-rust-project
palisade init          # writes a starting palisade.toml
palisade check         # runs it
```

`init` writes every gate at `warn`: it proposes, it does not judge. Nothing
blocks until you promote a gate to `error` and say why.

Exit codes, so CI can branch on them:

| Code | Meaning |
| --- | --- |
| 0 | `accept` — every enabled gate passed |
| 1 | `block` — at least one error-severity finding |
| 2 | `error` — **no verdict was reached**; a gate could not run |
| 3 | `escalate` — something wants a human |

Code 2 is not a stricter code 1. A gate that could not run has not passed.

## The idea

Coding agents don't know when to stop. The usual fix is a model above the
model, which means trusting a judgement. That was tried, and it does not work: a
400M span classifier and a frontier LLM both saturated on the same head, and the
classifier flagged rule violations on states where the diff was *empty*. The
rules in question were "standard library only" and "never delete a test" —
trivially machine-checkable.

So the quality bar becomes an executable contract. The project declares it;
the supervisor runs it. Whatever the contract cannot express goes in a required
`judgement.not_covered` list, so the gap is an artefact somebody owns rather
than an absence nobody notices.

## What it does not do

Verbatim from the PRD, and still true:

- Palisade does **not** judge whether code is good. It runs what the project
  declared.
- It does **not** prove semantic equivalence, assess security, or replace a
  linter — no more than `slop-gate` does, which says so in its own README.
- **Deterministic does not mean complete.** A contract checking dependencies
  and green tests will pass a change that is compliant and strategically
  wrong. That lives in `judgement.not_covered`, which is why that section is
  mandatory.
- A gate that checks form instead of intent is theatre. The contract's value
  is bounded by the discipline of the people writing it, and no tool fixes
  that.
- Zero false positives is a measured claim about a corpus, never a property of
  the software.

There is no model in the loop, and no network request at analysis time.

## Gates

| Gate | What it asks |
| --- | --- |
| `dependency_surface_unchanged` | did a declared production dependency, feature, or `default-features` change? |
| `tests_not_deleted` | did a test disappear, or gain `#[ignore]`? |
| `public_api_unchanged` | did a public item's signature change or vanish? |
| `unsafe_surface_unchanged` | was `unsafe` surface *added*? |
| `suppressions_not_widened` | was an `#[allow]` added or broadened? |
| `paths_unchanged` | was a frozen path touched? |
| `checks_green` | do `fmt`, `clippy` and `test` pass? |
| `external_tool` | what did a third-party gate say? (`slop-gate` verified) |
| `secret_absent` | **not implemented** — reports `error`, never passes |
| `judged` | **not implemented** — reserved for on-topic judgement, escalate-only |

`secret_absent` and `judged` are in the vocabulary on purpose. A gate that is
declared but absent reports `error`, so a contract that asks for something
unavailable fails loudly instead of going quietly green.

## The two gates that audit the contract

These are **not** declarable, and you cannot switch them off:

- `contract_not_loosened` — a diff that downgrades a severity, deletes a
  gate, adds a suppression, or removes a `not_covered` entry without a
  `[[changes]]` record saying why. A justified change is still reported, at
  `warn`; the rule is not "you may never change the contract", it is **"you may
  never change it silently"**.
- `contract_review_stale` — the review date is older than 180 days, or is not
  a date at all.

A gate that can be switched off by the thing it guards is not a gate. This was
found the hard way: the first implementation declared both in the contract, and
a worker deleted the declaration and got `accept`.

## CI

```yaml
- run: cargo install --git https://github.com/origo-labs/palisade palisade-cli
- run: palisade check --format sarif > results.sarif
  continue-on-error: true      # upload the SARIF even on a block
- uses: github/codeql-action/upload-sarif@v3
  if: always()
  with:
    sarif_file: results.sarif
```

`--format human|json|sarif`. The JSON schema keeps the three targets separate —
`acceptance`, `rule_violations`, `residual_judgement` — because conflating "does
this break a rule" with "should a supervisor intervene" is what confounded a
prior measurement programme.

## What we have not measured

This is the part to read twice.

**We measured 253 merges across ten repositories, generated contracts, zero
`Untrustworthy`, zero blocking-severity false positives.** Every finding was
`warn`.

**We judged those findings ourselves, and we wrote the gates.** So we have
evidence that the gates *run* and that their findings are *individually* sound,
and no evidence at all about whether a maintainer finds them tolerable. The
corpus also has one author, who is also the person choosing the gates.

The measurement is in [`corpus/JUDGEMENT.md`](corpus/JUDGEMENT.md), with the
raw output in `corpus/results/`. It includes the false positives we found and
fixed, which is the more useful half.

Things a beta user should know:

- **`public_api_unchanged` reports additions.** They are collapsed to one line
  per file, but on a large feature merge that is still the noisiest gate. It is
  `warn` for that reason.
- **A file-to-module split and a branch sync both once looked like mass test
  deletions.** Both are fixed, and both bugs were in the *pairing of trees*
  rather than the comparison. A two-tree gate is only as sound as the pair it is
  given.
- **`checks_green` is unmeasured on anything large.** It runs
  `cargo test --workspace`; on a 38-member crate that is minutes per merge.
- **`external_tool` needs a baseline artefact** that is bound to a commit, so it
  has to be rebuilt when the base moves. A stale one is an `error`, not a
  finding.

## Reading order

| File | What it is |
| --- | --- |
| [PLAN.md](PLAN.md) | How it is built, and why each decision was made. The findings in there are the interesting part. |
| [PRD.md](PRD.md) | The product. Thesis, gate contract, primitives, success criteria. |
| [EVIDENCE.md](EVIDENCE.md) | The measurements that motivated the design, **including the failures**. |
| [REFERENCE-slop-gate.md](REFERENCE-slop-gate.md) | Prior art, and the decisions taken from it. |
| [corpus/JUDGEMENT.md](corpus/JUDGEMENT.md) | Our own corpus run and the false positives it found. |

## Licence

MIT or Apache-2.0, at your option.
