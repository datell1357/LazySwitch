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
  crate instead of just that file (this has happened five times now), so
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
- **Verification note**: Claude independently rebuilds/retests every
  reported "done" (not just trusting the report), and periodically runs
  `npx tauri build --debug` + launches the actual `app.exe` to check it
  doesn't crash and to grep for functions that compile-warn as "never
  used" — that's how gaps like the one below get caught. Expect this kind
  of check between dispatches; it's not a sign anything is wrong, just due
  diligence given how much of this app is safety/behavior-sensitive
  (account credentials, process termination, etc.).

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
  `tauri::AppHandle` directly. **Not started/started anywhere yet** — see
  "Current task" below, this is the main gap it fills.
- `atomic_fs.rs` — shared atomic-write/atomic-copy helper.
- `cli_resume_routing.rs`, `codex_rollouts.rs`, `claude_sessions.rs`,
  `cli_cwd_script.rs`, `powershell.rs` (shared PowerShell-exec helper —
  `desktop_processes.rs` has its own older private copy of the same
  pattern; leave that one alone, only new code should use this shared one),
  `cli_sessions.rs`, `cli_hooks.rs` — CLI session detection/restart
  infrastructure. `cli_hooks.rs`'s `install_hooks` is fully implemented but
  **never called anywhere yet** (see "Open decision" below — its `cli.js`
  path parameter is unresolved, so wiring it is deliberately deferred to
  Phase 6/packaging, not this task).
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
  `windows/notify.rs`, `windows/cli_restart.rs`, `windows/widget.rs`
  (full-mode only), `windows/widget_settings.rs` — 7 of 7 windows now
  *exist and open*, but several are inert: the manager/onboarding pages
  have no `accounts:*` IPC or renderer bridge to call yet, and
  `windows/widget_settings.rs`'s `open_widget_settings` /
  `windows/cli_restart.rs`'s `TauriCliHandoverDeps` are never actually
  invoked by anything (confirmed via `cargo build`'s "never used"
  warnings + grep) — both are waiting on the monitor/limit-hit wiring and
  the widget's own context menu (widget-part2) respectively. This is
  expected, not a regression — noting it so the "current task" below makes
  sense.
- 116 unit tests across these modules, all passing (`cargo test`) — verified
  independently, not just via your own report, at each step so far. The
  whole app also builds via `npx tauri build --debug` and the resulting
  `app.exe` launches without crashing (tray icon shows, stays resident).

**Known deferred item, not a bug**: tray right-click positioning
(`tray_menu_position` in `tray.rs` is implemented/tested but never called).
Investigation found Tauri has no public screen-positioned tray popup API
without a native owner window; a real window's `popup_menu_at` might work
once one exists. Revisit once the widget window (always-on, unlike manager/
onboarding) exists.

## Current task — monitor wiring, limit-hit handling, and the rest of ipc.rs

**Reprioritized ahead of the widget's compact-mode slice**: an independent
check (building + running the app, then grepping for functions that
compile-warn as unused) found that the app's actual core function — polling
usage and auto-switching accounts when a limit is hit — isn't wired at all
yet. `UsageMonitor` is never started anywhere, and `handleLimit`/
`onLimitHit` (cooldown tracking, picking the next account, switching,
asking approval, handing off CLI sessions) doesn't exist in Rust yet, even
though the pieces it needs (`switcher.rs`, `windows/approval.rs`,
`cli_handover.rs` + `windows/cli_restart.rs`) are all already built and
tested. This task wires them together. Widget compact-mode/Win32 FFI is
still coming, just after this.

Read `src/main/index.ts` in full again for: `wireMonitors`,
`restartMonitors`, `manualSwitch`, `askApproval` (already built as
`windows/approval.rs` — this task just needs to *call* it),
`onLimitHit`/`handleLimit`, `broadcastChanged`, `listWithUsage`, and the
`accounts:*`/`providers:list`/`cli:testRestart`/`onboarding:finish`/
`open:url`/`manager:close` handlers inside `registerIpc`.

