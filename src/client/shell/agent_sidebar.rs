use std::collections::{HashMap, HashSet};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Paragraph, Widget},
};

use super::*;

pub(super) struct AgentRow {
    pub(super) pane_id: String,
    pub(super) status: crate::api::schema::AgentStatus,
    pub(super) focused: bool,
    pub(super) rows: Vec<Vec<crate::ui::ResolvedToken>>,
    pub(super) depth: u16,
    pub(super) has_children: bool,
    pub(super) expanded: bool,
    pub(super) rollup: Option<super::agent_tree::Rollup>,
    /// Same data as `rollup` but populated whether the row is expanded or
    /// collapsed; used to compute the `CD-` coordinator live-count suffix,
    /// which must show while expanded too.
    pub(super) descendant_rollup: Option<super::agent_tree::Rollup>,
    /// The agent's resolved display label (same value rendered via the
    /// `agent` token), used to detect `CD-` coordinator rows.
    pub(super) label: Option<String>,
}

pub(super) fn ordered_agent_pane_ids(
    snapshot: &ClientShellSnapshot,
    sort: crate::config::AgentPanelSortConfig,
) -> Vec<String> {
    if snapshot.agent_view_label.is_some() {
        return snapshot
            .agent_order
            .iter()
            .filter(|pane_id| {
                snapshot
                    .agents
                    .iter()
                    .any(|agent| agent.pane_id == pane_id.as_str())
            })
            .cloned()
            .collect();
    }
    let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
    if sort == crate::config::AgentPanelSortConfig::Priority {
        agents.sort_by_key(|agent| {
            (
                std::cmp::Reverse(status_priority(agent.agent_status)),
                std::cmp::Reverse(agent.state_change_seq),
            )
        });
    }
    agents
        .into_iter()
        .map(|agent| agent.pane_id.clone())
        .collect()
}

pub(super) fn render_agent_panel(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    agent_tree_toggled: &mut HashSet<String>,
    hits: &mut ShellHitMap,
) {
    if !render_agent_panel_header(
        buffer,
        area,
        snapshot.agent_view_label.as_deref(),
        config,
        hits,
    ) {
        return;
    }

    // A parent that disappears from the snapshot (e.g. its pane closed) is
    // pruned from the toggled set on each render so it cannot leak forever.
    agent_tree_toggled.retain(|pane_id| {
        snapshot
            .agents
            .iter()
            .any(|agent| &agent.pane_id == pane_id)
    });

    let rows = if config.agents.tree {
        agent_tree_rows(snapshot, config, agent_tree_toggled)
    } else {
        agent_rows(snapshot, config, None)
    };
    render_agent_list(
        buffer,
        area,
        &rows,
        snapshot
            .agent_view_label
            .as_ref()
            .map(|_| " no matching agents"),
        config,
        agent_scroll,
        hits,
        |row| row.rows.len(),
        |buffer, rect, row, hits| {
            hits.agents.push((rect, row.pane_id.clone()));
            if row.has_children && rect.width > 0 && rect.height > 0 {
                // The arrow sits after the depth indent on line 0 (see
                // `render_agent_row`), so the toggle hit-rect must follow it
                // there too, clamped to stay inside the row.
                let arrow_offset =
                    (2u16.saturating_mul(row.depth)).min(rect.width.saturating_sub(1));
                hits.agent_toggles.push((
                    Rect::new(rect.x + arrow_offset, rect.y, 1, 1),
                    row.pane_id.clone(),
                ));
            }
            render_agent_row(buffer, rect, row, config);
        },
    );
}

