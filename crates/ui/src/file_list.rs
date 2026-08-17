use crate::theme::Theme;
use diffident_diff::{DiffFile, FileStatus, LineKind, Row};
use gpui::{IntoElement, ParentElement, SharedString, div, prelude::*, px};

/// One row of the file panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// The new path, except for deletions — matches what GitHub anchors on.
    pub path: String,
    pub status: FileStatus,
    pub added: u32,
    pub removed: u32,
    /// Index of this file's `Row::FileHeader`. Clicking the entry scrolls the
    /// diff list to exactly this index, which is why the row model and the list
    /// must stay index-parallel (§3).
    pub row_ix: usize,
}

/// Summarise each file for the side panel.
///
/// Cannot fail: a file with no hunks (binary, mode-only) reports zero counts
/// rather than being dropped — it still changed, and hiding it would make the
/// panel disagree with the diff beside it.
pub fn file_entries(files: &[DiffFile], rows: &[Row]) -> Vec<FileEntry> {
    files
        .iter()
        .enumerate()
        .map(|(file_ix, file)| {
            let (mut added, mut removed) = (0, 0);
            for hunk in &file.hunks {
                for line in &hunk.lines {
                    match line.kind {
                        LineKind::Added => added += 1,
                        LineKind::Removed => removed += 1,
                        LineKind::Context => {}
                    }
                }
            }
            FileEntry {
                path: file.display_path().to_string(),
                status: file.status.clone(),
                added,
                removed,
                row_ix: rows
                    .iter()
                    .position(|r| matches!(r, Row::FileHeader { file_ix: f } if *f == file_ix))
                    .expect("build_rows emits a FileHeader for every file"),
            }
        })
        .collect()
}

/// The one-character status glyph shown before a filename.
pub fn status_glyph(status: &FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "A",
        FileStatus::Deleted => "D",
        FileStatus::Modified => "M",
        FileStatus::Renamed { .. } => "R",
        FileStatus::Copied { .. } => "C",
    }
}
/// The marker shown before a filename in the panel.
///
/// A check for read, a space for unread — same width either way, so the
/// filenames stay aligned in a column rather than jittering as you mark them.
pub fn reviewed_marker(reviewed: bool) -> &'static str {
    if reviewed { "✓" } else { " " }
}

/// One file-panel row. The path elides in the middle so both ends stay visible.
pub fn file_row(entry: &FileEntry, is_read: bool, theme: &Theme) -> impl IntoElement + use<> {
    div()
        .flex()
        .justify_between()
        .gap(px(theme.s2))
        .px(px(theme.s2))
        .py(px(theme.s1))
        .text_size(px(theme.ui_sm))
        .rounded_md()
        .hover(|this| this.bg(theme.surface_raised))
        .child(
            div()
                .flex()
                .gap(px(theme.s1))
                .text_color(theme.text_secondary)
                .child(SharedString::from(format!(
                    "{} {}",
                    reviewed_marker(is_read),
                    status_glyph(&entry.status),
                )))
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_middle()
                        .text_size(px(theme.ui_sm))
                        .child(SharedString::from(entry.path.clone())),
                ),
        )
        .child(
            div()
                .flex()
                .gap(px(theme.s1))
                .child(
                    div()
                        .text_color(theme.added_fg)
                        .child(SharedString::from(format!("+{}", entry.added))),
                )
                .child(
                    div()
                        .text_color(theme.removed_fg)
                        .child(SharedString::from(format!("-{}", entry.removed))),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_diff::{parser::parse, rows::build_rows};

    const TWO_FILES: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n@@ -50,1 +50,2 @@\n k\n+add\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1,1 +1,1 @@\n z\n";

    #[test]
    fn the_reviewed_marker_is_the_same_width_either_way() {
        // The filename column must not jitter as files are marked read.
        assert_eq!(
            reviewed_marker(true).chars().count(),
            reviewed_marker(false).chars().count()
        );
    }

    #[test]
    fn file_entries_count_added_and_removed_lines_per_file() {
        let files = parse(TWO_FILES);
        let e = file_entries(&files, &build_rows(&files));
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].added, e[0].removed), (2, 1));
        assert_eq!((e[1].added, e[1].removed), (0, 0));
    }

    #[test]
    fn file_entry_row_ix_points_at_that_files_header() {
        // This is what makes clicking a filename scroll the diff to it.
        let files = parse(TWO_FILES);
        let rows = build_rows(&files);
        for (ix, entry) in file_entries(&files, &rows).iter().enumerate() {
            assert_eq!(rows[entry.row_ix], Row::FileHeader { file_ix: ix });
        }
    }

    #[test]
    fn a_binary_file_has_zero_counts_rather_than_being_omitted() {
        let files = parse("diff --git a/x.png b/x.png\nBinary files a/x.png and b/x.png differ\n");
        let e = file_entries(&files, &build_rows(&files));
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].added, e[0].removed), (0, 0));
        assert_eq!(e[0].path, "x.png");
    }

    #[test]
    fn a_renamed_file_is_listed_under_its_new_path() {
        let files = parse(
            "diff --git a/old.rs b/new.rs\nsimilarity index 95%\nrename from old.rs\nrename to new.rs\n@@ -1,1 +1,1 @@\n a\n",
        );
        let e = file_entries(&files, &build_rows(&files));
        assert_eq!(e[0].path, "new.rs");
        assert_eq!(e[0].status, FileStatus::Renamed { similarity: 95 });
    }

    #[test]
    fn an_empty_diff_lists_no_files() {
        assert!(file_entries(&[], &[]).is_empty());
    }
}
