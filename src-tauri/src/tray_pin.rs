use std::path::Path;

use crate::powershell;

const WIN11_SCRIPT: &str = r#"
$root = 'HKCU:\Control Panel\NotifyIconSettings'
if (Test-Path $root) {
  Get-ChildItem $root | ForEach-Object {
    try {
      $props = Get-ItemProperty $_.PSPath
      if ($props.ExecutablePath -and $props.ExecutablePath -ieq '__EXE__') {
        Set-ItemProperty -Path $_.PSPath -Name 'IsPromoted' -Type DWord -Value 1
      }
    } catch {}
  }
}"#;

const WIN10_SCRIPT: &str = r#"
$key = 'HKCU:\SOFTWARE\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\TrayNotify'
$exe = '__EXE__'
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
}"#;

fn windows_build(release: &str) -> u32 {
    release
        .split('.')
        .nth(2)
        .and_then(|part| part.parse().ok())
        .unwrap_or_default()
}

fn promotion_script(release: &str, executable: &str) -> String {
    if windows_build(release) >= 22_000 {
        let escaped = executable.replace('\\', r"\\").replace('\'', "''");
        WIN11_SCRIPT.replace("__EXE__", &escaped)
    } else {
        let escaped = executable.replace('\'', "''");
        WIN10_SCRIPT.replace("__EXE__", &escaped)
    }
}

/// Best-effort promotion of the app's tray icon out of the overflow flyout.
pub async fn promote_tray_icon(powershell_exe: &Path, os_release: &str, executable: &Path) {
    if !cfg!(windows) {
        return;
    }
    let script = promotion_script(os_release, &executable.to_string_lossy());
    let _ = powershell::run(&powershell_exe.to_string_lossy(), &script, 10_000).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_windows_11_promotion_when_build_is_22000_or_newer() {
        let script = promotion_script("10.0.22631", r"C:\Program Files\LazySwitch\LazySwitch.exe");
        assert!(script.contains("NotifyIconSettings"));
        assert!(script.contains(r"C:\\Program Files\\LazySwitch\\LazySwitch.exe"));
        assert!(!script.contains("IconStreams"));
    }

    #[test]
    fn escapes_executable_path_for_windows_10_script() {
        let script = promotion_script("10.0.19045", r"C:\Apps\Owner's\LazySwitch.exe");
        assert!(script.contains(r"$exe = 'C:\Apps\Owner''s\LazySwitch.exe'"));
        assert!(script.contains("IconStreams"));
    }
}
