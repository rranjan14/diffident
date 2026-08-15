use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// The only way anything in diffident reaches GitHub.
///
/// One general-purpose method rather than one per endpoint: convenience
/// wrappers here would push GitHub knowledge down into the transport, and the
/// `Forge` impl is the right place for that knowledge.
///
/// Blocking by design — callers wrap it in an executor.
pub trait GhRunner: Send + Sync {
    /// `args` excludes the `gh` program name. `stdin` is piped when `Some`.
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<String, GhError>;
}

#[derive(Debug, thiserror::Error)]
pub enum GhError {
    #[error("`gh` was not found on PATH — install it from https://cli.github.com")]
    NotInstalled,
    #[error("GitHub CLI is not authenticated — run `gh auth login`")]
    NotAuthenticated,
    #[error("gh failed (exit {code}): {message}")]
    Failed { code: i32, message: String },
    #[error("gh returned output that could not be decoded: {0}")]
    BadOutput(String),
}

impl GhError {
    /// Build an error from a finished `gh` invocation.
    ///
    /// `gh api` writes the response body to **stdout** on non-2xx and puts only
    /// the status line on stderr, so stdout carries the actionable text. We
    /// prefer it and fall back to stderr when it is empty.
    pub fn from_exit(code: i32, stdout: &str, stderr: &str) -> Self {
        if stderr.contains("gh auth login") || stderr.contains("authentication") {
            return GhError::NotAuthenticated;
        }
        let message = if stdout.trim().is_empty() {
            stderr.trim()
        } else {
            stdout.trim()
        };
        GhError::Failed {
            code,
            message: message.to_string(),
        }
    }
}

/// Spawns the real `gh` binary.
pub struct Gh;

impl GhRunner for Gh {
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<String, GhError> {
        let mut child = Command::new("gh")
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => GhError::NotInstalled,
                _ => GhError::Failed {
                    code: -1,
                    message: e.to_string(),
                },
            })?;

        if let Some(payload) = stdin {
            child
                .stdin
                .as_mut()
                .expect("piped above")
                .write_all(payload.as_bytes())
                .map_err(|e| GhError::Failed {
                    code: -1,
                    message: e.to_string(),
                })?;
        }

        let out = child.wait_with_output().map_err(|e| GhError::Failed {
            code: -1,
            message: e.to_string(),
        })?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            return Ok(stdout);
        }
        Err(GhError::from_exit(
            out.status.code().unwrap_or(-1),
            &stdout,
            &String::from_utf8_lossy(&out.stderr),
        ))
    }
}

/// Test double. Matches on the space-joined argv.
pub struct FakeGh {
    responses: Vec<(String, String)>,
    calls: Mutex<Vec<String>>,
    stdins: Mutex<Vec<Option<String>>>,
}

impl FakeGh {
    pub fn new() -> Self {
        Self {
            responses: Vec::new(),
            calls: Mutex::new(Vec::new()),
            stdins: Mutex::new(Vec::new()),
        }
    }

    /// Register a canned response. `args` is the space-joined argv.
    pub fn with(mut self, args: &str, response: &str) -> Self {
        self.responses
            .push((args.to_string(), response.to_string()));
        self
    }

    /// Every argv seen so far, in order — lets a test assert the exact `gh`
    /// command a `Forge` impl built.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("not poisoned").clone()
    }

    pub fn stdins(&self) -> Vec<Option<String>> {
        self.stdins.lock().expect("not poisoned").clone()
    }
}

impl Default for FakeGh {
    fn default() -> Self {
        Self::new()
    }
}

impl GhRunner for FakeGh {
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<String, GhError> {
        let key = args.join(" ");
        self.calls.lock().expect("not poisoned").push(key.clone());
        self.stdins
            .lock()
            .expect("not poisoned")
            .push(stdin.map(str::to_string));
        self.responses
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
            .ok_or(GhError::Failed {
                code: 1,
                message: format!("FakeGh has no response registered for: {key}"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_returns_the_canned_response_for_matching_args() {
        let gh = FakeGh::new().with("pr list --repo o/r", r#"[{"number":1}]"#);
        assert_eq!(
            gh.run(&["pr", "list", "--repo", "o/r"], None).unwrap(),
            r#"[{"number":1}]"#
        );
    }

    #[test]
    fn fake_errors_on_an_unregistered_call() {
        let gh = FakeGh::new();
        assert!(gh.run(&["pr", "view", "1"], None).is_err());
    }

    #[test]
    fn fake_records_calls_in_order() {
        let gh = FakeGh::new().with("a", "1").with("b", "2");
        let _ = gh.run(&["a"], None);
        let _ = gh.run(&["b"], None);
        assert_eq!(gh.calls(), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn fake_captures_stdin() {
        let gh = FakeGh::new().with("api x", "{}");
        let _ = gh.run(&["api", "x"], Some("payload"));
        assert_eq!(gh.stdins(), vec![Some("payload".to_string())]);
    }

    #[test]
    fn missing_auth_is_its_own_error_not_a_generic_failure() {
        let err = GhError::from_exit(1, "", "gh: To get started with GitHub CLI, please run: gh auth login");
        assert!(matches!(err, GhError::NotAuthenticated));
    }

    #[test]
    fn error_message_prefers_stdout_because_gh_api_writes_bodies_there() {
        // `gh api` prints the JSON error body to stdout and only the status
        // line to stderr, so stdout is the useful half.
        let err = GhError::from_exit(1, r#"{"message":"Not Found"}"#, "HTTP 404");
        let text = err.to_string();
        assert!(text.contains("Not Found"), "got: {text}");
    }
}
