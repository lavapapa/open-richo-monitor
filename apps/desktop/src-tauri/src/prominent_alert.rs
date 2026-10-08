use crate::desktop_backend::{DesktopBackend, OperationResult};
use ricoh_monitor_core::app::ProminentAlert;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
    State, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

pub(crate) const LABEL: &str = "prominent-alert";

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Presentation {
    #[serde(flatten)]
    alert: ProminentAlert,
    presentation_id: u64,
    wait_for_frame: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct Geometry {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    scale: f64,
}

fn update_geometry<F>(
    current: &mut Option<Geometry>,
    next: Geometry,
    apply: F,
) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    if *current != Some(next) {
        apply()?;
        *current = Some(next);
    }
    Ok(())
}

fn position_overlay(
    window: &WebviewWindow,
    view: &ProminentView,
    monitor: &tauri::Monitor,
) -> Result<(), String> {
    let next = Geometry {
        position: *monitor.position(),
        size: *monitor.size(),
        scale: monitor.scale_factor(),
    };
    update_geometry(&mut view.geometry.lock().unwrap(), next, || {
        window
            .set_position(next.position)
            .map_err(|error| error.to_string())?;
        window
            .set_size(next.size)
            .map_err(|error| error.to_string())
    })
}

struct PendingDisplay {
    event_id: i64,
    presentation_id: u64,
    sender: tokio::sync::oneshot::Sender<Result<(), String>>,
}

#[derive(Default)]
pub(crate) struct ProminentView {
    channel: Mutex<Option<tauri::ipc::Channel<Presentation>>>,
    ready: tokio::sync::Notify,
    next_presentation: AtomicU64,
    displayed: Mutex<Option<PendingDisplay>>,
    visible: Mutex<Option<i64>>,
    geometry: Mutex<Option<Geometry>>,
    #[cfg(target_os = "windows")]
    previous_foreground: Mutex<Option<isize>>,
}

impl ProminentView {
    fn complete(&self, presentation_id: u64, result: Result<(), String>) {
        let mut pending = self.displayed.lock().unwrap();
        if pending.as_ref().map(|pending| pending.presentation_id) == Some(presentation_id) {
            if let Some(pending) = pending.take() {
                let _ = pending.sender.send(result);
            }
        }
    }
}

fn clear_alert_if_current(alert: &mut Option<ProminentAlert>, event_id: i64) {
    if alert.as_ref().map(|alert| alert.event_id) == Some(event_id) {
        *alert = None;
    }
}

async fn display_and_acknowledge<F, Display, A, Fut>(
    alert: ProminentAlert,
    display: F,
    acknowledge: A,
) -> Result<(), String>
where
    F: FnOnce(ProminentAlert) -> Display,
    Display: std::future::Future<Output = Result<(), String>>,
    A: FnOnce(i64) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let event_id = alert.event_id;
    display(alert).await?;
    acknowledge(event_id).await
}

fn overlay_bounds(
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    scale: f64,
) -> (LogicalPosition<f64>, LogicalSize<f64>) {
    (position.to_logical(scale), size.to_logical(scale))
}

#[cfg(target_os = "macos")]
fn overlay_collection_behavior() -> objc2_app_kit::NSWindowCollectionBehavior {
    use objc2_app_kit::NSWindowCollectionBehavior as Behavior;
    Behavior::CanJoinAllSpaces
        | Behavior::CanJoinAllApplications
        | Behavior::FullScreenAuxiliary
        | Behavior::Stationary
        | Behavior::IgnoresCycle
}

