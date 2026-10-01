//! Pure tree-shaping for the agent sidebar.
//!
//! Turns a flat agent list, parent links, and a collapsed/expanded set into
//! the ordered display rows the sidebar draws. No rendering, no I/O, no wire
//! snapshot types — callers translate the wire snapshot into `TreeAgent`s.

use std::collections::{HashMap, HashSet};

use crate::api::schema::AgentStatus;

/// One agent as the tree sees it. Constructed by the caller from the wire snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TreeAgent {
    pub(super) pane_id: String,
    pub(super) parent_pane_id: Option<String>,
    pub(super) status: AgentStatus,
    /// Higher = changed more recently. Used for child ordering.
    pub(super) state_change_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TreeRow {
    pub(super) pane_id: String,
    pub(super) depth: u16, // 0 = root
    pub(super) has_children: bool,
    pub(super) expanded: bool,         // meaningful only when has_children
    pub(super) rollup: Option<Rollup>, // Some only when has_children && !expanded
    /// Same rollup data as `rollup`, but populated whenever `has_children` is
    /// true regardless of expand/collapse state. Collapsed-only `rollup`
    /// exists to drive the default "N agents · k working" suffix, which only
    /// makes sense when the children are hidden; this field exists for
    /// callers (e.g. the `CD-` coordinator live-count suffix) that need the
    /// descendant totals even while the row is expanded.
    pub(super) descendant_rollup: Option<Rollup>,
}

/// Summary of a collapsed subtree (ALL descendants, not just direct children).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Rollup {
    pub(super) total: usize,
    pub(super) counts: Vec<(AgentStatus, usize)>, // non-zero statuses only, most urgent first
    pub(super) most_urgent: AgentStatus,
}

/// Status urgency, most urgent last... no: higher = more urgent, matching
/// `status_priority` in the parent `shell` module (see `agent_sidebar.rs`'s
/// use of it for `Priority` sort). That function is private to `shell` but
/// visible here since `agent_tree` is a descendant module, so we delegate
/// instead of duplicating the ordering.
fn urgency(status: AgentStatus) -> u8 {
    super::status_priority(status)
}

/// Builds the ordered display rows for the agent tree.
///
/// `toggled` holds pane ids whose expansion state is the OPPOSITE of
/// `default_expanded` — i.e. `is_expanded(id) == default_expanded ^
/// toggled.contains(id)`. With `default_expanded = false` (the shipped
/// config), `toggled` is effectively an "expanded" set; with
/// `default_expanded = true` it is effectively a "collapsed" set.
pub(super) fn build_rows(
    agents: &[TreeAgent],
    root_order: &[String],
    toggled: &HashSet<String>,
    default_expanded: bool,
) -> Vec<TreeRow> {
    if agents.is_empty() {
        return Vec::new();
    }

    let index_of: HashMap<&str, usize> = agents
        .iter()
        .enumerate()
        .map(|(i, a)| (a.pane_id.as_str(), i))
        .collect();

    // Only edges whose parent actually exists in `agents` are real tree
    // edges; a parent id absent from the input floats its child up to root.
    let parent_of: HashMap<&str, &str> = agents
        .iter()
        .filter_map(|a| {
            let parent = a.parent_pane_id.as_deref()?;
            index_of
                .contains_key(parent)
                .then_some((a.pane_id.as_str(), parent))
        })
        .collect();

    // Direct roots: no parent, or parent not present in the input at all.
    let mut is_root: HashSet<&str> = agents
        .iter()
        .filter(|a| match a.parent_pane_id.as_deref() {
            None => true,
            Some(parent) => !index_of.contains_key(parent),
        })
        .map(|a| a.pane_id.as_str())
        .collect();

    // Break cycles among the remaining (parent-linked) agents: walk each
    // node's parent chain. The first node seen twice in a given walk becomes
    // a root, and its own parent edge is cut (excluded from `children_of`
    // below) so the cycle does not get walked twice or infinitely.
    let mut cut: HashSet<&str> = HashSet::new();
    let mut resolved: HashSet<&str> = is_root.clone();
    for agent in agents {
        let start = agent.pane_id.as_str();
        if resolved.contains(start) {
            continue;
        }
        let mut path: Vec<&str> = Vec::new();
        let mut cur = start;
        loop {
            if resolved.contains(cur) {
                break;
            }
            if path.contains(&cur) {
                // `cur` is revisited: it becomes a cycle-break root.
                is_root.insert(cur);
                cut.insert(cur);
                resolved.insert(cur);
                break;
            }
            path.push(cur);
            match parent_of.get(cur) {
                Some(parent) => cur = *parent,
                None => break, // unreachable: `cur` would already be a root
            }
        }
        for node in path {
            resolved.insert(node);
        }
    }

    // Children, sorted by (urgency desc, state_change_seq desc).
    let mut children_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for agent in agents {
        let pane_id = agent.pane_id.as_str();
        if cut.contains(pane_id) {
            continue;
        }
        if let Some(parent) = parent_of.get(pane_id) {
            children_of.entry(*parent).or_default().push(pane_id);
        }
    }
    for kids in children_of.values_mut() {
        kids.sort_by_key(|pane_id: &&str| {
            let agent = &agents[index_of[*pane_id]];
            (
                std::cmp::Reverse(urgency(agent.status)),
                std::cmp::Reverse(agent.state_change_seq),
            )
        });
    }

    // Root order: by `root_order` position first, then agents absent from
    // it, in input order.
    let position_of: HashMap<&str, usize> = root_order
        .iter()
        .enumerate()
        .map(|(i, pane_id)| (pane_id.as_str(), i))
        .collect();
    let mut roots: Vec<&str> = agents
        .iter()
        .map(|a| a.pane_id.as_str())
        .filter(|pane_id| is_root.contains(pane_id))
        .collect();
    roots.sort_by_key(|pane_id: &&str| match position_of.get(*pane_id) {
        Some(pos) => (0usize, *pos),
        None => (1usize, index_of[*pane_id]),
    });

    let mut rows = Vec::with_capacity(agents.len());
    for root in roots {
        push_subtree(
            root,
            0,
            agents,
            &index_of,
            &children_of,
            toggled,
            default_expanded,
            &mut rows,
        );
    }
    rows
}

