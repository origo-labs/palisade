//! `palisade-observe` — bounded observation capture on a dirty tree.
//!
//! This crate exists because of the most expensive bug in the predecessor
//! programme. `git diff` reads unstaged changes; committing each state before
//! observing yields an empty diff *everywhere*, and the whole benchmark then
//! scores at chance while reporting success (`EVIDENCE.md`, apparatus bugs).
//! The model built on top was blamed for a bug in the harness.
//!
//! Two rules, both mechanical here:
//!
//! 1. The observation is collected on the worktree, and is *never* laundered
//!    through a commit. An empty observation says so, with a reason, rather
//!    than reporting a successful check of nothing.
//! 2. The diff is clipped to a declared byte budget, and **the truncation
//!    marker is reserved inside the budget** — never appended after the split
//!    (PLAN.md 1.6). Output length is `<= budget`, unconditionally.

use palisade_git::ObservationInputs;

/// Reserved inside the budget. If a budget is smaller than this, the marker
/// itself is shortened rather than the guarantee dropped.
pub const TRUNCATION_MARKER: &str = "\n[palisade: observation truncated to budget]\n";
const MARKER_SUFFIX: &str = "…(truncated)\n";

/// A hard bound on the observation, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    bytes: usize,
}

impl Budget {
    /// Below this a truncated observation is not evidence of anything, so the
    /// contract is told rather than the report being quietly meaningless.
    /// Smallest usable budget.
    pub const MIN: usize = 1024;
    /// 128 KiB: comfortably larger than a typical change to a single module,
    /// small enough that a runaway diff cannot blow up the per-commit budget.
    pub const DEFAULT: Self = Self { bytes: 128 * 1024 };

    /// A budget below [`Budget::MIN`] is a contract validation error, not a
    /// warning, and not something to be silently raised.
    ///
    /// Returns `None` below the minimum.
    pub const fn new(bytes: usize) -> Option<Self> {
        if bytes < Self::MIN {
            None
        } else {
            Some(Self { bytes })
        }
    }

    /// The bound, in bytes.
    pub const fn bytes(self) -> usize {
        self.bytes
    }

    /// Clip `text` so the result is at most `self.bytes`, with the truncation
    /// marker *inside* that allowance.
    ///
    /// The property, stated once and tested as one: `clip(t).len() <= budget`,
    /// and the marker appears exactly once if and only if something was cut.
    pub fn clip(&self, text: &str) -> Clipped {
        if text.len() <= self.bytes {
            return Clipped {
                text: text.to_string(),
                truncated: false,
            };
        }
        // The whole class of bug here is off-by-marker arithmetic that yields
        // budget + len(marker) bytes. Reserve first, then cut.
        let marker = self.marker();
        let keep = self.bytes - marker.len();
        let mut end = keep;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        Clipped {
            text: format!("{}{marker}", &text[..end]),
            truncated: true,
        }
    }

    /// The marker, shortened if the budget cannot even fit the full one.
    fn marker(&self) -> &str {
        if self.bytes >= TRUNCATION_MARKER.len() {
            return TRUNCATION_MARKER;
        }
        MARKER_SUFFIX
    }
}

/// Text that was clipped to a budget, with the truncation recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipped {
    /// The retained prefix, plus the marker if anything was cut.
    pub text: String,
    /// Whether anything was cut.
    pub truncated: bool,
}

/// Why an observation is empty. An empty observation is a legitimate state —
/// there is nothing to check — but it is never indistinguishable from "we
/// checked and it was fine".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyReason {
    /// The worktree is clean and there is no base to diff against.
    CleanWorktree,
    /// The worktree is clean relative to a base that *does* exist, so there
    /// is genuinely no work to review.
    NoChangesAgainstBase,
    /// A diff was expected and came back empty. This is a fault, not a state,
    /// and it is what a committed-state harness produces.
    DiffExpectedButAbsent,
}

impl EmptyReason {
    /// The exact spelling used in reports and in the empty-observation record.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CleanWorktree => "clean_worktree",
            Self::NoChangesAgainstBase => "no_changes_against_base",
            Self::DiffExpectedButAbsent => "diff_expected_but_absent",
        }
    }
}

impl std::fmt::Display for EmptyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the gates receive. Sized and clipped, so a gate can be a pure
/// function of this value with no repository and no subprocess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// Resolved base commit, when a base was requested.
    pub base: Option<String>,
    /// The bound this observation was captured under.
    pub budget: Budget,
    /// The clipped diff, marker included within the budget.
    pub diff: Clipped,
    /// `git status` entries, for path-level gates.
    pub status: Vec<StatusLine>,
    /// `Some` iff the diff is empty. Never `None` when it is — the point of
    /// the field is that "observed nothing" is always attributable.
    pub empty: Option<EmptyReason>,
}

/// One `git status --porcelain` entry, already parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    /// Staged status character, or `?` for untracked.
    pub x: char,
    /// Worktree status character, or ` ` when not applicable.
    pub y: char,
    /// Repository-relative path.
    pub path: String,
    /// Original path of a rename or copy.
    pub orig_path: Option<String>,
}

