//! tmux-style status line: a full-width row with left/right segments composed
//! of built-in `#{...}` tokens, scriptable command output, and interactive
//! widgets (menu button, clickable workspace list, agent rollup).
//!
//! Rendering is pure: [`build_statusline_content`] derives everything from
//! `&AppState` plus the terminal runtime registry (needed for live pane cwds,
//! which back auto-derived workspace names). It is called from BOTH
//! `compute_view` (to store hit rects in `ViewState.statusline_hits`) and
//! [`render_statusline`] (to draw) within the same frame, so hit geometry and
//! pixels can never diverge — do not let the two callers drift apart. Both
//! callers must pass the SAME registry. Space names come from [`space_labels`],
//! which mirrors the sidebar rather than deriving its own — see that function
//! before changing how a chip is named. Command segments read their cached output
//! (produced off the render path by the runtime refresh loop). See
//! `[ui.statusline]` config and `App::tick_statusline`.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

use crate::{
    app::state::{
        AppState, GlobalMenuAnchor, Mode, Palette, StatusSide, StatusWorkspaceHit,
        StatuslineHitAreas,
    },
    config::{StatusSegment, StatusStyle, StatusWidget},
    detect::AgentState,
    terminal::TerminalRuntimeRegistry,
};

use super::effects;
use super::sidebar::{agent_panel_entries, agent_panel_status_key};
use super::text::{display_width_u16, truncate_end};

/// Minimum name cells kept when the active workspace chip must be truncated.
const MIN_ACTIVE_NAME_CELLS: usize = 4;

/// Shared ramp for the static gradient chrome (gradient text segments and the
/// active-chip background): `start` through theme mauve to theme peach. Three
/// stops with full travel so the fade reads clearly even between adjacent
/// pastel accents; all palette tokens, so themes restyle it wholesale.
fn gradient_ramp(start: Color, p: &Palette) -> [Color; 3] {
    [start, p.mauve, p.peach]
}

/// Static blocked-glyph style: theme red. Upstream v0.8.0 removed the
/// animation tick, so attention states no longer pulse.
fn blocked_glyph_style(app: &AppState) -> Style {
    Style::default().fg(app.palette.red)
}

/// Static working-glyph color: theme yellow (no shimmer without the tick).
fn working_glyph_color(app: &AppState) -> Color {
    app.palette.yellow
}

/// What a rendered status item corresponds to, for hit-testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusItemKind {
    Plain,
    MenuButton,
    Workspace { ws_idx: usize },
}

/// One visual unit on the bar: a text segment, a widget button, or a single
/// workspace chip. `width` is the display width of `spans`.
pub(super) struct StatusItem {
    pub kind: StatusItemKind,
    pub spans: Vec<Span<'static>>,
    pub width: u16,
}

/// Everything needed to draw the bar and hit-test it.
#[derive(Default)]
pub(super) struct StatusLineContent {
    pub left: Vec<StatusItem>,
    pub right: Vec<StatusItem>,
    /// Left-side draw area; the left run is clipped to it.
    pub left_area: Rect,
    /// Right-side draw area, anchored to the right edge of the bar.
    pub right_area: Rect,
    pub hits: StatuslineHitAreas,
}

pub(super) fn render_statusline(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let palette = &app.palette;
    // Fill the row with the bar background first; item spans merge over it.
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.surface0).fg(palette.subtext0)),
        area,
    );

    let content = build_statusline_content(app, terminal_runtimes, area);
    if !content.left.is_empty() && content.left_area.width > 0 {
        let spans: Vec<Span<'static>> = content
            .left
            .into_iter()
            .flat_map(|item| item.spans)
            .collect();
        frame.render_widget(Paragraph::new(Line::from(spans)), content.left_area);
    }
    if !content.right.is_empty() && content.right_area.width > 0 {
        let spans: Vec<Span<'static>> = content
            .right
            .into_iter()
            .flat_map(|item| item.spans)
            .collect();
        frame.render_widget(Paragraph::new(Line::from(spans)), content.right_area);
    }
}

/// Derive the bar's items, draw areas, and hit rects from pure state.
///
/// Layout policy: the right side is built first at natural width (dropping
/// whole items from the FRONT if it alone overflows, so the rightmost items
/// survive); the left side gets the remaining budget minus a 1-cell gap, with
/// the workspace chip run reflowed to fit — the active chip is always emitted,
/// truncating its name before other placed chips are dropped.
pub(super) fn build_statusline_content(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
) -> StatusLineContent {
    let mut hits = StatuslineHitAreas::default();
    if area.width == 0 || area.height == 0 {
        // No bar, no hits, no widget flags: bar behavior is fully off.
        return StatusLineContent::default();
    }

    // Resolved once per frame: each label costs a per-pane cwd lookup plus a
    // grouping pass, and every chip/token below reads the same answer.
    let labels = space_labels(app, terminal_runtimes);

    let mut right = build_side_items(app, &labels, StatusSide::Right, &mut hits);
    while right.len() > 1 && item_total_width(&right) > area.width {
        right.remove(0);
    }
    let right_total = item_total_width(&right).min(area.width);

    let mut left = build_side_items(app, &labels, StatusSide::Left, &mut hits);
    let gap = if right_total > 0 { 1 } else { 0 };
    let left_budget = area.width.saturating_sub(right_total + gap);
    if item_total_width(&left) > left_budget {
        reflow_workspace_chips(app, &labels, &mut left, left_budget);
    }

    let left_area = Rect::new(area.x, area.y, left_budget, 1);
    let right_area = Rect::new(area.x + (area.width - right_total), area.y, right_total, 1);
    collect_hits(&left, left_area, &mut hits);
    collect_hits(&right, right_area, &mut hits);

    StatusLineContent {
        left,
        right,
        left_area,
        right_area,
        hits,
    }
}

