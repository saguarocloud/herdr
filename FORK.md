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
- **Fork features go through PRs.** Fork-specific work (features, fixes, docs)
  lands on master via a pull request against `saguarocloud/herdr` — no direct
  pushes. Upstream syncs are the exception: they are routine merges of the
  official project and may be pushed to master directly after validation.
- **Master is never rewritten.** No force pushes. Syncs use merge (not rebase)
  so PR-merged fork commits are never rewritten out from under their PRs.
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

Upstream syncs run directly on master, using a merge (not a rebase — master
is never rewritten), and may be pushed without a PR once validation passes:

```bash
git checkout master
git fetch upstream
git merge upstream/master               # resolve any conflicts with fork features
./.local/build-macos.sh nextest run     # validate (see build notes below)
cargo fmt --check
git push origin master
```

Notes:

- Rebuild and restart after a sync lands: `./.local/build-macos.sh` then
  `herdr server live-handoff`. The handoff moves live panes to a new server
  process without killing them, which matters because dev sessions usually run
  *inside* herdr. `~/bin/herdr` symlinks to `target/release/herdr`, so both the
  server and TUI pick up the new build on handoff.
- **Hand off to a release build, never a debug one.** `app_dir_name()` in
  `src/config/io.rs` keys the config/data/socket namespace off
  `cfg!(debug_assertions)`: release builds use `herdr`, debug builds
  (`cargo build`) use `herdr-dev`. Handing the live session off with
  `--import-exe target/debug/herdr` binds the `herdr-dev` sockets and reads an
  empty `herdr-dev` config, so your statusline and settings vanish and the
  original client is orphaned. Always test in-place with `target/release/herdr`
  (`server live-handoff --import-exe <abs path to release binary>`). To undo a
  bad handoff, hand back off to the previous binary — protocol version is
  unchanged across a sync, so the existing client reconnects cleanly.

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

## Fork releases and CI

The fork publishes identifiable build artifacts from GitHub Actions:

- **Release pipeline:** `.github/workflows/fork-release.yml` (fork-only file)
  runs on every push to `master` (docs-only `website/**` pushes are skipped).
  It gates on `just check`, builds the same four targets as upstream stable
  releases (`herdr-{linux,macos}-{x86_64,aarch64}` plus `.sha256` checksums),
  and publishes a GitHub release on `saguarocloud/herdr`, pruned to the newest
  15 releases.
- **Fork changelog:** each release's notes group the conventional commits since
  the previous fork release into Added/Fixed/Performance/Maintenance sections,
  generated by `scripts/fork_release_notes.py` (fork-only, reuses the parsing
  in `scripts/preview.py`). Upstream commits pulled in by sync merges are
  included and grouped too; the merge commit lines themselves are filtered out.
  The release history on GitHub is the fork changelog; no checked-in changelog
  file to maintain. Tests: `python3 -m unittest scripts.test_fork_release_notes`
  (run by the release workflow, not `just test`, to avoid editing the
  upstream justfile).
- **Version scheme:** fork builds are stamped `<base>-<N>+<sha7>` (for example
  `0.7.3-15+f2634a6`). `N` is the build number — the count of master commits
  since the `version =` line in `Cargo.toml` last changed, i.e. since the
  upstream release commit entered fork history — so fork versions order within
  a base version (`0.7.3-15 < 0.7.3-16`) and reset when upstream releases. The
  short SHA is traceability-only build metadata. The workflow passes `N` as
  `HERDR_BUILD_ID` and the SHA as `HERDR_BUILD_COMMIT`;
  `src/build_info.rs::version()` combines them for stable-channel builds.
  Upstream stable releases set neither variable and upstream preview builds use
  the preview channel branch, so upstream version strings are unchanged.
- **Tag scheme:** release tags are `fork-v<base>-<N>`, deliberately *not*
  `v*` — upstream's `release.yml` triggers on `v*` tags and must never fire on
  the fork.
- **Self-update is blocked in fork builds.** A fork release binary still shows
  upstream update notifications (it compares against `herdr.dev/latest.json`),
  but `herdr update` refuses to install so an upstream binary cannot overwrite
  the fork build; the guidance points at the fork releases page instead. Same
  motivation as the source-build protection (PR #2).
- **PR checks:** upstream's `ci.yml` is the PR gate (fmt, clippy `-D warnings`,
  nextest on ubuntu/macos/windows, conventional-commit titles). Upstream
  governance and release workflows that need upstream-only secrets are disabled
  at the repo level (Actions settings, not file edits): `pr-gate`, `issue-gate`,
  `approve-contributor`, `approve-merged-contributor`,
  `label-next-release-issues`, `release`, `preview`, and `nix`. Re-check this
  list after upstream syncs add new workflows.
