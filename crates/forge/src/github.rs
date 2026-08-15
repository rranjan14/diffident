use crate::gh::{GhError, GhRunner};
use crate::{Forge, PrDetail, PrFilter, PrSummary, Repo};

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
const LIST_FIELDS: &str = "number,title,headRefName,baseRefName,isDraft,url,isCrossRepository";
const DETAIL_FIELDS: &str = "number,title,body,headRefName,baseRefName,headRefOid,baseRefOid";

fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, GhError> {
    serde_json::from_str(raw).map_err(|e| GhError::BadOutput(e.to_string()))
}

impl<R: GhRunner> Forge for GitHub<R> {
    fn list_prs(&self, repo: &Repo, filter: PrFilter) -> Result<Vec<PrSummary>, GhError> {
        let slug = repo.slug();
        let mut args = vec![
            "pr", "list", "--repo", &slug, "--state", "open", "--limit", "100", "--json",
            LIST_FIELDS,
        ];
        if matches!(filter, PrFilter::ReviewRequested) {
            args.extend_from_slice(&["--search", "review-requested:@me"]);
        }
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

    const LIST_ARGS: &str = "pr list --repo o/r --state open --limit 100 --json number,title,headRefName,baseRefName,isDraft,url,isCrossRepository";

    #[test]
    fn list_prs_decodes_the_json_gh_returns() {
        let gh = FakeGh::new().with(
            LIST_ARGS,
            r#"[{"number":7,"title":"t","headRefName":"h","baseRefName":"main","isDraft":false,"url":"u","isCrossRepository":false}]"#,
        );
        let prs = GitHub::new(gh).list_prs(&repo(), PrFilter::AllOpen).unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 7);
        assert_eq!(prs[0].base_ref_name, "main");
    }

    #[test]
    fn review_requested_filter_appends_a_search_flag() {
        let args = format!("{LIST_ARGS} --search review-requested:@me");
        let gh = FakeGh::new().with(&args, "[]");
        let github = GitHub::new(gh);
        github
            .list_prs(&repo(), PrFilter::ReviewRequested)
            .unwrap();
        assert_eq!(github.runner().calls(), vec![args]);
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
            "pr view 7 --repo o/r --json number,title,body,headRefName,baseRefName,headRefOid,baseRefOid",
            r#"{"number":7,"title":"t","body":"b","headRefName":"h","baseRefName":"main","headRefOid":"abc","baseRefOid":"def"}"#,
        );
        let detail = GitHub::new(gh).pr_detail(&repo(), 7).unwrap();
        assert_eq!(detail.head_ref_oid, "abc");
    }

    #[test]
    fn malformed_json_surfaces_as_bad_output_not_a_panic() {
        let gh = FakeGh::new().with(LIST_ARGS, "not json");
        let err = GitHub::new(gh)
            .list_prs(&repo(), PrFilter::AllOpen)
            .unwrap_err();
        assert!(matches!(err, GhError::BadOutput(_)));
    }
}
