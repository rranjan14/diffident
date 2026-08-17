//! Comment authoring: a text buffer, a key decoder, and diff-row → scope resolution.
//!
//! **No gpui here, on purpose.** GPUI ships no text input element at all — its
//! only template is `examples/input.rs`, 778 lines of hand-rolled `Element` for
//! a *single* line. So the editing model has to be ours either way, and keeping
//! it free of gpui means it is unit-testable without opening a window, which is
//! the only way this logic gets covered at all.
//!
//! The cost of not using gpui's `EntityInputHandler` is that there is no IME
//! support: composition (CJK, Korean jamo, anything routed through an input
//! method) will not work. Latin text does, including accented characters
//! produced by option/alt chords, because those arrive as finished key events
//! rather than as a composition session.

use diffident_diff::{DiffFile, LineKind, Row};
use diffident_model::comment::{CommentScope, Side};

/// A multi-line editing buffer with a single cursor.
///
/// The cursor column is a **byte** offset, not a character index, because every
/// mutation is a `String` splice and Rust splices by byte range. Storing a char
/// index would mean an O(n) walk on every keystroke to convert it back. The
/// price is that every move has to land on a char boundary deliberately — hence
/// [`TextBuffer::prev_boundary`] / [`TextBuffer::next_boundary`] and the clamp
/// in [`TextBuffer::up`] / [`TextBuffer::down`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBuffer {
    lines: Vec<String>,
    line: usize,
    col: usize,
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextBuffer {
    /// An empty buffer is one empty line, never zero lines — the cursor must
    /// always have a line to sit on, so `lines` is never empty.
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            line: 0,
            col: 0,
        }
    }

    /// Loads existing text with the cursor at the end, which is where someone
    /// resuming an edit wants to start typing.
    pub fn from_text(text: &str) -> Self {
        // `"".split('\n')` already yields one empty string, so the empty case
        // needs no special handling.
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let line = lines.len() - 1;
        let col = lines[line].len();
        Self { lines, line, col }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.line, self.col)
    }

    /// Whitespace-only counts as blank: submitting a comment of three spaces is
    /// never what the reviewer meant.
    pub fn is_blank(&self) -> bool {
        self.text().trim().is_empty()
    }

    pub fn insert(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\n' {
                self.newline();
            } else {
                self.lines[self.line].insert(self.col, ch);
                self.col += ch.len_utf8();
            }
        }
    }

    pub fn newline(&mut self) {
        let rest = self.lines[self.line].split_off(self.col);
        self.lines.insert(self.line + 1, rest);
        self.line += 1;
        self.col = 0;
    }

    pub fn backspace(&mut self) {
        if self.col > 0 {
            let start = self.prev_boundary();
            self.lines[self.line].replace_range(start..self.col, "");
            self.col = start;
        } else if self.line > 0 {
            let tail = self.lines.remove(self.line);
            self.line -= 1;
            self.col = self.lines[self.line].len();
            self.lines[self.line].push_str(&tail);
        }
    }

    pub fn delete(&mut self) {
        if self.col < self.lines[self.line].len() {
            let end = self.next_boundary();
            self.lines[self.line].replace_range(self.col..end, "");
        } else if self.line + 1 < self.lines.len() {
            let next = self.lines.remove(self.line + 1);
            self.lines[self.line].push_str(&next);
        }
    }

    pub fn left(&mut self) {
        if self.col > 0 {
            self.col = self.prev_boundary();
        } else if self.line > 0 {
            self.line -= 1;
            self.col = self.lines[self.line].len();
        }
    }

    pub fn right(&mut self) {
        if self.col < self.lines[self.line].len() {
            self.col = self.next_boundary();
        } else if self.line + 1 < self.lines.len() {
            self.line += 1;
            self.col = 0;
        }
    }

    pub fn up(&mut self) {
        if self.line > 0 {
            self.line -= 1;
            self.clamp_col();
        }
    }

    pub fn down(&mut self) {
        if self.line + 1 < self.lines.len() {
            self.line += 1;
            self.clamp_col();
        }
    }

    pub fn home(&mut self) {
        self.col = 0;
    }

    pub fn end(&mut self) {
        self.col = self.lines[self.line].len();
    }

    /// Byte offset of the character *before* the cursor.
    fn prev_boundary(&self) -> usize {
        self.lines[self.line][..self.col]
            .char_indices()
            .next_back()
            .map_or(0, |(ix, _)| ix)
    }

    /// Byte offset just past the character *at* the cursor.
    fn next_boundary(&self) -> usize {
        self.lines[self.line][self.col..]
            .chars()
            .next()
            .map_or(self.col, |ch| self.col + ch.len_utf8())
    }

    /// After a vertical move the old column may be past the new line's end, or
    /// worse, in the middle of a multibyte character — slicing there panics.
    fn clamp_col(&mut self) {
        let line = &self.lines[self.line];
        self.col = self.col.min(line.len());
        while !line.is_char_boundary(self.col) {
            self.col -= 1;
        }
    }
}

