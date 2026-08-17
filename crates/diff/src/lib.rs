pub mod parser;
pub mod rows;

/// One file's change within a diff.
///
/// Paths are repo-relative with forward slashes, exactly as git emits them.
/// Do not build a `PathBuf` from them — on Windows that would silently rewrite
/// separators and break comparisons against GitHub API paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    /// `None` when the file was added.
    pub old_path: Option<String>,
    /// `None` when the file was deleted.
    pub new_path: Option<String>,
    pub status: FileStatus,
    pub kind: FileKind,
    /// Empty when `kind` is not `Text` — binary and mode-only changes have no hunks.
    pub hunks: Vec<Hunk>,
}

impl DiffFile {
    /// A fingerprint of this file's contribution to the diff (§7).
    ///
    /// Covers the path and every line's kind and text, so any edit to what the
    /// reviewer actually saw changes it. Deliberately *excludes* line numbers:
    /// a change earlier in the file shifts every later hunk's numbering without
    /// altering a byte of this file's content, and dropping the reviewer's mark
    /// for that would be noise.
    ///
    /// Not stable across releases — if the diff model changes shape, old hashes
    /// stop matching and marks reset once. That is an acceptable trade for
    /// having no versioning to maintain.
    pub fn content_hash(&self) -> u64 {
        let mut hash = fnv1a(self.display_path().as_bytes(), 0xcbf2_9ce4_8422_2325);
        for hunk in &self.hunks {
            for line in &hunk.lines {
                hash = fnv1a(&[line.kind as u8], hash);
                hash = fnv1a(line.text.as_bytes(), hash);
            }
        }
        hash
    }

    /// The path to show in UI and to send to GitHub as a comment anchor.
    /// GitHub anchors comments on the *new* path except for deletions.
    pub fn display_path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("<unknown>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    /// `similarity` is git's percentage from `similarity index NN%`.
    Renamed { similarity: u8 },
    Copied { similarity: u8 },
}

/// Why a file may have no renderable hunks.
///
/// Kept separate from `FileStatus` because the two are orthogonal: a file can be
/// both `Renamed` and `Binary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    Text,
    Binary,
    /// Permissions changed, content did not.
    ModeChangeOnly { old_mode: String, new_mode: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Text trailing the closing `@@` — usually the enclosing function.
    /// Empty when git emitted none.
    pub section: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Content with the leading `+`/`-`/space and the trailing newline removed.
    pub text: String,
    /// 1-based line number in the pre-image. `None` for added lines.
    pub old_lineno: Option<u32>,
    /// 1-based line number in the post-image. `None` for removed lines.
    pub new_lineno: Option<u32>,
    /// git emitted `\ No newline at end of file` directly after this line.
    /// Needed so round-tripping and rendering do not invent a newline.
    pub no_newline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// The flat, index-addressable render model (spec §3).
///
/// Variants carry **indices into `&[DiffFile]`, never copies**. Cloning strings
/// per row would double memory on large diffs, and the UI needs the indices
/// anyway to map a click back to a concrete line for commenting.
///
/// Index parity between `Vec<Row>` and the rendered list is the core invariant:
/// scroll position, hit-testing and keyboard navigation are all integer indices
/// into this vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    FileHeader {
        file_ix: usize,
    },
    HunkHeader {
        file_ix: usize,
        hunk_ix: usize,
    },
    Line {
        file_ix: usize,
        hunk_ix: usize,
        line_ix: usize,
    },
    /// Collapsed unchanged region.
    Expander {
        file_ix: usize,
        /// Index of the hunk this gap precedes. Equals `hunks.len()` for the
        /// gap that runs from the last hunk to the end of the file.
        before_hunk_ix: usize,
        /// How many source lines the gap covers, or `None` when that is not
        /// knowable from the diff alone — a trailing gap runs to end-of-file,
        /// and the diff never states how long the file is. Resolved when the
        /// expansion actually fetches the file.
        hidden: Option<u32>,
    },
    /// Blank separator. Carries no data so the UI can style it freely.
    Spacer,
}

