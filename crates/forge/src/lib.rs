pub mod gh;
pub mod github;
pub mod stack;
pub mod threads;

use serde::Deserialize;

/// A GitHub repository coordinate. Always `owner/name`; host lives in `gh`'s
/// own config, not here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Repo {
    pub owner: String,
    pub name: String,
}

impl Repo {
    /// The `owner/name` form every `gh --repo` flag expects.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// The cheap PR shape returned by list queries — enough to render the rail
/// and to compute stacks, without a per-PR round trip.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrSummary {
    pub number: u32,
    pub title: String,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub is_draft: bool,
    pub url: String,
    /// True when the head branch lives in a fork rather than in this repo.
    ///
    /// Load-bearing for stack detection: branch names are unique only *within*
    /// a repo, so `head_ref_name` alone cannot identify a PR. A fork PR whose
    /// head branch is `main` would otherwise look like the parent of every
    /// PR targeting `main`.
    pub is_cross_repository: bool,
    /// Head commit SHA, as of this listing.
    ///
    /// Free on `pr list` — measured at no change to a 100-PR call — and the
    /// only way to notice a force-push without re-fetching every diff (§6).
    pub head_ref_oid: String,
}

/// The full PR shape, fetched only when a review is opened.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrDetail {
    pub number: u32,
    pub title: String,
    pub head_ref_name: String,
    pub base_ref_name: String,
    /// Head commit SHA. Half of the session key — when this moves, drafts
    /// belong to a new session (spec §7).
    pub head_ref_oid: String,
}

/// The one seam between diffident and a code host.
///
/// **Every method must stay non-generic.** `dyn Forge` is what lets the UI hold
/// one injected handle instead of hard-coding `GitHub<Gh>` at each call site,
/// and a generic method would make the trait object-unsafe. The thread
/// operations below were free functions taking a `GhRunner` for two phases,
/// which forced callers to carry two handles to the same host — that is the
/// mistake this rule exists to prevent recurring.
pub trait Forge {
    fn list_prs(&self, repo: &Repo) -> Result<Vec<PrSummary>, gh::GhError>;
    fn pr_detail(&self, repo: &Repo, number: u32) -> Result<PrDetail, gh::GhError>;
    /// Raw unified diff text. Intentionally a `String`: this layer knows
    /// nothing about patch syntax, and `diff::parser` knows nothing about GitHub.
    fn pr_diff(&self, repo: &Repo, number: u32) -> Result<String, gh::GhError>;
    /// Post a review. `payload` is the create-review JSON, built by the caller.
    ///
    /// Takes serialised JSON rather than a typed struct: the shape belongs to
    /// whoever knows about comments, and this layer deliberately knows nothing
    /// about them — the same reason `pr_diff` returns a `String`.
    fn create_review(&self, repo: &Repo, number: u32, payload: &str) -> Result<(), gh::GhError>;

    /// Every review thread on a pull request, following pagination.
    fn review_threads(
        &self,
        repo: &Repo,
        number: u32,
    ) -> Result<Vec<threads::ReviewThread>, gh::GhError>;

    /// Resolve or unresolve a thread. The thread's node id is the whole
    /// address — a GraphQL global id already carries repo and PR.
    fn set_resolved(&self, thread_id: &str, resolved: bool) -> Result<(), gh::GhError>;

    /// Post a reply, returning the comment the host created so the caller can
    /// render it without a second fetch.
    fn reply(
        &self,
        thread_id: &str,
        body: &str,
    ) -> Result<threads::ThreadComment, gh::GhError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_stays_object_safe_so_the_ui_can_hold_one_injected_handle() {
        // This is a compile-time assertion wearing a test's clothes: if anyone
        // adds a generic method to `Forge`, the coercion below stops compiling
        // and this test is where they find out why. Without `dyn Forge` the UI
        // has to name a concrete `GitHub<Gh>` at every call site, which is what
        // made `Workspace` untestable in the first place.
        fn takes_a_dyn_forge(_: &(dyn Forge + Send + Sync)) {}
        let forge = github::GitHub::new(gh::FakeGh::new());
        takes_a_dyn_forge(&forge);
    }
}
