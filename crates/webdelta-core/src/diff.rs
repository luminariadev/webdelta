use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

/// Structured line-by-line comparison between two snapshots.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffResult {
    pub unified: String,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub summary: String,
}

/// Computes a unified diff and line-change counts between `previous` and `current`.
pub fn compute_diff(previous: &str, current: &str, context_lines: usize) -> DiffResult {
    let diff = TextDiff::from_lines(previous, current);
    let mut added_lines = 0usize;
    let mut removed_lines = 0usize;

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added_lines += 1,
            ChangeTag::Delete => removed_lines += 1,
            ChangeTag::Equal => {}
        }
    }

    let unified = diff
        .unified_diff()
        .context_radius(context_lines)
        .header("previous", "current")
        .to_string();

    let summary = format!("+{added_lines} / -{removed_lines} lines");

    DiffResult {
        unified,
        added_lines,
        removed_lines,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_diff_counts_and_unified_output() {
        let diff = compute_diff("alpha\nbeta\ngamma\n", "alpha\nbeta2\ngamma\ndelta\n", 2);
        assert_eq!(diff.added_lines, 2);
        assert_eq!(diff.removed_lines, 1);
        assert!(diff.unified.contains("-beta"));
        assert!(diff.unified.contains("+beta2"));
        assert!(diff.unified.contains("+delta"));
        assert_eq!(diff.summary, "+2 / -1 lines");
    }
}
