# LazySwitch Electron → Tauri migration: handoff to Codex

Written by Claude (planning/advisor role) for Codex to execute against.
Worktree: `D:\Vibe Project\claude_tauri`, branch `claude_tauri`, pushed to
`origin/claude_tauri`. This is a **separate** worktree/branch from the
earlier `tauri` branch at `.omo/worktrees/tauri` — that one was explicit
prior work the user asked to ignore; do not merge from it or reference it.

The Electron source of truth being ported lives at `src/main/**` and
`src/renderer/**` in this same worktree (unchanged — still builds/runs as
the Electron app too). Read the relevant `.ts`/`.html` file before touching
any Rust module or renderer file that claims to port/bridge it.

## Ground rules established so far

- **Full Rust rewrite**, no Node sidecar. **Windows-first**; macOS deferred
  (don't delete existing `cfg!(target_os = "macos")` branches, just don't
  spend effort on them).
- **Full 1:1 fidelity** on Windows-shell-specific UI behavior — already
  delivered (widget compact mode, taskbar docking, `WM_CONTEXTMENU`
  subclassing, no-activate topmost via the `windows` crate).
- The standalone `lazyswitch` CLI (`package.json`'s `bin` field,
  `src/main/cli.ts` + friends) is **out of scope** — stays a Node script.
  Its JSON config schema (`config.rs`) must not change.
- **Renderer HTML files are normally off-limits — this task is the one
  explicit exception.** See "Current task" below: you're authorized to add
  exactly one `<script>` tag near the top of each of the 7 existing HTML
  files. Do not otherwise edit their markup, CSS, or inline JS — the pages'
  own JS already calls `window.rotator.*`, and your job is to make that
  object exist and work, not to touch what calls it.
- Commit and push after each meaningfully-complete unit of work. Look at
  `git log` for the expected commit message style (explains *why*, calls
  out judgment calls).
