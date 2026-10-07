//! WebView2 boundary for the browser. Environments and controllers are created
//! asynchronously, so no nested message loop re-enters GPUI, and callbacks only
//! enqueue events. GPUI's scene overlay draws menus above the child window.
//! No page-to-engine IPC.
use super::model::{PageState, Presentation, allowed_navigation};
use gpui::{Bounds, Pixels, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::{Rc, Weak},
    sync::atomic::{AtomicU32, Ordering},
};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    AcceleratorKeyPressedEventHandler, CoTaskMemPWSTR, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    DocumentTitleChangedEventHandler, DownloadStartingEventHandler, ExecuteScriptCompletedHandler,
    HistoryChangedEventHandler, NavigationCompletedEventHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, ProcessFailedEventHandler, SourceChangedEventHandler,
};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, MAPVK_VK_TO_CHAR, MapVirtualKeyW, SetFocus, VIRTUAL_KEY, VK_CONTROL,
    VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, IsChild, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER,
    SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WS_CHILD, WS_CLIPCHILDREN,
};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR, PWSTR, w};

#[derive(Default)]
struct Store {
    root: Option<PathBuf>,
    profile: Option<u32>,
}

/// A window/profile's website data. Its pages share an InPrivate profile of
/// their own, so website data is ephemeral, and WebView2 environments on the
/// same user data folder share one browser process. The folder lives under
/// Zeron's data directory and holds browser state only.
#[derive(Clone, Default)]
pub(super) struct BrowserData(Rc<RefCell<Store>>);

impl BrowserData {
    pub(super) fn set_root(&self, root: Option<PathBuf>) {
        self.0.borrow_mut().root = root;
    }

    fn location(&self) -> (PathBuf, String) {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let mut store = self.0.borrow_mut();
        let profile = *store
            .profile
            .get_or_insert_with(|| NEXT.fetch_add(1, Ordering::Relaxed));
        let root = store.root.clone().unwrap_or_else(std::env::temp_dir);
        (root.join("WebView2"), format!("zeron{profile}"))
    }
}

type Sender = tokio::sync::mpsc::Sender<NativeEvent>;

pub(super) enum NativeEvent {
    Changed,
    Finished,
    NewTab(String),
    Key(gpui::Keystroke),
    Favicon { page: String, url: String },
}

#[derive(Default)]
struct PageEvents {
    loading: bool,
    navigation: u64,
    error: Option<String>,
    requested_url: Option<String>,
}

/// Page state written by WebView2 callbacks, which never borrow the host.
#[derive(Clone)]
struct Events {
    state: Rc<RefCell<PageEvents>>,
    tx: Sender,
}

impl Events {
    fn send(&self, event: NativeEvent) {
        let _ = self.tx.try_send(event);
    }
    fn fail(&self, message: &str) {
        self.state.borrow_mut().error = Some(message.into());
        self.send(NativeEvent::Changed);
    }
}

/// The page and its visible, hit-testable part in physical client pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    page: RECT,
    region: RECT,
}

impl Geometry {
    /// `bounds` and `mask` are platform logical pixels.
    fn new(bounds: Bounds<Pixels>, mask: Bounds<Pixels>, scale: f32) -> Self {
        // Round edges rather than sizes, so adjacent GPUI content never gaps.
        let px = |value: Pixels| (f32::from(value) * scale).round() as i32;
        let rect = |b: Bounds<Pixels>| RECT {
            left: px(b.left()),
            top: px(b.top()),
            right: px(b.right()),
            bottom: px(b.bottom()),
        };
        Self {
            page: rect(bounds),
            region: rect(bounds.intersect(&mask)),
        }
    }

    fn visible(&self) -> bool {
        self.region.right > self.region.left && self.region.bottom > self.region.top
    }
}

