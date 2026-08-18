use crate::gh::{GhError, GhRunner};
use crate::{Forge, PrDetail, PrSummary, Repo};

/// The GitHub `Forge`. Owns argv construction and JSON decoding; knows nothing
/// about how the command is executed.
pub struct GitHub<R: GhRunner> {
    runner: R,
}

impl<R: GhRunner> GitHub<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    /// Exposed so tests can assert the exact argv that was built.
    pub fn runner(&self) -> &R {
        &self.runner
    }
}

/// The `--json` field set for list queries. Kept as a constant so the tests
/// and the call site cannot drift apart.
const LIST_FIELDS: &str = "number,title,headRefName,baseRefName,isDraft,url,isCrossRepository,headRefOid";
const DETAIL_FIELDS: &str = "number,title,headRefName,baseRefName,headRefOid";

fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, GhError> {
    serde_json::from_str(raw).map_err(|e| GhError::BadOutput(e.to_string()))
}

impl<R: GhRunner> Forge for GitHub<R> {
    fn current_repo(&self) -> Result<Repo, GhError> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            name_with_owner: String,
        }
        let raw = self
            .runner
            .run(&["repo", "view", "--json", "nameWithOwner"], None)?;
        let wire: Wire = decode(&raw)?;
        let (owner, name) = wire.name_with_owner.split_once('/').ok_or_else(|| {
            GhError::BadOutput(format!("expected owner/name, got {:?}", wire.name_with_owner))
        })?;
        Ok(Repo {
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }

    fn list_prs(&self, repo: &Repo) -> Result<Vec<PrSummary>, GhError> {
        let slug = repo.slug();
        let args = [
            "pr", "list", "--repo", &slug, "--state", "open", "--limit", "100", "--json",
            LIST_FIELDS,
        ];
        decode(&self.runner.run(&args, None)?)
    }

    fn pr_detail(&self, repo: &Repo, number: u32) -> Result<PrDetail, GhError> {
        let slug = repo.slug();
        let n = number.to_string();
        let args = ["pr", "view", &n, "--repo", &slug, "--json", DETAIL_FIELDS];
        decode(&self.runner.run(&args, None)?)
    }

    fn pr_diff(&self, repo: &Repo, number: u32) -> Result<String, GhError> {
        let slug = repo.slug();
        let n = number.to_string();
        // No `--patch`: it returns per-commit mbox patches and duplicates files.
        let args = ["pr", "diff", &n, "--repo", &slug, "--color", "never"];
        self.runner.run(&args, None)
    }

    fn create_review(&self, repo: &Repo, number: u32, payload: &str) -> Result<(), GhError> {
        let endpoint = format!("repos/{}/pulls/{number}/reviews", repo.slug());
        // The payload goes on stdin, never in argv: a review with many comments
        // is easily past the OS argument-length limit (§5).
        let args = ["api", &endpoint, "--method", "POST", "--input", "-"];
        self.runner.run(&args, Some(payload)).map(|_| ())
    }

    // The three thread operations delegate to `threads`, which owns the GraphQL
    // documents and their parsers. That split is the same one `pr_diff` makes:
    // this impl knows which call to make, the module below knows what the wire
    // looks like, and the tests for each live where the knowledge does.
    fn review_threads(
        &self,
        repo: &Repo,
        number: u32,
    ) -> Result<Vec<crate::threads::ReviewThread>, GhError> {
        crate::threads::review_threads(&self.runner, repo, number)
    }

    fn file_at(&self, repo: &Repo, path: &str, sha: &str) -> Result<String, GhError> {
        // §5 names this endpoint. The raw Accept header is what makes it return
        // the file rather than a JSON envelope with base64 inside it.
        let endpoint = format!("repos/{}/contents/{path}?ref={sha}", repo.slug());
        let args = [
            "api",
            &endpoint,
            "-H",
            "Accept: application/vnd.github.raw",
        ];
        self.runner.run(&args, None)
    }

    fn file_encoded_at(&self, repo: &Repo, path: &str, sha: &str) -> Result<String, GhError> {
        // No raw Accept header here, deliberately: the default envelope is
        // JSON with base64 inside, which survives a String round trip.
        let endpoint = format!("repos/{}/contents/{path}?ref={sha}", repo.slug());
        self.runner.run(&["api", &endpoint], None)
    }

    fn set_resolved(&self, thread_id: &str, resolved: bool) -> Result<(), GhError> {
        crate::threads::set_resolved(&self.runner, thread_id, resolved)
    }

    fn reply(
        &self,
        thread_id: &str,
        body: &str,
    ) -> Result<crate::threads::ThreadComment, GhError> {
        crate::threads::reply(&self.runner, thread_id, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh::FakeGh;

    fn repo() -> Repo {
        Repo {
            owner: "o".into(),
            name: "r".into(),
        }
    }

    const LIST_ARGS: &str = "pr list --repo o/r --state open --limit 100 --json number,title,headRefName,baseRefName,isDraft,url,isCrossRepository,headRefOid";

    #[test]
    fn list_prs_decodes_the_json_gh_returns() {
        let gh = FakeGh::new().with(
            LIST_ARGS,
            r#"[{"number":7,"title":"t","headRefName":"h","baseRefName":"main","isDraft":false,"url":"u","isCrossRepository":false,"headRefOid":"abc"}]"#,
        );
        let prs = GitHub::new(gh).list_prs(&repo()).unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 7);
        assert_eq!(prs[0].base_ref_name, "main");
    }


    #[test]
    fn pr_diff_never_passes_the_patch_flag() {
        // `gh pr diff --patch` returns per-commit mbox patches and duplicates
        // files (spec §5 gotcha 1).
        let gh = FakeGh::new().with("pr diff 7 --repo o/r --color never", "diff --git a/a b/a\n");
        let github = GitHub::new(gh);
        github.pr_diff(&repo(), 7).unwrap();
        let call = &github.runner().calls()[0];
        assert!(!call.contains("--patch"), "got: {call}");
    }

    #[test]
    fn pr_detail_requests_the_head_sha_because_it_keys_the_session() {
        let gh = FakeGh::new().with(
            "pr view 7 --repo o/r --json number,title,headRefName,baseRefName,headRefOid",
            r#"{"number":7,"title":"t","headRefName":"h","baseRefName":"main","headRefOid":"abc"}"#,
        );
        let detail = GitHub::new(gh).pr_detail(&repo(), 7).unwrap();
        assert_eq!(detail.head_ref_oid, "abc");
    }

    #[test]
    fn malformed_json_surfaces_as_bad_output_not_a_panic() {
        let gh = FakeGh::new().with(LIST_ARGS, "not json");
        let err = GitHub::new(gh)
            .list_prs(&repo())
            .unwrap_err();
        assert!(matches!(err, GhError::BadOutput(_)));
    }

    #[test]
    fn file_at_asks_for_raw_contents_at_a_commit() {
        // Without the raw Accept header this returns a JSON envelope with the
        // file base64-encoded inside it, which would parse as a file whose
        // every line is gibberish.
        let gh = FakeGh::new().with(
            "api repos/o/r/contents/src/a.rs?ref=abc -H Accept: application/vnd.github.raw",
            "line one\nline two\n",
        );
        let github = GitHub::new(gh);
        let text = github.file_at(&repo(), "src/a.rs", "abc").unwrap();
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn encoded_contents_deliberately_omit_the_raw_header() {
        // Raw bytes cannot travel through GhRunner, which returns a String;
        // lossy UTF-8 would corrupt an image beyond recognition.
        let gh = FakeGh::new().with(
            "api repos/o/r/contents/logo.png?ref=abc",
            r#"{"content":"aGk="}"#,
        );
        let github = GitHub::new(gh);
        github.file_encoded_at(&repo(), "logo.png", "abc").unwrap();
        assert!(
            !github.runner().calls()[0].contains("raw"),
            "the raw header would defeat the point"
        );
    }

    #[test]
    fn a_missing_file_surfaces_rather_than_reading_as_empty() {
        // An empty string would expand the gap to nothing and look like the
        // file genuinely had no lines there.
        let github = GitHub::new(FakeGh::new());
        assert!(github.file_at(&repo(), "gone.rs", "abc").is_err());
    }

    #[test]
    fn the_repo_is_resolved_from_the_checkout_you_are_standing_in() {
        // So `--repo` can be optional. gh reads the remote, follows forks to
        // their parent and honours GH_REPO; parsing `git remote` ourselves
        // would be more code and get those three cases wrong.
        let gh = FakeGh::new().with(
            "repo view --json nameWithOwner",
            r#"{"nameWithOwner":"rranjan14/diffident"}"#,
        );
        let repo = GitHub::new(gh).current_repo().unwrap();
        assert_eq!(repo.slug(), "rranjan14/diffident");
    }

    #[test]
    fn a_directory_that_is_not_a_checkout_surfaces_rather_than_guessing() {
        // Nothing registered -> the call fails, as gh does outside a repo.
        // Guessing a repo here would silently show someone else's PRs.
        assert!(GitHub::new(FakeGh::new()).current_repo().is_err());
    }

    #[test]
    fn a_name_without_a_slash_is_rejected_rather_than_becoming_a_bad_slug() {
        let gh = FakeGh::new().with("repo view --json nameWithOwner", r#"{"nameWithOwner":"oops"}"#);
        assert!(matches!(
            GitHub::new(gh).current_repo().unwrap_err(),
            GhError::BadOutput(_)
        ));
    }

    #[test]
    fn create_review_posts_to_the_reviews_endpoint() {
        let gh = FakeGh::new().with("api repos/o/r/pulls/7/reviews --method POST --input -", "{}");
        let github = GitHub::new(gh);
        github.create_review(&repo(), 7, r#"{"body":"hi"}"#).unwrap();
        assert_eq!(
            github.runner().calls(),
            vec!["api repos/o/r/pulls/7/reviews --method POST --input -".to_string()]
        );
    }

    #[test]
    fn the_review_payload_travels_on_stdin_not_in_argv() {
        // A review with many comments is easily past the OS argument limit (§5).
        let gh = FakeGh::new().with("api repos/o/r/pulls/7/reviews --method POST --input -", "{}");
        let github = GitHub::new(gh);
        github.create_review(&repo(), 7, r#"{"body":"hi"}"#).unwrap();
        assert_eq!(github.runner().stdins(), vec![Some(r#"{"body":"hi"}"#.to_string())]);
    }

    #[test]
    fn a_rejected_review_surfaces_the_error_rather_than_reporting_success() {
        let gh = FakeGh::new(); // nothing registered -> the call fails
        assert!(GitHub::new(gh).create_review(&repo(), 7, "{}").is_err());
    }
}
