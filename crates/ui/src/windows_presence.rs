//! On Windows, closing the window used to quit, and quitting waits for live
//! runs to drain. Meanwhile the process was invisible but still owned the data
//! dir, so relaunching said Zeron was still running and Task Manager was the
//! only way out. Now Zeron lives in the tray like other background-capable
//! Windows apps:
//!
//! - closing the window hides it; agents keep running;
//! - the tray icon reopens it (click) or quits (right-click → Quit Zeron);
//! - launching Zeron again brings the running instance forward.

use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, LPARAM, LRESULT, POINT, WAIT_OBJECT_0, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows_sys::Win32::System::Threading::{
    CreateEventW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForMultipleObjects,
};
use windows_sys::Win32::UI::Shell::{
    ExtractIconExW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetCursorPos, GetMessageW, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, RegisterClassW,
    RegisterWindowMessageW, SW_HIDE, SW_SHOW, SetForegroundWindow, ShowWindow, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
    WNDCLASSW,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    Show,
    Quit,
}

const TRAY_CALLBACK: u32 = WM_APP + 1;
const TRAY_ID: u32 = 1;
const MENU_OPEN: usize = 1;
const MENU_QUIT: usize = 2;

static REQUESTS: OnceLock<UnboundedSender<Request>> = OnceLock::new();
/// The tray's hidden window, once its icon is in the notification area.
static TRAY_WINDOW: OnceLock<usize> = OnceLock::new();
/// Wakes the second-launch listener so it releases the name at quit.
static STOP: OnceLock<usize> = OnceLock::new();
static TIP_SHOWN: AtomicBool = AtomicBool::new(false);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// The running instance's wake-up event for `data_dir`. A dev copy with its
/// own data dir gets its own, so it never raises the installed app.
fn event_name(data_dir: &Path) -> String {
    let key = data_dir.to_string_lossy().replace('/', "\\").to_lowercase();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.trim_end_matches('\\').bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("Local\\sh.zeron.app.show.{hash:016x}")
}

/// A second launch: ask the instance already running on `data_dir` to show
/// its window. False when none is listening, so this launch should start.
pub fn signal_running(data_dir: &Path) -> bool {
    let name = wide(&event_name(data_dir));
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if event.is_null() {
            return false;
        }
        let signalled = SetEvent(event) != 0;
        CloseHandle(event);
        signalled
    }
}

/// Listen for second launches on `data_dir` and show the tray icon. Both
/// report through the returned channel.
pub(crate) fn start(data_dir: &Path) -> UnboundedReceiver<Request> {
    let (tx, rx) = unbounded();
    let _ = REQUESTS.set(tx.clone());
    listen(data_dir, tx);
    let spawned = std::thread::Builder::new()
        .name("zeron-tray".into())
        .spawn(run_tray);
    if let Err(error) = spawned {
        tracing::warn!(%error, "tray icon not started");
    }
    rx
}

/// Wait on this data dir's wake-up event until [`stop`].
fn listen(data_dir: &Path, tx: UnboundedSender<Request>) {
    let name = wide(&event_name(data_dir));
    // Auto-reset: one SetEvent, one wake. Holding the handle is what keeps
    // the name claimed, so a second launch finds it.
    let (show, stop) = unsafe {
        (
            CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()),
            CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()),
        )
    };
    if show.is_null() || stop.is_null() {
        tracing::warn!("second-launch listener not started");
        return;
    }
    let _ = STOP.set(stop as usize);
    let handles = [show as usize, stop as usize];
    let spawned = std::thread::Builder::new()
        .name("zeron-reopen".into())
        .spawn(move || {
            let handles = handles.map(|handle| handle as HANDLE);
            while unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) }
                == WAIT_OBJECT_0
            {
                if tx.unbounded_send(Request::Show).is_err() {
                    break;
                }
            }
            unsafe { CloseHandle(handles[0]) };
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "second-launch listener not started");
    }
}

/// Quit: remove the icon (no ghost until hover) and release the second-launch
/// name, so a launch during shutdown starts normally instead of waking us.
pub(crate) fn stop() {
    if let Some(&window) = TRAY_WINDOW.get() {
        let data = icon_data(window as HWND);
        unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
    }
    if let Some(&stop) = STOP.get() {
        unsafe { SetEvent(stop as HANDLE) };
    }
}

/// The window's close button: hide it to the tray instead of quitting, when
/// the tray icon is there to bring it back. False means close as usual.
pub(crate) fn hide_to_tray(window: &gpui::Window) -> bool {
    let Some(&tray) = TRAY_WINDOW.get() else {
        return false;
    };
    let Some(hwnd) = hwnd(window) else {
        return false;
    };
    unsafe { ShowWindow(hwnd, SW_HIDE) };
    if !TIP_SHOWN.swap(true, Ordering::Relaxed) {
        show_tip(tray as HWND);
    }
    true
}

