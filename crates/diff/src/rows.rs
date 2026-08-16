use crate::{DiffFile, Row};

/// Flatten parsed files into the render model (spec §3).
///
/// Rows carry indices, never copies — the UI needs them anyway to map a click
/// back to a concrete line, and cloning per row would double memory on large
/// diffs.
///
/// Cannot fail: every input shape, including files with no hunks, has a valid
/// row representation.
pub fn build_rows(files: &[DiffFile]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (file_ix, file) in files.iter().enumerate() {
        if file_ix > 0 {
            rows.push(Row::Spacer);
        }
        rows.push(Row::FileHeader { file_ix });

        let mut prev_end: Option<u32> = None;
        for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
            // The gap before this hunk: unchanged lines git omitted, either
            // since the previous hunk or since the start of the file.
            let hidden = match prev_end {
                Some(end) => hunk.new_start.saturating_sub(end),
                None => hunk.new_start.saturating_sub(1),
            };
            if hidden > 0 {
                rows.push(Row::Expander {
                    file_ix,
                    before_hunk_ix: hunk_ix,
                    hidden: Some(hidden),
                });
            }
            rows.push(Row::HunkHeader { file_ix, hunk_ix });
            for line_ix in 0..hunk.lines.len() {
                rows.push(Row::Line {
                    file_ix,
                    hunk_ix,
                    line_ix,
                });
            }
            prev_end = Some(hunk.new_start + hunk.new_count);
        }

        // A trailing gap, if the file continues past the last hunk. Emitted
        // unconditionally because the diff cannot say where the file ends —
        // an expansion that turns out to yield nothing is the standard
        // behaviour here, and is cheaper than a round trip to find out.
        if !file.hunks.is_empty() {
            rows.push(Row::Expander {
                file_ix,
                before_hunk_ix: file.hunks.len(),
                hidden: None,
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn rows_of(text: &str) -> Vec<Row> {
        build_rows(&parse(text))
    }

    const ONE_HUNK: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n";

    #[test]
    fn a_file_produces_a_header_then_its_hunk() {
        let rows = rows_of(ONE_HUNK);
        assert_eq!(rows[0], Row::FileHeader { file_ix: 0 });
        assert_eq!(
            rows[1],
            Row::HunkHeader {
                file_ix: 0,
                hunk_ix: 0
            }
        );
    }

    #[test]
    fn every_diff_line_becomes_exactly_one_row() {
        let line_rows = rows_of(ONE_HUNK)
            .into_iter()
            .filter(|r| matches!(r, Row::Line { .. }))
            .count();
        assert_eq!(line_rows, 3);
    }

    #[test]
    fn row_indices_address_back_into_the_source_files() {
        let files = parse(ONE_HUNK);
        let rows = build_rows(&files);
        let Row::Line {
            file_ix,
            hunk_ix,
            line_ix,
        } = rows[3]
        else {
            panic!("expected a line row at 3, got {:?}", rows[3]);
        };
        assert_eq!(files[file_ix].hunks[hunk_ix].lines[line_ix].text, "old");
    }

    #[test]
    fn a_binary_file_gets_a_header_and_no_line_rows() {
        let text = "diff --git a/x.png b/x.png\nBinary files a/x.png and b/x.png differ\n";
        let rows = rows_of(text);
        assert_eq!(rows[0], Row::FileHeader { file_ix: 0 });
        assert!(!rows.iter().any(|r| matches!(r, Row::Line { .. })));
    }

    fn expanders_of(text: &str) -> Vec<Row> {
        rows_of(text)
            .into_iter()
            .filter(|r| matches!(r, Row::Expander { .. }))
            .collect()
    }

    /// The gap that runs from the last hunk to end-of-file.
    fn trailing(before_hunk_ix: usize) -> Row {
        Row::Expander {
            file_ix: 0,
            before_hunk_ix,
            hidden: None,
        }
    }

    #[test]
    fn a_gap_between_hunks_becomes_an_expander() {
        let text = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,1 @@\n a\n@@ -50,1 +50,1 @@\n b\n";
        assert_eq!(
            expanders_of(text),
            vec![
                Row::Expander {
                    file_ix: 0,
                    before_hunk_ix: 1,
                    hidden: Some(48)
                },
                trailing(2),
            ]
        );
    }

    #[test]
    fn adjacent_hunks_get_no_expander_between_them() {
        let text = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,1 @@\n a\n@@ -2,1 +2,1 @@\n b\n";
        assert_eq!(expanders_of(text), vec![trailing(2)]);
    }

    #[test]
    fn a_file_whose_first_hunk_starts_below_line_one_gets_a_leading_expander() {
        // Without this there is no row to click to reveal lines 1..49, so the
        // top of such a file is permanently unreachable.
        let text = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -50,1 +50,1 @@\n-a\n+b\n";
        assert_eq!(
            expanders_of(text),
            vec![
                Row::Expander {
                    file_ix: 0,
                    before_hunk_ix: 0,
                    hidden: Some(49)
                },
                trailing(1),
            ]
        );
    }

    #[test]
    fn a_file_whose_first_hunk_starts_at_line_one_gets_no_leading_expander() {
        assert_eq!(expanders_of(ONE_HUNK), vec![trailing(1)]);
    }

    #[test]
    fn a_file_with_no_hunks_gets_no_trailing_expander() {
        // Nothing to expand into: binary and mode-only changes have no content.
        let text = "diff --git a/x.png b/x.png\nBinary files a/x.png and b/x.png differ\n";
        assert!(expanders_of(text).is_empty());
    }

    #[test]
    fn files_are_separated_by_a_spacer() {
        let text = format!("{ONE_HUNK}diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1,1 +1,1 @@\n z\n");
        let rows = rows_of(&text);
        let spacers = rows.iter().filter(|r| **r == Row::Spacer).count();
        assert_eq!(spacers, 1, "one spacer between two files, none trailing");
    }

    #[test]
    fn every_row_but_a_spacer_knows_its_file() {
        let text = format!("{ONE_HUNK}diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1,1 +1,1 @@\n z\n");
        let files = parse(&text);
        let rows = build_rows(&files);
        for row in &rows {
            match row {
                Row::Spacer => assert_eq!(row.file_ix(), None),
                _ => {
                    let ix = row.file_ix().expect("non-spacer rows have a file");
                    assert!(ix < files.len(), "file_ix must index into files");
                }
            }
        }
        // The second file's rows report index 1, not 0.
        let second = rows.iter().position(|r| *r == Row::FileHeader { file_ix: 1 }).unwrap();
        assert_eq!(rows[second].file_ix(), Some(1));
    }

    #[test]
    fn an_empty_diff_produces_no_rows() {
        assert!(build_rows(&[]).is_empty());
    }
}
