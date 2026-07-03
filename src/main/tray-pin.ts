import { execFile } from "child_process";
import * as os from "os";
import { app } from "electron";

/**
 * Best-effort "always show the tray icon" (avoid the hidden overflow flyout).
 *
 * Windows 11 (build >= 22000): per-icon promotion lives at
 *   HKCU\Control Panel\NotifyIconSettings\<id>  (IsPromoted = 1)
 * keyed by a random id; each subkey carries an ExecutablePath value.
 *
 * Windows 10: no supported API. The tray visibility table lives in a binary
 * REG_BINARY blob at
 *   HKCU\...\CurrentVersion\TrayNotify  (IconStreams)
 * 20-byte header, then 1640-byte records; each record starts with a 528-byte
 * ROT13-encoded UTF-16LE executable path, followed by a visibility DWORD
 * (0 = notifications only, 1 = hidden, 2 = always show). We flip ours to 2.
 * Explorer caches the table, so a change requires restarting explorer.exe —
 * we only do that when we actually changed the value (one time).
 *
 * The record only exists after the icon has been shown at least once, so this
 * runs a few seconds after tray creation, every startup (idempotent).
 */
export function promoteTrayIcon(): void {
  if (process.platform !== "win32") return;
  const build = Number(os.release().split(".")[2] ?? 0);
  if (build >= 22000) promoteWin11();
  else promoteWin10();
}

function runPs(script: string): void {
  execFile(
    "powershell.exe",
    ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script],
    { windowsHide: true },
    () => {
      /* best effort — ignore result */
    }
  );
}

function promoteWin11(): void {
  const exe = app.getPath("exe").replace(/\\/g, "\\\\");
  runPs(`
$root = 'HKCU:\\Control Panel\\NotifyIconSettings'
if (Test-Path $root) {
  Get-ChildItem $root | ForEach-Object {
    try {
      $props = Get-ItemProperty $_.PSPath
      if ($props.ExecutablePath -and $props.ExecutablePath -ieq '${exe}') {
        Set-ItemProperty -Path $_.PSPath -Name 'IsPromoted' -Type DWord -Value 1
      }
    } catch {}
  }
}`.trim());
}

function promoteWin10(): void {
  const exe = app.getPath("exe").replace(/'/g, "''");
  runPs(`
$key = 'HKCU:\\SOFTWARE\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\TrayNotify'
$exe = '${exe}'
try { $data = (Get-ItemProperty -Path $key -Name IconStreams -ErrorAction Stop).IconStreams } catch { exit 0 }
if (-not $data -or $data.Length -lt 1660) { exit 0 }

function Rot13Char([char]$c) {
  $n = [int]$c
  if ($n -ge 97 -and $n -le 122) { return [char]((($n - 97 + 13) % 26) + 97) }
  if ($n -ge 65 -and $n -le 90)  { return [char]((($n - 65 + 13) % 26) + 65) }
  return $c
}

$header = 20; $recSize = 1640; $changed = $false
for ($off = $header; ($off + $recSize) -le $data.Length; $off += $recSize) {
  $sb = New-Object System.Text.StringBuilder
  for ($j = 0; $j -lt 528; $j += 2) {
    $u = [BitConverter]::ToUInt16($data, $off + $j)
    if ($u -eq 0) { break }
    [void]$sb.Append((Rot13Char([char]$u)))
  }
  $path = $sb.ToString()
  if ($path -ieq $exe) {
    $visOff = $off + 528
    if ($data[$visOff] -ne 2) {
      $data[$visOff] = 2
      $changed = $true
    }
  }
}

if ($changed) {
  Set-ItemProperty -Path $key -Name IconStreams -Value $data
  Stop-Process -Name explorer -Force
  Start-Sleep -Seconds 2
  if (-not (Get-Process explorer -ErrorAction SilentlyContinue)) { Start-Process explorer.exe }
}`.trim());
}