fn agent_tree_rows(
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    toggled: &HashSet<String>,
) -> Vec<AgentRow> {
    let root_order = ordered_agent_pane_ids(snapshot, config.agent_panel_sort);
    // An active agent view (e.g. a review filter) restricts the whole panel
    // to `agent_order`; the tree must honor that same restriction instead of
    // letting an out-of-view parent leak back in as a root.
    let visible: Option<HashSet<&str>> = snapshot
        .agent_view_label
        .is_some()
        .then(|| root_order.iter().map(String::as_str).collect());
    let tree_agents = snapshot
        .agents
        .iter()
        .filter(|agent| {
            visible
                .as_ref()
                .is_none_or(|visible| visible.contains(agent.pane_id.as_str()))
        })
        .map(|agent| super::agent_tree::TreeAgent {
            pane_id: agent.pane_id.clone(),
            parent_pane_id: agent.parent_pane_id.clone(),
            status: agent.agent_status,
            state_change_seq: agent.state_change_seq,
        })
        .collect::<Vec<_>>();
    let tree_rows = super::agent_tree::build_rows(
        &tree_agents,
        &root_order,
        toggled,
        config.agents.default_expanded,
    );
    tree_rows
        .into_iter()
        .filter_map(|tree_row| {
            let mut row = agent_row(snapshot, &tree_row.pane_id, config, None)?;
            row.depth = tree_row.depth;
            row.has_children = tree_row.has_children;
            row.expanded = tree_row.expanded;
            row.rollup = tree_row.rollup;
            row.descendant_rollup = tree_row.descendant_rollup;
            Some(row)
        })
        .collect()
}

pub(super) fn render_agent_panel_header(
    buffer: &mut Buffer,
    area: Rect,
    agent_view_label: Option<&str>,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) -> bool {
    if area.height == 0 {
        return false;
    }
    put_text(
        buffer,
        area.x,
        area.y,
        area.width,
        &"─".repeat(area.width as usize),
        Style::default().fg(config.palette.surface_dim),
    );
    if area.height < 2 {
        return false;
    }
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        " agents",
        Style::default()
            .fg(config.palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );
    let sort_label = agent_view_label.unwrap_or(match config.agent_panel_sort {
        crate::config::AgentPanelSortConfig::Spaces => "grouped",
        crate::config::AgentPanelSortConfig::Priority => "priority",
    });
    let sort_width = display_width(sort_label).min(area.width as usize) as u16;
    let sort_rect = Rect::new(
        area.right().saturating_sub(sort_width),
        area.y + 1,
        sort_width,
        1,
    );
    hits.agent_sort_toggle = if config.mouse_capture && agent_view_label.is_none() {
        sort_rect
    } else {
        Rect::default()
    };
    put_text(
        buffer,
        sort_rect.x,
        sort_rect.y,
        sort_rect.width,
        sort_label,
        Style::default()
            .fg(if agent_view_label.is_some() {
                config.palette.accent
            } else {
                config.palette.overlay0
            })
            .add_modifier(Modifier::BOLD),
    );
    true
}

pub(super) fn render_agent_list<T>(
    buffer: &mut Buffer,
    area: Rect,
    rows: &[T],
    empty_message: Option<&str>,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
    row_lines: impl Fn(&T) -> usize,
    mut render_row: impl FnMut(&mut Buffer, Rect, &T, &mut ShellHitMap),
) {
    let body = Rect::new(
        area.x,
        area.y.saturating_add(3),
        area.width,
        area.height.saturating_sub(3),
    );
    hits.agent_body = body;
    if body.is_empty() || rows.is_empty() {
        *agent_scroll = 0;
        if let Some(message) = empty_message.filter(|_| !body.is_empty()) {
            put_text(
                buffer,
                body.x,
                body.y,
                body.width,
                message,
                Style::default()
                    .fg(config.palette.overlay0)
                    .add_modifier(Modifier::DIM),
            );
        }
        return;
    }

    let row_heights = rows
        .iter()
        .map(|row| row_lines(row).max(1).min(u16::MAX as usize) as u16)
        .collect::<Vec<_>>();
    let gaps = rows
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index + 1 < rows.len() {
                config.agents.row_gap
            } else {
                0
            }
        })
        .collect::<Vec<_>>();
    let metrics =
        super::scroll::list_scroll_metrics(&row_heights, &gaps, body.height, *agent_scroll);
    hits.agent_max_scroll = metrics.max_offset_from_bottom;
    hits.agent_scroll_metrics = Some(metrics);
    *agent_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut y = body.y;
    for (index, row) in rows.iter().enumerate().skip(*agent_scroll) {
        let height = row_heights[index].min(body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, height);
        render_row(buffer, rect, row, hits);
        y = y
            .saturating_add(height)
            .saturating_add(if index + 1 < rows.len() {
                config.agents.row_gap
            } else {
                0
            });
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.agent_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, &config.palette);
    }
}

