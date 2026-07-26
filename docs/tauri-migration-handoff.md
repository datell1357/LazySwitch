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
- Run `cargo test` (and where relevant `cargo build --tests`) before
  committing. **Also run `npx tauri build --debug` and launch the resulting
  `app.exe` (background + `tasklist` check that it's still alive after a
  few seconds, then stop that PID) before committing anything that touches
  startup/`setup()` wiring** — unit tests alone missed a real startup panic
  once already (spawning a monitor via `tokio::spawn` outside an active
  Tokio reactor, since `setup()` is a plain sync callback) that only this
  smoke-launch caught. Don't skip it for anything that touches `lib.rs`'s
  `setup()` or anything spawned from it.
- **Do not run `cargo fmt` on the whole crate.** It has pre-existing
  formatting drift across earlier phases that hasn't been normalized, and
  reformatting it is out of scope for this migration (noisy diffs, not worth
  the review overhead). If you need to format new files, run rustfmt
  scoped to exactly the new file paths, and double check with `git status`
  afterward that nothing else got touched — `cargo fmt -- path/to/file.rs`
  in this workspace has repeatedly been observed to reformat the *entire*
  crate instead of just that file (this has happened six times now), so
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
- **Verification note**: Claude independently rebuilds/retests/relaunches
  every reported "done" (not just trusting the report), and periodically
  greps compile warnings for functions that look "never used" — that's how
  the monitor-wiring gap and the startup panic got caught. Expect this
  between dispatches; it's due diligence given how safety/behavior-sensitive
  this app is (account credentials, process termination, etc.), not a sign
  anything is wrong.

## Done so far (see `git log` for exact commits)

`src-tauri/src/`:
- `config.rs`, `i18n.rs`, `paths.rs`, `accounts.rs`, `provider_types.rs`,
  `provider.rs` (match-based `ProviderId` dispatcher, not a trait object —
  **follow this pattern**, don't introduce trait objects for provider-generic
  code), `providers/codex.rs`, `providers/claude.rs`, `codex_api.rs`,
  `desktop.rs`, `desktop_processes.rs` (PowerShell kept verbatim — don't
  "improve" with e.g. `sysinfo`), `login.rs`, `switcher.rs`, `monitor.rs`,
  `atomic_fs.rs`, `cli_resume_routing.rs`, `codex_rollouts.rs`,
  `claude_sessions.rs`, `cli_cwd_script.rs`, `powershell.rs` (shared
  PowerShell-exec helper — `desktop_processes.rs` has its own older private
  copy, leave that one alone), `cli_sessions.rs`, `cli_hooks.rs` (fully
  implemented but not wired anywhere — see "Open decision" below),
  `tray_pin.rs` (implemented, **also not wired anywhere yet** — small,
  see "Current task" below, folded in as a quick addendum),
  `app_notify.rs`, `cli_handover.rs` (both use a dependency-injection trait,
  `ToastWindowFactory`/`CliHandoverDeps`, to defer Tauri window creation —
  **keep using this pattern** for anything similar), `app_state.rs`,
  `ipc.rs`, `tray.rs` (checkmark-based language menu — Tauri v2 has no
  native radio item), `limit_handler.rs` (monitor wiring + the
  cooldown/pick-next/switch/approve/handover orchestration — snapshots
  `AppState` and releases the `Mutex` guard before any `.await`, reacquiring
  only briefly for updates — **follow this same lock discipline** for any
  new async code touching `AppState`).
- All 7 windows exist: `windows/{manager,onboarding,approval,notify,
  cli_restart,widget,widget_settings}.rs`. Widget is full-mode only so far
  (no compact mode/taskbar docking/Win32 FFI yet — see "Current task").
- `ipc.rs` now has the full `accounts:*`/`providers:list`/`cli:testRestart`/
  `onboarding:finish`/`open:url`/`manager:close`/`config:*`/`lang:get`
  command set, all registered in `lib.rs`.
- **The app's core function now actually works**: monitors are started per
  provider at startup (wired through Tauri's async runtime so the startup
  spawn has an active reactor — this was the panic mentioned above),
  `limit_handler` handles threshold/error limit-hits with cooldown tracking,
  candidate verification, switching, the real approval popup, and CLI
  session handover.
- 118 unit tests, all passing, independently re-verified — as is the full
  `npx tauri build --debug` + launched `app.exe` staying alive (down to 43
  compiler warnings from 294, confirming most previously-dead code is now
  wired; the remaining ones are all expected/tracked deferrals below).

