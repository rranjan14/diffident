//! Orchestrator: parse args, open the window, wire the views. Nothing else.

use diffident_forge::{Repo, gh::Gh, github::GitHub};
use diffident_session::config;
use diffident_ui::{navigate, workspace::Workspace};
use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

const USAGE: &str = "usage: diffident --repo <owner/name> [--pr <number>]";

#[derive(Debug)]
pub struct Args {
    pub repo: Repo,
    /// Open this PR immediately instead of waiting for a rail click.
    pub pr: Option<u32>,
}

impl Args {
    /// Parse an argument list that has already had argv[0] removed.
    pub fn from(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let (mut repo, mut pr) = (None, None);
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--repo" => repo = args.next(),
                "--pr" => {
                    let raw = args.next().ok_or_else(|| format!("--pr needs a number\n{USAGE}"))?;
                    pr = Some(
                        raw.parse()
                            .map_err(|_| format!("--pr must be a number, got {raw:?}\n{USAGE}"))?,
                    );
                }
                other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
            }
        }
        let slug = repo.ok_or_else(|| format!("--repo is required\n{USAGE}"))?;
        let (owner, name) = slug
            .split_once('/')
            .ok_or_else(|| format!("--repo must be owner/name, got {slug:?}\n{USAGE}"))?;
        Ok(Args {
            repo: Repo {
                owner: owner.to_string(),
                name: name.to_string(),
            },
            pr,
        })
    }
}

fn main() {
    let args = match Args::from(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    // Read before the window opens, so the first frame is already the theme
    // and width the user asked for rather than a default that flips.
    let (config, config_error) = config::load(&config::default_path());

    application().run(move |cx: &mut App| {
        cx.bind_keys(navigate::key_bindings());
        let bounds = Bounds::centered(None, size(px(1400.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    Workspace::new(
                        std::sync::Arc::new(GitHub::new(Gh)),
                        args.repo.clone(),
                        args.pr,
                        config.clone(),
                        config_error.clone(),
                        window,
                        cx,
                    )
                })
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::from(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn repo_is_split_into_owner_and_name() {
        let a = parse(&["--repo", "cli/cli"]).unwrap();
        assert_eq!((a.repo.owner.as_str(), a.repo.name.as_str()), ("cli", "cli"));
        assert_eq!(a.pr, None);
    }

    #[test]
    fn a_pr_number_is_optional_and_parsed() {
        assert_eq!(parse(&["--repo", "o/r", "--pr", "42"]).unwrap().pr, Some(42));
    }

    #[test]
    fn a_missing_repo_is_an_error_with_usage_rather_than_a_panic() {
        let err = parse(&[]).unwrap_err();
        assert!(err.contains("--repo"), "got: {err}");
    }

    #[test]
    fn a_repo_without_a_slash_is_rejected() {
        assert!(parse(&["--repo", "cli"]).is_err());
    }

    #[test]
    fn a_non_numeric_pr_is_rejected() {
        assert!(parse(&["--repo", "o/r", "--pr", "abc"]).is_err());
    }
}