/// The GPUI keystroke for a key pressed in the page, if it could be an app
/// shortcut. Keys without Ctrl or Alt stay with the page.
fn shortcut_combo(key: u32, ctrl: bool, alt: bool, shift: bool) -> Option<String> {
    if !ctrl && !alt {
        return None;
    }
    let key = match key {
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(key)?.to_ascii_lowercase().to_string(),
        0x70..=0x7B => format!("f{}", key - 0x6F),
        0x09 => "tab".into(),
        0x6B => "+".into(),
        0x6D => "-".into(),
        // Punctuation keys follow the active keyboard layout.
        0xBA..=0xE2 => char::from_u32(unsafe { MapVirtualKeyW(key, MAPVK_VK_TO_CHAR) } & 0xFFFF)
            .filter(|c| !c.is_control())?
            .to_lowercase()
            .to_string(),
        _ => return None,
    };
    let mut combo = String::new();
    for (down, modifier) in [(ctrl, "ctrl-"), (alt, "alt-"), (shift, "shift-")] {
        if down {
            combo.push_str(modifier);
        }
    }
    Some(combo + &key)
}

/// Zeron's message for a failed main-frame navigation, or `None` when the page
/// itself shows, including error pages that a server sent.
fn load_error(
    success: bool,
    status: COREWEBVIEW2_WEB_ERROR_STATUS,
    http_status: i32,
) -> Option<&'static str> {
    if success || http_status > 0 || status == COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED {
        return None;
    }
    Some(match status {
        COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED
        | COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET
        | COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED => {
            "The connection was interrupted. Try loading this page again."
        }
        _ => "Check the address and make sure your server is running, then try again.",
    })
}

