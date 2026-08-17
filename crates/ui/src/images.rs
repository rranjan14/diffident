//! Showing image files instead of "Binary files differ" (§8).
//!
//! The parser already marks these `FileKind::Binary` and gives them no hunks,
//! so today they render as a header and nothing else — the reviewer is asked to
//! approve a change they cannot see. This module decides which binaries are
//! images, and turns GitHub's contents response into bytes gpui can decode.
//!
//! Pure: no fetching, no rendering. Both halves are testable without a network
//! or a window.

use gpui::ImageFormat;

/// The image format for `path`, or `None` when it is not one we can show.
///
/// By extension rather than by sniffing bytes, because the decision is made
/// *before* fetching — sniffing would mean downloading every binary in the diff
/// to find out it was a `.zip`.
///
/// SVG is excluded on purpose: it is text, so git diffs it line by line and the
/// reviewer is better served by the diff they already have than by two pictures.
pub fn format_of(path: &str) -> Option<ImageFormat> {
    let ext = path.rsplit('.').next()?.to_lowercase();
    match ext.as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::Webp),
        "bmp" => Some(ImageFormat::Bmp),
        "ico" => Some(ImageFormat::Ico),
        _ => None,
    }
}

/// Pull the file's bytes out of a GitHub contents response.
///
/// The raw `Accept` header used elsewhere returns bytes directly, which cannot
/// travel through `GhRunner` — it hands back a `String`, and lossy UTF-8 would
/// corrupt a PNG beyond recognition. So images go the other way: the default
/// JSON envelope, with `content` base64-encoded, which is text the whole way.
///
/// GitHub wraps the base64 at 60 columns, so the newlines have to come out
/// before decoding.
pub fn decode_contents(json: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("contents were not JSON: {e}"))?;
    let encoded = v
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| "contents had no `content` field".to_string())?;
    let packed: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(packed)
        .map_err(|e| format!("contents were not base64: {e}"))
}

/// A file's before and after, either of which may be absent — an image can be
/// added or deleted as well as changed.
#[derive(Debug, Clone, Default)]
pub struct Pair {
    pub old: Option<Vec<u8>>,
    pub new: Option<Vec<u8>>,
}

impl Pair {
    /// Nothing to show. An image whose fetch failed on both sides should say so
    /// rather than render two empty boxes.
    pub fn is_empty(&self) -> bool {
        self.old.is_none() && self.new.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_raster_formats_are_recognised() {
        assert_eq!(format_of("a/b/logo.png"), Some(ImageFormat::Png));
        assert_eq!(format_of("shot.JPEG"), Some(ImageFormat::Jpeg), "case-insensitive");
        assert_eq!(format_of("anim.gif"), Some(ImageFormat::Gif));
    }

    #[test]
    fn svg_is_left_to_the_text_diff() {
        // It is text; git already diffs it line by line, and the reviewer is
        // better served by that than by two pictures.
        assert_eq!(format_of("icon.svg"), None);
    }

    #[test]
    fn a_non_image_binary_is_not_offered() {
        assert_eq!(format_of("fixture.zip"), None);
        assert_eq!(format_of("Makefile"), None, "no extension at all");
    }

    #[test]
    fn contents_are_decoded_from_the_base64_envelope() {
        // The raw Accept header cannot be used here: GhRunner returns a String,
        // and lossy UTF-8 would corrupt a PNG beyond recognition.
        let json = r#"{"content":"aGVsbG8=","encoding":"base64"}"#;
        assert_eq!(decode_contents(json).unwrap(), b"hello");
    }

    #[test]
    fn githubs_wrapped_base64_decodes() {
        // It wraps at 60 columns, so the newlines have to come out first.
        let json = "{\"content\":\"aGVs\\nbG8=\\n\"}";
        assert_eq!(decode_contents(json).unwrap(), b"hello");
    }

    #[test]
    fn a_response_without_content_says_so_rather_than_yielding_nothing() {
        // An empty Vec would render as a zero-byte image and look like the file
        // was genuinely empty.
        let err = decode_contents(r#"{"message":"Not Found"}"#).unwrap_err();
        assert!(err.contains("content"), "{err}");
    }

    #[test]
    fn malformed_json_surfaces_rather_than_panicking() {
        assert!(decode_contents("not json").is_err());
    }

    #[test]
    fn an_added_image_has_only_a_new_side() {
        let pair = Pair {
            old: None,
            new: Some(vec![1, 2, 3]),
        };
        assert!(!pair.is_empty());
    }

    #[test]
    fn a_pair_with_neither_side_is_empty() {
        assert!(Pair::default().is_empty());
    }
}
