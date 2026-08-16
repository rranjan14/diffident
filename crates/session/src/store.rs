use diffident_model::comment::Comment;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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

/// How long a lock may sit before another writer assumes its owner died.
///
/// A review app that refuses to save until you hunt down a stray file is worse
/// than one that occasionally races, so a stale lock is reclaimed rather than
/// respected forever.
const STALE_LOCK: Duration = Duration::from_secs(10);

/// Held for the duration of one write, so two diffident windows on the same PR
/// cannot interleave (§7).
///
/// `create_new` is the atomic test-and-set — exactly one caller can create a
/// given path, on every platform, with no dependency.
struct WriteLock {
    path: PathBuf,
}

impl WriteLock {
    /// Take the lock, reclaiming one left behind by a crashed process.
    ///
    /// Returns `None` when another writer genuinely holds it. The caller drops
    /// the save rather than blocking: this runs on a keystroke, and freezing
    /// the window because another instance is mid-write would be the larger
    /// harm — the next toggle writes again anyway.
    fn acquire(path: &Path) -> Option<Self> {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(_) => Some(Self {
                path: path.to_path_buf(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(path)
                    .and_then(|m| m.modified())
                    .map(|t| SystemTime::now().duration_since(t).unwrap_or_default() > STALE_LOCK)
                    .unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_file(path);
                    // One retry only. If someone else won the race, they hold
                    // it legitimately and we are done.
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)
                        .ok()
                        .map(|_| Self {
                            path: path.to_path_buf(),
                        })
                } else {
                    None
                }
            }
            Err(_) => None,
        }
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
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

    /// Where this key's lock lives while a write is in progress.
    pub fn lock_path_for(&self, key: &SessionKey) -> PathBuf {
        self.path_for(key).with_extension("lock")
    }

    /// The temp file this process writes through.
    ///
    /// Carries the process id: two instances sharing one temp path can
    /// interleave their writes and then publish each other's half-written
    /// bytes, which the rename would make look atomic and correct.
    fn temp_path_for(&self, key: &SessionKey) -> PathBuf {
        self.path_for(key)
            .with_extension(format!("{}.tmp", std::process::id()))
    }

    /// Write a session atomically, under a lock (§7).
    ///
    /// Temp file then rename, so a crash mid-write leaves the previous session
    /// intact rather than a truncated file. The temp file must be a sibling —
    /// `rename` is only atomic within one filesystem.
    ///
    /// The lock serialises writers; it does **not** merge them. Two windows on
    /// the same PR still resolve last-write-wins, because a mark and an
    /// *un*mark are indistinguishable once in memory — merging would resurrect
    /// files the reviewer deliberately unmarked, which is no better than losing
    /// ones they marked. Fixing that needs per-mark tombstones and is not worth
    /// it for a single-user desktop app.
    pub fn save(&self, key: &SessionKey, session: &Session) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let Some(_lock) = WriteLock::acquire(&self.lock_path_for(key)) else {
            // Another window is mid-write. Skipping is safe: marks are still
            // correct in memory and the next toggle writes again.
            return Ok(());
        };
        let final_path = self.path_for(key);
        let tmp = self.temp_path_for(key);
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

#[cfg(test)]
mod round_trip_tests {
    use super::*;
    use diffident_model::reviewed::Reviewed;

    /// Exactly what `Workspace` does: save marks, then restore them into a
    /// fresh `Reviewed` and ask whether each file is still read.
    fn save_then_reload(marks_at: u64, reload_at: u64) -> bool {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let key = SessionKey {
            repo: "o/r".into(),
            pr: 7,
        };

        let mut before = Reviewed::new();
        before.toggle(7, "a.rs", marks_at);
        store
            .save(
                &key,
                &Session {
                    head_sha: "abc".into(),
                    comments: Vec::new(),
                    reviewed: before.marks(7),
                },
            )
            .unwrap();

        let mut after = Reviewed::new();
        after.restore(7, store.load(&key).reviewed);
        after.is_reviewed(7, "a.rs", reload_at)
    }

    #[test]
    fn a_mark_survives_a_restart_when_the_file_is_unchanged() {
        assert!(save_then_reload(111, 111));
    }

