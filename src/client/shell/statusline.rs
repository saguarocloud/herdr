//! tmux-style status line: a full-width row with left/right segments composed
//! of built-in `#{...}` tokens, scriptable command output, and interactive
//! widgets (menu button, clickable workspace list, agent rollup).
//!
//! Fork-only feature; see FORK.md.
//!
//! Rendering is pure: [`build_statusline_content`] derives everything from a
//! [`StatuslineCtx`] — the endpoint's [`ClientShellSnapshot`] plus client-local
//! chrome config and state. It is called from BOTH [`render_statusline`] (to
//! draw) and the hit collection in the same frame, so hit geometry and pixels
//! can never diverge — do not let the two drift apart.
//!
//! Before v0.9.0 the bar was built server-side from `&AppState` and needed the
//! terminal runtime registry to resolve workspace names. Upstream moved the
//! whole shell into the client (#3487), so the bar is chrome like the sidebar
//! and tab bar: it reads the snapshot the endpoint already sends, and workspace
//! names come from `ClientShellWorkspace::label` — the exact string the sidebar
//! draws, so the bar can no longer invent its own naming. Command segments read
//! their cached output, refreshed off the render path by the client's
//! `tick_statusline`. See `[ui.statusline]` config.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
};

use crate::api::schema::AgentStatus;
use crate::app::state::Palette;
use crate::config::{StatusSegment, StatusStyle, StatusWidget};
use crate::protocol::{ClientShellSnapshot, ClientShellWorkspace};
use crate::ui::truncate_end;

use super::effects;
use super::render::{display_width, put_segment};
use super::state::{ClientShellConfig, ClientShellMode, ClientShellState};
use super::{panel_contrast_fg, status_color, status_icon};

/// Minimum name cells kept when the active workspace chip must be truncated.
const MIN_ACTIVE_NAME_CELLS: usize = 4;

/// Which side of the bar a configured segment belongs to. Command output is
/// cached per `(side, index)`, so this is part of that key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum StatusSide {
    Left,
    Right,
}

/// Clickable rect for one workspace entry in the status-line workspace list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StatusWorkspaceHit {
    pub workspace_id: String,
    pub rect: Rect,
}

/// Clickable regions on the status line, refreshed every frame alongside the
/// other [`super::state::ShellHitMap`] entries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct StatuslineHitAreas {
    /// The whole bar row, `Rect::default()` when the bar is off. Clicks inside
    /// it belong to the bar even where they hit no widget.
    pub bar: Rect,
    /// The menu-button widget, `Rect::default()` when absent.
    pub menu_button: Rect,
    /// One entry per visible workspace chip, in display order.
    pub workspace_entries: Vec<StatusWorkspaceHit>,
    /// True when a `workspaces` widget is configured (gates wheel-cycling).
    pub has_workspaces_widget: bool,
}

/// Client-local status-line runtime: the cached output of command segments plus
/// the resolved session name. The configured segments themselves live in
/// [`ClientShellConfig`], since they are presentation config like the rest of
/// the client-owned chrome.
#[derive(Debug, Default)]
pub(super) struct StatusLineRuntime {
    pub session_name: String,
    pub command_outputs: HashMap<(StatusSide, usize), String>,
    /// When the interval last elapsed. `None` until the first tick, which
    /// therefore fires immediately.
    last_refresh: Option<Instant>,
    /// Set while a batch of command segments is running on a worker thread.
    /// Exactly one batch is ever in flight, so a slow command throttles the bar
    /// instead of piling up processes.
    in_flight: Option<StatusCommandSlot>,
    /// Bumped whenever the configured segments change. A batch that lands
    /// carrying an older generation is discarded: its `(side, index)` keys
    /// refer to a segment list that no longer exists.
    generation: u64,
}

/// A finished batch of command-segment output, handed back from the worker
/// thread: cache key plus the command's first stdout line.
type StatusCommandOutputs = Vec<((StatusSide, usize), String)>;

/// Slot a worker thread publishes its finished batch into, tagged with the
/// config generation its `(side, index)` keys were built against.
type StatusCommandSlot = Arc<Mutex<Option<(u64, StatusCommandOutputs)>>>;

/// Hard cap on one batch of command segments.
///
/// One batch runs at a time, so without this a single hung command would freeze
/// every command segment for the life of the client. The deadline covers the
/// whole batch: it bounds the work started by one tick, not each command.
const COMMAND_BATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// One command segment to run: where its output is cached, and the argv.
struct StatusCommandJob {
    key: (StatusSide, usize),
    command: Vec<String>,
}

impl StatusLineRuntime {
    /// Resolve the session name once from the environment (it cannot change for
    /// the life of the client) and warn about unusable color specs.
    pub(super) fn new(config: &crate::config::StatusLineConfig) -> Self {
        warn_invalid_statusline_colors(config);
        let session_name = std::env::var(crate::session::SESSION_ENV_VAR)
            .ok()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string());
        Self {
            session_name,
            ..Self::default()
        }
    }

    /// Publish a finished batch, if one has arrived, and report whether the bar
    /// needs repainting because of it.
    fn drain_finished(&mut self) -> bool {
        let Some(slot) = self.in_flight.as_ref() else {
            return false;
        };
        let finished = slot.lock().ok().and_then(|mut guard| guard.take());
        let Some((generation, outputs)) = finished else {
            return false;
        };
        self.in_flight = None;
        if generation != self.generation {
            // Batch from before a config reload: its keys index a segment list
            // that no longer exists, so publishing it would paint output onto
            // the wrong segments.
            return false;
        }
        let changed = outputs
            .iter()
            .any(|(key, text)| self.command_outputs.get(key) != Some(text));
        self.command_outputs = outputs.into_iter().collect();
        changed
    }

    /// Drop cached output and force the next tick to refresh immediately.
    /// Used on config reload, where segment indexes may have moved.
    ///
    /// Deliberately keeps `in_flight`. Clearing it would let the very next tick
    /// start a second batch while the first is still running, so a slow command
    /// plus repeated config reloads could spawn worker threads without bound.
    /// The generation bump is what invalidates the running batch instead, and
    /// `COMMAND_BATCH_TIMEOUT` guarantees the slot frees itself.
    pub(super) fn reset(&mut self) {
        self.command_outputs.clear();
        self.last_refresh = None;
        self.generation = self.generation.wrapping_add(1);
    }
}

/// Every configured command segment, keyed the way [`build_side_items`] reads
/// its cache: the enumerate index over the FULL side Vec. Keep them in lockstep.
fn statusline_command_jobs(config: &ClientShellConfig) -> Vec<StatusCommandJob> {
    let mut jobs = Vec::new();
    for (side, segments) in [
        (StatusSide::Left, &config.statusline.left),
        (StatusSide::Right, &config.statusline.right),
    ] {
        for (index, segment) in segments.iter().enumerate() {
            if let StatusSegment::Command { command, .. } = segment {
                if !command.is_empty() {
                    jobs.push(StatusCommandJob {
                        key: (side, index),
                        command: command.clone(),
                    });
                }
            }
        }
    }
    jobs
}

