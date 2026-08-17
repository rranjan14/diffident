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