1. **Wire the monitors.** In `lib.rs`'s `setup()` (after
   `ensure_live_enrolled`), for each `ProviderId` with enrolled accounts or
   live auth, construct a `monitor::UsageMonitor`, `start()` it with:
   - `on_usage`: update `AppState`'s `ProviderState.last_usage` and call
     `tray::refresh_tray`.
   - `on_limit_hit`: spawn (`tokio::spawn` or similar) the `handle_limit`
     flow described below — don't block the monitor's own loop on it.
   Store the `UsageMonitor` handle in `ProviderState.monitor` so it can be
   stopped/restarted later (`restartMonitors` — needed when
   `pollIntervalSec` changes via `config:set`, which `ipc.rs` already has a
   `TODO` for restarting monitors; check if that TODO already exists and
   wire it to actually call restart now).

2. **Port `handle_limit`** (new function, put it in `app_state.rs` or a new
   `limit_handler.rs` — your call, but keep it out of `tray.rs`/`ipc.rs`
   which are already large): prune expired cooldowns, park the current
   account in `cooling_down` on threshold/error, loop
   `switcher::pick_next_account` + a live `fetch_usage` check to skip
   still-exhausted candidates (mirroring the original's verify-before-commit
   loop), notify if nothing's available (throttled to once per 15 min, same
   as `lastNoAccountNotify`), otherwise `cli_handover::detect` +
   `switcher::switch_to` + notify, then (if the provider has desktop
   integration) `prefs.autoApprove` or call `windows::approval`'s real popup,
   then restart desktop if approved, then `cli_handover::schedule` in a
   `finally`-equivalent (make sure this runs even if the desktop-restart
   step errors, same as the original's `try/finally`).

3. **`ipc.rs` — add the remaining commands**: `providers_list`,
   `accounts_list` (mirrors `listWithUsage` — cached usage now, kick off a
   background live-fetch that calls `broadcastChanged`-equivalent if it
   differs, matching the original's `pendingUsageRefreshes` dedup-by-key
   set), `accounts_switch` (mirrors `manualSwitch`), `accounts_set_enabled`,
   `accounts_remove`, `accounts_rename`, `accounts_import_current`,
   `accounts_add_via_login`, `cli_test_restart`, `onboarding_finish`,
   `open_url` (use the `open` crate, already a dependency), `manager_close`.
   Register all of these in `lib.rs`'s `generate_handler!` list.

4. Anywhere this touches a `TODO(window-layer)` stub left in `tray.rs` or
   `windows/onboarding.rs` that's now resolvable (e.g. `usage-widget`
   toggle calling `sync_usage_widget`, if not already wired — check first),
   resolve it; leave anything still genuinely blocked on widget-part2
   or `cli.js`/autostart as-is.

Test the pure parts of `handle_limit` (the cooldown-pruning/pick-next-loop
logic, using the existing fake-provider-usage test hooks
`ROTATOR_FAKE_*_PCT` env vars from `monitor.rs`'s tests, or a similar
seam) the same rigor as before. `cargo build --tests` + `cargo test`,
then also run `npx tauri build --debug` once and confirm the resulting
`app.exe` still launches without crashing (a quick background run +
`tasklist` check is enough, no need for a full manual QA pass — Phase 7
covers that later). Commit, push.

If this task turns out considerably bigger than expected (it's a real
chunk — `handle_limit` alone is intricate), it's fine to split it into two
commits (e.g. monitor-wiring-and-handle_limit first, accounts IPC second)
rather than one giant one — use your judgment on the natural seam, just
keep each commit buildable/testable on its own.

## Not started yet (do not start these — future slices, after this one)

- Widget part 2: compact mode, taskbar docking (tray-rect PowerShell
  query + taskbar theme detection — port the PowerShell verbatim, see
  Win32 FFI notes below), the two genuine Win32 FFI spots (`WM_CONTEXTMENU`
  hook, no-activate topmost reassertion), the widget's own right-click
  context menu (`showWidgetContextMenu` — this is what would eventually
  call `open_widget_settings`).
- `tauri-plugin-autostart` wiring for `launchAtLogin`.
- `cli_hooks::install_hooks` startup wiring — blocked on the `cli.js`
  resource-path open decision below; bundled into Phase 6 (packaging).
- Renderer bridge (`window.rotator` shim, one `<script>` tag per HTML file)
  — needed before the manager/onboarding/widget pages' IPC calls actually
  do anything, even once the IPC commands above exist.
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
Electron's `__dirname`. Not your task right now — this is Phase 6
(packaging) territory, bundled together with the NSIS installer work,
since it's fundamentally a "what do we ship" packaging decision, not a
code-porting one.

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
