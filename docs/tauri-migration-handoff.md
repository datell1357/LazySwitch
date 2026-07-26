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

## Current task — app_state.rs + tray.rs (first slice of the index.ts port)

The full `index.ts` port (tray + 7 windows + every IPC handler + widget
Win32 positioning) is too large for one dispatch. This is the **first
slice only** — state management and the tray icon/menu, with **no window
creation yet**. Don't go beyond this scope; flag back when it's done rather
than continuing into the window layer unscoped.

Build `app_state.rs`:
- A struct mirroring `index.ts`'s module-level mutable state: `cfg:
  AppConfig` (already loaded/saved via `config.rs`), and a per-provider
  `PState` equivalent — `monitor: Option<UsageMonitor>`, `last_usage:
  Option<UsageSnapshot>`, `cooling_down: HashMap<String, i64>` (name ->
  epoch ms), `switching: bool`, `handling_limit: bool`,
  `last_no_account_notify: i64` — for both `ProviderId::Codex` and
  `ProviderId::Claude`.
- This needs to be shared, mutable, thread-safe state reachable from tray
  menu callbacks, the monitor's callbacks, and (later) IPC commands. Use
  `tauri::Manager::manage()` with a `Mutex<AppState>` wrapped in `Arc` (or
  however you find idiomatic in Tauri v2 — check the Tauri docs/examples if
  unsure, this is the one place in this codebase that genuinely needs to
  know about `tauri::AppHandle`).
- Also port `prefsOf`/`stateOf`/`providerById` (trivial accessors) and
  `pruneCooldowns` from `index.ts`.
- Port `config:get`/`config:set`/`lang:get` as `#[tauri::command]`s in a new
  `ipc.rs` (this will grow — just these three for now). `config:set`'s
  merge logic in the original is intricate (patches `usageWidget` fields,
  handles `alwaysOnTop`/`minimized`/`compactPosition` changes, restarts
  monitors if `pollIntervalSec` changed, calls `applyLaunchAtLogin` if
  `launchAtLogin` is in the patch) — for *this* slice, port the config-merge
  logic itself faithfully, but anywhere it would currently call into window
  code (`syncUsageWidget`, `applyWidgetMinimized`, etc.) or
  `app.setLoginItemSettings`, stub it as a TODO call-out (a private no-op
  fn with a `// TODO(window-layer):` comment is fine) — those need the
  window layer / `tauri-plugin-autostart` that don't exist yet.

Build `tray.rs`:
- Port `buildMenu`, `refreshTray`, `showTrayMenu`, `trayMenuPosition`,
  `bottomRightCompactWidgetRect` (this one needs the widget window's
  bounds, which doesn't exist yet — stub it to return `None` for now, same
  TODO-call-out approach), and the tray icon/tooltip refresh logic.
- Use `tauri::tray::TrayIconBuilder` + `tauri::menu::{Menu, MenuItem,
  CheckMenuItem, Submenu}` — the existing scaffold in `lib.rs` already
  builds a minimal tray from Phase 0; replace/extend it here rather than
  building a second one.
- The language submenu (`langItem` in the original) needs radio-style menu
  items — check Tauri's `CheckMenuItem`/`IconMenuItem` API for what's
  available; if Tauri v2 doesn't have a native radio-menu-item type,
  approximate with checkmarks (single-select enforced by your own click
  handler) and note the approximation in the commit message.

Test what's testable without a real Tauri runtime (the pure math —
`trayMenuPosition`'s clamping, `pruneCooldowns`, the config-merge logic
minus the stubbed window calls) the same way you tested `app_notify.rs`.
Wire what you can into `lib.rs`'s `setup()` hook. Run `cargo build --tests`
+ `cargo test`, commit, push — same process as before.

## Not started yet (do not start these — future slices)

- The 7 windows (manager, onboarding, widget, widget-settings, approval,
  notify, cli-restart) and wiring the `ToastWindowFactory` /
  `CliHandoverDeps` real implementations to them.
- The rest of `ipc.rs` (`accounts:*`, `cli:testRestart`, `onboarding:finish`,
  `open:url`, `manager:close`, `widget:*`, `widget-settings:close`).
- The two genuine Win32 FFI spots (`WM_CONTEXTMENU` subclass hook,
  no-activate topmost reassertion) — only relevant once the widget window
  exists.
- Renderer bridge (`window.rotator` shim, one `<script>` tag per HTML file).
- Phase 6 (packaging) and Phase 7 (verification).

## Win32 FFI notes (what genuinely needs the `windows` crate vs. PowerShell)

Most of `index.ts`'s Windows-shell logic **already shells out to
PowerShell** in the original (tray icon rect via `FindWindowW`/
`GetWindowRect` C# Add-Type, taskbar theme via registry `Get-ItemProperty`,
and `tray-pin.ts`'s registry promotion). Port those the same way as
`desktop_processes.rs` — keep the PowerShell script text, change the caller
to Rust + `powershell.rs`. You do **not** need raw Rust FFI for those,
including `tray_pin.rs` (task #1 above).

The two things that genuinely have no PowerShell/Electron-API equivalent
and need the `windows` crate + a raw `HWND` (obtainable from a Tauri
`WebviewWindow` via its `hwnd()` method on Windows) — **relevant only for
task #4, not your current tasks**:

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
wire `cli_hooks.rs` into actual app startup, which is task #4 territory) —
flag it back when you get there rather than deciding unilaterally; the user
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
