use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::{Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;

use crate::desktop_backend::{self, DesktopBackend};

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    phase: &'static str,
    version: Option<String>,
    notes: Option<String>,
    downloaded: u64,
    total: Option<u64>,
    error: Option<String>,
}

impl UpdateStatus {
    fn emit(&self, app: &tauri::AppHandle) {
        let _ = app.emit("desktop-update", self);
    }

    fn failed(&mut self, error: impl std::fmt::Display) {
        self.phase = if self.version.is_some() {
            "available"
        } else {
            "idle"
        };
        self.error = Some(error.to_string());
    }
}

struct PendingUpdate {
    update: Option<Update>,
    status: UpdateStatus,
}

impl Default for PendingUpdate {
    fn default() -> Self {
        Self {
            update: None,
            status: UpdateStatus {
                phase: "idle",
                ..Default::default()
            },
        }
    }
}

#[derive(Default)]
pub struct UpdateState(Mutex<PendingUpdate>);

#[tauri::command]
pub async fn check_for_updates(app: tauri::AppHandle) -> Result<UpdateStatus, String> {
    let state = app.state::<UpdateState>();
    let mut pending = state.0.try_lock().map_err(|_| "更新正在进行，请稍候。")?;
    pending.status.phase = "checking";
    pending.status.error = None;
    pending.status.emit(&app);
    let builder = app.updater_builder().timeout(Duration::from_secs(30));
    #[cfg(target_os = "windows")]
    let builder = {
        let handle = app.clone();
        // Windows 安装器直接退出进程，跳过 RunEvent；先收尾监控和 Bun，保存平台会话。
        builder.on_before_exit(move || {
            let backend = handle.state::<Arc<DesktopBackend>>();
            backend.exit_phase.store(1, Ordering::Release);
            tauri::async_runtime::block_on(crate::drain_worker(backend.inner()));
            handle.cleanup_before_exit();
        })
    };
    let result = match builder.build() {
        Ok(updater) => updater.check().await,
        Err(error) => Err(error),
    };
    match result {
        Ok(update) => {
            pending.status = UpdateStatus {
                phase: if update.is_some() {
                    "available"
                } else {
                    "idle"
                },
                version: update.as_ref().map(|item| item.version.clone()),
                notes: update.as_ref().and_then(|item| item.body.clone()),
                ..Default::default()
            };
            pending.update = update;
        }
        Err(error) => pending.status.failed(format!("检查更新失败：{error}")),
    }
    pending.status.emit(&app);
    Ok(pending.status.clone())
}

#[tauri::command]
pub async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<UpdateState>();
    let mut pending = state.0.try_lock().map_err(|_| "更新正在进行，请稍候。")?;
    let mut update = pending.update.clone().ok_or("请先检查更新。")?;
    update.timeout = Some(Duration::from_secs(600));
    pending.status.phase = "downloading";
    pending.status.downloaded = 0;
    pending.status.total = None;
    pending.status.error = None;
    pending.status.emit(&app);
    let mut last_progress = Instant::now();
    let downloaded = update
        .download(
            |bytes, total| {
                pending.status.downloaded += bytes as u64;
                pending.status.total = total;
                if last_progress.elapsed() >= Duration::from_millis(100) {
                    pending.status.emit(&app);
                    last_progress = Instant::now();
                }
            },
            || {},
        )
        .await;
    let result = match downloaded {
        Ok(bytes) => {
            pending.status.phase = "installing";
            pending.status.emit(&app);
            tauri::async_runtime::spawn_blocking(move || update.install(bytes))
                .await
                .map_err(|error| error.to_string())?
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => {
            app.request_restart();
            Ok(())
        }
        Err(error) => {
            pending.status.failed(format!(
                "更新失败：{error}。请重试或从项目发布页下载安装包。"
            ));
            pending.status.emit(&app);
            let backend = app.state::<Arc<DesktopBackend>>();
            desktop_backend::write_log(&backend.data_dir, pending.status.error.as_deref().unwrap());
            // 安装器启动失败时，Windows 已收尾；重启原版本恢复运行。
            if backend.exit_phase.load(Ordering::Acquire) != 0 {
                app.request_restart();
            }
            Err(pending.status.error.clone().unwrap())
        }
    }
}

pub fn start_hourly_checks(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            let backend = app.state::<Arc<DesktopBackend>>();
            if backend.exit_phase.load(Ordering::Acquire) != 0 {
                break;
            }
            let _ = check_for_updates(app.clone()).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_check_preserves_available_update_for_retry() {
        let mut status = UpdateStatus {
            phase: "checking",
            version: Some("0.2.0".into()),
            ..Default::default()
        };
        status.failed("断网");
        assert_eq!(status.phase, "available");
        assert_eq!(status.version.as_deref(), Some("0.2.0"));
        assert_eq!(status.error.as_deref(), Some("断网"));
        status.version = None;
        status.failed("超时");
        assert_eq!(status.phase, "idle");
    }

    #[test]
    fn pending_state_starts_idle_and_serializes_ui_contract() {
        let pending = PendingUpdate::default();
        assert!(pending.update.is_none());
        let status = serde_json::to_value(pending.status).unwrap();
        assert_eq!(status["phase"], "idle");
        assert_eq!(status["downloaded"], 0);
        assert!(status["version"].is_null());
        assert!(status["error"].is_null());
    }
}