pub(super) fn agent_rows(
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Vec<AgentRow> {
    ordered_agent_pane_ids(snapshot, config.agent_panel_sort)
        .into_iter()
        .filter_map(|pane_id| agent_row(snapshot, &pane_id, config, machine))
        .collect()
}

pub(super) fn agent_row(
    snapshot: &ClientShellSnapshot,
    pane_id: &str,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Option<AgentRow> {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.pane_id == pane_id)?;
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)?;
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == agent.pane_id);
    let tab_count = snapshot
        .tabs
        .iter()
        .filter(|candidate| candidate.workspace_id == agent.workspace_id)
        .count();
    let tab_label = tab
        .filter(|tab| tab_count > 1 || tab.custom_label)
        .map(|tab| tab.label.as_str());
    let agent_label = agent
        .display_agent
        .as_deref()
        .or(agent.name.as_deref())
        .or(agent.agent.as_deref())
        .or(agent.title.as_deref());
    let labels = agent
        .state_labels
        .iter()
        .cloned()
        .collect::<HashMap<_, _>>();
    let tokens = agent.tokens.iter().cloned().collect::<HashMap<_, _>>();
    let state_text = labels
        .get(status_text(agent.agent_status))
        .map(String::as_str)
        .unwrap_or_else(|| sidebar_status_text(agent.agent_status));
    let canonical_agent = agent
        .agent
        .as_deref()
        .and_then(crate::detect::parse_agent_label);
    let rows = crate::ui::sidebar_agent_rows(
        &config.agents,
        crate::ui::AgentTokenContext {
            machine,
            workspace: &workspace.label,
            tab: tab_label,
            pane: agent
                .title
                .as_deref()
                .or_else(|| pane.and_then(|pane| pane.label.as_deref())),
            agent_label,
            terminal_title: agent.terminal_title.as_deref(),
            terminal_title_stripped: agent.terminal_title_stripped.as_deref(),
            canonical_agent,
            tokens: &tokens,
        },
        state_text,
    );
    Some(AgentRow {
        pane_id: agent.pane_id.clone(),
        status: agent.agent_status,
        focused: agent.focused,
        rows,
        depth: 0,
        has_children: false,
        expanded: false,
        rollup: None,
        descendant_rollup: None,
        label: agent_label.map(str::to_string),
    })
}

/// `CD-` coordinator rows (e.g. `CD-S5-Cartographer`) show a live `-W/T`
/// count of Working-vs-total descendants instead of the default rollup
/// suffix, whether the row is expanded or collapsed.
fn cd_coordinator_suffix(row: &AgentRow) -> Option<String> {
    use crate::api::schema::AgentStatus;
    if !row.has_children {
        return None;
    }
    let label = row.label.as_deref()?;
    if !label.starts_with("CD-") {
        return None;
    }
    let rollup = row.descendant_rollup.as_ref()?;
    let working = rollup
        .counts
        .iter()
        .find(|(status, _)| *status == AgentStatus::Working)
        .map(|(_, count)| *count)
        .unwrap_or(0);
    Some(format!("-{working}/{}", rollup.total))
}

fn rollup_suffix_text(rollup: &super::agent_tree::Rollup) -> String {
    use crate::api::schema::AgentStatus;
    let mut text = format!(" {} agents", rollup.total);
    for (status, count) in &rollup.counts {
        if *status == AgentStatus::Idle || *status == AgentStatus::Unknown {
            continue;
        }
        text.push_str(&format!(" \u{b7} {count} {}", sidebar_status_text(*status)));
    }
    text
}

