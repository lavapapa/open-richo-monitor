mod desktop_backend;
mod desktop_notifications;
mod notification_proxy;
#[cfg(any(
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ),
    all(target_os = "windows", target_arch = "x86_64")
))]
mod notification_runtime_path;
mod prominent_alert;
mod updates;
#[cfg(target_os = "windows")]
mod windows_alert_keys;

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

use desktop_backend::{DesktopBackend, DesktopSnapshot};
use ricoh_monitor_core::app::{MonitoringAction, RuntimeState};
use tauri::{
    menu::{Menu, MenuItem},
    plugin::PermissionState,
    tray::TrayIconBuilder,
    Emitter, Manager, RunEvent, WindowEvent,
};

struct TrayControls {
    status: MenuItem<tauri::Wry>,
    pause: MenuItem<tauri::Wry>,
    resume: MenuItem<tauri::Wry>,
}

#[derive(Debug, PartialEq, Eq)]
enum ExitDecision {
    DrainWorker,
    WaitForWorker,
    Exit,
}

fn decide_exit(phase: &AtomicU8) -> ExitDecision {
    match phase.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => ExitDecision::DrainWorker,
        Err(2) => ExitDecision::Exit,
        Err(_) => ExitDecision::WaitForWorker,
    }
}

fn update_tray_status(app: &tauri::AppHandle, state: RuntimeState) {
    let Some(controls) = app.try_state::<TrayControls>() else {
        return;
    };
    let (label, can_pause, can_resume) = match state {
        RuntimeState::Monitoring => ("正在监控", true, false),
        RuntimeState::PartialError => ("部分检查失败", true, false),
        RuntimeState::OutsideSchedule => ("等待计划开始", true, false),
        RuntimeState::Paused => ("已暂停", false, true),
        RuntimeState::Stopped => ("已停止", false, false),
        RuntimeState::SetupIncomplete => ("尚未完成设置", false, false),
        RuntimeState::WorkerFailed => ("监控任务已停止", false, false),
    };
    let _ = controls.status.set_text(format!("状态：{label}"));
    let _ = controls.pause.set_enabled(can_pause);
    let _ = controls.resume.set_enabled(can_resume);
}

fn show_main(app: &tauri::AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || match prominent_alert::focus_existing(&handle) {
        Ok(true) => {}
        Ok(false) => {
            if let Some(window) = handle.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }
        Err(error) => {
            if let Some(backend) = handle.try_state::<Arc<DesktopBackend>>() {
                desktop_backend::write_log(&backend.data_dir, &error);
            }
            let _ = handle.emit("desktop-error", error);
        }
    });
}