fn take_string(f: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> Option<String> {
    let mut value = PWSTR::null();
    f(&mut value).ok()?;
    Some(CoTaskMemPWSTR::from(value).to_string())
}

pub(super) struct NativePage(Rc<RefCell<Host>>);

pub(super) struct Host {
    parent: HWND,
    container: HWND,
    controller: Option<ICoreWebView2Controller>,
    webview: Option<ICoreWebView2>,
    pending_url: Option<String>,
    events: Events,
    shortcuts: Rc<RefCell<Vec<String>>>,
    geometry: Option<Geometry>,
    presentation: Presentation,
    shown: Option<bool>,
}

impl NativePage {
    pub fn new(window: &Window, data: &BrowserData, tx: Sender) -> Result<Self, String> {
        let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)
            .map_err(|e| e.to_string())?
            .as_raw()
        else {
            return Err("Unsupported window handle".into());
        };
        let parent = HWND(handle.hwnd.get() as _);
        window
            .enable_scene_overlay()
            .map_err(|error| error.to_string())?;
        // A plain child whose window region clips the page to GPUI's mask.
        let container = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("Static"),
                w!(""),
                WS_CHILD | WS_CLIPCHILDREN,
                0,
                0,
                0,
                0,
                Some(parent),
                None,
                None,
                None,
            )
        }
        .map_err(|e| e.to_string())?;
        let host = Rc::new(RefCell::new(Host {
            parent,
            container,
            controller: None,
            webview: None,
            pending_url: None,
            events: Events {
                state: Default::default(),
                tx,
            },
            shortcuts: Default::default(),
            geometry: None,
            presentation: Presentation::Hidden,
            shown: None,
        }));
        let (folder, profile) = data.location();
        let weak = Rc::downgrade(&host);
        let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
            move |result, environment| {
                let Some(host) = weak.upgrade() else {
                    return Ok(());
                };
                let result = result
                    .and_then(|()| environment.ok_or_else(windows::core::Error::empty))
                    .and_then(|environment| {
                        create_controller(weak, &environment, container, &profile)
                    });
                if let Err(error) = result {
                    host.borrow()
                        .events
                        .fail(&format!("Could not start WebView2: {error}"));
                }
                Ok(())
            },
        ));
        unsafe {
            CreateCoreWebView2EnvironmentWithOptions(
                PCWSTR::null(),
                &HSTRING::from(folder.as_os_str()),
                &ICoreWebView2EnvironmentOptions::from(CoreWebView2EnvironmentOptions::default()),
                &handler,
            )
        }
        .map_err(|e| format!("Could not start WebView2: {e}"))?;
        Ok(Self(host))
    }
    pub fn handle(&self) -> Rc<RefCell<Host>> {
        self.0.clone()
    }
    pub fn focus_chrome(&self) {
        let _ = unsafe { SetFocus(Some(self.0.borrow().parent)) };
    }
    pub fn set_shortcuts(&self, shortcuts: Vec<String>) {
        *self.0.borrow().shortcuts.borrow_mut() = shortcuts;
    }
    pub fn present(&mut self, presentation: Presentation) {
        let mut host = self.0.borrow_mut();
        host.presentation = presentation;
        host.update_visibility();
    }
    pub fn load(&self, url: &str) -> Result<(), String> {
        let mut host = self.0.borrow_mut();
        {
            let mut state = host.events.state.borrow_mut();
            state.error = None;
            state.requested_url = Some(url.into());
            state.loading = true;
        }
        match &host.webview {
            Some(webview) => {
                unsafe { webview.Navigate(&HSTRING::from(url)) }.map_err(|e| e.to_string())
            }
            None => {
                host.pending_url = Some(url.into());
                Ok(())
            }
        }
    }
    pub fn reload(&self) {
        let host = self.0.borrow();
        host.events.state.borrow_mut().error = None;
        if let Some(webview) = &host.webview {
            let _ = unsafe { webview.Reload() };
        }
    }
    pub fn history(&self, forward: bool) {
        if let Some(webview) = &self.0.borrow().webview {
            let _ = unsafe {
                if forward {
                    webview.GoForward()
                } else {
                    webview.GoBack()
                }
            };
        }
    }
    pub fn state(&self) -> PageState {
        let host = self.0.borrow();
        let state = host.events.state.borrow();
        let webview = host.webview.as_ref();
        let flag = |back: bool| {
            webview.is_some_and(|webview| {
                let mut value = BOOL::default();
                let result = unsafe {
                    if back {
                        webview.CanGoBack(&mut value)
                    } else {
                        webview.CanGoForward(&mut value)
                    }
                };
                result.is_ok() && value.as_bool()
            })
        };
        PageState {
            url: state.requested_url.clone().or_else(|| {
                webview
                    .and_then(|w| take_string(|p| unsafe { w.Source(p) }))
                    .filter(|url| !url.is_empty() && url != "about:blank")
            }),
            title: webview
                .and_then(|w| take_string(|p| unsafe { w.DocumentTitle(p) }))
                .unwrap_or_default(),
            loading: state.loading,
            can_back: flag(true),
            can_forward: flag(false),
            error: state.error.clone(),
        }
    }
    pub fn discover_favicon(&self, page: String) {
        let host = self.0.borrow();
        let Some(webview) = &host.webview else {
            return;
        };
        let events = host.events.clone();
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
            if result.is_ok()
                && let Ok(url) = serde_json::from_str::<String>(&json)
            {
                events.send(NativeEvent::Favicon { page, url });
            }
            Ok(())
        }));
        let _ = unsafe {
            webview.ExecuteScript(&HSTRING::from(super::model::FAVICON_SCRIPT), &handler)
        };
    }
}

fn create_controller(
    host: Weak<RefCell<Host>>,
    environment: &ICoreWebView2Environment,
    container: HWND,
    profile: &str,
) -> windows::core::Result<()> {
    let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
        move |result, controller| {
            let Some(host) = host.upgrade() else {
                if let Some(controller) = controller {
                    let _ = unsafe { controller.Close() };
                }
                return Ok(());
            };
            let attached = result
                .and_then(|()| controller.ok_or_else(windows::core::Error::empty))
                .and_then(|controller| host.borrow_mut().attach(controller));
            let host = host.borrow();
            match attached {
                Ok(()) => host.events.send(NativeEvent::Changed),
                Err(error) => host
                    .events
                    .fail(&format!("Could not start WebView2: {error}")),
            }
            Ok(())
        },
    ));
    unsafe {
        let environment: ICoreWebView2Environment10 = environment.cast()?;
        let options = environment.CreateCoreWebView2ControllerOptions()?;
        options.SetIsInPrivateModeEnabled(true)?;
        options.SetProfileName(&HSTRING::from(profile))?;
        environment.CreateCoreWebView2ControllerWithOptions(container, &options, &handler)
    }
}

