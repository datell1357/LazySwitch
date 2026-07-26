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
  formatting drift across Phase 1/2 that hasn't been normalized, and
  reformatting it is out of scope for this migration (noisy diffs, not worth
  the review overhead). If you need to format new files, run rustfmt
  scoped to exactly the new file paths, and double check with `git status`
  afterward that nothing else got touched — `cargo fmt -- path/to/file.rs`
  in this workspace has been observed to reformat the *entire* crate instead
  of just that file, so verify before trusting it.
- **Environment note**: this machine's `cargo` sometimes can't reach the
  crates.io index (certificate provider issue) and needs `--offline`. If you
  hit this, prefer `--offline` over spending time debugging the network
  issue. If `--offline` resolution downgrades/upgrades unrelated transitive
  dependencies because the local registry cache is stale, it's fine to
  accept that rather than hand-editing `Cargo.lock` — just note it in the
  commit message.
- **Git worktree note**: this worktree's `.git` metadata lives in the main
  repo at `D:\Vibe Project\LazySwitch\.git\worktrees\claude_tauri\`, outside
  this worktree's own directory. If your sandbox can't write there even with
  an added directory allowance, that's a known environment quirk — flag it
  rather than working around it by, e.g., committing from a different
  working directory or altering git config.

## What's done (Phase 1, 2, 4 — committed, pushed, tested)

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
  **Deliberately keeps the original PowerShell scripts verbatim** rather
  than reimplementing via a native process-listing crate — don't "improve"
  this by switching to e.g. the `sysinfo` crate.
- `login.rs` — Codex `codex login` subprocess flow (`login.ts`).
- `switcher.rs`, `monitor.rs` — rotation/exhaustion math and the usage-polling
  loop. `UsageMonitor` runs a tokio background task and takes plain callback
  closures (`on_usage`, `on_limit_hit`) instead of depending on
  `tauri::AppHandle` directly — wire those callbacks to `app.emit(...)` /
  tray refresh when you get to the window/tray layer.
- `atomic_fs.rs` — shared atomic-write/atomic-copy helper.
- `cli_resume_routing.rs`, `codex_rollouts.rs`, `claude_sessions.rs`,
  `cli_cwd_script.rs`, `powershell.rs` (shared PowerShell-exec helper —
  `desktop_processes.rs` has its own older private copy of the same
  pattern; leave that one alone, only new code should use this shared one),
  `cli_sessions.rs`, `cli_hooks.rs` — CLI session detection/restart
  infrastructure (`cli-resume-routing.ts`, `codex-rollouts.ts`,
  `claude-sessions.ts`, `cli-cwd-script.ts`, `cli-sessions.ts`,
  `cli-hooks.ts`). `cli_hooks.rs`'s `install_hooks`/`install_codex_wrapper_hook`
  take the standalone CLI's `cli.js` path as a parameter rather than
  hardcoding it — still unresolved, see "Open decision" below.
- 91 unit tests across these modules, all passing (`cargo test`).

## Done — tray_pin.rs, app_notify.rs, cli_handover.rs

Committed as `eb2df8c`. `tray_pin.rs` ported the PowerShell scripts
verbatim as expected. `app_notify.rs` and `cli_handover.rs` both used a
dependency-injection trait (`ToastWindowFactory`, `CliHandoverDeps`) to
defer the actual Tauri window creation to a later pass — good pattern,
**keep using it** for the remaining window-shaped modules below: define the
pure logic + a small trait for "create/show/close a window and tell me
what happened", test the logic against a fake implementation of that trait,
and leave the real `WebviewWindowBuilder`-backed implementation for the
final wiring step once all the pieces exist. Minor nit if you touch
`cli_handover.rs` again: its two `use` statements ended up at the bottom of
the file after the `#[cfg(test)]` block — harmless but move them to the top
next time you're in there.

## Done — app_state.rs, ipc.rs, tray.rs (first index.ts slice)

Committed as `ecf57d6`, 108 tests passing (independently re-verified).
Good pattern worth continuing: `Mutex<AppState>` via `app.manage()`,
checkmark-based language menu (no native radio item in Tauri v2 — correct
call), `TODO(window-layer)` markers exactly where window creation would
otherwise be needed.

**One thing to fix in the next slice, not now-done code to redo**: the
tray's click behavior isn't wired to match the original yet —
`show_menu_on_left_click(true)` shows the menu on left-click via Tauri's
built-in mechanism, but the original specifically used *right*-click for
the context menu (positioned via the custom `trayMenuPosition`/
`popUpContextMenu`, not the OS default position) and *double*-click to open
the manager window. `tray_menu_position` is implemented and tested but not
actually called from anywhere yet. Wire this up properly once the manager
window exists (next task) — set `show_menu_on_left_click(false)`, handle
double-click to open the manager, and handle right-click by computing
`tray_menu_position` (you'll need the tray icon's rect and the primary
monitor's work area from Tauri's APIs, plus the widget's rect once it
exists later) and popping the menu at that computed position rather than
Tauri's default.

