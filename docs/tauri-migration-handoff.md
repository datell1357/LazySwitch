# LazySwitch Electron → Tauri migration: handoff to Codex

Written by Claude (planning/advisor role) for Codex to execute against.
Worktree: `D:\Vibe Project\claude_tauri`, branch `claude_tauri`, pushed to
`origin/claude_tauri`. This is a **separate** worktree/branch from the
earlier `tauri` branch at `.omo/worktrees/tauri` — that one was explicit
prior work the user asked to ignore; do not merge from it or reference it.

The Electron source of truth being ported lives at `src/main/**` and
`src/renderer/**` in this same worktree (unchanged — still builds/runs as
the Electron app too). Read the relevant `.ts` file before touching any Rust
module that claims to port it, to verify nothing has drifted.

## Ground rules established so far

- **Full Rust rewrite**, no Node sidecar (confirmed: zero native Node module
  dependencies in the original, only `child_process`/`fs`/`fetch`).
- **Windows-first**; macOS deferred, but don't delete existing `cfg!(target_os
  = "macos")` branches where already present (desktop.rs) — just don't spend
  effort testing/expanding them yet.
- **Full 1:1 fidelity** on Windows-shell-specific UI behavior — the user
  explicitly chose this over simplifying away things like the taskbar-docked
  compact widget positioning, the always-on-top reassertion timer, and the
  `WM_CONTEXTMENU` hook. Do not simplify these away; port them faithfully,
  using the `windows` crate for the few things that genuinely need raw Win32
  (see "Win32 FFI notes" below) and shelling out to the exact original
  PowerShell scripts for everything that already worked that way in the TS
  source (tray icon rect query, taskbar theme registry read, tray-pin
  registry edits, process enumeration).
- The standalone `lazyswitch` CLI (`package.json`'s `bin` field,
  `src/main/cli.ts` + friends) is **out of scope** — it stays a Node script,
  invoked via `node cli.js ...` from hook scripts. Do not port it to Rust.
  Its JSON config schema (read via `config.rs`) must not change.
- Commit and push after each meaningfully-complete unit of work. Look at
  `git log` on this branch for the expected commit message style/detail
  level (each commit so far explains *why*, not just *what*, and calls out
  any non-obvious judgment calls).
- Run `cargo test` (and where relevant `cargo build --tests`, `npx tauri
  build --debug`) before committing. Don't commit code that doesn't compile.
- **Do not run `cargo fmt` on the whole crate.** It has pre-existing
  formatting drift across earlier phases that hasn't been normalized, and
  reformatting it is out of scope for this migration (noisy diffs, not worth
  the review overhead). If you need to format new files, run rustfmt
  scoped to exactly the new file paths, and double check with `git status`
  afterward that nothing else got touched — `cargo fmt -- path/to/file.rs`
  in this workspace has repeatedly been observed to reformat the *entire*
  crate instead of just that file (this has happened four times now), so
  always verify before trusting it, and if it happens, stop and ask before
  restoring rather than committing through it.
- **Environment note**: this machine's `cargo` sometimes can't reach the
  crates.io index (certificate provider issue), but it's intermittent, not
  persistent — try a normal online build first. If you hit the network
  error, prefer `--offline` over spending time debugging it. If `--offline`
  resolution downgrades/upgrades unrelated transitive dependencies because
  the local registry cache is stale, it's fine to accept that rather than
  hand-editing `Cargo.lock` — just note it in the commit message.
- **Git worktree note**: this worktree's `.git` metadata lives in the main
  repo at `D:\Vibe Project\LazySwitch\.git\worktrees\claude_tauri\`, outside
  this worktree's own directory. Any git write (including `git restore`, not
  just `add`/`commit`) can hit a sandbox permission wall there. That's a
  known environment quirk, not something to work around yourself — flag it
  back for a re-invocation with sandbox bypass for that step.

## Done so far (see `git log` for exact commits)

`src-tauri/src/`:
- `config.rs`, `i18n.rs`, `paths.rs` — ports of `config.ts`/`i18n.ts`/`paths.ts`.
  `config_path()` deliberately targets `%APPDATA%/LazySwitch` (Electron's
  actual `userData` dir for this app) rather than Tauri's default
  identifier-based path, so existing users' settings keep working.
- `accounts.rs` — Codex account store (`accounts.ts`).
- `provider_types.rs` — data shapes (`providers/types.ts`), plus `ProviderId`
  enum used for dispatch.
- `provider.rs` — a match-based dispatcher over `ProviderId` to
  `providers::{codex,claude}`, standing in for the original's object-literal
  `Provider` interface. There are exactly two providers and no plugin model,
  so this avoids async-trait ceremony. **Follow this same pattern** for any
  new provider-generic code — don't introduce a trait object.
- `providers/codex.rs`, `providers/claude.rs` — ports of `providers/codex.ts`
  / `providers/claude.ts`, including Claude's OAuth PKCE login flow (local
  `tiny_http` callback server) and Codex's session-file rate-limit scanning.
- `codex_api.rs` — Codex usage-fetch/token-refresh (`codex-api.ts`).
- `desktop.rs`, `desktop_processes.rs` — Codex Desktop restart + Windows
  process enumeration/selection (`desktop.ts`, `desktop-processes.ts`).
  **Deliberately keeps the original PowerShell scripts verbatim** rather
  than reimplementing via a native process-listing crate — don't "improve"
  this by switching to e.g. the `sysinfo` crate.
- `login.rs` — Codex `codex login` subprocess flow (`login.ts`).
- `switcher.rs`, `monitor.rs` — rotation/exhaustion math and the usage-polling
  loop. `UsageMonitor` runs a tokio background task and takes plain callback
  closures (`on_usage`, `on_limit_hit`) instead of depending on
  `tauri::AppHandle` directly.
- `atomic_fs.rs` — shared atomic-write/atomic-copy helper.
- `cli_resume_routing.rs`, `codex_rollouts.rs`, `claude_sessions.rs`,
  `cli_cwd_script.rs`, `powershell.rs` (shared PowerShell-exec helper —
  `desktop_processes.rs` has its own older private copy of the same
  pattern; leave that one alone, only new code should use this shared one),
  `cli_sessions.rs`, `cli_hooks.rs` — CLI session detection/restart
  infrastructure. `cli_hooks.rs`'s `install_hooks`/`install_codex_wrapper_hook`
  take the standalone CLI's `cli.js` path as a parameter rather than
  hardcoding it — still unresolved, see "Open decision" below.
- `tray_pin.rs`, `app_notify.rs`, `cli_handover.rs` — `app_notify.rs` and
  `cli_handover.rs` both use a dependency-injection trait
  (`ToastWindowFactory`, `CliHandoverDeps`) to defer actual Tauri window
  creation to a later pass — **keep using this pattern** for any remaining
  window-shaped modules: pure logic + a small trait for "create/show/close
  a window and tell me what happened", tested against a fake implementation.
- `app_state.rs`, `ipc.rs` (so far: `config_get`/`config_set`/`lang_get`),
  `tray.rs` — shared `Mutex<AppState>` via `app.manage()`, the tray icon/menu
  (checkmark-based language selection — Tauri v2 has no native radio menu
  item), `TODO(window-layer)` markers where window creation is still needed.
- `windows/manager.rs`, `windows/onboarding.rs`, `windows/approval.rs`,
  `windows/notify.rs`, `windows/cli_restart.rs` — 5 of the 7 windows. Tray
  menu items and double-click open manager/onboarding; startup
  onboarding-vs-manager routing is wired into `lib.rs`. `notify.rs` is the
  real `ToastWindowFactory` implementation (extended that trait with
  positioning/resizing methods) and `cli_restart.rs` is the real
  `CliHandoverDeps` implementation (uses `arboard` for clipboard,
  `tauri-plugin-notification` for native OS notifications).
- 112 unit tests across these modules, all passing (`cargo test`) — verified
  independently, not just via your own report, at each step so far.

**Known deferred item, not a bug**: tray right-click positioning
(`tray_menu_position` in `tray.rs` is implemented/tested but never called).
Investigation found Tauri has no public screen-positioned tray popup API
without a native owner window; a real window's `popup_menu_at` might work
once one exists. Revisit once the widget window (always-on, unlike manager/
onboarding) exists.

## Current task — widget.rs + widget_settings.rs, part 1: full mode only

Only the widget + widget-settings pair remains of the 7 windows —
deliberately last since it's the most complex (compact/full modes, taskbar
docking, save/restore bounds, two genuine Win32 FFI spots). It's being
split into two slices. **This slice is full-mode only**: normal
draggable/resizable window, always-on-top toggle, save/restore bounds, the
settings popup. **Not** compact mode, **not** taskbar docking, **not** the
Win32 FFI (context-menu hook, no-activate topmost) — those are part 2,
dispatched after this lands.

Read `src/main/index.ts`'s widget-related functions in full:
`widgetDefaultBounds`, `clampWidgetBounds`, `restoreWidgetBounds`,
`saveWidgetBounds`, `scheduleSaveWidgetBounds`, `openUsageWidgetWindow`
(the parts relevant to normal/full mode — ignore the `minimized`/compact
branches for now), `openWidgetSettings`, `closeUsageWidget`,
`hasEnrolledAccounts`, `isOnboarding`, `syncUsageWidget`,
`setUsageWidgetEnabled`. Also read `src/renderer/widget.html` and
`src/renderer/widget-settings.html` (read-only).

Build `windows/widget.rs`:
- `open_usage_widget(app)`: mirrors `openUsageWidgetWindow` for the
  non-minimized case only — build a `WebviewWindowBuilder` matching the
  original's options (frameless, `skip_taskbar`, `always_on_top` per
  config, resizable/movable, min size, background color from
  `DEFAULT_WIDGET_BACKGROUND`), positioned via `restoreWidgetBounds`/
  `widgetDefaultBounds`/`clampWidgetBounds` (port these as pure, testable
  functions taking a `WorkArea`/display-bounds struct — same style as
  `app_notify.rs`'s `toast_bounds` and `tray.rs`'s `tray_menu_position`).
  Skip everything gated on `cfg.usageWidget.minimized` for now (compact
  bounds, `applyWidgetMinimized`, `positionCompactWidget`, the topmost
  reassertion timer, `hookWindowMessage`) — leave clear `TODO(widget-part2)`
  markers.
- `close_usage_widget(app)`: mirrors `closeUsageWidget` (save bounds, then
  close) minus the compact-mode context-menu-unhook step.
- `sync_usage_widget(app)`: mirrors `syncUsageWidget` — show/hide based on
  `cfg.usageWidget.enabled && hasEnrolledAccounts() && !isOnboarding()`.
  Wire this into the places that currently have
  `// TODO(window-layer): sync usage widget` (onboarding's close handler,
  the tray's `"usage-widget"` menu toggle in `tray.rs`) and into
  `lib.rs`'s `setup()` after the equivalent of `ensureLiveEnrolled`'s
  startup logic (check what's already wired vs. still a stub there).
- Bounds saving: `moved`/`resized` window events -> debounce (400ms, same
  as `scheduleSaveWidgetBounds`) -> `saveWidgetBounds` -> persist via
  `config::save_config`. Use `tokio::time` for the debounce timer, or
  whatever's idiomatic for a Tauri window event handler — your call.

Build `windows/widget_settings.rs`:
- `open_widget_settings(app)` mirroring `openWidgetSettings` (centered
  popup, frameless, always-on-top, fixed size).

You'll need a couple more `#[tauri::command]`s for these windows' own
needs (check `preload.ts` for `widget:close`, `widget-settings:close`,
`widget:compact-height` — that last one is compact-mode-only, skip it for
this slice). Don't implement the full `config:set` widget-related side
effects yet if they'd require part-2 functionality (e.g. anything gated on
`compactPosition`) — TODO-mark those.

