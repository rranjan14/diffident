use crate::PrSummary;
use std::collections::{HashMap, HashSet};

/// One row of the review rail: a PR plus how far to indent it.
///
/// Flat rather than a tree because the rail renders as a flat virtualised list
/// — a tree would have to be flattened at every frame anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RailEntry {
    pub number: u32,
    pub depth: usize,
}

/// Order PRs so that each sits directly under the PR it is stacked on.
///
/// A PR is stacked on another when its base branch is that PR's head branch
/// (spec §6) — no branch-name heuristics, no graphite/spr config.
///
/// Roots keep their input order; children follow their parent immediately.
/// Cannot fail: a cyclic graph (which GitHub should prevent, but a mid-retarget
/// read can expose) degrades to treating the unreachable PRs as roots, so every
/// input PR appears exactly once.
pub fn stack_order(prs: &[PrSummary]) -> Vec<RailEntry> {
    // head branch -> index, so a base branch can find its parent PR.
    //
    // Cross-repo PRs are excluded as candidate parents: their head branch lives
    // in a fork, so a base branch resolving to the same *name* in this repo is a
    // coincidence, not a stack. Skipping them also makes this map collision-free
    // — git already forbids two branches of one name in one repo — so there is
    // no ambiguous-parent case left to resolve.
    let by_head: HashMap<&str, usize> = prs
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.is_cross_repository)
        .map(|(i, p)| (p.head_ref_name.as_str(), i))
        .collect();

    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, p) in prs.iter().enumerate() {
        match by_head.get(p.base_ref_name.as_str()) {
            Some(&parent) if parent != i => children.entry(parent).or_default().push(i),
            _ => roots.push(i),
        }
    }

    let mut out = Vec::with_capacity(prs.len());
    let mut seen = HashSet::new();
    for root in roots {
        walk(root, 0, prs, &children, &mut seen, &mut out);
    }
    // Anything still unvisited sits in a cycle. Emit it flat rather than lose it.
    for (i, pr) in prs.iter().enumerate() {
        if seen.insert(i) {
            out.push(RailEntry {
                number: pr.number,
                depth: 0,
            });
        }
    }
    out
}

fn walk(
    ix: usize,
    depth: usize,
    prs: &[PrSummary],
    children: &HashMap<usize, Vec<usize>>,
    seen: &mut HashSet<usize>,
    out: &mut Vec<RailEntry>,
) {
    if !seen.insert(ix) {
        return;
    }
    out.push(RailEntry {
        number: prs[ix].number,
        depth,
    });
    for &child in children.get(&ix).map(Vec::as_slice).unwrap_or(&[]) {
        walk(child, depth + 1, prs, children, seen, out);
    }
}

/// The contiguous run of rail entries forming one stack.
///
/// `stack_order` emits a root at depth 0 followed immediately by its
/// dependents at greater depth, so a stack is exactly "this depth-0 entry and
/// every entry after it until the next depth-0 entry". Returned as a range so
/// callers can walk it in either direction without copying.
pub fn stack_bounds(depths: &[usize], ix: usize) -> std::ops::Range<usize> {
    if ix >= depths.len() {
        return 0..0;
    }
    let start = depths[..=ix].iter().rposition(|d| *d == 0).unwrap_or(0);
    let end = depths[start + 1..]
        .iter()
        .position(|d| *d == 0)
        .map(|offset| start + 1 + offset)
        .unwrap_or(depths.len());
    start..end
}