/// Space names exactly as the sidebar renders them, indexed by workspace index.
///
/// The bar must never invent its own naming: it mirrors
/// `render_workspace_cards` in [`super::sidebar`] (and the mobile switcher),
/// which means the live root-pane cwd label, and for a grouped worktree child
/// its branch instead. Grouping comes from the EXPANDED entry list on purpose —
/// a collapsed group hides children from the sidebar but the bar still draws a
/// chip for every space, and a chip must not change its name just because a
/// group got folded.
fn space_labels(app: &AppState, terminal_runtimes: &TerminalRuntimeRegistry) -> Vec<String> {
    let mut indented = vec![false; app.workspaces.len()];
    for entry in super::sidebar::workspace_list_entries_expanded(app) {
        let super::sidebar::WorkspaceListEntry::Workspace {
            ws_idx,
            indented: is_child,
        } = entry;
        if let Some(slot) = indented.get_mut(ws_idx) {
            *slot = is_child;
        }
    }

    app.workspaces
        .iter()
        .zip(indented)
        .map(|(ws, is_child)| {
            let label = ws.display_name_from(&app.terminals, terminal_runtimes);
            if is_child {
                super::sidebar::grouped_child_display_label(
                    &label,
                    ws.branch().as_deref(),
                    ws.custom_name.is_some(),
                )
            } else {
                label
            }
        })
        .collect()
}

fn space_label_at(labels: &[String], ws_idx: usize) -> &str {
    labels.get(ws_idx).map(String::as_str).unwrap_or_default()
}

fn item_total_width(items: &[StatusItem]) -> u16 {
    items
        .iter()
        .map(|item| item.width)
        .fold(0u16, u16::saturating_add)
}

fn item_from_spans(kind: StatusItemKind, spans: Vec<Span<'static>>) -> StatusItem {
    let width = spans
        .iter()
        .map(|span| display_width_u16(&span.content))
        .fold(0u16, u16::saturating_add);
    StatusItem { kind, spans, width }
}

/// Expand one side's config segments into items. Command output stays keyed by
/// the segment's enumerate index over the FULL side Vec — the same indexing
/// `App::statusline_command_jobs` uses; keep them in lockstep.
fn build_side_items(
    app: &AppState,
    labels: &[String],
    side: StatusSide,
    hits: &mut StatuslineHitAreas,
) -> Vec<StatusItem> {
    let segments = match side {
        StatusSide::Left => &app.statusline.left,
        StatusSide::Right => &app.statusline.right,
    };
    let mut items = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        match segment {
            StatusSegment::Widget { widget } => match widget {
                StatusWidget::Menu => items.extend(menu_item(app)),
                StatusWidget::Workspaces => {
                    hits.has_workspaces_widget = true;
                    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
                        items.push(workspace_chip(app, labels, ws_idx, ws, None));
                    }
                }
                StatusWidget::Agents => items.extend(agents_item(app)),
                StatusWidget::Mode => items.extend(mode_item(app)),
            },
            StatusSegment::Text(_)
            | StatusSegment::Styled { .. }
            | StatusSegment::Command { .. } => {
                let text = match segment {
                    StatusSegment::Command { .. } => app
                        .statusline
                        .command_outputs
                        .get(&(side, index))
                        .cloned()
                        .unwrap_or_default(),
                    StatusSegment::Text(raw) => resolve_tokens(app, labels, raw),
                    StatusSegment::Styled { text, .. } => resolve_tokens(app, labels, text),
                    // Handled by the outer match arm.
                    StatusSegment::Widget { .. } => String::new(),
                };
                if text.is_empty() {
                    continue;
                }
                let style = segment_span_style(segment, &app.palette);
                let spans = if segment.style() == StatusStyle::Gradient {
                    gradient_segment_spans(segment, &text, style, &app.palette)
                } else {
                    vec![Span::styled(text, style)]
                };
                items.push(item_from_spans(StatusItemKind::Plain, spans));
            }
        }
    }
    items
}

/// Walk items left-to-right from `area.x`, recording hit rects clipped to
/// `area`. Items clipped to zero width get no hit (not clickable).
fn collect_hits(items: &[StatusItem], area: Rect, hits: &mut StatuslineHitAreas) {
    let right_edge = area.x.saturating_add(area.width);
    let mut x = area.x;
    for item in items {
        if x >= right_edge {
            break;
        }
        let end = x.saturating_add(item.width).min(right_edge);
        let width = end.saturating_sub(x);
        if width > 0 {
            let rect = Rect::new(x, area.y, width, 1);
            match item.kind {
                StatusItemKind::MenuButton => hits.menu_button = rect,
                StatusItemKind::Workspace { ws_idx } => hits
                    .workspace_entries
                    .push(StatusWorkspaceHit { ws_idx, rect }),
                StatusItemKind::Plain => {}
            }
        }
        x = x.saturating_add(item.width);
    }
}

// ----- Widgets ----------------------------------------------------------------

/// The `☰` menu button. Hidden (and inert) when mouse support is off, exactly
/// like the sidebar launcher; inverted while its menu is open.
fn menu_item(app: &AppState) -> Option<StatusItem> {
    if !app.mouse_capture {
        return None;
    }
    let p = &app.palette;
    let open_here =
        app.mode == Mode::GlobalMenu && app.global_menu_anchor == GlobalMenuAnchor::Statusline;
    let base = if open_here {
        Style::default()
            .fg(super::widgets::panel_contrast_fg(p))
            .bg(p.accent)
    } else {
        Style::default().fg(p.overlay0)
    };
    let spans = if app.global_menu_attention_badge_visible() {
        let badge = if open_here {
            base.add_modifier(Modifier::BOLD)
        } else {
            // The badge exists to be noticed: a static accent mark (upstream
            // v0.8.0 removed the animation tick, so it no longer pulses).
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
        };
        vec![Span::styled(" ● ", badge), Span::styled("☰ ", base)]
    } else {
        vec![Span::styled(" ☰ ", base)]
    };
    Some(item_from_spans(StatusItemKind::MenuButton, spans))
}

/// One workspace chip: `" {glyph} {n}:{name} "`. The glyph is always exactly
/// one cell so chip widths stay stable when agent state flips. The active
/// workspace is inverted onto the theme accent (tab-bar convention).
fn workspace_chip(
    app: &AppState,
    labels: &[String],
    ws_idx: usize,
    ws: &crate::workspace::Workspace,
    name_budget: Option<usize>,
) -> StatusItem {
    let p = &app.palette;
    let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);
    let (glyph, state_glyph_style) = match (agg_state, agg_seen) {
        (AgentState::Blocked, _) => ("◉", blocked_glyph_style(app)),
        (AgentState::Working, _) => ("●", Style::default().fg(working_glyph_color(app))),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("○", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("·", Style::default().fg(p.overlay0)),
    };

    let mut name = space_label_at(labels, ws_idx).to_string();
    if let Some(budget) = name_budget {
        name = truncate_end(&name, budget.max(MIN_ACTIVE_NAME_CELLS));
    }
    let number = format!("{}:", ws_idx + 1);

    let active = app.active == Some(ws_idx);
    let (pad_style, glyph_style, number_style, name_style) = if active {
        let style = Style::default()
            .fg(super::widgets::panel_contrast_fg(p))
            .bg(p.accent);
        let name_style = style.add_modifier(Modifier::BOLD);
        (style, style, style, name_style)
    } else {
        (
            Style::default().fg(p.subtext0),
            state_glyph_style,
            Style::default().fg(p.subtext0),
            Style::default().fg(state_label_color_for(agg_state, agg_seen, p)),
        )
    };

    let mut spans = vec![
        Span::styled(" ", pad_style),
        Span::styled(glyph.to_string(), glyph_style),
        Span::styled(" ", pad_style),
        Span::styled(number, number_style),
        Span::styled(name, name_style),
        Span::styled(" ", pad_style),
    ];
    if active {
        spans = active_chip_gradient(spans, p);
    }
    item_from_spans(StatusItemKind::Workspace { ws_idx }, spans)
}

