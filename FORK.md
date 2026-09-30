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

- **Zig 0.16.0 is required as of the v0.9.1 sync, and the old macOS SDK
  workaround is gone.** Upstream revendored libghostty-vt and moved it to Zig
  0.16.0; a 0.15.2 toolchain now fails the build script outright with
  `Your Zig version v0.15.2 does not meet the required build version of v0.16.0`.
  Homebrew's plain `zig` formula is 0.16.0 (aliased `zig@0.16`), so
  `brew upgrade zig` is enough. **The MacOSX15.4 SDK pin that
  `.local/build-macos.sh` used to carry is no longer needed** — Zig 0.16.0
  parses the macOS 26 SDK that 0.15.2 choked on (verified by forcing a build
  script rerun with no `SDKROOT`). The wrapper now only puts the rustup shims
  ahead of Homebrew's cargo/rustc and warns when `zig version` is not 0.16.0.
  Check `.github/workflows/ci.yml`'s `Install Zig` step for the authoritative
  version after every sync.
- **Use nextest, not `cargo test`.** Plain `cargo test` is flaky here from
  in-process env races.
- **Known-environmental test failures live in `binary(live_handoff)`.** These
  live PTY integration tests fail on this machine because the suite runs inside
  a live herdr session, not because of fork changes. Upstream's own `ci.yml`
  excludes `binary(live_handoff)` on macOS runners, so they never gate CI.
  As of the v0.9.0 sync the full suite is 3165 tests with exactly 2 failures,
  both in that binary: `live_handoff_preserves_pane_process_io` and
  `live_handoff_keeps_unmanaged_agent_name_bound_to_saved_session`. **Verified
  environmental**: a pristine `v0.9.0` worktree fails both plus a third
  (`live_handoff_keeps_agent_started_pane_after_agent_exits`), so the fork tree
  is strictly cleaner than upstream here. The v0.8.x-era `cross_area_*`,
  `events_subscribe_*`, `live_server_holds_one_pty_master_fd_per_pane`, and
  `multi_client_broadcasts_frame_updates_to_all_clients` failures are gone —
  the v0.9.0 client-owned-shell refactor rewrote those tests.
  Re-verify against a pristine tag before blaming the fork:
  `git worktree add /tmp/herdr-pristine <tag> && cargo nextest run -E 'binary(live_handoff)'`.
- **`sound::tests::windows_media_player_reports_invalid_media_without_waiting_for_timeout`
  is a Windows CI flake.** It drives real Windows Media Player through
  PowerShell and asserts a `MediaFailed` error reaches stderr, so it is
  timing- and runner-image-dependent. It reddened `check (windows-latest)` on
  the v0.9.0 sync PR and passed on re-run with byte-identical code
  (`src/sound.rs` is untouched by the fork and unchanged upstream since
  v0.8.2). Re-run the job before investigating.
- `just` is not installed here; run the recipe bodies from the `justfile`
  directly (routing cargo build/test steps through `.local/build-macos.sh`).
  As of v0.9.0, `just check` = `lint` (fmt + clippy `--all-targets -D warnings`)
  + `nextest run` + `maintenance-test` + `ui-hot-path-architecture-test` +
  `integration-assets-test` + `plugin-marketplace-test` + `windows-lint` +
  `docs-contract-test`. The bun-based steps (`bun test ./scripts/docs`,
  `bun test src/integration/assets/...`, and `cd workers/plugin-marketplace &&
  bun install --frozen-lockfile && bun test`) need `bun` on PATH.
- **`windows-lint` changed shape in v0.9.1 and now needs a one-time setup.**
  It is no longer a raw `cargo clippy --target x86_64-pc-windows-msvc`; that
  form fails under Zig 0.16 with `'stdlib.h' not found` while translate-c'ing
  the vendored wuffs sources, because cross-compiling to `*-windows-msvc` needs
  real MSVC libc headers. Upstream added `scripts/windows_cross.py`, and
  `just windows-lint` now calls it. It requires `just setup-windows-cross`
  once, which downloads the Microsoft Windows SDK through `xwin` and needs an
  explicit license acceptance (`--accept-license`), or
  `LIBGHOSTTY_VT_WINDOWS_LIBC` pointed at an existing Zig libc config. Until
  that setup is done, `just check` cannot complete locally — rely on CI's
  native `check (windows-latest)` job, which is stronger coverage anyway.
  Fork code that only ever ran on macOS can still break the `cfg(windows)`
  paths, so do not skip the CI result.

