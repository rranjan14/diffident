use diffident_diff::{DiffFile, LineKind, Row, parser, rows};
use diffident_forge::gh::GhRunner;
use diffident_forge::threads::{ReviewThread, review_threads};
use diffident_forge::{Forge, Repo, gh::GhError, stack::stack_order};
use diffident_highlight::{Highlights, rows::for_rows};
use diffident_model::{LoadState, Review, ReviewId};

/// Everything one review needs to render, fetched and parsed.
pub struct LoadedReview {
    /// The commit the diff was fetched at. Identifies *which* diff this is, so
    /// a force-push between visits can be detected (§6, and Phase 5's session key).
    pub head_sha: String,
    pub title: String,
    pub files: Vec<DiffFile>,
    /// Index-parallel with what the diff list renders (§3).
    pub rows: Vec<Row>,
    /// Conversations already on the pull request (§7). Empty when nobody has
    /// reviewed it yet, which is not an error.
    pub threads: Vec<ReviewThread>,
    /// Index-parallel with `rows`. Computed here, on the caller's background
    /// thread, because it is by far the most expensive step — ~530ms on a
    /// 10k-row diff. Doing it in `DiffView::new` froze the window for that long.
    pub highlights: Vec<Highlights>,
    pub added: u32,
    pub removed: u32,
}

/// Fetch, parse and highlight one PR. Blocking — callers wrap it in the
/// background executor.
///
/// The two `gh` calls are independent and each costs most of a second, so they
/// run concurrently: `pr_detail` yields the head SHA, `pr_diff` the patch.
/// Highlighting happens here too rather than in the view, so that the whole
/// expensive path is off the foreground thread.
pub fn load_review<F: Forge + Sync, R: GhRunner + Sync>(
    forge: &F,
    runner: &R,
    repo: &Repo,
    number: u32,
) -> Result<LoadedReview, GhError> {
    // Three independent calls, each most of a second. `Sync` is bounded on this
    // function rather than on the traits: only this call site shares them
    // across threads.
    let (detail, text, threads) = std::thread::scope(|scope| {
        let diff = scope.spawn(|| forge.pr_diff(repo, number));
        let thr = scope.spawn(|| review_threads(runner, repo, number));
        let detail = forge.pr_detail(repo, number);
        (
            detail,
            diff.join().expect("pr_diff thread panicked"),
            thr.join().expect("threads thread panicked"),
        )
    });
    let (detail, text, threads) = (detail?, text?, threads?);
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

    let highlights = for_rows(&files, &rows);

    Ok(LoadedReview {
        head_sha: detail.head_ref_oid,
        title: detail.title,
        files,
        rows,
        highlights,
        added,
        removed,
        threads,
    })
}

