<div align="center">

# LazySwitch

Stop doing the logout → login dance. When one account is spent, the next one takes over.

Windows-first tray app that keeps several **Codex** and **Claude** accounts enrolled locally, watches each usage window, and rotates to a spare account the moment the active one runs out — then brings your running CLI sessions back where they left off.

<p>
  <a href="#requirements"><img src="https://img.shields.io/badge/OS-Windows-0078D4" alt="OS Windows"></a>
  <a href="#start-here"><img src="https://img.shields.io/badge/Providers-Codex%20%7C%20Claude-111111" alt="Providers Codex and Claude"></a>
  <a href="#license-and-attribution"><img src="https://img.shields.io/badge/License-MIT-blue" alt="License MIT"></a>
  <a href="https://github.com/datell1357/LazySwitch/stargazers"><img src="https://img.shields.io/github/stars/datell1357/LazySwitch?label=stars&labelColor=555555&color=F0B72F" alt="GitHub stars"></a>
</p>

<p>
  <a href="#cli-session-handover"><img src="https://img.shields.io/badge/CLI-auto%20resume-5B8DEF" alt="CLI auto resume"></a>
  <a href="#commands"><img src="https://img.shields.io/badge/command-lazyswitch%20status%20%7C%20watch-0A7F64" alt="LazySwitch commands"></a>
</p>

<sub>
  <a href="#설치하기">설치하기</a> ·
  <a href="#start-here">Start here</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#cli-session-handover">CLI 세션 인계</a> ·
  <a href="#config">Config</a> ·
  <a href="#commands">명령어</a> ·
  <a href="#boundaries">Boundaries</a> ·
  <a href="#license-and-attribution">라이선스</a>
</sub>

</div>

---

## 설치하기

Run this in PowerShell:

```powershell
$repo = "https://github.com/datell1357/LazySwitch.git"
$dir = "$HOME\LazySwitch"

if (Test-Path "$dir\.git") {
  git -C $dir pull --ff-only
} else {
  git clone $repo $dir
}

npm --prefix $dir install
npm --prefix $dir start
```

`npm start` builds and launches the tray app. LazySwitch lives in the tray — there is no main window.

### Build An Installer

```powershell
npm run dist
```

Produces an NSIS installer under `release/`.

---

## Start Here

A switch touches three surfaces, and each one is opt-in:

| Surface | What LazySwitch does | What you do |
|---|---|---|
| **Auth store** | Swaps the live auth file for the next account's | Nothing |
| **Codex Desktop** | Restarts it, because it caches the old token in memory | Tick *Auto-restart Desktop* |
| **Running CLI sessions** | Kills each one, closes its terminal, reopens it in a fresh terminal on its own `resume` command | Tick *Auto-restart CLI* |

Enroll two or more accounts per provider, tick what you want, leave it in the tray.

---

## How It Works

```
~/.codex-accounts/<name>/auth.json    each enrolled Codex account, isolated
~/.codex/auth.json                    the LIVE Codex account (CLI + Desktop both read this)

~/.claude-accounts/<name>/            each enrolled Claude account, isolated
~/.claude/                            the LIVE Claude account
```

- **Enroll** — log into an account (`codex login` / `claude`), then *Enroll current login as…* from the tray. Repeat per account.
- **Monitor** — reads each provider's own telemetry: Codex's `rate_limits` from the newest session rollout, Claude's usage API. A usage-limit **error** in the session stream is always a reactive backstop.
- **Switch** — when the 5-hour or weekly window crosses its threshold, LazySwitch saves the live auth back into its slot (keeping any refreshed `refresh_token`), atomically installs the next account's auth, and restarts Codex Desktop if that is enabled.
- **Approve** — a popup asks first by default. Tick **Auto-approve switches** for unattended rotation.

### Spare Pool

Each account card shows two independent facts, side by side:

| Badge | Meaning |
|---|---|
| **사용중** | the account currently in use |
| **활성화됨** | in the spare pool — rotation may switch to it |
| **비활성화됨** | still enrolled, but never rotated into |

An account can be in use *and* disabled as a spare; the card says so plainly. The button beside the badges is the action (**끄기** / **켜기**), not the state.