impl Observation {
    /// Assemble an observation from raw git output.
    ///
    /// `expect_diff` is the caller's declaration that a change ought to be
    /// present. It exists so that the "committed the state then diffed"
    /// failure surfaces as `DiffExpectedButAbsent` rather than as a clean
    /// bill of health.
    pub fn capture(inputs: &ObservationInputs, budget: Budget, expect_diff: bool) -> Self {
        let mut combined = String::with_capacity(inputs.unstaged.len() + inputs.staged.len());
        if !inputs.staged.is_empty() {
            combined.push_str("# staged (index vs HEAD)\n");
            combined.push_str(&inputs.staged);
        }
        if !inputs.unstaged.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str("# unstaged (worktree vs index/base)\n");
            combined.push_str(&inputs.unstaged);
        }

        let diff = budget.clip(&combined);
        let empty = if diff.text.is_empty() {
            Some(if expect_diff && !inputs.dirty {
                // Nothing dirty and nothing diffed while a change was
                // expected: the state was committed before observation.
                EmptyReason::DiffExpectedButAbsent
            } else if inputs.base.is_some() {
                EmptyReason::NoChangesAgainstBase
            } else {
                EmptyReason::CleanWorktree
            })
        } else {
            None
        };

        let status = inputs
            .status
            .iter()
            .map(|e| StatusLine {
                x: e.x,
                y: e.y,
                path: e.path.clone(),
                orig_path: e.orig_path.clone(),
            })
            .collect();

        Self {
            base: inputs.base.clone(),
            budget,
            diff,
            status,
            empty,
        }
    }

    /// Whether there is nothing to review. Check
    /// [`Observation::empty`] for the reason; the boolean alone is not enough
    /// to report.
    pub fn is_empty(&self) -> bool {
        self.empty.is_some()
    }

    /// Paths the observation knows about, from status. A gate that needs file
    /// contents reads them through `palisade-git`; a gate that needs to know
    /// *which* files moved reads this.
    /// Every path the status reports, for gates that need to know which files
    /// moved.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.status.iter().map(|s| s.path.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use palisade_git::StatusEntry;

    fn inputs(unstaged: &str, staged: &str, dirty: bool, base: Option<&str>) -> ObservationInputs {
        ObservationInputs {
            base: base.map(str::to_string),
            status: vec![StatusEntry {
                x: 'M',
                y: ' ',
                path: "src/lib.rs".to_string(),
                orig_path: None,
            }],
            dirty,
            unstaged: unstaged.to_string(),
            staged: staged.to_string(),
        }
    }

    #[test]
    fn clipping_a_short_text_is_a_no_op() {
        let b = Budget::new(4096).unwrap();
        let c = b.clip("hello");
        assert!(!c.truncated);
        assert_eq!(c.text, "hello");
    }

    #[test]
    fn marker_is_reserved_inside_the_budget() {
        let b = Budget::new(4096).unwrap();
        let c = b.clip(&"x".repeat(10_000));
        assert!(c.truncated);
        assert!(c.text.len() <= b.bytes());
        assert!(c.text.ends_with(TRUNCATION_MARKER));
        assert_eq!(c.text.matches(TRUNCATION_MARKER).count(), 1);
    }

    #[test]
    fn clipping_never_splits_a_char_boundary() {
        let b = Budget::new(Budget::MIN).unwrap();
        // Multi-byte characters straddling the keep boundary.
        for filler in 1..64usize {
            let text = format!("{}é€𝄞{}", "a".repeat(filler), "b".repeat(4096));
            let c = b.clip(&text);
            assert!(c.text.len() <= b.bytes(), "filler {filler}");
        }
    }

    #[test]
    fn budget_below_min_is_refused() {
        assert!(Budget::new(0).is_none());
        assert!(Budget::new(Budget::MIN - 1).is_none());
        assert!(Budget::new(Budget::MIN).is_some());
    }

    #[test]
    fn tiny_budget_shortens_the_marker_rather_than_breaking_the_guarantee() {
        let b = Budget { bytes: 32 };
        let c = b.clip(&"x".repeat(500));
        assert!(c.text.len() <= 32);
        assert!(c.truncated);
    }

    #[test]
    fn clean_tree_with_no_base_is_clean_worktree() {
        let o = Observation::capture(&inputs("", "", false, None), Budget::DEFAULT, false);
        assert_eq!(o.empty, Some(EmptyReason::CleanWorktree));
    }

    #[test]
    fn clean_tree_against_a_base_is_no_changes_against_base() {
        let o = Observation::capture(
            &inputs("", "", false, Some("deadbeef")),
            Budget::DEFAULT,
            false,
        );
        assert_eq!(o.empty, Some(EmptyReason::NoChangesAgainstBase));
    }

    #[test]
    fn a_change_present_is_never_empty() {
        let o = Observation::capture(
            &inputs("--- a\n+++ b\n+added\n", "", true, Some("deadbeef")),
            Budget::DEFAULT,
            true,
        );
        assert_eq!(o.empty, None);
        assert!(!o.is_empty());
    }

    #[test]
    fn staged_only_change_is_still_an_observation() {
        // `git status` says the file changed while a bare `git diff` is
        // silent. This is the other half of the apparatus bug.
        let o = Observation::capture(
            &inputs("", "--- a\n+++ b\n+staged\n", true, Some("deadbeef")),
            Budget::DEFAULT,
            true,
        );
        assert!(!o.is_empty());
        assert!(o.diff.text.contains("+staged"));
    }
}