## Fork releases and CI

The fork publishes identifiable build artifacts from GitHub Actions:

- **Release pipeline:** `.github/workflows/fork-release.yml` (fork-only file)
  runs on every push to `master` (docs-only `docs/**` pushes are skipped; this
  was `website/**` until v0.9.0 removed the in-repo website).
  It gates on `just check`, builds the same four targets as upstream stable
  releases (`herdr-{linux,macos}-{x86_64,aarch64}` plus `.sha256` checksums),
  and publishes a GitHub release on `saguarocloud/herdr`, pruned to the newest
  15 releases.
- **Fork changelog:** each release's notes group the conventional commits since
  the previous fork release into Added/Fixed/Performance/Maintenance sections,
  generated by `scripts/fork_release_notes.py` (fork-only, and self-contained
  since the v0.9.1 sync). Upstream commits pulled in by sync merges are
  included and grouped too; the merge commit lines themselves are filtered out.
  **The grouping logic used to be borrowed from `scripts/preview.py`, and
  upstream deleted it in v0.9.1** — its preview notes are now just a compare
  link, so `TYPE_ORDER`, `commit_subjects` and `humanize_subject` all vanished
  and this script broke with `AttributeError`. The grouping is fork-only
  behaviour, so it now lives in the fork script rather than depending on
  upstream internals. The only remaining borrowed symbol is
  `preview.normalize_version`, listed in `UPSTREAM_PREVIEW_HELPERS` and guarded
  by `UpstreamDependencySurfaceTest`, which fails loudly the next time upstream
  removes something the fork leans on instead of surfacing as a mock error.
  The release history on GitHub is the fork changelog; no checked-in changelog
  file to maintain. Tests: `python3 -m unittest scripts.test_fork_release_notes`
  (run by the release workflow, not `just test`, to avoid editing the
  upstream justfile).
  Note `just maintenance-test`'s module list changes between releases — v0.9.1
  added `scripts.test_release` and `scripts.test_windows_cross` plus a
  `bun test scripts/release-workflows.test.ts` step, and deleted
  `workers/plugin-marketplace/` along with its `plugin-marketplace-test`
  recipe. Re-read the `justfile` recipes after each sync instead of replaying
  the previous list from memory.
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
  `label-next-release-issues`, `release`, `preview`, `nix`, `Windows ARM64
  installer` (`windows-arm64.yml`, added by the v0.8.2 sync — it only exercises
  upstream's installer against `herdr.dev/latest.json`, so it has nothing to
  validate on the fork), and — **new in the v0.9.0 sync, must be disabled by
  hand** — `Trigger Website Deploy` (`website-deploy.yml`) and
  `Distribution contract` (`distribution.yml`, disabled during the v0.9.0 sync).
  `issue-gate.yml` was deleted upstream in v0.8.2. Re-check this list after
  upstream syncs add new workflows.
- **A workflow cannot be disabled until it exists on the default branch.**
  GitHub only registers a workflow once it is on `master`, so
  `gh workflow disable website-deploy.yml` answers `not found on the default
  branch` while the sync is still on its PR branch. A workflow with a
  `pull_request:` trigger (like `distribution.yml`) registers as soon as the PR
  opens and can be disabled immediately; one triggered only by
  `workflow_dispatch` + `push: master` (like `website-deploy.yml`) cannot.
  **Disable those the moment the sync merges**, before the first push to
  `master` fires them.
- **v0.9.0 renamed the website workflow, which silently re-enables it.** The
  repo-level disable is keyed to the workflow's *name*, and v0.9.0 renamed
  `website.yml` (`Website`) to `website-deploy.yml` (`Trigger Website Deploy`).
  A rename therefore arrives enabled. After any sync, diff
  `.github/workflows/` and re-disable anything renamed or added. This is the
  one piece of fork maintenance that cannot be done in a commit.
