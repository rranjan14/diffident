//! Review threads already on the pull request (§5, §7).

use crate::gh::{GhError, GhRunner};
use crate::Repo;
use serde::Deserialize;

/// One conversation anchored to a line of the diff.
///
/// The anchor lives on the **thread**, not on its comments (§5). Asking
/// GraphQL for `path` or `line` on a `PullRequestReviewComment` is a schema
/// error, which is easy to reach because the REST API puts them there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    /// GraphQL node id. Needed to resolve or reply to this thread.
    pub id: String,
    pub path: String,
    /// Line in the current diff. `None` once the thread has gone outdated —
    /// the code it referred to is no longer in the diff at all.
    pub line: Option<u32>,
    /// Where the thread was originally left. Survives the code moving, so it
    /// is the only anchor an outdated thread still has.
    pub original_line: Option<u32>,
    /// `true` when the anchor is the old side of the diff.
    pub on_old_side: bool,
    pub is_resolved: bool,
    /// GitHub has decided the thread no longer applies to the current diff.
    pub is_outdated: bool,
    pub comments: Vec<ThreadComment>,
}

impl ReviewThread {
    /// The line to anchor against, preferring where the code is now.
    ///
    /// An outdated thread keeps only its original line, which will not match
    /// the current diff — callers should expect to fail to place it and say so
    /// rather than guessing at a nearby line.
    pub fn anchor_line(&self) -> Option<u32> {
        self.line.or(self.original_line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadComment {
    pub id: String,
    /// Empty when the account has since been deleted — GraphQL returns a null
    /// author rather than omitting the comment.
    pub author: String,
    pub body: String,
}

/// The GraphQL document. One page of threads, with their comments.
///
/// `first: 100` on both: GitHub caps page size at 100, and a thread with more
/// than 100 replies is not a thing that happens in review.
const QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id
          isResolved
          isOutdated
          path
          line
          originalLine
          diffSide
          comments(first: 100) {
            nodes { id author { login } body }
          }
        }
      }
    }
  }
}
"#;

// --- the shape GraphQL actually returns, kept private ---

#[derive(Deserialize)]
struct Envelope {
    data: Data,
}
#[derive(Deserialize)]
struct Data {
    repository: RepositoryNode,
}
#[derive(Deserialize)]
struct RepositoryNode {
    #[serde(rename = "pullRequest")]
    pull_request: PullRequestNode,
}
#[derive(Deserialize)]
struct PullRequestNode {
    #[serde(rename = "reviewThreads")]
    review_threads: ThreadPage,
}
#[derive(Deserialize)]
struct ThreadPage {
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
    nodes: Vec<ThreadNode>,
}
#[derive(Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}
#[derive(Deserialize)]
struct ThreadNode {
    id: String,
    #[serde(rename = "isResolved")]
    is_resolved: bool,
    #[serde(rename = "isOutdated")]
    is_outdated: bool,
    path: String,
    line: Option<u32>,
    #[serde(rename = "originalLine")]
    original_line: Option<u32>,
    #[serde(rename = "diffSide")]
    diff_side: String,
    comments: CommentPage,
}
#[derive(Deserialize)]
struct CommentPage {
    nodes: Vec<CommentNode>,
}
#[derive(Deserialize)]
struct CommentNode {
    id: String,
    author: Option<AuthorNode>,
    body: String,
}
#[derive(Deserialize)]
struct AuthorNode {
    login: String,
}

/// Turn one page of GraphQL into threads, and say whether more follow.
///
/// Separate from the fetching so the parsing is testable against a recorded
/// response with no network and no `gh`.
pub fn parse_page(raw: &str) -> Result<(Vec<ReviewThread>, Option<String>), GhError> {
    let envelope: Envelope =
        serde_json::from_str(raw).map_err(|e| GhError::BadOutput(e.to_string()))?;
    let page = envelope.data.repository.pull_request.review_threads;
    let threads = page
        .nodes
        .into_iter()
        .map(|n| ReviewThread {
            id: n.id,
            path: n.path,
            line: n.line,
            original_line: n.original_line,
            // GitHub says LEFT/RIGHT; everything above this layer thinks in
            // old/new, so translate once, here.
            on_old_side: n.diff_side == "LEFT",
            is_resolved: n.is_resolved,
            is_outdated: n.is_outdated,
            comments: n
                .comments
                .nodes
                .into_iter()
                .map(|c| ThreadComment {
                    id: c.id,
                    author: c.author.map(|a| a.login).unwrap_or_default(),
                    body: c.body,
                })
                .collect(),
        })
        .collect();
    let next = page
        .page_info
        .has_next_page
        .then_some(page.page_info.end_cursor)
        .flatten();
    Ok((threads, next))
}

/// The JSON body for one page request.
///
/// Query and variables both go on stdin: `gh api graphql --input -` accepts the
/// whole document that way, which keeps a multi-line query out of argv where it
/// would run into length limits (§5).
pub fn page_body(repo: &Repo, number: u32, after: Option<&str>) -> String {
    serde_json::json!({
        "query": QUERY,
        "variables": {
            "owner": repo.owner,
            "name": repo.name,
            "number": number,
            "after": after,
        }
    })
    .to_string()
}