#[cfg(target_os = "windows")]
mod windows_native {
    use super::*;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, IsWindow, IsWindowVisible, SetForegroundWindow,
    };

    fn handle(window: &WebviewWindow) -> Result<HWND, String> {
        window
            .hwnd()
            .map(|handle| HWND(handle.0))
            .map_err(|error| error.to_string())
    }

    pub(super) fn foreground() -> isize {
        unsafe { GetForegroundWindow().0 as isize }
    }

    pub(super) fn focus(window: &WebviewWindow) -> Result<(), String> {
        let handle = handle(window)?;
        unsafe {
            if GetForegroundWindow() != handle {
                // 系统可保留原应用焦点；覆盖与关闭键无需等待其输入队列。
                let _ = SetForegroundWindow(handle);
            }
        }
        Ok(())
    }

    pub(super) fn restore(window: &WebviewWindow, previous: Option<isize>) {
        unsafe {
            if GetForegroundWindow()
                != match handle(window) {
                    Ok(handle) => handle,
                    Err(_) => return,
                }
            {
                return;
            }
            if let Some(previous) = previous {
                let previous = HWND(previous as *mut std::ffi::c_void);
                if IsWindow(Some(previous)).as_bool() && IsWindowVisible(previous).as_bool() {
                    let _ = SetForegroundWindow(previous);
                }
            }
        }
    }

    pub(super) fn interactive_desktop() -> bool {
        use windows::Win32::{
            Foundation::HANDLE,
            System::StationsAndDesktops::{
                CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_CONTROL_FLAGS,
                DESKTOP_READOBJECTS, UOI_NAME,
            },
        };
        unsafe {
            let Ok(desktop) =
                OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS)
            else {
                return false;
            };
            let mut name = [0u16; 256];
            let result = GetUserObjectInformationW(
                HANDLE(desktop.0),
                UOI_NAME,
                Some(name.as_mut_ptr().cast()),
                std::mem::size_of_val(&name) as u32,
                None,
            );
            let _ = CloseDesktop(desktop);
            result.is_ok()
                && String::from_utf16_lossy(
                    &name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())],
                )
                .eq_ignore_ascii_case("Default")
        }
    }

    pub(super) fn verify(window: &WebviewWindow) -> Result<(), String> {
        if !interactive_desktop() {
            return Err("Windows 桌面已锁定，提醒将在解锁后显示。".into());
        }
        let handle = handle(window)?;
        unsafe {
            if !IsWindowVisible(handle).as_bool() {
                return Err("Windows 尚未显示提醒。".into());
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod native {
    use objc2::{rc::Retained, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSColor, NSPanel, NSView, NSWindow, NSWindowStyleMask};
    use objc2_foundation::ns_string;
    use std::cell::RefCell;

    objc2::define_class!(
        #[unsafe(super(NSPanel))]
        #[thread_kind = MainThreadOnly]
        struct AlertPanel;

        impl AlertPanel {
            #[unsafe(method(canBecomeKeyWindow))]
            fn can_become_key_window(&self) -> bool { true }

            #[unsafe(method(canBecomeMainWindow))]
            fn can_become_main_window(&self) -> bool { false }
        }
    );

    struct PanelSession {
        panel: Retained<AlertPanel>,
        webview: Retained<NSView>,
        parent_view: Retained<NSView>,
        previous_key_window: Option<Retained<NSWindow>>,
        was_active: bool,
        event_id: i64,
    }

    impl PanelSession {
        fn focus(&self) -> Result<(), String> {
            self.panel.makeKeyAndOrderFront(None);
            self.panel.orderFrontRegardless();
            if !self.panel.makeFirstResponder(Some(&self.webview))
                || !self.panel.isKeyWindow()
                || !self.panel.isVisible()
                || !self.panel.isOnActiveSpace()
            {
                return Err("macOS 未显示可接收键盘的桌面覆盖图层。".into());
            }
            Ok(())
        }
    }

    impl Drop for PanelSession {
        fn drop(&mut self) {
            self.panel.orderOut(None);
            self.panel.setContentView(None);
            self.parent_view.addSubview(&self.webview);
            self.webview.setFrame(self.parent_view.bounds());
            self.panel.close();
            if restore_app_focus(self.was_active) {
                let application = NSApplication::sharedApplication(self.panel.mtm());
                // 前台提醒须归还原 keyWindow。
                #[allow(deprecated)]
                application.activateIgnoringOtherApps(false);
                if let Some(window) = &self.previous_key_window {
                    window.makeKeyAndOrderFront(None);
                }
            }
        }
    }

    pub(super) fn restore_app_focus(was_active: bool) -> bool {
        was_active
    }

    // AppKit 对象在主线程创建、使用与释放，不进入 Send/Sync 的后端状态。
    thread_local! {
        static PANEL: RefCell<Option<PanelSession>> = const { RefCell::new(None) };
    }

    pub(super) fn focus_existing() -> Result<bool, String> {
        let _mtm = MainThreadMarker::new().ok_or("桌面覆盖图层须在 macOS 主线程聚焦。")?;
        PANEL.with(|slot| match slot.borrow().as_ref() {
            Some(session) => session.focus().map(|()| true),
            None => Ok(false),
        })
    }

    pub(super) fn close(event_id: i64) -> Result<(), String> {
        let _mtm = MainThreadMarker::new().ok_or("桌面覆盖图层须在 macOS 主线程关闭。")?;
        let session = PANEL.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().map(|session| session.event_id) == Some(event_id) {
                slot.take()
            } else {
                None
            }
        });
        if let Some(session) = session {
            drop(session);
        }
        Ok(())
    }

    pub(super) fn show(owner: &NSWindow, webview: &NSView, event_id: i64) -> Result<(), String> {
        let mtm = MainThreadMarker::new().ok_or("桌面覆盖图层须在 macOS 主线程显示。")?;
        let screen = owner.screen().ok_or("无法读取提醒所在显示器。")?;
        // Tauri 在回调期间保留网页与原容器，superview 可在主线程借用。
        let parent_view = unsafe { webview.superview() }.ok_or("提醒网页没有原生容器。")?;
        let application = NSApplication::sharedApplication(mtm);
        let panel: Retained<AlertPanel> = unsafe {
            objc2::msg_send![AlertPanel::alloc(mtm),
                initWithContentRect: screen.frame(),
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: objc2_app_kit::NSBackingStoreType::Buffered,
                defer: false]
        };
        // session 独占 panel 的强引用；close 不得再次 release。
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setTitle(ns_string!("库存提醒 · RM"));
        panel.setCollectionBehavior(super::overlay_collection_behavior());
        panel.setLevel(objc2_app_kit::NSScreenSaverWindowLevel);
        panel.setHidesOnDeactivate(false);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(false);
        panel.setWorksWhenModal(true);
        let session = PanelSession {
            panel,
            // with_webview 回调期间，Tauri 保证 WKWebView 指针存续。
            webview: unsafe { Retained::retain(webview as *const NSView as *mut NSView) }
                .ok_or("无法保留提醒网页。")?,
            parent_view,
            previous_key_window: application.keyWindow(),
            was_active: application.isActive(),
            event_id,
        };
        // 保留 Tao 原容器，delegate 在窗口销毁时仍须读取其 contentView。
        session.webview.removeFromSuperview();
        session.panel.setContentView(Some(&session.webview));
        session.panel.setFrame_display(screen.frame(), false);
        session.focus()?;
        PANEL.with(|slot| *slot.borrow_mut() = Some(session));
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn focus_existing(_app: &AppHandle) -> Result<bool, String> {
    native::focus_existing()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn focus_existing(app: &AppHandle) -> Result<bool, String> {
    let backend = app.state::<Arc<DesktopBackend>>();
    if backend.prominent_view.visible.lock().unwrap().is_none() {
        return Ok(false);
    }
    let Some(window) = app.get_webview_window(LABEL) else {
        return Ok(false);
    };
    window.show().map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    {
        windows_native::focus(&window)?;
        windows_native::verify(&window)?;
    }
    #[cfg(not(target_os = "windows"))]
    window.set_focus().map_err(|error| error.to_string())?;
    Ok(true)
}

async fn show_overlay(
    window: &WebviewWindow,
    backend: Arc<DesktopBackend>,
    event_id: i64,
    presentation_id: u64,
) -> Result<(), String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    #[cfg(not(target_os = "macos"))]
    let owner = window.clone();
    window
        .with_webview(move |_platform| {
            // 校验与显示在同一主线程回调内完成，超时的旧页面回调不能显示下一次交付。
            let pending = backend.prominent_view.displayed.lock().unwrap();
            let current = pending.as_ref().is_some_and(|pending| {
                pending.event_id == event_id && pending.presentation_id == presentation_id
            });
            let result = if !current
                || backend
                    .exit_phase
                    .load(std::sync::atomic::Ordering::Acquire)
                    != 0
            {
                Err("提醒已更新。".into())
            } else {
                #[cfg(target_os = "macos")]
                let result = unsafe {
                    native::show(
                        &*_platform.ns_window().cast::<objc2_app_kit::NSWindow>(),
                        &*_platform.inner().cast::<objc2_app_kit::NSView>(),
                        event_id,
                    )
                };
                #[cfg(target_os = "windows")]
                let result = (|| {
                    if let Err(error) = crate::windows_alert_keys::bind(
                        &owner,
                        backend.clone(),
                        event_id,
                        presentation_id,
                    ) {
                        crate::desktop_backend::write_log(&backend.data_dir, &error);
                    }
                    *backend.prominent_view.previous_foreground.lock().unwrap() =
                        Some(windows_native::foreground());
                    owner.show().map_err(|error| error.to_string())?;
                    windows_native::verify(&owner)
                })();
                #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                let result = owner
                    .show()
                    .and_then(|()| owner.set_focus())
                    .map_err(|error| error.to_string());
                if result.is_ok() {
                    *backend.prominent_view.visible.lock().unwrap() = Some(event_id);
                }
                result
            };
            drop(pending);
            #[cfg(not(target_os = "windows"))]
            backend
                .prominent_view
                .complete(presentation_id, result.clone());
            let _ = sender.send(result);
        })
        .map_err(|error| error.to_string())?;
    receiver.await.map_err(|error| error.to_string())?
}

#[cfg(target_os = "macos")]
async fn close_overlay(app: &AppHandle, event_id: i64) -> Result<(), String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = sender.send(native::close(event_id));
    })
    .map_err(|error| error.to_string())?;
    receiver.await.map_err(|error| error.to_string())?
}