fn install_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "打开主窗口", true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", "状态：尚未完成设置", false, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "暂停监控", false, None::<&str>)?;
    let resume = MenuItem::with_id(app, "resume", "继续监控", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &status, &pause, &resume, &quit])?;
    app.manage(TrayControls {
        status,
        pause,
        resume,
    });
    #[cfg(target_os = "macos")]
    let icon = tauri::include_image!("icons/tray-macos.png");
    #[cfg(not(target_os = "macos"))]
    let icon = tauri::include_image!("icons/tray-windows.png");
    TrayIconBuilder::with_id("ricoh-monitor")
        .icon(icon)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("open-richo-monitor")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            "pause" | "resume" => {
                let Some(backend) = app.try_state::<Arc<DesktopBackend>>() else {
                    return;
                };
                let backend = backend.inner().clone();
                let action = if event.id().as_ref() == "pause" {
                    MonitoringAction::Pause
                } else {
                    MonitoringAction::Resume
                };
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let result = match backend.ensure_worker_alive() {
                        Ok(()) => backend
                            .app
                            .monitoring_action(action)
                            .await
                            .map_err(|error| error.to_string()),
                        Err(error) => Err(error),
                    };
                    if let Err(error) = result {
                        desktop_backend::write_log(&backend.data_dir, &error);
                        let _ = app.emit("desktop-error", error);
                    }
                });
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn forward_snapshots(backend: Arc<DesktopBackend>, app: tauri::AppHandle) {
    let mut snapshots = backend.subscribe();
    tauri::async_runtime::spawn(async move {
        if let Ok(initial) = backend.app.snapshot().await {
            update_tray_status(&app, initial.runtime.state);
        }
        loop {
            let snapshot = match snapshots.recv().await {
                Ok(snapshot) => snapshot,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            update_tray_status(&app, snapshot.runtime.state);
            let _ = app.emit(
                "desktop-state-changed",
                DesktopSnapshot::new(snapshot, backend.notification_permission()),
            );
        }
    });
}

fn forward_system_notifications(backend: Arc<DesktopBackend>, app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut permission_check = tokio::time::Instant::now();
        let mut permission_readable = false;
        loop {
            if backend.exit_phase.load(Ordering::Acquire) != 0 {
                break;
            }
            if tokio::time::Instant::now() >= permission_check {
                let permission_update = match desktop_notifications::permission_state(&app).await {
                    Ok(permission) => backend.set_notification_permission(permission).await,
                    Err(error) => Err(error),
                };
                permission_readable = permission_update.is_ok();
                permission_check = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
                if let Err(error) = permission_update {
                    desktop_backend::write_log(&backend.data_dir, &error);
                }
            }
            if !permission_readable || backend.notification_permission() != PermissionState::Granted
            {
                tokio::select! {
                    _ = backend.app.wait_for_system_notification() => {},
                    _ = tokio::time::sleep_until(permission_check) => {},
                }
                continue;
            }
            match backend.app.claim_system_notification().await {
                Ok(Some((id, event))) => {
                    let result = desktop_notifications::show(&app, &event).await;
                    if let Err(error) = &result {
                        desktop_backend::write_log(&backend.data_dir, error);
                        permission_check = tokio::time::Instant::now();
                    }
                    if let Err(error) = backend.app.finish_system_notification(id, result).await {
                        desktop_backend::write_log(&backend.data_dir, &error.to_string());
                    }
                }
                Ok(None) => {
                    tokio::select! {
                        _ = backend.app.wait_for_system_notification() => {},
                        _ = tokio::time::sleep_until(permission_check) => {},
                    }
                }
                Err(error) => {
                    desktop_backend::write_log(&backend.data_dir, &error.to_string());
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    });
}

fn forward_notification_proxy(backend: Arc<DesktopBackend>) {
    tauri::async_runtime::spawn(async move {
        while backend.exit_phase.load(Ordering::Acquire) == 0 {
            if let Ok(snapshot) = backend.app.snapshot().await {
                if snapshot.config.use_system_proxy
                    && snapshot.channels.iter().any(|channel| channel.enabled)
                {
                    backend
                        .app
                        .set_notification_proxy_url(notification_proxy::current());
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_main(app)
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updates::UpdateState::default())
        .invoke_handler(tauri::generate_handler![
            desktop_backend::get_desktop_snapshot,
            desktop_backend::refresh_catalog_metadata,
            desktop_backend::query_messages,
            desktop_backend::refresh_notification_permission,
            desktop_backend::request_notification_permission,
            desktop_backend::test_system_notification,
            desktop_backend::set_system_notifications_enabled,
            desktop_backend::save_monitoring_config,
            desktop_backend::set_auto_start_monitoring,
            desktop_backend::monitoring_action,
            desktop_backend::validate_product,
            desktop_backend::add_product,
            desktop_backend::set_product_enabled,
            desktop_backend::remove_product,
            desktop_backend::start_product_scan,
            desktop_backend::control_product_scan,
            desktop_backend::begin_channel_binding,
            desktop_backend::begin_channel_rebinding,
            desktop_backend::begin_channel_editing,
            desktop_backend::end_channel_editing,
            desktop_backend::channel_binding_status,
            desktop_backend::cancel_channel_binding,
            desktop_backend::detect_notification_channel_groups,
            desktop_backend::detect_binding_groups,
            desktop_backend::save_notification_channel,
            desktop_backend::test_notification_channel,
            desktop_backend::set_notification_channel_enabled,
            desktop_backend::remove_notification_channel,
            desktop_backend::add_proxy,
            desktop_backend::import_proxies,
            desktop_backend::test_proxy,
            desktop_backend::set_proxy_enabled,
            desktop_backend::remove_proxy,
            desktop_backend::set_login_start,
            desktop_backend::open_logs_directory,
            updates::check_for_updates,
            updates::install_update,
            desktop_backend::export_configuration_file,
            desktop_backend::import_configuration_file,
            desktop_backend::restore_defaults,
            desktop_backend::complete_setup,
            desktop_backend::set_onboarding_products,
            desktop_backend::set_product_prominent_alert,
            prominent_alert::subscribe_prominent_alert,
            prominent_alert::show_prominent_alert,
            prominent_alert::dismiss_prominent_alert,
            prominent_alert::test_prominent_alert,
            desktop_backend::create_diagnostic_preview,
            desktop_backend::save_diagnostic_report,
            desktop_notifications::open_external_url
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                } else if window.label() == prominent_alert::LABEL {
                    api.prevent_close();
                    if let Some(backend) = window.try_state::<Arc<DesktopBackend>>() {
                        let id = backend
                            .prominent_alert
                            .lock()
                            .unwrap()
                            .as_ref()
                            .map(|alert| alert.event_id);
                        if let Some(id) = id {
                            let backend = backend.inner().clone();
                            let app = window.app_handle().clone();
                            tauri::async_runtime::spawn(async move {
                                let _ = prominent_alert::dismiss(&app, &backend, id).await;
                            });
                        }
                    }
                }
            }
        })
        .setup(|app| {
            #[cfg(target_os = "macos")]
            desktop_notifications::install_foreground_delegate();
            let data_dir = app.path().app_data_dir().map_err(std::io::Error::other)?;
            let backend = Arc::new(
                DesktopBackend::open(data_dir, PermissionState::Prompt)
                    .map_err(std::io::Error::other)?,
            );
            #[cfg(any(
                all(
                    target_os = "macos",
                    any(target_arch = "aarch64", target_arch = "x86_64")
                ),
                all(target_os = "windows", target_arch = "x86_64")
            ))]
            {
                backend
                    .app
                    .set_notification_runtime_path(notification_runtime_path::current()?);
                backend.app.set_notification_runtime_arguments(
                    notification_runtime_path::arguments(
                        &app.path().resource_dir().map_err(std::io::Error::other)?,
                    ),
                );
            }
            app.manage(backend.clone());
            if let Err(error) = prominent_alert::prepare(app.handle()) {
                desktop_backend::write_log(
                    &backend.data_dir,
                    &format!("准备突出提醒失败：{error}"),
                );
            }
            forward_snapshots(backend.clone(), app.handle().clone());
            backend
                .app
                .set_notification_proxy_url(notification_proxy::current());
            forward_notification_proxy(backend.clone());
            forward_system_notifications(backend.clone(), app.handle().clone());
            prominent_alert::forward(backend.clone(), app.handle().clone());
            let runtime = backend.clone();
            let app_handle = app.handle().clone();
            let worker = tauri::async_runtime::spawn(async move {
                let worker_app = runtime.app.clone();
                let result = tauri::async_runtime::spawn(async move { worker_app.run().await })
                    .await
                    .map_err(|error| format!("监控任务异常退出：{error}"))
                    .and_then(|result| result.map_err(|error| error.to_string()));
                runtime.worker_alive.store(false, Ordering::Release);
                if let Err(error) = result {
                    runtime.app.report_worker_failure(error.clone()).await;
                    desktop_backend::write_log(&runtime.data_dir, &error);
                    let _ = app_handle.emit("desktop-error", error);
                }
            });
            *backend.worker_task.lock().unwrap() = Some(worker);
            install_tray(app)?;
            updates::start_hourly_checks(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build open-richo-monitor app");

    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if matches!(&event, RunEvent::Reopen { .. }) {
            show_main(app);
        }
        if let RunEvent::ExitRequested { api, code, .. } = event {
            let Some(backend) = app.try_state::<Arc<DesktopBackend>>() else {
                return;
            };
            match decide_exit(&backend.exit_phase) {
                ExitDecision::Exit => {}
                ExitDecision::WaitForWorker => api.prevent_exit(),
                ExitDecision::DrainWorker => {
                    api.prevent_exit();
                    #[cfg(target_os = "windows")]
                    if let Some(window) = app.get_webview_window(prominent_alert::LABEL) {
                        if let Err(error) = windows_alert_keys::unbind(&window) {
                            desktop_backend::write_log(&backend.data_dir, &error);
                        }
                    }
                    let backend = backend.inner().clone();
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        drain_worker(&backend).await;
                        app.exit(code.unwrap_or(0));
                    });
                }
            }
        }
    });
}