/// FNV-1a, 64-bit. Ten lines and no dependency; we need a stable fingerprint,
/// not a cryptographic one.
fn fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

impl DiffLine {
    /// Whether this line is the one a comment on `line` of `old_side` anchors to.
    ///
    /// One rule, one place. It was written out twice — once for placing a
    /// remote thread on a row, once for deciding at submit time whether a
    /// draft's anchor still exists — and the two copies had to agree for a
    /// comment to land where the reviewer put it.
    ///
    /// `old_side: bool` rather than a `Side` enum: `Side` lives in
    /// `diffident-model`, and this crate depends on serde alone. A whole
    /// dependency edge to name one word is a bad trade; callers pass
    /// `matches!(side, Side::Old)`.
    ///
    /// Note the asymmetry against `composer::scope_for_line`, which is
    /// deliberate: *matching* an anchor supplied by GitHub must accept a
    /// context line from either side, while *authoring* one picks a single
    /// canonical side per keystroke.
    pub fn anchors(&self, line: u32, old_side: bool) -> bool {
        if old_side {
            self.kind != LineKind::Added && self.old_lineno == Some(line)
        } else {
            self.kind != LineKind::Removed && self.new_lineno == Some(line)
        }
    }
}

impl Row {
    /// The diff line this row shows, if it shows one.
    ///
    /// The `file_ix`/`hunk_ix`/`line_ix` walk was open-coded at four call
    /// sites in three styles — `?`-chained, `let-else`, and direct indexing
    /// that panics. Indices come from `build_rows` and are valid by
    /// construction, but `Option` keeps every caller honest without any of
    /// them having to decide that for itself.
    pub fn line<'a>(&self, files: &'a [DiffFile]) -> Option<&'a DiffLine> {
        let Row::Line {
            file_ix,
            hunk_ix,
            line_ix,
        } = *self
        else {
            return None;
        };
        files.get(file_ix)?.hunks.get(hunk_ix)?.lines.get(line_ix)
    }

    /// Which file this row belongs to, if any.
    ///
    /// `Spacer` is the only row with no file — it sits *between* two of them.
    /// Callers use this to map a cursor position back to a path, which is how
    /// "mark this file reviewed" knows which file is meant.
    pub fn file_ix(&self) -> Option<usize> {
        match *self {
            Row::FileHeader { file_ix }
            | Row::HunkHeader { file_ix, .. }
            | Row::Line { file_ix, .. }
            | Row::Expander { file_ix, .. } => Some(file_ix),
            Row::Spacer => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>) -> DiffLine {
        DiffLine {
            kind,
            text: String::new(),
            old_lineno: old,
            new_lineno: new,
            no_newline: false,
        }
    }

    #[test]
    fn a_context_line_anchors_from_either_side() {
        // The case the two former copies of this rule existed to get right: an
        // unchanged line has a number on both sides, and a comment left on
        // either one belongs to it. Getting this wrong loses the thread.
        let l = line(LineKind::Context, Some(7), Some(9));
        assert!(l.anchors(7, true), "old side");
        assert!(l.anchors(9, false), "new side");
        assert!(!l.anchors(9, true), "the new number is not the old one");
    }

    #[test]
    fn an_added_line_never_anchors_on_the_old_side() {
        // It has no pre-image, so an old-side anchor cannot mean this line
        // however the numbers happen to line up.
        let l = line(LineKind::Added, None, Some(4));
        assert!(l.anchors(4, false));
        assert!(!l.anchors(4, true));
    }

    #[test]
    fn a_removed_line_never_anchors_on_the_new_side() {
        let l = line(LineKind::Removed, Some(4), None);
        assert!(l.anchors(4, true));
        assert!(!l.anchors(4, false));
    }

    #[test]
    fn only_a_line_row_yields_a_line() {
        let files = parser::parse(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,1 @@\n ctx\n",
        );
        let rows = rows::build_rows(&files);
        assert!(
            rows.iter().any(|r| r.line(&files).is_some()),
            "the fixture has a line row"
        );
        assert!(
            Row::Spacer.line(&files).is_none(),
            "a spacer shows no line, so asking for one is None rather than a panic"
        );
    }
}
