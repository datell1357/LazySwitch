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
- Commit and push after each meaningfully-complete unit of work, same as the
  Phase 1/2 commits already on this branch — look at those commit messages
  for the expected style/detail level.
- Run `cargo test` (and where relevant `cargo build --tests`, `npx tauri
  build --debug`) before committing. Don't commit code that doesn't compile.

## What's already done (Phase 1 + 2 — committed, pushed, tested)

`src-tauri/src/`:
- `config.rs`, `i18n.rs`, `paths.rs` — ports of `config.ts`/`i18n.ts`/`paths.ts`.
  `config_path()` deliberately targets `%APPDATA%/LazySwitch` (Electron's
  actual `userData` dir for this app) rather than Tauri's default
  identifier-based path, so existing users' settings keep working.
- `accounts.rs` — Codex account store (`accounts.ts`).
- `provider_types.rs` — data shapes (`providers/types.ts`), plus `ProviderId`
  enum used for dispatch (see below).
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
  **Deliberately keeps the original PowerShell scripts verbatim** (
  `Get-CimInstance Win32_Process`, `taskkill.exe`) rather than reimplementing
  via a native process-listing crate — the selection logic has hard-won
  edge-case fixes called out in comments; don't "improve" this by switching
  to e.g. the `sysinfo` crate.
- `login.rs` — Codex `codex login` subprocess flow (`login.ts`).
- `switcher.rs`, `monitor.rs` — rotation/exhaustion math and the usage-polling
  loop (`switcher.ts`, `monitor.ts`). `UsageMonitor` runs a tokio background
  task and takes plain callback closures (`on_usage`, `on_limit_hit`) instead
  of depending on `tauri::AppHandle` directly — wire those callbacks to
  `app.emit(...)` / tray refresh when you get to the window/tray layer.
- `atomic_fs.rs` — shared atomic-write/atomic-copy helper (consolidates a
  pattern that appeared 3 separate times in the original TS).
- 72 unit tests across these modules, all passing as of the Phase 2 commit.

## What's half-written (Phase 4, uncommitted — pick up here first)

These files exist on disk under `src-tauri/src/` but are **not yet declared
in `lib.rs`'s `mod` list, not compiled, not tested, not committed**:

- `cli_resume_routing.rs` — trivial port of `cli-resume-routing.ts`.
- `codex_rollouts.rs` — port of `codex-rollouts.ts` (matches a running Codex
  process to its `rollout-*.jsonl` session file by cwd or start-time
  proximity).
- `claude_sessions.rs` — port of `claude-sessions.ts` (same idea for Claude's
  `~/.claude/projects/**/*.jsonl`).
- `cli_cwd_script.rs` — port of `cli-cwd-script.ts`: just the PowerShell/C#
  string constant that reads a process's cwd from its PEB. No logic to port,
  literally a `pub const PEB_CWD_SCRIPT: &str = r#"..."#;`.
- `powershell.rs` — **new** shared helper (`encode`/`quote`/`exec_file_text`/
  `run`) for shelling out to `powershell.exe -EncodedCommand`. Note
  `desktop_processes.rs` has its own private, already-tested copy of this
  same pattern predating this file — **do not** go back and refactor
  `desktop_processes.rs` to use the new shared module; that's already-tested
  Phase 2 code and touching it isn't worth the regression risk. Only new code
  (`cli_sessions.rs` and anything else going forward) should use
  `powershell.rs`.
- `cli_sessions.rs` — port of `cli-sessions.ts`: detects running Codex/Claude
  CLI processes (via the PEB script), figures out if they're terminal-hosted
  or Orca-hosted, and on restart, terminates them, resolves their rollout/
  session file via `codex_rollouts`/`claude_sessions`, and reopens them in a
  fresh terminal (Windows Terminal, a PowerShell window, or an Orca tab).
  Uses `windows::Win32::System::Threading::OpenProcess` for the liveness
  check (`is_process_alive`) — this needed adding `windows` crate as a
  Windows-only dependency (already added to `Cargo.toml` under `[target.
  'cfg(windows)'.dependencies]`).
- `cli_hooks.rs` — port of `cli-hooks.ts` (installs the Claude Code
  statusline hook into `~/.claude/settings.json`, the Codex `status_line`
  TOML config, and the Windows Terminal "usage pane" wrapper scripts for
  `codex`/`codex.cmd`/`codex.ps1`). **Has an unresolved parameter**:
  `install_hooks(cli_js: &Path)` and `install_codex_wrapper_hook(node_exe,
  cli_js)` take the standalone CLI's `cli.js` path as a parameter rather than
  hardcoding it — see "Open decision" below, you need to resolve this before
  wiring these into the app's startup sequence.