- **Why `Distribution contract` must be disabled (v0.9.0).** Its `validate` job
  runs `node scripts/docs/versions.mjs check`, which calls `resolveCommit(git,
  entry.tag)` for every entry in `docs/versions/manifest.json` — i.e. it
  resolves upstream release tags like `v0.9.0`. Same failure mode that took out
  `Website`: the fork never pushes `v*` tags (confirmed: `git ls-remote --tags
  origin 'v*'` returns nothing), so the step dies with `fatal: Not a valid
  object name`. It passes locally only because the local checkout has upstream
  tags from the `upstream` remote — do not let that mislead you into thinking
  the fork's CI will pass.
- **Why the website workflow is disabled (v0.8.0, still true).** Its published
  snapshot validation runs `git ls-tree <v-tag>` against upstream release tags
  like `v0.7.5`. The fork tags releases `fork-v*`, **never** `v*` (so upstream's
  `release.yml` can't fire on it), so those tags don't exist on the fork remote
  and the step dies with `fatal: Not a valid object name v0.7.5`. Pushing `v*`
  tags to the fork is not an option — it would trigger the very workflows the
  `fork-v*` scheme avoids — so the workflow is disabled at the repo level
  instead. v0.9.0 moved the website out of this repo entirely, but kept the same
  tag-resolving validation in `distribution.yml`, so the rule survives its
  original workflow.
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
  v0.8.2 moved `website/src/data/config-reference.json` into
  `docs/versions/0.8.0/website/src/data/`, so `config_reference_check` now gates
  a single file, `docs/next/website/src/data/config-reference.json`. Register
  new fork config fields there; the `docs/versions/0.8.0/` copy is the fork's
  own published 0.8.0 snapshot and keeps the `ui.statusline.*` entries it
  shipped with.
- **`conventional-commits` only fails on master push, and must ignore synced-in
  upstream commits.** On a PR the job validates only the PR *title*; on push to
  `master` it validates every non-merge subject in
  `${{ github.event.before }}..${{ github.event.after }}`. After a sync that
  range includes all the upstream commits the merge pulled in — and the fork
  cannot rewrite them, so a single non-conventional upstream subject (v0.8.0:
  `Update rose pine surface_dim colour to "Overlay" (#2002)`) reddened master
  CI even though the PR was green. `scripts/conventional_commits.py` (fork-owned)
  therefore excludes the non-first parents of *upstream-sync* merges
  (`UPSTREAM_SYNC_RE`: `sync with upstream` / `Merge remote-tracking branch
  'upstream`), so only the fork's own commits are validated. Fork feature PR
  merges are not syncs, so their commits are still checked. `just check` does
  NOT run this validator (only `ci.yml` does), which is why Fork Release
  preflight stays green when this job is red. Verified against the v0.8.2 sync:
  the range held 134 non-merge subjects and the validator checked exactly 1 —
  the fork's own `docs:` commit.
- **Cross-compile builds follow `rust-toolchain.toml`.** v0.7.4 added
  `rust-toolchain.toml` (pins `1.96.1`), so `cargo build --target <cross>` uses
  that toolchain, not `stable`. The fork-release build job must `rustup target
  add` onto the *pinned* toolchain — `dtolnay/rust-toolchain` with
  `toolchain: ${{ env.RUST_TOOLCHAIN_VERSION }}` — or cross targets fail with
  `E0463: can't find crate for core`. Native (macOS-aarch64) builds hide this
  because the host target is always present, and `ci.yml` builds native only, so
  the failure appears only in Fork Release's cross builds. Keep
  `RUST_TOOLCHAIN_VERSION` in `fork-release.yml` matched to
  `rust-toolchain.toml` after upstream toolchain bumps.

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
effects  = true        # accepted for config compatibility but a NO-OP since the
                       # v0.8.0 sync (the animation tick was removed upstream —
                       # the bar is static; see below)

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
  focus, wheel to cycle, ● working / ◉ blocked / ○ idle marks), themed agent
  rollup, and a mode chip (PREFIX/COPY/RESIZE/NAV).