#[cfg(target_os = "windows")]
async fn close_overlay(app: &AppHandle, _event_id: i64) -> Result<(), String> {
    let Some(window) = app.get_webview_window(LABEL) else {
        return Ok(());
    };
    let backend = app.state::<Arc<DesktopBackend>>().inner().clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let shortcuts = crate::windows_alert_keys::unbind(&window);
        let previous = backend
            .prominent_view
            .previous_foreground
            .lock()
            .unwrap()
            .take();
        windows_native::restore(&window, previous);
        let hidden = window.hide().map_err(|error| error.to_string());
        let _ = sender.send(shortcuts.and(hidden));
    })
    .map_err(|error| error.to_string())?;
    receiver.await.map_err(|error| error.to_string())?
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
async fn close_overlay(_app: &AppHandle, _event_id: i64) -> Result<(), String> {
    Ok(())
}

fn target_monitor(app: &AppHandle) -> Result<tauri::Monitor, String> {
    let monitor = match app.get_webview_window("main") {
        Some(window) => window
            .current_monitor()
            .map_err(|error| error.to_string())?,
        None => None,
    };
    match monitor {
        Some(monitor) => Ok(monitor),
        None => app
            .primary_monitor()
            .map_err(|error| error.to_string())?
            .ok_or("无法读取显示器边界。".into()),
    }
}

