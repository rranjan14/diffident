pub mod gh;
pub mod github;
pub mod stack;

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
/// Deliberately narrow: three methods covering the whole read path. Submit,
/// threads and commits arrive in later plans — do not add stubs for them now.
pub trait Forge {
    fn list_prs(&self, repo: &Repo) -> Result<Vec<PrSummary>, gh::GhError>;
    fn pr_detail(&self, repo: &Repo, number: u32) -> Result<PrDetail, gh::GhError>;
    /// Raw unified diff text. Intentionally a `String`: this layer knows
    /// nothing about patch syntax, and `diff::parser` knows nothing about GitHub.
    fn pr_diff(&self, repo: &Repo, number: u32) -> Result<String, gh::GhError>;
}
