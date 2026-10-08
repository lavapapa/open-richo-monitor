use std::{
    error::Error as _,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;

use crate::desktop_backend::{self, DesktopBackend};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    phase: &'static str,
    version: Option<String>,
    notes: Option<String>,
    downloaded: u64,
    total: Option<u64>,
    error: Option<String>,
    auto_install: bool,
}

impl Default for UpdateStatus {
    fn default() -> Self {
        Self {
            phase: "idle",
            version: None,
            notes: None,
            downloaded: 0,
            total: None,
            error: None,
            auto_install: true,
        }
    }
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
    ready: Option<(Update, Vec<u8>)>,
    immediate: bool,
    automatic: bool,
    install_failure: Option<InstallFailure>,
}

#[derive(Clone, Deserialize, Serialize)]
struct InstallFailure {
    version: String,
    error: String,
}

impl PendingUpdate {
    fn automatic_allowed(&self) -> bool {
        self.status.auto_install
            && !self.install_failure.as_ref().is_some_and(|failure| {
                self.status.version.as_deref() == Some(failure.version.as_str())
            })
    }
}

impl Default for PendingUpdate {
    fn default() -> Self {
        Self {
            updates: Vec::new(),
            ready: None,
            immediate: false,
            automatic: false,
            install_failure: None,
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

fn require_checked_source(checked: bool, errors: &[String]) -> Result<(), String> {
    if checked {
        return Ok(());
    }
    Err(if errors.is_empty() {
        "未配置更新来源。".into()
    } else {
        errors.join("；")
    })
}

fn source_failure(endpoint: &url::Url, error: &tauri_plugin_updater::Error) -> String {
    let source = match endpoint.host_str() {
        Some("gitee.com") => "Gitee",
        Some("github.com") => "GitHub",
        Some(host) => host,
        None => "更新来源",
    };
    let reason = match error {
        tauri_plugin_updater::Error::Reqwest(error) if error.is_timeout() => {
            "连接超时，请检查系统代理或稍后重试。".into()
        }
        tauri_plugin_updater::Error::Reqwest(error) if error.is_connect() => {
            "连接失败，请检查网络与系统代理。".into()
        }
        tauri_plugin_updater::Error::Serialization(_) => "更新信息格式无效，请稍后重试。".into(),
        tauri_plugin_updater::Error::ReleaseNotFound => "更新来源暂不可用，请稍后重试。".into(),
        _ => error.to_string(),
    };
    format!("{source}：{reason}")
}

fn updater_builder(app: &tauri::AppHandle) -> tauri_plugin_updater::UpdaterBuilder {
    let builder = app.updater_builder().timeout(Duration::from_secs(30));
    #[cfg(target_os = "windows")]
    let builder = {
        let handle = app.clone();
        // Windows 安装器直接退出进程，跳过 RunEvent；先收尾监控和 Bun，保存平台会话。
        builder.on_before_exit(move || {
            let backend = handle.state::<Arc<DesktopBackend>>();
            if backend.exit_phase.load(Ordering::Acquire) == 0 {
                backend.exit_phase.store(1, Ordering::Release);
                tauri::async_runtime::block_on(crate::drain_worker(backend.inner()));
            }
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
                .endpoints(vec![endpoint.clone()])
                .and_then(|builder| builder.build())
            {
                Ok(updater) => updater.check().await,
                Err(error) => Err(error),
            };
            let result = result.map_err(|error| {
                let mut detail = error.to_string();
                let mut cause = error.source();
                while let Some(reason) = cause {
                    detail.push_str(&format!("：{reason}"));
                    cause = reason.source();
                }
                let backend = app.state::<Arc<DesktopBackend>>();
                desktop_backend::write_log(
                    &backend.data_dir,
                    &format!("更新来源 {endpoint}：{detail}"),
                );
                source_failure(&endpoint, &error)
            });
            (priority, result)
        });
    }
    let mut releases = collect_source_results(checks, Duration::from_secs(5)).await?;
    // 镜像可能滞后；在已取得的结果中保留最新版本的所有下载对象。
    let latest = latest_version(releases.iter().map(|(_, update)| update.version.as_str()));
    releases.retain(|(_, update)| Some(semver::Version::parse(&update.version).unwrap()) == latest);
    releases.sort_by_key(|(priority, _)| *priority);
    Ok(releases.into_iter().map(|(_, update)| update).collect())
}

async fn collect_source_results<T: 'static>(
    mut checks: tokio::task::JoinSet<(usize, Result<Option<T>, String>)>,
    window: Duration,
) -> Result<Vec<(usize, T)>, String> {
    let mut releases = Vec::new();
    let mut checked = false;
    let mut errors = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    let mut expired = false;
    while !checks.is_empty() {
        let result = tokio::select! {
            biased;
            result = checks.join_next() => result,
            _ = tokio::time::sleep_until(deadline), if !expired => {
                if checked { break; }
                expired = true;
                continue;
            }
        };
        let Some(result) = result else { break };
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
        if checked && tokio::time::Instant::now() >= deadline {
            break;
        }
    }
    // 截止时刻已经完成的来源仍参加比较；窗口后等待时以首份有效结果为准。
    if tokio::time::Instant::now() <= deadline {
        while let Some(result) = checks.try_join_next() {
            if let Ok((priority, Ok(Some(update)))) = result {
                releases.push((priority, update));
            }
        }
    }
    require_checked_source(checked, &errors)?;
    Ok(releases)
}

#[tauri::command]
pub async fn check_for_updates(app: tauri::AppHandle) -> Result<UpdateStatus, String> {
    let state = app.state::<UpdateState>();
    let mut pending = state.0.try_lock().map_err(|_| "更新正在进行，请稍候。")?;
    if ["waiting", "downloading", "installing"].contains(&pending.status.phase) {
        return Ok(pending.status.clone());
    }
    pending.status.phase = "checking";
    pending.status.error = None;
    pending.status.emit(&app);
    let result = check_sources(&app).await;
    match result {
        Ok(updates) => {
            let update = updates.first();
            let auto_install = pending.status.auto_install;
            pending.status = UpdateStatus {
                phase: if update.is_some() {
                    "available"
                } else {
                    "idle"
                },
                version: update.map(|item| item.version.clone()),
                notes: update.and_then(|item| item.body.clone()),
                auto_install,
                ..Default::default()
            };
            pending.updates = updates;
            pending.ready = None;
            if let Some(failure) = pending.install_failure.as_ref().filter(|failure| {
                pending.status.version.as_deref() == Some(failure.version.as_str())
            }) {
                pending.status.error = Some(failure.error.clone());
            }
        }
        Err(error) => pending.status.failed(format!("检查更新失败：{error}")),
    }
    pending.status.emit(&app);
    let status = pending.status.clone();
    let automatic =
        status.phase == "available" && pending.automatic_allowed() && status.error.is_none();
    drop(pending);
    if automatic {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = download_update(handle, false, true).await;
        });
    }
    Ok(status)
}

