use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Mutex, RwLock,
};

use crate::desktop_notifications;
use ricoh_monitor_core::app::{
    AppConfig, AppSnapshot, ChannelInput, ChannelTest, ChannelView, MessagePage, MessageQuery,
    MonitorApp, MonitoringAction, ProductRecord, ProductScan, ProxyImportResult, ProxyInput,
    ProxyRecord, ScanAction,
};
use ricoh_monitor_core::notifications::{ChannelBinding, NotificationTarget};
use serde::Serialize;
use tauri::{plugin::PermissionState, AppHandle, State};
use tauri_plugin_dialog::DialogExt;

pub(crate) struct DesktopBackend {
    pub app: MonitorApp,
    pub data_dir: PathBuf,
    notification_permission: RwLock<Result<PermissionState, String>>,
    pub worker_alive: AtomicBool,
    pub worker_task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    pub exit_phase: AtomicU8,
    pub prominent_alert: Mutex<Option<ricoh_monitor_core::app::ProminentAlert>>,
    pub prominent_operation: tokio::sync::Mutex<()>,
    pub prominent_view: crate::prominent_alert::ProminentView,
}

impl DesktopBackend {
    pub fn open(data_dir: impl AsRef<Path>, permission: PermissionState) -> Result<Self, String> {
        let data_dir = data_dir.as_ref();
        let app = MonitorApp::open(data_dir).map_err(|error| error.to_string())?;
        app.apply_startup_monitoring()
            .map_err(|error| error.to_string())?;
        app.set_platform_notifications_available(permission == PermissionState::Granted);
        Ok(Self {
            app,
            data_dir: data_dir.to_path_buf(),
            notification_permission: RwLock::new(Ok(permission)),
            worker_alive: AtomicBool::new(true),
            worker_task: Mutex::new(None),
            exit_phase: AtomicU8::new(0),
            prominent_alert: Mutex::new(None),
            prominent_operation: tokio::sync::Mutex::new(()),
            prominent_view: crate::prominent_alert::ProminentView::default(),
        })
    }

    pub fn notification_permission(&self) -> Result<PermissionState, String> {
        self.notification_permission.read().unwrap().clone()
    }