**Known deferred items, not bugs** (tracked, not regressions):
- Tray right-click positioning (`tray_menu_position` implemented/tested,
  never called — Tauri has no public screen-positioned tray popup API
  without a native owner window; revisit once the widget window, which is
  always-on, can serve as that owner).
- `cli_hooks::install_hooks` never called — blocked on the `cli.js`
  resource-path open decision, deferred to Phase 6 (packaging).
- `windows::widget_settings::open_widget_settings` never called — waiting
  on the widget's own right-click context menu, which is this task.

## Current task — widget part 2: compact mode + tray_pin wiring

Two independent, both-small pieces bundled into one dispatch since neither
needs its own round-trip:

**1. Widget compact mode** (the bulk of this task). Read
`src/main/index.ts`'s remaining widget functions not yet ported:
`isTaskbarCompactWidget`, `applyWidgetTaskbarTheme`,
`sendWidgetTaskbarTheme`, `getTaskbarTheme`/`queryTaskbarTheme`/
`readTaskbarTheme` (registry-based, port the PowerShell verbatim — see
`TASKBAR_THEME_POWERSHELL` — same pattern as `desktop_processes.rs`),
`queryTrayNotifyRect`/`readPhysicalRect` (PowerShell `FindWindowW`/
`GetWindowRect` C# Add-Type — also port verbatim), `isBottomTaskbar`,
`compactTaskbarBounds`, `compactBottomRightBounds`,
`compactBottomLeftBounds`, `positionCompactWidget`,
`widgetInitialCompactBounds`, `applyWidgetMinimized`, `recreateUsageWidget`
(transparency is construction-time-only in the original, so switching
modes recreates the window — same constraint likely applies to Tauri's
`transparent()` builder option, confirm), `showWidgetContextMenu`,
`setWidgetContextMenuHooked` (this is the `WM_CONTEXTMENU` hook — see
Win32 FFI notes), the always-on-top reassertion timer in
`openUsageWidgetWindow` (`reassertWidgetAlwaysOnTop`, the 100ms
`setInterval` — see Win32 FFI notes for the no-activate `SetWindowPos`
needed here), and `widget:compact-height` (the one IPC command skipped in
part 1).

Add the `windows` crate FFI for the two spots in "Win32 FFI notes" below
now — this is the task that actually needs them. Get the tray icon's HWND
and the widget window's HWND via Tauri's `hwnd()` (Windows-only).

Wire the `usage-widget` tray toggle and `config:set`'s remaining
`compactPosition`/`minimized` side effects (previously TODO-stubbed) now
that compact mode exists.

**2. `tray_pin` wiring** (small addendum): in `lib.rs`'s `setup()`, after
tray creation, spawn a delayed call to `tray_pin::promote_tray_icon` — the
original does `setTimeout(() => promoteTrayIcon(), 4000)` (the tray icon's
registry record only exists after being shown once). Use `tokio::spawn` +
`tokio::time::sleep` (through the async runtime the same way monitor
wiring now is, to avoid repeating the startup-panic mistake) or
`std::thread::spawn` + `std::thread::sleep` if simpler — your call, just
make sure it doesn't block `setup()` itself.

Test the pure parts (bounds math, taskbar-theme registry-value parsing —
`readTaskbarTheme`'s luminance computation is pure and testable, same as
before). Run the full verification sequence (`cargo test` +
`tauri build --debug` + launch-and-check `app.exe`). Commit, push. If this
is bigger than expected, it's fine to split the Win32 FFI part into its
own follow-up commit — flag back rather than guessing at scope.

## Not started yet (do not start these — future slices, after this one)

- `tauri-plugin-autostart` wiring for `launchAtLogin`.
- `cli_hooks::install_hooks` startup wiring — Phase 6 (packaging), bundled
  with the `cli.js` resource-path decision.
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
`WebviewWindow` via its `hwnd()` method on Windows) — **relevant now, this
is the task that needs them**:

1. **`WM_CONTEXTMENU` hook** (`win.hookWindowMessage` in the widget window,
   used because the drag region swallows the renderer's native
   context-menu event). Needs `SetWindowSubclass`/window-proc subclassing
   to intercept the raw message and call the context-menu-show logic.
2. **No-activate topmost reassertion** (`win.moveTop()` in the always-on-top
   re-raise timer). Needs `SetWindowPos(hwnd, HWND_TOPMOST, 0,0,0,0,
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
Electron's `__dirname`. Not your task right now — this is Phase 6
(packaging) territory, bundled together with the NSIS installer work.

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