fn truncate_to_display_width(text: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width > max_width {
            break;
        }
        used += width;
        result.push(ch);
    }
    result
}

pub(super) fn render_agent_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: &AgentRow,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let row_style = if row.focused {
        Style::default().bg(palette.active_row_bg)
    } else {
        Style::default()
    };
    let name_style = if row.focused {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD)
    };
    let status_style = Style::default().fg(status_color(row.status, palette));
    let secondary = Style::default().fg(palette.overlay0);
    // A hidden blocked (or otherwise urgent) descendant must still surface
    // through the collapsed parent's icon, not the parent's own status.
    let icon_status = row
        .rollup
        .as_ref()
        .map_or(row.status, |rollup| rollup.most_urgent);
    let icon = (
        status_icon(icon_status, config.status_indicators),
        Style::default().fg(status_color(icon_status, palette)),
    );
    let rows = if row.rows.is_empty() {
        vec![vec![crate::ui::ResolvedToken {
            kind: crate::ui::ResolvedTokenKind::StateIcon,
            style: Default::default(),
        }]]
    } else {
        row.rows.clone()
    };
    let depth_indent = (2 * row.depth) as usize;
    // The `CD-` live-count suffix reads best stuck to the name itself. The
    // name is whichever configured row renders the `agent` token (by default
    // row 1, under the workspace/tab line); fall back to row 0 — the same
    // slot the default rollup suffix uses — if no row carries that token.
    let cd_suffix = cd_coordinator_suffix(row);
    let cd_suffix_line = cd_suffix.as_ref().and_then(|_| {
        rows.iter()
            .position(|tokens| {
                tokens
                    .iter()
                    .any(|token| matches!(token.kind, crate::ui::ResolvedTokenKind::Agent(_)))
            })
            .or(Some(0))
    });
    for (index, tokens) in rows.iter().take(rect.height as usize).enumerate() {
        let (prefix, indent) = if index == 0 {
            let arrow = if !row.has_children {
                " "
            } else if row.expanded {
                "\u{25be}"
            } else {
                "\u{25b8}"
            };
            (
                format!("{}{arrow}", " ".repeat(depth_indent)),
                1 + depth_indent,
            )
        } else {
            let indent = 3 + depth_indent;
            (" ".repeat(indent), indent)
        };
        let available = rect.width.saturating_sub(indent as u16) as usize;
        let suffix = if let Some(cd_text) = cd_suffix.as_ref() {
            (cd_suffix_line == Some(index)).then(|| cd_text.clone())
        } else {
            (index == 0).then_some(row.rollup.as_ref()).flatten().map(rollup_suffix_text)
        };
        let suffix_width = suffix.as_ref().map_or(0, |text| {
            unicode_width::UnicodeWidthStr::width(text.as_str()).min(available)
        });
        let suffix = suffix.map(|text| truncate_to_display_width(&text, suffix_width));
        let content_max_width = available.saturating_sub(suffix_width);
        let mut spans = vec![ratatui::text::Span::raw(prefix)];
        spans.extend(crate::ui::resolved_token_spans(
            tokens,
            icon,
            status_style,
            name_style,
            secondary,
            secondary,
            palette,
            content_max_width,
        ));
        if let Some(suffix) = suffix.filter(|suffix| !suffix.is_empty()) {
            spans.push(ratatui::text::Span::styled(suffix, secondary));
        }
        Paragraph::new(Line::from(spans)).style(row_style).render(
            Rect::new(rect.x, rect.y + index as u16, rect.width, 1),
            buffer,
        );
    }
}

fn put_text(buffer: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    for (offset, character) in text.chars().take(width as usize).enumerate() {
        if let Some(cell) = buffer.cell_mut((x + offset as u16, y)) {
            cell.set_char(character).set_style(style);
        }
    }
}

fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

fn sidebar_status_text(status: crate::api::schema::AgentStatus) -> &'static str {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Working => "working",
        AgentStatus::Idle | AgentStatus::Unknown => "idle",
    }
}
