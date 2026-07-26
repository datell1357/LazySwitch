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
  crate instead of just that file (this has happened three times now), so
  always verify before trusting it, and if it happens, stop and ask before
  restoring rather than committing through it.
- **Environment note**: this machine's `cargo` sometimes can't reach the
  crates.io index (certificate provider issue) and needs `--offline`. If you
  hit this, prefer `--offline` over spending time debugging the network
  issue. If `--offline` resolution downgrades/upgrades unrelated transitive
  dependencies because the local registry cache is stale, it's fine to
  accept that rather than hand-editing `Cargo.lock` — just note it in the
  commit message.
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
- `windows/manager.rs`, `windows/onboarding.rs` — the first two of 7 windows.
  Tray menu items and double-click now open them; startup onboarding-vs-
  manager routing is wired into `lib.rs`.
- 111 unit tests across these modules, all passing (`cargo test`) — verified
  independently, not just via your own report, at each step so far.

**Known deferred item, not a bug**: tray right-click positioning
(`tray_menu_position` in `tray.rs` is implemented/tested but never called).
Investigation found Tauri has no public screen-positioned tray popup API
without a native owner window; a real window's `popup_menu_at` might work
once one exists. Revisit once the widget window (always-on, unlike manager/
onboarding) exists.

## Current task — approval.rs, notify.rs, cli_restart.rs (remaining small windows)

Three more windows, all smaller/simpler than the widget (which still comes
last). These three specifically complete the dependency-injection seams
already stubbed in earlier phases:

- **`windows/notify.rs`**: the real `ToastWindowFactory` implementation for
  `app_notify.rs`, backed by an actual `WebviewWindowBuilder` pointed at
  `src/renderer/notify.html`. Match the original `createToastWindow`'s
  options (transparent, frameless, always-on-top, skip-taskbar,
  `focusable: false`, `show: false` initially) and `positionToasts`'s
  bounds-setting (`app_notify.rs`'s `toast_bounds` already computes the
  positions — this file just needs to apply them to real windows and read
  the real work area from Tauri's monitor API instead of a passed-in
  `WorkArea` struct).
- **`windows/approval.rs`**: the "restart Codex Desktop?" popup
  (`askApproval` in `index.ts`) — this one doesn't have a pre-built
  trait/seam from an earlier phase, so read `index.ts`'s `askApproval`
  function directly and port it: a frameless/transparent/always-on-top
  window loading `approval.html` with query-string params, resolving a
  promise/future when the user responds or closes it.
- **`windows/cli_restart.rs`**: the real `CliHandoverDeps` implementation
  for `cli_handover.rs` — the `askRestart` popup loading `cli-restart.html`,
  plus wiring `copy_to_clipboard` (check what clipboard crate/API is
  available — Tauri has a clipboard plugin, or `arboard` directly) and
  `notify` (should call into the real `notify.rs` toast window plus, per
  the original's `notify()` helper in `index.ts`, also a native OS
  notification via `tauri-plugin-notification` — check if that plugin is
  already a dependency; if not, you'll need to add it).

None of these three need new IPC handlers beyond what might be required for
the popup's own response channel (`approval:respond`, `cli-restart:respond`,
`app-notify:resize`/`dismiss` in the original — check `preload.ts` for the
exact channel names/shapes, since the renderer bridge task later will need
to match these exactly). Add `#[tauri::command]`s for those specifically
(not the unrelated `accounts:*`/`config:*` ones, those come later).

Test what's testable as pure logic (little new here beyond what's already
tested — `app_notify.rs`'s math is already covered). Wire into `lib.rs`.
`cargo build --tests` + `cargo test`, commit, push. Flag back if this turns
out bigger than expected rather than pushing into the widget window
unscoped.

## Not started yet (do not start these — future slices, after approval/notify/cli-restart)

- The widget window — last and most complex: compact/full modes, save/
  restore bounds, taskbar docking via tray-rect PowerShell query, taskbar
  theme detection, the two genuine Win32 FFI spots (`WM_CONTEXTMENU` hook,
  no-activate topmost reassertion), widget-settings window.
- The rest of `ipc.rs` (`accounts:*`, `cli:testRestart`, `onboarding:finish`,
  `open:url`, `manager:close`, `widget:*`, `widget-settings:close`).
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