- Run `cargo test` + `cargo build --tests`. **Also run `npx tauri build
  --debug` and launch the resulting `app.exe` (background + `tasklist`
  check it's alive after a few seconds, then stop that PID) before
  committing anything touching startup/`setup()` wiring** — this caught a
  real startup panic once already that unit tests alone missed.
- **Do not run `cargo fmt` on the whole crate.** `cargo fmt -- path/to/
  file.rs` in this workspace has repeatedly reformatted the *entire* crate
  instead of just that file (six times now) — always verify with `git
  status` after running it on anything, and if it happens, stop and ask
  before restoring rather than committing through it.
- **Environment note**: `cargo`'s crates.io access is sometimes blocked
  (certificate provider issue) but it's intermittent — try online first,
  fall back to `--offline` only if it actually fails.
- **Git worktree note**: this worktree's `.git` metadata lives in the main
  repo at `D:\Vibe Project\LazySwitch\.git\worktrees\claude_tauri\`. Any
  git write (including `git restore`) can hit a sandbox permission wall
  there — flag it back for a re-invocation with sandbox bypass, don't work
  around it yourself.
- **Verification note**: Claude independently rebuilds/retests/relaunches
  every reported "done", and greps compile warnings for "never used" —
  that's how two real gaps (unwired monitor, a startup panic) got caught
  already. Expect this between dispatches.

## Done so far (see `git log` for exact commits)

The entire backend is done: `config.rs`, `i18n.rs`, `paths.rs`,
`accounts.rs`, `provider_types.rs`, `provider.rs` (match-based `ProviderId`
dispatcher — **follow this pattern**, no trait objects for provider-generic
code), `providers/{codex,claude}.rs`, `codex_api.rs`, `desktop.rs`,
`desktop_processes.rs` (PowerShell kept verbatim), `login.rs`,
`switcher.rs`, `monitor.rs`, `atomic_fs.rs`, the whole CLI-session stack
(`cli_resume_routing.rs`, `codex_rollouts.rs`, `claude_sessions.rs`,
`cli_cwd_script.rs`, `powershell.rs`, `cli_sessions.rs`, `cli_hooks.rs` —
implemented but not wired to startup, see "Open decision"), `tray_pin.rs`,
`app_notify.rs`, `cli_handover.rs` (dependency-injection trait pattern —
**keep using it** for anything similar), `app_state.rs`, `tray.rs`,
`limit_handler.rs` (monitor wiring + cooldown/pick-next/switch/approve/
handover orchestration — snapshots `AppState`, releases the `Mutex` guard
before any `.await`, reacquires only briefly — **follow this lock
discipline**), and all 7 windows: `windows/{manager,onboarding,approval,
notify,cli_restart,widget,widget_settings,widget_geometry,widget_native,
widget_taskbar}.rs`. Widget includes real Win32 FFI (`WM_CONTEXTMENU`
subclass hook, no-activate topmost `SetWindowPos`) — Claude reviewed the
unsafe blocks directly given the risk; it's correct (unwind-guarded
callback, no borrowed pointers in ref-data, paired subclass install/remove,
unconditional `DefSubclassProc` forwarding).

`ipc.rs` has the full command set, all registered in `lib.rs`:
`config_get`, `config_set`, `lang_get`, `providers_list`, `accounts_list`,
`accounts_switch`, `accounts_set_enabled`, `accounts_remove`,
`accounts_rename`, `accounts_import_current`, `accounts_add_via_login`,
`cli_test_restart`, `onboarding_finish`, `open_url`, `manager_close`,
`windows::approval::approval_respond`,
`windows::cli_restart::{cli_restart_payload,cli_restart_respond}`,
`windows::notify::{app_notify_payload,app_notify_resize,app_notify_dismiss}`,
`windows::widget::{widget_close,widget_compact_height}`,
`windows::widget_settings::widget_settings_close`. Events emitted:
`accounts:changed` (to `manager`/`usage-widget`/`widget-settings` windows,
via `limit_handler::broadcast_changed`), `widget:taskbar-theme` (to
`usage-widget`, via `widget_taskbar.rs`), `login:url` (to `manager`, via
`accounts_add_via_login`).

123 unit tests, all passing, independently re-verified — as is the full
`npx tauri build --debug` + launched `app.exe` staying alive. The app's
core function (usage monitoring -> auto-switch) actually works now.

**Known deferred items, not bugs**: tray right-click positioning
(`tray_menu_position`, implemented/tested, never called — no Tauri API for
a screen-positioned tray popup without a native owner window);
`cli_hooks::install_hooks` never called (blocked on the `cli.js`
resource-path decision, Phase 6); `tauri-plugin-autostart` for
`launchAtLogin` not wired (`apply_launch_at_login_stub` is a deliberate
no-op).

## Current task — the renderer bridge

Every backend IPC command now exists, but nothing in the 7 renderer HTML
pages can reach it yet — they're plain, untouched Electron-era HTML/JS
that calls `window.rotator.*`, which no longer exists in a Tauri webview.
This task creates that object, backed by `@tauri-apps/api` instead of
`contextBridge`/`ipcRenderer`.

Read `src/main/preload.ts` in full — it's the exact contract to replicate,
method-for-method, argument-for-argument. Read `package.json` to check
whether `@tauri-apps/api` is already an npm dependency (it should be, from
the Phase 0 scaffold) and what version/import style is available (ESM
`import { invoke } from "@tauri-apps/api/core"` / `import { listen } from
"@tauri-apps/api/event"` is the v2 pattern — verify against what's actually
installed rather than assuming). Also skim each of the 7 renderer HTML
files (`src/renderer/{manager,onboarding,widget,widget-settings,approval,
notify,cli-restart}.html`) to see how they currently load `preload.ts`'s
output (they don't directly — preload is Electron's separate preload
script, invisible to the page's own `<script>` tags; the pages just assume
`window.rotator` already exists as a global) so you match load-order
expectations (the bridge script must run and populate `window.rotator`
*before* each page's own inline `<script>` block runs, i.e. add your
`<script>` tag before the page's existing script tag(s), not after).

Steps:
1. Create `src/renderer/tauri-bridge.js` — a single shared script (module or
   plain script, your call based on what the pages' existing script tags
   use — check if they're `type="module"` or classic) that builds
   `window.rotator = { ... }` with every method `preload.ts` exposes,
   mapped to `invoke("<command_name>", { ...args })` calls against the
   actual Rust command names/argument names above (they're snake_case
   Rust identifiers now, not the old colon-namespaced Electron channel
   names — e.g. `accounts:switch` -> `invoke("accounts_switch", { pid,
   name })`). For the `on*` methods (`onChanged`, `onWidgetTaskbarTheme`,
   `onLoginUrl`), use `listen("accounts:changed", () => cb())` etc. against
   the event names listed above (those event *names* are unchanged from
   the original, only the command names changed).
2. Add exactly one `<script src="tauri-bridge.js">` tag to each of the 7
   HTML files, positioned so it runs before the page's own script that
   uses `window.rotator`.
3. Double-check argument shapes carefully — e.g. `accounts_set_enabled`
   also takes an implicit `window` parameter Tauri auto-injects (checked
   against `window.label() != "manager"` in `ipc.rs` — you don't need to
   pass that from JS, Tauri supplies it automatically for a `WebviewWindow`
   parameter), and `config_set`'s `patch` argument is a raw JSON value, not
   a typed struct — pass whatever the page already builds. Check
   `ProviderId`'s serde representation (`#[serde(rename_all = "lowercase")]`
   in `provider_types.rs` — so JS should just pass `"codex"`/`"claude"`
   strings, matching what the original passed as `pid: string` anyway).