async fn drain_worker(backend: &DesktopBackend) {
    backend.app.shutdown();
    let worker = backend.worker_task.lock().unwrap().take();
    if let Some(worker) = worker {
        let _ = worker.await;
    }
    backend.app.shutdown_notification_runtime().await;
    backend.exit_phase.store(2, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::{decide_exit, ExitDecision};
    use std::sync::atomic::{AtomicU8, Ordering};

    #[test]
    fn tray_images_are_small_and_independent_of_the_rm_app_icon() {
        let mac = tauri::include_image!("icons/tray-macos.png");
        let windows = tauri::include_image!("icons/tray-windows.png");
        assert_eq!((mac.width(), mac.height()), (44, 44));
        assert_eq!((windows.width(), windows.height()), (32, 32));
        // GR 机身平顶；中央上沿留白，快门只在左侧轻微突出。
        for y in 5..9 {
            for x in 18..28 {
                assert_eq!(mac.rgba()[((y * 44 + x) * 4 + 3) as usize], 0);
            }
        }
        assert!(mac
            .rgba()
            .chunks_exact(4)
            .all(|pixel| pixel[..3] == [0, 0, 0]));
        assert!(mac.rgba().chunks_exact(4).any(|pixel| pixel[3] == 0));
        assert!(mac.rgba().chunks_exact(4).any(|pixel| pixel[3] == 255));
        assert!(windows
            .rgba()
            .chunks_exact(4)
            .any(|pixel| pixel[2] > pixel[0] && pixel[3] > 0));
    }

    #[test]
    fn repeat_exit_waits_until_worker_has_joined() {
        let phase = AtomicU8::new(0);
        assert_eq!(decide_exit(&phase), ExitDecision::DrainWorker);
        assert_eq!(decide_exit(&phase), ExitDecision::WaitForWorker);
        phase.store(2, Ordering::Release);
        assert_eq!(decide_exit(&phase), ExitDecision::Exit);
    }
}