- **Static rendering (since the v0.8.0 sync).** Upstream v0.8.0 removed the
  animation tick (`81f355fa perf: replace agent spinners with static status
  marks`), which drove the fork's animated effects. Per maintainer decision the
  fork followed upstream and went static: the `effects` config key is now a
  **no-op** (kept only so existing configs still parse), and the animation
  infrastructure (`sync_animation_timer`, `spinner_tick`, `next_animation_tick`)
  is gone. What remains is entirely static and width-stable: solid state marks
  (no pulse/shimmer/spinner), and the **spatial** per-character gradients (active
  workspace chip background, `style = "gradient"` text) — these were always
  position-based, not tick-based, so they survive. If effects are ever wanted
  back, they must repaint only the status-bar region (not full panes) to respect
  the upstream perf change; see the AskUserQuestion decision in the v0.8.0 sync.
- **Architecture:** one pure builder `build_statusline_content()`
  (`src/ui/statusline.rs`) feeds both `compute_view` hit-testing and rendering,
  so geometry and clicks stay in lockstep. Command segments run off the render
  path in `App::tick_statusline` and cache via `AppEvent::StatusLineRefreshed`.
  Static gradient color math lives in `src/ui/effects.rs` (spatial only — the
  tick-driven `wave`/`pulse_*` functions were removed in the v0.8.0 sync).
  Working-pane agent marks come from upstream's static `status::state_icon`
  (renamed from `state_dot` in the v0.8.2 sync, and now takes the user's
  `ui.status_indicators` Dots/Symbols setting).
  Mouse handling is `statusline_mouse()` in `src/app/input/mouse.rs`, mode-gated
  so bar clicks cannot hijack modals.
