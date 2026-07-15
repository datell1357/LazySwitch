//! Operating-system integration that is deliberately kept outside the core
//! account and usage code.  The Windows implementation uses Win32 directly;
//! it does not depend on a shell or PowerShell.

use crate::core::types::ProviderPrefs;

#[cfg(windows)]
mod windows_impl {
    use super::ProviderPrefs;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use std::time::Duration;

    use windows::core::{w, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, POINT};
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, GetMonitorInfoW, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_SZ,
    };
    use windows::Win32::System::Threading::{
        CreateMutexW, GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, FindWindowW, GetWindow, GetWindowLongPtrW,
        GetWindowRect, GetTopWindow, SetForegroundWindow, SetWindowPos, ShowWindow,
        SetWindowLongPtrW, GWL_EXSTYLE, GW_HWNDNEXT,
        HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_RESTORE, WS_EX_LAYERED,
        WS_EX_TRANSPARENT,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::DataExchange::{EmptyClipboard, OpenClipboard, SetClipboardData, CloseClipboard};
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    const CF_UNICODETEXT: u32 = 13;

    const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    const APP_NAME: PCWSTR = w!("LazySwitch");
    const MUTEX_NAME: PCWSTR = w!("Local\\LazySwitch.SingleInstance");
    const TERMINAL_NAMES: [&str; 11] = [
        "alacritty.exe", "bash.exe", "cmd.exe", "conhost.exe", "mintty.exe",
        "powershell.exe", "pwsh.exe", "terminal.exe", "wezterm-gui.exe",
        "windowsterminal.exe", "wt.exe",
    ];

    static INSTANCE: OnceLock<usize> = OnceLock::new();

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Rect { pub left: i32, pub top: i32, pub right: i32, pub bottom: i32 }
    impl Rect {
        pub fn width(self) -> i32 { self.right - self.left }
        pub fn height(self) -> i32 { self.bottom - self.top }
    }

    fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(std::iter::once(0)).collect() }

    pub fn acquire_single_instance() -> bool {
        // Probe runs are isolated from the installed app's mutex so verification
        // never needs to terminate the user's running app.
        let probe_name = std::env::var_os("LAZYSWITCH_TAURI_PROBE")
            .map(|_| wide(&format!("Local\\LazySwitch.SingleInstance.Probe.{}", unsafe { GetCurrentProcessId() })));
        let mutex_name = probe_name.as_ref().map_or(MUTEX_NAME, |name| PCWSTR(name.as_ptr()));
        let mutex = unsafe { CreateMutexW(None, false, mutex_name) };
        let Ok(mutex) = mutex else { return true };
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { let _ = CloseHandle(mutex); };
            focus_manager();
            return false;
        }
        let _ = INSTANCE.set(mutex.0 as usize);
        true
    }

    pub fn focus_manager() {
        let Ok(hwnd) = (unsafe { FindWindowW(None, w!("Accounts")) }) else { return };
        if !hwnd.0.is_null() {
            unsafe { let _ = ShowWindow(hwnd, SW_RESTORE); let _ = SetForegroundWindow(hwnd); }
        }
    }

    pub fn open_url(url: &str) -> Result<(), String> {
        let url = wide(url);
        let result = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(url.as_ptr()), PCWSTR::null(), PCWSTR::null(), windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL) };
        if (result.0 as usize) <= 32 { Err(format!("ShellExecuteW failed: {}", result.0 as isize)) } else { Ok(()) }
    }

    pub fn set_topmost(hwnd: isize, enabled: bool) {
        let hwnd = HWND(hwnd as *mut _);
        if hwnd.0.is_null() { return; }
        unsafe {
            let insert_after = if enabled { HWND_TOPMOST } else { HWND_NOTOPMOST };
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | if enabled { SWP_SHOWWINDOW } else { Default::default() };
            let result = if enabled {
                let bounds = rect(hwnd.0 as isize).unwrap_or(Rect { left: 0, top: 0, right: 0, bottom: 0 });
                SetWindowPos(hwnd, Some(insert_after), bounds.left, bounds.top, bounds.width(), bounds.height(), SWP_NOACTIVATE | SWP_SHOWWINDOW)
            } else {
                SetWindowPos(hwnd, Some(insert_after), 0, 0, 0, 0, flags)
            };
            if let Err(error) = result {
                if std::env::var_os("LAZYSWITCH_TAURI_PROBE").is_some() { eprintln!("[probe:topmost] SetWindowPos({enabled}) failed: {error}"); }
            }
        }
    }

    pub fn is_layered(hwnd: isize) -> bool {
        hwnd != 0 && unsafe { (GetWindowLongPtrW(HWND(hwnd as *mut _), GWL_EXSTYLE) as u32 & WS_EX_LAYERED.0) != 0 }
    }

    pub fn is_click_through(hwnd: isize) -> bool {
        hwnd != 0 && unsafe { (GetWindowLongPtrW(HWND(hwnd as *mut _), GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT.0) != 0 }
    }

    pub fn set_click_through(hwnd: isize, enabled: bool) {
        if hwnd == 0 { return; }
        let window = HWND(hwnd as *mut _);
        unsafe {
            let current = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
            let next = if enabled { current | WS_EX_TRANSPARENT.0 } else { current & !WS_EX_TRANSPARENT.0 };
            let _ = SetWindowLongPtrW(window, GWL_EXSTYLE, next as isize);
        }
    }

    pub fn rect(hwnd: isize) -> Option<Rect> {
        let mut r = windows::Win32::Foundation::RECT::default();
        if hwnd == 0 || unsafe { GetWindowRect(HWND(hwnd as *mut _), &mut r).is_err() } { return None; }
        Some(Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom })
    }

    pub fn taskbar_rects() -> Option<(Rect, Rect)> {
        let taskbar = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok()? };
        let notify = unsafe { FindWindowExW(Some(taskbar), None, w!("TrayNotifyWnd"), PCWSTR::null()).ok()? };
        Some((rect(taskbar.0 as isize)?, rect(notify.0 as isize)?))
    }

    pub fn work_area_at(x: i32, y: i32) -> Option<Rect> {
        let monitor = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
        if monitor.0.is_null() { return None; }
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !unsafe { GetMonitorInfoW(monitor, &mut info).as_bool() } { return None; }
        Some(Rect { left: info.rcWork.left, top: info.rcWork.top, right: info.rcWork.right, bottom: info.rcWork.bottom })
    }

    pub fn compact_bounds(_widget: isize, width: i32, height: i32, position: &str) -> Option<Rect> {
        let (taskbar, notify) = taskbar_rects()?;
        let display = work_area_at(notify.left, notify.top)?;
        if position == "taskbar" {
            let strip_top = display.bottom;
            let strip_height = taskbar.bottom - strip_top;
            if strip_height <= 0 { return None; }
            let mut left = notify.left - 4 - width;
            left = left.clamp(display.left, display.right - width);
            let fitted_height = height.min(strip_height);
            let top = strip_top + (strip_height - fitted_height) / 2;
            return Some(Rect { left, top, right: left + width, bottom: top + fitted_height });
        }
        let left = if position == "bottom-left" { display.left } else { display.right - width };
        Some(Rect { left, top: display.bottom - height, right: left + width, bottom: display.bottom })
    }

    pub fn set_bounds(hwnd: isize, bounds: Rect, topmost: bool) {
        let insert_after = if topmost { HWND_TOPMOST } else { HWND_TOP };
        unsafe { let _ = SetWindowPos(HWND(hwnd as *mut _), Some(insert_after), bounds.left, bounds.top, bounds.width(), bounds.height(), SWP_NOACTIVATE); }
    }

    pub fn tray_menu_position(icon: Rect, widget: Option<Rect>) -> Option<(i32, i32)> {
        const WIDTH: i32 = 352; const HEIGHT: i32 = 317; const GAP: i32 = 8;
        let work = work_area_at(icon.left, icon.top)?;
        let clamp_x = |x: i32| x.clamp(work.left, work.right - WIDTH);
        let mut x = clamp_x(icon.left + (icon.width() - WIDTH) / 2);
        let y = (icon.top - HEIGHT).clamp(work.top, work.bottom - HEIGHT);
        if let Some(widget) = widget {
            let overlaps = x < widget.right && x + WIDTH > widget.left && y < widget.bottom && y + HEIGHT > widget.top;
            if overlaps { x = clamp_x(widget.left - GAP - WIDTH); }
        }
        Some((x, y))
    }

    fn window_index(target: HWND) -> Option<usize> {
        let mut index = 0usize;
        let mut current = unsafe { GetTopWindow(None).unwrap_or(HWND(std::ptr::null_mut())) };
        while !current.0.is_null() {
            if current == target { return Some(index); }
            current = unsafe { GetWindow(current, GW_HWNDNEXT).unwrap_or(HWND(std::ptr::null_mut())) };
            index += 1;
        }
        None
    }

    /// Collect the Win32 evidence used by the integration probe. The taskbar
    /// is raised once to model a taskbar click, then the caller waits for the
    /// app's 100 ms re-assert timer.
    pub fn widget_probe(widget: isize, simulate_taskbar_raise: bool) -> serde_json::Value {
        let taskbar = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok() };
        let notify = taskbar.and_then(|hwnd| unsafe { FindWindowExW(Some(hwnd), None, w!("TrayNotifyWnd"), PCWSTR::null()).ok() });
        let widget_hwnd = HWND(widget as *mut _);
        let before_widget = window_index(widget_hwnd);
        let before_taskbar = taskbar.and_then(window_index);
        if simulate_taskbar_raise {
            if let Some(hwnd) = taskbar {
                unsafe { let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE); }
                std::thread::sleep(Duration::from_millis(320));
            }
        }
        let after_widget = window_index(widget_hwnd);
        let after_taskbar = taskbar.and_then(window_index);
        let widget_rect = rect(widget);
        let taskbar_rect = taskbar.and_then(|hwnd| rect(hwnd.0 as isize));
        let notify_rect = notify.and_then(|hwnd| rect(hwnd.0 as isize));
        let strip_inside = widget_rect.zip(taskbar_rect).is_some_and(|(widget, taskbar)| widget.top >= taskbar.top && widget.bottom <= taskbar.bottom);
        let left_of_notify = widget_rect.zip(notify_rect).is_some_and(|(widget, notify)| widget.right <= notify.left);
        let rect_value = |value: Option<Rect>| value.map(|r| serde_json::json!({ "left": r.left, "top": r.top, "right": r.right, "bottom": r.bottom }));
        serde_json::json!({
            "before": { "widgetIndex": before_widget, "taskbarIndex": before_taskbar },
            "after": { "widgetIndex": after_widget, "taskbarIndex": after_taskbar },
            "widgetRect": rect_value(widget_rect),
            "taskbarRect": rect_value(taskbar_rect),
            "trayNotifyRect": rect_value(notify_rect),
            "layered": is_layered(widget),
            "clickThrough": is_click_through(widget),
            "insideTaskbarStrip": strip_inside,
            "rightEdgeLeftOfTrayNotify": left_of_notify,
        })
    }

    pub fn apply_launch_at_login(enabled: bool, exe: &Path) -> Result<(), String> {
        let mut handle = HKEY::default();
        let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, Some(0), KEY_SET_VALUE, &mut handle) };
        if status.0 != 0 { return Err(format!("RegOpenKeyExW failed: {}", status.0)); }
        let result = if enabled {
            let value = wide(&format!("\"{}\" --hidden", exe.display()));
            unsafe { RegSetValueExW(handle, APP_NAME, Some(0), REG_SZ, Some(std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2))) }
        } else { unsafe { RegDeleteValueW(handle, APP_NAME) } };
        unsafe { let _ = RegCloseKey(handle); }
        if result.0 == 0 { Ok(()) } else { Err(format!("registry operation failed: {}", result.0)) }
    }

    fn reg_dword(path: PCWSTR, name: PCWSTR) -> Option<u32> {
        let mut key = HKEY::default();
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path, Some(0), KEY_READ, &mut key).ok().ok()?; }
        let mut kind = REG_DWORD; let mut value = 0u32; let mut bytes = 4u32;
        let result = unsafe { RegQueryValueExW(key, name, None, Some(&mut kind), Some((&mut value as *mut u32).cast::<u8>()), Some(&mut bytes)) };
        unsafe { let _ = RegCloseKey(key); }
        result.ok().ok().map(|_| value)
    }

    pub fn system_locale() -> Option<String> {
        // Windows has no POSIX `LANG` env var; the UI language has to come
        // from the OS locale API instead.
        let mut buffer = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
        let len = unsafe { GetUserDefaultLocaleName(&mut buffer) };
        if len == 0 {
            return None;
        }
        let end = buffer[..len as usize]
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(len as usize - 1);
        Some(String::from_utf16_lossy(&buffer[..end]))
    }

    pub fn taskbar_theme() -> Option<bool> {
        let system_light = reg_dword(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), w!("SystemUsesLightTheme"));
        let prevalence = reg_dword(w!("Software\\Microsoft\\Windows\\DWM"), w!("ColorPrevalence"));
        let accent = reg_dword(w!("Software\\Microsoft\\Windows\\DWM"), w!("AccentColor"));
        match system_light {
            Some(value) => Some(value != 0),
            None => match (prevalence, accent) {
                (Some(1), Some(color)) => Some((((color & 0xff) + ((color >> 8) & 0xff) + ((color >> 16) & 0xff)) / 3) > 128),
                _ => None,
            },
        }
    }

    fn process_path(pid: u32) -> Option<PathBuf> {
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut buffer = vec![0u16; 32768]; let mut len = buffer.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_FORMAT(0), PWSTR(buffer.as_mut_ptr()), &mut len).is_ok() };
        unsafe { let _ = CloseHandle(process); };
        if ok { Some(PathBuf::from(OsString::from_wide(&buffer[..len as usize]))) } else { None }
    }

    fn process_name(entry: &PROCESSENTRY32W) -> String { let len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len()); String::from_utf16_lossy(&entry.szExeFile[..len]) }

    pub fn restart_desktop(prefs: &ProviderPrefs) -> bool {
        let configured = (!prefs.desktop_app_path.trim().is_empty()).then(|| prefs.desktop_app_path.trim().to_owned());
        let candidates = [
            configured.clone().filter(|p| !p.to_ascii_lowercase().starts_with("shell:")),
            Some(format!("{}\\AppData\\Local\\Programs\\Codex\\Codex.exe", crate::core::user_home().display())),
            Some(format!("{}\\AppData\\Local\\Codex\\Codex.exe", crate::core::user_home().display())),
            Some("C:\\Program Files\\Codex\\Codex.exe".into()),
            Some(format!("{}\\AppData\\Local\\Programs\\ChatGPT\\ChatGPT.exe", crate::core::user_home().display())),
            Some("C:\\Program Files\\ChatGPT\\ChatGPT.exe".into()),
        ];
        let launch = configured.filter(|p| p.to_ascii_lowercase().starts_with("shell:")).or_else(|| candidates.into_iter().flatten().find(|p| Path::new(p).exists()));
        let names: Vec<String> = [prefs.desktop_process_name.clone(), "Codex.exe".into(), "ChatGPT.exe".into()].into_iter().filter(|n| !n.trim().is_empty()).collect();
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if let Ok(snapshot) = snapshot {
            let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            if unsafe { Process32FirstW(snapshot, &mut entry).is_ok() } {
                loop {
                    let pid = entry.th32ProcessID;
                    let name = process_name(&entry);
                    let path = process_path(pid);
                    let match_name = names.iter().any(|n| n.eq_ignore_ascii_case(&name));
                    let lower_path = path.as_ref().map(|p| p.to_string_lossy().to_ascii_lowercase());
                    let known_cli = lower_path.as_ref().is_some_and(|p| p.contains("\\appdata\\local\\openai\\codex\\") || p.contains("\\.codex\\"));
                    let match_path = path.as_ref().is_some_and(|p| launch.as_ref().is_some_and(|configured| p.to_string_lossy().eq_ignore_ascii_case(configured))) || path.as_ref().is_some_and(|p| p.to_string_lossy().to_ascii_lowercase().contains("\\program files\\windowsapps\\openai."));
                    if pid != unsafe { GetCurrentProcessId() } && (match_name || match_path) && !known_cli && !path.as_ref().is_some_and(|p| TERMINAL_NAMES.iter().any(|n| p.file_name().is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(n)))) {
                        if let Ok(process) = unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) } { let _ = unsafe { TerminateProcess(process, 0) }; unsafe { let _ = CloseHandle(process); }; }
                    }
                    if unsafe { Process32NextW(snapshot, &mut entry).is_err() } { break; }
                }
            }
            unsafe { let _ = CloseHandle(snapshot); };
        }
        let Some(launch) = launch else { return false };
        std::thread::sleep(Duration::from_millis(1500));
        if launch.to_ascii_lowercase().starts_with("shell:") { open_url(&launch).is_ok() } else { std::process::Command::new(launch).spawn().is_ok() }
    }

    pub fn set_clipboard_text(value: &str) -> bool {
        let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let Ok(()) = (unsafe { OpenClipboard(Some(HWND::default())) }) else { return false };
        let result = unsafe {
            let _ = EmptyClipboard();
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * std::mem::size_of::<u16>());
            let Ok(memory) = memory else { let _ = CloseClipboard(); return false };
            let pointer = GlobalLock(memory);
            if pointer.is_null() {
                let _ = CloseClipboard();
                return false;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, pointer as *mut u8, wide.len() * 2);
            let _ = GlobalUnlock(memory);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0))).is_ok()
        };
        unsafe { let _ = CloseClipboard(); }
        result
    }
}

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(not(windows))]
pub fn restart_desktop(_prefs: &ProviderPrefs) -> bool { false }

#[cfg(not(windows))]
pub fn open_url(_url: &str) -> Result<(), String> { Err("browser launch is only implemented on Windows in this port".into()) }

#[cfg(not(windows))]
pub fn set_clipboard_text(_value: &str) -> bool { false }

#[cfg(not(windows))]
pub fn taskbar_theme() -> Option<bool> { None }

#[cfg(not(windows))]
pub fn system_locale() -> Option<String> { None }

#[cfg(not(windows))]
pub fn acquire_single_instance() -> bool { true }

#[cfg(not(windows))]
pub fn is_click_through(_hwnd: isize) -> bool { false }

#[cfg(not(windows))]
pub fn set_click_through(_hwnd: isize, _enabled: bool) {}

#[cfg(not(windows))]
#[derive(Clone, Copy, Debug)]
pub struct Rect { pub left: i32, pub top: i32, pub right: i32, pub bottom: i32 }

#[cfg(not(windows))]
pub fn widget_probe(_widget: isize, _simulate_taskbar_raise: bool) -> serde_json::Value { serde_json::json!({}) }