/// What a key event means to the composer, after layout and modifiers are
/// resolved. Anything unmapped is [`Key::Ignore`] rather than an error — a
/// composer that beeps at F13 helps nobody.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Insert(String),
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Newline,
    Cancel,
    Save,
    Ignore,
}

/// Decodes one key event.
///
/// `key_char` wins over `key` for text because `key` is the *layout-independent
/// key identity*, not what the user typed. On a macOS US layout option-s
/// reports `key = "s"` while producing "ß"; on a Dvorak or AZERTY layout the
/// mismatch is the norm rather than the exception. Trusting `key` would type
/// the wrong letter for anyone not on US QWERTY.
///
/// Named keys are matched first (they also carry a `key_char` on some
/// platforms), then chords are dropped — otherwise cmd-a would both select-all
/// *and* type an "a".
pub fn key_action(key: &str, key_char: Option<&str>, cmd: bool, ctrl: bool) -> Key {
    match key {
        "escape" => return Key::Cancel,
        // cmd-enter submits; plain enter is a paragraph break, since comments
        // are routinely multi-line.
        "enter" if cmd => return Key::Save,
        "enter" => return Key::Newline,
        "backspace" => return Key::Backspace,
        "delete" => return Key::Delete,
        "left" => return Key::Left,
        "right" => return Key::Right,
        "up" => return Key::Up,
        "down" => return Key::Down,
        "home" => return Key::Home,
        "end" => return Key::End,
        _ => {}
    }

    if cmd || ctrl {
        return Key::Ignore;
    }

    match key_char {
        Some(text) if !text.is_empty() && !text.chars().any(char::is_control) => {
            Key::Insert(text.to_string())
        }
        _ => Key::Ignore,
    }
}

/// The file a row belongs to, as a file-level comment scope.
pub fn scope_for_file(files: &[DiffFile], rows: &[Row], row_ix: usize) -> Option<CommentScope> {
    let file_ix = rows.get(row_ix)?.file_ix()?;
    Some(CommentScope::File {
        path: files.get(file_ix)?.display_path().to_string(),
    })
}

/// The line a row anchors to, if it is a line at all.
///
/// A removed line only exists in the pre-image, so it anchors to `old_lineno`
/// on [`Side::Old`]. Everything else anchors to `new_lineno` on [`Side::New`] —
/// including context lines, which exist on both sides: GitHub renders the new
/// side by default, so that is where the reviewer expects the comment to land.
pub fn scope_for_line(files: &[DiffFile], rows: &[Row], row_ix: usize) -> Option<CommentScope> {
    let row = rows.get(row_ix)?;
    let file = files.get(row.file_ix()?)?;
    let line = row.line(files)?;
    let (lineno, side) = match line.kind {
        LineKind::Removed => (line.old_lineno, Side::Old),
        _ => (line.new_lineno, Side::New),
    };
    Some(CommentScope::Line {
        path: file.display_path().to_string(),
        line: lineno?,
        side,
    })
}