pub(crate) fn prepare(app: &AppHandle) -> Result<WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        return Ok(window);
    }
    let monitor = target_monitor(app)?;
    let (position, size) =
        overlay_bounds(*monitor.position(), *monitor.size(), monitor.scale_factor());
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("prominent-alert".into()))
        .title("库存提醒 · RM")
        .position(position.x, position.y)
        .inner_size(size.width, size.height)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .focused(false)
        .build()
        .map_err(|error| error.to_string())?;
    let backend = app.state::<Arc<DesktopBackend>>();
    position_overlay(&window, &backend.prominent_view, &monitor)?;
    Ok(window)
}

async fn present(
    app: &AppHandle,
    backend: &DesktopBackend,
    alert: ProminentAlert,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    if backend.prominent_alert.lock().unwrap().is_some() {
        return Err("已有突出提醒正在显示，请先关闭。".into());
    }
    let window = prepare(app)?;
    let event_id = alert.event_id;
    let presentation_id = backend
        .prominent_view
        .next_presentation
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    *backend.prominent_alert.lock().unwrap() = Some(alert.clone());
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let channel = loop {
            let ready = backend.prominent_view.ready.notified();
            if let Some(channel) = backend.prominent_view.channel.lock().unwrap().clone() {
                break channel;
            }
            ready.await;
        };
        let monitor = target_monitor(app)?;
        // 相同屏幕复用原位置与大小，显示器或缩放改变时重新按像素定位。
        position_overlay(&window, &backend.prominent_view, &monitor)?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        *backend.prominent_view.displayed.lock().unwrap() = Some(PendingDisplay {
            event_id,
            presentation_id,
            sender,
        });
        channel
            .send(Presentation {
                alert,
                presentation_id,
                wait_for_frame: cfg!(target_os = "windows"),
            })
            .map_err(|error| error.to_string())?;
        receiver.await.map_err(|error| error.to_string())?
    })
    .await
    .unwrap_or_else(|_| Err("提醒页面未完成显示。".into()));
    if let Err(error) = result {
        backend
            .prominent_view
            .complete(presentation_id, Err(error.clone()));
        let _ = close_overlay(app, event_id).await;
        *backend.prominent_view.visible.lock().unwrap() = None;
        #[cfg(not(target_os = "windows"))]
        let _ = window.hide();
        clear_alert_if_current(&mut backend.prominent_alert.lock().unwrap(), event_id);
        return Err(format!("无法显示突出提醒：{error}"));
    }
    crate::desktop_backend::write_log(
        &backend.data_dir,
        &format!(
            "突出提醒 {event_id} 内容就绪并显示：{:.1} ms",
            started.elapsed().as_secs_f64() * 1000.0
        ),
    );
    Ok(())
}