- **Syncs can add consistency checks that fork-only surface must satisfy.**
  A sync's Rust build/tests can pass while a *new* maintenance check fails on
  fork-only code. v0.7.4 added `scripts/config_reference_check.py`, which fails
  unless every `src/config` field is documented in **both**
  `docs/next/website/src/data/config-reference.json` and
  `website/src/data/config-reference.json` (kept byte-identical;
  `just release-docs-check` diffs them). It surfaces in the Fork Release
  `preflight / Run checks` job (`just check`), not in `ci.yml`. Whenever the
  fork adds a config field, register it in both reference files; after a sync,
  run `just check` (or the maintenance-script tests) and document any fork-only
  surface the new check names. The `conventional-commits` job skips merge
  commits (`git log --no-merges`), so sync merge subjects no longer fail it.

## Fork-only features

### tmux-style status line (`[ui.statusline]`)

Commit `aa91f3b feat: add tmux-style status line with interactive widgets and
animated effects` (2026-07-07). Replaces a decade-old custom tmux setup. Built
as core frame chrome rather than a plugin because herdr plugins cannot draw
native non-terminal UI.

**Config guide (`config.toml`).** The bar is off by default; enable it under
`[ui.statusline]`. `herdr --default-config` prints a live annotated block; the
reference below is the authoritative fork copy. After editing, reload without a
restart via `herdr server reload-config`.

```toml
[ui.statusline]
enabled  = true        # draw the bar (default: false)
position = "bottom"    # "bottom" (default) or "top"
interval = "2s"        # refresh cadence for command segments and #{time} (e.g. "500ms", "1m")
effects  = true        # animated color effects (default: true; see below)

# Left- and right-aligned segment arrays. When the bar is too narrow the right
# side wins and left workspace entries truncate (active workspace stays visible).
left = [
  { widget = "mode" },                              # PREFIX/COPY/RESIZE/NAV chip
  { widget = "menu" },                              # clickable ☰ menu
  { text = " #{session} ", style = "gradient" },    # styled text with a token
  { widget = "workspaces" },                        # numbered workspace chips
]
right = [
  { command = ["sh", "-c", "git branch --show-current"], style = "accent" },
  " ",                                              # bare string segment
  { widget = "agents" },                            # blocked/working/done/idle rollup
  { text = " #{time:%H:%M} ", style = "dim" },      # clock
]
```

- **Segment forms:** a bare string (`"#{workspace} "`, may embed tokens); a
  styled table `{ text, style, fg, bg }`; a command table
  `{ command = ["prog", "args"...], style, fg, bg }` (run every `interval` in the
  active workspace dir, first stdout line shown); or a widget table
  `{ widget = "menu" | "workspaces" | "agents" | "mode" }`.
- **Styles** (`style =`): `normal`, `accent`, `dim`, `bold`, `gradient`.
  Optional `fg`/`bg` accept palette tokens (`accent`, `mauve`, …), `#rrggbb`,
  `rgb(r,g,b)`, or color names.
- **Tokens** (in any text/string segment, substituted per refresh):
  `#{session}`, `#{workspace}`, `#{tab}`, `#{pane_index}`, `#{pane_count}`,
  `#{mode}`, `#{agents_blocked|agents_working|agents_done|agents_idle|agents_total}`,
  `#{time}` (→ `%H:%M`), and `#{time:%FMT}` for any strftime format. An
  unrecognized `#{name}` renders literally.
- **Registered in the config-reference JSONs** (see "Syncs can add consistency
  checks" above): the `ui.statusline.*` keys must stay listed in both
  `config-reference.json` files, so update them when the config shape changes.
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
  `5b4450c` (23 commits) with zero conflicts; pushed as `aa91f3b`. (Syncs are
  merge-based from here on.)
- **2026-07-07:** adopted the PR workflow — fork-specific changes land on
  master via pull request; upstream syncs merge directly to master.
- **2026-07-15:** synced to upstream `v0.7.4` (105 commits, merge `89fb23b`).
  One additive conflict in `src/app/runtime.rs` (both sides added a `tests`
  fn — kept both). The sync's new `config_reference_check` and the
  merge-commit-strict `conventional-commits` check turned Fork Release and CI
  red; fixed in PR #5 (document `ui.statusline.*` in the config reference;
  `--no-merges` in the commit validator).