- **The bar is client chrome since v0.9.0 — it lives in `client/shell/`.**
  Upstream v0.9.0 (#3487) moved the whole terminal UI out of the server and into
  each client, so the bar is rendered by the client alongside the sidebar and
  tab bar. `render_shell()` (`client/shell/render.rs`) draws it from the
  endpoint's `ClientShellSnapshot` plus client-local `ClientShellConfig`; hits
  land in `ShellHitMap::statusline`, and `ClientShellState::tick_statusline`
  refreshes command segments in the client's own loop.
  **This structurally retired the fork's worst bug.** Before v0.9.0 the bar was
  server-rendered, and `statusline_refresh_deadline()` fed the shared
  `next_loop_deadline_with_resize_poll` while `tick_statusline` was wired only
  into the monolithic TUI loop. `last_statusline_refresh` starts *in the past* on
  purpose (so the first tick fires immediately), so on the headless server it
  stayed in the past forever: the loop woke on a deadline that never advanced,
  `sleep_until` returned instantly every iteration, and the server **burned a
  full core doing nothing** with zero panes and zero clients. There is no longer
  a server loop deadline to leave unadvanced — the deadline and the work that
  clears it are in the same loop by construction.
  **Rule for future fork state (still load-bearing):** anything added to a shared
  loop-deadline list must be advanced by *every* loop that schedules it, or gated
  off the loops that do not. Kept as a live assertion by
  `tick_advances_its_own_deadline_instead_of_refiring`.
- **Workspace names come from the snapshot now — the bar cannot drift.** Before
  v0.9.0 the bar re-derived space names from `AppState` + the terminal runtime
  registry, deliberately mirroring the sidebar, and a fork fix (`e2fe906`) existed
  purely to keep the two in step. The endpoint now resolves names once into
  `ClientShellWorkspace::label`, and the bar renders that string verbatim, so the
  whole class of naming-drift bug is gone along with the four tests that guarded
  it. `workspace_chip_uses_the_snapshot_label_verbatim` replaces them.
- **Do not add fields to `ClientShellSnapshot` for the bar.** `CLAUDE.md`'s
  stable client endpoint contract makes named core codecs immutable, and the bar
  needs nothing new: workspaces, tabs, panes, agents, and focus are all already
  in the snapshot. Command segments run *locally* in the client, which is also
  the semantically right answer — they are presentation, and they should describe
  the machine the user is looking at.
- **Three traps found in code review of the v0.9.0 port — do not reintroduce.**
  1. *Command cwd.* `ClientShellWorkspace::new_workspace_cwd` looks like the
     workspace directory but is already resolved through the user's
     `terminal.new_cwd` policy, so with `new_cwd = "home"` it is `$HOME`. A
     `git branch` segment would then describe the wrong repo. Use
     `statusline_command_cwd()`, which reads the focused workspace's pane cwd —
     the client-side equivalent of the pre-0.9 `resolved_identity_cwd_from`.
  2. *Reload must not release the in-flight slot.* Only one command batch runs at
     a time, and that is the only thing bounding worker threads. Clearing
     `in_flight` on config reload let the next tick start a second batch, so
     repeated reloads with a slow command spawned threads without bound.
     `reset()` therefore keeps the slot and bumps a generation instead; a batch
     landing with a stale generation is discarded, because its `(side, index)`
     keys index a segment list that no longer exists.
  3. *Overlays own the mouse.* The bar's mouse handler runs before upstream's
     overlay handling, which dismisses the global menu on a click outside its
     rows. Letting the bar act while its own menu was open meant a chip click
     focused a space and left the menu stranded, and would let the bar steal
     clicks from an overlay drawn across its row. The handler now returns early
     for *any* open overlay; the menu button still toggles closed through
     upstream's click-outside path.
- **Command segments run on a detached thread, one batch at a time.**
  `statusline_command_jobs()` keys output by `(side, index)` over the FULL side
  vec, and `build_side_items` reads the same key — keep them in lockstep or
  output lands on the wrong segment (`command_output_indexing_survives_interleaved_widgets`
  guards this). A batch in flight blocks the next one, so a slow command throttles
  the bar instead of forking processes without bound; the client loop is never
  blocked. Commands are argv (never a shell), get a null stdin so a segment
  cannot steal the terminal's input, and are killed at `COMMAND_BATCH_TIMEOUT`
  so one hung segment cannot freeze the bar for the life of the client.
- **New files:** `src/client/shell/statusline.rs`, `src/client/shell/effects.rs`.
  Touched upstream files (merge conflict surface): `config/model.rs`, `config.rs`,
  `config/theme.rs`, `app/state.rs` (just `Palette::color_token`),
  `client/shell.rs`, `client/shell/{state,config,render,composition,mouse,global_menu}.rs`,
  `client/mod.rs`, `main.rs`.

## History

- **2026-09-30:** synced to upstream `v0.9.3` (96 commits from `v0.9.1`), which
  is a hotfix one day after `v0.9.2` — so this picks up both. **Zero Rust
  conflicts.** All 15 conflicts were upstream-owned docs/metadata
  (`CHANGELOG.md`, `Cargo.toml`/`Cargo.lock`, `skills/herdr/SKILL.md`, and the
  `docs/next/website` en/ja/zh-cn pages), every one of which the fork has never
  edited — verified per-file with `git diff v0.9.1 master -- <file>` before
  resolving them all to upstream rather than eyeballing them. The conflicts
  exist only because the merge base sits between the two tags, not because of
  any fork divergence.
  The v0.9.0 client-shell port has now absorbed two upstream releases without a
  single code conflict, and compiled clean on the first `--all-targets` check —
  no repeat of the silent signature breaks from the v0.8.2 and v0.9.1 syncs.
  `v0.9.2`'s breaking change (the Herdr-specific pane graphics API removed) does
  not touch the bar. Zig stays at 0.16.0 and `PROTOCOL_VERSION` stays at 22, so
  no toolchain work and a same-protocol handoff.
  `just maintenance-test` gained `scripts.test_windows_input` — re-read the
  recipe, as the previous entry warns.
  Validation: fmt, clippy `--all-targets -D warnings`, **3766/3766 nextest**,
  maintenance + fork script tests, and all three bun suites. `just windows-lint`
  still needs the one-time Microsoft SDK setup that has deliberately not been
  accepted; CI's native `check (windows-latest)` covers it.

- **2026-09-21:** synced to upstream `v0.9.1` (135 commits) — a **patch release
  with a heavy client-shell diff**, since upstream spent the cycle on SSH
  machines, selection, and graphics. Only **two conflicts**, both small unions:
  `src/client/mod.rs` (upstream added `tick_endpoint_error` to the repaint
  chain next to the fork's `tick_statusline`) and
  `src/client/shell/composition.rs` (upstream rebuilt the disconnected-sidebar
  fallback with new `remote_collapsed_groups` / `reveal_navigation_workspace`
  fields and a `render_sidebar` vs `endpoint_sidebar::render_expanded` split —
  took upstream's structure and re-added the fork's four statusline fields).
  The v0.9.0 port held up well: no statusline logic needed changing, and all
  52 statusline tests passed untouched. `ClientShellSnapshot`'s fields are
  unchanged, so the bar's data mapping was unaffected.
  **Three non-conflict breaks, all outside the Rust merge:**
  1. Upstream moved the vendored libghostty-vt to **Zig 0.16.0**, so the build
     failed before compiling a line of Rust. `brew upgrade zig` fixed it, and
     the old macOS 26 SDK workaround became unnecessary — see the build quirks
     section.
  2. A new upstream test file (`client/shell/tests/graphics.rs`) constructed
     `ClientGlobalMenuOverlay` without the fork's `anchor` field. Same silent
     signature-break class as the v0.8.2 sync: it only shows up under
     `--all-targets`.
  3. `scripts/fork_release_notes.py` broke because upstream deleted the
     conventional-commit grouping from `scripts/preview.py`. Inlined it into
     the fork script and added a dependency-surface test.
  **Validation gap:** `just windows-lint` could not run locally — v0.9.1
  replaced it with `scripts/windows_cross.py`, which needs a one-time Microsoft
  SDK download and license acceptance. CI's native `check (windows-latest)`
  covers it. Everything else green: fmt, clippy `--all-targets -D warnings`,
  **3535/3535 nextest with zero failures** (the `live_handoff` tests that were
  environmentally failing since v0.8.0 now pass), 123 maintenance/fork script
  tests, and the bun docs, integration-asset and release-workflow suites.

- **2026-09-07:** synced to upstream `v0.9.0` (110 commits) — the **largest and
  most structural sync so far**, because upstream moved the entire terminal UI
  out of the server and into each client (#3487) and dissolved `src/app/input/`
  into `src/client/shell/`. The fork's status line was server-rendered chrome
  built from `&AppState`, so this was a **port, not a merge**: the bar moved to
  `src/client/shell/{statusline,effects}.rs`, was rebuilt against
  `ClientShellSnapshot` + `ClientShellConfig`, got a `Rect` in
  `ClientShellLayout`, hits in `ShellHitMap`, a draw call in `render_shell()`,
  a handler in `client/shell/mouse.rs`, and a refresh tick in the client loop.
  Conflicts: 8 files. Three were pure deletions of upstream code the fork had
  touched (`app/input/{modal,mouse,sidebar}.rs`); `app/{mod,runtime,state}.rs`
  and `ui.rs` were upstream gutting server-side shell rendering with fork hooks
  embedded — resolved to upstream, with only `Palette::color_token` kept in
  `app/state.rs` (the client still uses `Palette`); `config.rs` was the usual
  re-export union. The server-side statusline plumbing
  (`AppEvent::StatusLineRefreshed`, `apply_statusline_outputs`, the headless
  `tick_statusline` call) was deleted rather than ported.
  **Three things this sync improved, not just preserved:** the v0.8.20 headless
  busy-loop bug is now structurally impossible (no server deadline to leave
  unadvanced); workspace-name drift between the bar and sidebar is impossible
  (the endpoint ships one resolved `label`); and command segments now run on the
  viewing machine, which is what they always should have described. Rendering
  also changed from `Frame`/`Paragraph` to direct `Buffer` writes to match the
  rest of the client chrome.
  **Repo-settings follow-up (cannot be done in a commit):** `website.yml` was
  renamed to `website-deploy.yml`, so its repo-level disable did not carry over,
  and the new `distribution.yml` resolves `v*` tags the fork never pushes. Both
  must be disabled in Actions settings. See "Fork releases and CI".
  Validation: `just check` equivalents all green (fmt, clippy `--all-targets -D
  warnings`, Windows cross-lint, 3163/3165 nextest, 113 maintenance-script
  tests, bun docs/integration/marketplace suites). The 2 failures are in
  `binary(live_handoff)`, which upstream's own macOS CI excludes, and a pristine
  `v0.9.0` worktree fails those two plus a third.

- **2026-08-20:** fixed a fork-only headless busy loop: the status-line refresh
  deadline was scheduled by the shared loop-deadline helper but only advanced on
  the monolithic path, so `herdr server` spun at 100% CPU whenever
  `[ui.statusline] enabled = true`. Measured on an empty session: +5.01s CPU per
  5s wall with the bar on, +0.00s with it off or patched. See the status-line
  section above.

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
- **2026-07-23:** synced to upstream `v0.7.5` (55 commits, PR #7). One additive
  conflict in `src/ui.rs` (`mod statusline;` vs upstream's new `mod
  tab_surface;` — kept both).
- **2026-08-03:** synced to upstream `v0.8.0` (129 commits) — a **refactor-heavy
  minor bump**. Conflicts in `config.rs`, `app/mod.rs`, `app/runtime.rs` (import
  unions + additive struct fields, plus upstream moving git-refresh into the new
  `src/app/git_refresh.rs` module and changing the event drain to return
  `(had_event, changed)`). The big one: upstream removed the whole animation
  system (`81f355fa`, `b01fc37e`), which the fork's animated statusline effects
  depended on. **Decision: follow upstream — static statusline.** Ported the
  fork's `statusline.rs`/`effects.rs` off the removed tick APIs (`spinner_tick`,
  `spinner_frame`, `agent_icon`, the render-signal `store`→`request_generic`
  change, and `resolved_identity_cwd`→`resolved_identity_cwd_from`), dropped the
  tick-driven effects, kept the static spatial gradients. `effects` config key
  is now a no-op. Note: upstream now ships the two `config-reference.json` files
  intentionally divergent (preview vs stable), so they are no longer
  byte-identical — `config_reference_check` (both-documented) still gates.
- **2026-08-19:** synced to upstream `v0.8.2` (133 commits, PR #11). Two
  additive conflicts, both unions: `src/config.rs` re-exports (fork's
  `StatusLine*`/`parse_color_opt` plus upstream's `StatusIndicatorStyle`,
  `TabBarRightEntryConfig`, `THEME_NAMES`, `window_title::*`) and `src/events.rs`
  (kept both `StatusLineRefreshed` and upstream's new `TabBarCommandFinished`).
  Three *silent* semantic breaks that only surfaced at build/clippy time, not as
  conflicts: `status::state_dot` was renamed `state_icon` and gained an
  `indicator_style` argument (`app.status_indicators`);
  `AppState::handle_mouse` gained an `InputSourceId` parameter (fork statusline
  mouse tests now pass `crate::app::LOCAL_INPUT_SOURCE`); and
  `handle_internal_event_with_pane_updates` now returns
  `Vec<PaneStateUpdate>`, so the fork's `StatusLineRefreshed` arm needed
  `return Vec::new()`. **Lesson: after a sync, always run `just check` with the
  `rust-toolchain.toml` toolchain, not just `cargo build`** — clippy `-D
  warnings` and `--all-targets` are what catch fork test code that a clean
  auto-merge left calling a changed upstream signature. On macOS, note
  `cargo`/`rustc` from Homebrew shadow the rustup shims; prepend
  `/opt/homebrew/opt/rustup/bin` to `PATH` so the pinned `1.96.1` is used.
  Upstream's `tests/live_handoff.rs` has two tests that are flaky/failing on
  local macOS (`live_handoff_keeps_unmanaged_agent_name_bound_to_saved_session`
  reproduces on a pristine `v0.8.2` checkout); `ci.yml` excludes
  `binary(live_handoff)` on macOS runners, so they do not gate CI.