#[tauri::command]
pub fn get_prominent_alert(backend: State<'_, Arc<DesktopBackend>>) -> Option<ProminentAlert> {
    backend.prominent_alert.lock().unwrap().clone()
}

#[tauri::command]
pub fn subscribe_prominent_alert(
    backend: State<'_, Arc<DesktopBackend>>,
    channel: tauri::ipc::Channel<Presentation>,
) {
    *backend.prominent_view.channel.lock().unwrap() = Some(channel);
    backend.prominent_view.ready.notify_one();
}

#[tauri::command]
pub async fn show_prominent_alert(
    app: AppHandle,
    backend: State<'_, Arc<DesktopBackend>>,
    event_id: i64,
    presentation_id: u64,
) -> Result<(), String> {
    let result = match app.get_webview_window(LABEL) {
        Some(window) => {
            show_overlay(&window, backend.inner().clone(), event_id, presentation_id).await
        }
        None => Err("提醒窗口已关闭。".into()),
    };
    if result.is_err() {
        backend
            .prominent_view
            .complete(presentation_id, result.clone());
    }
    result
}

#[tauri::command]
pub async fn confirm_prominent_alert_frame(
    app: AppHandle,
    backend: State<'_, Arc<DesktopBackend>>,
    event_id: i64,
    presentation_id: u64,
) -> Result<(), String> {
    let current = backend
        .prominent_view
        .displayed
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|pending| {
            pending.event_id == event_id && pending.presentation_id == presentation_id
        });
    if !current {
        return Err("提醒已更新。".into());
    }
    #[cfg(target_os = "windows")]
    if let Some(window) = app.get_webview_window(LABEL) {
        if let Err(error) = windows_native::verify(&window) {
            backend
                .prominent_view
                .complete(presentation_id, Err(error.clone()));
            return Err(error);
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = app;
    backend.prominent_view.complete(presentation_id, Ok(()));
    Ok(())
}

pub(crate) fn forward(backend: Arc<DesktopBackend>, app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            if backend
                .exit_phase
                .load(std::sync::atomic::Ordering::Acquire)
                != 0
            {
                break;
            }
            #[cfg(target_os = "windows")]
            if !windows_native::interactive_desktop() {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
            let mut retry = false;
            {
                let _operation = backend.prominent_operation.lock().await;
                if backend.exit_phase.load(Ordering::Acquire) != 0 {
                    break;
                }
                if backend.prominent_alert.lock().unwrap().is_none() {
                    match backend.app.claim_prominent_alert().await {
                        Ok(Some(alert)) => {
                            if let Err(error) = display_and_acknowledge(
                                alert,
                                |alert| present(&app, &backend, alert),
                                |id| {
                                    let monitor = &backend.app;
                                    async move {
                                        monitor
                                            .acknowledge_prominent_alert(id)
                                            .await
                                            .map_err(|error| error.to_string())
                                    }
                                },
                            )
                            .await
                            {
                                retry = true;
                                crate::desktop_backend::write_log(&backend.data_dir, &error);
                                let _ = app.emit("desktop-error", error);
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            retry = true;
                            let error = error.to_string();
                            crate::desktop_backend::write_log(&backend.data_dir, &error);
                            let _ = app.emit("desktop-error", error);
                        }
                    }
                }
            }
            if retry {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            } else {
                backend.app.wait_for_prominent_alert().await;
            }
        }
    });
}

