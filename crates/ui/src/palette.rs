//! The command palette's matching and naming (§8: `Cmd-P`).
//!
//! Pure. The palette's *contents* are discovered from gpui's action registry at
//! runtime rather than listed here, so a new action appears in it the moment it
//! is declared — a hand-written table of commands is a table that silently
//! stops matching the keymap. What this module owns is turning a type name into
//! something a person would read, and ranking those names against a query.

/// A command the palette can run: the action's registered name, what to call it
/// on screen, and the key that already does it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The registry name, e.g. `diffident::ToggleResolved`. This is what
    /// `App::build_action` takes, so it is the identity that matters.
    pub action: String,
    pub label: String,
    /// The bound keystroke, when there is one. Commands with no key are still
    /// listed — the palette is the only way to reach them.
    pub keys: Option<String>,
}

/// Turn `diffident::ToggleResolved` into `Toggle resolved`.
///
/// Derived rather than curated. A curated label reads slightly better and goes
/// stale the first time someone adds an action without touching the table;
/// this cannot, because there is no table.
pub fn label_for(action: &str) -> String {
    let bare = action.rsplit("::").next().unwrap_or(action);
    let mut out = String::with_capacity(bare.len() + 4);
    for (i, ch) in bare.char_indices() {
        if ch.is_uppercase() && i != 0 {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else if i == 0 {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// How well `query` matches `label`, or `None` when it does not.
///
/// A subsequence match, scored so that the ranking matches what people expect
/// from a palette: letters that start a word count for much more than letters
/// in the middle of one, and a run of adjacent letters counts for more than the
/// same letters scattered. That is what makes `tr` find "Toggle resolved"
/// rather than "Prev thread", which contains both letters but neither at a
/// word start.
///
/// An empty query matches everything with score 0, so the palette opens showing
/// the full list rather than nothing.
pub fn score(label: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let label_lower = label.to_lowercase();
    let hay: Vec<char> = label_lower.chars().collect();
    let needle: Vec<char> = query.to_lowercase().chars().collect();

    let mut total = 0;
    let mut at = 0;
    let mut last_hit: Option<usize> = None;
    for want in needle {
        let found = hay[at..].iter().position(|c| *c == want)? + at;
        let starts_word = found == 0 || hay[found - 1] == ' ' || hay[found - 1] == '-';
        total += if starts_word {
            10
        } else if last_hit == Some(found.saturating_sub(1)) {
            5
        } else {
            1
        };
        last_hit = Some(found);
        at = found + 1;
    }
    // Shorter labels win ties: with equal evidence, the more specific command
    // is the one you meant.
    Some(total - (hay.len() as i32) / 8)
}

/// The commands matching `query`, best first.
///
/// Stable within a score so the list does not reshuffle under the cursor while
/// you type another letter that changes nothing.
pub fn filter(commands: &[Command], query: &str) -> Vec<usize> {
    let mut scored: Vec<(usize, i32)> = commands
        .iter()
        .enumerate()
        .filter_map(|(ix, c)| score(&c.label, query).map(|s| (ix, s)))
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(ix, _)| ix).collect()
}

/// Every diffident action the app has registered, with its key if it has one.
///
/// Read from gpui's registry rather than listed, so an action added tomorrow
/// appears here without anyone remembering to add it. Actions from gpui itself
/// are filtered out — they are not commands a reviewer means to run.
pub fn commands(all_action_names: &[&str], bindings: &[gpui::KeyBinding]) -> Vec<Command> {
    let mut out: Vec<Command> = all_action_names
        .iter()
        .filter(|n| n.starts_with("diffident::"))
        .map(|n| Command {
            action: (*n).to_string(),
            label: label_for(n),
            keys: bindings
                .iter()
                .find(|b| b.action().name() == *n)
                .map(|b| {
                    b.keystrokes()
                        .iter()
                        .map(|k| k.unparse())
                        .collect::<Vec<_>>()
                        .join(" ")
                }),
        })
        .collect();
    // Alphabetical, so the unfiltered list has an order a person can scan
    // rather than whatever order the registry happens to hold.
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(label: &str) -> Command {
        Command {
            action: format!("diffident::{}", label.replace(' ', "")),
            label: label.to_string(),
            keys: None,
        }
    }

    #[test]
    fn a_type_name_becomes_something_a_person_would_read() {
        assert_eq!(label_for("diffident::ToggleResolved"), "Toggle resolved");
        assert_eq!(label_for("diffident::NextUnreviewed"), "Next unreviewed");
        assert_eq!(label_for("diffident::Submit"), "Submit");
    }

    #[test]
    fn a_name_with_no_namespace_still_reads() {
        assert_eq!(label_for("HalfPageDown"), "Half page down");
    }

    #[test]
    fn an_empty_query_lists_everything() {
        // The palette opens showing what there is, not an empty box.
        let all = [cmd("Submit"), cmd("Next line")];
        assert_eq!(filter(&all, "").len(), 2);
    }

    #[test]
    fn letters_must_all_appear_in_order() {
        assert!(score("Toggle resolved", "tr").is_some());
        assert!(score("Toggle resolved", "rt").is_none(), "wrong order");
        assert!(score("Toggle resolved", "tz").is_none(), "no z");
    }

    #[test]
    fn word_starts_outrank_letters_buried_mid_word() {
        // What makes `tr` find "Toggle resolved" rather than something that
        // merely contains a t and an r.
        let word_starts = score("Toggle resolved", "tr").unwrap();
        let buried = score("Prev thread", "tr").unwrap();
        assert!(word_starts > buried, "{word_starts} vs {buried}");
    }

    #[test]
    fn adjacent_letters_beat_the_same_letters_scattered() {
        // Both match; the run is the one you meant. Without this, typing more
        // letters can rank a worse command higher.
        let adjacent = score("abc", "bc").unwrap();
        let scattered = score("abxc", "bc").unwrap();
        assert!(adjacent > scattered, "{adjacent} vs {scattered}");
    }

    #[test]
    fn ranking_puts_the_best_first() {
        let all = [cmd("Prev thread"), cmd("Toggle resolved"), cmd("Top")];
        let ranked = filter(&all, "tr");
        assert_eq!(ranked[0], 1, "Toggle resolved wins: {ranked:?}");
    }

    #[test]
    fn ties_keep_their_original_order_so_the_list_does_not_jitter() {
        // Re-sorting equal matches under the cursor while someone types is how
        // a palette selects the wrong thing on enter.
        let all = [cmd("Alpha"), cmd("Alpha"), cmd("Alpha")];
        assert_eq!(filter(&all, "a"), vec![0, 1, 2]);
    }

    #[test]
    fn matching_ignores_case_in_both_directions() {
        assert!(score("Toggle resolved", "TOGGLE").is_some());
        assert!(score("TOGGLE RESOLVED", "toggle").is_some());
    }

    #[test]
    fn commands_come_from_the_registry_and_skip_gpuis_own() {
        // Discovering them is the point: a table would stop matching the keymap
        // the first time someone added an action without updating it.
        let names = ["diffident::Submit", "gpui::Quit", "diffident::NextLine"];
        let cmds = commands(&names, &crate::navigate::key_bindings());
        assert_eq!(cmds.len(), 2, "gpui's own actions are not commands: {cmds:?}");
        assert!(cmds.iter().all(|c| c.action.starts_with("diffident::")));
    }

    #[test]
    fn a_command_carries_the_key_that_already_runs_it() {
        let cmds = commands(&["diffident::NextLine"], &crate::navigate::key_bindings());
        assert_eq!(cmds[0].keys.as_deref(), Some("j"));
    }

    #[test]
    fn a_command_with_no_binding_is_still_listed() {
        // The palette is the only way to reach it, so leaving it out would make
        // it unreachable rather than tidy.
        let cmds = commands(&["diffident::Submit"], &[]);
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].keys, None);
    }

    #[test]
    fn the_unfiltered_list_is_alphabetical() {
        let names = ["diffident::Submit", "diffident::NextLine"];
        let cmds = commands(&names, &[]);
        assert_eq!(cmds[0].label, "Next line");
    }

    #[test]
    fn nothing_matches_a_query_with_no_hope() {
        let all = [cmd("Submit"), cmd("Next line")];
        assert!(filter(&all, "zzzz").is_empty());
    }
}