/// A multi-line selection as a range scope, in either drag direction.
///
/// Mixed-side and cross-file ranges are refused **here, at authoring time**,
/// rather than at submit: GitHub 422s them, and a rejection surfacing minutes
/// later after the reviewer has typed a paragraph is a worse failure than the
/// selection simply not offering a comment.
///
/// A range whose ends coincide collapses to a `Line` for the same reason —
/// GitHub errors on `start_line == line`.
pub fn scope_for_range(
    files: &[DiffFile],
    rows: &[Row],
    a: usize,
    b: usize,
) -> Option<CommentScope> {
    let (
        CommentScope::Line {
            path: path_a,
            line: line_a,
            side: side_a,
        },
        CommentScope::Line {
            path: path_b,
            line: line_b,
            side: side_b,
        },
    ) = (
        scope_for_line(files, rows, a)?,
        scope_for_line(files, rows, b)?,
    )
    else {
        return None;
    };

    if path_a != path_b || side_a != side_b {
        return None;
    }
    if line_a == line_b {
        return Some(CommentScope::Line {
            path: path_a,
            line: line_a,
            side: side_a,
        });
    }
    Some(CommentScope::Range {
        path: path_a,
        start_line: line_a.min(line_b),
        end_line: line_a.max(line_b),
        side: side_a,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_advances_the_cursor() {
        let mut b = TextBuffer::new();
        b.insert("hi");
        assert_eq!(b.text(), "hi");
        assert_eq!(b.cursor(), (0, 2));
    }

    #[test]
    fn a_whitespace_only_buffer_counts_as_blank() {
        assert!(TextBuffer::new().is_blank());
        assert!(TextBuffer::from_text("  \n\t\n ").is_blank());
        assert!(!TextBuffer::from_text("  x  ").is_blank());
    }

    #[test]
    fn enter_splits_the_line_at_the_cursor() {
        let mut b = TextBuffer::from_text("abcd");
        b.home();
        b.right();
        b.right();
        b.newline();
        assert_eq!(b.text(), "ab\ncd");
        assert_eq!(b.cursor(), (1, 0));
    }

    #[test]
    fn multi_line_text_round_trips() {
        let b = TextBuffer::from_text("one\ntwo\nthree");
        assert_eq!(b.lines(), ["one", "two", "three"]);
        assert_eq!(b.text(), "one\ntwo\nthree");
    }

    #[test]
    fn loading_text_puts_the_cursor_at_the_end() {
        let b = TextBuffer::from_text("one\ntwo");
        assert_eq!(b.cursor(), (1, 3));
    }

    #[test]
    fn an_empty_buffer_still_has_one_line() {
        let b = TextBuffer::from_text("");
        assert_eq!(b.lines(), [""]);
        assert_eq!(b.cursor(), (0, 0));
        assert_eq!(b, TextBuffer::default());
    }

    #[test]
    fn backspace_at_the_start_of_a_line_joins_it_to_the_previous() {
        let mut b = TextBuffer::from_text("ab\ncd");
        b.home();
        b.backspace();
        assert_eq!(b.text(), "abcd");
        assert_eq!(b.cursor(), (0, 2));
    }

    #[test]
    fn backspace_on_an_empty_buffer_does_nothing() {
        let mut b = TextBuffer::new();
        b.backspace();
        assert_eq!(b, TextBuffer::new());
    }

    #[test]
    fn delete_at_the_end_of_a_line_pulls_the_next_one_up() {
        let mut b = TextBuffer::from_text("ab\ncd");
        b.up();
        b.end();
        b.delete();
        assert_eq!(b.text(), "abcd");
        assert_eq!(b.cursor(), (0, 2));
    }

    #[test]
    fn delete_at_the_very_end_does_nothing() {
        let mut b = TextBuffer::from_text("ab");
        b.delete();
        assert_eq!(b.text(), "ab");
        assert_eq!(b.cursor(), (0, 2));
    }

    #[test]
    fn backspace_removes_a_whole_multibyte_character() {
        let mut b = TextBuffer::from_text("héllo→");
        b.backspace();
        assert_eq!(b.text(), "héllo");
        b.home();
        b.right();
        b.right();
        b.backspace();
        assert_eq!(b.text(), "hllo");
    }

    #[test]
    fn arrows_step_over_whole_multibyte_characters() {
        let mut b = TextBuffer::from_text("é→");
        b.home();
        b.right();
        assert_eq!(b.cursor(), (0, 2)); // past 'é', 2 bytes
        b.right();
        assert_eq!(b.cursor(), (0, 5)); // past '→', 3 more
        b.left();
        assert_eq!(b.cursor(), (0, 2));
    }

    #[test]
    fn delete_removes_a_whole_multibyte_character() {
        let mut b = TextBuffer::from_text("→x");
        b.home();
        b.delete();
        assert_eq!(b.text(), "x");
    }

    #[test]
    fn left_and_right_wrap_across_line_ends() {
        let mut b = TextBuffer::from_text("ab\ncd");
        b.home();
        b.left();
        assert_eq!(b.cursor(), (0, 2));
        b.right();
        assert_eq!(b.cursor(), (1, 0));
    }

    #[test]
    fn moving_up_clamps_to_a_shorter_line() {
        let mut b = TextBuffer::from_text("ab\nlonger");
        assert_eq!(b.cursor(), (1, 6));
        b.up();
        assert_eq!(b.cursor(), (0, 2));
    }

    #[test]
    fn moving_between_lines_never_lands_mid_character() {
        // Byte 2 of "xxxx" is a boundary, but on "aéb" it is inside the 'é';
        // landing there would panic the next time anything slices the string.
        let mut b = TextBuffer::from_text("aéb\nxxxx");
        b.home();
        b.right();
        b.right();
        assert_eq!(b.cursor(), (1, 2));
        b.up();
        let (line, col) = b.cursor();
        assert!(b.lines()[line].is_char_boundary(col));
        assert_eq!((line, col), (0, 1));
        b.down();
        let (line, col) = b.cursor();
        assert!(b.lines()[line].is_char_boundary(col));
    }

    #[test]
    fn up_and_down_stop_at_the_ends() {
        let mut b = TextBuffer::from_text("ab\ncd");
        b.up();
        b.up();
        assert_eq!(b.cursor().0, 0);
        b.down();
        b.down();
        assert_eq!(b.cursor().0, 1);
    }

    #[test]
    fn inserting_text_containing_a_newline_splits_the_line() {
        let mut b = TextBuffer::from_text("ad");
        b.left();
        b.insert("b\nc");
        assert_eq!(b.text(), "ab\ncd");
        assert_eq!(b.cursor(), (1, 1));
    }

    #[test]
    fn home_and_end_jump_within_the_current_line() {
        let mut b = TextBuffer::from_text("ab\ncdef");
        b.home();
        assert_eq!(b.cursor(), (1, 0));
        b.end();
        assert_eq!(b.cursor(), (1, 4));
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn a_typed_character_is_inserted() {
        assert_eq!(
            key_action("a", Some("a"), false, false),
            Key::Insert("a".into())
        );
    }

    #[test]
    fn the_produced_character_wins_over_the_key_name() {
        // macOS option-s reports key "s" but types "ß"; obeying `key` would
        // type the wrong letter on every non-US layout.
        assert_eq!(
            key_action("s", Some("ß"), false, false),
            Key::Insert("ß".into())
        );
    }

    #[test]
    fn a_command_chord_types_nothing() {
        assert_eq!(key_action("a", Some("a"), true, false), Key::Ignore);
        assert_eq!(key_action("a", Some("a"), false, true), Key::Ignore);
    }

    #[test]
    fn enter_breaks_a_line_but_cmd_enter_saves() {
        assert_eq!(key_action("enter", None, false, false), Key::Newline);
        assert_eq!(key_action("enter", None, true, false), Key::Save);
    }

    #[test]
    fn escape_cancels() {
        assert_eq!(key_action("escape", None, false, false), Key::Cancel);
    }

    #[test]
    fn every_movement_key_is_mapped() {
        for (key, want) in [
            ("backspace", Key::Backspace),
            ("delete", Key::Delete),
            ("left", Key::Left),
            ("right", Key::Right),
            ("up", Key::Up),
            ("down", Key::Down),
            ("home", Key::Home),
            ("end", Key::End),
        ] {
            assert_eq!(key_action(key, None, false, false), want);
        }
    }

    #[test]
    fn named_keys_win_even_when_a_character_is_reported() {
        // Some platforms attach "\r" to enter and "\u{7f}" to backspace.
        assert_eq!(key_action("enter", Some("\r"), false, false), Key::Newline);
        assert_eq!(
            key_action("backspace", Some("\u{7f}"), false, false),
            Key::Backspace
        );
    }

    #[test]
    fn a_control_character_is_never_typed_into_the_buffer() {
        assert_eq!(key_action("f5", Some("\u{1b}"), false, false), Key::Ignore);
        assert_eq!(key_action("tab", Some("\t"), false, false), Key::Ignore);
    }

    #[test]
    fn space_is_typed_and_never_treated_as_a_named_key() {
        // Since Phase 7c, `space` in the `Diff` context resolves a thread on
        // GitHub. The composer is protected by holding no key context while it
        // has focus, but this is the other half: space must decode as text.
        // A reply is mostly spaces, so a regression here would not be subtle —
        // it would write to someone else's pull request once per word.
        assert_eq!(
            key_action("space", Some(" "), false, false),
            Key::Insert(" ".into())
        );
    }

    #[test]
    fn a_key_with_no_character_is_ignored() {
        assert_eq!(key_action("f13", None, false, false), Key::Ignore);
        assert_eq!(key_action("shift", Some(""), false, false), Key::Ignore);
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    use diffident_diff::{parser::parse, rows::build_rows};

    const DIFF: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1,0 +1,1 @@\n+solo\n";

    fn fixture() -> (Vec<DiffFile>, Vec<Row>) {
        let files = parse(DIFF);
        let rows = build_rows(&files);
        (files, rows)
    }

    /// Row index of the `n`th `Row::Line` (0-based): 0 = ctx, 1 = removed,
    /// 2 = added in `a.rs`, 3 = the added line in `b.rs`.
    fn nth_line(rows: &[Row], n: usize) -> usize {
        rows.iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, Row::Line { .. }))
            .map(|(ix, _)| ix)
            .nth(n)
            .expect("fixture has that many lines")
    }

    fn line(path: &str, line: u32, side: Side) -> Option<CommentScope> {
        Some(CommentScope::Line {
            path: path.into(),
            line,
            side,
        })
    }

    #[test]
    fn an_added_line_anchors_to_the_new_side() {
        let (files, rows) = fixture();
        let ix = nth_line(&rows, 2);
        assert_eq!(
            scope_for_line(&files, &rows, ix),
            line("a.rs", 2, Side::New)
        );
    }

    #[test]
    fn a_removed_line_anchors_to_the_old_side() {
        // It has no new_lineno at all — the new side is where it stopped existing.
        let (files, rows) = fixture();
        let ix = nth_line(&rows, 1);
        assert_eq!(
            scope_for_line(&files, &rows, ix),
            line("a.rs", 2, Side::Old)
        );
    }

    #[test]
    fn a_context_line_prefers_the_new_side() {
        let (files, rows) = fixture();
        let ix = nth_line(&rows, 0);
        assert_eq!(
            scope_for_line(&files, &rows, ix),
            line("a.rs", 1, Side::New)
        );
    }

    #[test]
    fn headers_and_spacers_carry_no_line_scope() {
        let (files, rows) = fixture();
        for (ix, row) in rows.iter().enumerate() {
            if !matches!(row, Row::Line { .. }) {
                assert_eq!(scope_for_line(&files, &rows, ix), None, "row {ix}: {row:?}");
            }
        }
        assert!(rows.iter().any(|r| matches!(r, Row::Spacer)));
        assert!(rows.iter().any(|r| matches!(r, Row::FileHeader { .. })));
        assert!(rows.iter().any(|r| matches!(r, Row::Expander { .. })));
    }

    #[test]
    fn an_out_of_bounds_row_has_no_scope() {
        let (files, rows) = fixture();
        assert_eq!(scope_for_line(&files, &rows, rows.len()), None);
        assert_eq!(scope_for_file(&files, &rows, rows.len()), None);
    }

    #[test]
    fn a_file_comment_can_be_started_from_any_row_of_that_file() {
        let (files, rows) = fixture();
        let want = Some(CommentScope::File { path: "a.rs".into() });
        for ix in 0..rows.len() {
            if rows[ix].file_ix() == Some(0) {
                assert_eq!(scope_for_file(&files, &rows, ix), want, "row {ix}");
            }
        }
        let solo = nth_line(&rows, 3);
        assert_eq!(
            scope_for_file(&files, &rows, solo),
            Some(CommentScope::File { path: "b.rs".into() })
        );
    }

    #[test]
    fn a_spacer_belongs_to_no_file() {
        let (files, rows) = fixture();
        let ix = rows.iter().position(|r| matches!(r, Row::Spacer)).unwrap();
        assert_eq!(scope_for_file(&files, &rows, ix), None);
    }

    #[test]
    fn two_lines_on_the_same_side_make_a_range() {
        let (files, rows) = fixture();
        let (ctx, added) = (nth_line(&rows, 0), nth_line(&rows, 2));
        assert_eq!(
            scope_for_range(&files, &rows, ctx, added),
            Some(CommentScope::Range {
                path: "a.rs".into(),
                start_line: 1,
                end_line: 2,
                side: Side::New,
            })
        );
    }

    #[test]
    fn selecting_bottom_to_top_gives_the_same_range() {
        let (files, rows) = fixture();
        let (ctx, added) = (nth_line(&rows, 0), nth_line(&rows, 2));
        assert_eq!(
            scope_for_range(&files, &rows, added, ctx),
            scope_for_range(&files, &rows, ctx, added)
        );
    }

    #[test]
    fn a_range_mixing_sides_is_refused_at_authoring_time() {
        // GitHub 422s a range that starts on LEFT and ends on RIGHT. Better to
        // offer no comment than to reject a written one at submit.
        let (files, rows) = fixture();
        let (removed, added) = (nth_line(&rows, 1), nth_line(&rows, 2));
        assert_eq!(scope_for_range(&files, &rows, removed, added), None);
    }

    #[test]
    fn a_range_spanning_two_files_is_refused() {
        let (files, rows) = fixture();
        let (a, b) = (nth_line(&rows, 2), nth_line(&rows, 3));
        assert_eq!(scope_for_range(&files, &rows, a, b), None);
    }

    #[test]
    fn a_range_anchored_to_one_line_collapses_to_a_line_comment() {
        // GitHub errors when start_line equals line.
        let (files, rows) = fixture();
        let ix = nth_line(&rows, 2);
        assert_eq!(
            scope_for_range(&files, &rows, ix, ix),
            line("a.rs", 2, Side::New)
        );
    }

    #[test]
    fn a_range_touching_a_non_line_row_is_refused() {
        let (files, rows) = fixture();
        let added = nth_line(&rows, 2);
        assert_eq!(scope_for_range(&files, &rows, 0, added), None);
        assert_eq!(scope_for_range(&files, &rows, added, 0), None);
    }
}