#[tauri::command]
pub async fn complete_purchase(
    app: AppHandle,
    backend: State<'_, Arc<DesktopBackend>>,
    event_id: i64,
) -> Result<OperationResult, String> {
    let _operation = backend.prominent_operation.lock().await;
    dismiss_current(&app, &backend, event_id).await?;
    app.emit_to("main", "purchase-completed", ())
        .map_err(|e| e.to_string())?;
    crate::show_main(&app);
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn dismiss_prominent_alert(
    app: AppHandle,
    backend: State<'_, Arc<DesktopBackend>>,
    event_id: i64,
) -> Result<OperationResult, String> {
    dismiss(&app, &backend, event_id).await
}

pub(crate) async fn dismiss(
    app: &AppHandle,
    backend: &DesktopBackend,
    event_id: i64,
) -> Result<OperationResult, String> {
    let _operation = backend.prominent_operation.lock().await;
    dismiss_current(app, backend, event_id).await
}

#[cfg(target_os = "windows")]
pub(crate) async fn dismiss_presentation(
    app: &AppHandle,
    backend: &DesktopBackend,
    event_id: i64,
    presentation_id: u64,
) -> Result<OperationResult, String> {
    let _operation = backend.prominent_operation.lock().await;
    if backend
        .prominent_view
        .next_presentation
        .load(Ordering::Relaxed)
        != presentation_id
        || *backend.prominent_view.visible.lock().unwrap() != Some(event_id)
    {
        return Ok(OperationResult::ok());
    }
    dismiss_current(app, backend, event_id).await
}

async fn dismiss_current(
    app: &AppHandle,
    backend: &DesktopBackend,
    event_id: i64,
) -> Result<OperationResult, String> {
    if backend
        .prominent_alert
        .lock()
        .unwrap()
        .as_ref()
        .map(|alert| alert.event_id)
        != Some(event_id)
    {
        return Err("提醒已更新，请重新读取后再关闭。".into());
    }
    close_overlay(app, event_id).await?;
    *backend.prominent_view.visible.lock().unwrap() = None;
    #[cfg(not(target_os = "windows"))]
    if let Some(window) = app.get_webview_window(LABEL) {
        window.hide().map_err(|error| error.to_string())?;
    }
    clear_alert_if_current(&mut backend.prominent_alert.lock().unwrap(), event_id);
    backend.app.wake_prominent_alerts();
    Ok(OperationResult::ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn alert() -> ProminentAlert {
        ProminentAlert {
            event_id: 42,
            product_id: "65".into(),
            name: "GR IIIx".into(),
            image_url: None,
            image_path: None,
            price: None,
            stock: 1.0,
            at: String::new(),
        }
    }

    #[test]
    fn repeated_alerts_only_reposition_when_monitor_geometry_changes() {
        let base = Geometry {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(1024, 768),
            scale: 1.0,
        };
        let calls = AtomicUsize::new(0);
        let mut current = None;
        for next in [
            base,
            base,
            Geometry {
                position: PhysicalPosition::new(-1024, 0),
                ..base
            },
            Geometry {
                scale: 1.25,
                ..base
            },
            Geometry {
                size: PhysicalSize::new(1920, 1080),
                ..base
            },
        ] {
            update_geometry(&mut current, next, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        let previous = current;
        assert!(update_geometry(&mut current, base, || Err("窗口大小更新失败".into())).is_err());
        assert!(current == previous);
        update_geometry(&mut current, base, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn expired_presentation_cannot_complete_a_retry_of_the_same_event() {
        tauri::async_runtime::block_on(async {
            let view = ProminentView::default();
            let (sender, receiver) = tokio::sync::oneshot::channel();
            *view.displayed.lock().unwrap() = Some(PendingDisplay {
                event_id: 42,
                presentation_id: 1,
                sender,
            });
            view.complete(1, Err("显示超时".into()));
            assert_eq!(receiver.await.unwrap().unwrap_err(), "显示超时");
            let (sender, mut receiver) = tokio::sync::oneshot::channel();
            *view.displayed.lock().unwrap() = Some(PendingDisplay {
                event_id: 42,
                presentation_id: 2,
                sender,
            });
            view.complete(1, Ok(()));
            assert!(matches!(
                receiver.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            view.complete(2, Ok(()));
            assert!(receiver.await.unwrap().is_ok());
            view.complete(2, Ok(()));
            assert!(view.displayed.lock().unwrap().is_none());
        });
    }

    #[test]
    fn overlay_bounds_cover_full_monitor_at_each_scale() {
        for (position, size, scale) in [
            ((0, 0), (3024, 1964), 2.0),
            ((-3840, -2160), (3840, 2160), 2.0),
            ((1920, 0), (2560, 1440), 1.0),
        ] {
            let position = tauri::PhysicalPosition::new(position.0, position.1);
            let size = tauri::PhysicalSize::new(size.0, size.1);
            let (logical_position, logical_size) = overlay_bounds(position, size, scale);
            assert_eq!(logical_position.to_physical::<i32>(scale), position);
            assert_eq!(logical_size.to_physical::<u32>(scale), size);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_overlay_joins_spaces_without_becoming_a_fullscreen_space() {
        use objc2_app_kit::NSWindowCollectionBehavior as Behavior;
        let behavior = overlay_collection_behavior();
        assert!(behavior.contains(
            Behavior::CanJoinAllSpaces
                | Behavior::FullScreenAuxiliary
                | Behavior::CanJoinAllApplications
                | Behavior::Stationary
        ));
        assert!(!behavior.intersects(Behavior::FullScreenPrimary | Behavior::MoveToActiveSpace));
    }

    #[test]
    fn macos_minimum_version_supports_cross_application_panels() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(config["bundle"]["macOS"]["minimumSystemVersion"], "13.0");
    }

    #[test]
    fn destroyed_window_clears_its_alert_once_and_preserves_later_alerts() {
        let mut current = Some(alert());
        clear_alert_if_current(&mut current, 41);
        assert_eq!(current.as_ref().unwrap().event_id, 42);
        clear_alert_if_current(&mut current, 42);
        assert!(current.is_none());
        clear_alert_if_current(&mut current, 42);
        assert!(current.is_none());
        let mut next = alert();
        next.event_id = 43;
        current = Some(next);
        clear_alert_if_current(&mut current, 42);
        assert_eq!(current.as_ref().unwrap().event_id, 43);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dismissing_background_panel_preserves_the_foreground_app() {
        assert!(!native::restore_app_focus(false));
        assert!(native::restore_app_focus(true));
    }

    #[test]
    fn failed_window_keeps_event_unacknowledged_for_retry() {
        tauri::async_runtime::block_on(async {
            let acknowledged = AtomicUsize::new(0);
            let failed = display_and_acknowledge(
                alert(),
                |_| async { Err("窗口创建失败".into()) },
                |_| async {
                    acknowledged.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await;
            assert!(failed.is_err());
            assert_eq!(acknowledged.load(Ordering::SeqCst), 0);
            display_and_acknowledge(
                alert(),
                |_| async { Ok(()) },
                |id| {
                    let acknowledged = &acknowledged;
                    async move {
                        assert_eq!(id, 42);
                        acknowledged.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .await
            .unwrap();
            assert_eq!(acknowledged.load(Ordering::SeqCst), 1);
        });
    }
}

#[tauri::command]
pub async fn test_prominent_alert(
    app: AppHandle,
    backend: State<'_, Arc<DesktopBackend>>,
    product_id: String,
) -> Result<OperationResult, String> {
    let _operation = backend.prominent_operation.lock().await;
    backend.ensure_worker_alive()?;
    let snapshot = backend
        .app
        .snapshot()
        .await
        .map_err(|error| error.to_string())?;
    let product = snapshot
        .products
        .into_iter()
        .chain(snapshot.catalog)
        .find(|product| product.product_id == product_id)
        .ok_or("请先选择一个有效商品。")?;
    let metadata = product.metadata;
    let alert = ProminentAlert {
        event_id: -(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis() as i64),
        product_id,
        name: product.name,
        image_url: metadata
            .as_ref()
            .and_then(|metadata| metadata.image_url.clone()),
        image_path: product.image_path,
        price: metadata.and_then(|metadata| metadata.price),
        stock: product
            .observation
            .and_then(|observation| observation.stock)
            .unwrap_or(3.0),
        at: String::new(),
    };
    present(&app, &backend, alert).await?;
    Ok(OperationResult::ok())
}
