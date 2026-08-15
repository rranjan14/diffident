use diffident_diff::{DiffFile, LineKind, Row, parser, rows};
use diffident_forge::{Forge, PrFilter, Repo, gh::GhError, stack::stack_order};
use diffident_model::{LoadState, Review, ReviewId};

/// Tags an in-flight fetch so a late result can be discarded (§5).
///
/// The head SHA is part of the identity, not just the repo and number: a force
/// push mid-fetch produces a diff that no longer describes the PR, and rendering
/// it would silently show the reviewer the wrong code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId {
    /// `owner/name`.
    pub repo: String,
    pub number: u32,
    pub head_sha: String,
}

impl RequestId {
    /// Whether a result tagged with `self` should be thrown away given what is
    /// selected now. `None` means nothing is selected, so there is nothing to
    /// render into.
    pub fn is_stale_for(&self, current: Option<&RequestId>) -> bool {
        current != Some(self)
    }
}

/// Everything one review needs to render, fetched and parsed.
pub struct LoadedReview {
    pub request: RequestId,
    pub title: String,
    pub files: Vec<DiffFile>,
    /// Index-parallel with what the diff list renders (§3).
    pub rows: Vec<Row>,
    pub added: u32,
    pub removed: u32,
}

/// Fetch and parse one PR. Blocking — callers wrap it in the background executor.
///
/// Two `gh` calls, deliberately: the detail call is what yields the head SHA
/// that keys the session, and it is cheap next to the diff.
pub fn load_review<F: Forge>(forge: &F, repo: &Repo, number: u32) -> Result<LoadedReview, GhError> {
    let detail = forge.pr_detail(repo, number)?;
    let text = forge.pr_diff(repo, number)?;
    let files = parser::parse(&text);
    let rows = rows::build_rows(&files);

    let (mut added, mut removed) = (0, 0);
    for file in &files {
        for hunk in &file.hunks {
            for line in &hunk.lines {
                match line.kind {
                    LineKind::Added => added += 1,
                    LineKind::Removed => removed += 1,
                    LineKind::Context => {}
                }
            }
        }
    }

    Ok(LoadedReview {
        request: RequestId {
            repo: repo.slug(),
            number,
            head_sha: detail.head_ref_oid,
        },
        title: detail.title,
        files,
        rows,
        added,
        removed,
    })
}

/// List the repo's open PRs as rail-ready reviews, already in stack order.
pub fn list_reviews<F: Forge>(
    forge: &F,
    repo: &Repo,
    filter: PrFilter,
) -> Result<Vec<Review>, GhError> {
    let prs = forge.list_prs(repo, filter)?;
    let slug = repo.slug();
    Ok(stack_order(&prs)
        .into_iter()
        .filter_map(|entry| {
            let pr = prs.iter().find(|p| p.number == entry.number)?;
            Some(Review {
                id: ReviewId {
                    repo: slug.clone(),
                    number: pr.number,
                },
                title: pr.title.clone(),
                branch: pr.head_ref_name.clone(),
                depth: entry.depth,
                is_draft: pr.is_draft,
                state: LoadState::Idle,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use diffident_forge::gh::FakeGh;
    use diffident_forge::github::GitHub;

    fn repo() -> Repo {
        Repo {
            owner: "o".into(),
            name: "r".into(),
        }
    }

    const DETAIL_ARGS: &str =
        "pr view 7 --repo o/r --json number,title,body,headRefName,baseRefName,headRefOid,baseRefOid";
    const DETAIL_JSON: &str = r#"{"number":7,"title":"t","body":"b","headRefName":"h","baseRefName":"main","headRefOid":"abc","baseRefOid":"def"}"#;
    const DIFF: &str =
        "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n";

    #[test]
    fn a_loaded_review_carries_its_parsed_files_rows_and_head_sha() {
        let gh = FakeGh::new()
            .with(DETAIL_ARGS, DETAIL_JSON)
            .with("pr diff 7 --repo o/r --color never", DIFF);
        let loaded = load_review(&GitHub::new(gh), &repo(), 7).unwrap();
        assert_eq!(loaded.request.head_sha, "abc");
        assert_eq!(loaded.files.len(), 1);
        assert!(!loaded.rows.is_empty());
    }

    #[test]
    fn added_and_removed_counts_come_from_the_parsed_diff() {
        let gh = FakeGh::new()
            .with(DETAIL_ARGS, DETAIL_JSON)
            .with("pr diff 7 --repo o/r --color never", DIFF);
        let loaded = load_review(&GitHub::new(gh), &repo(), 7).unwrap();
        assert_eq!((loaded.added, loaded.removed), (1, 1));
    }

    #[test]
    fn a_failed_fetch_surfaces_the_error_rather_than_an_empty_review() {
        let gh = FakeGh::new(); // nothing registered
        assert!(load_review(&GitHub::new(gh), &repo(), 7).is_err());
    }

    #[test]
    fn a_result_for_the_current_request_is_not_stale() {
        let id = RequestId {
            repo: "o/r".into(),
            number: 7,
            head_sha: "abc".into(),
        };
        assert!(!id.is_stale_for(Some(&id)));
    }

    #[test]
    fn a_result_whose_head_moved_is_stale() {
        // The reviewer force-pushed while the diff was in flight. Rendering it
        // would show a diff that no longer matches the PR.
        let sent = RequestId {
            repo: "o/r".into(),
            number: 7,
            head_sha: "abc".into(),
        };
        let now = RequestId {
            head_sha: "def".into(),
            ..sent.clone()
        };
        assert!(sent.is_stale_for(Some(&now)));
    }

    #[test]
    fn a_result_for_a_review_the_user_switched_away_from_is_stale() {
        let sent = RequestId {
            repo: "o/r".into(),
            number: 7,
            head_sha: "abc".into(),
        };
        let now = RequestId {
            number: 9,
            ..sent.clone()
        };
        assert!(sent.is_stale_for(Some(&now)));
        assert!(
            sent.is_stale_for(None),
            "nothing selected means nothing to render into"
        );
    }

    #[test]
    fn list_reviews_returns_prs_in_stack_order_with_their_depth() {
        let list_args = "pr list --repo o/r --state open --limit 100 --json number,title,headRefName,baseRefName,isDraft,url,isCrossRepository";
        let json = r#"[
            {"number":1,"title":"base","headRefName":"a","baseRefName":"main","isDraft":false,"url":"u","isCrossRepository":false},
            {"number":2,"title":"top","headRefName":"b","baseRefName":"a","isDraft":false,"url":"u","isCrossRepository":false}
        ]"#;
        let gh = FakeGh::new().with(list_args, json);
        let reviews = list_reviews(&GitHub::new(gh), &repo(), PrFilter::AllOpen).unwrap();
        assert_eq!(reviews.len(), 2);
        assert_eq!((reviews[0].id.number, reviews[0].depth), (1, 0));
        assert_eq!((reviews[1].id.number, reviews[1].depth), (2, 1));
        assert_eq!(reviews[0].id.repo, "o/r");
    }
}
