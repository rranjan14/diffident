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
