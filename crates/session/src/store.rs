use diffident_model::comment::Comment;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Identifies a stored session: one file per pull request.
///
/// The head SHA is deliberately **not** part of the key, though §7 describes it
/// that way. Keying the file on the head means a force-push starts a wholly
/// empty session, which would make the `content_hash` that §7 asks for in the
/// very next sentence dead weight — nothing could ever survive to be checked
/// against it. Instead the head is stored *inside*, so it can govern the drafts
/// (meaningless once their line anchors move) while reviewed marks survive and
/// are invalidated per file by their hash. Both of §7's sentences then do work.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    /// `owner/name`.
    pub repo: String,
    pub pr: u32,
}

/// Everything worth surviving a restart.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    /// The head the drafts were written against.
    #[serde(default)]
    pub head_sha: String,
    #[serde(default)]
    pub comments: Vec<Comment>,
    /// Path to the content hash it was read at.
    #[serde(default)]
    pub reviewed: HashMap<String, u64>,
}

impl Session {
    /// The drafts that still apply at `head`.
    ///
    /// Empty when the head has moved: a comment anchored to line 12 of a diff
    /// that no longer exists would attach itself to unrelated code, which is
    /// worse than losing it. Reviewed marks are not filtered here — they carry
    /// their own per-file hash and simply read as unread when it stops matching.
    pub fn comments_at(&self, head: &str) -> &[Comment] {
        if self.head_sha == head {
            &self.comments
        } else {
            &[]
        }
    }
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
    /// A flattened key rather than a hash: debuggability beats brevity, and `/`
    /// is the only character needing escaping.
    pub fn path_for(&self, key: &SessionKey) -> PathBuf {
        self.root
            .join(format!("{}--{}.json", key.repo.replace('/', "-"), key.pr))
    }

    /// Read a session, or an empty one.
    ///
    /// Never fails: a missing file is the normal first-open case, and a corrupt
    /// one is not something the reviewer can fix mid-review. Losing drafts is
    /// bad; refusing to open the PR at all is worse.
    pub fn load(&self, key: &SessionKey) -> Session {
        std::fs::read_to_string(self.path_for(key))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Write a session atomically.
    ///
    /// Temp file then rename, so a crash mid-write leaves the previous session
    /// intact rather than a truncated file. The temp file must be a sibling —
    /// `rename` is only atomic within one filesystem.
    pub fn save(&self, key: &SessionKey, session: &Session) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let final_path = self.path_for(key);
        let tmp = final_path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(session)?)?;
        std::fs::rename(&tmp, &final_path)
    }
}

/// The default storage root.
///
/// Falls back to a temp dir when there is no home directory, so a sandboxed run
/// still works instead of erroring at startup.
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
    use diffident_model::comment::{Comment, Side};
    use std::collections::HashMap;

    fn key() -> SessionKey {
        SessionKey {
            repo: "owner/name".into(),
            pr: 42,
        }
    }

    fn session_with_a_draft(head: &str) -> Session {
        Session {
            head_sha: head.into(),
            comments: vec![Comment::new_line("src/main.rs", 12, Side::New, "nit")],
            reviewed: HashMap::from([("src/main.rs".to_string(), 999)]),
        }
    }

    #[test]
    fn loading_an_unknown_session_returns_an_empty_one_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Store::new(dir.path()).load(&key()).comments.is_empty());
    }

    #[test]
    fn a_saved_session_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.save(&key(), &session_with_a_draft("abc")).unwrap();

        let loaded = store.load(&key());
        assert_eq!(loaded.head_sha, "abc");
        assert_eq!(loaded.comments.len(), 1);
        assert_eq!(loaded.reviewed.get("src/main.rs"), Some(&999));
    }

    #[test]
    fn drafts_reattach_at_the_same_head() {
        assert_eq!(session_with_a_draft("abc").comments_at("abc").len(), 1);
    }

    #[test]
    fn drafts_do_not_reattach_at_a_new_head() {
        // Their line anchors point into a diff that no longer exists.
        assert!(session_with_a_draft("abc").comments_at("def").is_empty());
    }

    #[test]
    fn reviewed_marks_survive_a_new_head_and_are_judged_by_hash_instead() {
        // §7 asks for both a head-sensitive session and a per-file content
        // hash. Marks outliving the head is what gives the hash anything to do.
        let s = session_with_a_draft("abc");
        assert!(s.comments_at("def").is_empty());
        assert_eq!(s.reviewed.get("src/main.rs"), Some(&999));
    }

    #[test]
    fn a_corrupt_session_file_reads_as_empty_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
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
    fn distinct_prs_do_not_collide_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let other = SessionKey { pr: 43, ..key() };
        assert_ne!(store.path_for(&key()), store.path_for(&other));
    }

    #[test]
    fn the_same_pr_uses_one_file_regardless_of_head() {
        // The head lives inside the file, so a force-push updates it in place
        // rather than orphaning the old one on disk forever.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.save(&key(), &session_with_a_draft("abc")).unwrap();
        store.save(&key(), &session_with_a_draft("def")).unwrap();
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().filter_map(Result::ok).collect();
        assert_eq!(files.len(), 1, "one file per PR, not one per head");
    }
}
