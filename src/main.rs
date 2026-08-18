//! Orchestrator: parse args, open the window, wire the views. Nothing else.

use diffident_forge::{Forge as _, Repo, gh::Gh, github::GitHub};
use diffident_session::config;
use diffident_ui::{navigate, workspace::Workspace};
use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

const USAGE: &str = "usage: diffident [--repo <owner/name>] [--pr <number>]";

#[derive(Debug)]
pub struct Args {
    /// `None` means "whatever repo the working directory is in", resolved at
    /// startup. Optional because requiring it means the repo you review is
    /// whatever was last on the command line, not the one you are working in.
    pub repo: Option<Repo>,
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
        let repo = match repo {
            None => None,
            Some(slug) => {
                let (owner, name) = slug
                    .split_once('/')
                    .ok_or_else(|| format!("--repo must be owner/name, got {slug:?}\n{USAGE}"))?;
                Some(Repo {
                    owner: owner.to_string(),
                    name: name.to_string(),
                })
            }
        };
        Ok(Args { repo, pr })
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

    let forge = std::sync::Arc::new(GitHub::new(Gh));
    // Resolved before the window opens: failing here should print why and exit,
    // not open a window onto an empty rail that gives no clue what went wrong.
    let repo = match args.repo.clone() {
        Some(repo) => repo,
        None => match forge.current_repo() {
            Ok(repo) => repo,
            Err(e) => {
                eprintln!("could not tell which repository this is: {e}\n{USAGE}");
                std::process::exit(2);
            }
        },
    };

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
                        forge.clone(),
                        repo.clone(),
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
        let repo = a.repo.unwrap();
        assert_eq!((repo.owner.as_str(), repo.name.as_str()), ("cli", "cli"));
        assert_eq!(a.pr, None);
    }

    #[test]
    fn a_pr_number_is_optional_and_parsed() {
        assert_eq!(parse(&["--repo", "o/r", "--pr", "42"]).unwrap().pr, Some(42));
    }

    #[test]
    fn no_repo_flag_means_ask_the_checkout_rather_than_fail() {
        // The common case: run it in the repo you are working in. Requiring
        // the flag is how you end up reviewing whatever was last typed.
        assert!(parse(&[]).unwrap().repo.is_none());
        assert!(parse(&["--pr", "7"]).unwrap().repo.is_none());
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