#[tauri::command]
pub async fn install_update(app: tauri::AppHandle, immediate: bool) -> Result<(), String> {
    download_update(app, immediate, false).await
}

async fn download_update(
    app: tauri::AppHandle,
    immediate: bool,
    automatic: bool,
) -> Result<(), String> {
    let state = app.state::<UpdateState>();
    let mut pending = state.0.try_lock().map_err(|_| "更新正在进行，请稍候。")?;
    if automatic && !pending.automatic_allowed() {
        return Ok(());
    }
    if pending.status.phase == "installing" {
        return Err("正在安装更新。".into());
    }
    if pending.updates.is_empty() {
        return Err("请先检查更新。".into());
    }
    pending.immediate = immediate;
    pending.automatic = automatic;
    if pending.ready.is_some() {
        pending.status.phase = "waiting";
        pending.status.emit(&app);
        return Ok(());
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
    if let Some(ready) = downloaded {
        pending.ready = Some(ready);
        pending.status.phase = "waiting";
        pending.status.emit(&app);
        return Ok(());
    }
    pending
        .status
        .failed(format!("下载更新失败：{download_error}"));
    pending.status.emit(&app);
    Err(download_error)
}

async fn install_when_ready(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<UpdateState>();
    let Ok(mut pending) = state.0.try_lock() else {
        return Ok(());
    };
    if pending.status.phase != "waiting" {
        return Ok(());
    }
    let backend = app.state::<Arc<DesktopBackend>>();
    let _operation = backend.update_operation.lock().await;
    backend.ensure_worker_alive()?;
    let Ok(prominent_operation) = backend.prominent_operation.try_lock() else {
        return Ok(());
    };
    if !pending.immediate && backend.prominent_alert.lock().unwrap().is_some() {
        return Ok(());
    }
    let _reservation = if pending.immediate {
        None
    } else {
        match backend
            .app
            .reserve_idle_update()
            .await
            .map_err(|e| e.to_string())?
        {
            Some(reservation) => Some(reservation),
            None => return Ok(()),
        }
    };
    let visible = app
        .get_webview_window("main")
        .map(|window| window.is_visible())
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or(false);
    // 收尾前记录安装意图；安装器异常退出后，旧版不会反复自动重试同一版本。
    let failure = InstallFailure {
        version: pending.status.version.clone().unwrap(),
        error: "上次安装更新未完成，该版本自动安装已暂停，请手动重试或从项目发布页下载安装包。"
            .into(),
    };
    // 与托盘退出共用退出所有权，避免两次收尾在安装过程中关闭进程。
    if crate::decide_exit(&backend.exit_phase) != crate::ExitDecision::DrainWorker {
        return Ok(());
    }
    let restart_path = backend.data_dir.join("update-restart.json");
    let prepare = (|| -> std::io::Result<()> {
        std::fs::write(&restart_path, serde_json::to_vec(&visible).unwrap())?;
        std::fs::write(
            backend.data_dir.join("update-install-failure.json"),
            serde_json::to_vec(&failure).unwrap(),
        )
    })();
    if let Err(error) = prepare {
        let _ = std::fs::remove_file(restart_path);
        backend.exit_phase.store(0, Ordering::Release);
        return Err(error.to_string());
    }
    pending.install_failure = Some(failure);
    pending.status.phase = "installing";
    pending.status.emit(app);
    drop(prominent_operation);
    crate::drain_worker(backend.inner()).await;
    let (update, bytes) = pending.ready.take().expect("等待安装已有更新包");
    let result = tauri::async_runtime::spawn_blocking(move || update.install(bytes))
        .await
        .map_err(|e| e.to_string())
        .and_then(|result| result.map_err(|e| e.to_string()));
    backend.exit_phase.store(2, Ordering::Release);
    match result {
        Ok(()) => {
            app.request_restart();
            Ok(())
        }
        Err(error) => {
            pending.status.failed(format!(
                "更新失败：{error}。该版本自动安装已暂停，请手动重试或从项目发布页下载安装包。"
            ));
            pending.status.emit(app);
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

#[tauri::command]
pub async fn set_auto_install_updates(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<UpdateStatus, String> {
    let state = app.state::<UpdateState>();
    let mut pending = state.0.lock().await;
    let backend = app.state::<Arc<DesktopBackend>>();
    std::fs::write(
        backend.data_dir.join("auto-install-updates.json"),
        serde_json::to_vec(&enabled).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    pending.status.auto_install = enabled;
    if !enabled && pending.status.phase == "waiting" && pending.automatic {
        pending.status.phase = "available";
    }
    let status = pending.status.clone();
    status.emit(&app);
    let automatic = enabled && status.phase == "available" && pending.automatic_allowed();
    drop(pending);
    if automatic {
        tauri::async_runtime::spawn(async move {
            let _ = download_update(app, false, true).await;
        });
    }
    Ok(status)
}

pub fn load_preferences(app: &tauri::AppHandle) {
    let backend = app.state::<Arc<DesktopBackend>>();
    let state = app.state::<UpdateState>();
    let mut pending = state.0.try_lock().expect("启动时更新状态尚未被使用");
    if let Ok(bytes) = std::fs::read(backend.data_dir.join("update-install-failure.json")) {
        match serde_json::from_slice::<InstallFailure>(&bytes) {
            Ok(failure) => pending.install_failure = Some(failure),
            Err(error) => desktop_backend::write_log(
                &backend.data_dir,
                &format!("读取更新失败记录失败：{error}"),
            ),
        }
    }
    match std::fs::read(backend.data_dir.join("auto-install-updates.json")) {
        Ok(bytes) => match serde_json::from_slice::<bool>(&bytes) {
            Ok(enabled) => pending.status.auto_install = enabled,
            Err(error) => {
                pending.status.auto_install = false;
                desktop_backend::write_log(
                    &backend.data_dir,
                    &format!("读取更新偏好失败：{error}"),
                );
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            pending.status.auto_install = false;
            desktop_backend::write_log(&backend.data_dir, &format!("读取更新偏好失败：{error}"));
        }
    }
}

pub fn start_hourly_checks(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut next_check = tokio::time::Instant::now();
        loop {
            interval.tick().await;
            let backend = app.state::<Arc<DesktopBackend>>();
            if backend.exit_phase.load(Ordering::Acquire) != 0 {
                break;
            }
            if tokio::time::Instant::now() >= next_check {
                let _ = check_for_updates(app.clone()).await;
                next_check = tokio::time::Instant::now() + Duration::from_secs(3600);
            }
            if let Err(error) = install_when_ready(&app).await {
                let state = app.state::<UpdateState>();
                let mut pending = state.0.lock().await;
                pending.status.failed(&error);
                pending.status.emit(&app);
                desktop_backend::write_log(&backend.data_dir, &format!("安装更新失败：{error}"));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_install_respects_disabled_preference_and_does_not_repeat_failed_version() {
        let mut pending = PendingUpdate::default();
        pending.status.version = Some("0.2.0".into());
        assert!(pending.automatic_allowed());
        pending.status.auto_install = false;
        assert!(!pending.automatic_allowed());
        pending.status.auto_install = true;
        pending.install_failure = Some(InstallFailure {
            version: "0.2.0".into(),
            error: "只读".into(),
        });
        assert!(!pending.automatic_allowed());
        pending.status.version = Some("0.2.1".into());
        assert!(pending.automatic_allowed());
    }

    fn sources(
        items: [(u64, Result<Option<&'static str>, &'static str>); 2],
    ) -> tokio::task::JoinSet<(usize, Result<Option<&'static str>, String>)> {
        let mut checks = tokio::task::JoinSet::new();
        for (priority, (seconds, result)) in items.into_iter().enumerate() {
            checks.spawn(async move {
                tokio::time::sleep(Duration::from_secs(seconds)).await;
                (priority, result.map_err(str::to_owned))
            });
        }
        checks
    }

    #[tokio::test(start_paused = true)]
    async fn update_window_compares_both_early_results() {
        let start = tokio::time::Instant::now();
        let results = collect_source_results(
            sources([(1, Ok(Some("0.1.4"))), (3, Ok(Some("0.1.5")))]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(3));
        assert_eq!(
            latest_version(results.iter().map(|(_, version)| *version))
                .unwrap()
                .to_string(),
            "0.1.5"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn update_window_compares_results_ready_at_deadline() {
        let results = collect_source_results(
            sources([(5, Ok(Some("0.1.4"))), (5, Ok(Some("0.1.5")))]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            latest_version(results.iter().map(|(_, version)| *version))
                .unwrap()
                .to_string(),
            "0.1.5"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn update_window_returns_at_five_seconds_with_one_source() {
        let start = tokio::time::Instant::now();
        let results = collect_source_results(
            sources([(1, Ok(Some("0.1.4"))), (30, Ok(Some("0.1.5")))]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        assert_eq!(results, [(0, "0.1.4")]);
    }

    #[tokio::test(start_paused = true)]
    async fn update_window_waits_for_first_valid_result_after_deadline() {
        let start = tokio::time::Instant::now();
        let results = collect_source_results(
            sources([(10, Ok(Some("0.1.4"))), (8, Ok(Some("0.1.5")))]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(8));
        assert_eq!(results, [(1, "0.1.5")]);
    }

    #[tokio::test(start_paused = true)]
    async fn update_window_uses_only_first_result_when_both_are_late() {
        let results = collect_source_results(
            sources([(8, Ok(Some("0.1.4"))), (8, Ok(Some("0.1.5")))]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn update_window_errors_are_not_valid_release_information() {
        let start = tokio::time::Instant::now();
        let results = collect_source_results(
            sources([(1, Err("Gitee：连接失败")), (8, Ok(Some("0.1.5")))]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(8));
        assert_eq!(results, [(1, "0.1.5")]);
    }

    #[tokio::test(start_paused = true)]
    async fn update_window_no_update_is_a_valid_result() {
        let start = tokio::time::Instant::now();
        let results = collect_source_results(
            sources([(1, Ok(None)), (30, Ok(Some("0.1.5")))]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(5));
        assert!(results.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn update_window_all_failed_reports_both_sources() {
        let start = tokio::time::Instant::now();
        let error = collect_source_results(
            sources([(1, Err("Gitee：连接失败")), (2, Err("GitHub：连接超时"))]),
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert_eq!(start.elapsed(), Duration::from_secs(2));
        assert!(error.contains("Gitee") && error.contains("GitHub"));
    }

    #[tokio::test(start_paused = true)]
    async fn update_window_cancels_unused_slow_source() {
        let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut checks = tokio::task::JoinSet::new();
        checks.spawn(async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            (0, Ok::<_, String>(Some("0.1.4")))
        });
        let slow = completed.clone();
        checks.spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            slow.store(true, Ordering::Relaxed);
            (1, Ok::<_, String>(Some("0.1.5")))
        });
        collect_source_results(checks, Duration::from_secs(5))
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert!(!completed.load(Ordering::Relaxed));
    }

    #[test]
    fn successful_source_without_update_is_not_failed_by_an_unavailable_mirror() {
        assert!(require_checked_source(true, &["GitHub：连接超时".into()]).is_ok());
        assert!(require_checked_source(true, &["Gitee：连接超时".into()]).is_ok());
        assert!(require_checked_source(true, &[]).is_ok());
        assert!(require_checked_source(
            false,
            &["Gitee：连接超时".into(), "GitHub：连接失败".into()]
        )
        .is_err());
        assert_eq!(
            require_checked_source(false, &[]).unwrap_err(),
            "未配置更新来源。"
        );
    }

    #[test]
    fn failed_sources_are_named_and_explain_the_next_action() {
        let error = tauri_plugin_updater::Error::ReleaseNotFound;
        for (url, source) in [
            ("https://gitee.com/example/latest.json", "Gitee"),
            ("https://github.com/example/latest.json", "GitHub"),
        ] {
            let message = source_failure(&url.parse().unwrap(), &error);
            assert!(message.starts_with(source));
            assert!(message.contains("稍后重试"));
        }
    }

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