/// Sweep the active chip's background across the gradient ramp (accent through
/// mauve to peach), left to right, so the chip reads as a polished pill instead
/// of a flat block. Per-character recolor only — total width is untouched, so
/// hit rects stay valid. No-op on themes without RGB colors.
fn active_chip_gradient(spans: Vec<Span<'static>>, p: &Palette) -> Vec<Span<'static>> {
    let ramp = gradient_ramp(p.accent, p);
    if ramp.iter().any(|stop| !effects::is_rgb(*stop)) {
        return spans;
    }
    let total: u16 = spans
        .iter()
        .map(|span| display_width_u16(&span.content))
        .fold(0u16, u16::saturating_add);
    if total <= 1 {
        return spans;
    }
    let denom = f32::from(total - 1);
    let mut x: u16 = 0;
    let mut out = Vec::new();
    for span in spans {
        let style = span.style;
        for c in span.content.chars() {
            let t = f32::from(x) / denom;
            out.push(Span::styled(
                c.to_string(),
                style.bg(effects::lerp_stops(&ramp, t)),
            ));
            x = x.saturating_add(display_width_u16(&c.to_string()));
        }
    }
    out
}

fn state_label_color_for(state: AgentState, seen: bool, p: &Palette) -> ratatui::style::Color {
    super::status::state_label_color(state, seen, p)
}

/// Reflow the (contiguous) workspace chip run on the left side into whatever
/// budget remains after the fixed items. The active chip is always emitted.
fn reflow_workspace_chips(
    app: &AppState,
    labels: &[String],
    left: &mut Vec<StatusItem>,
    left_budget: u16,
) {
    let Some(run_start) = left
        .iter()
        .position(|item| matches!(item.kind, StatusItemKind::Workspace { .. }))
    else {
        return;
    };
    let run_len = left[run_start..]
        .iter()
        .take_while(|item| matches!(item.kind, StatusItemKind::Workspace { .. }))
        .count();
    let fixed: u16 = left
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < run_start || *i >= run_start + run_len)
        .map(|(_, item)| item.width)
        .fold(0u16, u16::saturating_add);
    let ws_budget = left_budget.saturating_sub(fixed);
    let fitted = fit_workspace_chips(app, labels, ws_budget);
    left.splice(run_start..run_start + run_len, fitted);
}

/// Fit workspace chips into `ws_budget` cells: greedy in display order, with
/// the active chip reserved up-front (name-truncated if necessary) so earlier
/// chips can never starve it. A dim `…` marks dropped chips.
fn fit_workspace_chips(app: &AppState, labels: &[String], ws_budget: u16) -> Vec<StatusItem> {
    let natural: Vec<StatusItem> = app
        .workspaces
        .iter()
        .enumerate()
        .map(|(ws_idx, ws)| workspace_chip(app, labels, ws_idx, ws, None))
        .collect();
    if item_total_width(&natural) <= ws_budget {
        return natural;
    }

    // Reserve one cell for the trailing "…" overflow marker.
    let mut remaining = ws_budget.saturating_sub(1);
    let active_idx = app.active.filter(|idx| *idx < natural.len());
    let mut active_chip = active_idx.map(|ws_idx| {
        let ws = &app.workspaces[ws_idx];
        if natural[ws_idx].width <= remaining {
            workspace_chip(app, labels, ws_idx, ws, None)
        } else {
            let name_width = display_width_u16(space_label_at(labels, ws_idx));
            let frame_width = natural[ws_idx].width.saturating_sub(name_width);
            let name_budget = usize::from(remaining.saturating_sub(frame_width));
            workspace_chip(app, labels, ws_idx, ws, Some(name_budget))
        }
    });
    let mut reserve = active_chip.as_ref().map(|chip| chip.width).unwrap_or(0);

    let mut out = Vec::new();
    let mut dropped = false;
    for (ws_idx, chip) in natural.into_iter().enumerate() {
        if Some(ws_idx) == active_idx {
            if let Some(chip) = active_chip.take() {
                remaining = remaining.saturating_sub(chip.width);
                reserve = 0;
                out.push(chip);
            }
        } else if chip.width <= remaining.saturating_sub(reserve) {
            remaining = remaining.saturating_sub(chip.width);
            out.push(chip);
        } else {
            dropped = true;
        }
    }
    if dropped {
        out.push(item_from_spans(
            StatusItemKind::Plain,
            vec![Span::styled("…", Style::default().fg(app.palette.overlay0))],
        ));
    }
    out
}

/// Agent rollup: blocked/working/done/idle glyph+count pairs in the sidebar's
/// visual language; zero-count buckets hidden; `Unknown` omitted (matching the
/// `#{agents_*}` token semantics). `None` when there is nothing to show.
fn agents_item(app: &AppState) -> Option<StatusItem> {
    let p = &app.palette;
    let entries = agent_panel_entries(app);
    // Representative (state, seen) pairs for each bucket's glyph.
    const BUCKETS: [(&str, AgentState, bool); 4] = [
        ("blocked", AgentState::Blocked, true),
        ("working", AgentState::Working, true),
        ("done", AgentState::Idle, false),
        ("idle", AgentState::Idle, true),
    ];
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (key, state, seen) in BUCKETS {
        let count = entries
            .iter()
            .filter(|entry| agent_panel_status_key(entry.state, entry.seen) == key)
            .count();
        if count == 0 {
            continue;
        }
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        let (mut glyph, mut style) = super::status::state_dot(state, seen, p);
        // Blocked/working take their bucket's static color so the rollup reads
        // at a glance; other buckets keep the state_dot styling. The blocked
        // ring mirrors the workspace-chip glyph.
        match key {
            "blocked" => {
                glyph = "◉";
                style = blocked_glyph_style(app);
            }
            "working" => style = style.fg(working_glyph_color(app)),
            _ => {}
        }
        spans.push(Span::styled(glyph.to_string(), style));
        spans.push(Span::styled(
            format!(" {count}"),
            Style::default().fg(super::status::state_label_color(state, seen, p)),
        ));
    }
    if spans.is_empty() {
        return None;
    }
    Some(item_from_spans(StatusItemKind::Plain, spans))
}

