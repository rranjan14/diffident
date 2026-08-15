use crate::{DiffFile, DiffLine, FileKind, FileStatus, Hunk, LineKind};

/// Parse unified diff text into files.
///
/// Never fails: unrecognised extended headers and leading noise are skipped.
/// A caller has no useful recovery for a malformed patch, and dropping every
/// file because one header is odd is strictly worse than dropping that header.
pub fn parse(text: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        if !line.starts_with("diff --git ") {
            continue; // leading noise, or trailing junk between files
        }
        let mut file = DiffFile {
            old_path: None,
            new_path: None,
            status: FileStatus::Modified,
            kind: FileKind::Text,
            hunks: Vec::new(),
        };
        let (mut old_mode, mut new_mode) = (None, None);

        // Extended header block: everything up to the first `@@`, `diff --git`,
        // or binary marker.
        while let Some(&next) = lines.peek() {
            if next.starts_with("@@") || next.starts_with("diff --git ") {
                break;
            }
            let header = lines.next().expect("peeked");
            if let Some(rest) = header.strip_prefix("similarity index ") {
                let pct = rest.trim_end_matches('%').parse().unwrap_or(0);
                file.status = FileStatus::Renamed { similarity: pct };
            } else if let Some(p) = header.strip_prefix("rename from ") {
                file.old_path = Some(p.to_string());
            } else if let Some(p) = header.strip_prefix("rename to ") {
                file.new_path = Some(p.to_string());
            } else if let Some(p) = header.strip_prefix("copy from ") {
                file.old_path = Some(p.to_string());
                if let FileStatus::Renamed { similarity } = file.status {
                    file.status = FileStatus::Copied { similarity };
                }
            } else if let Some(p) = header.strip_prefix("copy to ") {
                file.new_path = Some(p.to_string());
            } else if header.starts_with("new file mode") {
                file.status = FileStatus::Added;
            } else if header.starts_with("deleted file mode") {
                file.status = FileStatus::Deleted;
            } else if let Some(m) = header.strip_prefix("old mode ") {
                old_mode = Some(m.trim().to_string());
            } else if let Some(m) = header.strip_prefix("new mode ") {
                new_mode = Some(m.trim().to_string());
            } else if header.starts_with("Binary files ") || header.starts_with("GIT binary patch")
            {
                file.kind = FileKind::Binary;
            } else if let Some(p) = header.strip_prefix("--- ")
                && p != "/dev/null"
            {
                file.old_path = Some(strip_prefix_marker(p));
            } else if let Some(p) = header.strip_prefix("+++ ")
                && p != "/dev/null"
            {
                file.new_path = Some(strip_prefix_marker(p));
            }
            // Anything else (`index …`, unknown extensions) is deliberately ignored.
        }

        if let (Some(o), Some(n)) = (old_mode, new_mode)
            && file.kind == FileKind::Text
        {
            file.kind = FileKind::ModeChangeOnly {
                old_mode: o,
                new_mode: n,
            };
        }

        // Fall back to the `diff --git a/X b/X` paths when no ---/+++ appeared
        // (mode-only and binary changes often omit them).
        if file.old_path.is_none()
            && file.new_path.is_none()
            && let Some((a, b)) = split_git_header_paths(line)
        {
            file.old_path = Some(a);
            file.new_path = Some(b);
        }

        // Hunks.
        while let Some(&next) = lines.peek() {
            if !next.starts_with("@@") {
                break;
            }
            let header = lines.next().expect("peeked");
            let Some(mut hunk) = parse_hunk_header(header) else {
                continue;
            };
            let (mut old_no, mut new_no) = (hunk.old_start, hunk.new_start);

            while let Some(&body) = lines.peek() {
                if body.starts_with("@@") || body.starts_with("diff --git ") {
                    break;
                }
                let body = lines.next().expect("peeked");

                if body.starts_with('\\') {
                    // `\ No newline at end of file` — belongs to the line before it.
                    if let Some(last) = hunk.lines.last_mut() {
                        last.no_newline = true;
                    }
                    continue;
                }

                let (kind, text) = match body.chars().next() {
                    Some('+') => (LineKind::Added, &body[1..]),
                    Some('-') => (LineKind::Removed, &body[1..]),
                    Some(' ') => (LineKind::Context, &body[1..]),
                    // git emits a bare empty line for an empty context line.
                    None => (LineKind::Context, ""),
                    _ => break, // not part of this hunk
                };

                let (old_lineno, new_lineno) = match kind {
                    LineKind::Added => (None, Some(new_no)),
                    LineKind::Removed => (Some(old_no), None),
                    LineKind::Context => (Some(old_no), Some(new_no)),
                };
                match kind {
                    LineKind::Added => new_no += 1,
                    LineKind::Removed => old_no += 1,
                    LineKind::Context => {
                        old_no += 1;
                        new_no += 1;
                    }
                }

                hunk.lines.push(DiffLine {
                    kind,
                    text: text.to_string(),
                    old_lineno,
                    new_lineno,
                    no_newline: false,
                });
            }
            file.hunks.push(hunk);
        }

        files.push(file);
    }
    files
}