**Your first task**: add `mod` declarations for all 7 files to `lib.rs`,
run `cargo build --tests` and fix whatever compile errors surface (there
will likely be a few small ones — I wrote these without compiling), run
`cargo test`, then commit + push. Keep the existing Phase 1/2 commit message
style (see `git log` on this branch).

## What's not started at all

In rough dependency order:

1. **`tray_pin.rs`** — port of `tray-pin.ts` (best-effort "always show tray
   icon", Win11 registry promotion + Win10 binary-blob edit). Same pattern
   as `desktop_processes.rs`: keep the original PowerShell scripts verbatim,
   just change the calling harness to Rust/`powershell.rs`.

2. **`app_notify.rs`** — port of `app-notify.ts` (the in-app toast
   notification queue/stacking/positioning math). The pure math (queue,
   stacking positions, height clamping) can be ported standalone; the actual
   `BrowserWindow`-equivalent creation needs Tauri's `WebviewWindowBuilder`
   and belongs in the window-management module below.

3. **`cli_handover.rs`** — port of `cli-handover.ts` (the "CLI sessions
   found — restart or copy resume command?" popup + orchestration around
   `cli_sessions::detect_cli_sessions`/`restart_cli_sessions`). Also needs a
   Tauri window for the `cli-restart.html` popup.

4. **The big one — a Tauri equivalent of `index.ts`** (1534 lines). This is
   the tray + 7 windows (manager, onboarding, widget, widget-settings,
   approval, notify, cli-restart) + every `ipcMain` handler + all the
   Windows-shell-specific widget positioning logic. Suggested module split
   (don't feel bound to this exactly, but don't put it all in one file):
   - `app_state.rs` — the `AppConfig` + per-provider runtime state
     (`PState` equivalent: monitor handle, last usage, cooling-down map,
     switching/handling-limit flags) that `index.ts` keeps as module-level
     mutable state. In Tauri this should live in `tauri::Manager::manage()`
     -managed state (`Mutex<AppState>` or similar), not global `static`s —
     unlike the Rust modules above, this one genuinely needs an `AppHandle`.
   - `tray.rs` — tray icon + menu building (`buildMenu`/`refreshTray`/
     `showTrayMenu`/`trayMenuPosition` — the "position the menu above the
     icon, clamped to the work area, dodging the compact widget" math).
   - `windows/manager.rs`, `windows/onboarding.rs`, `windows/widget.rs`,
     `windows/widget_settings.rs`, `windows/approval.rs`, `windows/notify.rs`,
     `windows/cli_restart.rs` — one module per window, each owning its
     `WebviewWindowBuilder` construction + the window-specific logic
     currently inlined in `index.ts` (e.g. all of the widget positioning/
     compact-mode/always-on-top/context-menu code belongs in
     `windows/widget.rs`).
   - `ipc.rs` — every `#[tauri::command]`, one-to-one with the
     `ipcMain.handle(...)`/`ipcMain.on(...)` calls in `registerIpc()`. Match
     the channel names' *intent* (not literally the Electron channel string)
     to what `preload.ts` exposes as `window.rotator.*` — see "Renderer
     bridge" below for why the channel *names* matter.
   - `win32.rs` (or fold into `windows/widget.rs`) — the two spots that
     genuinely need raw `windows` crate FFI rather than a PowerShell
     shell-out (see "Win32 FFI notes").

5. **Renderer bridge**: `src/renderer/preload.ts` exposes `window.rotator =
   {...}` via `contextBridge` + `ipcRenderer`. The 7 renderer HTML files
   (`manager.html`, `widget.html`, etc.) are plain HTML/CSS/vanilla-JS and
   call `window.rotator.*` — **do not rewrite these HTML files**. Instead,
   write a small `src/renderer/tauri-bridge.js` that defines the *same*
   `window.rotator` object shape, backed by `@tauri-apps/api`'s `invoke`/
   `listen` instead of `ipcRenderer`, and add one `<script src="tauri-
   bridge.js">` tag to each of the 7 HTML files (that's the only renderer
   edit needed). Keep every method name and argument shape in `window.
   rotator` byte-for-byte identical to what `preload.ts` currently exposes —
   read `src/main/preload.ts` for the exact list.

6. **Phase 6 — packaging**: `tauri.conf.json`'s `bundle.targets` is already
   set to `["nsis"]`. You still need to:
   - Add the compiled `dist/main/cli.js` (see "Open decision" below) to
     `bundle.resources`.
   - Reimplement `build/installer.nsh`'s custom reinstall dialog ("데이터를
     삭제하고 새로 설치할까요?") as a Tauri NSIS template hook. Tauri v2's
     NSIS bundler supports a custom template
     (`bundle.windows.nsis.template`) — you'll likely need to adapt the
     existing `installer.nsh` macros (`customInit`) rather than write from
     scratch. Read `build/installer.nsh` first; the comments there explain
     exactly what must be preserved (never touch `~/.codex-accounts` or
     `~/.claude-accounts`).
   - Regenerate proper app icons via `npx tauri icon <source-art>` — right
     now `src-tauri/icons/*` are the generic Tauri scaffold icons from
     `tauri init`, not LazySwitch's actual branding. If no source art
     exists yet, flag this back rather than guessing at a logo.

7. **Phase 7 — verification**: manual pass on Windows comparing the Tauri
   build against the Electron build's behavior, using
   `src-tauri/../README.md` / `README.ko.md` as the feature list. Pay
   particular attention to: taskbar-docked compact widget positioning across
   different DPI scaling and multi-monitor setups, the reinstall dialog
   preserving `~/.codex-accounts`/`~/.claude-accounts`, and the Windows
   toast/notification shortcut (`ensureWindowsToastShortcut` in `index.ts` —
   not yet ported at all; needs `IShellLink` COM interop via the `windows`
   crate to create/read the `.lnk` file, since Tauri has no built-in
   shortcut-file API).

## Win32 FFI notes (what genuinely needs the `windows` crate vs. PowerShell)

Most of `index.ts`'s Windows-shell logic **already shells out to
PowerShell** in the original (tray icon rect via `FindWindowW`/
`GetWindowRect` C# Add-Type, taskbar theme via registry `Get-ItemProperty`).
Port those the same way as `desktop_processes.rs` — keep the PowerShell
script text, change the caller to Rust + `powershell.rs`. You do **not**
need raw Rust FFI for those.

The two things that genuinely have no PowerShell/Electron-API equivalent
and need the `windows` crate + a raw `HWND` (obtainable from a Tauri
`WebviewWindow` via its `hwnd()` method on Windows, from `raw-window-
handle`):

1. **`WM_CONTEXTMENU` hook** (`win.hookWindowMessage` in the widget window,
   used because the drag region swallows the renderer's native context-menu
   event). Needs `SetWindowSubclass`/window-proc subclassing to intercept
   the raw message and call `showWidgetContextMenu()`.
2. **No-activate topmost reassertion** (`win.moveTop()` in the always-on-top
   re-raise timer). `set_always_on_top(bool)` on the Tauri window should
   already cover Electron's `setAlwaysOnTop(true, "screen-saver")` — on
   Windows, Electron's "level" parameter only differentiates macOS NSWindow
   levels; Windows always-on-top is just `HWND_TOPMOST` regardless of level,
   so no FFI needed there. But re-raising *without stealing focus* (as the
   100ms timer does) needs `SetWindowPos(hwnd, HWND_TOPMOST, 0,0,0,0,
   SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)` directly, since Tauri's
   `set_focus()` would steal focus.

Everything else Win32-shaped in `index.ts` (tray rect, taskbar theme,
Windows toast shortcut via `IShellLink`) either already shells out to
PowerShell (keep doing that) or needs a small, separate bit of `windows`
crate COM interop (the `.lnk` shortcut creation) — not the same
subclassing/window-message work as the two items above.

## Open decision: `cli.js` path for hook installation

`cli-hooks.ts`'s `statuslineCommand()` builds `node "<path>/cli.js"
statusline claude`, where `<path>` is `path.join(__dirname, "cli.js")` —
i.e. the *Electron app's own* installed `dist/main/cli.js`, unpacked via
`asarUnpack` in `package.json`. Since the standalone `lazyswitch` CLI stays
out of Rust-porting scope (see "Ground rules"), the Tauri app still needs
*some* copy of the compiled `dist/main/cli.js` (+ its `dist/main/**`
dependency closure) sitting next to the installed binary, referenced via
Tauri's `app.path().resource_dir()` instead of Electron's `__dirname`.

This requires:
- Keep `npm run build` (the existing `tsc` compile of `src/main/**`) as a
  pre-build step even for the Tauri app.
- Add the compiled `dist/main/**` (or just the subset `cli.js` actually
  needs — check its `require`/`import` graph) to `tauri.conf.json`'s
  `bundle.resources`.
- `install_hooks`/`install_codex_wrapper_hook` in `cli_hooks.rs` need the
  resolved resource-dir path passed in at the call site (from wherever
  `index.ts`'s `ensureCliStatusHooks()` gets ported to — the app-state/
  startup module), not resolved internally.

If this turns out to be more awkward than expected (e.g. `cli.js`'s
dependency closure is large, or asar-unpacking assumptions don't translate
cleanly), stop and flag it back — this is a real architecture fork, not a
mechanical translation, and the user should weigh in on alternatives (e.g.
shipping the CLI as a genuinely separate npm package the user installs
themselves, rather than bundling it inside the Tauri app's resources).

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
- If `cargo test` or `npx tauri build` reveals a design assumption from
  Phase 1/2 was wrong (e.g. a config field type mismatch against a real
  user's `config.json`), fix it, but leave a clear commit message explaining
  what changed and why — don't silently patch over it.