/// Key-mode chip: ` PREFIX `, ` COPY `, ` RESIZE `, or ` NAV `, each inverted
/// onto its own theme color so the active key mode is unmissable. Hidden (and
/// zero-width) outside those modes.
fn mode_item(app: &AppState) -> Option<StatusItem> {
    let label = mode_label(app.mode);
    if label.is_empty() {
        return None;
    }
    let p = &app.palette;
    let bg = match app.mode {
        Mode::Prefix => p.accent,
        Mode::Copy => p.yellow,
        Mode::Resize => p.peach,
        Mode::Navigate => p.teal,
        // mode_label is non-empty only for the four modes above.
        _ => p.accent,
    };
    let style = Style::default()
        .fg(super::widgets::panel_contrast_fg(p))
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    Some(item_from_spans(
        StatusItemKind::Plain,
        vec![Span::styled(format!(" {label} "), style)],
    ))
}

// ----- Segment styling ---------------------------------------------------------

fn segment_style(style: StatusStyle, palette: &Palette) -> Style {
    match style {
        StatusStyle::Normal => Style::default().fg(palette.subtext0),
        // Gradient's per-character colors are applied in
        // `gradient_segment_spans`; the accent is its non-RGB fallback.
        StatusStyle::Accent | StatusStyle::Gradient => Style::default().fg(palette.accent),
        StatusStyle::Dim => Style::default().fg(palette.overlay0),
        StatusStyle::Bold => Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD),
    }
}

/// Spans for a `style = "gradient"` segment: a per-character fade from the
/// segment's `fg` override (or the theme accent) through the theme mauve to
/// the theme peach. The resolved base style keeps any `bg` override and
/// modifiers.
fn gradient_segment_spans(
    segment: &StatusSegment,
    text: &str,
    base: Style,
    palette: &Palette,
) -> Vec<Span<'static>> {
    let from = segment
        .color_overrides()
        .0
        .and_then(|spec| resolve_status_color(spec, palette))
        .unwrap_or(palette.accent);
    effects::gradient_spans(text, &gradient_ramp(from, palette), base)
}

/// Resolve a user color spec: palette tokens win over ANSI color names so a
/// themed bar stays themed (`"red"` is the theme red; use `"#ff0000"` for raw
/// RGB). Unknown specs resolve to `None` — warned once at config load, never
/// on the render path.
fn resolve_status_color(spec: &str, palette: &Palette) -> Option<ratatui::style::Color> {
    palette
        .color_token(spec)
        .or_else(|| crate::config::parse_color_opt(spec))
}

/// The style preset, with any per-segment `fg`/`bg` overrides applied on top.
fn segment_span_style(segment: &StatusSegment, palette: &Palette) -> Style {
    let mut style = segment_style(segment.style(), palette);
    let (fg, bg) = segment.color_overrides();
    if let Some(color) = fg.and_then(|spec| resolve_status_color(spec, palette)) {
        style = style.fg(color);
    }
    if let Some(color) = bg.and_then(|spec| resolve_status_color(spec, palette)) {
        style = style.bg(color);
    }
    style
}

// ----- Tokens -------------------------------------------------------------------