Test the pure bounds/clamping math the same way as before. `cargo build
--tests` + `cargo test`, commit, push. Flag back if this is bigger than
expected rather than continuing into part 2 (compact mode/Win32 FFI)
unscoped.

## Not started yet (do not start these — future slices, after widget part 1)

- Widget part 2: compact mode, taskbar docking (tray-rect PowerShell
  query + taskbar theme detection, already described as "port the
  PowerShell verbatim" in the Win32 FFI notes below), the two genuine
  Win32 FFI spots (`WM_CONTEXTMENU` hook, no-activate topmost reassertion),
  the widget's own right-click context menu (`showWidgetContextMenu`).
- The rest of `ipc.rs` (`accounts:*`, `cli:testRestart`, `onboarding:finish`,
  `open:url`, `manager:close`, the remaining `config:set` widget side
  effects).
- `tauri-plugin-autostart` wiring for `launchAtLogin`.
- Renderer bridge (`window.rotator` shim, one `<script>` tag per HTML file).
- Phase 6 (packaging) and Phase 7 (verification).

## Win32 FFI notes (what genuinely needs the `windows` crate vs. PowerShell)

Most of `index.ts`'s Windows-shell logic **already shells out to
PowerShell** in the original (tray icon rect via `FindWindowW`/
`GetWindowRect` C# Add-Type, taskbar theme via registry `Get-ItemProperty`,
and `tray-pin.ts`'s registry promotion, already ported this way). Port any
remaining ones the same way — keep the PowerShell script text, change the
caller to Rust + `powershell.rs`. You do **not** need raw Rust FFI for those.

The two things that genuinely have no PowerShell/Electron-API equivalent
and need the `windows` crate + a raw `HWND` (obtainable from a Tauri
`WebviewWindow` via its `hwnd()` method on Windows) — **only relevant for
widget part 2, not the current task**:

1. **`WM_CONTEXTMENU` hook** (`win.hookWindowMessage` in the widget window).
   Needs `SetWindowSubclass`/window-proc subclassing.
2. **No-activate topmost reassertion** (`win.moveTop()` in the always-on-top
   re-raise timer) — needs `SetWindowPos(hwnd, HWND_TOPMOST, 0,0,0,0,
   SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)` directly, since Tauri's
   `set_focus()` would steal focus. `set_always_on_top(bool)` itself needs
   no FFI — Electron's "level" parameter only differentiates macOS NSWindow
   levels; Windows always-on-top is just `HWND_TOPMOST` regardless of level.

## Open decision: `cli.js` path for hook installation

`cli-hooks.ts`'s `statuslineCommand()` builds `node "<path>/cli.js"
statusline claude`, where `<path>` is the *Electron app's own* installed
`dist/main/cli.js`. Since the standalone `lazyswitch` CLI stays out of
Rust-porting scope, the Tauri app still needs *some* copy of the compiled
`dist/main/cli.js` (+ its dependency closure) sitting next to the installed
binary, referenced via Tauri's `app.path().resource_dir()` instead of
Electron's `__dirname`. Not your task right now (only relevant once you
wire `cli_hooks.rs` into actual app startup) — flag it back when you get
there rather than deciding unilaterally; the user should weigh in on
alternatives (e.g. shipping the CLI as a genuinely separate npm package).

## Things to flag back rather than deciding unilaterally

- Anything that would require **deleting** files, branches, or the
  already-committed Electron source (`src/main/**`, `src/renderer/**`) —
  the user has explicitly reserved deletion decisions for themselves. The
  Electron app must keep building/running throughout this migration; don't
  remove it until the user says the Tauri build has fully replaced it.
- Any change to the `~/.codex-accounts` / `~/.claude-accounts` directory
  layout or the `config.json` schema — these must stay byte-compatible with
  what the still-installed Electron build (and the standalone `lazyswitch`
  CLI) reads/writes.
- If `cargo test` or `npx tauri build` reveals a design assumption from an
  earlier phase was wrong (e.g. a config field type mismatch against a real
  user's `config.json`), fix it, but leave a clear commit message explaining
  what changed and why — don't silently patch over it.
- Scope creep of any kind (reformatting unrelated files, "while I'm here"
  cleanups, touching already-committed phases beyond what a compile error
  strictly requires) — stop and ask rather than including it in the same
  commit.