/// Every review thread on a pull request, following pagination.
pub fn review_threads<R: GhRunner>(
    runner: &R,
    repo: &Repo,
    number: u32,
) -> Result<Vec<ReviewThread>, GhError> {
    let mut all = Vec::new();
    let mut after: Option<String> = None;
    // A hard stop, so a server that always claims another page cannot hang the
    // fetch forever. 100 pages is 10,000 threads.
    for _ in 0..100 {
        let body = page_body(repo, number, after.as_deref());
        let raw = runner.run(&["api", "graphql", "--input", "-"], Some(&body))?;
        let (mut page, next) = parse_page(&raw)?;
        all.append(&mut page);
        match next {
            Some(cursor) => after = Some(cursor),
            None => return Ok(all),
        }
    }
    Ok(all)
}

/// Mark a thread resolved. The thread's node id is the whole address — no repo
/// or PR number is needed, because a GraphQL global id already carries them.
const RESOLVE: &str = r#"
mutation($id: ID!) {
  resolveReviewThread(input: {threadId: $id}) {
    thread { id isResolved }
  }
}
"#;

const UNRESOLVE: &str = r#"
mutation($id: ID!) {
  unresolveReviewThread(input: {threadId: $id}) {
    thread { id isResolved }
  }
}
"#;

#[derive(Deserialize)]
struct ResolutionEnvelope {
    data: ResolutionData,
}
/// One struct for both mutations. They return the same shape under different
/// keys, and `alias` is what stops that from becoming two structs that
/// gradually stop agreeing.
#[derive(Deserialize)]
struct ResolutionData {
    #[serde(rename = "resolveReviewThread", alias = "unresolveReviewThread")]
    payload: ResolutionPayload,
}
#[derive(Deserialize)]
struct ResolutionPayload {
    thread: ResolvedThread,
}
#[derive(Deserialize)]
struct ResolvedThread {
    #[serde(rename = "isResolved")]
    is_resolved: bool,
}

/// The JSON body for one resolve/unresolve. Stdin, like every other payload (§5).
pub fn resolution_body(thread_id: &str, resolved: bool) -> String {
    serde_json::json!({
        "query": if resolved { RESOLVE } else { UNRESOLVE },
        "variables": { "id": thread_id },
    })
    .to_string()
}

/// The thread's resolved state, as GitHub reports it back.
pub fn parse_resolution(raw: &str) -> Result<bool, GhError> {
    let env: ResolutionEnvelope =
        serde_json::from_str(raw).map_err(|e| GhError::BadOutput(e.to_string()))?;
    Ok(env.data.payload.thread.is_resolved)
}