/// The next entry to visit after `from`, wrapping within the stack.
///
/// Wraps rather than stopping: a reviewer working a stack wants to keep
/// cycling until everything is read, and stopping at the last member would
/// make them navigate back by hand.
pub fn next_in_stack(depths: &[usize], from: usize) -> Option<usize> {
    let bounds = stack_bounds(depths, from);
    if bounds.is_empty() {
        return None;
    }
    let len = bounds.end - bounds.start;
    let offset = from - bounds.start;
    Some(bounds.start + (offset + 1) % len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: u32, head: &str, base: &str) -> PrSummary {
        PrSummary {
            number,
            title: format!("PR {number}"),
            head_ref_name: head.to_string(),
            base_ref_name: base.to_string(),
            is_draft: false,
            url: String::new(),
            is_cross_repository: false,
        }
    }

    /// Same as `pr`, but the head branch lives in a fork.
    fn fork_pr(number: u32, head: &str, base: &str) -> PrSummary {
        PrSummary {
            is_cross_repository: true,
            ..pr(number, head, base)
        }
    }

    fn shape(entries: &[RailEntry]) -> Vec<(u32, usize)> {
        entries.iter().map(|e| (e.number, e.depth)).collect()
    }

    #[test]
    fn unrelated_prs_are_all_at_depth_zero() {
        let prs = [pr(1, "a", "main"), pr(2, "b", "main")];
        assert_eq!(shape(&stack_order(&prs)), vec![(1, 0), (2, 0)]);
    }

    #[test]
    fn a_pr_based_on_another_pr_head_is_nested() {
        let prs = [pr(1, "a", "main"), pr(2, "b", "a")];
        assert_eq!(shape(&stack_order(&prs)), vec![(1, 0), (2, 1)]);
    }

    #[test]
    fn a_three_deep_stack_nests_progressively() {
        let prs = [pr(1, "a", "main"), pr(2, "b", "a"), pr(3, "c", "b")];
        assert_eq!(shape(&stack_order(&prs)), vec![(1, 0), (2, 1), (3, 2)]);
    }

    #[test]
    fn children_follow_their_parent_regardless_of_input_order() {
        let prs = [pr(3, "c", "b"), pr(1, "a", "main"), pr(2, "b", "a")];
        assert_eq!(shape(&stack_order(&prs)), vec![(1, 0), (2, 1), (3, 2)]);
    }

    #[test]
    fn two_prs_on_the_same_base_both_nest_under_it() {
        let prs = [pr(1, "a", "main"), pr(2, "b", "a"), pr(3, "c", "a")];
        assert_eq!(shape(&stack_order(&prs)), vec![(1, 0), (2, 1), (3, 1)]);
    }

    #[test]
    fn every_pr_appears_exactly_once_even_in_a_cycle() {
        // GitHub should make this impossible; a corrupt or mid-retarget state
        // must still render rather than hang or drop PRs.
        let prs = [pr(1, "a", "b"), pr(2, "b", "a")];
        let out = stack_order(&prs);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert!(stack_order(&[]).is_empty());
    }

    #[test]
    fn a_fork_pr_never_becomes_the_parent_of_prs_that_merely_share_its_branch_name() {
        // `main` and `patch-1` are the two most common fork head branches.
        // Matching on branch name alone made every main-targeting PR a child.
        let prs = [
            fork_pr(1, "main", "main"),
            pr(2, "feature-a", "main"),
            pr(3, "feature-b", "main"),
        ];
        assert_eq!(shape(&stack_order(&prs)), vec![(1, 0), (2, 0), (3, 0)]);
    }

    #[test]
    fn two_forks_sharing_a_head_branch_name_nest_nothing_under_either() {
        let prs = [
            fork_pr(1, "patch-1", "main"),
            fork_pr(2, "patch-1", "main"),
            pr(3, "x", "patch-1"),
        ];
        assert_eq!(shape(&stack_order(&prs)), vec![(1, 0), (2, 0), (3, 0)]);
    }

    #[test]
    fn a_fork_pr_can_still_be_stacked_on_a_branch_in_this_repo() {
        // Only the *parent* side needs a same-repo head; a fork PR's base
        // always names a branch here, so it can legitimately be a child.
        let prs = [pr(1, "a", "main"), fork_pr(2, "b", "a")];
        assert_eq!(shape(&stack_order(&prs)), vec![(1, 0), (2, 1)]);
    }
}

#[cfg(test)]
mod stack_walk_tests {
    use super::*;

    // Two stacks and a loner:
    //   0: #1 root      depth 0
    //   1: #2 on #1     depth 1
    //   2: #3 on #2     depth 2
    //   3: #4 alone     depth 0
    //   4: #5 root      depth 0
    //   5: #6 on #5     depth 1
    const DEPTHS: [usize; 6] = [0, 1, 2, 0, 0, 1];

    #[test]
    fn a_three_deep_stack_is_one_group() {
        assert_eq!(stack_bounds(&DEPTHS, 0), 0..3);
        assert_eq!(stack_bounds(&DEPTHS, 1), 0..3);
        assert_eq!(stack_bounds(&DEPTHS, 2), 0..3, "found from any member");
    }

    #[test]
    fn a_pr_with_no_dependents_is_a_stack_of_one() {
        assert_eq!(stack_bounds(&DEPTHS, 3), 3..4);
    }

    #[test]
    fn a_later_stack_does_not_absorb_the_earlier_one() {
        assert_eq!(stack_bounds(&DEPTHS, 5), 4..6);
    }

    #[test]
    fn walking_a_stack_visits_every_member_then_wraps() {
        let mut seen = vec![0];
        let mut at = 0;
        for _ in 0..3 {
            at = next_in_stack(&DEPTHS, at).unwrap();
            seen.push(at);
        }
        assert_eq!(seen, vec![0, 1, 2, 0], "three members, then back to the root");
    }

    #[test]
    fn walking_never_leaves_the_stack() {
        // From the middle of the first stack we must never reach #4 or #5.
        let mut at = 1;
        for _ in 0..10 {
            at = next_in_stack(&DEPTHS, at).unwrap();
            assert!((0..3).contains(&at), "escaped the stack to {at}");
        }
    }

    #[test]
    fn a_lone_pr_walks_to_itself() {
        assert_eq!(next_in_stack(&DEPTHS, 3), Some(3));
    }

    #[test]
    fn an_empty_rail_has_nowhere_to_go() {
        assert_eq!(next_in_stack(&[], 0), None);
        assert_eq!(stack_bounds(&[], 0), 0..0);
    }

    #[test]
    fn an_out_of_range_index_is_not_a_panic() {
        assert_eq!(next_in_stack(&DEPTHS, 99), None);
    }
}
