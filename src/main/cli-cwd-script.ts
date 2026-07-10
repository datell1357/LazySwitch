export const PEB_CWD_SCRIPT = `
$ErrorActionPreference = "SilentlyContinue"
$Name = "__PROCESS_NAME__"
$RootPid = __ROOT_PID__

$source = @"
using System;
using System.Runtime.InteropServices;
public static class Native {
  [StructLayout(LayoutKind.Sequential)]
  public struct PROCESS_BASIC_INFORMATION {
    public IntPtr Reserved1;
    public IntPtr PebBaseAddress;
    public IntPtr Reserved2_0;
    public IntPtr Reserved2_1;
    public IntPtr UniqueProcessId;
    public IntPtr InheritedFromUniqueProcessId;
  }
  [DllImport("ntdll.dll")]
  public static extern int NtQueryInformationProcess(IntPtr ProcessHandle, int ProcessInformationClass, ref PROCESS_BASIC_INFORMATION ProcessInformation, int ProcessInformationLength, out int ReturnLength);
  [DllImport("kernel32.dll", SetLastError=true)]
  public static extern IntPtr OpenProcess(int dwDesiredAccess, bool bInheritHandle, int dwProcessId);
  [DllImport("kernel32.dll", SetLastError=true)]
  public static extern bool ReadProcessMemory(IntPtr hProcess, IntPtr lpBaseAddress, byte[] lpBuffer, int dwSize, out IntPtr lpNumberOfBytesRead);
  [DllImport("kernel32.dll", SetLastError=true)]
  public static extern bool CloseHandle(IntPtr hObject);
}
"@

$NativeOk = $false
try {
  Add-Type -TypeDefinition $source -ErrorAction Stop
  $NativeOk = $true
} catch {
  $NativeOk = $false
}

function Get-Win32Process($filter) {
  if (Get-Command Get-CimInstance -ErrorAction SilentlyContinue) {
    if ([string]::IsNullOrWhiteSpace($filter)) {
      return Get-CimInstance Win32_Process -ErrorAction SilentlyContinue
    }
    return Get-CimInstance Win32_Process -Filter $filter -ErrorAction SilentlyContinue
  }
  if ([string]::IsNullOrWhiteSpace($filter)) {
    return Get-WmiObject Win32_Process -ErrorAction SilentlyContinue
  }
  return Get-WmiObject Win32_Process -Filter $filter -ErrorAction SilentlyContinue
}

function Read-Bytes($handle, $address, [int]$size) {
  $buf = New-Object byte[] $size
  $read = [IntPtr]::Zero
  if (-not [Native]::ReadProcessMemory($handle, $address, $buf, $size, [ref]$read)) {
    return $null
  }
  return $buf
}

function Read-Ptr($handle, $address) {
  $buf = Read-Bytes $handle $address ([IntPtr]::Size)
  if ($null -eq $buf) { return [IntPtr]::Zero }
  if ([IntPtr]::Size -eq 8) { return [IntPtr]([BitConverter]::ToInt64($buf, 0)) }
  return [IntPtr]([BitConverter]::ToInt32($buf, 0))
}

function Read-RemoteUnicode($handle, $address) {
  $headerSize = if ([IntPtr]::Size -eq 8) { 16 } else { 8 }
  $header = Read-Bytes $handle $address $headerSize
  if ($null -eq $header) { return $null }
  $len = [BitConverter]::ToUInt16($header, 0)
  if ($len -le 0 -or $len -gt 32766) { return $null }
  $bufferOffset = if ([IntPtr]::Size -eq 8) { 8 } else { 4 }
  $buffer = Read-Ptr $handle ([IntPtr]::Add($address, $bufferOffset))
  if ($buffer -eq [IntPtr]::Zero) { return $null }
  $bytes = Read-Bytes $handle $buffer $len
  if ($null -eq $bytes) { return $null }
  return [Text.Encoding]::Unicode.GetString($bytes).TrimEnd([char]0)
}

function Get-Cwd([int]$targetPid) {
  if (-not $NativeOk) { return $null }
  $handle = [Native]::OpenProcess(0x1010, $false, $targetPid)
  if ($handle -eq [IntPtr]::Zero) { return $null }
  try {
    $pbi = New-Object Native+PROCESS_BASIC_INFORMATION
    $ret = 0
    $size = [Runtime.InteropServices.Marshal]::SizeOf([type][Native+PROCESS_BASIC_INFORMATION])
    $status = [Native]::NtQueryInformationProcess($handle, 0, [ref]$pbi, $size, [ref]$ret)
    if ($status -ne 0 -or $pbi.PebBaseAddress -eq [IntPtr]::Zero) { return $null }
    $processParametersOffset = if ([IntPtr]::Size -eq 8) { 0x20 } else { 0x10 }
    $currentDirectoryOffset = if ([IntPtr]::Size -eq 8) { 0x38 } else { 0x24 }
    $params = Read-Ptr $handle ([IntPtr]::Add($pbi.PebBaseAddress, $processParametersOffset))
    if ($params -eq [IntPtr]::Zero) { return $null }
    return Read-RemoteUnicode $handle ([IntPtr]::Add($params, $currentDirectoryOffset))
  } catch {
    return $null
  } finally {
    [Native]::CloseHandle($handle) | Out-Null
  }
}

function Convert-StartTime($value) {
  if ($null -eq $value) { return $null }
  if ($value -is [datetime]) { return $value.ToLocalTime().ToString("o") }
  try {
    return ([Management.ManagementDateTimeConverter]::ToDateTime([string]$value)).ToLocalTime().ToString("o")
  } catch {
    return [string]$value
  }
}

$parentRows = @(Get-Win32Process $null)
$parents = @()
foreach ($row in $parentRows) {
  if ($null -ne $row.ProcessId) {
    $parentPid = 0
    if ($null -ne $row.ParentProcessId) { $parentPid = [int]$row.ParentProcessId }
    $parents += [pscustomobject]@{
      pid = [int]$row.ProcessId
      parentPid = $parentPid
      name = [string]$row.Name
      executablePath = if ($null -ne $row.ExecutablePath) { [string]$row.ExecutablePath } else { $null }
    }
  }
}

$targets = @($parentRows | Where-Object { $_.Name -ieq $Name })
$result = @()
foreach ($proc in $targets) {
  $procId = [int]$proc.ProcessId
  $parentPid = 0
  if ($null -ne $proc.ParentProcessId) { $parentPid = [int]$proc.ParentProcessId }
  $startTime = Convert-StartTime $proc.CreationDate
  $cwd = Get-Cwd $procId
  $result += [pscustomobject]@{
    pid = $procId
    parentPid = $parentPid
    name = [string]$proc.Name
    executablePath = if ($null -ne $proc.ExecutablePath) { [string]$proc.ExecutablePath } else { $null }
    startTime = $startTime
    cwd = $cwd
  }
}
ConvertTo-Json -InputObject ([pscustomobject]@{
  targets = $result
  parents = $parents
}) -Compress
`;