    pub(crate) async fn set_notification_permission(
        &self,
        permission: Result<PermissionState, String>,
    ) -> Result<(), String> {
        {
            let mut current = self.notification_permission.write().unwrap();
            if *current == permission {
                return Ok(());
            }
            self.app
                .set_platform_notifications_available(permission == Ok(PermissionState::Granted));
            if let Err(error) = &permission {
                write_log(&self.data_dir, error);
            }
            *current = permission;
        }
        self.app
            .snapshot()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn ensure_worker_alive(&self) -> Result<(), String> {
        if self.worker_alive.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err("监控任务已停止。请重新打开应用后再试。".into())
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<AppSnapshot> {
        self.app.subscribe()
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSnapshot {
    #[serde(flatten)]
    core: AppSnapshot,
    platform: PlatformSnapshot,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformSnapshot {
    login_start_enabled: Option<bool>,
    notification_permission: &'static str,
    notification_permission_error: Option<String>,
    notification_settings_available: bool,
    project_url: Option<String>,
    tutorial_url: Option<String>,
    feedback_url: Option<String>,
}

impl DesktopSnapshot {
    pub(crate) fn new(core: AppSnapshot, permission: Result<PermissionState, String>) -> Self {
        Self {
            core,
            platform: PlatformSnapshot::current(permission),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationResult {
    message: Option<String>,
}

impl OperationResult {
    pub(crate) fn ok() -> Self {
        Self { message: None }
    }
}

pub(crate) fn write_log(data_dir: &Path, message: &str) {
    static LOG_WRITE_LOCK: Mutex<()> = Mutex::new(());
    let _write = LOG_WRITE_LOCK.lock().unwrap();
    let directory = data_dir.join("logs");
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    let path = directory.join("monitor.log");
    const LOG_LIMIT: u64 = 1_048_576;
    const ENTRY_LIMIT: usize = 65_536;
    let record = format!("{} {message}", now_ms());
    let mut end = record.len().min(ENTRY_LIMIT);
    while !record.is_char_boundary(end) {
        end -= 1;
    }
    let suffix = if end < record.len() {
        "… [日志条目已截短]"
    } else {
        ""
    };
    let record = format!("{}{}\n", &record[..end], suffix);
    if std::fs::metadata(&path)
        .is_ok_and(|metadata| metadata.len().saturating_add(record.len() as u64) > LOG_LIMIT)
    {
        if std::fs::rename(&path, directory.join("monitor.previous.log")).is_err() {
            return;
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = file.write_all(record.as_bytes());
    }
}

#[tauri::command]
pub async fn get_desktop_snapshot(
    handle: AppHandle,
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
) -> Result<DesktopSnapshot, String> {
    let permission = desktop_notifications::permission_state(&handle).await;
    backend.set_notification_permission(permission).await?;
    backend
        .app
        .snapshot()
        .await
        .map(|snapshot| DesktopSnapshot::new(snapshot, backend.notification_permission()))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn refresh_catalog_metadata(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
) -> Result<OperationResult, String> {
    backend.app.refresh_catalog_metadata();
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn query_messages(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    query: MessageQuery,
) -> Result<MessagePage, String> {
    backend
        .app
        .query_messages(query)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn refresh_notification_permission(
    app: AppHandle,
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
) -> Result<PlatformSnapshot, String> {
    let permission = desktop_notifications::permission_state(&app).await;
    backend
        .set_notification_permission(permission.clone())
        .await?;
    permission?;
    Ok(PlatformSnapshot::current(backend.notification_permission()))
}

#[tauri::command]
pub async fn request_notification_permission(
    app: AppHandle,
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
) -> Result<PlatformSnapshot, String> {
    let permission = desktop_notifications::request_permission(&app).await?;
    backend.set_notification_permission(Ok(permission)).await?;
    Ok(PlatformSnapshot::current(backend.notification_permission()))
}

fn needs_notification_request(permission: PermissionState) -> bool {
    matches!(
        permission,
        PermissionState::Prompt | PermissionState::PromptWithRationale
    )
}

#[tauri::command]
pub async fn test_system_notification(
    app: AppHandle,
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
) -> Result<&'static str, String> {
    let reading = desktop_notifications::permission_state(&app).await;
    backend.set_notification_permission(reading.clone()).await?;
    let mut permission = reading?;
    if needs_notification_request(permission) {
        permission = desktop_notifications::request_permission(&app).await?;
        backend.set_notification_permission(Ok(permission)).await?;
    }
    if permission != PermissionState::Granted {
        return Ok("permission_denied");
    }
    desktop_notifications::show_test(&app).await?;
    Ok("accepted_by_system")
}

#[tauri::command]
pub async fn set_system_notifications_enabled(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    enabled: bool,
) -> Result<OperationResult, String> {
    backend
        .app
        .set_system_notifications_enabled(enabled)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn set_auto_start_monitoring(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    enabled: bool,
) -> Result<OperationResult, String> {
    backend
        .app
        .set_auto_start_monitoring(enabled)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn save_monitoring_config(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    config: AppConfig,
) -> Result<OperationResult, String> {
    backend
        .app
        .save_config(config)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn monitoring_action(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    action: MonitoringAction,
) -> Result<OperationResult, String> {
    backend.ensure_worker_alive()?;
    backend
        .app
        .monitoring_action(action)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn validate_product(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    product_id: String,
) -> Result<ProductRecord, String> {
    let product_id = parse_product_id(&product_id)?;
    backend
        .app
        .validate_product(product_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn add_product(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    product_id: String,
) -> Result<ProductRecord, String> {
    let product_id = parse_product_id(&product_id)?;
    backend
        .app
        .add_product(product_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_product_enabled(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    product_id: String,
    enabled: bool,
) -> Result<OperationResult, String> {
    let product_id = parse_product_id(&product_id)?;
    backend
        .app
        .set_product_enabled(product_id, enabled)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn remove_product(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    product_id: String,
) -> Result<OperationResult, String> {
    let product_id = parse_product_id(&product_id)?;
    backend
        .app
        .remove_product(product_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn start_product_scan(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    start_id: String,
    end_id: String,
) -> Result<ProductScan, String> {
    let start_id = parse_product_id(&start_id)?;
    let end_id = parse_product_id(&end_id)?;
    backend
        .app
        .start_product_scan(start_id, end_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn control_product_scan(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    action: ScanAction,
) -> Result<ProductScan, String> {
    backend
        .app
        .control_product_scan(action)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn begin_channel_binding(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    provider_id: String,
) -> Result<ChannelBinding, String> {
    backend.ensure_worker_alive()?;
    backend
        .app
        .set_notification_proxy_url(crate::notification_proxy::current());
    backend
        .app
        .begin_channel_binding(&provider_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn begin_channel_rebinding(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    channel_id: String,
) -> Result<ChannelBinding, String> {
    backend.ensure_worker_alive()?;
    backend
        .app
        .set_notification_proxy_url(crate::notification_proxy::current());
    backend
        .app
        .begin_channel_rebinding(&channel_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn begin_channel_editing(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    channel_id: String,
) -> Result<OperationResult, String> {
    backend.ensure_worker_alive()?;
    backend
        .app
        .set_notification_proxy_url(crate::notification_proxy::current());
    backend
        .app
        .begin_channel_editing(&channel_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn end_channel_editing(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    channel_id: String,
) -> Result<OperationResult, String> {
    backend
        .app
        .end_channel_editing(&channel_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn detect_notification_channel_groups(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    channel_id: String,
) -> Result<Vec<NotificationTarget>, String> {
    backend.ensure_worker_alive()?;
    backend
        .app
        .set_notification_proxy_url(crate::notification_proxy::current());
    backend
        .app
        .detect_channel_groups(&channel_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn detect_binding_groups(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    binding_id: String,
) -> Result<Vec<NotificationTarget>, String> {
    backend
        .app
        .detect_binding_groups(&binding_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn channel_binding_status(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    binding_id: String,
) -> Result<ChannelBinding, String> {
    backend
        .app
        .channel_binding_status(&binding_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn cancel_channel_binding(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    binding_id: String,
) -> Result<OperationResult, String> {
    backend
        .app
        .cancel_channel_binding(&binding_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn submit_channel_binding_verification(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    binding_id: String,
    code: String,
) -> Result<ChannelBinding, String> {
    backend
        .app
        .submit_channel_binding_verification(&binding_id, &code)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn save_notification_channel(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    channel: ChannelInput,
) -> Result<ChannelView, String> {
    backend
        .app
        .save_channel(channel)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn test_notification_channel(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    channel_id: String,
) -> Result<ChannelTest, String> {
    backend
        .app
        .set_notification_proxy_url(crate::notification_proxy::current());
    backend
        .app
        .test_channel(&channel_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_notification_channel_enabled(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    channel_id: String,
    enabled: bool,
) -> Result<OperationResult, String> {
    backend
        .app
        .set_channel_enabled(&channel_id, enabled)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn remove_notification_channel(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    channel_id: String,
) -> Result<OperationResult, String> {
    backend
        .app
        .remove_channel(&channel_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn add_proxy(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    proxy: ProxyInput,
) -> Result<ProxyRecord, String> {
    backend
        .app
        .add_proxy(proxy)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn import_proxies(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    entries: String,
) -> Result<ProxyImportResult, String> {
    backend
        .app
        .import_proxies(&entries)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn test_proxy(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    proxy_id: String,
) -> Result<ProxyRecord, String> {
    backend
        .app
        .test_proxy(&proxy_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_proxy_enabled(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    proxy_id: String,
    enabled: bool,
) -> Result<OperationResult, String> {
    backend
        .app
        .set_proxy_enabled(&proxy_id, enabled)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn remove_proxy(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    proxy_id: String,
) -> Result<OperationResult, String> {
    backend
        .app
        .remove_proxy(&proxy_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub fn set_login_start(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    enabled: bool,
) -> Result<PlatformSnapshot, String> {
    set_login_start_enabled(enabled)?;
    Ok(PlatformSnapshot::current(backend.notification_permission()))
}

#[tauri::command]
pub fn open_logs_directory(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
) -> Result<OperationResult, String> {
    let path = backend.data_dir.join("logs");
    std::fs::create_dir_all(&path).map_err(|error| format!("无法创建日志目录：{error}"))?;
    open_path(&path)?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn create_diagnostic_preview(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
) -> Result<String, String> {
    let snapshot = backend
        .app
        .snapshot()
        .await
        .map_err(|error| error.to_string())?;
    let mut preview = serde_json::to_value(snapshot).map_err(|error| error.to_string())?;
    redact_channel_messages(&mut preview);
    serde_json::to_string_pretty(&preview).map_err(|error| error.to_string())
}

fn redact_channel_messages(snapshot: &mut serde_json::Value) {
    let Some(channels) = snapshot
        .get_mut("channels")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for channel in channels {
        for key in ["lastTest", "lastDelivery"] {
            let Some(result) = channel
                .get_mut(key)
                .and_then(serde_json::Value::as_object_mut)
            else {
                continue;
            };
            let Some(outcome) = result.get("outcome").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let message = match outcome {
                "accepted" => "平台已接受通知，不代表用户已读。",
                "failed" => "操作失败，详细错误已省略。",
                "unknown" => "操作结果未知，详细错误已省略。",
                "invalid" => "配置无效，详细错误已省略。",
                "not_configured" => "尚未配置通知渠道凭据。",
                _ => "操作结果未知，详细错误已省略。",
            };
            if let Some(value) = result.get_mut("message") {
                *value = serde_json::Value::String(message.into());
            }
        }
    }
}

#[tauri::command]
pub async fn export_configuration_file(
    handle: AppHandle,
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
) -> Result<Option<String>, String> {
    let contents = backend
        .app
        .export_config()
        .await
        .map_err(|error| error.to_string())?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    handle
        .dialog()
        .file()
        .set_file_name("ricoh-monitor-config.json")
        .add_filter("JSON", &["json"])
        .save_file(move |path| {
            let _ = sender.send(path);
        });
    let Some(path) = receiver.await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let path = path.into_path().map_err(|error| error.to_string())?;
    std::fs::write(&path, contents).map_err(|error| format!("无法保存配置文件：{error}"))?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn import_configuration_file(
    handle: AppHandle,
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
) -> Result<Option<String>, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    handle
        .dialog()
        .file()
        .add_filter("JSON", &["json"])
        .pick_file(move |path| {
            let _ = sender.send(path);
        });
    let Some(path) = receiver.await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let path = path.into_path().map_err(|error| error.to_string())?;
    if std::fs::metadata(&path)
        .map_err(|error| format!("无法读取配置文件：{error}"))?
        .len()
        > ricoh_monitor_core::app::MAX_CONFIG_IMPORT_BYTES as u64
    {
        return Err("配置文件超过 4 MiB，请减少商品或渠道后再导入".into());
    }
    let contents =
        std::fs::read_to_string(&path).map_err(|error| format!("无法读取配置文件：{error}"))?;
    backend
        .app
        .import_config(&contents)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn restore_defaults(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    clear_history: bool,
) -> Result<OperationResult, String> {
    backend
        .app
        .restore_defaults(clear_history)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn complete_setup(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
) -> Result<OperationResult, String> {
    backend
        .app
        .complete_setup()
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn set_onboarding_products(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    product_ids: Vec<String>,
) -> Result<OperationResult, String> {
    let ids = product_ids
        .iter()
        .map(|id| parse_product_id(id))
        .collect::<Result<Vec<_>, _>>()?;
    backend
        .app
        .set_onboarding_products(&ids)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub async fn set_product_prominent_alert(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    product_id: String,
    enabled: bool,
) -> Result<OperationResult, String> {
    backend
        .app
        .set_product_prominent_alert(parse_product_id(&product_id)?, enabled)
        .await
        .map_err(|error| error.to_string())?;
    Ok(OperationResult::ok())
}

#[tauri::command]
pub fn save_diagnostic_report(
    backend: State<'_, std::sync::Arc<DesktopBackend>>,
    contents: String,
) -> Result<DiagnosticPath, String> {
    let directory = backend.data_dir.join("diagnostics");
    std::fs::create_dir_all(&directory).map_err(|error| format!("无法创建诊断目录：{error}"))?;
    let path = directory.join(format!("diagnostic-{}.json", now_ms()));
    std::fs::write(&path, contents).map_err(|error| format!("无法保存诊断报告：{error}"))?;
    Ok(DiagnosticPath {
        path: path.to_string_lossy().into_owned(),
    })
}

#[derive(Serialize)]
pub struct DiagnosticPath {
    path: String,
}

impl PlatformSnapshot {
    fn current(permission: Result<PermissionState, String>) -> Self {
        Self {
            login_start_enabled: login_start_enabled(),
            notification_permission: match &permission {
                Ok(PermissionState::Granted) => "granted",
                Ok(PermissionState::Denied) => "denied",
                Ok(PermissionState::Prompt) => "prompt",
                Ok(PermissionState::PromptWithRationale) => "prompt_with_rationale",
                Err(_) => "unavailable",
            },
            notification_settings_available: cfg!(target_os = "windows"),
            notification_permission_error: permission.err(),
            project_url: Some("https://github.com/lavapapa/open-richo-monitor".into()),
            tutorial_url: Some("https://github.com/lavapapa/open-richo-monitor#一桌面使用".into()),
            feedback_url: Some("https://github.com/lavapapa/open-richo-monitor/issues".into()),
        }
    }
}

#[cfg(target_os = "macos")]
fn login_agent_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("无法确定用户主目录。")?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join("com.ricoh.monitor.plist"))
}

#[cfg(target_os = "linux")]
fn login_agent_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("无法确定用户主目录。")?;
    Ok(PathBuf::from(home)
        .join(".config/autostart")
        .join("ricoh-monitor.desktop"))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn login_start_enabled() -> Option<bool> {
    login_agent_path().ok().map(|path| path.exists())
}

#[cfg(target_os = "windows")]
const LOGIN_START_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const LOGIN_START_NAME: &str = "open-richo-monitor";

#[cfg(target_os = "windows")]
fn login_start_enabled() -> Option<bool> {
    match windows_registry::CURRENT_USER
        .open(LOGIN_START_KEY)
        .and_then(|key| key.get_string(LOGIN_START_NAME))
    {
        Ok(command) => Some(!command.is_empty()),
        // Windows 的 ERROR_FILE_NOT_FOUND 表示尚未创建登录启动项。
        Err(error) if error.code().0 == 0x80070002u32 as i32 => Some(false),
        Err(_) => None,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn login_start_enabled() -> Option<bool> {
    None
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn set_login_start_enabled(enabled: bool) -> Result<(), String> {
    let path = login_agent_path()?;
    if enabled {
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let contents = login_start_contents(&executable);
        let parent = path.parent().ok_or("无法确定登录启动目录。")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::write(&path, contents).map_err(|error| error.to_string())?;
        #[cfg(target_os = "macos")]
        {
            let domain = launchd_domain()?;
            let _ = std::process::Command::new("launchctl")
                .args(["bootout", &domain, path.to_string_lossy().as_ref()])
                .status();
            let status = std::process::Command::new("launchctl")
                .args(["bootstrap", &domain, path.to_string_lossy().as_ref()])
                .status()
                .map_err(|error| format!("无法启用登录启动：{error}"))?;
            if !status.success() {
                return Err("系统未能启用登录启动。".into());
            }
        }
    } else if path.exists() {
        #[cfg(target_os = "macos")]
        {
            let domain = launchd_domain()?;
            let _ = std::process::Command::new("launchctl")
                .args(["bootout", &domain, path.to_string_lossy().as_ref()])
                .status();
        }
        std::fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn launchd_domain() -> Result<String, String> {
    let output = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|error| format!("无法确定当前用户：{error}"))?;
    if !output.status.success() {
        return Err("无法确定当前用户。".into());
    }
    let uid = String::from_utf8(output.stdout).map_err(|_| "当前用户信息无效。")?;
    Ok(format!("gui/{}", uid.trim()))
}

#[cfg(target_os = "macos")]
fn login_start_contents(executable: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>com.ricoh.monitor</string><key>ProgramArguments</key><array><string>{}</string></array><key>RunAtLoad</key><true/></dict></plist>\n",
        xml_escape(&executable.to_string_lossy())
    )
}

#[cfg(target_os = "linux")]
fn login_start_contents(executable: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=open-richo-monitor\nExec=\"{}\"\nX-GNOME-Autostart-enabled=true\n",
        desktop_exec_escape(&executable.to_string_lossy())
    )
}

#[cfg(target_os = "linux")]
fn desktop_exec_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "windows")]
fn login_start_contents(executable: &Path) -> String {
    format!("\"{}\"", executable.to_string_lossy())
}

#[cfg(target_os = "windows")]
fn set_login_start_enabled(enabled: bool) -> Result<(), String> {
    let key = windows_registry::CURRENT_USER
        .create(LOGIN_START_KEY)
        .map_err(|error| format!("无法打开登录启动设置：{error}"))?;
    let result = if enabled {
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        key.set_string(LOGIN_START_NAME, login_start_contents(&executable))
    } else {
        key.remove_value(LOGIN_START_NAME).or_else(|error| {
            if error.code().0 == 0x80070002u32 as i32 {
                Ok(())
            } else {
                Err(error)
            }
        })
    };
    result.map_err(|error| format!("无法保存登录启动设置：{error}"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn set_login_start_enabled(_enabled: bool) -> Result<(), String> {
    Err("当前系统暂不支持登录后启动。".into())
}

#[cfg(target_os = "macos")]
fn open_path(path: &Path) -> Result<(), String> {
    open_with("open", path)
}

#[cfg(target_os = "linux")]
fn open_path(path: &Path) -> Result<(), String> {
    open_with("xdg-open", path)
}

#[cfg(target_os = "windows")]
fn open_path(path: &Path) -> Result<(), String> {
    open_with("explorer.exe", path)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn open_path(_path: &Path) -> Result<(), String> {
    Err("当前系统暂不支持打开目录。".into())
}

fn open_with(program: &str, path: &Path) -> Result<(), String> {
    std::process::Command::new(program)
        .arg(path)
        .spawn()
        .map_err(|error| format!("无法打开目录：{error}"))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn parse_product_id(value: &str) -> Result<u64, String> {
    let normalized = value.trim().trim_start_matches('0');
    let normalized = if normalized.is_empty() {
        "0"
    } else {
        normalized
    };
    let id = normalized
        .parse::<u64>()
        .map_err(|_| "Product ID 必须是正整数。".to_string())?;
    if id == 0 {
        return Err("Product ID 必须是正整数。".into());
    }
    Ok(id)
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    #[test]
    fn windows_login_entry_quotes_chinese_and_spaced_path() {
        assert_eq!(
            super::login_start_contents(std::path::Path::new(
                r"C:\Users\ricohtest\理光 测试\monitor.exe"
            )),
            r#""C:\Users\ricohtest\理光 测试\monitor.exe""#
        );
    }

    #[test]
    fn windows_login_start_state_is_available() {
        assert!(
            super::login_start_enabled().is_some(),
            "Windows 应能读取当前用户的登录启动状态"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{desktop_exec_escape, login_start_contents, redact_channel_messages};
    use std::path::Path;

    #[test]
    fn linux_login_entry_quotes_executable_path() {
        let entry = login_start_contents(Path::new("/opt/Ricoh Monitor/bin/app"));
        assert!(entry.contains("Exec=\"/opt/Ricoh Monitor/bin/app\""));
        assert_eq!(desktop_exec_escape("/opt/a\\b\"c"), "/opt/a\\\\b\\\"c");
    }

    #[test]
    fn diagnostic_preview_omits_channel_error_details() {
        let mut snapshot = serde_json::json!({
            "channels": [{
                "lastTest": {"outcome": "failed", "message": "https://hook.example/secret"},
                "lastDelivery": {"outcome": "accepted", "message": "服务已接受通知"}
            }]
        });
        redact_channel_messages(&mut snapshot);
        let preview = snapshot.to_string();
        assert!(!preview.contains("secret"));
        assert!(preview.contains("操作失败，详细错误已省略。"));
        assert!(preview.contains("平台已接受通知，不代表用户已读。"));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{
        login_start_contents, needs_notification_request, now_ms, redact_channel_messages,
        xml_escape, DesktopBackend, DesktopSnapshot,
    };
    use ricoh_monitor_core::storage::{ProductIdentity, Storage};
    use std::path::Path;
    use tauri::plugin::PermissionState;

    #[test]
    fn logs_rotate_with_bounded_unicode_entries() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-log-limit-{}-{}",
            std::process::id(),
            now_ms()
        ));
        for _ in 0..35 {
            super::write_log(&directory, &"监控".repeat(25_000));
        }
        let logs = directory.join("logs");
        assert!(
            std::fs::metadata(logs.join("monitor.previous.log"))
                .unwrap()
                .len()
                <= 1_048_576
        );
        assert!(std::fs::metadata(logs.join("monitor.log")).unwrap().len() <= 1_048_576);
        let text = std::fs::read_to_string(logs.join("monitor.log")).unwrap();
        assert!(text.contains("日志条目已截短"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn mac_login_entry_escapes_executable_path() {
        let entry = login_start_contents(Path::new("/Applications/A&B/app"));
        assert!(entry.contains("/Applications/A&amp;B/app"));
        assert_eq!(xml_escape("<&\"'"), "&lt;&amp;&quot;&apos;");
    }

    #[test]
    fn test_notification_requests_permission_only_when_needed() {
        assert!(needs_notification_request(PermissionState::Prompt));
        assert!(needs_notification_request(
            PermissionState::PromptWithRationale
        ));
        assert!(!needs_notification_request(PermissionState::Denied));
        assert!(!needs_notification_request(PermissionState::Granted));
    }

    #[test]
    fn diagnostic_preview_omits_channel_error_details() {
        let mut snapshot = serde_json::json!({
            "channels": [{
                "lastTest": {"outcome": "failed", "message": "https://hook.example/secret"},
                "lastDelivery": {"outcome": "accepted", "message": "服务已接受通知"}
            }]
        });
        redact_channel_messages(&mut snapshot);
        let preview = snapshot.to_string();
        assert!(!preview.contains("secret"));
        assert!(preview.contains("操作失败，详细错误已省略。"));
        assert!(preview.contains("平台已接受通知，不代表用户已读。"));
    }

    #[tokio::test]
    async fn notification_read_errors_survive_monitor_snapshots_and_clear_on_real_recovery() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-permission-read-error-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let backend = DesktopBackend::open(&directory, PermissionState::Granted).unwrap();
        let mut snapshots = backend.subscribe();
        let failure = "Windows 通知权限读取失败：0x80070490".to_string();
        backend
            .set_notification_permission(Err(failure.clone()))
            .await
            .unwrap();
        snapshots.try_recv().expect("权限错误应发布状态");
        let snapshot = DesktopSnapshot::new(
            backend.app.snapshot().await.unwrap(),
            backend.notification_permission(),
        );
        assert_eq!(snapshot.platform.notification_permission, "unavailable");
        assert_eq!(
            snapshot.platform.notification_permission_error.as_deref(),
            Some(failure.as_str())
        );
        while snapshots.try_recv().is_ok() {}
        backend
            .set_notification_permission(Err(failure))
            .await
            .unwrap();
        assert!(snapshots.try_recv().is_err(), "相同错误不反复发布");
        backend
            .set_notification_permission(Ok(PermissionState::Granted))
            .await
            .unwrap();
        snapshots.try_recv().expect("读取恢复应发布状态");
        let snapshot = DesktopSnapshot::new(
            backend.app.snapshot().await.unwrap(),
            backend.notification_permission(),
        );
        assert_eq!(snapshot.platform.notification_permission, "granted");
        assert!(snapshot.platform.notification_permission_error.is_none());
        drop(backend);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn idle_permission_changes_publish_once_and_recover() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-idle-permission-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let backend = DesktopBackend::open(&directory, PermissionState::Prompt).unwrap();
        let mut snapshots = backend.subscribe();
        backend
            .set_notification_permission(Ok(PermissionState::Prompt))
            .await
            .unwrap();
        assert!(snapshots.try_recv().is_err());
        for permission in [
            PermissionState::Granted,
            PermissionState::Denied,
            PermissionState::Granted,
        ] {
            backend
                .set_notification_permission(Ok(permission))
                .await
                .unwrap();
            let snapshot = snapshots.try_recv().expect("权限变化应立即通知界面");
            assert_eq!(
                DesktopSnapshot::new(snapshot, backend.notification_permission())
                    .platform
                    .notification_permission,
                if permission == PermissionState::Granted {
                    "granted"
                } else {
                    "denied"
                }
            );
            backend
                .set_notification_permission(Ok(permission))
                .await
                .unwrap();
            assert!(snapshots.try_recv().is_err());
        }
        drop(backend);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn notification_permission_updates_platform_without_gating_setup() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-desktop-permission-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir(&directory).unwrap();
        let mut storage = Storage::open(directory.join("monitor.sqlite3")).unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "65".into(),
                    product_id: "65".into(),
                    sku_id: None,
                },
                "Fixture 65".into(),
                "fixture".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("65", true).unwrap();
        drop(storage);

        let backend = DesktopBackend::open(&directory, PermissionState::Prompt).unwrap();
        assert!(!backend.app.snapshot().await.unwrap().setup_completed);
        assert_eq!(
            DesktopSnapshot::new(
                backend.app.snapshot().await.unwrap(),
                backend.notification_permission()
            )
            .platform
            .notification_permission,
            "prompt"
        );
        backend.app.complete_setup().await.unwrap();
        assert!(backend.app.snapshot().await.unwrap().setup_completed);
        backend
            .set_notification_permission(Ok(PermissionState::Granted))
            .await
            .unwrap();
        assert_eq!(
            DesktopSnapshot::new(
                backend.app.snapshot().await.unwrap(),
                backend.notification_permission()
            )
            .platform
            .notification_permission,
            "granted"
        );
        assert!(backend.app.snapshot().await.unwrap().setup_completed);
        backend
            .set_notification_permission(Ok(PermissionState::Denied))
            .await
            .unwrap();
        assert_eq!(
            DesktopSnapshot::new(
                backend.app.snapshot().await.unwrap(),
                backend.notification_permission()
            )
            .platform
            .notification_permission,
            "denied"
        );
        backend
            .worker_alive
            .store(false, std::sync::atomic::Ordering::Release);
        assert!(backend
            .ensure_worker_alive()
            .unwrap_err()
            .contains("重新打开应用"));
        drop(backend);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
