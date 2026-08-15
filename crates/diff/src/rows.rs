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
            // Unchanged lines git omitted between the previous hunk and this one.
            if let Some(end) = prev_end {
                let hidden = hunk.new_start.saturating_sub(end);
                if hidden > 0 {
                    rows.push(Row::Expander {
                        file_ix,
                        before_hunk_ix: hunk_ix,
                        hidden,
                    });
                }
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

    #[test]
    fn a_gap_between_hunks_becomes_an_expander() {
        let text = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,1 @@\n a\n@@ -50,1 +50,1 @@\n b\n";
        let expanders: Vec<_> = rows_of(text)
            .into_iter()
            .filter(|r| matches!(r, Row::Expander { .. }))
            .collect();
        assert_eq!(
            expanders,
            vec![Row::Expander {
                file_ix: 0,
                before_hunk_ix: 1,
                hidden: 48
            }]
        );
    }

    #[test]
    fn adjacent_hunks_get_no_expander() {
        let text = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,1 @@\n a\n@@ -2,1 +2,1 @@\n b\n";
        assert!(!rows_of(text).iter().any(|r| matches!(r, Row::Expander { .. })));
    }

    #[test]
    fn files_are_separated_by_a_spacer() {
        let text = format!("{ONE_HUNK}diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1,1 +1,1 @@\n z\n");
        let rows = rows_of(&text);
        let spacers = rows.iter().filter(|r| **r == Row::Spacer).count();
        assert_eq!(spacers, 1, "one spacer between two files, none trailing");
    }

    #[test]
    fn an_empty_diff_produces_no_rows() {
        assert!(build_rows(&[]).is_empty());
    }
}
