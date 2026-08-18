//! Orchestrator: parse args, open the window, wire the views. Nothing else.

use diffident_forge::{Forge as _, Repo, gh::Gh, github::GitHub};
use diffident_session::config;
use diffident_ui::{navigate, workspace::Workspace};
use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

const USAGE: &str = "\
usage:
  diffident                  review the pull requests of the checkout you are in
  diffident pr <target>      open one, where <target> is
                               123
                               owner/repo#123
                               https://github.com/owner/repo/pull/123
  diffident --repo <owner/name> [--pr <number>]";

/// Resolve a `pr` target into the repo it names, if it names one, and a number.
///
/// Three forms because that is what people actually have in hand: a bare number
/// from a conversation, a slug from a commit message, a URL from a browser tab.
/// Two of them carry the repo, which is the point — pasting a URL should not
/// also require saying which repository it came from.
fn parse_target(raw: &str) -> Result<(Option<Repo>, u32), String> {
    let bad = || {
        format!("cannot read {raw:?} as a pull request\n{USAGE}")
    };
    let repo = |owner: &str, name: &str| Repo {
        owner: owner.to_string(),
        name: name.to_string(),
    };

    // A URL, in any of the forms a browser leaves on the clipboard:
    // .../owner/repo/pull/123, .../pull/123/files, .../pull/123#discussion_r1.
    if let Some((before, after)) = raw.split_once("/pull/") {
        let mut segments = before.rsplit('/').filter(|s| !s.is_empty());
        let name = segments.next().ok_or_else(bad)?;
        let owner = segments.next().ok_or_else(bad)?;
        let number = after
            .split(['/', '#', '?'])
            .next()
            .unwrap_or_default()
            .parse()
            .map_err(|_| bad())?;
        return Ok((Some(repo(owner, name)), number));
    }

    // `owner/repo#123`, the form that fits in a commit message.
    if let Some((slug, number)) = raw.split_once('#') {
        let (owner, name) = slug.split_once('/').ok_or_else(bad)?;
        if owner.is_empty() || name.is_empty() {
            return Err(bad());
        }
        return Ok((Some(repo(owner, name)), number.parse().map_err(|_| bad())?));
    }

    // A bare number: whichever repo we are standing in.
    Ok((None, raw.parse().map_err(|_| bad())?))
}

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

        // `diffident pr <target>`. A subcommand rather than a flag because the
        // pull request is the object you are acting on, not a modifier.
        if args.peek().is_some_and(|a| a == "pr") {
            args.next();
            let target = args
                .next()
                .ok_or_else(|| format!("`pr` needs a target\n{USAGE}"))?;
            let (from_target, number) = parse_target(&target)?;
            repo = from_target;
            pr = Some(number);
        }

        while let Some(arg) = args.next() {
            match arg.as_str() {
                // Explicit wins: a target's repo is an inference, this is not.
                "--repo" => {
                    let slug = args
                        .next()
                        .ok_or_else(|| format!("--repo needs owner/name\n{USAGE}"))?;
                    let (owner, name) = slug.split_once('/').ok_or_else(|| {
                        format!("--repo must be owner/name, got {slug:?}\n{USAGE}")
                    })?;
                    repo = Some(Repo {
                        owner: owner.to_string(),
                        name: name.to_string(),
                    });
                }
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

    fn target(args: &[&str]) -> (Option<String>, Option<u32>) {
        let a = parse(args).unwrap();
        (a.repo.map(|r| r.slug()), a.pr)
    }

    #[test]
    fn pr_with_a_bare_number_uses_the_checkout_you_are_in() {
        assert_eq!(target(&["pr", "123"]), (None, Some(123)));
    }

    #[test]
    fn pr_with_a_slug_carries_its_own_repo() {
        // The form that fits in a commit message, so pasting one is enough.
        assert_eq!(
            target(&["pr", "cli/cli#13758"]),
            (Some("cli/cli".to_string()), Some(13758))
        );
    }

    #[test]
    fn pr_with_a_url_carries_its_own_repo() {
        // Straight off the clipboard from a browser tab; requiring --repo as
        // well would mean retyping what the URL already says.
        assert_eq!(
            target(&["pr", "https://github.com/cli/cli/pull/13758"]),
            (Some("cli/cli".to_string()), Some(13758))
        );
    }

    #[test]
    fn a_url_still_parses_with_whatever_the_browser_appended() {
        // GitHub adds these itself as you click around a review.
        for url in [
            "https://github.com/cli/cli/pull/13758/files",
            "https://github.com/cli/cli/pull/13758#discussion_r123",
            "https://github.com/cli/cli/pull/13758?w=1",
        ] {
            assert_eq!(target(&["pr", url]), (Some("cli/cli".to_string()), Some(13758)), "{url}");
        }
    }

    #[test]
    fn an_explicit_repo_overrides_the_one_a_target_implied() {
        // The flag is a statement; a target's repo is an inference.
        assert_eq!(
            target(&["pr", "cli/cli#5", "--repo", "o/r"]),
            (Some("o/r".to_string()), Some(5))
        );
    }

    #[test]
    fn a_target_that_is_not_a_pull_request_says_so_with_the_forms_it_accepts() {
        let err = parse(&["pr", "banana"]).unwrap_err();
        assert!(err.contains("owner/repo#123"), "got: {err}");
    }

    #[test]
    fn pr_without_a_target_is_an_error_rather_than_opening_the_rail() {
        assert!(parse(&["pr"]).unwrap_err().contains("target"));
    }

    #[test]
    fn a_non_numeric_pr_is_rejected() {
        assert!(parse(&["--repo", "o/r", "--pr", "abc"]).is_err());
    }
}
