# In-Place Resume Feasibility (no new window)

Verified on this machine (Windows 10 IoT LTSC 2021, 2026-07-11).

## Goal

Today `restartCliSessions` (src/main/cli-sessions.ts) kills the CLI and opens a
**new** wt.exe / PowerShell window to run the resume command. Instead, reuse the
console the CLI was already running in: after the account switch kills
`claude.exe` / `codex.exe`, its parent shell survives at a prompt. Send the
resume command into that same shell.

## Mechanism

Win32 console input injection from a short-lived helper process:

1. `FreeConsole()`
2. `AttachConsole(shellPid)`
3. `CreateFileW("CONIN$", GENERIC_READ|GENERIC_WRITE, ...)`
4. `WriteConsoleInputW(...)` with KEY_EVENT records for each char + CR

`shellPid` is the CLI's nearest terminal ancestor — the detector already walks
this chain (`hasTerminalAncestor`, `TERMINAL_PROCESS_NAMES`); it just needs to
return the pid instead of a bool.

## Test results

| Host | Shell parent | Elevated | Result |
| --- | --- | --- | --- |
| Classic conhost | `powershell.exe` standalone | no | injected, executed |
| Windows Terminal (ConPTY) | `WindowsTerminal.exe` | no | injected, executed |
| Orca (ConPTY) | `orca-terminal-daemon.exe` | no | injected, executed; visible in the Orca pane |
| Classic conhost | `cmd.exe` | no | injected, executed |
| Classic conhost | `powershell.exe` | **yes** | `AttachConsole` refused (ACCESS_DENIED); new-window fallback |
| Classic conhost | `cmd.exe` | **yes** | `AttachConsole` refused (ACCESS_DENIED); new-window fallback |

Full chain verified end to end: the shell spawns a long-lived child, the child is
killed with the CLI's own pid, **the shell survives at its prompt**, and the
injected resume command then runs in that shell.

Verified against a live Orca-hosted codex pane as well: the resumed `codex.exe`
came back under the original pane's shell (`powershell.exe` ->
`orca-terminal-daemon.exe`), with `resumedInPlace: 1` and no new window.

For both elevated consoles the real `restartCliSessions` returned
`{restarted: 1, resumedInPlace: 0}` and left the elevated shell untouched, which
is the intended degrade.

An `ok: true` from the helper means the keystrokes reached the console's input
buffer, not that they ran: a shell busy inside a script never reads them. That is
fine here because injection only happens once the CLI is dead and the shell is
back at its prompt.

## Constraints

- **Order matters.** Inject only after the CLI process is confirmed dead.
  While the TUI is alive, keystrokes go to the TUI, not the shell.
- **Elevated consoles.** An unelevated helper cannot `AttachConsole` to an
  elevated console. Keep the existing manual/copy fallback for those.
- **Run in a helper process, not Electron main.** `AttachConsole` mutates
  process-global console state; spawn `powershell.exe -EncodedCommand` (or a
  small native helper) so the app's own process is never touched.
- **`FlushConsoleInputBuffer`** before writing, to drop stray keystrokes.
- Orca additionally exposes a supported API (`orca terminal send --terminal
  <handle>`), but `orca terminal list --json` does not expose the pane's shell
  pid. It is safe only when exactly one pane and exactly one eligible session
  share the normalized worktree path; never infer a handle from list order when
  multiple panes or sessions share a worktree.

## Fallback ladder

1. Any console-hosted session -> console input injection into the pid-exact
   parent shell.
2. Injection fails, the session is Orca-hosted, and exactly one Orca pane plus
   one eligible session have the normalized worktree path -> `orca terminal
   send`.
3. Otherwise -> current behavior: new wt/PowerShell window, or the manual copy
   path.