/// The directory command segments run in: the focused workspace's active tab,
/// first pane, preferring the pane's live foreground cwd.
///
/// Stable per workspace rather than per focused pane, so a `git branch` segment
/// does not flicker as the user moves between panes in the same repo.
pub(super) fn statusline_command_cwd(snapshot: &ClientShellSnapshot) -> Option<std::path::PathBuf> {
    let workspace = snapshot.workspaces.iter().find(|ws| ws.focused)?;
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.tab_id == workspace.active_tab_id)
        .or_else(|| {
            snapshot
                .panes
                .iter()
                .find(|pane| pane.workspace_id == workspace.workspace_id)
        })?;
    let path = pane
        .foreground_cwd
        .as_deref()
        .or(pane.cwd.as_deref())
        .filter(|cwd| !cwd.is_empty())?;
    Some(std::path::PathBuf::from(path))
}

/// Run one command segment and return its first stdout line.
///
/// Failures (spawn error, non-zero exit, timeout, no output) resolve to an empty
/// string, which renders as a hidden segment rather than an error on the bar.
///
/// The command is run as an argv, never through a shell, and its stdin is null
/// so a segment can never steal the terminal's input. A command still running at
/// `deadline` is killed and reaped, so one hung segment cannot wedge the bar.
fn run_status_command(
    command: &[String],
    cwd: Option<&std::path::Path>,
    deadline: Instant,
) -> String {
    let Some((program, args)) = command.split_first() else {
        return String::new();
    };
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    // Statting the directory here keeps the filesystem off the client's loop
    // thread; an unusable cwd just means the command inherits the client's.
    if let Some(cwd) = cwd.filter(|path| path.is_dir()) {
        cmd.current_dir(cwd);
    }
    let Ok(mut child) = cmd.spawn() else {
        return String::new();
    };

    loop {
        match child.try_wait() {
            Ok(Some(status)) if !status.success() => return String::new(),
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    // Reap it: an unwaited child would linger as a zombie.
                    let _ = child.wait();
                    return String::new();
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => return String::new(),
        }
    }

    // The child has already exited, so its stdout is closed and bounded by the
    // pipe buffer. A segment that writes more than that blocks before exiting
    // and is killed by the deadline above — which is the right outcome for a
    // one-line status segment.
    let Ok(output) = child.wait_with_output() else {
        return String::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_owned()
}

/// Everything the bar reads, gathered once per frame.
///
/// This replaces the pre-0.9 `&AppState`: the shell is client-owned now, so the
/// bar is built from the wire snapshot plus client-local chrome state.
pub(super) struct StatuslineCtx<'a> {
    pub snapshot: &'a ClientShellSnapshot,
    pub config: &'a ClientShellConfig,
    pub runtime: &'a StatusLineRuntime,
    pub mode: ClientShellMode,
    /// True while the global menu is open *anchored to this bar's button*, so
    /// the button can render inverted. Menus opened from the sidebar launcher
    /// leave the bar's button in its resting style.
    pub menu_open_here: bool,
    pub menu_attention: bool,
}

impl StatuslineCtx<'_> {
    fn palette(&self) -> &Palette {
        &self.config.palette
    }

    fn workspaces(&self) -> &[ClientShellWorkspace] {
        &self.snapshot.workspaces
    }

    /// Index of the focused workspace within [`Self::workspaces`].
    fn active_index(&self) -> Option<usize> {
        self.workspaces().iter().position(|ws| ws.focused)
    }
}

/// Shared ramp for the static gradient chrome (gradient text segments and the
/// active-chip background): `start` through theme mauve to theme peach. Three
/// stops with full travel so the fade reads clearly even between adjacent
/// pastel accents; all palette tokens, so themes restyle it wholesale.
fn gradient_ramp(start: Color, p: &Palette) -> [Color; 3] {
    [start, p.mauve, p.peach]
}

/// Static blocked-glyph style: theme red. Upstream v0.8.0 removed the
/// animation tick, so attention states no longer pulse.
fn blocked_glyph_style(p: &Palette) -> Style {
    Style::default().fg(p.red)
}

/// Static working-glyph color: theme yellow (no shimmer without the tick).
fn working_glyph_color(p: &Palette) -> Color {
    p.yellow
}

/// What a rendered status item corresponds to, for hit-testing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StatusItemKind {
    Plain,
    MenuButton,
    Workspace { workspace_id: String },
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

/// Draw the bar and return its hit areas for this frame.
///
/// The client renders chrome into a [`Buffer`], so this walks the built spans
/// itself instead of handing ratatui a `Paragraph` the way the server-rendered
/// version did. Hits come from the same [`build_statusline_content`] call that
/// produced the spans, so geometry and pixels cannot diverge.
pub(super) fn render_statusline(
    buffer: &mut Buffer,
    area: Rect,
    ctx: &StatuslineCtx<'_>,
) -> StatuslineHitAreas {
    if area.width == 0 || area.height == 0 {
        return StatuslineHitAreas::default();
    }

    let palette = ctx.palette();
    // Fill the row with the bar background first; item spans draw over it.
    let base = Style::default().bg(palette.surface0).fg(palette.subtext0);
    for x in area.x..area.right() {
        buffer[(x, area.y)].set_symbol(" ").set_style(base);
    }

    let mut content = build_statusline_content(ctx, area);
    put_spans(buffer, content.left_area, content.left.iter());
    put_spans(buffer, content.right_area, content.right.iter());
    content.hits.bar = area;
    content.hits
}

/// Lay a run of items out left-to-right across `area`, clipping at its right
/// edge. Mirrors the width accounting in [`collect_hits`].
fn put_spans<'a>(buffer: &mut Buffer, area: Rect, items: impl Iterator<Item = &'a StatusItem>) {
    if area.width == 0 {
        return;
    }
    let right = area.right();
    let mut x = area.x;
    for item in items {
        for span in &item.spans {
            if x >= right {
                return;
            }
            x = put_segment(buffer, x, area.y, right, &span.content, span.style);
        }
    }
}

