# Codex Account Rotator

Tray app that keeps several Codex (ChatGPT-login) accounts enrolled locally and
automatically rotates to the next one when the active account's usage window is
spent — so heavy users stop doing the logout → login dance by hand.

Both the **Codex CLI** and the **Codex Desktop** app read the same live auth
store (`~/.codex/auth.json`), so swapping that one file switches both. Desktop
keeps the old token cached in memory, so switching it requires a full app
restart (optional toggle).

## How it works

```
~/.codex-accounts/
  <name>/auth.json     ← each enrolled account, isolated
~/.codex/auth.json     ← the LIVE account (CLI + Desktop both read this)
```

- **Enroll**: log into an account with `codex login`, then "Enroll current
  login as…" from the tray. Repeat per account.
- **Monitor** (proactive): reads Codex's own `rate_limits` telemetry from the
  newest session rollout file; if your build doesn't emit it, set an authoritative
  `rateLimitEndpoint` in the config to poll the backend with the account's
  `access_token`. A usage-limit **error** in the session stream is always used as
  a reactive backstop.
- **Switch**: when the 5-hour or weekly window crosses the threshold, the app
  saves the live auth back into its slot (to keep any refreshed `refresh_token`),
  atomically installs the next account's `auth.json`, and — if enabled — restarts
  the Desktop app.
- **Approval**: default shows a popup before switching. Tick **Auto-approve
  switches** in the tray to consent to unattended rotation.

## Config

`%APPDATA%/codex-account-rotator/config.json` (macOS: `~/Library/Application Support/...`).

| key | default | meaning |
|---|---|---|
| `autoApprove` | `false` | restart Desktop without the approval popup |
| `desktopAppPath` | `""` | Codex Desktop exe or `shell:AppsFolder\…` AUMID (auto-detected if empty) |
| `primaryMinLeftPct` | `10` | switch when the primary (5h) window has ≤ this % left |
| `weeklyMinLeftPct` | `5` | switch when the weekly window has ≤ this % left |
| `pollIntervalSec` | `30` | threshold check cadence |
| `language` | `""` | UI language: `""` system, `ko`/`en`/`ja`/`zh` |
| `rateLimitEndpoint` | `""` | optional authoritative rate-limit URL |

## Run

```bash
npm install
npm start          # build + launch the tray app
```

## Status / caveats

- Proactive % depends on Codex emitting `rate_limits` locally **or** a pinned
  `rateLimitEndpoint`. The reactive error backstop works regardless.
- Windows first; macOS paths are stubbed (`desktop.ts`, tray) and need testing.
- Running multiple accounts to extend usage may conflict with OpenAI's terms —
  your call.
