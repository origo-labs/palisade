//! `palisade-exec` — the `Delegated` gate kind. M0 stub. **Empty on purpose.**
//!
//! M3 lands process execution here, and this crate's entire reason to exist is
//! that it is the only place in the workspace allowed to do it. The rule it
//! enforces is the one that cost the predecessor programme a set of
//! measurements (`EVIDENCE.md` 5, and `slop-gate` decision 5):
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
//! that. A verifier that could not start and a verifier whose tests failed
//! were indistinguishable to the predecessor's caller, and every model
//! comparison it ran was confounded by that. It does not get a home in the
//! least-tested layer, and it does not get a home in `palisade-orchestrate`.
//!
//! The table lives here rather than in prose so that a future contributor
//! widening it has to edit the code that implements it.