/// Derive the bar's items, draw areas, and hit rects from pure state.
///
/// Layout policy: the right side is built first at natural width (dropping
/// whole items from the FRONT if it alone overflows, so the rightmost items
/// survive); the left side gets the remaining budget minus a 1-cell gap, with
/// the workspace chip run reflowed to fit — the active chip is always emitted,
/// truncating its name before other placed chips are dropped.
pub(super) fn build_statusline_content(ctx: &StatuslineCtx<'_>, area: Rect) -> StatusLineContent {
    let mut hits = StatuslineHitAreas::default();
    if area.width == 0 || area.height == 0 {
        // No bar, no hits, no widget flags: bar behavior is fully off.
        return StatusLineContent::default();
    }

    let mut right = build_side_items(ctx, StatusSide::Right, &mut hits);
    while right.len() > 1 && item_total_width(&right) > area.width {
        right.remove(0);
    }
    let right_total = item_total_width(&right).min(area.width);

    let mut left = build_side_items(ctx, StatusSide::Left, &mut hits);
    let gap = if right_total > 0 { 1 } else { 0 };
    let left_budget = area.width.saturating_sub(right_total + gap);
    if item_total_width(&left) > left_budget {
        reflow_workspace_chips(ctx, &mut left, left_budget);
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

fn item_total_width(items: &[StatusItem]) -> u16 {
    items
        .iter()
        .map(|item| item.width)
        .fold(0u16, u16::saturating_add)
}

fn item_from_spans(kind: StatusItemKind, spans: Vec<Span<'static>>) -> StatusItem {
    let width = spans
        .iter()
        .map(|span| display_width(&span.content))
        .fold(0u16, u16::saturating_add);
    StatusItem { kind, spans, width }
}

/// Expand one side's config segments into items. Command output stays keyed by
/// the segment's enumerate index over the FULL side Vec — the same indexing
/// `ClientShell::statusline_command_jobs` uses; keep them in lockstep.
fn build_side_items(
    ctx: &StatuslineCtx<'_>,
    side: StatusSide,
    hits: &mut StatuslineHitAreas,
) -> Vec<StatusItem> {
    let segments = match side {
        StatusSide::Left => &ctx.config.statusline.left,
        StatusSide::Right => &ctx.config.statusline.right,
    };
    let mut items = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        match segment {
            StatusSegment::Widget { widget } => match widget {
                StatusWidget::Menu => items.extend(menu_item(ctx)),
                StatusWidget::Workspaces => {
                    hits.has_workspaces_widget = true;
                    for ws in ctx.workspaces() {
                        items.push(workspace_chip(ctx, ws, None));
                    }
                }
                StatusWidget::Agents => items.extend(agents_item(ctx)),
                StatusWidget::Mode => items.extend(mode_item(ctx)),
            },
            StatusSegment::Text(_)
            | StatusSegment::Styled { .. }
            | StatusSegment::Command { .. } => {
                let text = match segment {
                    StatusSegment::Command { .. } => ctx
                        .runtime
                        .command_outputs
                        .get(&(side, index))
                        .cloned()
                        .unwrap_or_default(),
                    StatusSegment::Text(raw) => resolve_tokens(ctx, raw),
                    StatusSegment::Styled { text, .. } => resolve_tokens(ctx, text),
                    // Handled by the outer match arm.
                    StatusSegment::Widget { .. } => String::new(),
                };
                if text.is_empty() {
                    continue;
                }
                let style = segment_span_style(segment, ctx.palette());
                let spans = if segment.style() == StatusStyle::Gradient {
                    gradient_segment_spans(segment, &text, style, ctx.palette())
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
            match &item.kind {
                StatusItemKind::MenuButton => hits.menu_button = rect,
                StatusItemKind::Workspace { workspace_id } => {
                    hits.workspace_entries.push(StatusWorkspaceHit {
                        workspace_id: workspace_id.clone(),
                        rect,
                    })
                }
                StatusItemKind::Plain => {}
            }
        }
        x = x.saturating_add(item.width);
    }
}

// ----- Widgets ----------------------------------------------------------------

/// The `☰` menu button. Hidden (and inert) when mouse support is off, exactly
/// like the sidebar launcher; inverted while its menu is open.
fn menu_item(ctx: &StatuslineCtx<'_>) -> Option<StatusItem> {
    if !ctx.config.mouse_capture {
        return None;
    }
    let p = ctx.palette();
    let base = if ctx.menu_open_here {
        Style::default().fg(panel_contrast_fg(p)).bg(p.accent)
    } else {
        Style::default().fg(p.overlay0)
    };
    let spans = if ctx.menu_attention {
        let badge = if ctx.menu_open_here {
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
    ctx: &StatuslineCtx<'_>,
    ws: &ClientShellWorkspace,
    name_budget: Option<usize>,
) -> StatusItem {
    let p = ctx.palette();
    let status = ws.agent_status;
    let (glyph, state_glyph_style) = match status {
        AgentStatus::Blocked => ("◉", blocked_glyph_style(p)),
        AgentStatus::Working => ("●", Style::default().fg(working_glyph_color(p))),
        AgentStatus::Done => ("●", Style::default().fg(p.teal)),
        AgentStatus::Idle => ("○", Style::default().fg(p.green)),
        AgentStatus::Unknown => ("·", Style::default().fg(p.overlay0)),
    };

    // The sidebar's label, verbatim: the bar must never invent its own naming.
    let mut name = ws.label.clone();
    if let Some(budget) = name_budget {
        name = truncate_end(&name, budget.max(MIN_ACTIVE_NAME_CELLS));
    }
    let number = format!("{}:", ws.number);

    let active = ws.focused;
    let (pad_style, glyph_style, number_style, name_style) = if active {
        let style = Style::default().fg(panel_contrast_fg(p)).bg(p.accent);
        let name_style = style.add_modifier(Modifier::BOLD);
        (style, style, style, name_style)
    } else {
        (
            Style::default().fg(p.subtext0),
            state_glyph_style,
            Style::default().fg(p.subtext0),
            Style::default().fg(status_color(status, p)),
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
    item_from_spans(
        StatusItemKind::Workspace {
            workspace_id: ws.workspace_id.clone(),
        },
        spans,
    )
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
        .map(|span| display_width(&span.content))
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
            x = x.saturating_add(display_width(&c.to_string()));
        }
    }
    out
}

/// Reflow the (contiguous) workspace chip run on the left side into whatever
/// budget remains after the fixed items. The active chip is always emitted.
fn reflow_workspace_chips(ctx: &StatuslineCtx<'_>, left: &mut Vec<StatusItem>, left_budget: u16) {
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
    let fitted = fit_workspace_chips(ctx, ws_budget);
    left.splice(run_start..run_start + run_len, fitted);
}

/// Fit workspace chips into `ws_budget` cells: greedy in display order, with
/// the active chip reserved up-front (name-truncated if necessary) so earlier
/// chips can never starve it. A dim `…` marks dropped chips.
fn fit_workspace_chips(ctx: &StatuslineCtx<'_>, ws_budget: u16) -> Vec<StatusItem> {
    let workspaces = ctx.workspaces();
    let natural: Vec<StatusItem> = workspaces
        .iter()
        .map(|ws| workspace_chip(ctx, ws, None))
        .collect();
    if item_total_width(&natural) <= ws_budget {
        return natural;
    }

    // Reserve one cell for the trailing "…" overflow marker.
    let mut remaining = ws_budget.saturating_sub(1);
    let active_idx = ctx.active_index().filter(|idx| *idx < natural.len());
    let mut active_chip = active_idx.map(|ws_idx| {
        let ws = &workspaces[ws_idx];
        if natural[ws_idx].width <= remaining {
            workspace_chip(ctx, ws, None)
        } else {
            let name_width = display_width(&ws.label);
            let frame_width = natural[ws_idx].width.saturating_sub(name_width);
            let name_budget = usize::from(remaining.saturating_sub(frame_width));
            workspace_chip(ctx, ws, Some(name_budget))
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
            vec![Span::styled(
                "…",
                Style::default().fg(ctx.palette().overlay0),
            )],
        ));
    }
    out
}

/// Agent rollup: blocked/working/done/idle glyph+count pairs in the sidebar's
/// visual language; zero-count buckets hidden; `Unknown` omitted (matching the
/// `#{agents_*}` token semantics). `None` when there is nothing to show.
fn agents_item(ctx: &StatuslineCtx<'_>) -> Option<StatusItem> {
    let p = ctx.palette();
    const BUCKETS: [AgentStatus; 4] = [
        AgentStatus::Blocked,
        AgentStatus::Working,
        AgentStatus::Done,
        AgentStatus::Idle,
    ];
    let mut spans: Vec<Span<'static>> = Vec::new();
    for status in BUCKETS {
        let count = agent_count(ctx, status);
        if count == 0 {
            continue;
        }
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        let mut glyph = status_icon(status, ctx.config.status_indicators);
        let mut style = Style::default().fg(status_color(status, p));
        // Blocked/working take their bucket's static color so the rollup reads
        // at a glance; other buckets keep the status_icon styling. The blocked
        // ring mirrors the workspace-chip glyph.
        match status {
            AgentStatus::Blocked => {
                glyph = "◉";
                style = blocked_glyph_style(p);
            }
            AgentStatus::Working => style = style.fg(working_glyph_color(p)),
            _ => {}
        }
        spans.push(Span::styled(glyph.to_string(), style));
        spans.push(Span::styled(
            format!(" {count}"),
            Style::default().fg(status_color(status, p)),
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
fn mode_item(ctx: &StatuslineCtx<'_>) -> Option<StatusItem> {
    let label = mode_label(ctx.mode);
    if label.is_empty() {
        return None;
    }
    let p = ctx.palette();
    let bg = match ctx.mode {
        ClientShellMode::Prefix => p.accent,
        ClientShellMode::Copy => p.yellow,
        ClientShellMode::Resize => p.peach,
        ClientShellMode::Navigate => p.teal,
        // mode_label is non-empty only for the four modes above.
        ClientShellMode::Terminal => p.accent,
    };
    let style = Style::default()
        .fg(panel_contrast_fg(p))
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
fn resolve_tokens(ctx: &StatuslineCtx<'_>, raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("#{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                out.push_str(&resolve_token(ctx, &after[..end]));
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

fn resolve_token(ctx: &StatuslineCtx<'_>, token: &str) -> String {
    match token {
        "session" => ctx.runtime.session_name.clone(),
        "workspace" => active_workspace_label(ctx).unwrap_or_default(),
        "tab" => active_tab_label(ctx).unwrap_or_default(),
        "pane_index" => pane_index(ctx).map(|i| i.to_string()).unwrap_or_default(),
        "pane_count" => pane_count(ctx).to_string(),
        "mode" => mode_label(ctx.mode).to_string(),
        "agents_blocked" => agent_count(ctx, AgentStatus::Blocked).to_string(),
        "agents_working" => agent_count(ctx, AgentStatus::Working).to_string(),
        "agents_done" => agent_count(ctx, AgentStatus::Done).to_string(),
        "agents_idle" => agent_count(ctx, AgentStatus::Idle).to_string(),
        "agents_total" => ctx.snapshot.agents.len().to_string(),
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

fn active_workspace_label(ctx: &StatuslineCtx<'_>) -> Option<String> {
    ctx.workspaces()
        .iter()
        .find(|ws| ws.focused)
        .map(|ws| ws.label.clone())
}

/// The focused tab's label. Prefers the snapshot's `focused_tab_id` and falls
/// back to the tab's own `focused` flag, which is how the sidebar resolves it.
fn focused_tab<'a>(ctx: &'a StatuslineCtx<'_>) -> Option<&'a crate::protocol::ClientShellTab> {
    let tabs = &ctx.snapshot.tabs;
    if let Some(tab_id) = ctx.snapshot.focused_tab_id.as_deref() {
        if let Some(tab) = tabs.iter().find(|tab| tab.tab_id == tab_id) {
            return Some(tab);
        }
    }
    tabs.iter().find(|tab| tab.focused)
}

fn active_tab_label(ctx: &StatuslineCtx<'_>) -> Option<String> {
    focused_tab(ctx).map(|tab| tab.label.clone())
}

/// Panes in the focused tab. `#{pane_count}` counts these, and `#{pane_index}`
/// is the focused pane's 1-based position among them, in snapshot order.
fn focused_tab_panes<'a>(ctx: &'a StatuslineCtx<'_>) -> Vec<&'a crate::protocol::ClientShellPane> {
    let Some(tab) = focused_tab(ctx) else {
        return Vec::new();
    };
    ctx.snapshot
        .panes
        .iter()
        .filter(|pane| pane.tab_id == tab.tab_id)
        .collect()
}

fn pane_count(ctx: &StatuslineCtx<'_>) -> usize {
    focused_tab_panes(ctx).len()
}

fn pane_index(ctx: &StatuslineCtx<'_>) -> Option<usize> {
    let panes = focused_tab_panes(ctx);
    let focused_id = ctx.snapshot.focused_pane_id.as_deref();
    let position = panes.iter().position(|pane| match focused_id {
        Some(id) => pane.pane_id == id,
        None => pane.focused,
    })?;
    Some(position + 1)
}

fn mode_label(mode: ClientShellMode) -> &'static str {
    match mode {
        ClientShellMode::Prefix => "PREFIX",
        ClientShellMode::Copy => "COPY",
        ClientShellMode::Resize => "RESIZE",
        ClientShellMode::Navigate => "NAV",
        ClientShellMode::Terminal => "",
    }
}

fn agent_count(ctx: &StatuslineCtx<'_>, status: AgentStatus) -> usize {
    ctx.snapshot
        .agents
        .iter()
        .filter(|agent| agent.agent_status == status)
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

// ----- Refresh tick -------------------------------------------------------------

impl ClientShellState {
    /// Advance the status line: publish any finished command batch, and start a
    /// new one when the configured interval has elapsed.
    ///
    /// Returns true when the bar should be repainted. Built-in tokens (the
    /// clock, agent counts) are resolved during render, so an elapsed interval
    /// asks for a repaint even with no command segments configured.
    ///
    /// Client-side by design: before v0.9.0 the server owned this tick, and
    /// scheduling it on the shared server loop deadline without advancing it on
    /// the headless path pegged a core. The bar is client chrome now, so the
    /// deadline and the work that clears it live in the same loop.
    pub(crate) fn tick_statusline(&mut self, now: Instant) -> bool {
        let mut repaint = self.statusline.drain_finished();
        if !self.config.statusline.enabled {
            return repaint;
        }

        let interval = self.config.statusline.interval_duration();
        if let Some(last) = self.statusline.last_refresh {
            if now.saturating_duration_since(last) < interval {
                return repaint;
            }
        }
        self.statusline.last_refresh = Some(now);
        // An elapsed interval always repaints: #{time} and the agent rollup are
        // resolved at render time, not cached.
        repaint = true;

        if self.statusline.in_flight.is_some() {
            return repaint;
        }
        let jobs = statusline_command_jobs(&self.config);
        if jobs.is_empty() {
            return repaint;
        }

        // Commands run in the active workspace's directory, so `git branch` and
        // friends describe the space the user is looking at.
        //
        // Deliberately NOT `ClientShellWorkspace::new_workspace_cwd`: that field
        // is already run through the user's `terminal.new_cwd` policy, so with
        // `new_cwd = "home"` it is $HOME rather than the workspace. Use the
        // workspace's own pane cwd, which is what the pre-0.9 server-side bar
        // used (`Workspace::resolved_identity_cwd_from`).
        let cwd = self.snapshot.as_deref().and_then(statusline_command_cwd);

        let slot = Arc::new(Mutex::new(None));
        self.statusline.in_flight = Some(Arc::clone(&slot));
        let generation = self.statusline.generation;
        let deadline = now + COMMAND_BATCH_TIMEOUT;
        // Detached: a hung command must never block the client's input loop.
        std::thread::spawn(move || {
            let outputs = jobs
                .into_iter()
                .map(|job| {
                    (
                        job.key,
                        run_status_command(&job.command, cwd.as_deref(), deadline),
                    )
                })
                .collect::<Vec<_>>();
            if let Ok(mut guard) = slot.lock() {
                *guard = Some((generation, outputs));
            }
        });
        repaint
    }
}

/// Warn once — at load or hot-reload, never on the render path — about
/// `fg`/`bg` specs that neither the palette nor `parse_color_opt` recognize.
/// Render silently ignores them, so without this a typo would be invisible.
pub(super) fn warn_invalid_statusline_colors(config: &crate::config::StatusLineConfig) {
    // Palette token names are theme-independent, so any palette works here.
    let palette = Palette::catppuccin();
    for (side, segments) in [("left", &config.left), ("right", &config.right)] {
        for segment in segments {
            let (fg, bg) = segment.color_overrides();
            for (field, spec) in [("fg", fg), ("bg", bg)] {
                if let Some(spec) = spec {
                    if palette.color_token(spec).is_none()
                        && crate::config::parse_color_opt(spec).is_none()
                    {
                        tracing::warn!(
                            side,
                            field,
                            color = spec,
                            "unknown statusline color; override ignored"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::shell::tests::snapshot;
    use crate::config::{Config, StatusLinePosition};
    use crate::protocol::ClientShellAgent;
    use std::time::Duration;

    fn test_area(width: u16) -> Rect {
        Rect::new(0, 30, width, 1)
    }

    fn test_config() -> ClientShellConfig {
        ClientShellConfig::from_config(&Config::default())
    }

    fn runtime() -> StatusLineRuntime {
        StatusLineRuntime {
            session_name: "work".into(),
            ..StatusLineRuntime::default()
        }
    }

    /// A context over the shared client-shell test snapshot.
    fn ctx<'a>(
        snapshot: &'a ClientShellSnapshot,
        config: &'a ClientShellConfig,
        runtime: &'a StatusLineRuntime,
    ) -> StatuslineCtx<'a> {
        StatuslineCtx {
            snapshot,
            config,
            runtime,
            mode: ClientShellMode::Terminal,
            menu_open_here: false,
            menu_attention: false,
        }
    }

    /// Push a workspace with `label` whose aggregate agent state is `status`.
    fn push_workspace(snapshot: &mut ClientShellSnapshot, label: &str, status: AgentStatus) {
        let number = snapshot.workspaces.len() + 1;
        let workspace_id = format!("ws_{number}");
        let mut ws = snapshot.workspaces[0].clone();
        ws.workspace_id = workspace_id.clone();
        ws.number = number;
        ws.label = label.into();
        ws.focused = false;
        ws.agent_status = status;
        snapshot.workspaces.push(ws);
        snapshot.agents.push(ClientShellAgent {
            pane_id: format!("pane_{number}"),
            workspace_id,
            tab_id: format!("tab_{number}"),
            name: Some("agent".into()),
            display_agent: Some("agent".into()),
            agent: Some("agent".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: 0,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        });
    }

    fn flat_text(items: &[StatusItem]) -> String {
        items
            .iter()
            .flat_map(|item| item.spans.iter())
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn widget(widget: StatusWidget) -> StatusSegment {
        StatusSegment::Widget { widget }
    }

    #[test]
    fn resolve_tokens_substitutes_session_and_leaves_unknown() {
        let snap = snapshot();
        let config = test_config();
        let rt = runtime();
        let ctx = ctx(&snap, &config, &rt);
        assert_eq!(resolve_tokens(&ctx, "#{session}"), "work");
        assert_eq!(resolve_tokens(&ctx, "#{workspace}"), "client-shell");
        assert_eq!(resolve_tokens(&ctx, "#{tab}"), "1");
        assert_eq!(resolve_tokens(&ctx, "#{pane_count}"), "1");
        assert_eq!(resolve_tokens(&ctx, "#{pane_index}"), "1");
        // Unknown tokens stay verbatim so typos are visible, not silent.
        assert_eq!(resolve_tokens(&ctx, "#{nope}"), "#{nope}");
    }

    #[test]
    fn resolve_tokens_handles_unterminated_and_plain_text() {
        let snap = snapshot();
        let config = test_config();
        let rt = runtime();
        let ctx = ctx(&snap, &config, &rt);
        assert_eq!(resolve_tokens(&ctx, "plain"), "plain");
        assert_eq!(resolve_tokens(&ctx, "a #{session"), "a #{session");
        assert_eq!(resolve_tokens(&ctx, "[#{session}]"), "[work]");
    }

    #[test]
    fn agent_tokens_count_snapshot_agents_by_status() {
        let mut snap = snapshot();
        push_workspace(&mut snap, "one", AgentStatus::Blocked);
        push_workspace(&mut snap, "two", AgentStatus::Working);
        push_workspace(&mut snap, "three", AgentStatus::Working);
        let config = test_config();
        let rt = runtime();
        let ctx = ctx(&snap, &config, &rt);
        assert_eq!(resolve_tokens(&ctx, "#{agents_blocked}"), "1");
        assert_eq!(resolve_tokens(&ctx, "#{agents_working}"), "2");
        assert_eq!(resolve_tokens(&ctx, "#{agents_done}"), "0");
        assert_eq!(resolve_tokens(&ctx, "#{agents_total}"), "3");
    }

    #[test]
    fn mode_label_maps_prefix_and_terminal() {
        assert_eq!(mode_label(ClientShellMode::Prefix), "PREFIX");
        assert_eq!(mode_label(ClientShellMode::Copy), "COPY");
        assert_eq!(mode_label(ClientShellMode::Resize), "RESIZE");
        assert_eq!(mode_label(ClientShellMode::Navigate), "NAV");
        assert_eq!(mode_label(ClientShellMode::Terminal), "");
    }

    #[test]
    fn resolve_status_color_prefers_palette_tokens_over_ansi() {
        let palette = Palette::catppuccin();
        // "red" is the *theme* red, not ANSI red: a themed bar stays themed.
        assert_eq!(resolve_status_color("red", &palette), Some(palette.red));
        assert_eq!(
            resolve_status_color("accent", &palette),
            Some(palette.accent)
        );
        // Raw RGB needs an explicit spec.
        assert_eq!(
            resolve_status_color("#ff0000", &palette),
            Some(Color::Rgb(255, 0, 0))
        );
        assert_eq!(resolve_status_color("not-a-color", &palette), None);
    }

    #[test]
    fn segment_span_style_presets_unchanged_without_overrides() {
        let p = Palette::catppuccin();
        assert_eq!(
            segment_span_style(&StatusSegment::Text("x".into()), &p),
            Style::default().fg(p.subtext0)
        );
        assert_eq!(
            segment_span_style(
                &StatusSegment::Styled {
                    text: "x".into(),
                    style: StatusStyle::Dim,
                    fg: None,
                    bg: None,
                },
                &p
            ),
            Style::default().fg(p.overlay0)
        );
    }

    #[test]
    fn segment_span_style_applies_fg_bg_overrides() {
        let p = Palette::catppuccin();
        let style = segment_span_style(
            &StatusSegment::Styled {
                text: "x".into(),
                style: StatusStyle::Normal,
                fg: Some("mauve".into()),
                bg: Some("#101010".into()),
            },
            &p,
        );
        assert_eq!(style.fg, Some(p.mauve));
        assert_eq!(style.bg, Some(Color::Rgb(16, 16, 16)));
    }

    #[test]
    fn command_output_indexing_survives_interleaved_widgets() {
        let mut snap = snapshot();
        push_workspace(&mut snap, "two", AgentStatus::Idle);
        let mut config = test_config();
        // Widgets sit between the command segments on purpose: the cache key is
        // the index over the FULL side vec, not over commands only.
        config.statusline.left = vec![
            StatusSegment::Command {
                command: vec!["true".into()],
                style: StatusStyle::Normal,
                fg: None,
                bg: None,
            },
            widget(StatusWidget::Workspaces),
            StatusSegment::Command {
                command: vec!["true".into()],
                style: StatusStyle::Normal,
                fg: None,
                bg: None,
            },
        ];
        let mut rt = runtime();
        rt.command_outputs
            .insert((StatusSide::Left, 0), "FIRST".into());
        rt.command_outputs
            .insert((StatusSide::Left, 2), "THIRD".into());

        let jobs = statusline_command_jobs(&config);
        assert_eq!(
            jobs.iter().map(|job| job.key).collect::<Vec<_>>(),
            vec![(StatusSide::Left, 0), (StatusSide::Left, 2)],
            "command jobs must use the same keys the renderer reads"
        );

        let ctx = ctx(&snap, &config, &rt);
        let text = flat_text(&build_statusline_content(&ctx, test_area(120)).left);
        assert!(text.starts_with("FIRST"), "{text}");
        assert!(text.ends_with("THIRD"), "{text}");
    }

    #[test]
    fn workspace_chip_uses_the_snapshot_label_verbatim() {
        // Since v0.9.0 the endpoint resolves space names and ships them in the
        // snapshot, so the bar renders exactly what the sidebar renders. It has
        // no independent naming path left to drift.
        let mut snap = snapshot();
        snap.workspaces[0].label = "herdr (feature/x)".into();
        let mut config = test_config();
        config.statusline.left = vec![widget(StatusWidget::Workspaces)];
        let rt = runtime();
        let ctx = ctx(&snap, &config, &rt);
        let text = flat_text(&build_statusline_content(&ctx, test_area(120)).left);
        assert!(text.contains("herdr (feature/x)"), "{text}");
    }

    #[test]
    fn menu_widget_hidden_without_mouse_capture() {
        let snap = snapshot();
        let mut config = test_config();
        config.statusline.left = vec![widget(StatusWidget::Menu)];

        config.mouse_capture = true;
        let rt = runtime();
        let content = build_statusline_content(&ctx(&snap, &config, &rt), test_area(40));
        assert!(flat_text(&content.left).contains('☰'));
        assert_ne!(content.hits.menu_button, Rect::default());

        config.mouse_capture = false;
        let content = build_statusline_content(&ctx(&snap, &config, &rt), test_area(40));
        assert!(!flat_text(&content.left).contains('☰'));
        assert_eq!(content.hits.menu_button, Rect::default());
    }

    #[test]
    fn workspace_chips_have_hits_and_active_accent_background() {
        let mut snap = snapshot();
        push_workspace(&mut snap, "second", AgentStatus::Idle);
        let mut config = test_config();
        config.statusline.left = vec![widget(StatusWidget::Workspaces)];
        let rt = runtime();
        let content = build_statusline_content(&ctx(&snap, &config, &rt), test_area(120));

        assert_eq!(content.hits.workspace_entries.len(), 2);
        assert!(content.hits.has_workspaces_widget);
        assert_eq!(content.hits.workspace_entries[0].workspace_id, "ws_1");
        assert_eq!(content.hits.workspace_entries[1].workspace_id, "ws_2");
        // Hits are ordered left to right and never overlap.
        assert!(
            content.hits.workspace_entries[0].rect.right()
                <= content.hits.workspace_entries[1].rect.x
        );

        // The focused chip is inverted onto the gradient ramp, so its first
        // cell carries a background the inactive chips do not.
        let active = &content.left[0];
        assert!(active.spans.iter().all(|span| span.style.bg.is_some()));
        let inactive = &content.left[1];
        assert!(inactive.spans.iter().all(|span| span.style.bg.is_none()));
    }

    #[test]
    fn workspace_chip_glyphs_follow_agent_state() {
        let mut snap = snapshot();
        snap.workspaces[0].focused = false;
        snap.workspaces[0].agent_status = AgentStatus::Blocked;
        push_workspace(&mut snap, "working", AgentStatus::Working);
        push_workspace(&mut snap, "done", AgentStatus::Done);
        push_workspace(&mut snap, "idle", AgentStatus::Idle);
        let mut config = test_config();
        config.statusline.left = vec![widget(StatusWidget::Workspaces)];
        let rt = runtime();
        let content = build_statusline_content(&ctx(&snap, &config, &rt), test_area(160));
        let text = flat_text(&content.left);
        assert!(text.contains('◉'), "blocked ring missing: {text}");
        assert!(text.contains('●'), "working/done dot missing: {text}");
        assert!(text.contains('○'), "idle ring missing: {text}");
    }

    #[test]
    fn narrow_bar_truncates_but_keeps_active_workspace() {
        let mut snap = snapshot();
        snap.workspaces[0].label = "the-active-space".into();
        for i in 0..8 {
            push_workspace(&mut snap, &format!("filler-{i}"), AgentStatus::Idle);
        }
        let mut config = test_config();
        config.statusline.left = vec![widget(StatusWidget::Workspaces)];
        let rt = runtime();
        let content = build_statusline_content(&ctx(&snap, &config, &rt), test_area(30));

        assert!(item_total_width(&content.left) <= 30);
        // The active chip survives even when its own name must be truncated.
        let active_hit = content
            .hits
            .workspace_entries
            .iter()
            .find(|hit| hit.workspace_id == "ws_1");
        assert!(
            active_hit.is_some(),
            "active chip dropped from a narrow bar"
        );
        assert!(flat_text(&content.left).contains('…'));
    }

    #[test]
    fn right_side_drops_items_from_front_when_overflowing() {
        let snap = snapshot();
        let mut config = test_config();
        config.statusline.right = vec![
            StatusSegment::Text("AAAAAAAAAA".into()),
            StatusSegment::Text("BBBBBBBBBB".into()),
            StatusSegment::Text("KEEP".into()),
        ];
        let rt = runtime();
        let content = build_statusline_content(&ctx(&snap, &config, &rt), test_area(12));
        let text = flat_text(&content.right);
        // The rightmost items are the ones that survive.
        assert!(text.ends_with("KEEP"), "{text}");
        assert!(!text.contains("AAAAAAAAAA"), "{text}");
    }

    #[test]
    fn agents_rollup_hides_zero_buckets_and_unknown() {
        let mut snap = snapshot();
        push_workspace(&mut snap, "blocked", AgentStatus::Blocked);
        push_workspace(&mut snap, "unknown", AgentStatus::Unknown);
        let mut config = test_config();
        config.statusline.left = vec![widget(StatusWidget::Agents)];
        let rt = runtime();
        let content = build_statusline_content(&ctx(&snap, &config, &rt), test_area(60));
        let text = flat_text(&content.left);
        assert!(text.contains("◉ 1"), "{text}");
        // Only the blocked bucket has a count; unknown is never shown.
        assert_eq!(text.trim(), "◉ 1", "{text}");
    }

    #[test]
    fn agents_rollup_is_absent_without_agents() {
        let snap = snapshot();
        let mut config = test_config();
        config.statusline.left = vec![widget(StatusWidget::Agents)];
        let rt = runtime();
        let content = build_statusline_content(&ctx(&snap, &config, &rt), test_area(60));
        assert!(content.left.is_empty());
    }

    #[test]
    fn mode_widget_renders_colored_chip_per_mode() {
        let snap = snapshot();
        let mut config = test_config();
        config.statusline.left = vec![widget(StatusWidget::Mode)];
        let rt = runtime();
        let p = config.palette.clone();

        for (mode, label, bg) in [
            (ClientShellMode::Prefix, "PREFIX", p.accent),
            (ClientShellMode::Copy, "COPY", p.yellow),
            (ClientShellMode::Resize, "RESIZE", p.peach),
            (ClientShellMode::Navigate, "NAV", p.teal),
        ] {
            let mut c = ctx(&snap, &config, &rt);
            c.mode = mode;
            let content = build_statusline_content(&c, test_area(40));
            assert_eq!(flat_text(&content.left).trim(), label);
            assert_eq!(content.left[0].spans[0].style.bg, Some(bg));
        }

        // Terminal mode has no chip at all — zero width, not a blank one.
        let content = build_statusline_content(&ctx(&snap, &config, &rt), test_area(40));
        assert!(content.left.is_empty());
    }

    #[test]
    fn blocked_glyph_is_static_red() {
        // Upstream v0.8.0 removed the animation tick; attention states are
        // static now, so this must not depend on any clock.
        let p = Palette::catppuccin();
        assert_eq!(blocked_glyph_style(&p), Style::default().fg(p.red));
        assert_eq!(working_glyph_color(&p), p.yellow);
    }

    #[test]
    fn active_chip_gradient_recolors_without_changing_width() {
        let p = Palette::catppuccin();
        let spans = vec![
            Span::styled(" ", Style::default()),
            Span::styled("1:name", Style::default()),
            Span::styled(" ", Style::default()),
        ];
        let before: u16 = spans
            .iter()
            .map(|s| display_width(&s.content))
            .fold(0, u16::saturating_add);
        let after_spans = active_chip_gradient(spans, &p);
        let after: u16 = after_spans
            .iter()
            .map(|s| display_width(&s.content))
            .fold(0, u16::saturating_add);
        // Width is load-bearing: hit rects are computed from it.
        assert_eq!(before, after);
        let backgrounds: Vec<_> = after_spans.iter().map(|s| s.style.bg).collect();
        assert!(backgrounds.iter().all(Option::is_some));
        assert!(
            backgrounds.first() != backgrounds.last(),
            "gradient did not travel across the chip"
        );
    }

    #[test]
    fn gradient_style_fades_text_across_the_ramp() {
        let p = Palette::catppuccin();
        let segment = StatusSegment::Styled {
            text: "gradient".into(),
            style: StatusStyle::Gradient,
            fg: None,
            bg: None,
        };
        let spans = gradient_segment_spans(&segment, "gradient", Style::default(), &p);
        let colors: Vec<_> = spans.iter().filter_map(|s| s.style.fg).collect();
        assert!(colors.len() > 1);
        assert_ne!(colors.first(), colors.last());
    }

    #[test]
    fn statusline_position_does_not_affect_content_math() {
        // Position only decides which row the bar claims; the content built for
        // a given rect must be identical either way.
        let snap = snapshot();
        let mut config = test_config();
        config.statusline.left = vec![StatusSegment::Text("#{session}".into())];
        let rt = runtime();

        config.statusline.position = StatusLinePosition::Top;
        let top =
            flat_text(&build_statusline_content(&ctx(&snap, &config, &rt), test_area(40)).left);
        config.statusline.position = StatusLinePosition::Bottom;
        let bottom =
            flat_text(&build_statusline_content(&ctx(&snap, &config, &rt), test_area(40)).left);
        assert_eq!(top, bottom);
    }

    /// The tick must actually advance its own deadline.
    ///
    /// The pre-0.9 server-side bar scheduled a refresh deadline on the shared
    /// loop-deadline list but only advanced it on the monolithic path, so the
    /// headless server woke on a deadline permanently in the past and burned a
    /// full core. The bar is client chrome now, so the deadline and the work
    /// that clears it are in the same loop — but assert the advance anyway,
    /// because that is the bug this feature has already shipped once.
    #[test]
    fn tick_advances_its_own_deadline_instead_of_refiring() {
        let mut config = test_config();
        config.statusline.enabled = true;
        config.statusline.interval = "10s".into();
        let mut state = ClientShellState::new(config);

        let start = Instant::now();
        // First tick fires immediately (last_refresh starts unset).
        assert!(state.tick_statusline(start));
        // ...and then stays quiet until the interval actually elapses.
        assert!(!state.tick_statusline(start));
        assert!(!state.tick_statusline(start + Duration::from_secs(9)));
        assert!(state.tick_statusline(start + Duration::from_secs(10)));
    }

    /// A config reload must not let a second batch start while the first is
    /// still running: `reset()` used to clear `in_flight`, so repeated reloads
    /// with a slow command could spawn worker threads without bound.
    #[test]
    fn reset_does_not_release_the_in_flight_slot() {
        let mut rt = runtime();
        let slot: StatusCommandSlot = Arc::new(Mutex::new(None));
        rt.in_flight = Some(Arc::clone(&slot));
        let before = rt.generation;

        rt.reset();
        assert!(
            rt.in_flight.is_some(),
            "reset released the throttle; the next tick would spawn a second batch"
        );
        assert_ne!(
            rt.generation, before,
            "reset must invalidate the running batch"
        );
    }

    /// Output produced before a reload is keyed to segments that have moved, so
    /// publishing it would paint command output onto the wrong segment.
    #[test]
    fn a_batch_from_before_a_reload_is_discarded() {
        let mut rt = runtime();
        let slot: StatusCommandSlot = Arc::new(Mutex::new(None));
        rt.in_flight = Some(Arc::clone(&slot));
        let stale_generation = rt.generation;
        rt.reset();

        // The worker finishes after the reload, still tagged with the old key space.
        *slot.lock().unwrap() = Some((
            stale_generation,
            vec![((StatusSide::Left, 0), "old".into())],
        ));
        assert!(!rt.drain_finished(), "stale batch reported a repaint");
        assert!(
            rt.command_outputs.is_empty(),
            "stale batch was published against the new segment list"
        );
        // ...and the slot is released, so the next tick can start a fresh batch.
        assert!(rt.in_flight.is_none());
    }

    #[test]
    fn a_current_batch_is_published() {
        let mut rt = runtime();
        let slot: StatusCommandSlot = Arc::new(Mutex::new(None));
        rt.in_flight = Some(Arc::clone(&slot));
        *slot.lock().unwrap() =
            Some((rt.generation, vec![((StatusSide::Left, 0), "fresh".into())]));
        assert!(rt.drain_finished());
        assert_eq!(
            rt.command_outputs
                .get(&(StatusSide::Left, 0))
                .map(String::as_str),
            Some("fresh")
        );
        assert!(rt.in_flight.is_none());
    }

    /// A hung segment must not wedge the bar forever: one batch runs at a time,
    /// so without a deadline every command segment would freeze permanently.
    // Unix-gated: these drive real `sh`/`sleep` binaries. On Windows they would
    // pass vacuously (spawn fails, output is empty), which is worse than not
    // claiming the coverage at all.
    #[cfg(unix)]
    #[test]
    fn a_hung_command_is_killed_at_the_deadline() {
        let start = Instant::now();
        let out = run_status_command(
            &["sleep".into(), "30".into()],
            None,
            start + Duration::from_millis(150),
        );
        assert_eq!(out, "", "a killed command contributes no text");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "run_status_command waited on a hung child instead of killing it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_output_is_the_first_stdout_line_and_failures_are_blank() {
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            run_status_command(
                &["sh".into(), "-c".into(), "printf 'one\ntwo\n'".into()],
                None,
                deadline
            ),
            "one"
        );
        // Non-zero exit renders as a hidden segment, not an error on the bar.
        assert_eq!(
            run_status_command(
                &["sh".into(), "-c".into(), "echo x; exit 1".into()],
                None,
                deadline
            ),
            ""
        );
        assert_eq!(
            run_status_command(&["definitely-not-a-real-binary-xyz".into()], None, deadline),
            ""
        );
    }

    /// A disabled bar must never schedule work.
    #[test]
    fn tick_is_inert_while_the_bar_is_disabled() {
        let mut state = ClientShellState::new(test_config());
        let start = Instant::now();
        assert!(!state.tick_statusline(start));
        assert!(!state.tick_statusline(start + Duration::from_secs(60)));
    }

    /// Config reload re-indexes command output, so the cache must be dropped
    /// rather than left keyed to segments that have moved.
    #[test]
    fn reset_drops_cached_output_and_refreshes_immediately() {
        let mut rt = runtime();
        rt.command_outputs
            .insert((StatusSide::Left, 0), "stale".into());
        rt.last_refresh = Some(Instant::now());
        rt.reset();
        assert!(rt.command_outputs.is_empty());
        assert!(rt.last_refresh.is_none());
    }

    #[test]
    fn zero_area_returns_default_content() {
        let snap = snapshot();
        let mut config = test_config();
        config.statusline.left = vec![widget(StatusWidget::Workspaces)];
        let rt = runtime();
        let content = build_statusline_content(&ctx(&snap, &config, &rt), Rect::new(0, 0, 0, 0));
        assert!(content.left.is_empty());
        assert!(content.right.is_empty());
        // No bar means no widget flags either: the wheel gesture stays inert.
        assert!(!content.hits.has_workspaces_widget);
        assert_eq!(content.hits.menu_button, Rect::default());
    }
}