Verify however you reasonably can — at minimum, `npx tauri build --debug`,
launch `app.exe`, and use the tray menu to open the manager window,
confirming (via a quick screenshot or by checking the process doesn't
immediately show a broken/blank page — you may not have full browser
devtools access in this environment, do what you can) that it at least
attempts to load account data rather than throwing `window.rotator is
undefined` immediately. If you have no way to visually verify the page
content in this environment, say so explicitly in your report rather than
claiming it works — Claude will do a manual click-through pass separately
(Phase 7) regardless.

Commit, push, same process as always. If this is bigger than expected
(e.g. some page does something preload.ts doesn't cover, or argument
shapes don't line up cleanly), flag it back rather than guessing.

## Not started yet (do not start these — future slices, after this one)

- `tauri-plugin-autostart` wiring for `launchAtLogin`.
- `cli_hooks::install_hooks` startup wiring — Phase 6 (packaging), bundled
  with the `cli.js` resource-path decision below.
- Phase 6 (packaging: NSIS reinstall-dialog parity with `installer.nsh`,
  real app icons instead of Tauri's scaffold ones, the `cli.js` resource
  bundling).
- Phase 7 (manual verification pass comparing against the Electron build).

## Open decision: `cli.js` path for hook installation

`cli-hooks.ts`'s `statuslineCommand()` builds `node "<path>/cli.js"
statusline claude`, where `<path>` is the *Electron app's own* installed
`dist/main/cli.js`. Since the standalone `lazyswitch` CLI stays out of
Rust-porting scope, the Tauri app still needs *some* copy of the compiled
`dist/main/cli.js` (+ its dependency closure) sitting next to the installed
binary, referenced via Tauri's `app.path().resource_dir()` instead of
Electron's `__dirname`. Not your task right now — Phase 6 territory,
bundled with the NSIS installer work.

## Things to flag back rather than deciding unilaterally

- Anything that would require **deleting** files, branches, or the
  already-committed Electron source (`src/main/**`, `src/renderer/**`) —
  the user has explicitly reserved deletion decisions for themselves. The
  Electron app must keep building/running throughout this migration.
- Any change to the `~/.codex-accounts` / `~/.claude-accounts` directory
  layout or the `config.json` schema.
- Editing renderer HTML/CSS/inline-JS beyond the single authorized
  `<script>` tag insertion for this task.
- If `cargo test` or `npx tauri build` reveals a design assumption from an
  earlier phase was wrong, fix it, but explain what changed and why.
- Scope creep of any kind (reformatting unrelated files, "while I'm here"
  cleanups, touching already-committed phases beyond what a compile error
  strictly requires) — stop and ask.
