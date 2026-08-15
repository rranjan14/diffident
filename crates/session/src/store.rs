use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Identifies a draft session.
///
/// `head_sha` is part of the key on purpose (spec §7): reopening the same PR at
/// the same head must reattach existing drafts, but once the author pushes, the
/// old line anchors are meaningless and the drafts must not silently reattach
/// to different code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    /// `owner/name`.
    pub repo: String,
    pub pr: u32,
    pub head_sha: String,
}

/// Everything worth surviving a restart.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    /// Draft comments. Typed as `Value` only until the comment model lands;
    /// swapping this to `Vec<Comment>` is a one-line change.
    #[serde(default)]
    pub comments: Vec<serde_json::Value>,
    /// Paths the reviewer has marked as read.
    #[serde(default)]
    pub reviewed: Vec<String>,
}

/// Draft storage rooted at a directory.
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where a key's JSON lives. Public so tests can assert non-collision.
    ///
    /// The filename is a flattened key rather than a hash: debuggability beats
    /// brevity here, and `/` is the only character needing escaping.
    pub fn path_for(&self, key: &SessionKey) -> PathBuf {
        let name = format!(
            "{}--{}--{}.json",
            key.repo.replace('/', "-"),
            key.pr,
            key.head_sha
        );
        self.root.join(name)
    }

    /// Read a session, or an empty one.
    ///
    /// Never fails: a missing file is the normal first-open case, and a corrupt
    /// file is not something the reviewer can fix mid-review. Losing unsaved
    /// drafts is bad, but refusing to open the PR at all is worse.
    pub fn load(&self, key: &SessionKey) -> Session {
        std::fs::read_to_string(self.path_for(key))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Write a session atomically.
    ///
    /// Writes to a sibling temp file then renames, so a crash mid-write leaves
    /// the previous session intact rather than a truncated file. The temp file
    /// must be a sibling — `rename` is only atomic within one filesystem.
    pub fn save(&self, key: &SessionKey, session: &Session) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let final_path = self.path_for(key);
        let tmp = final_path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(session)?)?;
        std::fs::rename(&tmp, &final_path)
    }
}

/// The default storage root: `~/.local/share/diffident/sessions` on Linux,
/// `~/Library/Application Support/diffident/sessions` on macOS.
///
/// Falls back to a temp dir when no home directory is available, so a headless
/// or sandboxed run still works instead of erroring at startup.
pub fn default_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| {
            if cfg!(target_os = "macos") {
                h.join("Library/Application Support/diffident/sessions")
            } else {
                h.join(".local/share/diffident/sessions")
            }
        })
        .unwrap_or_else(|| std::env::temp_dir().join("diffident-sessions"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SessionKey {
        SessionKey {
            repo: "owner/name".into(),
            pr: 42,
            head_sha: "abc123".into(),
        }
    }

    #[test]
    fn loading_an_unknown_session_returns_an_empty_one_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        assert!(store.load(&key()).comments.is_empty());
    }

    #[test]
    fn a_saved_session_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let mut s = Session::default();
        s.comments.push(serde_json::json!({"body": "nit"}));
        s.reviewed.push("src/main.rs".into());
        store.save(&key(), &s).unwrap();

        let loaded = store.load(&key());
        assert_eq!(loaded.comments.len(), 1);
        assert_eq!(loaded.reviewed, vec!["src/main.rs".to_string()]);
    }

    #[test]
    fn a_new_head_sha_is_a_different_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let mut s = Session::default();
        s.comments.push(serde_json::json!({"body": "old"}));
        store.save(&key(), &s).unwrap();

        let rebased = SessionKey {
            head_sha: "def456".into(),
            ..key()
        };
        assert!(store.load(&rebased).comments.is_empty());
    }

    #[test]
    fn a_corrupt_session_file_reads_as_empty_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(store.path_for(&key()), b"{ not json").unwrap();
        assert!(store.load(&key()).comments.is_empty());
    }

    #[test]
    fn save_leaves_no_temp_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.save(&key(), &Session::default()).unwrap();
        let stray: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(stray.is_empty(), "temp file left behind");
    }

    #[test]
    fn distinct_keys_do_not_collide_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let a = key();
        let b = SessionKey { pr: 43, ..key() };
        assert_ne!(store.path_for(&a), store.path_for(&b));
    }
}