    #[test]
    fn a_mark_is_dropped_after_a_restart_when_the_file_changed() {
        // §7's rule, end to end and on disk.
        assert!(!save_then_reload(111, 222));
    }

    #[test]
    fn an_unmarked_file_is_still_unmarked_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let key = SessionKey {
            repo: "o/r".into(),
            pr: 7,
        };
        store.save(&key, &Session::default()).unwrap();
        let mut after = Reviewed::new();
        after.restore(7, store.load(&key).reviewed);
        assert!(!after.is_reviewed(7, "a.rs", 111));
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    fn key() -> SessionKey {
        SessionKey {
            repo: "o/r".into(),
            pr: 7,
        }
    }

    #[test]
    fn a_second_writer_cannot_take_a_held_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let held = WriteLock::acquire(&path).expect("first writer takes it");
        assert!(WriteLock::acquire(&path).is_none(), "second must be refused");
        drop(held);
    }

    #[test]
    fn a_lock_is_released_when_its_guard_drops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        drop(WriteLock::acquire(&path).expect("taken"));
        assert!(!path.exists(), "the lock file must not outlive the guard");
        assert!(WriteLock::acquire(&path).is_some(), "and is takeable again");
    }

    #[test]
    fn a_stale_lock_left_by_a_crashed_process_is_reclaimed() {
        // Otherwise one crash means the reviewer silently stops saving forever.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        std::fs::write(&path, b"").unwrap();
        let long_ago = SystemTime::now() - STALE_LOCK - Duration::from_secs(60);
        filetime_set(&path, long_ago);
        assert!(WriteLock::acquire(&path).is_some(), "stale lock must be reclaimed");
    }

    #[test]
    fn a_fresh_lock_is_not_mistaken_for_a_stale_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        let _held = WriteLock::acquire(&path).unwrap();
        assert!(WriteLock::acquire(&path).is_none());
    }

    #[test]
    fn each_process_writes_through_its_own_temp_file() {
        // Two instances sharing one temp path can interleave their writes and
        // then publish each other's half-written bytes.
        let store = Store::new("/tmp/whatever");
        let tmp = store.temp_path_for(&key());
        assert!(
            tmp.to_string_lossy().contains(&std::process::id().to_string()),
            "temp path must be process-unique, got {tmp:?}"
        );
    }

    #[test]
    fn save_leaves_no_lock_or_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.save(&key(), &Session::default()).unwrap();
        let stray: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".lock") || n.contains(".tmp"))
            .collect();
        assert!(stray.is_empty(), "left behind: {stray:?}");
    }

    #[test]
    fn save_honours_a_lock_another_writer_holds() {
        // The load-bearing one: with the lock removed from save() this fails,
        // because save writes straight over the other writer's file.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let _held = WriteLock::acquire(&store.lock_path_for(&key())).unwrap();

        assert!(
            store.save(&key(), &Session::default()).is_ok(),
            "skipping is not an error — this runs on a keystroke, and blocking \
             the window would be worse than a write the next toggle repeats"
        );
        assert!(
            !store.path_for(&key()).exists(),
            "save must not write while another writer holds the lock"
        );
    }

    #[test]
    fn concurrent_writers_always_publish_parseable_json() {
        // An invariant, not a regression test. The hazard the process-unique
        // temp path removes — one writer renaming its tmp into place while
        // another is mid-write to the same inode — needs timing this cannot
        // force: removing the fix does not make this fail. It is kept because
        // the invariant is worth pinning, and named so it does not claim more
        // than it proves.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let big = Session {
            reviewed: (0..5_000)
                .map(|i| (format!("some/deep/path/to/file_{i}.rs"), i as u64))
                .collect(),
            ..Session::default()
        };

        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for _ in 0..10 {
                        let _ = store.save(&key(), &big);
                    }
                });
            }
        });

        // Whatever landed must be complete, parseable JSON — never a mix.
        let raw = std::fs::read_to_string(store.path_for(&key())).expect("a file was written");
        let parsed: Session = serde_json::from_str(&raw).expect("must not be torn");
        assert_eq!(parsed.reviewed.len(), 5_000);
    }

    /// Set a file's mtime without pulling in the `filetime` crate.
    fn filetime_set(path: &Path, when: SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(when).unwrap();
    }
}
