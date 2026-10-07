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
    updates: Vec<Update>,
    status: UpdateStatus,
}

impl Default for PendingUpdate {
    fn default() -> Self {
        Self {
            updates: Vec::new(),
            status: UpdateStatus {
                phase: "idle",
                ..Default::default()
            },
        }
    }
}

#[derive(Default)]
pub struct UpdateState(Mutex<PendingUpdate>);

fn latest_version<'a>(versions: impl Iterator<Item = &'a str>) -> Option<semver::Version> {
    // Update 的版本已由官方插件解析过。
    versions
        .map(|version| semver::Version::parse(version).unwrap())
        .max()
}

fn updater_builder(app: &tauri::AppHandle) -> tauri_plugin_updater::UpdaterBuilder {
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
    builder
}

async fn check_sources(app: &tauri::AppHandle) -> Result<Vec<Update>, String> {
    let config: tauri_plugin_updater::Config = serde_json::from_value(
        app.config()
            .plugins
            .0
            .get("updater")
            .cloned()
            .unwrap_or_default(),
    )
    .map_err(|error| error.to_string())?;
    let mut checks = tokio::task::JoinSet::new();
    for (priority, endpoint) in config.endpoints.into_iter().enumerate() {
        let app = app.clone();
        checks.spawn(async move {
            let result = match updater_builder(&app)
                .endpoints(vec![endpoint])
                .and_then(|builder| builder.build())
            {
                Ok(updater) => updater.check().await,
                Err(error) => Err(error),
            };
            (priority, result)
        });
    }
    let mut releases = Vec::new();
    let mut checked = false;
    let mut errors = Vec::new();
    while let Some(result) = checks.join_next().await {
        match result {
            Ok((priority, Ok(update))) => {
                checked = true;
                if let Some(update) = update {
                    releases.push((priority, update));
                }
            }
            Ok((_, Err(error))) => errors.push(error.to_string()),
            Err(error) => errors.push(error.to_string()),
        }
    }
    if !checked || (releases.is_empty() && !errors.is_empty()) {
        return Err(format!("更新服务检查未完成：{}", errors.join("；")));
    }
    // 镜像可能滞后；两个来源独立检查后，保留最新版本的所有下载对象。
    let latest = latest_version(releases.iter().map(|(_, update)| update.version.as_str()));
    releases.retain(|(_, update)| Some(semver::Version::parse(&update.version).unwrap()) == latest);
    releases.sort_by_key(|(priority, _)| *priority);
    Ok(releases.into_iter().map(|(_, update)| update).collect())
}

#[tauri::command]
pub async fn check_for_updates(app: tauri::AppHandle) -> Result<UpdateStatus, String> {
    let state = app.state::<UpdateState>();
    let mut pending = state.0.try_lock().map_err(|_| "更新正在进行，请稍候。")?;
    pending.status.phase = "checking";
    pending.status.error = None;
    pending.status.emit(&app);
    let result = check_sources(&app).await;
    match result {
        Ok(updates) => {
            let update = updates.first();
            pending.status = UpdateStatus {
                phase: if update.is_some() {
                    "available"
                } else {
                    "idle"
                },
                version: update.map(|item| item.version.clone()),
                notes: update.and_then(|item| item.body.clone()),
                ..Default::default()
            };
            pending.updates = updates;
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
    if pending.updates.is_empty() {
        return Err("请先检查更新。".into());
    }
    pending.status.phase = "downloading";
    pending.status.downloaded = 0;
    pending.status.total = None;
    pending.status.error = None;
    pending.status.emit(&app);
    let mut downloaded = None;
    let mut download_error = String::new();
    for mut update in pending.updates.clone() {
        update.timeout = Some(Duration::from_secs(600));
        pending.status.downloaded = 0;
        pending.status.total = None;
        pending.status.emit(&app);
        let mut last_progress = Instant::now();
        let result = update
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
        match result {
            Ok(bytes) => {
                downloaded = Some((update, bytes));
                break;
            }
            Err(error) => download_error = error.to_string(),
        }
    }
    let result = match downloaded {
        Some((update, bytes)) => {
            pending.status.phase = "installing";
            pending.status.emit(&app);
            tauri::async_runtime::spawn_blocking(move || update.install(bytes))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())
        }
        None => Err(download_error),
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
    fn mirror_selection_uses_newest_semantic_version() {
        assert_eq!(
            latest_version(["0.1.2", "0.1.10"].into_iter())
                .unwrap()
                .to_string(),
            "0.1.10"
        );
        assert_eq!(
            latest_version(["0.1.10", "0.1.2"].into_iter())
                .unwrap()
                .to_string(),
            "0.1.10"
        );
        assert!(latest_version(std::iter::empty()).is_none());
    }

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
        assert!(pending.updates.is_empty());
        let status = serde_json::to_value(pending.status).unwrap();
        assert_eq!(status["phase"], "idle");
        assert_eq!(status["downloaded"], 0);
        assert!(status["version"].is_null());
        assert!(status["error"].is_null());
    }
}
