//! Windows process metadata used by the CLI detector.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRow {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: Option<String>,
    pub executable_path: Option<String>,
    pub start_time: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub targets: Vec<ProcessRow>,
    pub parents: Vec<ProcessRow>,
}

pub fn trim_cwd(value: &str) -> String {
    let mut value = value
        .strip_prefix("\\\\?\\UNC\\")
        .map(|rest| format!("\\\\{rest}"))
        .unwrap_or_else(|| value.strip_prefix("\\\\?\\").unwrap_or(value).to_owned());
    while value.ends_with(['\\', '/']) {
        value.pop();
    }
    if value.len() == 2 && value.as_bytes()[1] == b':' {
        value.push('\\');
    }
    value
}

pub fn normalize_windows_path(value: &str) -> String {
    let value = trim_cwd(value).replace('/', "\\").to_ascii_lowercase();
    let unc = value.starts_with("\\\\");
    let prefix = if unc { "\\\\" } else { "" };
    let mut rest = value.trim_start_matches('\\').to_owned();
    while rest.contains("\\\\") { rest = rest.replace("\\\\", "\\"); }
    format!("{prefix}{rest}")
}

pub fn existing_cwd(value: &str) -> Option<String> {
    let cwd = trim_cwd(value);
    let path = PathBuf::from(&cwd);
    path.is_dir().then_some(cwd)
}

#[cfg(windows)]
mod windows_impl {
    use super::{trim_cwd, ProcessRow, ProcessSnapshot};
    use chrono::Local;
    use std::mem::size_of;
    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };
    use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows::core::PWSTR;

    fn process_name(entry: &PROCESSENTRY32W) -> String {
        let len = entry
            .szExeFile
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(entry.szExeFile.len());
        String::from_utf16_lossy(&entry.szExeFile[..len])
    }

    fn image_path(pid: u32) -> Option<String> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        let result = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
        };
        unsafe { let _ = CloseHandle(handle); }
        result.ok().map(|_| String::from_utf16_lossy(&buffer[..length as usize]))
    }

    fn filetime_millis(value: FILETIME) -> i64 {
        let ticks = (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime);
        // FILETIME is 100ns intervals since 1601-01-01; Unix epoch is 11644473600s later.
        ((ticks / 10_000) as i64) - 11_644_473_600_000
    }

    fn start_time(pid: u32) -> Option<String> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let ok = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user).is_ok() };
        unsafe { let _ = CloseHandle(handle); }
        if !ok { return None; }
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(filetime_millis(creation))
            .map(|time| time.with_timezone(&Local).to_rfc3339())
    }

    fn read_bytes(handle: HANDLE, address: usize, size: usize) -> Option<Vec<u8>> {
        let mut bytes = vec![0u8; size];
        let mut read = 0usize;
        let ok = unsafe {
            ReadProcessMemory(
                handle,
                address as *const _,
                bytes.as_mut_ptr() as *mut _,
                size,
                Some(&mut read),
            ).is_ok()
        };
        ok.then_some(bytes).filter(|_| read == size)
    }

    fn read_ptr(handle: HANDLE, address: usize) -> Option<usize> {
        let bytes = read_bytes(handle, address, size_of::<usize>())?;
        Some(if size_of::<usize>() == 8 {
            u64::from_ne_bytes(bytes.try_into().ok()?) as usize
        } else {
            u32::from_ne_bytes(bytes.try_into().ok()?) as usize
        })
    }

    #[repr(C)]
    struct ProcessBasicInformation {
        reserved1: *mut std::ffi::c_void,
        peb_base_address: *mut std::ffi::c_void,
        reserved2: [*mut std::ffi::c_void; 2],
        unique_process_id: *mut std::ffi::c_void,
        inherited_from_unique_process_id: *mut std::ffi::c_void,
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(
            process_handle: HANDLE,
            process_information_class: u32,
            process_information: *mut ProcessBasicInformation,
            process_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    pub fn process_cwd(pid: u32) -> Option<String> {
        let handle = unsafe {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, false, pid)
        }.ok()?;
        let mut info = ProcessBasicInformation {
            reserved1: std::ptr::null_mut(),
            peb_base_address: std::ptr::null_mut(),
            reserved2: [std::ptr::null_mut(); 2],
            unique_process_id: std::ptr::null_mut(),
            inherited_from_unique_process_id: std::ptr::null_mut(),
        };
        let mut returned = 0u32;
        let status = unsafe {
            NtQueryInformationProcess(
                handle,
                0,
                &mut info,
                size_of::<ProcessBasicInformation>() as u32,
                &mut returned,
            )
        };
        let result = if status == 0 && !info.peb_base_address.is_null() {
            let pointer_offset = if size_of::<usize>() == 8 { 0x20 } else { 0x10 };
            let cwd_offset = if size_of::<usize>() == 8 { 0x38 } else { 0x24 };
            let params = read_ptr(handle, info.peb_base_address as usize + pointer_offset);
            params.and_then(|params| read_unicode(handle, params + cwd_offset))
        } else { None };
        unsafe { let _ = CloseHandle(handle); }
        result.map(|value| trim_cwd(&value))
    }

    fn read_unicode(handle: HANDLE, address: usize) -> Option<String> {
        let header_size = if size_of::<usize>() == 8 { 16 } else { 8 };
        let header = read_bytes(handle, address, header_size)?;
        let length = u16::from_ne_bytes([header[0], header[1]]) as usize;
        if length == 0 || length > 32_766 { return None; }
        let pointer_offset = if size_of::<usize>() == 8 { 8 } else { 4 };
        let pointer = read_ptr(handle, address + pointer_offset)?;
        let bytes = read_bytes(handle, pointer, length)?;
        Some(String::from_utf16_lossy(
            &bytes.chunks_exact(2).map(|pair| u16::from_ne_bytes([pair[0], pair[1]])).collect::<Vec<_>>(),
        ))
    }

    pub fn snapshot(target_process_name: &str) -> Result<ProcessSnapshot, String> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|error| error.to_string())?;
        let mut entries = Vec::new();
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if unsafe { Process32FirstW(snapshot, &mut entry).is_ok() } {
            loop {
                entries.push((entry.th32ProcessID, entry.th32ParentProcessID, process_name(&entry)));
                if unsafe { Process32NextW(snapshot, &mut entry).is_err() } { break; }
            }
        }
        unsafe { let _ = CloseHandle(snapshot); }
        let parents = entries.iter().map(|(pid, ppid, name)| ProcessRow {
            pid: *pid, parent_pid: *ppid, name: Some(name.clone()), executable_path: image_path(*pid), start_time: None, cwd: None,
        }).collect::<Vec<_>>();
        let targets = entries.into_iter().filter(|(_, _, name)| name.eq_ignore_ascii_case(target_process_name)).map(|(pid, ppid, name)| ProcessRow {
            pid, parent_pid: ppid, name: Some(name), executable_path: image_path(pid), start_time: start_time(pid), cwd: process_cwd(pid),
        }).collect();
        Ok(ProcessSnapshot { targets, parents })
    }
}

#[cfg(windows)]
pub use windows_impl::{process_cwd, snapshot};

#[cfg(not(windows))]
pub fn process_cwd(_pid: u32) -> Option<String> { None }

#[cfg(not(windows))]
pub fn snapshot(_process_name: &str) -> Result<ProcessSnapshot, String> { Ok(ProcessSnapshot::default()) }
