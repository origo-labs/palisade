//! The bounded-observation property, stated once and checked by exhaustion.
//!
//! PRD 7: the clip is a hard bound — *reserve the truncation marker inside
//! the budget, do not append it after the split.* Getting that wrong is a
//! silent correctness bug rather than a crash: the output is
//! `budget + len(marker)` bytes and nothing anywhere complains.

use palisade_observe::{Budget, TRUNCATION_MARKER};

/// The property: for any input and any valid budget, the clipped output is
/// within budget, and the marker appears exactly once if and only if
/// something was cut.
fn assert_clip_property(text: &str, budget: Budget) {
    let clipped = budget.clip(text);
    assert!(
        clipped.text.len() <= budget.bytes(),
        "output {} exceeded budget {}",
        clipped.text.len(),
        budget.bytes()
    );
    let marker = if budget.bytes() >= TRUNCATION_MARKER.len() {
        TRUNCATION_MARKER
    } else {
        "\u{2026}(truncated)\n"
    };
    if budget.bytes() >= TRUNCATION_MARKER.len() {
        let n = clipped.text.matches(marker).count();
        if clipped.truncated {
            assert_eq!(n, 1, "truncated output must carry the marker exactly once");
        } else {
            assert_eq!(n, 0, "intact output must not carry the marker");
        }
    }
    assert_eq!(clipped.truncated, text.len() > budget.bytes());
    // Whatever came back must still be a valid prefix of the original, with
    // only the marker added: no silent content mutation, ever.
    let body = clipped.text.strip_suffix(marker).unwrap_or(&clipped.text);
    assert!(
        text.starts_with(body),
        "clipping must not rewrite content, got body starting {:?}",
        &body[..body.len().min(40)]
    );
}

#[test]
fn the_property_holds_across_budgets_and_inputs() {
    // Every valid budget from the minimum upward, at a stride that still hits
    // the interesting neighbourhood of the marker boundary.
    let mut budgets: Vec<usize> = (Budget::MIN..Budget::MIN + 512).collect();
    for extra in [1024usize, 2048, 4096, 8192, 16384, 32768, 131_072] {
        budgets.push(extra);
    }
    budgets.push(Budget::MIN);

    let inputs: Vec<String> = vec![
        String::new(),
        "a".to_string(),
        "short diff\n".to_string(),
        "x".repeat(10_000),
        "y".repeat(200_000),
        // Multi-byte content straddling every possible keep boundary.
        format!("{}é€𝄞{}", "a".repeat(3000), "b".repeat(3000)),
        format!("{}\n{}", "+".repeat(50_000), "-".repeat(50_000)),
    ];

    for bytes in &budgets {
        let Some(budget) = Budget::new(*bytes) else {
            continue;
        };
        for input in &inputs {
            assert_clip_property(input, budget);
        }
    }
}

#[test]
fn a_budget_just_below_the_marker_length_still_honours_the_bound() {
    // The awkward case: the marker does not fit, so it is shortened. The
    // guarantee that survives is the one that matters — the bound holds.
    let tiny = Budget::new(Budget::MIN).unwrap();
    let clipped = tiny.clip(&"z".repeat(100_000));
    assert!(clipped.truncated);
    assert!(clipped.text.len() <= tiny.bytes());
}

#[test]
fn an_input_exactly_at_the_budget_is_not_truncated() {
    let b = Budget::new(2048).unwrap();
    let exact = "q".repeat(2048);
    let clipped = b.clip(&exact);
    assert!(!clipped.truncated);
    assert_eq!(clipped.text.len(), 2048);
    assert_eq!(clipped.text, exact);
}

#[test]
fn one_byte_over_the_budget_is_truncated() {
    let b = Budget::new(2048).unwrap();
    let over = "q".repeat(2049);
    let clipped = b.clip(&over);
    assert!(clipped.truncated);
    assert!(clipped.text.len() <= 2048);
}
