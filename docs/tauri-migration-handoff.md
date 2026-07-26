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

## What's not started at all

In rough dependency order — **your current task is just #1-#3**, don't
start #4 yet (it's large enough to deserve its own dedicated handoff update
once you get here; flag back when #1-#3 are done rather than continuing
into it unscoped):

1. **`tray_pin.rs`** — port of `tray-pin.ts` (best-effort "always show tray
   icon", Win11 registry promotion + Win10 binary-blob edit). Same pattern
   as `desktop_processes.rs`: keep the original PowerShell scripts verbatim,
   just change the calling harness to Rust/`powershell.rs` (the new shared
   one, since this is new code).

2. **`app_notify.rs`** — port of `app-notify.ts`'s *pure logic only*: the
   toast queue (`queue`/`payloads`/`activeToasts` bookkeeping), the stacking
   position math (`positionToasts`), height clamping (`clampHeight`), and
   the draining logic (`drainQueue`). Do **not** create the actual Tauri
   window yet — stub the "create and show a toast window" step behind a
   function signature/trait that the not-yet-written window-management
   module (task #4) will implement later; the queue/math logic is what's
   worth porting and testing now, the `BrowserWindow`-equivalent
   construction belongs with the rest of the window layer.

3. **`cli_handover.rs`** — port of `cli-handover.ts`'s orchestration logic:
   `providerName`, `notifyCliRestart`, and the `handle`/`schedule`/`detect`
   flow that decides whether to auto-restart CLI sessions or ask the user
   first, calling into `cli_sessions::detect_cli_sessions` /
   `restart_cli_sessions` (already ported). Same deal as `app_notify.rs`:
   the `askRestart` popup window creation is out of scope here, stub it
   behind a function signature the window layer will implement.

4. **(Not your task yet)** A Tauri equivalent of `index.ts` (1534 lines):
   tray + 7 windows + every `ipcMain` handler + the Windows-shell-specific
   widget positioning logic, plus wiring the stubbed window-creation points
   from #2 and #3. This needs its own scoping pass once #1-#3 land — flag
   back rather than guessing at the module split yourself.

5. **(Not your task yet)** Renderer bridge (`window.rotator` shim backed by
   `@tauri-apps/api` instead of `ipcRenderer`, one `<script>` tag added to
   each of the 7 existing HTML files — do not rewrite the HTML files
   themselves).

6. **(Not your task yet)** Phase 6 (NSIS packaging parity, icon generation)
   and Phase 7 (manual verification pass).

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