/// Strip git's `a/` or `b/` prefix and any trailing tab-separated timestamp.
fn strip_prefix_marker(p: &str) -> String {
    let p = p.split('\t').next().unwrap_or(p);
    p.strip_prefix("a/")
        .or_else(|| p.strip_prefix("b/"))
        .unwrap_or(p)
        .to_string()
}

/// `diff --git a/x b/y` -> ("x", "y"). Returns `None` on paths containing
/// spaces, which git quotes differently; the ---/+++ lines cover that case.
fn split_git_header_paths(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("diff --git ")?;
    let mut parts = rest.split(' ');
    let a = parts.next()?;
    let b = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some((strip_prefix_marker(a), strip_prefix_marker(b)))
}

/// `@@ -a,b +c,d @@ section` -> a `Hunk` with no lines yet.
/// An omitted count means 1 — this is the single most commonly mis-parsed
/// part of the format.
fn parse_hunk_header(header: &str) -> Option<Hunk> {
    let rest = header.strip_prefix("@@ ")?;
    let (ranges, section) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(' ')?;
    let (old_start, old_count) = parse_range(old.strip_prefix('-')?)?;
    let (new_start, new_count) = parse_range(new.strip_prefix('+')?)?;
    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        section: section.trim().to_string(),
        lines: Vec::new(),
    })
}

fn parse_range(r: &str) -> Option<(u32, u32)> {
    match r.split_once(',') {
        Some((s, c)) => Some((s.parse().ok()?, c.parse().ok()?)),
        None => Some((r.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileKind, FileStatus, LineKind};

    fn edge_cases() -> Vec<DiffFile> {
        parse(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/edge_cases.diff")))
    }

    #[test]
    fn parses_every_file_in_the_fixture() {
        assert_eq!(edge_cases().len(), 7);
    }

    #[test]
    fn added_file_has_no_old_path() {
        let f = &edge_cases()[0];
        assert_eq!(f.status, FileStatus::Added);
        assert_eq!(f.old_path, None);
        assert_eq!(f.new_path.as_deref(), Some("added.txt"));
    }

    #[test]
    fn deleted_file_has_no_new_path() {
        let f = &edge_cases()[1];
        assert_eq!(f.status, FileStatus::Deleted);
        assert_eq!(f.new_path, None);
    }

    #[test]
    fn renamed_file_keeps_both_paths() {
        let f = &edge_cases()[2];
        assert_eq!(f.status, FileStatus::Renamed { similarity: 95 });
        assert_eq!(f.old_path.as_deref(), Some("old_name.txt"));
        assert_eq!(f.new_path.as_deref(), Some("new_name.txt"));
    }

    #[test]
    fn mode_only_change_has_no_hunks() {
        let f = &edge_cases()[3];
        assert!(f.hunks.is_empty());
        assert_eq!(
            f.kind,
            FileKind::ModeChangeOnly {
                old_mode: "100644".into(),
                new_mode: "100755".into()
            }
        );
    }

    #[test]
    fn binary_file_has_no_hunks() {
        let f = &edge_cases()[4];
        assert_eq!(f.kind, FileKind::Binary);
        assert!(f.hunks.is_empty());
    }

    #[test]
    fn no_newline_marker_attaches_to_preceding_line() {
        let f = &edge_cases()[5];
        let lines = &f.hunks[0].lines;
        assert_eq!(lines.len(), 2, "the marker must not become its own line");
        assert!(lines[0].no_newline);
        assert!(lines[1].no_newline);
    }

    #[test]
    fn omitted_hunk_count_means_one() {
        let h = &edge_cases()[6].hunks[0];
        assert_eq!((h.old_start, h.old_count), (12, 1));
        assert_eq!((h.new_start, h.new_count), (12, 2));
    }

    #[test]
    fn hunk_section_heading_is_captured() {
        assert_eq!(edge_cases()[6].hunks[0].section, "fn enclosing_function()");
    }

    #[test]
    fn line_numbers_advance_per_side() {
        let h = &edge_cases()[2].hunks[0];
        let ctx = &h.lines[0];
        assert_eq!((ctx.old_lineno, ctx.new_lineno), (Some(1), Some(1)));
        let removed = &h.lines[1];
        assert_eq!(removed.kind, LineKind::Removed);
        assert_eq!((removed.old_lineno, removed.new_lineno), (Some(2), None));
        let added = &h.lines[2];
        assert_eq!((added.old_lineno, added.new_lineno), (None, Some(2)));
    }

    #[test]
    fn content_line_starting_with_triple_dash_is_not_a_header() {
        let text = "diff --git a/a.md b/a.md\n--- a/a.md\n+++ b/a.md\n@@ -1,2 +1,2 @@\n---\n+++\n";
        let files = parse(text);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks[0].lines.len(), 2);
    }

    #[test]
    fn garbage_before_first_file_is_skipped() {
        assert!(parse("warning: something\n").is_empty());
    }

    #[test]
    fn real_pr_diff_parses_without_panicking() {
        let files = parse(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/simple.diff")));
        assert!(!files.is_empty());
        for f in &files {
            assert!(f.old_path.is_some() || f.new_path.is_some());
        }
    }
}
