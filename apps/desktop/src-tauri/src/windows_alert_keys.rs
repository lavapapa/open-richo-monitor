#![cfg(target_os = "windows")]

use crate::desktop_backend::{write_log, DesktopBackend};
use std::sync::Arc;
use tauri::{AppHandle, Manager, WebviewWindow};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{
            RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT, VK_ESCAPE, VK_RETURN, VK_SPACE,
        },
        Shell::{DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{WM_HOTKEY, WM_NCDESTROY},
    },
};

const SUBCLASS_ID: usize = 1;
const KEYS: [(u32, &str); 3] = [
    (VK_ESCAPE.0 as u32, "Esc"),
    (VK_SPACE.0 as u32, "空格"),
    (VK_RETURN.0 as u32, "回车"),
];

#[derive(Clone, Copy)]
struct Binding {
    event_id: i64,
    presentation_id: u64,
    ids: [i32; 3],
}

impl Binding {
    fn new(event_id: i64, presentation_id: u64) -> Self {
        // 应用 ID 为 0..=0xBFFF；消息泵须处理每次交付，不会积压跨 16,384 轮的旧消息。
        let first = ((presentation_id % 0x4000) * 3) as i32;
        Self {
            event_id,
            presentation_id,
            ids: [first, first + 1, first + 2],
        }
    }

    fn matches(&self, id: usize) -> bool {
        self.ids.iter().any(|&current| current as usize == id)
    }
}

struct Context {
    app: AppHandle,
    backend: Arc<DesktopBackend>,
    binding: Option<Binding>,
    registered: Vec<i32>,
}

impl Context {
    fn release(&mut self, hwnd: HWND) -> Result<(), String> {
        self.binding = None;
        let mut error = None;
        self.registered
            .retain(|&id| match unsafe { UnregisterHotKey(Some(hwnd), id) } {
                Ok(()) => false,
                Err(cause) => {
                    error.get_or_insert_with(|| format!("无法释放提醒关闭键：{cause}"));
                    true
                }
            });
        error.map_or(Ok(()), Err)
    }
}

fn handle(window: &WebviewWindow) -> Result<HWND, String> {
    window
        .hwnd()
        .map(|handle| HWND(handle.0))
        .map_err(|error| error.to_string())
}

unsafe fn context(hwnd: HWND) -> Option<*mut Context> {
    let mut data = 0;
    unsafe { GetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, Some(&mut data)) }
        .as_bool()
        .then_some(data as *mut Context)
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    data: usize,
) -> LRESULT {
    if message == WM_NCDESTROY {
        // refdata 由窗口独占；移除回调后，待下游销毁消息处理结束再释放。
        let mut context = unsafe { Box::from_raw(data as *mut Context) };
        if let Err(error) = context.release(hwnd) {
            write_log(&context.backend.data_dir, &error);
        }
        let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass), subclass_id) };
        return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    }
    if message == WM_HOTKEY {
        let context = unsafe { &*(data as *const Context) };
        if let Some(binding) = context.binding.filter(|binding| binding.matches(wparam.0)) {
            // 提醒窗口获得焦点时，由页面区分关闭键和已聚焦的购买按钮。
            if let Some(window) = context
                .app
                .get_webview_window(crate::prominent_alert::LABEL)
                .filter(|window| window.is_focused().unwrap_or(false))
            {
                let index = binding
                    .ids
                    .iter()
                    .position(|&id| id as usize == wparam.0)
                    .unwrap();
                let key = ["Escape", " ", "Enter"][index];
                let payload = serde_json::json!({ "eventId": binding.event_id, "presentationId": binding.presentation_id, "key": key });
                if let Err(error) = window.eval(&format!("window.dispatchEvent(new CustomEvent('rm-prominent-hotkey',{{detail:{payload}}}));")) {
                    write_log(&context.backend.data_dir, &error.to_string());
                }
                return LRESULT(0);
            }
            let app = context.app.clone();
            let backend = context.backend.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = crate::prominent_alert::dismiss_presentation(
                    &app,
                    &backend,
                    binding.event_id,
                    binding.presentation_id,
                )
                .await
                {
                    write_log(&backend.data_dir, &error);
                }
            });
            return LRESULT(0);
        }
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

/// 须在窗口线程、提醒显示期间调用。
pub(crate) fn bind(
    window: &WebviewWindow,
    backend: Arc<DesktopBackend>,
    event_id: i64,
    presentation_id: u64,
) -> Result<(), String> {
    let hwnd = handle(window)?;
    let data = match unsafe { context(hwnd) } {
        Some(data) => data,
        None => {
            let data = Box::into_raw(Box::new(Context {
                app: window.app_handle().clone(),
                backend,
                binding: None,
                registered: Vec::with_capacity(KEYS.len()),
            }));
            if !unsafe { SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, data as usize) }
                .as_bool()
            {
                unsafe { drop(Box::from_raw(data)) };
                return Err("无法安装提醒关闭键，请重新打开应用后重试。".into());
            }
            data
        }
    };
    // bind、unbind 与回调均在窗口线程；引用不会进入异步任务。
    let context = unsafe { &mut *data };
    context.release(hwnd)?;
    let binding = Binding::new(event_id, presentation_id);
    for (id, (key, name)) in binding.ids.into_iter().zip(KEYS) {
        if let Err(error) = unsafe { RegisterHotKey(Some(hwnd), id, MOD_NOREPEAT, key) } {
            write_log(
                &context.backend.data_dir,
                &format!("提醒关闭键（{name}）不可用：{error}；关闭按钮仍可用。"),
            );
            continue;
        }
        context.registered.push(id);
    }
    context.binding = Some(binding);
    Ok(())
}

/// 须在窗口线程调用；先清绑定，已入队的旧按键便不会关闭后续交付。
pub(crate) fn unbind(window: &WebviewWindow) -> Result<(), String> {
    let hwnd = handle(window)?;
    match unsafe { context(hwnd) } {
        Some(data) => unsafe { &mut *data }.release(hwnd),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_old_hotkey_cannot_target_a_new_presentation_of_the_same_event() {
        let old = Binding::new(42, 1);
        let current = Binding::new(42, 2);
        assert!(old.ids.iter().all(|&id| !current.matches(id as usize)));
        assert!(current.ids.iter().all(|&id| current.matches(id as usize)));
        assert_eq!(current.presentation_id, 2);
    }

    #[test]
    fn presentation_ids_stay_in_the_application_hotkey_range_at_wrap() {
        for presentation in [0, 1, 0x3FFF, 0x4000, u64::MAX] {
            let binding = Binding::new(42, presentation);
            assert!(binding.ids.iter().all(|id| (0..=0xBFFF).contains(id)));
            let next = Binding::new(42, presentation.wrapping_add(1));
            assert!(binding.ids.iter().all(|&id| !next.matches(id as usize)));
        }
    }
}
