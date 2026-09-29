# Palisade

A supervisor for coding agents that runs a project's declared quality contract,
and escalates only what the contract could not cover.

**Status: specification. Nothing is built yet.** This folder is self-contained
and is meant to be sufficient for someone to build the thing from scratch, in
Rust, without reading anything else in this repository.

## What to read

| File | What it is |
| --- | --- |
| [PRD.md](PRD.md) | The product. Problem, thesis, why Rust first, the gate contract, primitives, orchestration rules, success criteria, open questions. |
| [EVIDENCE.md](EVIDENCE.md) | Every measurement, including the failures. Read this before proposing anything that resembles a model judgement. |
| [REFERENCE-slop-gate.md](REFERENCE-slop-gate.md) | Prior art. A deterministic CI gate for the hardest rule class, in Rust. Seven design decisions to take from it. |
| `results/` | Raw per-question scores from each run in the evidence. |

## The idea in one paragraph

Coding agents don't know when to stop. The usual fix is a model above the model,
which means trusting a judgement. That was tried here and it does not work: a
400M span classifier and a frontier LLM both saturated the same head, and the
classifier flagged rule violations on states where the diff was *empty*. But the
rules in question were things like "standard library only" and "never delete a
test" — trivially machine-checkable. So the project declares its quality bar as
an executable contract, and the supervisor runs the contract instead of forming
an opinion. Whatever the contract cannot express is declared in a required
`judgement.not_covered` list, so the gap is an artefact somebody owns.

## Why Rust

Not a language preference. `slop-gate` already solves the hard part — "did this
change make the codebase worse" — deterministically and well, in Rust. Rebuilding
it elsewhere would be a worse version of a solved problem. And in Rust, every
check Palisade needs is an AST or a build-graph question that `tree-sitter-rust`
and `cargo` answer fast enough to run on every commit, which is what makes a
supervisor acceptable in the first place.

## The one thing to get right

Not the gates. The `judgement` section.

A contract that checks what is checkable and is silent about the rest is worse
than no contract, because it manufactures confidence. The gap has to be written
down, dated, and reviewed. That requirement is the difference between a tool
that runs and a tool you can rely on.