---

## CLI Session Handover

A switch invalidates the token every running `codex` / `claude` CLI is holding. With **Auto-restart CLI** on, LazySwitch finds those sessions and hands each one over:

1. Kill the CLI process.
2. Close the shell it ran in, when that shell can be closed.
3. Reopen it in a **new terminal** on its own resume command — `codex resume <id>` / `claude --resume <id>`, resolved per session so every terminal comes back to its own conversation.

Sessions living in an **Orca** tab reopen as a new Orca tab and never spill a desktop console onto your machine. Everything else gets a Windows Terminal tab, or a PowerShell window when that cannot launch.

Detection is machine-wide: a `codex` in project A and a `codex` in project B are both handed over, each to its own directory and its own transcript.

### What It Will Not Do

| Case | Behaviour |
|---|---|
| **In-flight work** | Lost. Resume restores the conversation transcript, not the turn that was running. |
| **Elevated terminals** | Left alone. An unelevated app cannot read an elevated process's working directory, cannot identify its transcript, and cannot kill it. Those sessions are reported as *manual* and the resume command is copied to your clipboard, to paste into that admin terminal yourself. |
| **Terminal emulators** | Never closed. Only real shells (`powershell`, `pwsh`, `cmd`, `bash`) are — a `wt.exe` may own tabs that have nothing to do with you. |

---

## Requirements

**Required**

- Windows 10/11
- Node.js 18+ and npm
- At least two accounts enrolled for whichever provider you want rotated

**Optional**

- Codex Desktop — only if you want it restarted on a switch
- Windows Terminal — used for reopened sessions when present; a PowerShell window is the fallback
- Orca — Orca-hosted sessions reopen as Orca tabs

**Not needed**

- Any API key. LazySwitch moves the auth files the CLIs already wrote; it never asks you for credentials.

---

## Config

`%APPDATA%/LazySwitch/config.json` (macOS: `~/Library/Application Support/LazySwitch/`). Most settings are per provider.

| key | default | meaning |
|---|---|---|
| `autoApprove` | `false` | rotate without the approval popup |
| `autoRestartCli` | `false` | hand running CLI sessions over on a switch |
| `desktopAppPath` | `""` | Codex Desktop exe or `shell:AppsFolder\…` AUMID (auto-detected if empty) |
| `primaryMinLeftPct` | `5` | switch when the primary (5h) window has ≤ this % left |
| `weeklyMinLeftPct` | `1` | switch when the weekly window has ≤ this % left |
| `pollIntervalSec` | `30` Codex · `300` Claude | threshold check cadence — Claude's usage API rate-limits below ~5 min |
| `rateLimitEndpoint` | `""` | optional authoritative rate-limit URL |
| `language` | `""` | UI language: `""` system, `ko` / `en` / `ja` / `zh` |

---

## 명령어

The tray app is the whole product, but the same numbers are available from a terminal:

```bash
lazyswitch status                    # print the account usage table once
lazyswitch watch --interval 30       # keep the table on screen
lazyswitch statusline                # one compact line
lazyswitch statusline claude         # …for a single provider
lazyswitch install-hooks             # Claude statusLine + Codex built-ins
lazyswitch install-codex-wrapper     # wrap codex with a LazySwitch usage pane
```

From a checkout, before installing:

```bash
npm run build
node dist/main/cli.js status
```

---

## Boundaries

LazySwitch does **not**:

- bypass, extend, or defeat any usage limit — it rotates between accounts you already own
- ask for, store, or transmit credentials; it moves auth files the CLIs wrote, on your machine only
- phone home. No server, no telemetry, no network call except each provider's own usage endpoint

Running several accounts to extend usage may conflict with the terms you agreed to. That call is yours.

### Status

- Proactive percentages depend on Codex emitting `rate_limits` locally, or a pinned `rateLimitEndpoint`. The reactive error backstop works either way.
- Windows first. macOS paths (`desktop.ts`, tray) are stubbed and untested.

---

## License And Attribution

MIT. See [LICENSE](LICENSE).

Codex and Claude are products of OpenAI and Anthropic respectively. LazySwitch is an independent tool, not affiliated with, endorsed by, or supported by either.