## Current task — manager.rs + onboarding.rs (first two windows)

The 7-window layer is being built one or two windows at a time, simplest
first. This slice is exactly: the account manager window and the
first-run onboarding window. **Not** the widget (most complex — Win32 FFI,
compact-mode positioning, save/restore bounds — gets its own dedicated
slice later) and **not** the other 4 windows yet.

Read `src/main/index.ts`'s `openManager`/`openOnboarding` functions and the
renderer files `src/renderer/manager.html` + `src/renderer/onboarding.html`
(read-only — do not edit the HTML) to understand what each window needs
from the backend.

Build `windows/manager.rs` and `windows/onboarding.rs` (new `windows/`
directory under `src-tauri/src/`, add `mod windows;` with a `pub mod
manager; pub mod onboarding;` in `windows/mod.rs`):

- Each should have an `open_manager(app: &AppHandle)` / `open_onboarding(app:
  &AppHandle)` function that: if the window already exists (by label — use
  Tauri's `app.get_webview_window("manager")` / `"onboarding"`), restore/
  show/focus it (mirroring `if (managerWin && !managerWin.isDestroyed())`
  in the original); otherwise build one with `WebviewWindowBuilder`
  matching the original's `BrowserWindow` options (width/height/resizable/
  frame:false/title/backgroundColor — check the exact values in
  `index.ts`), pointing at the existing HTML file (`manager.html` /
  `onboarding.html` — the frontend is untouched, still plain HTML/JS; don't
  worry about the `window.rotator` bridge not existing yet, that's a
  separate later task, the window should still open and load the page even
  though the page's JS calls will fail silently until the bridge exists).
  Match the original's "force-show once ready" `ready-to-show` handling.
- `onboarding`'s close handler needs to call back into syncing the usage
  widget (`syncUsageWidget()` in the original) — that widget doesn't exist
  yet, so stub it as `TODO(window-layer): sync usage widget` same as
  before.
- Wire `tray.rs`'s `"manage"` and `"tutorial"` menu-item handlers (currently
  `TODO(window-layer)` stubs) to call `open_manager`/`open_onboarding`.
- Wire the tray's double-click behavior to open the manager (see the "one
  thing to fix" note above) — this is the natural point to do that since
  the manager window now exists.
- `app.whenReady()`'s onboarding-vs-manager decision at startup (`if
  (!cfg.onboarded) openOnboarding(); else if (total < 2) openManager();`)
  should also get wired into `lib.rs`'s `setup()` now.

You do not need to implement the manager's or onboarding's IPC handlers
yet (`accounts:list`, `accounts:switch`, `onboarding:finish`, etc.) — those
belong in a later `ipc.rs` expansion pass once more of the window layer
exists. Getting the windows to *open* correctly (with the tray wiring) is
this slice's job; the pages will render but most buttons on them won't do
anything yet, and that's expected at this stage.

Same process as always: test what's testable without a real window (little
to test here beyond maybe a pure "should we show onboarding or manager at
startup" decision function — extract that as testable pure logic if you
can), `cargo build --tests` + `cargo test`, commit, push. Flag back if this
turns out bigger than expected rather than plowing into windows #3-7
unscoped.

## Not started yet (do not start these — future slices, after manager+onboarding)

- The widget window (compact/full modes, save/restore bounds, taskbar
  docking via tray-rect PowerShell query, taskbar theme detection, the two
  genuine Win32 FFI spots — `WM_CONTEXTMENU` hook and no-activate topmost
  reassertion).
- widget-settings, approval, notify, cli-restart windows (wiring the
  `ToastWindowFactory`/`CliHandoverDeps` real implementations to notify/
  cli-restart specifically).
- The rest of `ipc.rs` (`accounts:*`, `cli:testRestart`, `onboarding:finish`,
  `open:url`, `manager:close`, `widget:*`, `widget-settings:close`).
- `tauri-plugin-autostart` wiring for `launchAtLogin`.
- Renderer bridge (`window.rotator` shim, one `<script>` tag per HTML file)
  — needed before any of these windows' pages actually do anything, but not
  before they at least *open*.
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
`WebviewWindow` via its `hwnd()` method on Windows) — **only relevant once
the widget window is being built, not the current task**:

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
there rather than deciding unilaterally; the user
should weigh in on alternatives (e.g. shipping the CLI as a genuinely
separate npm package).

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
  commit. This bit us once already (an accidental whole-crate `cargo fmt`
  during the Phase 4 commit, caught and reverted before it landed).