impl Host {
    fn attach(&mut self, controller: ICoreWebView2Controller) -> windows::core::Result<()> {
        let webview = unsafe { controller.CoreWebView2()? };
        let mut token = 0i64;
        unsafe {
            let events = self.events.clone();
            webview.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let url = take_string(|p| args.Uri(p)).unwrap_or_default();
                    if !allowed_navigation(&url) {
                        return args.SetCancel(true);
                    }
                    let mut navigation = 0;
                    args.NavigationId(&mut navigation)?;
                    *events.state.borrow_mut() = PageEvents {
                        loading: true,
                        navigation,
                        error: None,
                        requested_url: Some(url),
                    };
                    events.send(NativeEvent::Changed);
                    Ok(())
                })),
                &mut token,
            )?;
            let events = self.events.clone();
            webview.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let (mut navigation, mut success, mut http_status) = (0, BOOL::default(), 0);
                    let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                    args.NavigationId(&mut navigation)?;
                    args.IsSuccess(&mut success)?;
                    args.WebErrorStatus(&mut status)?;
                    if let Ok(args) = args.cast::<ICoreWebView2NavigationCompletedEventArgs2>() {
                        args.HttpStatusCode(&mut http_status)?;
                    }
                    let error = {
                        let mut state = events.state.borrow_mut();
                        // A superseded navigation completes after its successor starts.
                        if navigation != state.navigation {
                            return Ok(());
                        }
                        state.loading = false;
                        state.error =
                            load_error(success.as_bool(), status, http_status).map(Into::into);
                        if state.error.is_none() {
                            state.requested_url = None;
                        }
                        state.error.is_some()
                    };
                    events.send(if error {
                        NativeEvent::Changed
                    } else {
                        NativeEvent::Finished
                    });
                    Ok(())
                })),
                &mut token,
            )?;
            let events = self.events.clone();
            webview.add_SourceChanged(
                &SourceChangedEventHandler::create(Box::new(move |_, _| {
                    events.send(NativeEvent::Changed);
                    Ok(())
                })),
                &mut token,
            )?;
            let events = self.events.clone();
            webview.add_HistoryChanged(
                &HistoryChangedEventHandler::create(Box::new(move |_, _| {
                    events.send(NativeEvent::Changed);
                    Ok(())
                })),
                &mut token,
            )?;
            let events = self.events.clone();
            webview.add_DocumentTitleChanged(
                &DocumentTitleChangedEventHandler::create(Box::new(move |_, _| {
                    events.send(NativeEvent::Changed);
                    Ok(())
                })),
                &mut token,
            )?;
            let events = self.events.clone();
            webview.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let url = take_string(|p| args.Uri(p)).unwrap_or_default();
                    args.SetHandled(true)?;
                    if allowed_navigation(&url) {
                        events.send(NativeEvent::NewTab(url));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let events = self.events.clone();
            webview.add_ProcessFailed(
                &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                    args.ProcessFailedKind(&mut kind)?;
                    if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED {
                        events.fail("The page stopped responding. Reload to continue.");
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let events = self.events.clone();
            webview.cast::<ICoreWebView2_4>()?.add_DownloadStarting(
                &DownloadStartingEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                        events.fail(
                            "This file can’t be previewed here. Open it in your default browser.",
                        );
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let (shortcuts, events) = (self.shortcuts.clone(), self.events.clone());
            controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let (mut kind, mut key) = (COREWEBVIEW2_KEY_EVENT_KIND::default(), 0);
                    args.KeyEventKind(&mut kind)?;
                    args.VirtualKey(&mut key)?;
                    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                        && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
                    {
                        return Ok(());
                    }
                    let down = |key: VIRTUAL_KEY| GetKeyState(key.0 as i32) < 0;
                    let Some(combo) =
                        shortcut_combo(key, down(VK_CONTROL), down(VK_MENU), down(VK_SHIFT))
                    else {
                        return Ok(());
                    };
                    let Ok(keystroke) = gpui::Keystroke::parse(&combo) else {
                        return Ok(());
                    };
                    let app_key = shortcuts
                        .borrow()
                        .iter()
                        .any(|s| gpui::Keystroke::parse(s).is_ok_and(|s| s == keystroke));
                    if (super::model::browser_shortcut(&combo) || app_key)
                        && events.tx.try_send(NativeEvent::Key(keystroke)).is_ok()
                    {
                        args.SetHandled(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            if let Some(url) = self.pending_url.take() {
                webview.Navigate(&HSTRING::from(url))?;
            }
        }
        self.controller = Some(controller);
        self.webview = Some(webview);
        self.apply_geometry();
        self.shown = None;
        self.update_visibility();
        Ok(())
    }

    /// Positions the page from platform logical pixels. GPUI's scene overlay
    /// disables child windows while it owns drag input. A child window cannot
    /// pass hit testing through while it paints, so the page keeps the
    /// divider overlap rather than showing a strip of GPUI there.
    pub fn sync(
        &mut self,
        bounds: Bounds<Pixels>,
        mask: Bounds<Pixels>,
        _dragging: bool,
        _resize_inset: Pixels,
    ) {
        let scale = unsafe { GetDpiForWindow(self.parent) } as f32 / 96.;
        let geometry = Geometry::new(bounds, mask, scale);
        if self.geometry != Some(geometry) {
            self.geometry = Some(geometry);
            self.apply_geometry();
        }
        self.update_visibility();
    }

    fn apply_geometry(&self) {
        let Some(Geometry { page, region }) = self.geometry else {
            return;
        };
        let (width, height) = (page.right - page.left, page.bottom - page.top);
        unsafe {
            let _ = SetWindowPos(
                self.container,
                None,
                page.left,
                page.top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if let Some(controller) = &self.controller {
                let _ = controller.SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                });
            }
            let region = CreateRectRgn(
                region.left - page.left,
                region.top - page.top,
                region.right - page.left,
                region.bottom - page.top,
            );
            SetWindowRgn(self.container, Some(region), true);
        }
    }

    fn has_focus(&self) -> bool {
        unsafe { IsChild(self.container, GetFocus()).as_bool() }
    }

    fn update_visibility(&mut self) {
        let visible = self.controller.is_some()
            && self.presentation != Presentation::Hidden
            && self.events.state.borrow().error.is_none()
            && self.geometry.is_some_and(|geometry| geometry.visible());
        if self.shown == Some(visible) {
            return;
        }
        self.shown = Some(visible);
        unsafe {
            if !visible && self.has_focus() {
                let _ = SetFocus(Some(self.parent));
            }
            let _ = ShowWindow(self.container, if visible { SW_SHOWNA } else { SW_HIDE });
            if let Some(controller) = &self.controller {
                let _ = controller.SetIsVisible(visible);
            }
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        unsafe {
            if self.has_focus() {
                let _ = SetFocus(Some(self.parent));
            }
            if let Some(controller) = self.controller.take() {
                let _ = controller.Close();
            }
            let _ = DestroyWindow(self.container);
        }
    }
}

#[cfg(feature = "browser-fixture")]
impl NativePage {
    pub fn fixture_visible(&self) -> bool {
        self.0.borrow().shown == Some(true)
    }
    pub fn fixture_eval(&self, script: &str) {
        if let Some(webview) = &self.0.borrow().webview {
            let _ = unsafe { webview.ExecuteScript(&HSTRING::from(script), None) };
        }
    }
    pub fn fixture_physical_width(&self) -> i32 {
        let mut rect = RECT::default();
        let _ = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetClientRect(
                self.0.borrow().container,
                &mut rect,
            )
        };
        rect.right
    }
    /// GPUI's scene overlay disables child windows while it owns input.
    pub fn fixture_input_blocked(&self) -> bool {
        !unsafe {
            windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(self.0.borrow().container)
        }
        .as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn page_geometry_follows_display_density_and_ui_scale() {
        // 125% UI scale on a 150% display: layout units scale by both.
        let ui = |b: Bounds<Pixels>| b.map(|v| v * 1.25);
        let bounds = Bounds::new(point(px(400.), px(40.)), size(px(300.25), px(200.)));
        let full = Geometry::new(ui(bounds), ui(bounds), 1.5);
        assert_eq!(full.page, rect(750, 75, 1313, 450));
        assert_eq!(full.region, full.page);

        // A right occlusion clips the page without resizing it.
        let mask = Bounds::new(point(px(0.), px(0.)), size(px(600.), px(1000.)));
        let clipped = Geometry::new(ui(bounds), ui(mask), 1.5);
        assert_eq!(clipped.page, full.page);
        assert_eq!(clipped.region, rect(750, 75, 1125, 450));
        assert!(clipped.visible());

        let hidden = Bounds::new(point(px(800.), px(0.)), size(px(10.), px(10.)));
        assert!(!Geometry::new(ui(bounds), ui(hidden), 1.5).visible());
    }

    #[test]
    fn page_keys_reach_app_shortcuts_only_with_ctrl_or_alt() {
        for (key, ctrl, alt, shift, combo) in [
            (0x57, true, false, false, "ctrl-w"),
            (0x54, true, false, false, "ctrl-t"),
            (0x42, true, false, false, "ctrl-b"),
            (0x54, true, false, true, "ctrl-shift-t"),
            (0x30, true, false, false, "ctrl-0"),
            (0x6B, true, false, false, "ctrl-+"),
            (0x6D, true, false, false, "ctrl--"),
            (0x74, false, true, false, "alt-f5"),
            (0x09, true, false, true, "ctrl-shift-tab"),
        ] {
            assert_eq!(
                shortcut_combo(key, ctrl, alt, shift).as_deref(),
                Some(combo)
            );
            assert!(gpui::Keystroke::parse(combo).is_ok(), "{combo}");
        }
        assert_eq!(shortcut_combo(0x41, false, false, true), None);
        assert_eq!(shortcut_combo(0x74, false, false, false), None);
        assert_eq!(shortcut_combo(0x0D, true, false, false), None);
        // Zoom reaches the UI-scale bindings rather than the page.
        let zoom: Vec<_> = crate::ui_scale::combos()
            .into_iter()
            .filter_map(|(combo, _)| gpui::Keystroke::parse(combo).ok())
            .collect();
        for key in [0x6B, 0x6D, 0x30] {
            let keystroke =
                gpui::Keystroke::parse(&shortcut_combo(key, true, false, false).unwrap()).unwrap();
            assert!(zoom.contains(&keystroke), "{key:#x}");
        }
    }

    #[test]
    fn server_error_pages_show_instead_of_load_errors() {
        assert_eq!(
            load_error(false, COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN, 404),
            None
        );
        assert_eq!(
            load_error(false, COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN, 502),
            None
        );
        assert_eq!(
            load_error(true, COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN, 200),
            None
        );
        assert_eq!(
            load_error(false, COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED, 0),
            None
        );
        assert!(
            load_error(false, COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT, 0)
                .is_some_and(|message| message.contains("server is running"))
        );
        assert!(
            load_error(false, COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET, 0)
                .is_some_and(|message| message.contains("interrupted"))
        );
    }
}