fn is_expanded(pane_id: &str, toggled: &HashSet<String>, default_expanded: bool) -> bool {
    default_expanded ^ toggled.contains(pane_id)
}

#[allow(clippy::too_many_arguments)]
fn push_subtree(
    pane_id: &str,
    depth: u16,
    agents: &[TreeAgent],
    index_of: &HashMap<&str, usize>,
    children_of: &HashMap<&str, Vec<&str>>,
    toggled: &HashSet<String>,
    default_expanded: bool,
    rows: &mut Vec<TreeRow>,
) {
    let no_children: Vec<&str> = Vec::new();
    let children = children_of.get(pane_id).unwrap_or(&no_children);
    let has_children = !children.is_empty();
    let expanded = has_children && is_expanded(pane_id, toggled, default_expanded);
    let descendant_rollup = if has_children {
        Some(build_rollup(children, agents, index_of, children_of))
    } else {
        None
    };
    let rollup = if expanded {
        None
    } else {
        descendant_rollup.clone()
    };
    rows.push(TreeRow {
        pane_id: pane_id.to_string(),
        depth,
        has_children,
        expanded,
        rollup,
        descendant_rollup,
    });
    if expanded {
        for child in children {
            push_subtree(
                child,
                depth + 1,
                agents,
                index_of,
                children_of,
                toggled,
                default_expanded,
                rows,
            );
        }
    }
}

