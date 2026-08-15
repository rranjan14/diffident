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
    let by_head: HashMap<&str, usize> = prs
        .iter()
        .enumerate()
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
}