/// Undo [`hide_to_tray`]; harmless on a visible window.
pub(crate) fn unhide(window: &gpui::Window) {
    if let Some(hwnd) = hwnd(window) {
        unsafe { ShowWindow(hwnd, SW_SHOW) };
    }
}

fn hwnd(window: &gpui::Window) -> Option<HWND> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}

fn icon_data(window: HWND) -> NOTIFYICONDATAW {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = TRAY_ID;
    data
}

fn copy_into(slot: &mut [u16], text: &str) {
    // Keep the trailing NUL the zeroed struct already has.
    let room = slot.len() - 1;
    for (slot, unit) in slot[..room].iter_mut().zip(text.encode_utf16()) {
        *slot = unit;
    }
}

fn add_icon(window: HWND) -> bool {
    let mut data = icon_data(window);
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = TRAY_CALLBACK;
    // The exe's own embedded icon, at tray size.
    let mut exe = [0u16; 1024];
    let len =
        unsafe { GetModuleFileNameW(std::ptr::null_mut(), exe.as_mut_ptr(), exe.len() as u32) };
    if len > 0 {
        unsafe { ExtractIconExW(exe.as_ptr(), 0, std::ptr::null_mut(), &mut data.hIcon, 1) };
    }
    copy_into(&mut data.szTip, "Zeron");
    unsafe { Shell_NotifyIconW(NIM_ADD, &data) != 0 }
}

/// The first close of a run explains where the window went.
fn show_tip(window: HWND) {
    let mut data = icon_data(window);
    data.uFlags = NIF_INFO;
    data.dwInfoFlags = NIIF_INFO;
    copy_into(&mut data.szInfoTitle, "Zeron is still running");
    copy_into(
        &mut data.szInfo,
        "Agents keep working in the background. Click the tray icon to reopen, or right-click it to quit.",
    );
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

/// Explorer broadcasts this after it restarts; the icon must be added again.
fn taskbar_created() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) })
}

fn run_tray() {
    let class = wide("ZeronTray");
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(tray_proc);
        wc.hInstance = instance;
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        // A hidden top-level window rather than a message-only one: only
        // top-level windows hear Explorer's TaskbarCreated broadcast.
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Zeron tray").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if window.is_null() {
            tracing::warn!("tray window not created");
            return;
        }
        taskbar_created();
        // Hiding to the tray is only safe once the icon to come back by exists.
        if add_icon(window) {
            let _ = TRAY_WINDOW.set(window as usize);
        } else {
            tracing::warn!("tray icon not added; the close button quits");
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn send(request: Request) {
    if let Some(tx) = REQUESTS.get() {
        let _ = tx.unbounded_send(request);
    }
}

unsafe extern "system" fn tray_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == TRAY_CALLBACK {
        match lparam as u32 {
            WM_LBUTTONUP => send(Request::Show),
            WM_RBUTTONUP => match unsafe { show_menu(window) } {
                MENU_OPEN => send(Request::Show),
                MENU_QUIT => send(Request::Quit),
                _ => {}
            },
            _ => {}
        }
        return 0;
    }
    if message == taskbar_created() {
        add_icon(window);
        return 0;
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// The right-click menu at the cursor; returns the chosen command.
unsafe fn show_menu(window: HWND) -> usize {
    unsafe {
        let menu = CreatePopupMenu();
        AppendMenuW(menu, MF_STRING, MENU_OPEN, wide("Open Zeron").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, MENU_QUIT, wide("Quit Zeron").as_ptr());
        let mut at = POINT { x: 0, y: 0 };
        GetCursorPos(&mut at);
        // Without these two, the menu would not close on an outside click.
        SetForegroundWindow(window);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            0,
            window,
            std::ptr::null(),
        );
        PostMessageW(window, WM_NULL, 0, 0);
        DestroyMenu(menu);
        chosen as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[test]
    fn each_data_dir_has_its_own_event() {
        let installed = event_name(Path::new(r"C:\Users\me\AppData\Local\Zeron"));
        assert_eq!(
            installed,
            event_name(Path::new(r"c:/users/ME/AppData/Local/Zeron/"))
        );
        assert_ne!(
            installed,
            event_name(Path::new(r"C:\Users\me\AppData\Local\Zeron-dev"))
        );
        assert!(installed.starts_with("Local\\"));
    }

    #[test]
    fn a_second_launch_wakes_the_running_instance() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!signal_running(dir.path()), "nothing is running yet");
        let (tx, mut rx) = unbounded();
        listen(dir.path(), tx);
        assert!(signal_running(dir.path()));
        let request = futures::executor::block_on(rx.next());
        assert_eq!(request, Some(Request::Show));
    }

    #[test]
    fn tray_text_keeps_its_terminator() {
        let mut slot = [0u16; 8];
        copy_into(&mut slot, "a tip far longer than the slot");
        assert_eq!(slot[7], 0);
        assert_eq!(String::from_utf16_lossy(&slot[..7]), "a tip f");
    }
}