/// List the repo's open PRs as rail-ready reviews, already in stack order.
pub fn list_reviews<F: Forge>(forge: &F, repo: &Repo) -> Result<Vec<Review>, GhError> {
    let prs = forge.list_prs(repo)?;
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
                head_sha: pr.head_ref_oid.clone(),
                rebased: false,
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
        "pr view 7 --repo o/r --json number,title,headRefName,baseRefName,headRefOid";
    const DETAIL_JSON: &str = r#"{"number":7,"title":"t","headRefName":"h","baseRefName":"main","headRefOid":"abc"}"#;
    const DIFF: &str =
        "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n";

    #[test]
    fn a_loaded_review_carries_its_parsed_files_rows_and_head_sha() {
        let gh = FakeGh::new()
            .with(DETAIL_ARGS, DETAIL_JSON)
            .with("pr diff 7 --repo o/r --color never", DIFF)
            .with("api graphql --input -", THREADS_JSON);
        let github = GitHub::new(gh);
        let loaded = load_review(&github, github.runner(), &repo(), 7).unwrap();
        assert_eq!(loaded.head_sha, "abc");
        assert_eq!(loaded.files.len(), 1);
        assert!(!loaded.rows.is_empty());
    }

    #[test]
    fn a_loaded_review_carries_highlights_parallel_to_its_rows() {
        // The view indexes highlights by row number and no longer computes
        // them, so a mismatch here paints every line the wrong colour.
        let gh = FakeGh::new()
            .with(DETAIL_ARGS, DETAIL_JSON)
            .with("pr diff 7 --repo o/r --color never", DIFF)
            .with("api graphql --input -", THREADS_JSON);
        let github = GitHub::new(gh);
        let loaded = load_review(&github, github.runner(), &repo(), 7).unwrap();
        assert_eq!(loaded.highlights.len(), loaded.rows.len());
    }

    #[test]
    fn all_three_gh_calls_are_issued_even_though_they_run_concurrently() {
        let gh = FakeGh::new()
            .with(DETAIL_ARGS, DETAIL_JSON)
            .with("pr diff 7 --repo o/r --color never", DIFF)
            .with("api graphql --input -", THREADS_JSON);
        let github = GitHub::new(gh);
        load_review(&github, github.runner(), &repo(), 7).unwrap();
        let mut calls = github.runner().calls();
        calls.sort();
        assert_eq!(
            calls.len(),
            3,
            "one detail call, one diff call, one threads call: {calls:?}"
        );
    }

    #[test]
    fn added_and_removed_counts_come_from_the_parsed_diff() {
        let gh = FakeGh::new()
            .with(DETAIL_ARGS, DETAIL_JSON)
            .with("pr diff 7 --repo o/r --color never", DIFF)
            .with("api graphql --input -", THREADS_JSON);
        let github = GitHub::new(gh);
        let loaded = load_review(&github, github.runner(), &repo(), 7).unwrap();
        assert_eq!((loaded.added, loaded.removed), (1, 1));
    }

    #[test]
    fn a_failed_fetch_surfaces_the_error_rather_than_an_empty_review() {
        let github = GitHub::new(FakeGh::new()); // nothing registered
        assert!(load_review(&github, github.runner(), &repo(), 7).is_err());
    }

    #[test]
    fn list_reviews_returns_prs_in_stack_order_with_their_depth() {
        let list_args = "pr list --repo o/r --state open --limit 100 --json number,title,headRefName,baseRefName,isDraft,url,isCrossRepository,headRefOid";
        let json = r#"[
            {"number":1,"title":"base","headRefName":"a","baseRefName":"main","isDraft":false,"url":"u","isCrossRepository":false,"headRefOid":"abc"},
            {"number":2,"title":"top","headRefName":"b","baseRefName":"a","isDraft":false,"url":"u","isCrossRepository":false,"headRefOid":"abc"}
        ]"#;
        let gh = FakeGh::new().with(list_args, json);
        let reviews = list_reviews(&GitHub::new(gh), &repo()).unwrap();
        assert_eq!(reviews.len(), 2);
        assert_eq!((reviews[0].id.number, reviews[0].depth), (1, 0));
        assert_eq!((reviews[1].id.number, reviews[1].depth), (2, 1));
        assert_eq!(reviews[0].id.repo, "o/r");
    }

    #[test]
    fn a_listed_review_carries_the_head_sha_so_a_force_push_can_be_noticed() {
        let list_args = "pr list --repo o/r --state open --limit 100 --json number,title,headRefName,baseRefName,isDraft,url,isCrossRepository,headRefOid";
        let json = r#"[{"number":1,"title":"t","headRefName":"a","baseRefName":"main","isDraft":false,"url":"u","isCrossRepository":false,"headRefOid":"deadbeef"}]"#;
        let gh = FakeGh::new().with(list_args, json);
        let reviews = list_reviews(&GitHub::new(gh), &repo()).unwrap();
        assert_eq!(reviews[0].head_sha, "deadbeef");
        assert!(!reviews[0].rebased, "a first listing is never a rebase");
    }

    const THREADS_JSON: &str = r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{
      "pageInfo":{"hasNextPage":false,"endCursor":null},
      "nodes":[{"id":"PRRT_1","isResolved":false,"isOutdated":false,"path":"a.rs",
        "line":2,"originalLine":2,"diffSide":"RIGHT",
        "comments":{"nodes":[{"id":"PRRC_1","author":{"login":"octocat"},"body":"nit"}]}}]
      }}}}}"#;

    #[test]
    fn a_loaded_review_carries_the_threads_already_on_the_pr() {
        let gh = FakeGh::new()
            .with(DETAIL_ARGS, DETAIL_JSON)
            .with("pr diff 7 --repo o/r --color never", DIFF)
            .with("api graphql --input -", THREADS_JSON);
        let github = GitHub::new(gh);
        let loaded = load_review(&github, github.runner(), &repo(), 7).unwrap();
        assert_eq!(loaded.threads.len(), 1);
        assert_eq!(loaded.threads[0].comments[0].author, "octocat");
    }

    #[test]
    fn a_pr_nobody_has_reviewed_yet_loads_with_no_threads() {
        let empty = r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{
          "pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}}"#;
        let gh = FakeGh::new()
            .with(DETAIL_ARGS, DETAIL_JSON)
            .with("pr diff 7 --repo o/r --color never", DIFF)
            .with("api graphql --input -", empty);
        let github = GitHub::new(gh);
        let loaded = load_review(&github, github.runner(), &repo(), 7).unwrap();
        assert!(loaded.threads.is_empty());
    }
}
