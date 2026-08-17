//! Suggestion fences: authoring them, and splitting them back out (§7).

use diffident_diff::{DiffFile, LineKind, Row};

/// One run of a comment body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    /// The contents of a ```suggestion fence, without the fence lines.
    Suggestion(String),
}

/// Wrap `lines` in a ```suggestion fence, ready for the composer to seed.
pub fn fence(lines: &[String]) -> String {
    format!("```suggestion\n{}\n```", lines.join("\n"))
}

/// The post-image text of the rows in `a..=b`, in either drag direction.
///
/// Removed lines are skipped: a suggestion replaces the lines it is anchored
/// to, and those only exist on the new side. A selection of nothing but
/// removals therefore yields no lines, which is the caller's cue not to open a
/// suggestion at all rather than to offer one GitHub will reject.
pub fn source_lines(files: &[DiffFile], rows: &[Row], a: usize, b: usize) -> Vec<String> {
    let (lo, hi) = (a.min(b), a.max(b));
    rows.get(lo..=hi)
        .unwrap_or_default()
        .iter()
        .filter_map(|row| {
            let line = row.line(files)?;
            (line.kind != LineKind::Removed).then(|| line.text.clone())
        })
        .collect()
}

/// Split a comment body into prose and suggestion runs.
///
/// An unterminated fence still yields a suggestion: GitHub renders it that way,
/// and showing the reviewer raw backticks because the author forgot a closing
/// line would misreport what is on the PR.
pub fn segments(body: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut buf: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let flush = |buf: &mut Vec<&str>, in_fence: bool, out: &mut Vec<Segment>| {
        if buf.is_empty() {
            return;
        }
        let text = buf.join("\n");
        buf.clear();
        if in_fence {
            out.push(Segment::Suggestion(text));
        } else if !text.trim().is_empty() {
            out.push(Segment::Text(text));
        }
    };
    for line in body.lines() {
        match (in_fence, line.trim()) {
            (false, "```suggestion") => {
                flush(&mut buf, false, &mut out);
                in_fence = true;
            }
            (true, "```") => {
                flush(&mut buf, true, &mut out);
                in_fence = false;
            }
            _ => buf.push(line),
        }
    }
    flush(&mut buf, in_fence, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_with_no_fence_is_one_run_of_text() {
        assert_eq!(segments("just a nit"), vec![Segment::Text("just a nit".into())]);
    }

    #[test]
    fn a_fence_comes_back_without_its_backticks() {
        let body = "try this:\n```suggestion\nlet x = 1;\n```\nthanks";
        assert_eq!(
            segments(body),
            vec![
                Segment::Text("try this:".into()),
                Segment::Suggestion("let x = 1;".into()),
                Segment::Text("thanks".into()),
            ]
        );
    }

    #[test]
    fn a_body_that_is_only_a_suggestion_has_no_empty_text_runs() {
        // An empty Text segment would render as a blank line above every
        // suggestion, which looks like a layout bug.
        assert_eq!(
            segments("```suggestion\nx\n```"),
            vec![Segment::Suggestion("x".into())]
        );
    }

    #[test]
    fn a_multi_line_suggestion_keeps_its_line_breaks() {
        assert_eq!(
            segments("```suggestion\na\nb\n```"),
            vec![Segment::Suggestion("a\nb".into())]
        );
    }

    #[test]
    fn an_unterminated_fence_is_still_a_suggestion() {
        // GitHub renders it as one. Showing raw backticks because the author
        // forgot a closing line would misreport what is on the PR.
        assert_eq!(
            segments("```suggestion\na"),
            vec![Segment::Suggestion("a".into())]
        );
    }

    #[test]
    fn a_plain_code_fence_is_not_a_suggestion() {
        let body = "```\nnot a suggestion\n```";
        assert_eq!(segments(body), vec![Segment::Text(body.into())]);
    }

    #[test]
    fn an_empty_body_has_no_segments() {
        assert!(segments("").is_empty());
    }

    #[test]
    fn fencing_lines_produces_something_segments_reads_back() {
        let f = fence(&["let x = 1;".to_string(), "let y = 2;".to_string()]);
        assert_eq!(
            segments(&f),
            vec![Segment::Suggestion("let x = 1;\nlet y = 2;".into())]
        );
    }

    // a.rs: line 1 context, old line 2 removed, new line 2 added.
    const DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n";

    fn fixture() -> (Vec<DiffFile>, Vec<Row>) {
        let files = diffident_diff::parser::parse(DIFF);
        let rows = diffident_diff::rows::build_rows(&files);
        (files, rows)
    }

    /// Row index of the `n`th line row — headers and spacers make raw indices
    /// unreadable in these assertions.
    fn row_of(rows: &[Row], n: usize) -> usize {
        rows.iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, Row::Line { .. }))
            .nth(n)
            .map(|(i, _)| i)
            .expect("line row")
    }

    #[test]
    fn one_line_yields_its_post_image_text() {
        let (files, rows) = fixture();
        let at = row_of(&rows, 0); // the context line
        assert_eq!(source_lines(&files, &rows, at, at), vec!["ctx".to_string()]);
    }

    #[test]
    fn a_removed_line_contributes_nothing_because_it_is_not_on_the_new_side() {
        // A suggestion replaces the lines it is anchored to, and those only
        // exist on the new side. GitHub rejects the alternative.
        let (files, rows) = fixture();
        let at = row_of(&rows, 1); // "-old"
        assert!(source_lines(&files, &rows, at, at).is_empty());
    }

    #[test]
    fn a_range_takes_the_new_side_of_everything_it_covers() {
        let (files, rows) = fixture();
        let (a, b) = (row_of(&rows, 0), row_of(&rows, 2));
        assert_eq!(
            source_lines(&files, &rows, a, b),
            vec!["ctx".to_string(), "new".to_string()],
            "the removed line between them is skipped"
        );
    }

    #[test]
    fn a_range_dragged_upward_reads_the_same_as_one_dragged_down() {
        let (files, rows) = fixture();
        let (a, b) = (row_of(&rows, 0), row_of(&rows, 2));
        assert_eq!(
            source_lines(&files, &rows, b, a),
            source_lines(&files, &rows, a, b)
        );
    }

    #[test]
    fn headers_and_out_of_range_rows_are_ignored_rather_than_panicking() {
        let (files, rows) = fixture();
        assert!(source_lines(&files, &rows, 0, 0).is_empty(), "a file header");
        assert!(source_lines(&files, &rows, 999, 1000).is_empty());
    }
}