/// Substitute every `#{token}` in `raw`. Unrecognized tokens are left verbatim
/// (including the braces) so typos are visible rather than silently dropped.
fn resolve_tokens(app: &AppState, labels: &[String], raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("#{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                out.push_str(&resolve_token(app, labels, &after[..end]));
                rest = &after[end + 1..];
            }
            None => {
                // Unterminated token: emit the remainder literally.
                out.push_str(&rest[start..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn resolve_token(app: &AppState, labels: &[String], token: &str) -> String {
    match token {
        "session" => app.statusline.session_name.clone(),
        "workspace" => active_workspace_label(app, labels).unwrap_or_default(),
        "tab" => active_tab_label(app).unwrap_or_default(),
        "pane_index" => pane_index(app).map(|i| i.to_string()).unwrap_or_default(),
        "pane_count" => pane_count(app).to_string(),
        "mode" => mode_label(app.mode).to_string(),
        "agents_blocked" => agent_count(app, "blocked").to_string(),
        "agents_working" => agent_count(app, "working").to_string(),
        "agents_done" => agent_count(app, "done").to_string(),
        "agents_idle" => agent_count(app, "idle").to_string(),
        "agents_total" => agent_panel_entries(app).len().to_string(),
        "time" => format_time(&now_parts(), "%H:%M"),
        other => {
            if let Some(fmt) = other.strip_prefix("time:") {
                format_time(&now_parts(), fmt)
            } else {
                format!("#{{{other}}}")
            }
        }
    }
}

fn active_workspace_label(app: &AppState, labels: &[String]) -> Option<String> {
    labels.get(app.active?).cloned()
}

fn active_tab_label(app: &AppState) -> Option<String> {
    let ws = app.active.and_then(|i| app.workspaces.get(i))?;
    ws.active_tab_display_name()
}

fn pane_count(app: &AppState) -> usize {
    app.active
        .and_then(|i| app.workspaces.get(i))
        .and_then(|ws| ws.active_tab())
        .map(|tab| tab.layout.pane_count())
        .unwrap_or(0)
}

fn pane_index(app: &AppState) -> Option<usize> {
    let ws = app.active.and_then(|i| app.workspaces.get(i))?;
    let focused = ws.focused_pane_id()?;
    let tab = ws.active_tab()?;
    let position = tab
        .layout
        .pane_ids()
        .into_iter()
        .position(|id| id == focused)?;
    Some(position + 1)
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Prefix => "PREFIX",
        Mode::Copy => "COPY",
        Mode::Resize => "RESIZE",
        Mode::Navigate => "NAV",
        _ => "",
    }
}

fn agent_count(app: &AppState, key: &str) -> usize {
    agent_panel_entries(app)
        .iter()
        .filter(|entry| agent_panel_status_key(entry.state, entry.seen) == key)
        .count()
}

// ----- Clock -----------------------------------------------------------------

struct TimeParts {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
    /// Days from Sunday (0) to Saturday (6).
    wday: u32,
}

fn now_parts() -> TimeParts {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    #[cfg(unix)]
    {
        if let Some(parts) = local_parts_unix(secs) {
            return parts;
        }
    }
    utc_parts(secs)
}

#[cfg(unix)]
fn local_parts_unix(secs: i64) -> Option<TimeParts> {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `tm` is a valid, zeroed destination; `localtime_r` writes into it
    // and returns null only on failure.
    let result = unsafe { libc::localtime_r(&t, &mut tm) };
    if result.is_null() {
        return None;
    }
    Some(TimeParts {
        year: tm.tm_year as i64 + 1900,
        month: tm.tm_mon as u32 + 1,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        min: tm.tm_min as u32,
        sec: tm.tm_sec as u32,
        wday: tm.tm_wday as u32,
    })
}

/// Break a Unix timestamp into UTC calendar parts. Used on non-Unix targets and
/// as a fallback when `localtime_r` fails. Civil-from-days per Howard Hinnant.
fn utc_parts(secs: i64) -> TimeParts {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hour = (rem / 3600) as u32;
    let min = ((rem % 3600) / 60) as u32;
    let sec = (rem % 60) as u32;
    // 1970-01-01 was a Thursday (wday 4).
    let wday = ((days.rem_euclid(7)) as u32 + 4) % 7;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = if month <= 2 { y + 1 } else { y };

    TimeParts {
        year,
        month,
        day,
        hour,
        min,
        sec,
        wday,
    }
}

const WDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A small strftime subset covering the specifiers useful in a status line.
fn format_time(parts: &TimeParts, fmt: &str) -> String {
    let mut out = String::with_capacity(fmt.len());
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('H') => out.push_str(&format!("{:02}", parts.hour)),
            Some('M') => out.push_str(&format!("{:02}", parts.min)),
            Some('S') => out.push_str(&format!("{:02}", parts.sec)),
            Some('I') => {
                let h12 = ((parts.hour + 11) % 12) + 1;
                out.push_str(&format!("{h12:02}"));
            }
            Some('p') => out.push_str(if parts.hour < 12 { "AM" } else { "PM" }),
            Some('d') => out.push_str(&format!("{:02}", parts.day)),
            Some('m') => out.push_str(&format!("{:02}", parts.month)),
            Some('Y') => out.push_str(&parts.year.to_string()),
            Some('y') => out.push_str(&format!("{:02}", (parts.year % 100).unsigned_abs())),
            Some('a') => out.push_str(WDAYS[(parts.wday as usize) % 7]),
            Some('b') => out.push_str(MONTHS[(parts.month as usize).saturating_sub(1) % 12]),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StatusLinePosition;
    use crate::terminal::TerminalState;
    use crate::workspace::Workspace;

    fn test_area(width: u16) -> Rect {
        Rect::new(0, 30, width, 1)
    }

    /// Build the bar without live PTY runtimes. Workspace names then resolve
    /// from `AppState::terminals` — the cwd the runtime loop keeps fresh — which
    /// is exactly what these tests want to assert on.
    fn build_content(app: &AppState, area: Rect) -> StatusLineContent {
        build_statusline_content(app, &TerminalRuntimeRegistry::new(), area)
    }

    fn resolve(app: &AppState, raw: &str) -> String {
        let labels = space_labels(app, &TerminalRuntimeRegistry::new());
        resolve_tokens(app, &labels, raw)
    }

    /// The space name the sidebar draws for `ws_idx`, derived independently of
    /// the bar so a test failure means the two surfaces actually disagree.
    fn sidebar_label(app: &AppState, ws_idx: usize) -> String {
        let runtimes = TerminalRuntimeRegistry::new();
        let indented = super::super::sidebar::workspace_list_entries_expanded(app)
            .into_iter()
            .any(
                |super::super::sidebar::WorkspaceListEntry::Workspace {
                     ws_idx: i,
                     indented,
                 }| { i == ws_idx && indented },
            );
        let ws = &app.workspaces[ws_idx];
        let label = ws.display_name_from(&app.terminals, &runtimes);
        if indented {
            super::super::sidebar::grouped_child_display_label(
                &label,
                ws.branch().as_deref(),
                ws.custom_name.is_some(),
            )
        } else {
            label
        }
    }

    /// Push a workspace whose root pane's terminal runs a detected agent in
    /// `state` with `seen` (the agent panel only lists panes with an agent).
    fn push_workspace(app: &mut AppState, name: &str, state: AgentState, seen: bool) {
        let mut ws = Workspace::test_new(name);
        let root = ws.tabs[0].root_pane;
        let terminal_id = ws
            .terminal_id(root)
            .expect("test workspace root pane has a terminal")
            .clone();
        let mut terminal = TerminalState::new(terminal_id.clone(), "/tmp".into());
        terminal.state = state;
        terminal.agent_name = Some("agent".into());
        app.terminals.insert(terminal_id, terminal);
        if let Some(pane) = ws.tabs[0].panes.get_mut(&root) {
            pane.seen = seen;
        }
        app.workspaces.push(ws);
    }

    fn flat_text(items: &[StatusItem]) -> String {
        items
            .iter()
            .flat_map(|item| item.spans.iter())
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn resolve_tokens_substitutes_session_and_leaves_unknown() {
        let mut app = AppState::test_new();
        app.statusline.session_name = "work".into();
        assert_eq!(resolve(&app, "#{session}"), "work");
        assert_eq!(resolve(&app, "[#{session}]"), "[work]");
        assert_eq!(resolve(&app, "#{bogus}"), "#{bogus}");
    }

    #[test]
    fn resolve_tokens_handles_unterminated_and_plain_text() {
        let app = AppState::test_new();
        assert_eq!(resolve(&app, "plain text"), "plain text");
        assert_eq!(resolve(&app, "a #{unterminated"), "a #{unterminated");
    }

    #[test]
    fn mode_label_maps_prefix_and_terminal() {
        assert_eq!(mode_label(Mode::Prefix), "PREFIX");
        assert_eq!(mode_label(Mode::Terminal), "");
    }

    #[test]
    fn format_time_supports_common_specifiers() {
        // 2021-01-02 03:04:05 UTC = 1609556645.
        let parts = utc_parts(1_609_556_645);
        assert_eq!(parts.year, 2021);
        assert_eq!(parts.month, 1);
        assert_eq!(parts.day, 2);
        assert_eq!(format_time(&parts, "%H:%M"), "03:04");
        assert_eq!(format_time(&parts, "%Y-%m-%d"), "2021-01-02");
        assert_eq!(format_time(&parts, "%I%p"), "03AM");
        assert_eq!(format_time(&parts, "100%%"), "100%");
    }

    #[test]
    fn utc_parts_weekday_is_correct() {
        // 1970-01-01 was a Thursday.
        assert_eq!(utc_parts(0).wday, 4);
        assert_eq!(format_time(&utc_parts(0), "%a"), "Thu");
    }

    #[test]
    fn resolve_status_color_prefers_palette_tokens_over_ansi() {
        let palette = Palette::catppuccin();
        assert_eq!(
            resolve_status_color("accent", &palette),
            Some(palette.accent)
        );
        assert_eq!(
            resolve_status_color("red", &palette),
            Some(palette.red),
            "palette token shadows the ANSI name"
        );
        assert_eq!(
            resolve_status_color("#ff0000", &palette),
            Some(ratatui::style::Color::Rgb(255, 0, 0))
        );
        assert_eq!(resolve_status_color("bogus", &palette), None);
    }

    #[test]
    fn segment_span_style_presets_unchanged_without_overrides() {
        let palette = Palette::catppuccin();
        let segment = StatusSegment::Styled {
            text: "x".into(),
            style: StatusStyle::Accent,
            fg: None,
            bg: None,
        };
        assert_eq!(
            segment_span_style(&segment, &palette),
            segment_style(StatusStyle::Accent, &palette)
        );
    }

    #[test]
    fn segment_span_style_applies_fg_bg_overrides() {
        let palette = Palette::catppuccin();
        let segment = StatusSegment::Styled {
            text: "x".into(),
            style: StatusStyle::Bold,
            fg: Some("mauve".into()),
            bg: Some("#102030".into()),
        };
        let style = segment_span_style(&segment, &palette);
        assert_eq!(style.fg, Some(palette.mauve));
        assert_eq!(style.bg, Some(ratatui::style::Color::Rgb(0x10, 0x20, 0x30)));
        // Bold preset survives the color override.
        assert!(style.add_modifier.contains(Modifier::BOLD));

        // Invalid specs keep the preset color untouched.
        let segment = StatusSegment::Styled {
            text: "x".into(),
            style: StatusStyle::Accent,
            fg: Some("bogus".into()),
            bg: None,
        };
        assert_eq!(
            segment_span_style(&segment, &palette).fg,
            Some(palette.accent)
        );
    }

    #[test]
    fn command_output_indexing_survives_interleaved_widgets() {
        let mut app = AppState::test_new();
        app.mouse_capture = true;
        app.statusline.enabled = true;
        app.statusline.left = vec![
            StatusSegment::Widget {
                widget: StatusWidget::Menu,
            },
            StatusSegment::Command {
                command: vec!["true".into()],
                style: StatusStyle::Normal,
                fg: None,
                bg: None,
            },
            StatusSegment::Text("mid".into()),
            StatusSegment::Command {
                command: vec!["true".into()],
                style: StatusStyle::Normal,
                fg: None,
                bg: None,
            },
        ];
        // Outputs keyed by the enumerate index over the FULL side Vec —
        // widgets occupy indices without shifting command keys.
        app.statusline
            .command_outputs
            .insert((StatusSide::Left, 1), "first-cmd".into());
        app.statusline
            .command_outputs
            .insert((StatusSide::Left, 3), "second-cmd".into());

        let content = build_content(&app, test_area(120));
        let text = flat_text(&content.left);
        assert!(text.contains("first-cmd"), "text: {text}");
        assert!(text.contains("mid"), "text: {text}");
        assert!(text.contains("second-cmd"), "text: {text}");
    }

    /// `identity_cwd` is frozen at workspace creation, so an auto-named space
    /// whose pane has since `cd`-ed must be labelled from the live pane cwd —
    /// the same source the sidebar and navigator read. Covers both the chip
    /// widget and the `#{workspace}` token.
    #[test]
    fn workspace_label_tracks_live_pane_cwd_not_frozen_identity_cwd() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![
            StatusSegment::Widget {
                widget: StatusWidget::Workspaces,
            },
            StatusSegment::Text(" #{workspace}".into()),
        ];

        let mut ws = Workspace::test_new("ignored");
        // Auto-named: no custom name, so the label derives from cwd.
        ws.custom_name = None;
        ws.identity_cwd = "/projects/stale".into();
        let root = ws.tabs[0].root_pane;
        let terminal_id = ws
            .terminal_id(root)
            .expect("test workspace root pane has a terminal")
            .clone();
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        app.terminals
            .get_mut(&terminal_id)
            .expect("root pane terminal exists")
            .cwd = "/projects/live".into();
        app.active = Some(0);

        let content = build_content(&app, test_area(80));
        let text = flat_text(&content.left);
        assert!(text.contains("1:live"), "chip text: {text}");
        assert!(
            !text.contains("stale"),
            "stale identity_cwd leaked into the bar: {text}"
        );
        let labels = space_labels(&app, &TerminalRuntimeRegistry::new());
        assert_eq!(
            active_workspace_label(&app, &labels).as_deref(),
            Some("live")
        );
        assert_eq!(labels[0], sidebar_label(&app, 0));
    }

    /// A grouped worktree child is named after its BRANCH in the sidebar, not
    /// its checkout directory. The bar must show the same thing — this is the
    /// case that diverged most visibly, since worktree spaces are usually
    /// auto-named and their branch changes under them.
    #[test]
    fn grouped_worktree_child_chip_matches_sidebar_branch_label() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Widget {
            widget: StatusWidget::Workspaces,
        }];

        let mut parent = Workspace::test_new("parent");
        parent.custom_name = None;
        parent.identity_cwd = "/repo/herdr".into();
        parent.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });

        let mut child = Workspace::test_new("child");
        child.custom_name = None;
        child.identity_cwd = "/repo/herdr-wt".into();
        child.cached_git_branch = Some("worktree/fix-statusline".into());
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-wt".into(),
            is_linked_worktree: true,
        });

        app.workspaces = vec![parent, child];
        app.ensure_test_terminals();
        app.active = Some(0);

        let labels = space_labels(&app, &TerminalRuntimeRegistry::new());
        // Branch wins for the child, with the `worktree/` prefix stripped.
        assert_eq!(labels[1], "fix-statusline");
        // ...and that is exactly what the sidebar draws.
        assert_eq!(labels[0], sidebar_label(&app, 0));
        assert_eq!(labels[1], sidebar_label(&app, 1));

        let text = flat_text(&build_content(&app, test_area(80)).left);
        assert!(text.contains("2:fix-statusline"), "chip text: {text}");
    }

    /// Collapsing a group hides children from the sidebar but the bar still
    /// draws every chip; a chip must not rename itself when a group folds.
    #[test]
    fn collapsed_group_does_not_change_child_chip_label() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Widget {
            widget: StatusWidget::Workspaces,
        }];

        let mut parent = Workspace::test_new("parent");
        parent.custom_name = None;
        parent.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });
        let mut child = Workspace::test_new("child");
        child.custom_name = None;
        child.cached_git_branch = Some("feature-x".into());
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-wt".into(),
            is_linked_worktree: true,
        });
        app.workspaces = vec![parent, child];
        app.ensure_test_terminals();
        app.active = Some(0);

        let expanded = space_labels(&app, &TerminalRuntimeRegistry::new());
        app.collapsed_space_keys.insert("repo-key".into());
        let collapsed = space_labels(&app, &TerminalRuntimeRegistry::new());

        assert_eq!(expanded, collapsed);
        assert_eq!(collapsed[1], "feature-x");
    }

    /// An explicit rename still wins over the live cwd on both surfaces.
    #[test]
    fn workspace_label_prefers_custom_name_over_live_cwd() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Widget {
            widget: StatusWidget::Workspaces,
        }];

        let mut ws = Workspace::test_new("ignored");
        ws.custom_name = Some("renamed".into());
        ws.identity_cwd = "/projects/stale".into();
        let root = ws.tabs[0].root_pane;
        let terminal_id = ws
            .terminal_id(root)
            .expect("test workspace root pane has a terminal")
            .clone();
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        app.terminals
            .get_mut(&terminal_id)
            .expect("root pane terminal exists")
            .cwd = "/projects/live".into();
        app.active = Some(0);

        let text = flat_text(&build_content(&app, test_area(80)).left);
        assert!(text.contains("1:renamed"), "chip text: {text}");
    }

    #[test]
    fn menu_widget_hidden_without_mouse_capture() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Widget {
            widget: StatusWidget::Menu,
        }];

        app.mouse_capture = false;
        let content = build_content(&app, test_area(80));
        assert!(content.left.is_empty());
        assert_eq!(content.hits.menu_button, Rect::default());

        app.mouse_capture = true;
        let content = build_content(&app, test_area(80));
        assert_eq!(content.left.len(), 1);
        let button = content.hits.menu_button;
        assert!(button.width > 0);
        assert_eq!(button.y, 30);
        assert_eq!(button.x, 0);
    }

    #[test]
    fn workspace_chips_have_hits_and_active_accent_background() {
        let mut app = AppState::test_new();
        app.mouse_capture = true;
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Widget {
            widget: StatusWidget::Workspaces,
        }];
        push_workspace(&mut app, "alpha", AgentState::Idle, true);
        push_workspace(&mut app, "beta", AgentState::Idle, true);
        app.active = Some(0);

        let content = build_content(&app, test_area(120));
        assert!(content.hits.has_workspaces_widget);
        assert_eq!(content.hits.workspace_entries.len(), 2);
        assert_eq!(content.hits.workspace_entries[0].ws_idx, 0);
        assert_eq!(content.hits.workspace_entries[1].ws_idx, 1);
        // Hit rects are adjacent and in order.
        let first = content.hits.workspace_entries[0].rect;
        let second = content.hits.workspace_entries[1].rect;
        assert_eq!(first.x + first.width, second.x);

        // Active chip inverted onto the accent; inactive chip is not.
        let active_bg = content.left[0].spans[0].style.bg;
        assert_eq!(active_bg, Some(app.palette.accent));
        assert_eq!(content.left[1].spans[0].style.bg, None);

        let text = flat_text(&content.left);
        assert!(text.contains("1:alpha"), "text: {text}");
        assert!(text.contains("2:beta"), "text: {text}");
    }

    #[test]
    fn workspace_chip_glyphs_follow_agent_state() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Widget {
            widget: StatusWidget::Workspaces,
        }];
        // Names all the same length so chip widths are directly comparable.
        push_workspace(&mut app, "alpha", AgentState::Working, true);
        push_workspace(&mut app, "block", AgentState::Blocked, true);
        push_workspace(&mut app, "fresh", AgentState::Idle, false);

        let content = build_content(&app, test_area(120));
        // Working workspace: static ● in theme yellow.
        assert_eq!(content.left[0].spans[1].content.as_ref(), "●");
        assert_eq!(content.left[0].spans[1].style.fg, Some(app.palette.yellow));
        // Blocked: ◉ in theme red.
        assert_eq!(content.left[1].spans[1].content.as_ref(), "◉");
        assert_eq!(content.left[1].spans[1].style.fg, Some(app.palette.red));
        // Done (idle, unseen): ● in theme teal.
        assert_eq!(content.left[2].spans[1].content.as_ref(), "●");
        assert_eq!(content.left[2].spans[1].style.fg, Some(app.palette.teal));

        // Chip width is constant across state flips (1-cell glyph anatomy).
        assert_eq!(content.left[0].width, content.left[1].width);
    }

    #[test]
    fn narrow_bar_truncates_but_keeps_active_workspace() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Widget {
            widget: StatusWidget::Workspaces,
        }];
        app.statusline.right = vec![StatusSegment::Text("RIGHT".into())];
        for name in ["one", "two", "three", "four", "five"] {
            push_workspace(&mut app, name, AgentState::Idle, true);
        }
        app.active = Some(4); // last workspace must survive truncation

        let content = build_content(&app, test_area(30));
        // Right side always fits and is right-anchored.
        assert_eq!(flat_text(&content.right), "RIGHT");
        assert_eq!(content.right_area.x + content.right_area.width, 30);
        // The active workspace chip is present despite the squeeze.
        assert!(
            content
                .hits
                .workspace_entries
                .iter()
                .any(|hit| hit.ws_idx == 4),
            "active chip must always be emitted; hits: {:?}",
            content.hits.workspace_entries
        );
        // Something was dropped, so the overflow marker is shown.
        let text = flat_text(&content.left);
        assert!(text.contains('…'), "text: {text}");
        // No hit rect crosses into the right side.
        for hit in &content.hits.workspace_entries {
            assert!(
                hit.rect.x + hit.rect.width <= content.right_area.x,
                "hit {hit:?} overlaps right area {:?}",
                content.right_area
            );
        }
    }

    #[test]
    fn right_side_drops_items_from_front_when_overflowing() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.right = vec![
            StatusSegment::Text("dropped-first".into()),
            StatusSegment::Text("clock".into()),
        ];
        let content = build_content(&app, test_area(10));
        assert_eq!(flat_text(&content.right), "clock");
    }

    #[test]
    fn agents_rollup_hides_zero_buckets_and_unknown() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.right = vec![StatusSegment::Widget {
            widget: StatusWidget::Agents,
        }];

        // All-unknown agents: nothing to show at all.
        push_workspace(&mut app, "mystery", AgentState::Unknown, true);
        let content = build_content(&app, test_area(80));
        assert!(content.right.is_empty(), "unknown agents are omitted");

        // One working, one blocked: exactly those two buckets appear.
        push_workspace(&mut app, "busy", AgentState::Working, true);
        push_workspace(&mut app, "stuck", AgentState::Blocked, true);
        let content = build_content(&app, test_area(80));
        let text = flat_text(&content.right);
        assert!(text.contains("◉ 1"), "blocked bucket: {text}");
        assert!(text.contains("● 1"), "working bucket static glyph: {text}");
        assert!(!text.contains('✓'), "idle bucket hidden when zero: {text}");
    }

    #[test]
    fn mode_widget_renders_colored_chip_per_mode() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Widget {
            widget: StatusWidget::Mode,
        }];

        // Hidden outside the key modes.
        app.mode = Mode::Terminal;
        let content = build_content(&app, test_area(80));
        assert!(content.left.is_empty(), "no chip outside key modes");

        app.mode = Mode::Prefix;
        let content = build_content(&app, test_area(80));
        assert_eq!(flat_text(&content.left), " PREFIX ");
        let style = content.left[0].spans[0].style;
        assert_eq!(style.bg, Some(app.palette.accent));
        assert!(style.add_modifier.contains(Modifier::BOLD));

        app.mode = Mode::Copy;
        let content = build_content(&app, test_area(80));
        assert_eq!(flat_text(&content.left), " COPY ");
        assert_eq!(content.left[0].spans[0].style.bg, Some(app.palette.yellow));
    }

    #[test]
    fn blocked_glyph_is_static_red() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Widget {
            widget: StatusWidget::Workspaces,
        }];
        push_workspace(&mut app, "stuck", AgentState::Blocked, true);

        // Upstream v0.8.0 removed the animation tick: the blocked glyph is a
        // static ◉ in theme red regardless of the (now no-op) effects flag.
        for effects in [false, true] {
            app.statusline.effects = effects;
            let content = build_content(&app, test_area(80));
            assert_eq!(content.left[0].spans[1].content.as_ref(), "◉");
            assert_eq!(content.left[0].spans[1].style.fg, Some(app.palette.red));
        }
    }

    #[test]
    fn active_chip_gradient_recolors_without_changing_width() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Widget {
            widget: StatusWidget::Workspaces,
        }];
        push_workspace(&mut app, "alpha", AgentState::Idle, true);
        app.active = Some(0);

        // Baseline: a single-color chip of the same anatomy, to compare widths.
        let mut plain_app = AppState::test_new();
        plain_app.statusline.enabled = true;
        plain_app.statusline.left = vec![StatusSegment::Widget {
            widget: StatusWidget::Workspaces,
        }];
        push_workspace(&mut plain_app, "alpha", AgentState::Idle, true);
        let plain = build_content(&plain_app, test_area(80));
        let fancy = build_content(&app, test_area(80));

        // Same cells, same text, same hit rects — only colors moved.
        assert_eq!(plain.left[0].width, fancy.left[0].width);
        assert_eq!(flat_text(&plain.left), flat_text(&fancy.left));

        // The static gradient anchors on the accent and lands on peach.
        let first = fancy.left[0].spans.first().expect("chip has spans");
        let last = fancy.left[0].spans.last().expect("chip has spans");
        assert_eq!(first.style.bg, Some(app.palette.accent));
        assert_eq!(last.style.bg, Some(app.palette.peach));
    }

    #[test]
    fn gradient_style_fades_text_across_the_ramp() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Styled {
            text: "grads".into(),
            style: StatusStyle::Gradient,
            fg: None,
            bg: None,
        }];
        let content = build_content(&app, test_area(80));
        assert_eq!(flat_text(&content.left), "grads");
        let spans = &content.left[0].spans;
        assert_eq!(spans.len(), 5, "one span per character");
        assert_eq!(spans[0].style.fg, Some(app.palette.accent));
        assert_eq!(spans[2].style.fg, Some(app.palette.mauve));
        assert_eq!(spans[4].style.fg, Some(app.palette.peach));
    }

    #[test]
    fn agents_rollup_counts_take_bucket_colors() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.right = vec![StatusSegment::Widget {
            widget: StatusWidget::Agents,
        }];
        push_workspace(&mut app, "stuck", AgentState::Blocked, true);

        let content = build_content(&app, test_area(80));
        let spans = &content.right[0].spans;
        assert_eq!(spans[0].content.as_ref(), "◉");
        assert_eq!(spans[1].content.as_ref(), " 1");
        assert_eq!(spans[1].style.fg, Some(app.palette.red));
    }

    #[test]
    fn statusline_position_does_not_affect_content_math() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.position = StatusLinePosition::Top;
        app.statusline.left = vec![StatusSegment::Text("x".into())];
        let area = Rect::new(0, 0, 40, 1);
        let content = build_content(&app, area);
        assert_eq!(content.left_area.y, 0);
        assert_eq!(flat_text(&content.left), "x");
    }

    #[test]
    fn zero_area_returns_default_content() {
        let mut app = AppState::test_new();
        app.statusline.enabled = true;
        app.statusline.left = vec![StatusSegment::Text("x".into())];
        let content = build_content(&app, Rect::default());
        assert!(content.left.is_empty());
        assert_eq!(content.hits, StatuslineHitAreas::default());
    }
}