fn build_rollup(
    direct_children: &[&str],
    agents: &[TreeAgent],
    index_of: &HashMap<&str, usize>,
    children_of: &HashMap<&str, Vec<&str>>,
) -> Rollup {
    // `AgentStatus` is not `Hash`, so tally with a small linear-scan Vec
    // instead of a HashMap — there are only a handful of statuses.
    let mut counts: Vec<(AgentStatus, usize)> = Vec::new();
    let mut stack: Vec<&str> = direct_children.to_vec();
    let mut total = 0usize;
    while let Some(pane_id) = stack.pop() {
        total += 1;
        let agent = &agents[index_of[pane_id]];
        match counts
            .iter_mut()
            .find(|(status, _)| *status == agent.status)
        {
            Some((_, n)) => *n += 1,
            None => counts.push((agent.status, 1)),
        }
        if let Some(kids) = children_of.get(pane_id) {
            stack.extend(kids.iter().copied());
        }
    }
    counts.sort_by_key(|(status, _)| std::cmp::Reverse(urgency(*status)));
    let most_urgent = counts
        .first()
        .map(|(status, _)| *status)
        .unwrap_or(AgentStatus::Unknown);
    Rollup {
        total,
        counts,
        most_urgent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(pane_id: &str, parent: Option<&str>, status: AgentStatus, seq: u64) -> TreeAgent {
        TreeAgent {
            pane_id: pane_id.to_string(),
            parent_pane_id: parent.map(str::to_string),
            status,
            state_change_seq: seq,
        }
    }

    fn ids(pane_ids: &[&str]) -> Vec<String> {
        pane_ids.iter().map(|s| s.to_string()).collect()
    }

    fn set(pane_ids: &[&str]) -> HashSet<String> {
        pane_ids.iter().map(|s| s.to_string()).collect()
    }

    fn row_ids(rows: &[TreeRow]) -> Vec<&str> {
        rows.iter().map(|r| r.pane_id.as_str()).collect()
    }

    #[test]
    fn flat_list_matches_root_order() {
        let agents = vec![
            agent("a", None, AgentStatus::Idle, 1),
            agent("b", None, AgentStatus::Idle, 2),
            agent("c", None, AgentStatus::Idle, 3),
        ];
        let root_order = ids(&["c", "a", "b"]);
        let rows = build_rows(&agents, &root_order, &HashSet::new(), false);
        assert_eq!(row_ids(&rows), vec!["c", "a", "b"]);
        assert!(rows.iter().all(|r| r.depth == 0 && !r.has_children));
    }

    #[test]
    fn one_parent_two_children_collapsed_by_default() {
        let agents = vec![
            agent("p", None, AgentStatus::Idle, 0),
            agent("c1", Some("p"), AgentStatus::Working, 1),
            agent("c2", Some("p"), AgentStatus::Blocked, 2),
        ];
        let root_order = ids(&["p"]);
        let rows = build_rows(&agents, &root_order, &HashSet::new(), false);
        assert_eq!(row_ids(&rows), vec!["p"]);
        let rollup = rows[0].rollup.as_ref().expect("collapsed rollup");
        assert_eq!(rollup.total, 2);
        assert_eq!(rollup.most_urgent, AgentStatus::Blocked);
    }

    #[test]
    fn same_toggled_expands_children_ordered_by_urgency_then_seq() {
        let agents = vec![
            agent("p", None, AgentStatus::Idle, 0),
            agent("c1", Some("p"), AgentStatus::Working, 5),
            agent("c2", Some("p"), AgentStatus::Blocked, 1),
            agent("c3", Some("p"), AgentStatus::Working, 9),
        ];
        let root_order = ids(&["p"]);
        let toggled = set(&["p"]);
        let rows = build_rows(&agents, &root_order, &toggled, false);
        assert_eq!(row_ids(&rows), vec!["p", "c2", "c3", "c1"]);
        assert!(rows[0].expanded);
        assert!(rows[0].rollup.is_none());
        assert!(rows[1..].iter().all(|r| r.depth == 1 && !r.has_children));
    }

    #[test]
    fn three_levels_expanded_and_collapsing_middle_hides_only_its_subtree() {
        let agents = vec![
            agent("root", None, AgentStatus::Idle, 0),
            agent("mid", Some("root"), AgentStatus::Idle, 1),
            agent("leaf", Some("mid"), AgentStatus::Blocked, 2),
        ];
        let root_order = ids(&["root"]);

        let all_expanded = build_rows(&agents, &root_order, &set(&["root", "mid"]), false);
        assert_eq!(row_ids(&all_expanded), vec!["root", "mid", "leaf"]);
        assert_eq!(all_expanded[0].depth, 0);
        assert_eq!(all_expanded[1].depth, 1);
        assert_eq!(all_expanded[2].depth, 2);

        let mid_collapsed = build_rows(&agents, &root_order, &set(&["root"]), false);
        assert_eq!(row_ids(&mid_collapsed), vec!["root", "mid"]);
        let rollup = mid_collapsed[1]
            .rollup
            .as_ref()
            .expect("mid collapsed rollup");
        assert_eq!(rollup.total, 1);
        assert_eq!(rollup.most_urgent, AgentStatus::Blocked);
    }

    #[test]
    fn orphan_becomes_root() {
        let agents = vec![agent("a", Some("missing-parent"), AgentStatus::Idle, 0)];
        let rows = build_rows(&agents, &[], &HashSet::new(), false);
        assert_eq!(row_ids(&rows), vec!["a"]);
        assert_eq!(rows[0].depth, 0);
    }

    #[test]
    fn cycle_emits_both_nodes_once_without_panicking() {
        let agents = vec![
            agent("a", Some("b"), AgentStatus::Idle, 0),
            agent("b", Some("a"), AgentStatus::Idle, 1),
        ];
        let rows = build_rows(&agents, &[], &set(&["a", "b"]), false);
        let mut got = row_ids(&rows);
        got.sort_unstable();
        assert_eq!(got, vec!["a", "b"]);
    }

    #[test]
    fn agent_missing_from_root_order_is_appended_after() {
        let agents = vec![
            agent("a", None, AgentStatus::Idle, 0),
            agent("b", None, AgentStatus::Idle, 1),
            agent("c", None, AgentStatus::Idle, 2),
        ];
        let root_order = ids(&["b", "a"]);
        let rows = build_rows(&agents, &root_order, &HashSet::new(), false);
        assert_eq!(row_ids(&rows), vec!["b", "a", "c"]);
    }

    #[test]
    fn descendant_rollup_is_populated_whether_expanded_or_collapsed() {
        let agents = vec![
            agent("p", None, AgentStatus::Idle, 0),
            agent("c1", Some("p"), AgentStatus::Working, 1),
            agent("c2", Some("p"), AgentStatus::Working, 2),
            agent("c3", Some("p"), AgentStatus::Idle, 3),
            agent("c4", Some("p"), AgentStatus::Blocked, 4),
        ];
        let root_order = ids(&["p"]);

        let collapsed = build_rows(&agents, &root_order, &HashSet::new(), false);
        assert!(!collapsed[0].expanded);
        assert!(collapsed[0].rollup.is_some(), "collapsed row keeps rollup");
        let collapsed_descendants = collapsed[0]
            .descendant_rollup
            .as_ref()
            .expect("descendant_rollup present when collapsed");
        assert_eq!(collapsed_descendants.total, 4);
        let working = |rollup: &Rollup| {
            rollup
                .counts
                .iter()
                .find(|(status, _)| *status == AgentStatus::Working)
                .map(|(_, n)| *n)
                .unwrap_or(0)
        };
        assert_eq!(working(collapsed_descendants), 2);

        let expanded = build_rows(&agents, &root_order, &set(&["p"]), false);
        assert!(expanded[0].expanded);
        assert!(
            expanded[0].rollup.is_none(),
            "expanded row has no collapsed-only rollup"
        );
        let expanded_descendants = expanded[0]
            .descendant_rollup
            .as_ref()
            .expect("descendant_rollup present even when expanded");
        assert_eq!(expanded_descendants.total, 4);
        assert_eq!(working(expanded_descendants), 2);
    }

    #[test]
    fn default_expanded_true_with_empty_toggled_shows_everything() {
        let agents = vec![
            agent("p", None, AgentStatus::Idle, 0),
            agent("c1", Some("p"), AgentStatus::Idle, 1),
            agent("c2", Some("p"), AgentStatus::Idle, 2),
        ];
        let rows = build_rows(&agents, &ids(&["p"]), &HashSet::new(), true);
        assert_eq!(rows.len(), 3);
        assert!(rows[0].expanded);
    }

    fn assert_expanded_covers_every_agent_once(agents: &[TreeAgent], root_order: &[String]) {
        let rows = build_rows(agents, root_order, &HashSet::new(), true);
        assert_eq!(rows.len(), agents.len());
        let mut seen: HashSet<&str> = HashSet::new();
        for row in &rows {
            assert!(
                seen.insert(row.pane_id.as_str()),
                "duplicate row for {}",
                row.pane_id
            );
        }
    }

    #[test]
    fn invariant_expanded_output_has_len_agents_with_unique_ids() {
        // Case 1: single chain.
        assert_expanded_covers_every_agent_once(
            &[
                agent("a", None, AgentStatus::Idle, 0),
                agent("b", Some("a"), AgentStatus::Idle, 1),
                agent("c", Some("b"), AgentStatus::Idle, 2),
                agent("d", Some("c"), AgentStatus::Idle, 3),
            ],
            &ids(&["a"]),
        );

        // Case 2: wide fan-out from one root.
        assert_expanded_covers_every_agent_once(
            &[
                agent("root", None, AgentStatus::Idle, 0),
                agent("c1", Some("root"), AgentStatus::Working, 1),
                agent("c2", Some("root"), AgentStatus::Blocked, 2),
                agent("c3", Some("root"), AgentStatus::Done, 3),
                agent("c4", Some("root"), AgentStatus::Unknown, 4),
            ],
            &ids(&["root"]),
        );

        // Case 3: multiple disjoint roots, mixed depths.
        assert_expanded_covers_every_agent_once(
            &[
                agent("r1", None, AgentStatus::Idle, 0),
                agent("r2", None, AgentStatus::Idle, 1),
                agent("r1c", Some("r1"), AgentStatus::Working, 2),
                agent("r2c", Some("r2"), AgentStatus::Blocked, 3),
                agent("r2gc", Some("r2c"), AgentStatus::Idle, 4),
            ],
            &ids(&["r1", "r2"]),
        );

        // Case 4: an orphan mixed in with a real tree.
        assert_expanded_covers_every_agent_once(
            &[
                agent("root", None, AgentStatus::Idle, 0),
                agent("child", Some("root"), AgentStatus::Idle, 1),
                agent("orphan", Some("nowhere"), AgentStatus::Idle, 2),
            ],
            &ids(&["root"]),
        );

        // Case 5: a cycle plus an unrelated real tree.
        assert_expanded_covers_every_agent_once(
            &[
                agent("root", None, AgentStatus::Idle, 0),
                agent("child", Some("root"), AgentStatus::Idle, 1),
                agent("x", Some("y"), AgentStatus::Idle, 2),
                agent("y", Some("z"), AgentStatus::Idle, 3),
                agent("z", Some("x"), AgentStatus::Idle, 4),
            ],
            &ids(&["root"]),
        );
    }
}
