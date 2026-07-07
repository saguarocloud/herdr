# saguarocloud/herdr fork notes

This is a personal fork of [ogulcancelik/herdr](https://github.com/ogulcancelik/herdr).
This file documents fork policy, fork-only features, and the maintenance workflow.
It exists only in the fork, so it never conflicts with upstream during syncs —
prefer adding fork documentation here instead of editing `CLAUDE.md`, `README.md`,
or other upstream-owned files.

## Fork policy

- **Always sync with upstream.** The fork tracks `upstream/master` continuously,
  even though fork-only features are not intended to be merged upstream. Upstream
  moves fast; letting the fork drift makes each sync harder.
- **No direct pushes to master.** Every change — feature work and upstream syncs
  alike — goes through a pull request against `saguarocloud/herdr`. Master is
  never force-pushed or rewritten.
- **Keep the fork surface small.** Prefer new files over editing upstream files
  where practical, and follow upstream's own conventions (see `CLAUDE.md`:
  lowercase conventional commits, no `unwrap()` in production code, state/render
  separation) so upstream merges stay clean.
- Upstream's external-contributor rules in `CLAUDE.md` still apply if anything
  here is ever proposed upstream: discussions first, no unsolicited PRs.

## Workflow

Feature work happens on branches and lands via PR:

```bash
git checkout -b feat/<slug> master
# ... work, validate ...
git push -u origin feat/<slug>
gh pr create --repo saguarocloud/herdr
```

Upstream syncs also land via PR, using a merge (not a rebase — master is
never rewritten):

```bash
git fetch upstream
git checkout -b sync/upstream-$(date +%Y%m%d) master
git merge upstream/master               # resolve any conflicts with fork features
./.local/build-macos.sh nextest run     # validate (see build notes below)
cargo fmt --check
git push -u origin sync/upstream-$(date +%Y%m%d)
gh pr create --repo saguarocloud/herdr
```

Notes:

- Rebuild and restart after a sync lands: `./.local/build-macos.sh` then
  `herdr server live-handoff`. The handoff moves live panes to a new server
  process without killing them, which matters because dev sessions usually run
  *inside* herdr. `~/bin/herdr` symlinks to `target/release/herdr`, so both the
  server and TUI pick up the new build on handoff.

## Local build and test quirks (this machine)

- **macOS 26 / Zig SDK workaround:** herdr pins Zig 0.15.2 for the vendored
  libghostty-vt, which cannot parse the macOS 26 SDK. Plain `cargo build` fails
  with link errors. Use `./.local/build-macos.sh` (gitignored, machine-local),
  which pins Zig 0.15.2 against the older MacOSX15.4 SDK from CommandLineTools.
- **Use nextest, not `cargo test`.** Plain `cargo test` is flaky here from
  in-process env races.
- **5 known-environmental test failures.** These live PTY integration tests fail
  on this machine on every commit, including pristine upstream — likely because
  tests run inside a live herdr session. Exclude them when validating:
  `cross_area_agent_process_survives_detach_and_reattach`,
  `cross_area_two_clients_shared_view_and_single_detach_stability`,
  `events_subscribe_streams_output_and_agent_status_events`,
  `live_server_holds_one_pty_master_fd_per_pane`,
  `multi_client_broadcasts_frame_updates_to_all_clients`.
- `just` is not installed here; run the recipe bodies from the `justfile`
  directly (routing cargo build/test steps through `.local/build-macos.sh`).

## Fork-only features

### tmux-style status line (`[ui.statusline]`)

Commit `aa91f3b feat: add tmux-style status line with interactive widgets and
animated effects` (2026-07-07). Replaces a decade-old custom tmux setup. Built
as core frame chrome rather than a plugin because herdr plugins cannot draw
native non-terminal UI.

- **Config:** `[ui.statusline]` in `config.toml` with `enabled`, `position`
  (top/bottom), `interval`, `effects`, and `left`/`right` segment arrays.
  Segments are bare strings, `{ text, style, fg, bg }`,
  `{ command = [...], style }`, or widgets
  (`{ widget = "menu" | "workspaces" | "agents" | "mode" }`). Styles:
  normal/accent/dim/bold/gradient. Built-in tokens include
  `#{session|workspace|tab|mode|agents_*|time|time:%FMT}`. Hot-reloads via
  `herdr server reload-config`.
- **Widgets:** clickable ☰ menu button, numbered workspace chips (click to
  focus, wheel to cycle, spinner while working, ◉ when blocked), themed agent
  rollup, and a mode chip (PREFIX/COPY/RESIZE/NAV).
- **Animated effects** (`effects = true`, width-stable and recolor-only):
  blocked-glyph breathing pulse, menu badge pulse, working-spinner shimmer,
  gradient sweep on the active workspace chip, per-character gradient text.
  RGB themes interpolate; ANSI-indexed themes fall back to a DIM blink or
  static rendering. The `terminal` theme needs `[theme.custom]` RGB accent
  overrides for smooth effects.
- **Architecture:** one pure builder `build_statusline_content()`
  (`src/ui/statusline.rs`) feeds both `compute_view` hit-testing and rendering,
  so geometry and clicks stay in lockstep. Command segments run off the render
  path in `App::tick_statusline` and cache via `AppEvent::StatusLineRefreshed`.
  Pure color effects live in `src/ui/effects.rs`. Mouse handling is
  `statusline_mouse()` in `src/app/input/mouse.rs`, mode-gated so bar clicks
  cannot hijack modals.
- **New files:** `src/ui/statusline.rs`, `src/ui/effects.rs`. Touched upstream
  files (rebase conflict surface): `config/model.rs`, `config.rs`,
  `config/theme.rs`, `app/state.rs`, `app/mod.rs`, `app/runtime.rs`,
  `app/api.rs`, `app/actions.rs`, `app/input/{modal,mouse,sidebar}.rs`,
  `events.rs`, `ui.rs`, `main.rs`.

## History

- **2026-07-06:** statusline v1 (segments/tokens/commands), v2 (widgets,
  per-segment colors, mouse), v3 (animated effects, mode widget, gradients).
- **2026-07-07:** first upstream sync with the feature — rebased onto upstream
  `5b4450c` (23 commits) with zero conflicts; pushed as `aa91f3b`. (Predates the
  PR-only rule; syncs are merge-based PRs from here on.)
- **2026-07-07:** adopted PR-only workflow — all changes, including upstream
  syncs, land on master via pull request.
