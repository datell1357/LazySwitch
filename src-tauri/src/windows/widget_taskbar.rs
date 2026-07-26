use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewWindow, Wry};

use crate::app_state::AppState;
use crate::config::CompactPosition;
use crate::powershell;
use crate::windows::widget_geometry::{
    compact_bottom_left_bounds, compact_bottom_right_bounds, compact_taskbar_bounds,
    taskbar_theme_is_light, DisplayArea, WidgetBounds,
};

const WIDGET_COMPACT_WIDTH: f64 = 280.0;
const POWERSHELL_TIMEOUT_MS: u64 = 1_800;
static COMPACT_POSITION_REQUEST: AtomicU64 = AtomicU64::new(0);

const TRAY_RECT_POWERSHELL: &str = r#"
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class LazySwitchTrayNative {
  [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  public static extern IntPtr FindWindowW(string lpClassName, string lpWindowName);
  [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  public static extern IntPtr FindWindowExW(IntPtr hWndParent, IntPtr hWndChildAfter, string lpszClass, string lpszWindow);
  [DllImport("user32.dll", SetLastError = true)]
  public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
'@
$tray = [LazySwitchTrayNative]::FindWindowW('Shell_TrayWnd', $null)
$notify = [LazySwitchTrayNative]::FindWindowExW($tray, [IntPtr]::Zero, 'TrayNotifyWnd', $null)
$rect = New-Object LazySwitchTrayNative+RECT
if ($tray -eq [IntPtr]::Zero -or $notify -eq [IntPtr]::Zero -or -not [LazySwitchTrayNative]::GetWindowRect($notify, [ref]$rect)) { exit 1 }
[Console]::WriteLine((ConvertTo-Json @{ left = $rect.Left; top = $rect.Top; right = $rect.Right; bottom = $rect.Bottom } -Compress))
"#;

const TASKBAR_THEME_POWERSHELL: &str = r#"
$personalize = Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize' -ErrorAction Stop
$dwm = Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\DWM' -ErrorAction Stop
[Console]::WriteLine((ConvertTo-Json @{
  SystemUsesLightTheme = [int]$personalize.SystemUsesLightTheme
  ColorPrevalence = [int]$dwm.ColorPrevalence
  AccentColor = [uint32]$dwm.AccentColor
} -Compress))
"#;

#[derive(Debug, Deserialize)]
struct PhysicalRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TaskbarRegistryTheme {
    system_uses_light_theme: i64,
    color_prevalence: i64,
    accent_color: u32,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct WidgetTaskbarTheme {
    light: bool,
}

fn powershell_exe() -> std::path::PathBuf {
    std::env::var("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

pub fn display_areas(app: &AppHandle<Wry>) -> Result<(DisplayArea, DisplayArea, f64), String> {
    let monitor = app
        .primary_monitor()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "primary monitor unavailable".to_string())?;
    let scale = monitor.scale_factor();
    let size = monitor.size();
    let position = monitor.position();
    let work = monitor.work_area();
    Ok((
        DisplayArea {
            x: f64::from(position.x) / scale,
            y: f64::from(position.y) / scale,
            width: f64::from(size.width) / scale,
            height: f64::from(size.height) / scale,
        },
        DisplayArea {
            x: f64::from(work.position.x) / scale,
            y: f64::from(work.position.y) / scale,
            width: f64::from(work.size.width) / scale,
            height: f64::from(work.size.height) / scale,
        },
        scale,
    ))
}

async fn query_tray_notify_rect(scale: f64) -> Option<WidgetBounds> {
    let output = powershell::run(
        &powershell_exe().to_string_lossy(),
        TRAY_RECT_POWERSHELL,
        POWERSHELL_TIMEOUT_MS,
    )
    .await
    .ok()?;
    let rect: PhysicalRect = serde_json::from_str(output.trim()).ok()?;
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return None;
    }
    Some(WidgetBounds {
        x: f64::from(rect.left) / scale,
        y: f64::from(rect.top) / scale,
        width: f64::from(rect.right - rect.left) / scale,
        height: f64::from(rect.bottom - rect.top) / scale,
    })
}

async fn query_taskbar_theme() -> Option<WidgetTaskbarTheme> {
    let output = powershell::run(
        &powershell_exe().to_string_lossy(),
        TASKBAR_THEME_POWERSHELL,
        POWERSHELL_TIMEOUT_MS,
    )
    .await
    .ok()?;
    let theme: TaskbarRegistryTheme = serde_json::from_str(output.trim()).ok()?;
    taskbar_theme_is_light(
        theme.system_uses_light_theme,
        theme.color_prevalence,
        theme.accent_color,
    )
    .map(|light| WidgetTaskbarTheme { light })
}

pub fn is_taskbar_compact_widget(app: &AppHandle<Wry>) -> bool {
    app.state::<Mutex<AppState>>()
        .lock()
        .map(|state| {
            state.cfg.usage_widget.minimized
                && state.cfg.usage_widget.compact_position == CompactPosition::Taskbar
        })
        .unwrap_or(false)
}

pub fn send_taskbar_theme(window: &WebviewWindow<Wry>, theme: Option<WidgetTaskbarTheme>) {
    let _ = window.emit("widget:taskbar-theme", theme);
}

pub fn apply_taskbar_theme(app: &AppHandle<Wry>, window: WebviewWindow<Wry>) {
    if !is_taskbar_compact_widget(app) {
        send_taskbar_theme(&window, None);
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let theme = query_taskbar_theme()
            .await
            .unwrap_or(WidgetTaskbarTheme { light: false });
        if is_taskbar_compact_widget(&app) {
            send_taskbar_theme(&window, Some(theme));
        }
    });
}

pub async fn position_compact_widget(
    app: AppHandle<Wry>,
    window: WebviewWindow<Wry>,
    height: f64,
) -> Result<(), String> {
    let request = COMPACT_POSITION_REQUEST.fetch_add(1, Ordering::Relaxed) + 1;
    let position = app
        .state::<Mutex<AppState>>()
        .lock()
        .map_err(|error| error.to_string())?
        .cfg
        .usage_widget
        .compact_position;
    let (display, work_area, scale) = display_areas(&app)?;
    let bounds = match position {
        CompactPosition::Taskbar => match query_tray_notify_rect(scale).await {
            Some(tray) => {
                compact_taskbar_bounds(height, WIDGET_COMPACT_WIDTH, display, work_area, tray)
                    .unwrap_or_else(|| {
                        compact_bottom_right_bounds(
                            height,
                            WIDGET_COMPACT_WIDTH,
                            work_area,
                            display,
                        )
                    })
            }
            None => compact_bottom_right_bounds(height, WIDGET_COMPACT_WIDTH, work_area, display),
        },
        CompactPosition::BottomRight => {
            compact_bottom_right_bounds(height, WIDGET_COMPACT_WIDTH, work_area, display)
        }
        CompactPosition::BottomLeft => {
            compact_bottom_left_bounds(height, WIDGET_COMPACT_WIDTH, work_area, display)
        }
    };
    let still_compact = app
        .state::<Mutex<AppState>>()
        .lock()
        .map_err(|error| error.to_string())?
        .cfg
        .usage_widget
        .minimized;
    if still_compact && COMPACT_POSITION_REQUEST.load(Ordering::Relaxed) == request {
        window
            .set_size(LogicalSize::new(bounds.width, bounds.height))
            .map_err(|error| error.to_string())?;
        window
            .set_position(LogicalPosition::new(bounds.x, bounds.y))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}