/// Resolve or unresolve a thread.
///
/// A GraphQL error already arrives as `GhError::Failed` — `gh api graphql`
/// exits non-zero whenever the response carries an `errors` array, even with
/// `data` partly populated. The check below is for the other failure: a 200
/// that reports a state we did not ask for. Reporting success there would leave
/// the reviewer's pane disagreeing with GitHub, and nothing would ever correct
/// it until the next full reload.
pub fn set_resolved<R: GhRunner>(
    runner: &R,
    thread_id: &str,
    resolved: bool,
) -> Result<(), GhError> {
    let raw = runner.run(
        &["api", "graphql", "--input", "-"],
        Some(&resolution_body(thread_id, resolved)),
    )?;
    let now = parse_resolution(&raw)?;
    if now != resolved {
        return Err(GhError::BadOutput(format!(
            "asked GitHub to set resolved={resolved}, it reports {now}"
        )));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh::FakeGh;

    fn repo() -> Repo {
        Repo {
            owner: "cli".into(),
            name: "cli".into(),
        }
    }

    /// A trimmed copy of a real response from `cli/cli` PR 14160.
    const ONE_PAGE: &str = r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{
      "pageInfo":{"hasNextPage":false,"endCursor":"abc"},
      "nodes":[
        {"id":"PRRT_1","isResolved":false,"isOutdated":false,"path":"pkg/a.go",
         "line":341,"originalLine":341,"diffSide":"RIGHT",
         "comments":{"nodes":[{"id":"PRRC_1","author":{"login":"octocat"},"body":"nit"}]}},
        {"id":"PRRT_2","isResolved":true,"isOutdated":true,"path":"pkg/b.go",
         "line":null,"originalLine":12,"diffSide":"LEFT",
         "comments":{"nodes":[{"id":"PRRC_2","author":null,"body":"from a deleted account"}]}}
      ]}}}}}"#;

    #[test]
    fn a_thread_carries_its_anchor_and_its_comments() {
        let (threads, _) = parse_page(ONE_PAGE).unwrap();
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].path, "pkg/a.go");
        assert_eq!(threads[0].line, Some(341));
        assert_eq!(threads[0].comments[0].author, "octocat");
        assert_eq!(threads[0].comments[0].body, "nit");
    }

    #[test]
    fn github_sides_are_translated_to_old_and_new() {
        // Everything above this layer thinks in old/new; translating once here
        // keeps LEFT and RIGHT from leaking through the whole app.
        let (threads, _) = parse_page(ONE_PAGE).unwrap();
        assert!(!threads[0].on_old_side, "RIGHT is the new side");
        assert!(threads[1].on_old_side, "LEFT is the old side");
    }

    #[test]
    fn an_outdated_thread_falls_back_to_the_line_it_was_left_on() {
        let (threads, _) = parse_page(ONE_PAGE).unwrap();
        assert_eq!(threads[1].line, None, "no longer in the diff");
        assert_eq!(threads[1].anchor_line(), Some(12), "but it remembers where");
        assert!(threads[1].is_outdated);
    }

    #[test]
    fn a_comment_from_a_deleted_account_still_shows_its_body() {
        // GraphQL returns a null author rather than dropping the comment.
        let (threads, _) = parse_page(ONE_PAGE).unwrap();
        assert_eq!(threads[1].comments[0].author, "");
        assert_eq!(threads[1].comments[0].body, "from a deleted account");
    }

    #[test]
    fn a_single_page_reports_no_cursor_to_follow() {
        let (_, next) = parse_page(ONE_PAGE).unwrap();
        assert_eq!(next, None);
    }

    #[test]
    fn malformed_json_surfaces_as_bad_output_not_a_panic() {
        assert!(matches!(parse_page("not json"), Err(GhError::BadOutput(_))));
    }

    #[test]
    fn the_query_and_its_variables_travel_on_stdin() {
        // A multi-line GraphQL document in argv runs into length limits (§5).
        let body = page_body(&repo(), 14160, None);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(parsed["query"].as_str().unwrap().contains("reviewThreads"));
        assert_eq!(parsed["variables"]["owner"], "cli");
        assert_eq!(parsed["variables"]["number"], 14160);
        assert!(parsed["variables"]["after"].is_null(), "first page has no cursor");
    }

    #[test]
    fn a_later_page_asks_for_everything_after_the_cursor() {
        let body = page_body(&repo(), 1, Some("CURSOR"));
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["variables"]["after"], "CURSOR");
    }

    #[test]
    fn every_page_is_followed_until_the_cursor_runs_out() {
        let gh = FakeGh::new().with("api graphql --input -", ONE_PAGE);
        let threads = review_threads(&gh, &repo(), 1).unwrap();
        assert_eq!(threads.len(), 2, "one page, both threads");
    }

    #[test]
    fn a_failed_request_surfaces_rather_than_returning_no_threads() {
        // Reporting "no threads" for a network failure would tell the reviewer
        // the PR is clean when nobody knows.
        let gh = FakeGh::new();
        assert!(review_threads(&gh, &repo(), 1).is_err());
    }

    const RESOLVED_OK: &str =
        r#"{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":true}}}}"#;
    const UNRESOLVED_OK: &str =
        r#"{"data":{"unresolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":false}}}}"#;

    #[test]
    fn resolving_and_unresolving_share_one_parser() {
        // The two mutations differ only in their payload key, so one aliased
        // struct reads both rather than two near-identical ones drifting apart.
        assert!(parse_resolution(RESOLVED_OK).unwrap());
        assert!(!parse_resolution(UNRESOLVED_OK).unwrap());
    }

    #[test]
    fn each_direction_sends_its_own_mutation() {
        let r: serde_json::Value = serde_json::from_str(&resolution_body("T", true)).unwrap();
        let q = r["query"].as_str().unwrap();
        assert!(q.contains("resolveReviewThread"));
        assert!(!q.contains("unresolveReviewThread"), "must not send both");
        let u: serde_json::Value = serde_json::from_str(&resolution_body("T", false)).unwrap();
        assert!(u["query"].as_str().unwrap().contains("unresolveReviewThread"));
        assert_eq!(u["variables"]["id"], "T");
    }

    #[test]
    fn the_mutation_travels_on_stdin_like_every_other_payload() {
        // §5 gotcha 3: a multi-line GraphQL document in argv hits length limits.
        let gh = FakeGh::new().with("api graphql --input -", RESOLVED_OK);
        set_resolved(&gh, "PRRT_1", true).unwrap();
        assert_eq!(gh.calls(), vec!["api graphql --input -".to_string()]);
        assert!(gh.stdins()[0].as_ref().unwrap().contains("PRRT_1"));
    }

    #[test]
    fn a_server_that_disagrees_is_an_error_not_a_silent_success() {
        // If the UI marks a thread resolved on the strength of a call that did
        // not resolve it, the reviewer moves on from a conversation that is
        // still open on GitHub. Comparing the returned state costs one `if`.
        let gh = FakeGh::new().with("api graphql --input -", UNRESOLVED_OK);
        assert!(set_resolved(&gh, "PRRT_1", true).is_err());
    }

    #[test]
    fn a_failed_mutation_surfaces_rather_than_reporting_success() {
        let gh = FakeGh::new(); // nothing registered
        assert!(set_resolved(&gh, "PRRT_1", true).is_err());
    }

    #[test]
    fn a_malformed_response_is_bad_output_not_a_panic() {
        assert!(matches!(parse_resolution("not json"), Err(GhError::BadOutput(_))));
    }
}
