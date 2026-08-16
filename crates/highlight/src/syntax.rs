use crate::Highlights;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Loaded once and leaked for the process lifetime — the syntax dump is several
/// megabytes and parsing it per call would dominate every frame.
fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    // `_no_newlines`: our input is already newline-stripped by the diff parser,
    // and the newline-carrying variant mis-scopes end-of-line tokens against it.
    S.get_or_init(two_face::syntax::extra_no_newlines)
}

fn theme() -> &'static Theme {
    static T: OnceLock<Theme> = OnceLock::new();
    T.get_or_init(|| {
        let set: ThemeSet = two_face::theme::extra().into();
        set.themes["base16-eighties.dark"].clone()
    })
}

/// Highlight a contiguous run of source lines from one file.
///
/// Returns exactly one `Highlights` per input line, in order — callers index the
/// result positionally against their own row model, so a short return would
/// silently misalign colours.
///
/// Cannot fail: an unrecognised language yields empty highlights, which renders
/// as plain text. Refusing to show a file because we lack a grammar for it
/// would be strictly worse than showing it uncoloured.
pub fn highlight_run(path: &str, lines: &[&str]) -> Vec<Highlights> {
    let ss = syntaxes();
    let ext = path.rsplit('.').next().unwrap_or("");
    let Some(syntax) = ss
        .find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_first_line(lines.first().copied().unwrap_or("")))
    else {
        return vec![Vec::new(); lines.len()];
    };
    let mut h = HighlightLines::new(syntax, theme());
    lines
        .iter()
        .map(|line| {
            let Ok(spans) = h.highlight_line(line, ss) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            let mut at = 0usize;
            for (style, text) in spans {
                let end = at + text.len();
                let c = style.foreground;
                out.push((at..end, u32::from_be_bytes([0, c.r, c.g, c.b])));
                at = end;
            }
            out
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_sorted_contiguous_and_cover_the_line() {
        let out = highlight_run("a.rs", &["fn main() { let x = 1; }"]);
        let r = &out[0];
        assert!(!r.is_empty());
        let mut at = 0;
        for (range, _) in r {
            assert_eq!(range.start, at, "ranges must be contiguous and sorted");
            at = range.end;
        }
        assert_eq!(at, "fn main() { let x = 1; }".len());
    }

    #[test]
    fn a_keyword_and_a_literal_get_different_colours() {
        let out = highlight_run("a.rs", &["fn x() { 1 }"]);
        let colours: std::collections::HashSet<u32> = out[0].iter().map(|(_, c)| *c).collect();
        assert!(colours.len() > 1, "expected >1 colour, got {colours:?}");
    }

    #[test]
    fn state_carries_across_lines_so_block_comments_stay_comments() {
        let out = highlight_run("a.rs", &["/* open", "still comment", "*/ fn x() {}"]);
        let line1 = out[1][0].1;
        let code = *highlight_run("a.rs", &["fn x() {}"])[0]
            .first()
            .map(|(_, c)| c)
            .unwrap();
        assert_ne!(line1, code, "line 2 must still be comment-coloured");
    }

    #[test]
    fn an_unknown_extension_yields_one_empty_result_per_line() {
        let out = highlight_run("data.zzz", &["a", "b"]);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|l| l.is_empty()));
    }

    #[test]
    fn ranges_land_on_char_boundaries_for_multibyte_text() {
        let line = "let s = \"héllo → wörld\";";
        let out = highlight_run("a.rs", &[line]);
        for (range, _) in &out[0] {
            assert!(line.is_char_boundary(range.start) && line.is_char_boundary(range.end));
        }
    }
}
