#[cfg(windows)]
mod windows_impl {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;

    use tauri::{AppHandle, WebviewWindow, Wry};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };

    const WM_CONTEXTMENU: u32 = 0x007b;
    const WIDGET_SUBCLASS_ID: usize = 0x4c53_5754;
    static APP: OnceLock<AppHandle<Wry>> = OnceLock::new();
    static CONTEXT_MENU_OPEN: AtomicBool = AtomicBool::new(false);

    fn hwnd(window: &WebviewWindow<Wry>) -> Result<HWND, String> {
        window
            .hwnd()
            .map(|handle| HWND(handle.0))
            .map_err(|error| error.to_string())
    }

    unsafe extern "system" fn widget_subclass_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _subclass_id: usize,
        _reference_data: usize,
    ) -> LRESULT {
        if message == WM_CONTEXTMENU {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                if let Some(app) = APP.get() {
                    CONTEXT_MENU_OPEN.store(true, Ordering::Release);
                    let _menu_guard = ContextMenuGuard;
                    let _ = super::super::widget::show_widget_context_menu(app);
                }
            }));
        }
        // SAFETY: UB category 13 (library contract). `hwnd`, message parameters,
        // and subclass-chain state are supplied by comctl32 to this callback;
        // forwarding every message preserves the next subclass's contract.
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    struct ContextMenuGuard;

    impl Drop for ContextMenuGuard {
        fn drop(&mut self) {
            CONTEXT_MENU_OPEN.store(false, Ordering::Release);
        }
    }

    pub fn install_context_menu_hook(
        app: &AppHandle<Wry>,
        window: &WebviewWindow<Wry>,
    ) -> Result<(), String> {
        let _ = APP.set(app.clone());
        let hwnd = hwnd(window)?;
        // SAFETY: UB categories 8 and 13 (FFI/library contract). `hwnd` belongs
        // to this live Tauri window, the callback uses no borrowed ref-data,
        // and its fixed ID is paired with `remove_context_menu_hook`.
        let installed =
            unsafe { SetWindowSubclass(hwnd, Some(widget_subclass_proc), WIDGET_SUBCLASS_ID, 0) };
        installed
            .as_bool()
            .then_some(())
            .ok_or_else(|| "SetWindowSubclass failed".to_string())
    }

    pub fn remove_context_menu_hook(window: &WebviewWindow<Wry>) -> Result<(), String> {
        let hwnd = hwnd(window)?;
        // SAFETY: UB categories 8 and 13 (FFI/library contract). This uses the
        // same live HWND, callback address, and ID passed at installation.
        let removed =
            unsafe { RemoveWindowSubclass(hwnd, Some(widget_subclass_proc), WIDGET_SUBCLASS_ID) };
        removed
            .as_bool()
            .then_some(())
            .ok_or_else(|| "RemoveWindowSubclass failed".to_string())
    }

    pub fn reassert_topmost(window: &WebviewWindow<Wry>) -> Result<(), String> {
        let hwnd = hwnd(window)?;
        // SAFETY: UB category 8 (FFI boundary). `hwnd` is obtained from the
        // live Tauri window; flags explicitly prohibit activation, movement,
        // and resizing, so the zero geometry arguments are ignored by Win32.
        unsafe {
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        }
        .map_err(|error| error.to_string())
    }

    pub fn context_menu_open() -> bool {
        CONTEXT_MENU_OPEN.load(Ordering::Acquire)
    }
}

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(not(windows))]
mod non_windows_impl {
    use tauri::{AppHandle, WebviewWindow, Wry};

    pub fn install_context_menu_hook(
        _app: &AppHandle<Wry>,
        _window: &WebviewWindow<Wry>,
    ) -> Result<(), String> {
        Ok(())
    }

    pub fn remove_context_menu_hook(_window: &WebviewWindow<Wry>) -> Result<(), String> {
        Ok(())
    }

    pub fn reassert_topmost(_window: &WebviewWindow<Wry>) -> Result<(), String> {
        Ok(())
    }

    pub const fn context_menu_open() -> bool {
        false
    }
}

#[cfg(not(windows))]
pub use non_windows_impl::*;
