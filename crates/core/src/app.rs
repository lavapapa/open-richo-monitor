//! Desktop 与命令行共用的业务边界。
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch, Mutex as AsyncMutex, Notify};

use crate::{
    availability::{
        reducer::{
            AvailabilityResponse, ObservationReducer, ObservationResult, PrepareOutcome,
            RuntimeGate,
        },
        Availability,
    },
    catalog::{
        ProductIdScan, ScanCheckpoint, ScanOutcome, ScanStatus as CoreScanStatus, DEFAULT_PRODUCTS,
    },
    config::{MonitorConfig, MonitoringMode},
    monitoring_time::MonitoringTime,
    notifications::{NotificationRuntime, NotificationTarget, SendResult},
    ricoh::{ProductDetail, ProductMetadata},
    ricoh_api::RicohApi,
    runtime::RicohSchedulerClient,
    scheduler::{
        schedule_state, ControlToken, LineId, OsJitter, RequestGate, ScheduledMonitor, Scheduler,
        SchedulerClient, TokioClock,
    },
    storage::{HistoryCursor, OutboxJob, ProductIdentity, RunIntent, Storage, StorageError},
};

const SNAPSHOT_CAPACITY: usize = 16;
static BINDING_SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub const MAX_CONFIG_IMPORT_BYTES: usize = 4 * 1024 * 1024;

struct ProductRead {
    id: u64,
    cancelled: Arc<AtomicBool>,
    pending: Arc<Mutex<BTreeMap<u64, Arc<AtomicBool>>>>,
}

struct NotificationTestLease {
    id: String,
    active: Arc<Mutex<BTreeSet<String>>>,
}

impl Drop for NotificationTestLease {
    fn drop(&mut self) {
        self.active.lock().unwrap().remove(&self.id);
    }
}

impl Drop for ProductRead {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}

struct NotificationJob {
    event: crate::storage::ListingEvent,
    subscription: &'static str,
    channel_id: Option<String>,
    generation: Option<u64>,
    target: Option<NotificationTarget>,
    waited_for_connection: bool,
}

struct RoutedSchedulerClient {
    clients: BTreeMap<String, Arc<dyn crate::scheduler::SchedulerClient>>,
}

impl crate::scheduler::SchedulerClient for RoutedSchedulerClient {
    fn product_page(
        &self,
        outlet_id: String,
        page: u32,
        limit: u32,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Vec<ProductDetail>, crate::ricoh_api::RicohApiError>,
                > + Send,
        >,
    > {
        let client = self.clients.get(&outlet_id).cloned();
        Box::pin(async move {
            match client {
                Some(client) => client.product_page(outlet_id, page, limit).await,
                None => Err(crate::ricoh_api::RicohApiError::InvalidConfiguration),
            }
        })
    }
    fn product_detail(
        &self,
        line: LineId,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<ProductDetail, crate::ricoh_api::RicohApiError>>
                + Send,
        >,
    > {
        let client = self.clients.get(&line.outlet_id).cloned();
        Box::pin(async move {
            match client {
                Some(client) => client.product_detail(line).await,
                None => Err(crate::ricoh_api::RicohApiError::InvalidConfiguration),
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub days: Vec<Weekday>,
    pub start: String,
    pub end: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RateConfig {
    pub interval_min_ms: u64,
    pub interval_max_ms: u64,
    pub failures_before_backoff: u32,
    pub failure_backoff_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default = "crate::config::default_auto_start_monitoring")]
    pub auto_start_monitoring: bool,
    #[serde(default)]
    pub monitoring_mode: MonitoringMode,
    pub schedule: Schedule,
    pub rate: RateConfig,
    pub use_system_proxy: bool,
    pub use_proxy_pool: bool,
    pub failure_alert_after_minutes: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::from_monitor(MonitorConfig::default())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Monitoring,
    PartialError,
    OutsideSchedule,
    Paused,
    SetupIncomplete,
    Stopped,
    WorkerFailed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub state: RuntimeState,
    pub next_start_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProductObservation {
    pub availability: String,
    pub is_show: u8,
    pub stock: Option<f64>,
    pub checked_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProductRecord {
    pub product_id: String,
    pub name: String,
    pub enabled: bool,
    pub prominent_alert: bool,
    pub check_count: u64,
    pub observation: Option<ProductObservation>,
    pub runtime_error: Option<String>,
    pub metadata: Option<ProductMetadata>,
    pub image_path: Option<String>,
    pub metadata_updated_at: Option<String>,
    pub today_check_count: u64,
    pub today_success_count: u64,
    pub today_failure_count: u64,
    pub monitoring_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelEvent {
    StockAvailable,
    MonitoringFailed,
    Recovered,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChannelTest {
    pub outcome: String,
    pub message: Option<String>,
    #[serde(default)]
    pub recipients: Vec<RecipientTest>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecipientTest {
    pub target: NotificationTarget,
    pub outcome: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChannelDelivery {
    pub outcome: String,
    pub event: String,
    pub at: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecipientDelivery {
    pub target: NotificationTarget,
    pub last_delivery: Option<ChannelDelivery>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChannelView {
    pub id: String,
    pub name: String,
    pub bot_name: Option<String>,
    pub bot_url: Option<String>,
    pub app_name: Option<String>,
    pub provider_id: String,
    pub provider_name: String,
    pub enabled: bool,
    pub configured_field_keys: Vec<String>,
    pub subscriptions: Vec<ChannelEvent>,
    pub last_test: Option<ChannelTest>,
    pub last_delivery: Option<ChannelDelivery>,
    pub connection_status: String,
    pub targets: Vec<NotificationTarget>,
    pub target_id: Option<String>,
    pub target_kind: Option<String>,
    pub selected_targets: Vec<NotificationTarget>,
    pub recipient_deliveries: Vec<RecipientDelivery>,
}

/// 此输入短暂携带凭据，因此有意不实现 Debug、Clone 与 Serialize。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInput {
    pub id: Option<String>,
    pub name: String,
    pub provider_id: String,
    pub values: BTreeMap<String, String>,
    pub subscriptions: Vec<ChannelEvent>,
    #[serde(default)]
    pub binding_id: Option<String>,
    #[serde(default)]
    pub targets: Option<Vec<NotificationTarget>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderField {
    pub key: String,
    pub label: String,
    pub r#type: String,
    pub required: bool,
    pub placeholder: Option<String>,
    pub help: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDefinition {
    pub id: String,
    pub name: String,
    pub documentation_url: Option<String>,
    pub fields: Vec<ProviderField>,
    pub supports_binding: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProxyProtocol {
    Http,
    Https,
    Socks5,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProxyInput {
    pub protocol: ProxyProtocol,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProxyStatus {
    Untested,
    Available,
    Cooldown,
    AutoDisabled,
    ManuallyDisabled,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProxyRecord {
    pub id: String,
    pub protocol: ProxyProtocol,
    pub display_address: String,
    pub enabled: bool,
    pub status: ProxyStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecentEvent {
    pub id: i64,
    pub at: String,
    pub kind: String,
    pub product_id: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProminentAlert {
    pub event_id: i64,
    pub product_id: String,
    pub name: String,
    pub image_url: Option<String>,
    pub image_path: Option<String>,
    pub price: Option<String>,
    pub stock: f64,
    pub at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecentCheck {
    pub product_id: String,
    pub name: String,
    pub availability: String,
    pub is_show: u8,
    pub stock: Option<f64>,
    pub at: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MessageCursor {
    pub at_ms: i64,
    pub source: i64,
    pub id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MessageQuery {
    pub date: Option<String>,
    pub product_id: Option<String>,
    pub cursor: Option<MessageCursor>,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MessageItem {
    pub at: String,
    pub product_id: String,
    pub name: String,
    pub is_show: Option<u8>,
    pub stock: Option<f64>,
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MessagePage {
    pub items: Vec<MessageItem>,
    pub next_cursor: Option<MessageCursor>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    Running,
    Paused,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProductScan {
    pub id: String,
    pub start_id: String,
    pub end_id: String,
    pub current_id: Option<String>,
    pub checked: u64,
    pub found: u64,
    pub status: ScanStatus,
    pub error: Option<String>,
    pub results: Vec<ProductRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub setup_completed: bool,
    pub system_notifications_enabled: bool,
    pub system_notification_delivery: Option<ChannelDelivery>,
    pub runtime: RuntimeSnapshot,
    pub config: AppConfig,
    pub products: Vec<ProductRecord>,
    pub catalog: Vec<ProductRecord>,
    pub channels: Vec<ChannelView>,
    pub providers: Vec<ProviderDefinition>,
    pub proxies: Vec<ProxyRecord>,
    pub scan: Option<ProductScan>,
    pub recent_events: Vec<RecentEvent>,
    pub recent_checks: Vec<RecentCheck>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MonitoringAction {
    Start,
    Pause,
    Resume,
    Stop,
    Restart,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScanAction {
    Pause,
    Resume,
    Cancel,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProxyImportIssue {
    pub line: usize,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProxyImportResult {
    pub added: Vec<String>,
    pub duplicates: usize,
    pub invalid: Vec<ProxyImportIssue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppError {
    Storage(String),
    InvalidInput(String),
    Network(String),
    NotFound(String),
    Credential(String),
    Unsupported(String),
    AlreadyRunning,
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(s)
            | Self::InvalidInput(s)
            | Self::Network(s)
            | Self::NotFound(s)
            | Self::Credential(s)
            | Self::Unsupported(s) => f.write_str(s),
            Self::AlreadyRunning => f.write_str("此数据目录已有监控进程运行"),
        }
    }
}
impl std::error::Error for AppError {}
#[derive(Clone)]
pub struct MonitorApp {
    data_dir: PathBuf,
    storage: Arc<Mutex<Storage>>,
    snapshots: broadcast::Sender<AppSnapshot>,
    prominent_alert_wake: Arc<Notify>,
    channel_notification_wake: Arc<Notify>,
    system_notification_wake: Arc<Notify>,
    scan: Arc<AsyncMutex<Option<ProductIdScan>>>,
    scan_epoch: watch::Sender<u64>,
    platform_notifications_available: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    product_generations: Arc<Mutex<BTreeMap<u64, u64>>>,
    request_gate: Arc<RequestGate>,
    client_override: Option<Arc<dyn crate::scheduler::SchedulerClient>>,
    notification_runtime: NotificationRuntime,
    notification_proxy_url: Arc<Mutex<Result<Option<String>, String>>>,
    notification_test_accounts: Arc<Mutex<BTreeSet<String>>>,
    notification_edit_accounts: Arc<Mutex<HashMap<String, usize>>>,
    notification_edit_operations: Arc<AsyncMutex<()>>,
    notification_configuration: Arc<AsyncMutex<()>>,
    notification_binding_operations: Arc<AsyncMutex<()>>,
    runtime_errors: Arc<Mutex<BTreeMap<String, String>>>,
    last_error: Arc<Mutex<Option<String>>>,
    worker_error: Arc<Mutex<Option<String>>>,
    latest_snapshot: Arc<Mutex<Option<AppSnapshot>>>,
    monitoring_time: Arc<Mutex<MonitoringTime>>,
    catalog_epoch: Arc<AtomicU64>,
    catalog_refreshing: Arc<AtomicBool>,
    pending_products: Arc<Mutex<BTreeMap<u64, Arc<AtomicBool>>>>,
    image_paths: Arc<Mutex<BTreeMap<u64, (String, PathBuf)>>>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigExport {
    config: AppConfig,
    products: Vec<ProductExport>,
    channels: Vec<ChannelExport>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductExport {
    product_id: String,
    name: String,
    enabled: bool,
    prominent_alert: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelExport {
    id: String,
    name: String,
    provider_id: String,
    subscriptions: Vec<ChannelEvent>,
    #[serde(default)]
    targets: Vec<NotificationTarget>,
}

impl MonitorApp {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, AppError> {
        let data_dir = data_dir.as_ref();
        #[cfg(unix)]
        let directory_existed = data_dir.exists();
        std::fs::create_dir_all(data_dir).map_err(|e| AppError::Storage(e.to_string()))?;
        #[cfg(unix)]
        if !directory_existed {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| AppError::Storage(error.to_string()))?;
        }
        let storage = Storage::open(data_dir.join("monitor.sqlite3"))?;
        let config = storage.monitor_config()?;
        let request_gate = Arc::new(RequestGate::new(
            &config.requests,
            config.scan.interval,
            Arc::new(TokioClock::default()),
        ));
        let checkpoint = storage.scan_checkpoint()?;
        let product_generations = storage.product_generations()?;
        let (snapshots, _) = broadcast::channel(SNAPSHOT_CAPACITY);
        let (scan_epoch, _) = watch::channel(0);
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            storage: Arc::new(Mutex::new(storage)),
            snapshots,
            prominent_alert_wake: Arc::new(Notify::new()),
            channel_notification_wake: Arc::new(Notify::new()),
            system_notification_wake: Arc::new(Notify::new()),
            scan: Arc::new(AsyncMutex::new(checkpoint.map(ProductIdScan::restore))),
            scan_epoch,
            platform_notifications_available: Arc::new(AtomicBool::new(cfg!(test))),
            stopping: Arc::new(AtomicBool::new(false)),
            product_generations: Arc::new(Mutex::new(product_generations)),
            request_gate,
            client_override: None,
            notification_runtime: NotificationRuntime::default(),
            notification_proxy_url: Arc::new(Mutex::new(Ok(None))),
            notification_test_accounts: Arc::new(Mutex::new(BTreeSet::new())),
            notification_edit_accounts: Arc::new(Mutex::new(HashMap::new())),
            notification_edit_operations: Arc::new(AsyncMutex::new(())),
            notification_configuration: Arc::new(AsyncMutex::new(())),
            notification_binding_operations: Arc::new(AsyncMutex::new(())),
            runtime_errors: Arc::new(Mutex::new(BTreeMap::new())),
            last_error: Arc::new(Mutex::new(None)),
            worker_error: Arc::new(Mutex::new(None)),
            latest_snapshot: Arc::new(Mutex::new(None)),
            monitoring_time: Arc::new(Mutex::new(MonitoringTime::default())),
            catalog_epoch: Arc::new(AtomicU64::new(0)),
            catalog_refreshing: Arc::new(AtomicBool::new(false)),
            pending_products: Arc::new(Mutex::new(BTreeMap::new())),
            image_paths: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    /// 桌面进程首次启动时应用偏好，关窗后重新显示窗口不调用此方法。
    pub fn apply_startup_monitoring(&self) -> Result<(), AppError> {
        let mut db = self.storage.lock().unwrap();
        let start = db.monitor_config()?.auto_start_monitoring && setup_completed(&db)?;
        let intent = if start {
            RunIntent::Running
        } else {
            RunIntent::Stopped
        };
        if db.run_intent()? != intent {
            db.set_run_intent(intent, !start)?;
            *self.product_generations.lock().unwrap() = db.product_generations()?;
        }
        Ok(())
    }

    /// 本地 fixture 接口，供 Core 与 shell 验收使用；正式应用通过 `open` 创建。
    pub fn open_with_scheduler_client(
        data_dir: impl AsRef<Path>,
        client: Arc<dyn crate::scheduler::SchedulerClient>,
    ) -> Result<Self, AppError> {
        let mut app = Self::open(data_dir)?;
        app.client_override = Some(client);
        Ok(app)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppSnapshot> {
        self.snapshots.subscribe()
    }

    /// 单个突出提醒消费者等待队列变化；先领取启动时已有任务，再等待唤醒。
    pub async fn wait_for_prominent_alert(&self) {
        self.prominent_alert_wake.notified().await;
    }

    /// 关闭当前提醒或退出时唤醒消费者；等待尚未开始时保留一个许可。
    pub fn wake_prominent_alerts(&self) {
        self.prominent_alert_wake.notify_one();
    }

    pub async fn wait_for_system_notification(&self) {
        self.system_notification_wake.notified().await;
    }

    fn wake_notifications(&self) {
        self.channel_notification_wake.notify_one();
        self.system_notification_wake.notify_one();
        self.wake_prominent_alerts();
    }

    pub async fn query_messages(&self, query: MessageQuery) -> Result<MessagePage, AppError> {
        let now_ms = (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
        let cutoff = now_ms.saturating_sub(30 * 24 * 60 * 60 * 1000);
        let (from_ms, to_ms) = if let Some(date) = query.date.as_deref() {
            let date = parse_beijing_date(date)?;
            let start = date
                .midnight()
                .assume_offset(time::UtcOffset::from_hms(8, 0, 0).expect("UTC+8"));
            let from = (start.unix_timestamp_nanos() / 1_000_000) as i64;
            (from.max(cutoff), from.saturating_add(24 * 60 * 60 * 1000))
        } else {
            (cutoff, i64::MAX)
        };
        let product_id = query.product_id.filter(|value| !value.is_empty());
        let cursor = query.cursor.map(|c| HistoryCursor {
            at_ms: c.at_ms,
            source: c.source,
            id: c.id,
        });
        let page = self
            .call(move |db| {
                db.query_history(
                    from_ms,
                    to_ms,
                    product_id.as_deref(),
                    cursor,
                    query.limit as usize,
                )
            })
            .await?;
        Ok(MessagePage {
            items: page
                .items
                .into_iter()
                .map(|item| MessageItem {
                    at: format_timestamp(item.cursor.at_ms),
                    product_id: item.product_id,
                    name: item.name,
                    is_show: item.is_show,
                    stock: item.stock,
                    detail: item.detail,
                })
                .collect(),
            next_cursor: page.next_cursor.map(|c| MessageCursor {
                at_ms: c.at_ms,
                source: c.source,
                id: c.id,
            }),
        })
    }

    pub fn set_platform_notifications_available(&self, available: bool) {
        self.platform_notifications_available
            .store(available, Ordering::Relaxed);
        if available {
            self.system_notification_wake.notify_one();
        }
    }
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.wake_notifications();
        self.notification_runtime.cancel_pending();
    }

    pub fn set_notification_runtime_path(&self, path: impl AsRef<Path>) {
        self.notification_runtime.set_path(path);
    }

    pub fn set_notification_runtime_arguments(&self, arguments: Vec<String>) {
        self.notification_runtime.set_arguments(arguments);
    }

    pub fn take_notification_diagnostics(&self) -> Vec<serde_json::Value> {
        self.notification_runtime.take_diagnostics()
    }

    pub fn set_notification_proxy_url(&self, url: Result<Option<String>, String>) {
        *self.notification_proxy_url.lock().unwrap() = url;
    }

    pub async fn shutdown_notification_runtime(&self) {
        let _edit_operation = self.notification_edit_operations.lock().await;
        self.notification_runtime.shutdown().await;
        self.notification_edit_accounts.lock().unwrap().clear();
        let _ = self.persist_notification_credentials().await;
    }

    pub async fn begin_channel_editing(&self, id: &str) -> Result<(), AppError> {
        let _operation = self.notification_edit_operations.lock().await;
        let id = id.to_owned();
        let id_for_db = id.clone();
        self.call(move |db| {
            let channel = db
                .notification_channels()?
                .into_iter()
                .find(|channel| channel.id == id_for_db)
                .ok_or(StorageError::ChannelMissing)?;
            let reference = channel.credential_ref.ok_or(StorageError::InvalidChannel)?;
            let saved = db
                .credential("channel", &reference)?
                .ok_or(StorageError::InvalidChannel)?;
            let values: BTreeMap<String, String> =
                serde_json::from_str(&saved).map_err(StorageError::ConfigJson)?;
            if channel.provider == "system" || !channel_credentials(&values).is_object() {
                return Err(StorageError::InvalidChannel);
            }
            Ok(())
        })
        .await?;

        *self
            .notification_edit_accounts
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_default() += 1;
        if let Err(error) = self.configure_notification_runtime(Some(&id)).await {
            Self::release_channel_editing(&self.notification_edit_accounts, &id);
            let _ = self.configure_notification_runtime(None).await;
            return Err(error);
        }
        Ok(())
    }

    pub async fn end_channel_editing(&self, id: &str) -> Result<(), AppError> {
        let _operation = self.notification_edit_operations.lock().await;
        Self::release_channel_editing(&self.notification_edit_accounts, id);
        self.configure_notification_runtime(None).await
    }

    fn release_channel_editing(active: &Arc<Mutex<HashMap<String, usize>>>, id: &str) {
        let mut active = active.lock().unwrap();
        if let Some(count) = active.get_mut(id) {
            *count -= 1;
            if *count == 0 {
                active.remove(id);
            }
        }
    }

    pub async fn claim_system_notification(&self) -> Result<Option<(i64, RecentEvent)>, AppError> {
        if !self
            .platform_notifications_available
            .load(Ordering::Relaxed)
        {
            return Ok(None);
        }
        let (job, changed) = self
            .call(|db| {
                let before = db.system_notification_delivery()?;
                let job = db.claim_system_notification(now_ms())?;
                let changed = before != db.system_notification_delivery()?;
                Ok((job, changed))
            })
            .await?;
        if changed {
            self.snapshot().await?;
        }
        let Some(job) = job else { return Ok(None) };
        let alert_text = self.notification_text(&job.event).await?;
        Ok(Some({
            let kind = match job.event.kind {
                crate::storage::MonitorEventKind::FirstObservedInStock
                | crate::storage::MonitorEventKind::OutOfStockToInStock => "stock_available",
                crate::storage::MonitorEventKind::MonitoringFailed => "monitoring_failed",
                crate::storage::MonitorEventKind::StockIncreased => "stock_increased",
                crate::storage::MonitorEventKind::Recovered => "recovered",
            };
            let event = RecentEvent {
                id: job.event.id,
                at: format_timestamp(job.event.observed_at_ms),
                kind: kind.into(),
                product_id: job.event.product.product_id.clone(),
                message: alert_text,
            };
            (job.id, event)
        }))
    }

    pub async fn claim_prominent_alert(&self) -> Result<Option<ProminentAlert>, AppError> {
        let image_paths = self.image_paths.clone();
        let image_root = self.data_dir.join("images");
        self.call(move |db| {
            let Some(job) = db.claim_prominent_alert(now_ms())? else {
                return Ok(None);
            };
            let product = db
                .product_configs()?
                .into_iter()
                .find(|p| p.identity.key == job.event.product.key)
                .ok_or(StorageError::ProductConfigMissing)?;
            let metadata = db
                .product_metadata(&job.event.product.key)?
                .map(|(metadata, _)| metadata);
            let image_url = metadata.as_ref().and_then(|m| m.image_url.clone());
            let image_path = job
                .event
                .product
                .product_id
                .parse::<u64>()
                .ok()
                .and_then(|id| {
                    let memory = image_paths.lock().unwrap().get(&id).cloned();
                    let (url, path) = memory
                        .filter(|(url, path)| {
                            path.is_file()
                                && image_url.as_ref().is_none_or(|expected| expected == url)
                        })
                        .or_else(|| {
                            crate::image_cache::ImageCache::cached_image(&image_root, id)
                        })?;
                    image_url
                        .as_ref()
                        .is_none_or(|expected| expected == &url)
                        .then(|| path.to_string_lossy().into_owned())
                });
            Ok(Some(ProminentAlert {
                event_id: job.event.id,
                product_id: job.event.product.product_id,
                name: product.name,
                image_url,
                image_path,
                price: metadata.and_then(|m| m.price),
                stock: job.event.stock,
                at: format_timestamp(job.event.observed_at_ms),
            }))
        })
        .await
    }

    pub async fn set_product_prominent_alert(
        &self,
        product_id: u64,
        enabled: bool,
    ) -> Result<(), AppError> {
        self.call(move |db| db.set_product_prominent_alert(&product_id.to_string(), enabled))
            .await?;
        self.snapshot().await?;
        Ok(())
    }

    pub async fn acknowledge_prominent_alert(&self, event_id: i64) -> Result<(), AppError> {
        self.call(move |db| db.acknowledge_prominent_alert(event_id))
            .await
    }

    pub async fn set_onboarding_products(&self, product_ids: &[u64]) -> Result<(), AppError> {
        let ids = product_ids.to_vec();
        self.settle_monitoring_time(false).await?;
        let app = self.clone();
        self.call(move |db| {
            db.set_onboarding_products(&ids)?;
            *app.product_generations.lock().unwrap() = db.product_generations()?;
            app.catalog_epoch.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
        .await?;
        self.snapshot().await?;
        Ok(())
    }

    pub async fn finish_system_notification(
        &self,
        id: i64,
        result: Result<(), String>,
    ) -> Result<(), AppError> {
        let (outcome, message) = match result {
            Ok(()) => ("accepted", "系统已接受通知".to_owned()),
            Err(error) => ("failed", error),
        };
        self.call(move |db| db.finish_notification(id, outcome, &message, now_ms()))
            .await?;
        self.snapshot().await?;
        Ok(())
    }

    /// 任务异常归 Core 记录，存储不可读时仍发布最后观察的时间及故障状态。
    pub async fn report_worker_failure(&self, error: String) {
        *self.worker_error.lock().unwrap() = Some(error.clone());
        if self.snapshot().await.is_err() {
            if let Some(mut snapshot) = self.latest_snapshot.lock().unwrap().clone() {
                snapshot.runtime.state = RuntimeState::WorkerFailed;
                snapshot.runtime.next_start_at = None;
                snapshot.runtime.last_error = Some(error);
                let _ = self.snapshots.send(snapshot);
            }
        }
    }

    async fn call<T, F>(&self, operation: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Storage) -> Result<T, StorageError> + Send + 'static,
    {
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || operation(&mut storage.lock().unwrap()))
            .await
            .map_err(|error| AppError::Storage(error.to_string()))?
            .map_err(Into::into)
    }

    async fn settle_monitoring_time(&self, stop: bool) -> Result<(), AppError> {
        let timer = self.monitoring_time.clone();
        self.call(move |db| {
            let mut timer = timer.lock().unwrap();
            db.commit_time_settlement(|db| {
                let now = std::time::Instant::now();
                let epoch = db.catalog_epoch()?;
                let configured = db.product_configs()?;
                let versions = configured
                    .iter()
                    .filter(|p| p.enabled)
                    .map(|p| {
                        Ok((
                            p.identity.key.clone(),
                            (db.product_generation(&p.identity.key)?, epoch),
                        ))
                    })
                    .collect::<Result<BTreeMap<_, _>, StorageError>>()?;
                timer.reconcile(versions);
                if stop {
                    timer.stop(now);
                }
                let deltas = timer.pending(now).into_iter().collect::<Vec<_>>();
                for (key, delta) in deltas {
                    db.add_monitoring_duration(&key, delta)?;
                }
                Ok(())
            })?;
            timer.settled();
            Ok(())
        })
        .await
    }

    async fn set_timing_products(
        &self,
        keys: Vec<String>,
        until: std::time::Instant,
    ) -> Result<(), AppError> {
        let timer = self.monitoring_time.clone();
        self.call(move |db| {
            db.commit_time_settlement(|db| {
                let epoch = db.catalog_epoch()?;
                let current = db.product_configs()?;
                let versions = current
                    .iter()
                    .filter(|p| p.enabled)
                    .map(|p| {
                        Ok((
                            p.identity.key.clone(),
                            (db.product_generation(&p.identity.key)?, epoch),
                        ))
                    })
                    .collect::<Result<BTreeMap<_, _>, StorageError>>()?;
                let config = db.monitor_config()?;
                let (in_schedule, transition) =
                    schedule_state(&config.schedule, std::time::SystemTime::now());
                let now = std::time::Instant::now();
                let keys = if db.run_intent()? == RunIntent::Running && in_schedule {
                    keys.into_iter()
                        .filter(|k| versions.contains_key(k))
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let mut timer = timer.lock().unwrap();
                timer.reconcile(versions);
                timer.update(now, &keys, until.min(now + transition));
                Ok(())
            })
        })
        .await
    }

    /// 展示资料通过同一个签名客户端和请求门读取，目录刷新不会启用商品。
    pub fn refresh_catalog_metadata(&self) {
        if self.catalog_refreshing.swap(true, Ordering::AcqRel) {
            return;
        }
        let app = self.clone();
        tokio::spawn(async move {
            let result = app.hydrate_catalog_metadata().await;
            if let Err(error) = result {
                *app.last_error.lock().unwrap() = Some(error.to_string());
                let _ = app.snapshot().await;
            }
            app.catalog_refreshing.store(false, Ordering::Release);
        });
    }

    async fn hydrate_catalog_metadata(&self) -> Result<(), AppError> {
        let epoch = self.catalog_epoch.load(Ordering::Acquire);
        let session_epoch = self.call(|db| db.catalog_epoch()).await?;
        let mut ids = self
            .call(|db| {
                Ok(db
                    .product_configs()?
                    .into_iter()
                    .filter_map(|p| p.identity.product_id.parse::<u64>().ok())
                    .collect::<Vec<_>>())
            })
            .await?;
        for &(id, _) in DEFAULT_PRODUCTS {
            let key = id.to_string();
            let removed = self.call(move |db| db.product_removed(&key)).await?;
            if !removed && !ids.contains(&id) {
                ids.push(id);
            }
        }
        for id in ids {
            if self.stopping.load(Ordering::Relaxed)
                || self.catalog_epoch.load(Ordering::Acquire) != epoch
            {
                break;
            }
            let cache_root = self.data_dir.join("images");
            let pending = self
                .call(move |db| {
                    if db.catalog_epoch()? != session_epoch {
                        return Ok(None);
                    }
                    let metadata = db.product_metadata(&id.to_string())?;
                    if metadata
                        .as_ref()
                        .is_some_and(|(m, _)| m.image_url.is_some())
                        || crate::image_cache::ImageCache::cached_image(&cache_root, id).is_some()
                    {
                        return Ok(None);
                    }
                    Ok(Some((
                        db.product_generation(&id.to_string())?,
                        metadata.map(|(m, _)| m),
                    )))
                })
                .await?;
            let Some((generation, expected_metadata)) = pending else {
                if self.call(|db| db.catalog_epoch()).await? != session_epoch {
                    break;
                }
                continue;
            };
            let detail = match self.fetch_product_detail(id).await {
                Ok(detail) => detail,
                Err(error) => {
                    *self.last_error.lock().unwrap() = Some(error.to_string());
                    self.snapshot().await?;
                    continue;
                }
            };
            let catalog_epoch = self.catalog_epoch.clone();
            let saved = self
                .call(move |db| {
                    if catalog_epoch.load(Ordering::Acquire) != epoch {
                        return Ok(false);
                    }
                    db.commit_verified_product(
                        &detail,
                        generation,
                        session_epoch,
                        false,
                        expected_metadata.as_ref(),
                        now_ms(),
                    )
                })
                .await?;
            if saved {
                self.snapshot().await?;
            }
        }
        Ok(())
    }

    async fn image_cache_loop(&self) {
        use crate::image_cache::{ImageCache, ImageCacheError};
        let mut cache = None;
        let mut previous_network = None;
        let mut initialization_error = None;
        let mut attempts: BTreeMap<u64, (String, std::time::Instant)> = BTreeMap::new();
        while !self.stopping.load(Ordering::Relaxed) {
            let Ok((config, mut products)) = self
                .call(|db| {
                    let products = db
                        .product_configs()?
                        .into_iter()
                        .map(|p| {
                            let metadata = db.product_metadata(&p.identity.key)?;
                            Ok((p, metadata))
                        })
                        .collect::<Result<Vec<_>, StorageError>>()?;
                    Ok((db.monitor_config()?, products))
                })
                .await
            else {
                break;
            };
            let network = (
                config.use_system_proxy,
                config.requests.connect_timeout,
                config.requests.total_timeout,
            );
            if previous_network != Some(network) {
                match ImageCache::new(
                    self.data_dir.join("images"),
                    network.0,
                    network.1,
                    network.2,
                ) {
                    Ok(initialized) => {
                        cache = Some(initialized);
                        previous_network = Some(network);
                        attempts.clear();
                        if let Some(error) = initialization_error.take() {
                            let cleared = self
                                .last_error
                                .lock()
                                .unwrap()
                                .take_if(|current| current == &error)
                                .is_some();
                            if cleared {
                                let _ = self.snapshot().await;
                            }
                        }
                    }
                    Err(error) => {
                        cache = None;
                        let message = format!("图片缓存初始化失败：{error}");
                        *self.last_error.lock().unwrap() = Some(message.clone());
                        initialization_error = Some(message);
                        let _ = self.snapshot().await;
                    }
                }
            }
            let Some(cache) = cache.as_mut() else {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            };
            let mut ids = products
                .iter()
                .filter_map(|(p, _)| p.identity.product_id.parse::<u64>().ok())
                .collect::<Vec<_>>();
            for &(id, _) in DEFAULT_PRODUCTS {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            attempts.retain(|id, _| ids.contains(id));
            self.image_paths
                .lock()
                .unwrap()
                .retain(|id, _| ids.contains(id));
            if let Err(error) = cache.prune(&ids) {
                #[cfg(debug_assertions)]
                eprintln!("图片缓存清理失败：{error}");
                #[cfg(not(debug_assertions))]
                let _ = error;
            }
            products.sort_by_key(|(p, _)| !p.enabled);
            for (product, metadata) in products {
                if self.stopping.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(id) = product.identity.product_id.parse::<u64>() else {
                    continue;
                };
                let Some(url) = metadata.and_then(|(m, _)| m.image_url) else {
                    continue;
                };
                if let Some(path) = cache.cached_path(id, &url) {
                    let changed = self
                        .image_paths
                        .lock()
                        .unwrap()
                        .insert(id, (url.clone(), path))
                        .is_none_or(|(old_url, _)| old_url != url);
                    if changed {
                        let _ = self.snapshot().await;
                    }
                    continue;
                }
                let now = std::time::Instant::now();
                if attempts
                    .get(&id)
                    .is_some_and(|(old, retry)| old == &url && *retry > now)
                {
                    continue;
                }
                match cache.cache(id, &url).await {
                    Ok(path) => {
                        let key = id.to_string();
                        let check_url = url.clone();
                        let valid = self
                            .call(move |db| {
                                Ok(db.product_configs()?.iter().any(|p| p.identity.key == key)
                                    && db.product_metadata(&key)?.is_some_and(|(m, _)| {
                                        m.image_url.as_deref() == Some(&check_url)
                                    }))
                            })
                            .await
                            .unwrap_or(false);
                        if valid {
                            self.image_paths.lock().unwrap().insert(id, (url, path));
                            let _ = self.snapshot().await;
                        } else {
                            let _ = cache.remove(id);
                        }
                        attempts.remove(&id);
                    }
                    Err(error) => {
                        let cooldown = if matches!(error, ImageCacheError::CacheFull { .. }) {
                            600
                        } else {
                            60
                        };
                        attempts.insert(id, (url, now + std::time::Duration::from_secs(cooldown)));
                        *self.last_error.lock().unwrap() =
                            Some(format!("商品 {id} 图片缓存失败：{error}"));
                        let _ = self.snapshot().await;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    pub async fn snapshot(&self) -> Result<AppSnapshot, AppError> {
        let timer = self.monitoring_time.clone();
        let image_paths = self.image_paths.clone();
        let image_root = self.data_dir.join("images");
        let notification_runtime = self.notification_runtime.clone();
        let (mut snapshot, persisted_checkpoint) = self
            .call(move |db| {
                let now = now_ms();
                let epoch = db.catalog_epoch()?;
                let configured = db.product_configs()?;
                let versions = configured
                    .iter()
                    .filter(|p| p.enabled)
                    .map(|p| {
                        Ok((
                            p.identity.key.clone(),
                            (db.product_generation(&p.identity.key)?, epoch),
                        ))
                    })
                    .collect::<Result<BTreeMap<_, _>, StorageError>>()?;
                let pending = {
                    let mut timer = timer.lock().unwrap();
                    timer.reconcile(versions);
                    timer.pending(std::time::Instant::now())
                };
                let monitor_config = db.monitor_config()?;
                let config = AppConfig::from_monitor(monitor_config.clone());
                let persisted_errors = db.monitoring_errors()?;
                let image_path = |id: &str, expected_url: Option<&str>| {
                    let id = id.parse::<u64>().ok()?;
                    let memory = image_paths.lock().unwrap().get(&id).cloned();
                    let (url, path) = memory
                        .filter(|(url, path)| {
                            path.is_file() && expected_url.is_none_or(|expected| expected == url)
                        })
                        .or_else(|| {
                            crate::image_cache::ImageCache::cached_image(&image_root, id)
                        })?;
                    expected_url
                        .is_none_or(|expected| expected == url)
                        .then(|| path.to_string_lossy().into_owned())
                };
                let mut products = db
                    .product_configs()?
                    .into_iter()
                    .map(|product| {
                        let product_id = product.identity.product_id;
                        let cached = db.product_metadata(&product.identity.key)?;
                        let stats = db.product_statistics(&product.identity.key, now)?;
                        let local_image = image_path(
                            &product_id,
                            cached.as_ref().and_then(|(m, _)| m.image_url.as_deref()),
                        );
                        let observation = db.observation(&product.identity.key)?.map(|state| {
                            ProductObservation {
                                availability: match state.state.availability {
                                    Availability::InStock => "in_stock",
                                    Availability::OutOfStock => "out_of_stock",
                                }
                                .into(),
                                is_show: state.state.is_show,
                                stock: state.state.stock,
                                checked_at: Some(format_timestamp(state.state.observed_at_ms)),
                            }
                        });
                        Ok(ProductRecord {
                            runtime_error: persisted_errors.get(&product_id).cloned(),
                            product_id,
                            name: product.name,
                            enabled: product.enabled,
                            prominent_alert: product.prominent_alert,
                            check_count: db.check_count(&product.identity.key)?,
                            observation,
                            image_path: local_image,
                            metadata: cached.as_ref().map(|(metadata, _)| metadata.clone()),
                            metadata_updated_at: cached.map(|(_, at)| format_timestamp(at)),
                            today_check_count: stats.today_check_count,
                            today_success_count: stats.today_success_count,
                            today_failure_count: stats.today_failure_count,
                            monitoring_ms: stats
                                .monitoring_ms
                                .saturating_add(
                                    pending.get(&product.identity.key).copied().unwrap_or(0),
                                )
                                .min(i64::MAX as u64),
                        })
                    })
                    .collect::<Result<Vec<_>, StorageError>>()?;
                for &(id, name) in DEFAULT_PRODUCTS {
                    let product_id = id.to_string();
                    if db.product_removed(&product_id)? {
                        continue;
                    }
                    if !products
                        .iter()
                        .any(|product| product.product_id == product_id)
                    {
                        let cached = db.product_metadata(&product_id)?;
                        let local_image = image_path(
                            &product_id,
                            cached.as_ref().and_then(|(m, _)| m.image_url.as_deref()),
                        );
                        products.push(ProductRecord {
                            product_id,
                            name: name.into(),
                            enabled: false,
                            prominent_alert: false,
                            check_count: 0,
                            observation: None,
                            runtime_error: None,
                            metadata: cached.as_ref().map(|(m, _)| m.clone()),
                            image_path: local_image,
                            metadata_updated_at: cached.map(|(_, at)| format_timestamp(at)),
                            today_check_count: 0,
                            today_success_count: 0,
                            today_failure_count: 0,
                            monitoring_ms: 0,
                        });
                    }
                }
                let recent_events = db
                    .recent_events(100)?
                    .into_iter()
                    .map(|event| {
                        let (kind, label) = match event.kind {
                            crate::storage::MonitorEventKind::FirstObservedInStock
                            | crate::storage::MonitorEventKind::OutOfStockToInStock => {
                                ("stock_available", "上架或补货")
                            }
                            crate::storage::MonitorEventKind::MonitoringFailed => {
                                ("monitoring_failed", "监控异常")
                            }
                            crate::storage::MonitorEventKind::StockIncreased => {
                                ("stock_increased", "补货")
                            }
                            crate::storage::MonitorEventKind::Recovered => {
                                ("recovered", "监控恢复")
                            }
                        };
                        RecentEvent {
                            id: event.id,
                            at: format_timestamp(event.observed_at_ms),
                            kind: kind.into(),
                            product_id: event.product.product_id.clone(),
                            message: format!("商品 {} {}", event.product.product_id, label),
                        }
                    })
                    .collect();
                let recent_checks = db
                    .recent_check_runs(100)?
                    .into_iter()
                    .map(|run| RecentCheck {
                        product_id: run.product.product_id,
                        name: run.name,
                        availability: match run.availability {
                            Availability::InStock => "in_stock",
                            Availability::OutOfStock => "out_of_stock",
                        }
                        .into(),
                        is_show: run.is_show,
                        stock: run.stock,
                        at: format_timestamp(run.first_at_ms),
                    })
                    .collect();
                let scan_active = db.scan_active()?;
                let persisted_checkpoint = db.scan_checkpoint()?;
                let scan = persisted_checkpoint
                    .clone()
                    .map(|checkpoint| {
                        let mut scan = ProductIdScan::restore(checkpoint);
                        if scan_active {
                            scan.continue_scan();
                        }
                        let error = db.scan_failure(scan.scan_id());
                        error.map(|error| {
                            scan_view(&Some(scan)).map(|mut view| {
                                if let Some(error) = error {
                                    view.status = ScanStatus::Failed;
                                    view.error = Some(error);
                                }
                                view
                            })
                        })
                    })
                    .transpose()?
                    .flatten();
                let intent = db.run_intent()?;
                let system_notifications_enabled = db.system_notifications_enabled()?;
                let system_notification_delivery =
                    db.system_notification_delivery()?
                        .map(|record| ChannelDelivery {
                            outcome: record.outcome,
                            event: record.event_kind,
                            at: format_timestamp(record.at_ms),
                            message: record.message,
                        });
                let stored_channels = db.notification_channels()?;
                let channels = stored_channels
                    .iter()
                    .map(|channel| {
                        let test =
                            db.notification_channel_test(&channel.id)?
                                .map(|record| ChannelTest {
                                    outcome: record.outcome,
                                    message: Some(record.message),
                                    recipients: Vec::new(),
                                });
                        let last_delivery =
                            db.notification_delivery(&channel.id)?
                                .map(|record| ChannelDelivery {
                                    outcome: record.outcome,
                                    event: record.event_kind,
                                    at: format_timestamp(record.at_ms),
                                    message: record.message,
                                });
                        let values: BTreeMap<String, String> =
                            match channel.credential_ref.as_deref() {
                                Some(reference) => db
                                    .credential("channel", reference)?
                                    .map(|stored| {
                                        serde_json::from_str::<BTreeMap<String, String>>(&stored)
                                    })
                                    .transpose()
                                    .map_err(StorageError::ConfigJson)?
                                    .unwrap_or_default(),
                                None => BTreeMap::new(),
                            };
                        let configured_field_keys = values
                            .keys()
                            .filter(|key| !key.starts_with('_'))
                            .cloned()
                            .collect();
                        let status = notification_runtime.account_status(&channel.id);
                        let routes = db.notification_routes(&channel.id)?;
                        let targets = merge_channel_targets(
                            channel_targets(&values),
                            routes.iter().map(|route| route.target.clone()).chain(
                                status
                                    .iter()
                                    .flat_map(|status| status.targets.iter().cloned()),
                            ),
                        );
                        Ok(ChannelView {
                            id: channel.id.clone(),
                            name: channel.name.clone(),
                            bot_name: channel_credentials(&values)["botName"]
                                .as_str()
                                .map(str::to_owned),
                            bot_url: channel_credentials(&values)["botUrl"]
                                .as_str()
                                .map(str::to_owned),
                            app_name: channel_credentials(&values)["appName"]
                                .as_str()
                                .map(str::to_owned),
                            provider_id: channel.provider.clone(),
                            provider_name: provider_name(&channel.provider).into(),
                            enabled: channel.enabled,
                            configured_field_keys,
                            subscriptions: channel
                                .subscriptions
                                .iter()
                                .filter_map(|name| channel_event(name))
                                .collect(),
                            last_test: test,
                            last_delivery,
                            connection_status: status
                                .map(|status| status.status)
                                .unwrap_or_else(|| "stopped".into()),
                            targets: targets.clone(),
                            target_id: values.get("targetId").cloned(),
                            target_kind: values.get("targetKind").cloned(),
                            selected_targets: routes
                                .iter()
                                .map(|route| {
                                    if !route.target.label.is_empty()
                                        && route.target.label != route.target.id
                                    {
                                        route.target.clone()
                                    } else {
                                        targets
                                            .iter()
                                            .find(|target| {
                                                target.id == route.target.id
                                                    && target.kind == route.target.kind
                                            })
                                            .cloned()
                                            .unwrap_or_else(|| route.target.clone())
                                    }
                                })
                                .collect(),
                            recipient_deliveries: routes
                                .into_iter()
                                .map(|route| RecipientDelivery {
                                    target: route.target,
                                    last_delivery: route.last_delivery.map(|record| {
                                        ChannelDelivery {
                                            outcome: record.outcome,
                                            event: record.event_kind,
                                            at: format_timestamp(record.at_ms),
                                            message: record.message,
                                        }
                                    }),
                                })
                                .collect(),
                        })
                    })
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let proxies = db
                    .proxies()?
                    .into_iter()
                    .map(|proxy| ProxyRecord {
                        id: proxy.id,
                        protocol: proxy_protocol(&proxy.protocol).unwrap_or(ProxyProtocol::Http),
                        display_address: format!("{}:{}", proxy.host, proxy.port),
                        enabled: proxy.enabled,
                        status: match proxy.status.as_str() {
                            "available" => ProxyStatus::Available,
                            "cooldown" => ProxyStatus::Cooldown,
                            "auto_disabled" => ProxyStatus::AutoDisabled,
                            "manually_disabled" => ProxyStatus::ManuallyDisabled,
                            _ => ProxyStatus::Untested,
                        },
                    })
                    .collect();
                let setup_completed = setup_completed(db)?;
                let now = std::time::SystemTime::now();
                let (in_schedule, until_transition) = schedule_state(&monitor_config.schedule, now);
                let outside_schedule =
                    intent == RunIntent::Running && setup_completed && !in_schedule;
                Ok((
                    AppSnapshot {
                        setup_completed,
                        system_notifications_enabled,
                        system_notification_delivery,
                        runtime: RuntimeSnapshot {
                            state: match (intent, setup_completed) {
                                (RunIntent::Running, false) => RuntimeState::SetupIncomplete,
                                (RunIntent::Stopped, _) => RuntimeState::Stopped,
                                (RunIntent::Paused, _) => RuntimeState::Paused,
                                (RunIntent::Running, true) if !in_schedule => {
                                    RuntimeState::OutsideSchedule
                                }
                                (RunIntent::Running, true) => RuntimeState::Monitoring,
                            },
                            next_start_at: outside_schedule.then(|| {
                                format_timestamp(
                                    (now + until_transition)
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .expect("下一次计划开始时间晚于 Unix 纪元")
                                        .as_millis() as i64,
                                )
                            }),
                            last_success_at: None,
                            last_error: None,
                        },
                        config,
                        catalog: products
                            .iter()
                            .filter(|product| !product.enabled)
                            .cloned()
                            .collect(),
                        products: products
                            .into_iter()
                            .filter(|product| product.enabled)
                            .collect(),
                        channels,
                        providers: provider_definitions(),
                        proxies,
                        scan,
                        recent_events,
                        recent_checks,
                    },
                    persisted_checkpoint,
                ))
            })
            .await?;
        if let Ok(errors) = self.runtime_errors.lock() {
            for product in snapshot.products.iter_mut() {
                product.runtime_error = errors.get(&product.product_id).cloned();
            }
        }
        let live = self.scan.lock().await;
        if let Some(scan) = live.as_ref() {
            if snapshot
                .scan
                .as_ref()
                .is_none_or(|view| view.error.is_none())
                && ((scan.current_id().is_some()
                    && scan.checkpoint().as_ref() == persisted_checkpoint.as_ref())
                    || (persisted_checkpoint.is_none()
                        && scan.status() == CoreScanStatus::Cancelled))
            {
                snapshot.scan = scan_view(&Some(scan.clone()));
            }
        }
        drop(live);
        snapshot.runtime.last_success_at = snapshot
            .products
            .iter()
            .filter_map(|product| product.observation.as_ref()?.checked_at.as_ref())
            .max()
            .cloned();
        snapshot.runtime.last_error = self.last_error.lock().ok().and_then(|value| value.clone());
        if snapshot.runtime.state == RuntimeState::Monitoring
            && snapshot
                .products
                .iter()
                .any(|product| product.runtime_error.is_some())
        {
            snapshot.runtime.state = RuntimeState::PartialError;
        }
        if let Some(error) = self.worker_error.lock().unwrap().clone() {
            snapshot.runtime.state = RuntimeState::WorkerFailed;
            snapshot.runtime.next_start_at = None;
            snapshot.runtime.last_error = Some(error);
        }
        *self.latest_snapshot.lock().unwrap() = Some(snapshot.clone());
        let _ = self.snapshots.send(snapshot.clone());
        Ok(snapshot)
    }

    pub async fn save_config(&self, config: AppConfig) -> Result<(), AppError> {
        let monitor = config.into_monitor()?;
        self.settle_monitoring_time(true).await?;
        let gate_requests = monitor.requests.clone();
        let scan_interval = monitor.scan.interval;
        let mut scan = self.scan.lock().await;
        let previous = self.call(|db| db.monitor_config()).await?;
        let network_changed = previous.use_system_proxy != monitor.use_system_proxy
            || previous.use_proxy_pool != monitor.use_proxy_pool
            || previous.requests.connect_timeout != monitor.requests.connect_timeout
            || previous.requests.total_timeout != monitor.requests.total_timeout;
        let request_gate = self.request_gate.clone();
        self.call(move |db| {
            db.save_monitor_config(&monitor)?;
            request_gate.set_monitor_schedule(&monitor.schedule);
            Ok(())
        })
        .await?;
        if network_changed {
            let (checkpoint, active) = self
                .call(|db| Ok((db.scan_checkpoint()?, db.scan_active()?)))
                .await?;
            *scan = checkpoint.map(ProductIdScan::restore);
            if active {
                if let Some(scan) = scan.as_mut() {
                    scan.continue_scan();
                }
            }
            self.scan_epoch.send_modify(|epoch| *epoch += 1);
        }
        drop(scan);
        self.request_gate.reconfigure(&gate_requests, scan_interval);
        self.snapshot().await?;
        Ok(())
    }

    pub async fn set_auto_start_monitoring(&self, enabled: bool) -> Result<(), AppError> {
        self.call(move |db| {
            let mut config = db.monitor_config()?;
            config.auto_start_monitoring = enabled;
            db.save_monitor_config(&config)
        })
        .await?;
        self.snapshot().await?;
        Ok(())
    }

    pub async fn set_system_notifications_enabled(&self, enabled: bool) -> Result<(), AppError> {
        self.call(move |db| db.set_system_notifications_enabled(enabled))
            .await?;
        self.snapshot().await?;
        Ok(())
    }

    pub async fn validate_product(&self, product_id: u64) -> Result<ProductRecord, AppError> {
        Ok(product_record(
            &self.fetch_product_detail(product_id).await?,
            false,
        ))
    }

    async fn fetch_product_detail(&self, product_id: u64) -> Result<ProductDetail, AppError> {
        let config = self.call(|db| db.monitor_config()).await?;
        let route = if config.use_proxy_pool {
            Some(
                self.call(|db| {
                    Ok(db
                        .proxies()?
                        .into_iter()
                        .find(|proxy| proxy.enabled && proxy.status == "available"))
                })
                .await?
                .ok_or_else(|| AppError::Network("代理池没有可用出口".into()))?,
            )
        } else {
            None
        };
        let route_id = route
            .as_ref()
            .map(|proxy| proxy.id.as_str())
            .unwrap_or("direct");
        let (_control, mut receiver) = ControlToken::new();
        let _permit = self
            .request_gate
            .acquire_scan(product_id, &mut receiver)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        let result = match &self.client_override {
            Some(client) => {
                client
                    .product_detail(LineId::new(product_id, route_id))
                    .await
            }
            None => {
                if let Some(proxy) = route.as_ref() {
                    RicohSchedulerClient::with_proxy(&config, self.proxy_route(proxy).await?)
                        .map_err(|error| AppError::Network(error.to_string()))?
                        .product_detail(LineId::new(product_id, route_id))
                        .await
                } else {
                    RicohApi::with_options(
                        config.requests.connect_timeout,
                        config.requests.total_timeout,
                        config.use_system_proxy,
                    )
                    .map_err(|error| AppError::Network(error.to_string()))?
                    .product_detail(product_id)
                    .await
                }
            }
        };
        if let Some(proxy) = route.as_ref() {
            match &result {
                Ok(_) => {
                    let id = proxy.id.clone();
                    self.call(move |db| db.record_proxy_success(&id)).await?;
                }
                Err(crate::ricoh_api::RicohApiError::Transport(_)) => {
                    let id = proxy.id.clone();
                    self.call(move |db| db.record_proxy_failure(&id)).await?;
                }
                Err(_) => {}
            }
        }
        if let Err(crate::ricoh_api::RicohApiError::RateLimited { retry_after }) = &result {
            self.request_gate.cool_domain(retry_after.as_ref()).await;
        }
        #[cfg(debug_assertions)]
        if let Err(error) = &result {
            eprintln!("商品 {product_id} 检查失败：{error}");
        }
        let detail = result.map_err(|error| AppError::Network(error.to_string()))?;
        Ok(detail)
    }

    pub async fn add_product(&self, product_id: u64) -> Result<ProductRecord, AppError> {
        let read = {
            let mut pending = self.pending_products.lock().unwrap();
            if pending.contains_key(&product_id)
                || pending.len() >= crate::storage::MAX_CATALOG_PRODUCTS
            {
                return Err(AppError::InvalidInput("商品正在验证，请等待完成".into()));
            }
            let cancelled = Arc::new(AtomicBool::new(false));
            pending.insert(product_id, cancelled.clone());
            ProductRead {
                id: product_id,
                cancelled,
                pending: self.pending_products.clone(),
            }
        };
        let (expected_generation, persistent_epoch) = self
            .call(move |db| {
                Ok((
                    db.product_generation(&product_id.to_string())?,
                    db.catalog_epoch()?,
                ))
            })
            .await?;
        let epoch = self.catalog_epoch.load(Ordering::Acquire);
        let detail = self.fetch_product_detail(product_id).await?;
        let generations = self.product_generations.clone();
        let app = self.clone();
        let cancelled = read.cancelled.clone();
        self.call(move |db| {
            if cancelled.load(Ordering::Acquire)
                || app.catalog_epoch.load(Ordering::Acquire) != epoch
                || !db.commit_verified_product(
                    &detail,
                    expected_generation,
                    persistent_epoch,
                    true,
                    None,
                    now_ms(),
                )?
            {
                return Ok(false);
            }
            let mut generations = generations.lock().unwrap();
            generations.insert(product_id, db.product_generation(&product_id.to_string())?);
            app.forget_product_error(product_id);
            Ok(true)
        })
        .await?
        .then_some(())
        .ok_or_else(|| AppError::InvalidInput("商品选择已经改变，请重新添加".into()))?;
        self.snapshot()
            .await?
            .products
            .into_iter()
            .find(|p| p.product_id == product_id.to_string())
            .ok_or_else(|| AppError::InvalidInput("商品已从目录移除".into()))
    }

    pub async fn set_product_enabled(
        &self,
        product_id: u64,
        enabled: bool,
    ) -> Result<(), AppError> {
        if !enabled {
            if let Some(cancelled) = self.pending_products.lock().unwrap().get(&product_id) {
                cancelled.store(true, Ordering::Release);
            }
        }
        self.settle_monitoring_time(false).await?;
        if enabled && DEFAULT_PRODUCTS.iter().any(|&(id, _)| id == product_id) {
            let needs_validation = self
                .call(move |db| {
                    Ok(!db.product_configs()?.iter().any(|product| {
                        product.identity.product_id == product_id.to_string()
                            && product.verified_at_ms.is_some()
                    }))
                })
                .await?;
            if needs_validation {
                self.add_product(product_id).await?;
                return Ok(());
            }
        }
        let key = product_id.to_string();
        let generations = self.product_generations.clone();
        let app = self.clone();
        self.call(move |db| {
            db.set_product_enabled(&key, enabled)?;
            app.monitoring_time.lock().unwrap().forget(&key);
            let mut generations = generations.lock().unwrap();
            generations.insert(product_id, db.product_generation(&key)?);
            app.forget_product_error(product_id);
            Ok(())
        })
        .await?;
        self.snapshot().await?;
        Ok(())
    }

    pub async fn remove_product(&self, product_id: u64) -> Result<(), AppError> {
        if let Some(cancelled) = self.pending_products.lock().unwrap().get(&product_id) {
            cancelled.store(true, Ordering::Release);
        }
        self.settle_monitoring_time(false).await?;
        let key = product_id.to_string();
        let generations = self.product_generations.clone();
        let app = self.clone();
        self.call(move |db| {
            db.remove_product(&key)?;
            app.monitoring_time.lock().unwrap().forget(&key);
            let mut generations = generations.lock().unwrap();
            generations.insert(product_id, db.product_generation(&key)?);
            app.forget_product_error(product_id);
            Ok(())
        })
        .await?;
        self.snapshot().await?;
        Ok(())
    }

    fn forget_product_error(&self, product_id: u64) {
        if let Ok(mut errors) = self.runtime_errors.lock() {
            if let Some(removed) = errors.remove(&product_id.to_string()) {
                if let Ok(mut last_error) = self.last_error.lock() {
                    if last_error.as_deref() == Some(removed.as_str()) {
                        *last_error = errors.values().next().cloned();
                    }
                }
            }
        }
    }

    async fn configure_notification_runtime(
        &self,
        force_channel: Option<&str>,
    ) -> Result<(), AppError> {
        let _configuration = self.notification_configuration.lock().await;
        self.persist_notification_credentials().await?;
        let force_channel = force_channel.map(str::to_owned);
        let force = force_channel.is_some();
        let testing = self.notification_test_accounts.lock().unwrap().clone();
        let editing = self
            .notification_edit_accounts
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let proxy_url = self.notification_proxy_url.lock().unwrap().clone();
        let configuration = self.call(move |db| {
            let proxy_url = match (db.monitor_config()?.use_system_proxy, proxy_url) {
                (true, Ok(proxy)) => proxy,
                (true, Err(error)) => return Ok(Err(error)),
                (false, _) => None,
            };
            let mut accounts = Vec::new();
            for channel in db.notification_channels()? {
                if channel.provider == "system" { continue; }
                let Some(reference) = channel.credential_ref.as_deref() else { continue; };
                let Some(saved) = db.credential("channel", reference)? else { continue; };
                let Ok(values) = serde_json::from_str::<BTreeMap<String,String>>(&saved) else { continue; };
                let credentials = channel_credentials(&values);
                let targets = merge_channel_targets(channel_targets(&values),
                    db.notification_routes(&channel.id)?.into_iter().map(|route| route.target));
                accounts.push(serde_json::json!({
                    "id":channel.id,"provider":channel.provider,"credentials":credentials,
                    "targets":targets,"enabled":channel.enabled || testing.contains(&channel.id) || editing.contains(&channel.id) || force_channel.as_ref() == Some(&channel.id)
                }));
            }
            let system_proxy = proxy_url.is_some();
            Ok(Ok(serde_json::json!({"accounts":accounts,"network":if system_proxy {"system_proxy"} else {"direct"},"proxyUrl":proxy_url})))
        }).await?.map_err(AppError::Network)?;
        self.notification_runtime
            .configure(configuration, force)
            .await
            .map_err(AppError::Network)
    }

    async fn persist_notification_credentials(&self) -> Result<(), AppError> {
        let updates = self.notification_runtime.take_credential_updates();
        if updates.is_empty() {
            return Ok(());
        }
        self.call(move |db| {
            for (account_id, update) in updates {
                let channel = db
                    .notification_channels()?
                    .into_iter()
                    .find(|channel| channel.id == account_id);
                let Some(channel) = channel else {
                    if let Some(saved) = db.credential("channel", &account_id)? {
                        let mut binding: serde_json::Value =
                            serde_json::from_str(&saved).map_err(StorageError::ConfigJson)?;
                        crate::notifications::merge_credentials(
                            &mut binding["credentials"],
                            &update,
                        );
                        db.save_credential("channel", &account_id, &binding.to_string())?;
                    }
                    continue;
                };
                let Some(reference) = channel.credential_ref else {
                    continue;
                };
                let Some(saved) = db.credential("channel", &reference)? else {
                    continue;
                };
                let mut values: BTreeMap<String, String> =
                    serde_json::from_str(&saved).map_err(StorageError::ConfigJson)?;
                let mut credentials = channel_credentials(&values);
                crate::notifications::merge_credentials(&mut credentials, &update);
                values.insert("_sdkCredentials".into(), credentials.to_string());
                if update.get("targetAliases").is_some() {
                    let routes = db.notification_routes(&account_id)?;
                    let selected = merge_channel_targets(
                        Vec::new(),
                        routes
                            .into_iter()
                            .map(|route| canonical_channel_target(&values, route.target)),
                    );
                    db.save_notification_routes(&account_id, &selected)?;
                    let known = channel_targets(&values);
                    values.insert(
                        "_targets".into(),
                        serde_json::to_string(&known).map_err(StorageError::ConfigJson)?,
                    );
                }
                db.save_credential(
                    "channel",
                    &reference,
                    &serde_json::to_string(&values).map_err(StorageError::ConfigJson)?,
                )?;
            }
            Ok(())
        })
        .await
    }

    pub async fn begin_channel_binding(
        &self,
        provider: &str,
    ) -> Result<crate::notifications::ChannelBinding, AppError> {
        self.begin_channel_binding_for_app(provider, None).await
    }

    pub async fn begin_channel_rebinding(
        &self,
        id: &str,
    ) -> Result<crate::notifications::ChannelBinding, AppError> {
        let id = id.to_owned();
        let (provider, app_id) = self
            .call(move |db| {
                let channel = db
                    .notification_channels()?
                    .into_iter()
                    .find(|channel| channel.id == id)
                    .ok_or(StorageError::ChannelMissing)?;
                if channel.provider != "feishu" {
                    return Ok((channel.provider, None));
                }
                let saved = channel
                    .credential_ref
                    .as_deref()
                    .map(|reference| db.credential("channel", reference))
                    .transpose()?
                    .flatten();
                let Some(saved) = saved else {
                    return Ok((channel.provider, None));
                };
                let values: BTreeMap<String, String> =
                    serde_json::from_str(&saved).map_err(StorageError::ConfigJson)?;
                Ok((
                    channel.provider,
                    channel_credentials(&values)["appId"]
                        .as_str()
                        .map(str::to_owned),
                ))
            })
            .await?;
        self.begin_channel_binding_for_app(&provider, app_id).await
    }

    async fn begin_channel_binding_for_app(
        &self,
        provider: &str,
        app_id: Option<String>,
    ) -> Result<crate::notifications::ChannelBinding, AppError> {
        let _binding_operation = self.notification_binding_operations.lock().await;
        if !provider_definitions()
            .iter()
            .any(|p| p.id == provider && p.supports_binding)
        {
            return Err(AppError::InvalidInput("此平台请使用应用凭据连接".into()));
        }
        let id = format!(
            "binding-{}-{}",
            now_ms(),
            BINDING_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        // SDK 返回之前，首次绑定也持有通知进程，避免后台将它视为空闲。
        self.notification_runtime
            .remember_binding(serde_json::json!({
                "id":id, "provider":provider, "status":"waiting", "targets":[]
            }));
        let result = async {
            self.configure_notification_runtime(Some("")).await?;
            let value = self
                .notification_runtime
                .request(
                    "begin_binding",
                    serde_json::json!({"provider":provider,"bindingId":id,"appId":app_id}),
                )
                .await
                .map_err(AppError::Network)?;
            self.persist_binding(value).await
        }
        .await;
        if result.is_err() {
            self.notification_runtime.forget_binding(&id);
        }
        result
    }

    pub async fn detect_channel_groups(
        &self,
        id: &str,
    ) -> Result<Vec<NotificationTarget>, AppError> {
        let generation = self.notification_runtime.account_generation(id);
        if !self
            .notification_test_accounts
            .lock()
            .unwrap()
            .insert(id.to_owned())
        {
            return Err(AppError::InvalidInput(
                "此渠道正在连接或测试，请等待结果".into(),
            ));
        }
        let lease = NotificationTestLease {
            id: id.to_owned(),
            active: self.notification_test_accounts.clone(),
        };
        let result = async {
            self.configure_notification_runtime(Some(id)).await?;
            self.wait_notification_account_ready(id, generation)
                .await
                .map_err(AppError::Network)?;
            let value = self
                .notification_runtime
                .request("detect_groups", serde_json::json!({"accountId":id}))
                .await
                .map_err(AppError::Network)?;
            let targets = read_detected_targets(value)?;
            let account_id = id.to_owned();
            let targets_for_db = targets.clone();
            self.call(move |db| {
                let channel = db
                    .notification_channels()?
                    .into_iter()
                    .find(|channel| channel.id == account_id)
                    .ok_or(StorageError::ChannelMissing)?;
                let reference = channel.credential_ref.ok_or(StorageError::InvalidChannel)?;
                let packed = db
                    .credential("channel", &reference)?
                    .ok_or(StorageError::InvalidChannel)?;
                let mut values: BTreeMap<String, String> =
                    serde_json::from_str(&packed).map_err(StorageError::ConfigJson)?;
                values.insert(
                    "_targets".into(),
                    serde_json::to_string(&targets_for_db).map_err(StorageError::ConfigJson)?,
                );
                db.save_credential(
                    "channel",
                    &reference,
                    &serde_json::to_string(&values).map_err(StorageError::ConfigJson)?,
                )
            })
            .await?;
            Ok(targets)
        }
        .await;
        drop(lease);
        let _ = self.configure_notification_runtime(None).await;
        self.snapshot().await?;
        result
    }

    pub async fn detect_binding_groups(
        &self,
        id: &str,
    ) -> Result<Vec<NotificationTarget>, AppError> {
        let _binding_operation = self.notification_binding_operations.lock().await;
        let value = self
            .notification_runtime
            .request("detect_binding_groups", serde_json::json!({"bindingId":id}))
            .await
            .map_err(AppError::Network)?;
        let targets = read_detected_targets(value)?;
        let binding = self
            .notification_runtime
            .request("binding_status", serde_json::json!({"bindingId":id}))
            .await
            .map_err(AppError::Network)?;
        self.persist_binding(binding).await?;
        Ok(targets)
    }

    pub async fn channel_binding_status(
        &self,
        id: &str,
    ) -> Result<crate::notifications::ChannelBinding, AppError> {
        let _binding_operation = self.notification_binding_operations.lock().await;
        if let Some(binding) = self.notification_runtime.binding(id).filter(|binding| {
            matches!(
                binding["status"].as_str(),
                Some("failed" | "expired" | "cancelled")
            )
        }) {
            return self.persist_binding(binding).await;
        }
        let value = self
            .notification_runtime
            .request("binding_status", serde_json::json!({"bindingId":id}))
            .await
            .map_err(AppError::Network)?;
        self.persist_binding(value).await
    }

    pub async fn submit_channel_binding_verification(
        &self,
        id: &str,
        code: &str,
    ) -> Result<crate::notifications::ChannelBinding, AppError> {
        let _binding_operation = self.notification_binding_operations.lock().await;
        let value = self
            .notification_runtime
            .request(
                "submit_binding_verification",
                serde_json::json!({"bindingId":id,"code":code}),
            )
            .await
            .map_err(AppError::Network)?;
        self.persist_binding(value).await
    }

    async fn persist_binding(
        &self,
        mut value: serde_json::Value,
    ) -> Result<crate::notifications::ChannelBinding, AppError> {
        if value["credentials"].is_object() {
            if let Some(credentials) = self
                .notification_runtime
                .binding(value["id"].as_str().unwrap_or_default())
                .and_then(|binding| binding.get("credentials").cloned())
            {
                crate::notifications::merge_credentials(&mut value["credentials"], &credentials);
            }
        }
        let mut public: crate::notifications::ChannelBinding =
            serde_json::from_value(value.clone())
                .map_err(|_| AppError::Network("通知绑定响应格式无效".into()))?;
        if public.provider == "weixin" && public.status == "complete" {
            public.private_message_received = Some({
                let credentials = &value["credentials"];
                credentials["userId"]
                    .as_str()
                    .and_then(|id| credentials["contextTokens"][id].as_str())
                    .is_some_and(|context| !context.is_empty())
            });
        }
        if public.status == "complete" {
            public.private_chat_ready = Some(public.private_message_received == Some(true)
                || public.provider == "feishu" && value["credentials"]["userOpenId"].as_str()
                    .and_then(|id| value["credentials"]["p2pChatIds"][id].as_str())
                    .is_some_and(|id| !id.is_empty()));
        }
        if public.status == "complete" {
            if let Some(credentials) = value.get("credentials").filter(|value| value.is_object()) {
                let id = public.id.clone();
                let packed = serde_json::to_string(&serde_json::json!({"provider":public.provider,"credentials":credentials,"targets":public.targets})).map_err(|_| AppError::Credential("绑定凭据格式无效".into()))?;
                self.call(move |db| db.save_credential("channel", &id, &packed))
                    .await?;
            }
        }
        public.message = public.message.and_then(|message| {
            let message = match (public.status.as_str(), message.as_str()) {
                ("complete", "dingtalk_missing_staff_id") => "已收到私信，钉钉未提供可用于通知的员工 ID。请使用机器人所属组织的账号发送私信，或在群内 @机器人后选择群聊。",
                ("failed", _) => "账户连接失败，请检查平台授权后重试",
                ("expired", _) => "二维码已过期，请重新开始连接",
                ("complete", _) => return None,
                _ => "等待平台授权",
            };
            Some(message.into())
        });
        self.notification_runtime.remember_binding(value);
        Ok(public)
    }

    pub async fn cancel_channel_binding(&self, id: &str) -> Result<(), AppError> {
        let _binding_operation = self.notification_binding_operations.lock().await;
        let cancellation = self
            .notification_runtime
            .request("cancel_binding", serde_json::json!({"bindingId":id}))
            .await;
        self.notification_runtime.forget_binding(id);
        let id = id.to_owned();
        self.call(move |db| db.delete_credential("channel", &id))
            .await?;
        let _ = self.configure_notification_runtime(None).await;
        cancellation.map_err(AppError::Network)?;
        Ok(())
    }

    pub async fn save_channel(&self, input: ChannelInput) -> Result<ChannelView, AppError> {
        let _binding_operation = if input.binding_id.is_some() {
            Some(self.notification_binding_operations.lock().await)
        } else {
            None
        };
        let configuration_operation = self.notification_configuration.lock().await;
        let id = input.id.unwrap_or_else(|| format!("channel-{}", now_ms()));
        let provider = input.provider_id.clone();
        let definition = provider_definitions()
            .into_iter()
            .find(|p| p.id == provider)
            .ok_or_else(|| AppError::InvalidInput("通知渠道类型无效".into()))?;
        let old_values = self
            .call({
                let id = id.clone();
                let provider = provider.clone();
                move |db| {
                    let channel = db
                        .notification_channels()?
                        .into_iter()
                        .find(|c| c.id == id && c.provider == provider);
                    channel
                        .and_then(|c| c.credential_ref)
                        .map(|reference| db.credential("channel", &reference))
                        .transpose()
                        .map(Option::flatten)
                }
            })
            .await?;
        let mut values: BTreeMap<String, String> = old_values
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|_| AppError::Credential("渠道凭据格式无效".into()))?
            .unwrap_or_default();
        let previous_credentials = channel_credentials(&values);
        if let Some(binding_id) = input.binding_id.as_deref() {
            let reference = binding_id.to_owned();
            let packed = self
                .call(move |db| db.credential("channel", &reference))
                .await?
                .ok_or_else(|| AppError::Credential("绑定尚未完成，请完成平台授权".into()))?;
            let binding: serde_json::Value = serde_json::from_str(&packed)
                .map_err(|_| AppError::Credential("绑定凭据格式无效".into()))?;
            if binding["provider"].as_str() != Some(provider.as_str()) {
                return Err(AppError::InvalidInput("绑定平台与通知渠道不一致".into()));
            }
            values.retain(|key, _| key.starts_with("target"));
            values.insert("_sdkCredentials".into(), binding["credentials"].to_string());
            values.insert("_targets".into(), binding["targets"].to_string());
        }
        let manual_target =
            input.values.contains_key("targetId") || input.values.contains_key("targetKind");
        for (key, value) in input.values {
            if !definition.fields.iter().any(|field| field.key == key) {
                return Err(AppError::InvalidInput("通知渠道字段无效".into()));
            }
            if !value.trim().is_empty() {
                values.insert(key, value.trim().to_owned());
            }
        }
        if !values.contains_key("_sdkCredentials") {
            for field in definition
                .fields
                .iter()
                .filter(|field| field.required && !field.key.starts_with("target"))
            {
                if values.get(&field.key).is_none_or(|value| value.is_empty()) {
                    return Err(AppError::InvalidInput(format!("请填写{}", field.label)));
                }
            }
        }
        let mut selected_targets = if let Some(targets) = input.targets {
            targets
        } else if manual_target {
            selected_target(&values).into_iter().collect()
        } else {
            let account_id = id.clone();
            self.call(move |db| {
                Ok(db
                    .notification_routes(&account_id)?
                    .into_iter()
                    .map(|route| route.target)
                    .collect::<Vec<_>>())
            })
            .await?
        };
        selected_targets.sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
        selected_targets.dedup_by(|a, b| a.id == b.id && a.kind == b.kind);
        if selected_targets.is_empty()
            || selected_targets
                .iter()
                .any(|target| target.id.trim().is_empty() || target.kind.trim().is_empty())
        {
            return Err(AppError::InvalidInput("请选择或填写接收会话".into()));
        }
        values.insert("targetId".into(), selected_targets[0].id.clone());
        values.insert("targetKind".into(), selected_targets[0].kind.clone());
        let subscriptions = input
            .subscriptions
            .iter()
            .map(channel_event_name)
            .collect::<Vec<_>>();
        let id_for_db = id.clone();
        let name = input.name;
        let credentials_changed = old_values.is_none()
            || input.binding_id.is_some()
            || previous_credentials != channel_credentials(&values);
        if credentials_changed && old_values.is_some() {
            self.notification_runtime.quiesce_account(&id).await.map_err(AppError::Network)?;
        }
        let notification_runtime = self.notification_runtime.clone();
        self.call(move |db| {
            if !credentials_changed {
                let packed = db
                    .credential("channel", &id_for_db)?
                    .ok_or(StorageError::InvalidChannel)?;
                let current_values: BTreeMap<String, String> =
                    serde_json::from_str(&packed).map_err(StorageError::ConfigJson)?;
                selected_targets = merge_channel_targets(
                    Vec::new(),
                    selected_targets
                        .into_iter()
                        .map(|target| canonical_channel_target(&current_values, target)),
                );
                values = current_values;
                values.insert("targetId".into(), selected_targets[0].id.clone());
                values.insert("targetKind".into(), selected_targets[0].kind.clone());
            }
            let previous_routes = db.notification_routes(&id_for_db)?;
            let destinations_changed = previous_routes.len() != selected_targets.len()
                || previous_routes.iter().any(|route| {
                    !selected_targets.iter().any(|target| {
                        target.kind == route.target.kind && target.id == route.target.id
                    })
                });
            if credentials_changed || destinations_changed {
                notification_runtime.cancel_account(&id_for_db);
            }
            let packed = serde_json::to_string(&values).map_err(StorageError::ConfigJson)?;
            db.save_credential("channel", &id_for_db, &packed)?;
            if credentials_changed {
                db.save_notification_channel(
                    &id_for_db,
                    &name,
                    &provider,
                    Some(&id_for_db),
                    &subscriptions,
                )?;
            } else {
                db.update_notification_channel_details(&id_for_db, &name, &subscriptions)?;
            }
            db.save_notification_routes(&id_for_db, &selected_targets)?;
            Ok(())
        })
        .await?;
        if let Some(binding_id) = input.binding_id {
            self.notification_runtime.forget_binding(&binding_id);
            self.call(move |db| db.delete_credential("channel", &binding_id))
                .await?;
        }
        drop(configuration_operation);
        let _ = self.configure_notification_runtime(None).await;
        self.snapshot()
            .await?
            .channels
            .into_iter()
            .find(|channel| channel.id == id)
            .ok_or_else(|| AppError::NotFound("通知渠道不存在".into()))
    }

    pub async fn test_channel(&self, id: &str) -> Result<ChannelTest, AppError> {
        let generation = self.notification_runtime.account_generation(id);
        if !self
            .notification_test_accounts
            .lock()
            .unwrap()
            .insert(id.to_owned())
        {
            return Err(AppError::InvalidInput("此渠道正在测试，请等待结果".into()));
        }
        let lease = NotificationTestLease {
            id: id.to_owned(),
            active: self.notification_test_accounts.clone(),
        };
        let id_owned = id.to_owned();
        let (saved, mut targets) = self
            .call(move |db| {
                db.reserve_notification_test(&id_owned, now_ms())?;
                let channel = db
                    .notification_channels()?
                    .into_iter()
                    .find(|c| c.id == id_owned)
                    .ok_or(StorageError::ChannelMissing)?;
                let saved = channel
                    .credential_ref
                    .as_deref()
                    .map(|reference| db.credential("channel", reference))
                    .transpose()?
                    .flatten();
                let targets = db
                    .notification_routes(&id_owned)?
                    .into_iter()
                    .map(|route| route.target)
                    .collect::<Vec<_>>();
                Ok((saved, targets))
            })
            .await?;
        let mut recipients = Vec::new();
        let readiness = if let Some(saved) = saved {
            if targets.is_empty() {
                let values: BTreeMap<String, String> = serde_json::from_str(&saved)
                    .map_err(|_| AppError::Credential("渠道凭据格式无效".into()))?;
                targets.extend(selected_target(&values));
            }
            if targets.is_empty() {
                Err("请填写接收会话".to_owned())
            } else {
                match self.configure_notification_runtime(Some(id)).await {
                    Ok(()) => self.wait_notification_account_ready(id, generation).await,
                    Err(error) => Err(error.to_string()),
                }
            }
        } else {
            Err("请填写通知渠道凭据".to_owned())
        };
        if readiness.is_ok() {
            for target in targets {
                let result = self
                    .notification_runtime
                    .send_at_generation(
                        id,
                        &target,
                        "理光相机库存监控测试消息",
                        generation,
                        || Ok(true),
                    )
                    .await;
                recipients.push(RecipientTest {
                    target,
                    outcome: if result.outcome == "deferred" { "failed".into() } else { result.outcome },
                    message: result.message,
                });
            }
        }
        let accepted = recipients
            .iter()
            .filter(|result| result.outcome == "accepted")
            .count();
        let outcome = if !recipients.is_empty() && accepted == recipients.len() {
            "accepted"
        } else if recipients.iter().any(|result| result.outcome == "unknown") {
            "unknown"
        } else {
            "failed"
        };
        let message = readiness.err().unwrap_or_else(|| {
            let summary = format!(
                "平台已接受 {accepted}/{} 个接收会话的测试消息",
                recipients.len()
            );
            let failed = recipients
                .iter()
                .filter(|recipient| recipient.outcome != "accepted")
                .map(|recipient| {
                    let name = if recipient.target.label.is_empty() {
                        &recipient.target.id
                    } else {
                        &recipient.target.label
                    };
                    format!("{name}：{}", recipient.message)
                })
                .collect::<Vec<_>>();
            if failed.is_empty() {
                summary
            } else {
                format!("{summary}。{}", failed.join("；"))
            }
        });
        let mut result = ChannelTest {
            outcome: outcome.into(),
            message: Some(message.clone()),
            recipients,
        };
        let result_copy = result.clone();
        let id_owned = id.to_owned();
        let notification_runtime = self.notification_runtime.clone();
        self.call(move |db| {
            if notification_runtime.account_generation(&id_owned) != generation {
                return Ok(());
            }
            for recipient in &result_copy.recipients {
                db.record_recipient_delivery(
                    &id_owned,
                    &recipient.target,
                    &recipient.outcome,
                    "test",
                    now_ms(),
                    &recipient.message,
                )?;
            }
            db.record_notification_test(&id_owned, &result_copy.outcome, now_ms(), &message, None)
        })
        .await?;
        if self.notification_runtime.account_generation(id) != generation {
            result.outcome = "failed".into();
            result.message = Some("通知渠道已修改，请重新测试".into());
        }
        drop(lease);
        let _ = self.configure_notification_runtime(None).await;
        self.snapshot().await?;
        Ok(result)
    }

    async fn wait_notification_account_ready(
        &self,
        id: &str,
        generation: u64,
    ) -> Result<(), String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if self.stopping.load(Ordering::Relaxed) {
                return Err("通知服务已关闭".into());
            }
            if self.notification_runtime.account_generation(id) != generation {
                return Err("渠道已停用".into());
            }
            if let Some(account) = self.notification_runtime.account_status(id) {
                match account.status.as_str() {
                    "ready" => return Ok(()),
                    "auth_required" => return Err("账户授权已失效，请重新连接".into()),
                    "failed" => return Err("账户连接失败，请检查授权与网络".into()),
                    _ => {}
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err("账户连接超时，请检查网络后重试".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    pub async fn set_channel_enabled(&self, id: &str, enabled: bool) -> Result<(), AppError> {
        let id = id.to_owned();
        self.call({
            let id = id.clone();
            move |db| db.set_notification_channel_enabled(&id, enabled)
        })
        .await?;
        if !enabled {
            if self
                .notification_edit_accounts
                .lock()
                .unwrap()
                .contains_key(&id)
            {
                self.configure_notification_runtime(None).await?;
            } else {
                self.notification_runtime.cancel_account(&id);
            }
        }
        self.snapshot().await?;
        Ok(())
    }

    pub async fn remove_channel(&self, id: &str) -> Result<(), AppError> {
        let _edit_operation = self.notification_edit_operations.lock().await;
        let id = id.to_owned();
        let id_for_db = id.clone();
        self.call(move |db| {
            db.delete_notification_channel(&id_for_db)?;
            Ok(())
        })
        .await?;
        self.notification_edit_accounts.lock().unwrap().remove(&id);
        self.notification_runtime.cancel_account(&id);
        let _ = self.configure_notification_runtime(None).await;
        self.snapshot().await?;
        Ok(())
    }

    pub async fn add_proxy(&self, input: ProxyInput) -> Result<ProxyRecord, AppError> {
        if input.host.trim().is_empty() || input.port == 0 {
            return Err(AppError::InvalidInput("代理地址和端口不能为空".into()));
        }
        let id = format!("proxy-{}", now_ms());
        let protocol = proxy_protocol_name(input.protocol);
        let credential = if input
            .username
            .as_ref()
            .is_some_and(|value| !value.is_empty())
            || input
                .password
                .as_ref()
                .is_some_and(|value| !value.is_empty())
        {
            Some(
                serde_json::json!({"username": input.username, "password": input.password})
                    .to_string(),
            )
        } else {
            None
        };
        let proxy = crate::storage::StoredProxy {
            id: id.clone(),
            protocol: protocol.into(),
            host: input.host,
            port: input.port,
            credential_ref: credential.as_ref().map(|_| id.clone()),
            enabled: false,
            status: "untested".into(),
            cooldown_until_ms: None,
            consecutive_failures: 0,
        };
        self.call(move |db| {
            if let Some(credential) = credential.as_deref() {
                db.save_credential("proxy", &proxy.id, credential)?;
            }
            db.save_proxy(&proxy)
        })
        .await?;
        self.snapshot().await?;
        self.proxy_view(&id)
            .await
            .ok_or_else(|| AppError::NotFound("代理不存在".into()))
    }

    pub async fn import_proxies(&self, text: &str) -> Result<ProxyImportResult, AppError> {
        let mut result = ProxyImportResult {
            added: Vec::new(),
            duplicates: 0,
            invalid: Vec::new(),
        };
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parsed = reqwest::Url::parse(line);
            let Ok(url) = parsed else {
                result.invalid.push(ProxyImportIssue {
                    line: index + 1,
                    reason: "代理格式无效".into(),
                });
                continue;
            };
            let Some(protocol) = proxy_protocol(url.scheme()) else {
                result.invalid.push(ProxyImportIssue {
                    line: index + 1,
                    reason: "仅支持 http、https、socks5 代理".into(),
                });
                continue;
            };
            let (Some(host), Some(port)) = (url.host_str().map(str::to_owned), url.port()) else {
                result.invalid.push(ProxyImportIssue {
                    line: index + 1,
                    reason: "代理地址需包含端口".into(),
                });
                continue;
            };
            let scheme = url.scheme().to_owned();
            let known = self
                .call({
                    let host = host.clone();
                    move |db| {
                        Ok(db.proxies()?.iter().any(|saved| {
                            saved.host == host && saved.port == port && saved.protocol == scheme
                        }))
                    }
                })
                .await?;
            if known {
                result.duplicates += 1;
                continue;
            }
            let proxy = ProxyInput {
                protocol,
                host,
                port,
                username: (!url.username().is_empty()).then(|| url.username().to_owned()),
                password: url.password().map(str::to_owned),
            };
            let added = self.add_proxy(proxy).await?;
            result.added.push(added.id);
        }
        self.snapshot().await?;
        Ok(result)
    }

    pub async fn test_proxy(&self, id: &str) -> Result<ProxyRecord, AppError> {
        let owned = id.to_owned();
        let proxy = self
            .call(move |db| {
                db.proxies()?
                    .into_iter()
                    .find(|proxy| proxy.id == owned)
                    .ok_or(StorageError::ChannelMissing)
            })
            .await?;
        let (control, mut receiver) = ControlToken::new();
        let _permit = self
            .request_gate
            .acquire_scan(1, &mut receiver)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        let config = self.call(|db| db.monitor_config()).await?;
        let client = RicohSchedulerClient::with_proxy(&config, self.proxy_route(&proxy).await?)
            .map_err(|error| AppError::Network(error.to_string()))?;
        let result = client
            .product_detail(LineId::new(1, format!("proxy-test:{id}")))
            .await;
        drop(control);
        match result {
            Ok(_) => {
                let id = id.to_owned();
                self.call(move |db| db.record_proxy_success(&id)).await?;
            }
            Err(crate::ricoh_api::RicohApiError::Transport(_)) => {
                let id = id.to_owned();
                self.call(move |db| db.record_proxy_failure(&id)).await?;
            }
            Err(crate::ricoh_api::RicohApiError::RateLimited { retry_after }) => {
                self.request_gate.cool_domain(retry_after.as_ref()).await;
            }
            Err(_) => {}
        }
        self.snapshot().await?;
        self.proxy_view(id)
            .await
            .ok_or_else(|| AppError::NotFound("代理不存在".into()))
    }

    pub async fn set_proxy_enabled(&self, id: &str, enabled: bool) -> Result<(), AppError> {
        let id = id.to_owned();
        self.call(move |db| db.set_proxy_enabled(&id, enabled))
            .await?;
        self.snapshot().await?;
        Ok(())
    }

    pub async fn remove_proxy(&self, id: &str) -> Result<(), AppError> {
        let owned = id.to_owned();
        self.call(move |db| {
            db.remove_proxy(&owned)?;
            Ok(())
        })
        .await?;
        self.snapshot().await?;
        Ok(())
    }

    pub async fn export_config(&self) -> Result<String, AppError> {
        let export = self
            .call(|db| {
                let config = AppConfig::from_monitor(db.monitor_config()?);
                let products = db
                    .product_configs()?
                    .into_iter()
                    .map(|p| ProductExport {
                        product_id: p.identity.product_id,
                        name: p.name,
                        enabled: p.enabled,
                        prominent_alert: p.prominent_alert,
                    })
                    .collect();
                let channels = db
                    .notification_channels()?
                    .into_iter()
                    .map(|channel| {
                        let targets = db
                            .notification_routes(&channel.id)?
                            .into_iter()
                            .map(|route| route.target)
                            .collect();
                        Ok(ChannelExport {
                            id: channel.id,
                            name: channel.name,
                            provider_id: channel.provider,
                            subscriptions: channel
                                .subscriptions
                                .iter()
                                .filter_map(|s| channel_event(s))
                                .collect(),
                            targets,
                        })
                    })
                    .collect::<Result<Vec<_>, StorageError>>()?;
                Ok(ConfigExport {
                    config,
                    products,
                    channels,
                })
            })
            .await?;
        serde_json::to_string_pretty(&export).map_err(|e| AppError::InvalidInput(e.to_string()))
    }

    pub async fn import_config(&self, text: &str) -> Result<(), AppError> {
        if text.len() > MAX_CONFIG_IMPORT_BYTES {
            return Err(AppError::InvalidInput(
                "配置文件超过 4 MiB，请减少商品或渠道后再导入".into(),
            ));
        }
        let import: ConfigExport = serde_json::from_str(text)
            .map_err(|e| AppError::InvalidInput(format!("配置文件无法读取：{e}")))?;
        let config = import.config.into_monitor()?;
        let gate_requests = config.requests.clone();
        let scan_interval = config.scan.interval;
        let products = import
            .products
            .into_iter()
            .map(|product| {
                let product_id = product
                    .product_id
                    .parse::<u64>()
                    .map_err(|_| AppError::InvalidInput("导入文件中的 Product ID 无效".into()))?;
                Ok((
                    ProductIdentity {
                        key: product_id.to_string(),
                        product_id: product_id.to_string(),
                        sku_id: None,
                    },
                    product.name,
                    product.enabled,
                    product.prominent_alert,
                ))
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        let routes = import
            .channels
            .iter()
            .map(|channel| (channel.id.clone(), channel.targets.clone()))
            .collect::<Vec<_>>();
        if routes
            .iter()
            .flat_map(|(_, targets)| targets)
            .any(|target| target.id.trim().is_empty() || target.kind.trim().is_empty())
        {
            return Err(AppError::InvalidInput(
                "导入文件中的通知接收会话无效".into(),
            ));
        }
        let channels = import
            .channels
            .into_iter()
            .map(|channel| {
                (
                    channel.id,
                    channel.name,
                    channel.provider_id,
                    channel
                        .subscriptions
                        .iter()
                        .map(channel_event_name)
                        .collect(),
                )
            })
            .collect();
        let mut scan = self.scan.lock().await;
        self.settle_monitoring_time(true).await?;
        let generations = self.product_generations.clone();
        let catalog_epoch = self.catalog_epoch.clone();
        self.call(move |db| {
            db.import_configuration(&config, products, channels, now_ms())?;
            for (account, targets) in routes {
                db.save_notification_routes(&account, &targets)?;
            }
            catalog_epoch.fetch_add(1, Ordering::AcqRel);
            *generations.lock().unwrap() = db.product_generations()?;
            Ok(())
        })
        .await?;
        let (checkpoint, active) = self
            .call(|db| Ok((db.scan_checkpoint()?, db.scan_active()?)))
            .await?;
        *scan = checkpoint.map(ProductIdScan::restore);
        if active {
            if let Some(scan) = scan.as_mut() {
                scan.continue_scan();
            }
        }
        self.scan_epoch.send_modify(|epoch| *epoch += 1);
        drop(scan);
        self.request_gate.reconfigure(&gate_requests, scan_interval);
        self.snapshot().await?;
        Ok(())
    }

    pub async fn restore_defaults(&self, clear_history: bool) -> Result<(), AppError> {
        let config = AppConfig::default().into_monitor()?;
        let requests = config.requests.clone();
        let scan_interval = config.scan.interval;
        let mut scan = self.scan.lock().await;
        self.settle_monitoring_time(true).await?;
        let epoch = self.catalog_epoch.clone();
        self.call(move |db| {
            db.restore_defaults(&config, clear_history)?;
            epoch.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
        .await?;
        *scan = None;
        self.scan_epoch.send_modify(|epoch| *epoch += 1);
        drop(scan);
        self.request_gate.reconfigure(&requests, scan_interval);
        self.runtime_errors.lock().unwrap().clear();
        *self.last_error.lock().unwrap() = None;
        self.snapshot().await?;
        Ok(())
    }

    async fn proxy_view(&self, id: &str) -> Option<ProxyRecord> {
        let id = id.to_owned();
        self.call(move |db| {
            Ok(db
                .proxies()?
                .into_iter()
                .find(|proxy| proxy.id == id)
                .map(|proxy| ProxyRecord {
                    id: proxy.id,
                    protocol: proxy_protocol(&proxy.protocol).unwrap_or(ProxyProtocol::Http),
                    display_address: format!("{}:{}", proxy.host, proxy.port),
                    enabled: proxy.enabled,
                    status: match proxy.status.as_str() {
                        "available" => ProxyStatus::Available,
                        "cooldown" => ProxyStatus::Cooldown,
                        "auto_disabled" => ProxyStatus::AutoDisabled,
                        "manually_disabled" => ProxyStatus::ManuallyDisabled,
                        _ => ProxyStatus::Untested,
                    },
                }))
        })
        .await
        .ok()
        .flatten()
    }

    async fn proxy_route(
        &self,
        proxy: &crate::storage::StoredProxy,
    ) -> Result<reqwest::Proxy, AppError> {
        let address = format!("{}://{}:{}", proxy.protocol, proxy.host, proxy.port);
        let mut route = reqwest::Proxy::all(&address)
            .map_err(|_| AppError::InvalidInput("代理地址无效".into()))?;
        if let Some(reference) = proxy.credential_ref.as_deref() {
            let saved = self
                .call({
                    let reference = reference.to_owned();
                    move |db| db.credential("proxy", &reference)
                })
                .await?;
            let Some(saved) = saved else {
                return Err(AppError::Credential(
                    "代理凭据缺失，请重新添加带认证信息的代理".into(),
                ));
            };
            {
                let values: serde_json::Value = serde_json::from_str(&saved)
                    .map_err(|_| AppError::Credential("代理凭据格式无效".into()))?;
                route = route.basic_auth(
                    values["username"].as_str().unwrap_or_default(),
                    values["password"].as_str().unwrap_or_default(),
                );
            }
        }
        Ok(route)
    }

    pub async fn start_product_scan(
        &self,
        start_id: u64,
        end_id: u64,
    ) -> Result<ProductScan, AppError> {
        let mut live_scan = self.scan.lock().await;
        let scan = ProductIdScan::new(start_id, end_id)
            .map_err(|error| AppError::InvalidInput(format!("商品范围无效：{error:?}")))?;
        let checkpoint = scan
            .checkpoint()
            .ok_or_else(|| AppError::InvalidInput("扫描范围无效".into()))?;
        if !self
            .call(move |db| db.start_scan_if_idle(&checkpoint))
            .await?
        {
            return Err(AppError::InvalidInput("已有商品扫描正在运行".into()));
        }
        *live_scan = Some(scan);
        self.scan_epoch.send_modify(|epoch| *epoch += 1);
        let result = scan_view(&live_scan).expect("scan just stored");
        drop(live_scan);
        self.snapshot().await?;
        Ok(result)
    }

    pub async fn control_product_scan(&self, action: ScanAction) -> Result<ProductScan, AppError> {
        let mut scan = self.scan.lock().await;
        let checkpoint = self.call(|db| db.scan_checkpoint()).await?;
        let keep_in_flight = scan
            .as_ref()
            .is_some_and(|live| live.in_flight_id().is_some() && live.checkpoint() == checkpoint);
        if !keep_in_flight {
            *scan = checkpoint.map(ProductIdScan::restore);
        }
        let state = scan
            .as_mut()
            .ok_or_else(|| AppError::NotFound("没有正在进行的商品扫描".into()))?;
        match action {
            ScanAction::Pause => state.pause(),
            ScanAction::Resume => state.continue_scan(),
            ScanAction::Cancel => {
                state.cancel();
            }
        }
        let scan_id = state.scan_id();
        let active = action == ScanAction::Resume;
        let cancel = action == ScanAction::Cancel;
        if !self
            .call(move |db| db.control_scan_if_current(scan_id, active, cancel))
            .await?
        {
            *scan = self
                .call(|db| db.scan_checkpoint())
                .await?
                .map(ProductIdScan::restore);
            return Err(AppError::InvalidInput("扫描状态已变化，请重试".into()));
        }
        if action == ScanAction::Cancel {
            self.scan_epoch.send_modify(|epoch| *epoch += 1);
        }
        let result = scan_view(&scan).ok_or_else(|| AppError::NotFound("扫描已取消".into()))?;
        drop(scan);
        self.snapshot().await?;
        Ok(result)
    }

    pub async fn wait_for_scan(&self) -> Result<ProductScan, AppError> {
        loop {
            let (checkpoint, active) = self
                .call(|db| Ok((db.scan_checkpoint()?, db.scan_active()?)))
                .await?;
            let failure = match checkpoint.as_ref() {
                Some(checkpoint) => {
                    let scan_id = checkpoint.scan_id();
                    self.call(move |db| db.scan_failure(scan_id)).await?
                }
                None => None,
            };
            let mut scan = if let Some(checkpoint) = checkpoint {
                let mut scan = ProductIdScan::restore(checkpoint);
                if active {
                    scan.continue_scan();
                }
                scan_view(&Some(scan))
                    .ok_or_else(|| AppError::NotFound("没有商品扫描记录".into()))?
            } else {
                let state = self.scan.lock().await;
                let Some(mut scan) = state.clone() else {
                    return Err(AppError::NotFound("没有商品扫描记录".into()));
                };
                scan.cancel();
                scan_view(&Some(scan)).expect("cancelled scan remains visible")
            };
            if let Some(error) = failure {
                scan.status = ScanStatus::Failed;
                scan.error = Some(error);
            }
            if scan.status != ScanStatus::Running {
                return Ok(scan);
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    async fn cancel_missing_scan(&self, scan_id: u64) {
        let mut live = self.scan.lock().await;
        if let Some(scan) = live.as_mut().filter(|scan| scan.scan_id() == scan_id) {
            scan.cancel();
        }
        drop(live);
        let _ = self.snapshot().await;
    }

    async fn scan_replaced(&self, scan_id: u64) {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let checkpoint = self.call(|db| db.scan_checkpoint()).await;
            if !matches!(checkpoint, Ok(Some(checkpoint)) if checkpoint.scan_id() == scan_id) {
                break;
            }
        }
    }

    async fn scan_worker_loop(&self, generation: u64) -> Result<(), AppError> {
        let mut epoch = self.scan_epoch.subscribe();
        if *epoch.borrow() != generation {
            return Ok(());
        }
        let (checkpoint, active) = self
            .call(|db| Ok((db.scan_checkpoint()?, db.scan_active()?)))
            .await?;
        let Some(checkpoint) = checkpoint else {
            return Ok(());
        };
        let mut restored = ProductIdScan::restore(checkpoint);
        let scan_id = restored.scan_id();
        if active {
            restored.continue_scan();
        }
        let mut live = self.scan.lock().await;
        if *epoch.borrow() != generation {
            return Ok(());
        }
        if live.as_ref().is_none_or(|scan| scan.scan_id() != scan_id) {
            *live = Some(restored);
        }
        drop(live);
        let config = self.call(|db| db.monitor_config()).await?;
        let api = if config.use_proxy_pool || self.client_override.is_some() {
            None
        } else {
            Some(
                RicohApi::with_options(
                    config.requests.connect_timeout,
                    config.requests.total_timeout,
                    config.use_system_proxy,
                )
                .map_err(|e| AppError::Network(e.to_string()))?,
            )
        };
        loop {
            if *epoch.borrow() != generation {
                break;
            }
            let (active, exists) = self
                .call(|db| {
                    Ok((
                        db.scan_active()?,
                        db.scan_checkpoint()?.map(|checkpoint| checkpoint.scan_id()),
                    ))
                })
                .await?;
            if exists != Some(scan_id) {
                if exists.is_none() && *epoch.borrow() == generation {
                    self.cancel_missing_scan(scan_id).await;
                }
                break;
            }
            if !active {
                if let Some(scan) = self.scan.lock().await.as_mut() {
                    scan.pause();
                }
                tokio::select! {
                    biased;
                    _ = epoch.changed() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {}
                }
                continue;
            }
            let route = if config.use_proxy_pool {
                self.call(|db| {
                    Ok(db
                        .proxies()?
                        .into_iter()
                        .find(|proxy| proxy.enabled && proxy.status == "available"))
                })
                .await?
            } else {
                None
            };
            if config.use_proxy_pool && route.is_none() {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
            let id = {
                let mut scan = self.scan.lock().await;
                let Some(scan) = scan.as_mut() else { break };
                if scan.scan_id() != scan_id {
                    break;
                }
                scan.continue_scan();
                scan.next_id()
            };
            let Some(id) = id else { break };
            self.snapshot().await?;
            let (_control, mut receiver) = ControlToken::new();
            let permit = tokio::select! {
                biased;
                _ = epoch.changed() => break,
                _ = self.scan_replaced(scan_id) => break,
                result = self.request_gate.acquire_scan(id, &mut receiver) =>
                    result.map_err(|error| AppError::Network(error.to_string()))?,
            };
            let (active, exists) = self
                .call(|db| {
                    Ok((
                        db.scan_active()?,
                        db.scan_checkpoint()?.map(|checkpoint| checkpoint.scan_id()),
                    ))
                })
                .await?;
            if *epoch.borrow() != generation || exists != Some(scan_id) {
                if exists.is_none() && *epoch.borrow() == generation {
                    self.cancel_missing_scan(scan_id).await;
                }
                break;
            }
            if !active {
                drop(permit);
                let checkpoint = self.call(|db| db.scan_checkpoint()).await?;
                *self.scan.lock().await = checkpoint.map(ProductIdScan::restore);
                let _ = self.snapshot().await;
                continue;
            }
            let route_id = route
                .as_ref()
                .map(|proxy| proxy.id.as_str())
                .unwrap_or("direct");
            let request = async {
                match (&self.client_override, route.as_ref(), api.as_ref()) {
                    (Some(client), _, _) => {
                        Ok::<_, AppError>(client.product_detail(LineId::new(id, route_id)).await)
                    }
                    (None, Some(proxy), _) => {
                        let client = RicohSchedulerClient::with_proxy(
                            &config,
                            self.proxy_route(proxy).await?,
                        )
                        .map_err(|error| AppError::Network(error.to_string()))?;
                        Ok(client.product_detail(LineId::new(id, route_id)).await)
                    }
                    (None, None, Some(api)) => Ok(api.product_detail(id).await),
                    (None, None, None) => unreachable!("代理池与可用代理已在循环前校验"),
                }
            };
            let result = tokio::select! {
                biased;
                _ = epoch.changed() => break,
                _ = self.scan_replaced(scan_id) => break,
                result = request => result?,
            };
            drop(permit);
            let (active, exists) = self
                .call(|db| {
                    Ok((
                        db.scan_active()?,
                        db.scan_checkpoint()?.map(|checkpoint| checkpoint.scan_id()),
                    ))
                })
                .await
                .unwrap_or((false, None));
            {
                let mut scan_guard = self.scan.lock().await;
                let Some(scan) = scan_guard.as_mut() else {
                    break;
                };
                if *epoch.borrow() != generation
                    || scan.scan_id() != scan_id
                    || exists != Some(scan_id)
                {
                    if exists.is_none() && scan.scan_id() == scan_id {
                        scan.cancel();
                        drop(scan_guard);
                        let _ = self.snapshot().await;
                    }
                    break;
                }
                if !active {
                    scan.pause();
                }
                if scan.status() == CoreScanStatus::Cancelled {
                    break;
                }
                if let Err(error) = &result {
                    #[cfg(debug_assertions)]
                    eprintln!("商品 {id} 检查失败：{error}");
                    if let crate::ricoh_api::RicohApiError::RateLimited { retry_after } = error {
                        self.request_gate.cool_domain(retry_after.as_ref()).await;
                    }
                }
                let proxy_success = match &result {
                    Ok(_) => Some(true),
                    Err(crate::ricoh_api::RicohApiError::Transport(_)) => Some(false),
                    Err(_) => None,
                };
                let candidate = scan.clone();
                let committed = self
                    .call(move |db| db.commit_scan_item(scan_id, candidate, result.ok(), now_ms()))
                    .await?;
                let Some((mut committed, active)) = committed else {
                    break;
                };
                if !active {
                    committed.pause();
                }
                *scan = committed;
                if let (Some(proxy), Some(success)) = (route.as_ref(), proxy_success) {
                    let proxy_id = proxy.id.clone();
                    if success {
                        self.call(move |db| db.record_proxy_success(&proxy_id))
                            .await?;
                    } else {
                        self.call(move |db| db.record_proxy_failure(&proxy_id))
                            .await?;
                    }
                }
            }
            let _ = self.snapshot().await;
        }
        Ok(())
    }

    pub async fn monitoring_action(&self, action: MonitoringAction) -> Result<(), AppError> {
        let intent = match action {
            MonitoringAction::Start | MonitoringAction::Resume | MonitoringAction::Restart => {
                RunIntent::Running
            }
            MonitoringAction::Pause => RunIntent::Paused,
            MonitoringAction::Stop => RunIntent::Stopped,
        };
        if intent == RunIntent::Running && !self.snapshot().await?.setup_completed {
            return Err(AppError::InvalidInput(
                "开始监控前，请勾选有效商品并完成设置".into(),
            ));
        }
        let end_round = matches!(
            action,
            MonitoringAction::Pause | MonitoringAction::Stop | MonitoringAction::Restart
        );
        let generations = self.product_generations.clone();
        self.settle_monitoring_time(true).await?;
        self.call(move |db| {
            db.set_run_intent(intent, end_round)?;
            *generations.lock().unwrap() = db.product_generations()?;
            Ok(())
        })
        .await?;
        self.snapshot().await?;
        Ok(())
    }

    pub async fn complete_setup(&self) -> Result<(), AppError> {
        if !self.call(|db| setup_ready(db)).await? {
            return Err(AppError::InvalidInput("请先勾选有效商品".into()));
        }
        let generations = self.product_generations.clone();
        self.call(move |db| {
            db.complete_setup()?;
            *generations.lock().unwrap() = db.product_generations()?;
            Ok(())
        })
        .await?;
        self.snapshot().await?;
        Ok(())
    }

    pub async fn run(&self) -> Result<(), AppError> {
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(self.data_dir.join("monitor.run.lock"))
            .map_err(|error| AppError::Storage(error.to_string()))?;
        lock_file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => AppError::AlreadyRunning,
            std::fs::TryLockError::Error(error) => AppError::Storage(error.to_string()),
        })?;
        let mut scan_tasks = tokio::task::JoinSet::new();
        let image_worker = tokio::spawn({
            let app = self.clone();
            async move { app.image_cache_loop().await }
        });
        let mut dispatcher = tokio::spawn({
            let app = self.clone();
            async move { app.dispatch_notifications().await }
        });
        let result = tokio::select! {
            result = self.run_loop(&mut scan_tasks) => result,
            result = &mut dispatcher => result.unwrap_or_else(|error| Err(AppError::Storage(error.to_string()))),
        };
        if !dispatcher.is_finished() {
            dispatcher.abort();
            let _ = dispatcher.await;
        }
        scan_tasks.abort_all();
        while scan_tasks.join_next().await.is_some() {}
        image_worker.abort();
        let _ = image_worker.await;
        self.shutdown_notification_runtime().await;
        self.settle_monitoring_time(true).await?;
        drop(lock_file);
        if let Err(error) = &result {
            self.report_worker_failure(error.to_string()).await;
        }
        result
    }

    async fn dispatch_notifications(&self) -> Result<(), AppError> {
        self.call(|db| db.recover_channel_notifications(now_ms()))
            .await?;
        let mut deliveries = tokio::task::JoinSet::new();
        let mut maintenance = tokio::time::Instant::now();
        loop {
            if tokio::time::Instant::now() >= maintenance {
                let _ = self.configure_notification_runtime(None).await;
                if self.notification_runtime.take_status_changed() {
                    self.snapshot().await?;
                }
                maintenance = tokio::time::Instant::now() + std::time::Duration::from_millis(250);
            }
            // 每个接收路由同时只领取一条；不同路由并行，限制在途网络请求数量。
            if deliveries.len() < 32 {
                let ready = self.notification_runtime.ready_account_ids();
                if let Some(job) = self
                    .call(move |db| db.claim_ready_channel_notification(now_ms(), &ready))
                    .await?
                {
                    let app = self.clone();
                    deliveries.spawn(async move { app.deliver_notification(job).await });
                    continue;
                }
            }
            tokio::select! {
                result = deliveries.join_next(), if !deliveries.is_empty() => {
                    result.expect("存在在途通知").map_err(|error| AppError::Storage(error.to_string()))??;
                },
                _ = self.channel_notification_wake.notified() => {},
                _ = tokio::time::sleep_until(maintenance) => {},
            }
        }
    }

    async fn deliver_notification(&self, job: OutboxJob) -> Result<(), AppError> {
        let OutboxJob {
            id,
            channel_id,
            generation,
            event,
            target,
            waited_for_connection,
        } = job;
        let subscription = match event.kind {
            crate::storage::MonitorEventKind::FirstObservedInStock
            | crate::storage::MonitorEventKind::OutOfStockToInStock => "stock_available",
            crate::storage::MonitorEventKind::MonitoringFailed => "monitoring_failed",
            crate::storage::MonitorEventKind::StockIncreased => "stock_available",
            crate::storage::MonitorEventKind::Recovered => "recovered",
        };
        let result = self
            .send_event(NotificationJob {
                event,
                subscription,
                channel_id: Some(channel_id),
                generation: Some(generation),
                target,
                waited_for_connection,
            })
            .await;
        let (outcome, message) = match result {
            Ok(Some(result)) => result,
            Ok(None) => ("skipped".into(), String::new()),
            Err(error) => {
                let message = error.to_string();
                self.call(move |db| db.finish_notification(id, "failed", &message, now_ms()))
                    .await?;
                return Err(error);
            }
        };
        self.call(move |db| db.finish_notification(id, &outcome, &message, now_ms()))
            .await?;
        self.snapshot().await?;
        Ok(())
    }

    async fn run_loop(&self, scan_tasks: &mut tokio::task::JoinSet<()>) -> Result<(), AppError> {
        let mut outlet_sequences = BTreeMap::new();
        let mut active: Option<(
            ScheduledMonitor,
            Vec<String>,
            MonitorConfig,
            Vec<String>,
            BTreeMap<u64, u64>,
        )> = None;
        let mut was_in_schedule = None;
        let mut next_cleanup = tokio::time::Instant::now();
        let mut next_time_settlement = tokio::time::Instant::now();
        let mut next_clock_snapshot = tokio::time::Instant::now();
        let mut previous_scan_state: Option<(Option<ScanCheckpoint>, bool)> = None;
        let mut last_monitor_snapshot =
            tokio::time::Instant::now() - std::time::Duration::from_millis(500);
        loop {
            if tokio::time::Instant::now() >= next_time_settlement {
                self.settle_monitoring_time(false).await?;
                next_time_settlement =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(1);
            }
            if tokio::time::Instant::now() >= next_clock_snapshot {
                self.snapshot().await?;
                next_clock_snapshot =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            }
            while let Some(result) = scan_tasks.try_join_next() {
                result.map_err(|error| AppError::Storage(format!("扫描任务异常退出：{error}")))?;
            }
            let scan_state = self
                .call(|db| Ok((db.scan_checkpoint()?, db.scan_active()?)))
                .await?;
            let needs_sync = {
                let live = self.scan.lock().await;
                match &scan_state.0 {
                    Some(checkpoint) => {
                        live.as_ref().and_then(ProductIdScan::checkpoint).as_ref()
                            != Some(checkpoint)
                    }
                    None => live.as_ref().is_some_and(|scan| {
                        matches!(
                            scan.status(),
                            CoreScanStatus::Running | CoreScanStatus::Paused
                        )
                    }),
                }
            };
            if previous_scan_state.as_ref() != Some(&scan_state) || needs_sync {
                let (checkpoint, scan_active) = &scan_state;
                let mut live = self.scan.lock().await;
                let keep_in_flight = live.as_ref().is_some_and(|scan| {
                    scan.in_flight_id().is_some()
                        && scan.checkpoint().as_ref() == checkpoint.as_ref()
                });
                if !keep_in_flight {
                    if let Some(checkpoint) = checkpoint.clone() {
                        *live = Some(ProductIdScan::restore(checkpoint));
                    } else if let Some(scan) = live.as_mut() {
                        scan.cancel();
                    }
                }
                if let Some(scan) = live.as_mut() {
                    if *scan_active {
                        scan.continue_scan();
                    } else {
                        scan.pause();
                    }
                }
                drop(live);
                previous_scan_state = Some(scan_state);
                self.snapshot().await?;
            }
            if tokio::time::Instant::now() >= next_cleanup {
                self.call(|db| {
                    db.cleanup(now_ms(), 500)?;
                    db.checkpoint()?;
                    Ok(())
                })
                .await?;
                next_cleanup = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
            }
            if self.stopping.load(Ordering::Relaxed) {
                if let Some((monitor, _, _, _, _)) = active.take() {
                    monitor.shutdown().await;
                }
                return Ok(());
            }
            if self.call(|db| db.scan_active()).await? && scan_tasks.is_empty() {
                let app = self.clone();
                let generation = *self.scan_epoch.borrow();
                scan_tasks.spawn(async move {
                    if let Err(error) = app.scan_worker_loop(generation).await {
                        let scan_id = app.scan.lock().await.as_ref().map(ProductIdScan::scan_id);
                        let message = error.to_string();
                        let result = if let Some(scan_id) = scan_id {
                            app.call(move |db| db.record_scan_failure(scan_id, &message))
                                .await
                        } else {
                            Ok(())
                        };
                        if let Err(storage_error) = result {
                            app.report_worker_failure(storage_error.to_string()).await;
                            app.shutdown();
                        } else {
                            let _ = app.snapshot().await;
                        }
                    }
                });
            }
            let intent = self.call(|db| db.run_intent()).await?;
            if intent != RunIntent::Running {
                self.settle_monitoring_time(true).await?;
                was_in_schedule = None;
                if let Some((monitor, _, _, _, _)) = active.take() {
                    monitor.shutdown().await;
                }
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                continue;
            }
            if !self.call(|db| setup_completed(db)).await? {
                self.settle_monitoring_time(true).await?;
                if let Some((monitor, _, _, _, _)) = active.take() {
                    monitor.shutdown().await;
                }
                if let Ok(mut last_error) = self.last_error.lock() {
                    *last_error = Some("监控设置尚未完成，请检查已启用商品".into());
                }
                let _ = self.snapshot().await;
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                continue;
            }
            let request_gate = self.request_gate.clone();
            let (config, products, generations) = self
                .call(move |db| {
                    let config = db.monitor_config()?;
                    request_gate.set_monitor_schedule(&config.schedule);
                    Ok((
                        config,
                        db.product_configs()?
                            .into_iter()
                            .filter(|p| p.enabled)
                            .collect::<Vec<_>>(),
                        db.product_generations()?,
                    ))
                })
                .await?;
            *self.product_generations.lock().unwrap() = generations;
            let (in_schedule, schedule_transition) =
                schedule_state(&config.schedule, std::time::SystemTime::now());
            if was_in_schedule.is_some_and(|previous| previous != in_schedule) {
                self.snapshot().await?;
            }
            was_in_schedule = Some(in_schedule);
            if products.is_empty() {
                self.settle_monitoring_time(true).await?;
                if let Some((monitor, _, _, _, _)) = active.take() {
                    monitor.shutdown().await;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
            let routes = if config.use_proxy_pool {
                self.call(|db| {
                    Ok(db
                        .proxies()?
                        .into_iter()
                        .filter(|proxy| proxy.enabled && proxy.status == "available")
                        .collect::<Vec<_>>())
                })
                .await?
            } else {
                Vec::new()
            };
            let route_ids = if config.use_proxy_pool {
                routes
                    .iter()
                    .map(|proxy| proxy.id.clone())
                    .collect::<Vec<_>>()
            } else {
                vec!["direct".into()]
            };
            if route_ids.is_empty() {
                self.settle_monitoring_time(true).await?;
                if let Some((monitor, _, _, _, _)) = active.take() {
                    monitor.shutdown().await;
                }
                if let Ok(mut last_error) = self.last_error.lock() {
                    *last_error = Some("代理池没有可用出口".into());
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
            outlet_sequences.retain(|outlet, _| route_ids.contains(outlet));
            let keys = products
                .iter()
                .map(|p| p.identity.key.clone())
                .collect::<Vec<_>>();
            let current_generations = {
                let generations = self.product_generations.lock().unwrap();
                products
                    .iter()
                    .map(|product| {
                        let id = product
                            .identity
                            .product_id
                            .parse::<u64>()
                            .unwrap_or_default();
                        (id, generations.get(&id).copied().unwrap_or(0))
                    })
                    .collect::<BTreeMap<_, _>>()
            };
            let needs_restart = active
                .as_ref()
                .is_none_or(|(_, _, old_config, old_routes, _)| {
                    old_config.use_system_proxy != config.use_system_proxy
                        || old_config.monitoring_mode != config.monitoring_mode
                        || old_config.use_proxy_pool != config.use_proxy_pool
                        || old_config.requests.connect_timeout != config.requests.connect_timeout
                        || old_config.requests.total_timeout != config.requests.total_timeout
                        || *old_routes != route_ids
                });
            let product_changed = active
                .as_ref()
                .is_some_and(|(_, old_keys, _, _, _)| *old_keys != keys);
            let reset_products = active
                .as_ref()
                .map(|(_, _, _, _, old_generations)| {
                    current_generations
                        .iter()
                        .filter_map(|(id, generation)| {
                            (old_generations.get(id).copied().unwrap_or(0) != *generation)
                                .then_some(*id)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let lines = products
                .iter()
                .flat_map(|product| {
                    let product_id = product.identity.product_id.parse().unwrap_or_default();
                    route_ids
                        .iter()
                        .map(move |route_id| LineId::new(product_id, route_id))
                })
                .collect::<Vec<_>>();
            let (sequence_floor, saved_health) =
                if needs_restart || product_changed || !reset_products.is_empty() {
                    self.call(|db| Ok((db.monitoring_sequence_floor()?, db.monitoring_health()?)))
                        .await?
                } else {
                    (0, Default::default())
                };
            if needs_restart {
                if let Some((monitor, _, _, _, _)) = active.take() {
                    monitor.shutdown().await;
                }
                let mut route_clients = BTreeMap::new();
                for route_id in &route_ids {
                    let client: Arc<dyn crate::scheduler::SchedulerClient> =
                        if let Some(client) = &self.client_override {
                            client.clone()
                        } else if route_id == "direct" {
                            let mut direct = config.clone();
                            direct.use_proxy_pool = false;
                            Arc::new(
                                RicohSchedulerClient::new(&direct)
                                    .map_err(|e| AppError::Network(e.to_string()))?,
                            )
                        } else {
                            let proxy = routes
                                .iter()
                                .find(|proxy| &proxy.id == route_id)
                                .expect("route IDs come from selected proxies");
                            Arc::new(
                                RicohSchedulerClient::with_proxy(
                                    &config,
                                    self.proxy_route(proxy).await?,
                                )
                                .map_err(|e| AppError::Network(e.to_string()))?,
                            )
                        };
                    route_clients.insert(route_id.clone(), client);
                }
                let client: Arc<dyn crate::scheduler::SchedulerClient> =
                    Arc::new(RoutedSchedulerClient {
                        clients: route_clients,
                    });
                let scheduler = Scheduler::new_with_gate(
                    config.clone(),
                    client,
                    Arc::new(OsJitter::new().map_err(|e| AppError::Unsupported(e.to_string()))?),
                    self.request_gate.clone(),
                )
                .map_err(|e| AppError::InvalidInput(format!("监控配置无效：{e:?}")))?
                .with_product_generations(self.product_generations.clone())
                .with_health(saved_health);
                let monitor = scheduler
                    .start_with_sequence_floor(lines, sequence_floor)
                    .map_err(|e| AppError::InvalidInput(format!("无法启动监控：{e:?}")))?;
                active = Some((
                    monitor,
                    keys,
                    config.clone(),
                    route_ids.clone(),
                    current_generations,
                ));
            } else {
                let (monitor, old_keys, old_config, _, old_generations) =
                    active.as_mut().expect("monitor started");
                if product_changed || !reset_products.is_empty() {
                    for product_id in &reset_products {
                        self.forget_product_error(*product_id);
                    }
                    monitor
                        .reconcile_lines(lines, sequence_floor, &reset_products)
                        .await
                        .map_err(|e| AppError::InvalidInput(format!("无法更新监控商品：{e:?}")))?;
                    *old_keys = keys;
                    *old_generations = current_generations;
                }
                if *old_config != config {
                    monitor.reconfigure(&config).await;
                    *old_config = config.clone();
                }
            }
            let timing_keys = if in_schedule {
                active
                    .as_ref()
                    .map(|(_, keys, _, _, _)| keys.clone())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            self.set_timing_products(timing_keys, std::time::Instant::now() + schedule_transition)
                .await?;
            let (completed, product_map) = {
                let (monitor, _, _, _, _) = active.as_mut().expect("monitor started");
                monitor.check_tasks().await.map_err(AppError::Network)?;
                let result = tokio::select! { result = monitor.recv() => result, _ = tokio::time::sleep(std::time::Duration::from_millis(400)) => None };
                (
                    result,
                    products
                        .into_iter()
                        .map(|p| {
                            (
                                p.identity.product_id.parse::<u64>().unwrap_or_default(),
                                p.identity,
                            )
                        })
                        .collect::<BTreeMap<_, _>>(),
                )
            };
            let Some(completed) = completed else { continue };
            if self
                .product_generations
                .lock()
                .unwrap()
                .get(&completed.line.product_id)
                .copied()
                .unwrap_or(0)
                != completed.generation
            {
                continue;
            }
            // 列表轮次向每件商品发布结果，出口健康按同一轮次计一次。
            let outlet_result_is_new = config.monitoring_mode != MonitoringMode::ListedProducts
                || outlet_sequences.insert(completed.line.outlet_id.clone(), completed.sequence)
                    != Some(completed.sequence);
            if outlet_result_is_new && completed.line.outlet_id != "direct" {
                let proxy_id = completed.line.outlet_id.clone();
                match &completed.result {
                    Ok(_) => {
                        self.call(move |db| db.record_proxy_success(&proxy_id))
                            .await?;
                    }
                    Err(crate::ricoh_api::RicohApiError::Transport(_)) => {
                        self.call(move |db| db.record_proxy_failure(&proxy_id))
                            .await?;
                    }
                    Err(_) => {}
                }
            }
            let Some(identity) = product_map.get(&completed.line.product_id).cloned() else {
                continue;
            };
            #[cfg(debug_assertions)]
            if let Err(error) = &completed.result {
                eprintln!("商品 {} 检查失败：{error}", completed.line.product_id);
            }
            let events = self
                .call({
                    let identity = identity.clone();
                    let completed = completed.clone();
                    let generations = self.product_generations.clone();
                    let app = self.clone();
                    move |db| {
                        if generations
                            .lock()
                            .unwrap()
                            .get(&completed.line.product_id)
                            .copied()
                            .unwrap_or(0)
                            != completed.generation
                        {
                            return Ok(None);
                        }
                        let product_key = identity.key.clone();
                        let committed = db.commit_monitoring_attempt(
                            &product_key,
                            completed.generation,
                            |db| {
                                let mut events = Vec::new();
                                let error =
                                    completed.result.as_ref().err().map(ToString::to_string);
                                if !db.record_monitoring_runtime(
                                    &identity.key,
                                    completed.generation,
                                    completed.sequence,
                                    completed.health,
                                    error.as_deref(),
                                )? {
                                    return Ok((events, false));
                                }
                                db.record_check_attempt(&identity.key)?;
                                db.record_daily_check(
                                    &identity.key,
                                    completed.completed_at_ms,
                                    completed.result.is_ok(),
                                )?;
                                if let Some(transition) = completed.health_transition {
                                    if let Some(event) = db.commit_health_transition(
                                        identity.clone(),
                                        completed.sequence,
                                        transition,
                                        completed.completed_at_ms,
                                        &[],
                                    )? {
                                        events.push(event);
                                    }
                                }
                                let Ok(detail) = completed.result else {
                                    return Ok((events, true));
                                };
                                if let Some(detail) = &detail {
                                    db.record_product_detail(
                                        &identity.key,
                                        detail,
                                        completed.completed_at_ms,
                                    )?;
                                }
                                let prior = db.observation(&identity.key)?.map(|saved| saved.state);
                                let mut reducer = ObservationReducer::new(prior);
                                let response = AvailabilityResponse {
                                    sequence: completed.sequence,
                                    generation: 0,
                                    result: Ok(ObservationResult {
                                        availability: detail
                                            .as_ref()
                                            .map_or(Availability::OutOfStock, |d| d.availability),
                                        is_show: detail.as_ref().map_or(0, |d| d.is_show),
                                        stock: detail.as_ref().and_then(|d| d.stock.as_f64()),
                                        observed_at_ms: completed.completed_at_ms,
                                    }),
                                };
                                match reducer.prepare(
                                    response,
                                    RuntimeGate {
                                        generation: 0,
                                        enabled: true,
                                        paused: false,
                                    },
                                ) {
                                    PrepareOutcome::Prepared(prepared) => {
                                        if let Some(event) =
                                            db.commit_observation(identity, prepared, &[])?.event
                                        {
                                            events.push(event);
                                        }
                                        Ok((events, true))
                                    }
                                    PrepareOutcome::Failed | PrepareOutcome::Ignored(_) => {
                                        Ok((events, true))
                                    }
                                }
                            },
                        )?;
                        if let Some((events, true)) = &committed {
                            if !events.is_empty() {
                                app.channel_notification_wake.notify_one();
                                app.system_notification_wake.notify_one();
                                for event in events {
                                    if db.has_prominent_alert_for_event(event.id)? {
                                        app.wake_prominent_alerts();
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(committed)
                    }
                })
                .await?;
            let Some((events, true)) = events else {
                continue;
            };
            let previous_error = self
                .runtime_errors
                .lock()
                .ok()
                .and_then(|errors| errors.get(&identity.product_id).cloned());
            let current_error = completed.result.as_ref().err().map(ToString::to_string);
            {
                let generations = self.product_generations.lock().unwrap();
                if generations
                    .get(&completed.line.product_id)
                    .copied()
                    .unwrap_or(0)
                    != completed.generation
                {
                    continue;
                }
                let _ = events;
                let product_key = identity.product_id.clone();
                match &completed.result {
                    Ok(_) => {
                        if let Ok(mut errors) = self.runtime_errors.lock() {
                            errors.remove(&product_key);
                        }
                        if let Ok(mut error) = self.last_error.lock() {
                            *error = None;
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if let Ok(mut errors) = self.runtime_errors.lock() {
                            errors.insert(product_key, message.clone());
                        }
                        if let Ok(mut last_error) = self.last_error.lock() {
                            *last_error = Some(message);
                        }
                    }
                }
            }
            if previous_error != current_error
                || last_monitor_snapshot.elapsed() >= std::time::Duration::from_millis(500)
            {
                self.snapshot().await?;
                last_monitor_snapshot = tokio::time::Instant::now();
            }
        }
    }

    async fn notification_text(
        &self,
        event: &crate::storage::ListingEvent,
    ) -> Result<String, AppError> {
        let key = event.product.key.clone();
        let product = self
            .call(move |db| {
                Ok(db
                    .product_configs()?
                    .into_iter()
                    .find(|p| p.identity.key == key))
            })
            .await?;
        let name = product
            .as_ref()
            .map(|p| p.name.as_str())
            .unwrap_or(&event.product.product_id);
        if matches!(
            event.kind,
            crate::storage::MonitorEventKind::FirstObservedInStock
                | crate::storage::MonitorEventKind::OutOfStockToInStock
                | crate::storage::MonitorEventKind::StockIncreased
        ) {
            let restock = event.kind == crate::storage::MonitorEventKind::StockIncreased
                || event.kind == crate::storage::MonitorEventKind::OutOfStockToInStock
                    && event.notification_details.previous_is_show == Some(1)
                    && event.stock > 0.0;
            let label = if product.as_ref().is_some_and(|p| p.prominent_alert) {
                "【⚠️⚠️⚠️立即抢购⚠️⚠️⚠️】"
            } else if restock {
                "【⚠️补货⚠️】"
            } else {
                "【⚠️上架⚠️】"
            };
            let change = event.notification_details.previous_stock.map_or_else(
                || {
                    format!(
                        "{}：{}",
                        if restock {
                            "补货后库存"
                        } else {
                            "记录库存"
                        },
                        event.stock
                    )
                },
                |previous| {
                    format!(
                        "{}：{} → {}",
                        if restock { "补货" } else { "库存变化" },
                        previous,
                        event.stock
                    )
                },
            );
            return Ok(format!(
                "{label}{name}\n商品 ID：{}\n{change}\n发生时间：{}（北京时间）",
                event.product.product_id,
                format_timestamp(event.observed_at_ms)
            ));
        }
        let label = match event.kind {
            crate::storage::MonitorEventKind::MonitoringFailed => "监控持续失败，请检查连接：",
            crate::storage::MonitorEventKind::Recovered => "监控已恢复：",
            _ => unreachable!(),
        };
        if let Some(failure) = &event.notification_details.failure {
            return Ok(format!(
                "{label}{name}\n商品 ID：{}\n失败原因：{}\n连续失败：{} 次\n持续时间：{} 分钟 {} 秒（有效监控时间）\n记录库存：{}\n发生时间：{}（北京时间）",
                event.product.product_id,
                failure.reason.as_deref().unwrap_or("未提供具体原因"),
                failure.count, failure.active_duration_ms / 60_000,
                failure.active_duration_ms / 1000 % 60, event.stock,
                format_timestamp(event.observed_at_ms)
            ));
        }
        Ok(format!(
            "{label}{name}\n商品 ID：{}\n记录库存：{}\n发生时间：{}（北京时间）",
            event.product.product_id,
            event.stock,
            format_timestamp(event.observed_at_ms)
        ))
    }

    async fn send_event(&self, job: NotificationJob) -> Result<Option<(String, String)>, AppError> {
        let event = job.event;
        let subscription = job.subscription.to_owned();
        let generation = job.generation;
        let selected_recipient = job.target;
        let waited_for_connection = job.waited_for_connection;
        let filter_subscription = subscription.clone();
        let channels = self
            .call(move |db| {
                Ok(db
                    .notification_channels()?
                    .into_iter()
                    .filter(|channel| {
                        channel.enabled
                            && channel.tested
                            && job.channel_id.as_ref().is_none_or(|id| id == &channel.id)
                            && channel
                                .subscriptions
                                .iter()
                                .any(|value| value == &filter_subscription)
                    })
                    .collect::<Vec<_>>())
            })
            .await?;
        if channels.is_empty() {
            return Ok(None);
        }
        let account_generations = channels
            .iter()
            .map(|channel| {
                (
                    channel.id.clone(),
                    self.notification_runtime.account_generation(&channel.id),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let configured = self.configure_notification_runtime(None).await;
        let text = self.notification_text(&event).await?;
        let mut last_result = None;
        for channel in channels {
            let account_generation = account_generations[&channel.id];
            let target = if selected_recipient.is_some() {
                selected_recipient.clone()
            } else {
                self.call({
                    let reference = channel.credential_ref.clone();
                    move |db| {
                        let saved = reference
                            .as_deref()
                            .map(|reference| db.credential("channel", reference))
                            .transpose()?
                            .flatten();
                        Ok(saved
                            .and_then(|saved| {
                                serde_json::from_str::<BTreeMap<String, String>>(&saved).ok()
                            })
                            .and_then(|values| selected_target(&values)))
                    }
                })
                .await?
            };
            let mut result = SendResult::failed("请检查账户凭据与接收会话");
            let mut applicable = true;
            if let (Ok(()), Some(target)) = (&configured, target) {
                for attempt in 0..3 {
                    result = self
                        .notification_runtime
                        .send_at_generation(&channel.id, &target, &text, account_generation, || {
                            let db = self.storage.lock().unwrap();
                            let valid = db
                                .notification_event_is_enabled(
                                    &channel.id,
                                    &subscription,
                                    &event.product.key,
                                    generation,
                                )
                                .map_err(|error| error.to_string())?;
                            if !valid || (waited_for_connection && !db.waiting_notification_is_current(&event, now_ms()).map_err(|error| error.to_string())?) {
                                return Ok(false);
                            }
                            match &selected_recipient {
                                Some(target) => Ok(db
                                    .notification_routes(&channel.id)
                                    .map_err(|error| error.to_string())?
                                    .iter()
                                    .any(|route| {
                                        route.target.id == target.id
                                            && route.target.kind == target.kind
                                    })),
                                None => Ok(true),
                            }
                        })
                        .await;
                    if result.outcome == "skipped" {
                        applicable = false;
                        break;
                    }
                    if result.outcome != "failed" || !result.retryable || attempt == 2 {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2u64.pow(attempt))).await;
                }
            } else if let Err(error) = &configured {
                result = if self.notification_runtime.is_recovering().await {
                    SendResult::deferred(error.to_string())
                } else { SendResult::failed(error.to_string()) };
            }
            if !applicable {
                continue;
            }
            if result.outcome == "deferred" {
                last_result = Some((result.outcome, result.message));
                continue;
            }
            let id = channel.id;
            let kind = subscription.clone();
            last_result = Some((result.outcome.clone(), result.message.clone()));
            self.call(move |db| {
                db.record_notification_delivery(
                    &id,
                    &result.outcome,
                    &kind,
                    now_ms(),
                    &result.message,
                )
            })
            .await?;
        }
        Ok(last_result)
    }
}

fn scan_view(scan: &Option<ProductIdScan>) -> Option<ProductScan> {
    let scan = scan.as_ref()?;
    let status = match scan.status() {
        CoreScanStatus::Running => ScanStatus::Running,
        CoreScanStatus::Paused => ScanStatus::Paused,
        CoreScanStatus::Completed => ScanStatus::Completed,
        CoreScanStatus::Cancelled => ScanStatus::Cancelled,
    };
    let results = scan
        .completed()
        .iter()
        .filter_map(|item| match &item.outcome {
            ScanOutcome::Found(detail) => Some(product_record(detail, false)),
            ScanOutcome::Failed => None,
        })
        .collect::<Vec<_>>();
    Some(ProductScan {
        id: "product-scan".into(),
        start_id: scan.start_id().to_string(),
        end_id: scan.end_id().to_string(),
        current_id: scan.current_id().map(|id| id.to_string()),
        checked: scan.completed().len() as u64,
        found: results.len() as u64,
        status,
        error: None,
        results,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn setup_completed(db: &Storage) -> Result<bool, StorageError> {
    Ok(db.setup_confirmed()? && setup_ready(db)?)
}

fn setup_ready(db: &Storage) -> Result<bool, StorageError> {
    Ok(db.product_configs()?.iter().any(|product| product.enabled))
}
fn format_timestamp(ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .to_offset(time::UtcOffset::from_hms(8, 0, 0).expect("UTC+8"))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn parse_beijing_date(value: &str) -> Result<time::Date, AppError> {
    let parts = value.split('-').collect::<Vec<_>>();
    let invalid = || AppError::InvalidInput("日期应为 YYYY-MM-DD".into());
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return Err(invalid());
    }
    let year = parts[0].parse().map_err(|_| invalid())?;
    let month = parts[1].parse::<u8>().map_err(|_| invalid())?;
    let month = time::Month::try_from(month).map_err(|_| invalid())?;
    let day = parts[2].parse().map_err(|_| invalid())?;
    time::Date::from_calendar_date(year, month, day).map_err(|_| invalid())
}

fn product_record(detail: &ProductDetail, enabled: bool) -> ProductRecord {
    ProductRecord {
        product_id: detail.product_id.to_string(),
        name: detail.name.clone(),
        enabled,
        prominent_alert: false,
        check_count: 0,
        observation: Some(ProductObservation {
            availability: match detail.availability {
                Availability::InStock => "in_stock",
                Availability::OutOfStock => "out_of_stock",
            }
            .into(),
            is_show: detail.is_show,
            stock: detail.stock.as_f64(),
            checked_at: Some(now_ms().to_string()),
        }),
        runtime_error: None,
        metadata: Some(detail.metadata.clone()),
        image_path: None,
        metadata_updated_at: None,
        today_check_count: 0,
        today_success_count: 0,
        today_failure_count: 0,
        monitoring_ms: 0,
    }
}

fn provider_name(provider: &str) -> &'static str {
    match provider {
        "feishu" => "飞书",
        "wecom" => "企业微信",
        "dingtalk" => "钉钉",
        "weixin" => "微信",
        _ => "通知渠道",
    }
}

fn channel_credentials(values: &BTreeMap<String, String>) -> serde_json::Value {
    let manual = serde_json::to_value(
        values
            .iter()
            .filter(|(key, _)| !key.starts_with('_') && !key.starts_with("target"))
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap();
    if let Some(packed) = values.get("_sdkCredentials") {
        let mut credentials = serde_json::from_str(packed).unwrap_or(serde_json::Value::Null);
        if let Some(token) = manual.get("botToken") {
            if credentials.get("botToken").or_else(|| credentials.get("token")).is_some_and(|old| old != token) {
                for field in ["token", "contextTokens", "contextMetadata", "getUpdatesBuf"] {
                    credentials.as_object_mut().unwrap().remove(field);
                }
            }
        }
        crate::notifications::merge_credentials(&mut credentials, &manual);
        return credentials;
    }
    manual
}

fn selected_target(values: &BTreeMap<String, String>) -> Option<NotificationTarget> {
    let id = values
        .get("targetId")
        .filter(|value| !value.is_empty())?
        .clone();
    let kind = values
        .get("targetKind")
        .filter(|value| !value.is_empty())?
        .clone();
    Some(NotificationTarget {
        label: id.clone(),
        id,
        kind,
    })
}

fn channel_targets(values: &BTreeMap<String, String>) -> Vec<NotificationTarget> {
    let mut targets: Vec<NotificationTarget> = values
        .get("_targets")
        .and_then(|packed| serde_json::from_str(packed).ok())
        .unwrap_or_default();
    if let Some(target) = selected_target(values) {
        if !targets
            .iter()
            .any(|old| old.id == target.id && old.kind == target.kind)
        {
            targets.push(target);
        }
    }
    merge_channel_targets(
        Vec::new(),
        targets
            .into_iter()
            .map(|target| canonical_channel_target(values, target)),
    )
}

fn canonical_channel_target(
    values: &BTreeMap<String, String>,
    mut target: NotificationTarget,
) -> NotificationTarget {
    if target.kind == "user" {
        if let Some(id) = channel_credentials(values)["targetAliases"][&target.id].as_str() {
            target.id = id.to_owned();
        }
    }
    target
}

fn merge_channel_targets(
    mut targets: Vec<NotificationTarget>,
    updates: impl IntoIterator<Item = NotificationTarget>,
) -> Vec<NotificationTarget> {
    for target in updates {
        if let Some(old) = targets
            .iter_mut()
            .find(|old| old.id == target.id && old.kind == target.kind)
        {
            if !target.label.is_empty() && target.label != target.id {
                old.label = target.label;
            }
        } else {
            targets.push(target);
        }
    }
    targets
}

fn read_detected_targets(value: serde_json::Value) -> Result<Vec<NotificationTarget>, AppError> {
    let targets = value.get("targets").cloned().unwrap_or(value);
    serde_json::from_value(targets)
        .map_err(|_| AppError::Network("群组列表格式无效，请重新检测".into()))
}
fn proxy_protocol(protocol: &str) -> Option<ProxyProtocol> {
    match protocol {
        "http" => Some(ProxyProtocol::Http),
        "https" => Some(ProxyProtocol::Https),
        "socks5" => Some(ProxyProtocol::Socks5),
        _ => None,
    }
}
fn proxy_protocol_name(protocol: ProxyProtocol) -> &'static str {
    match protocol {
        ProxyProtocol::Http => "http",
        ProxyProtocol::Https => "https",
        ProxyProtocol::Socks5 => "socks5",
    }
}
fn channel_event(event: &str) -> Option<ChannelEvent> {
    match event {
        "stock_available" => Some(ChannelEvent::StockAvailable),
        "monitoring_failed" => Some(ChannelEvent::MonitoringFailed),
        "recovered" => Some(ChannelEvent::Recovered),
        _ => None,
    }
}
fn channel_event_name(event: &ChannelEvent) -> String {
    match event {
        ChannelEvent::StockAvailable => "stock_available",
        ChannelEvent::MonitoringFailed => "monitoring_failed",
        ChannelEvent::Recovered => "recovered",
    }
    .into()
}
fn provider_definitions() -> Vec<ProviderDefinition> {
    ["feishu", "wecom", "dingtalk", "weixin"]
        .into_iter()
        .map(|id| {
            let credentials: &[(&str, &str, bool)] = match id {
                "wecom" => &[("botId", "机器人 ID", true), ("secret", "机器人密钥", true)],
                "weixin" => &[
                    ("botToken", "机器人令牌", true),
                    ("baseUrl", "服务地址", false),
                    ("userId", "用户 ID", false),
                    ("contextToken", "会话令牌", false),
                ],
                _ => &[("appId", "应用 ID", true), ("appSecret", "应用密钥", true)],
            };
            let mut fields = credentials
                .iter()
                .map(|(key, label, required)| ProviderField {
                    key: (*key).into(),
                    label: (*label).into(),
                    r#type: if *key == "baseUrl" {
                        "text"
                    } else {
                        "password"
                    }
                    .into(),
                    required: *required,
                    placeholder: None,
                    help: None,
                })
                .collect::<Vec<_>>();
            fields.extend(
                [("targetId", "接收会话 ID"), ("targetKind", "接收会话类型")]
                    .into_iter()
                    .map(|(key, label)| ProviderField {
                        key: key.into(),
                        label: label.into(),
                        r#type: "text".into(),
                        required: true,
                        placeholder: None,
                        help: None,
                    }),
            );
            ProviderDefinition {
                id: id.into(),
                name: provider_name(id).into(),
                supports_binding: true,
                documentation_url: Some(
                    match id {
                        "feishu" => "https://open.feishu.cn/document/home/index",
                        "wecom" => "https://developer.work.weixin.qq.com/document/",
                        "dingtalk" => "https://open.dingtalk.com/document/",
                        _ => "https://developers.weixin.qq.com/",
                    }
                    .into(),
                ),
                fields,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "prominent_wakeup_tests.rs"]
mod prominent_wakeup_tests;

#[cfg(test)]
mod tests {
    fn detail_app_config() -> super::AppConfig {
        let mut config = super::AppConfig::default();
        config.monitoring_mode = crate::config::MonitoringMode::ProductDetail;
        config
    }
    use super::*;
    use std::{
        future::Future,
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn changing_weixin_credentials_clears_previous_conversation_state() {
        let mut values = BTreeMap::from([
            ("_sdkCredentials".into(), serde_json::json!({"botToken":"old","token":"old","contextTokens":{"owner":"old-context"},"contextMetadata":{"owner":{"seq":"20"}},"getUpdatesBuf":"old-cursor"}).to_string()),
            ("botToken".into(), "old".into()),
        ]);
        assert_eq!(channel_credentials(&values)["contextTokens"]["owner"], "old-context");
        values.insert("botToken".into(), "new".into());
        let credentials = channel_credentials(&values);
        for field in ["token", "contextTokens", "contextMetadata", "getUpdatesBuf"] {
            assert!(credentials.get(field).is_none(), "旧会话字段 {field} 未清理");
        }
        assert_eq!(credentials["botToken"], "new");
    }

    #[tokio::test]
    async fn revoked_platform_permission_keeps_system_notification_queued() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-platform-notification-permission-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_onboarding_products(&[65]).await.unwrap();
        app.set_system_notifications_enabled(true).await.unwrap();
        app.call(|db| {
            db.save_notification_channel(
                "fixture-channel",
                "Fixture",
                "feishu",
                Some("fixture-channel"),
                &["stock_available".into()],
            )?;
            db.mark_notification_channel_tested("fixture-channel")?;
            db.set_notification_channel_enabled("fixture-channel", true)
        })
        .await
        .unwrap();
        app.call(|db| {
            let mut reducer = ObservationReducer::new(None);
            let generation = db.product_generation("65")?;
            let PrepareOutcome::Prepared(prepared) = reducer.prepare(
                AvailabilityResponse {
                    sequence: 1,
                    generation,
                    result: Ok(ObservationResult {
                        availability: Availability::InStock,
                        is_show: 1,
                        stock: Some(1.0),
                        observed_at_ms: now_ms(),
                    }),
                },
                RuntimeGate {
                    generation,
                    enabled: true,
                    paused: false,
                },
            ) else {
                panic!("expected listing")
            };
            db.commit_observation(
                ProductIdentity {
                    key: "65".into(),
                    product_id: "65".into(),
                    sku_id: None,
                },
                prepared,
                &[],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        app.set_platform_notifications_available(false);
        assert!(app.claim_system_notification().await.unwrap().is_none());
        let channel_job = app
            .call(|db| db.claim_channel_notification(now_ms()))
            .await
            .unwrap();
        assert!(channel_job.is_some(), "撤销系统通知权限不应阻断外部渠道");

        app.set_platform_notifications_available(true);
        assert!(app.claim_system_notification().await.unwrap().is_some());
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    struct CatalogImageClient {
        calls: Arc<AtomicUsize>,
        fail_once: Arc<AtomicBool>,
        url: String,
    }
    impl SchedulerClient for CatalogImageClient {
        fn product_detail(
            &self,
            line: LineId,
        ) -> Pin<
            Box<dyn Future<Output = Result<ProductDetail, crate::ricoh_api::RicohApiError>> + Send>,
        > {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let fail = self.fail_once.swap(false, Ordering::Relaxed);
            let url = self.url.clone();
            Box::pin(async move {
                if fail {
                    return Err(crate::ricoh_api::RicohApiError::InvalidConfiguration);
                }
                Ok(ProductDetail {
                    product_id: line.product_id,
                    name: "图片样例商品".into(),
                    is_show: 1,
                    stock: serde_json::Number::from(1),
                    availability: Availability::InStock,
                    metadata: ProductMetadata {
                        image_url: Some(url),
                        price: Some("19.99".into()),
                        ..Default::default()
                    },
                })
            })
        }
    }

    #[tokio::test]
    async fn catalog_images_restore_cache_fill_partial_metadata_and_retry_failed_browse_once() {
        use crate::image_cache::ImageCache;
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/image.png", listener.local_addr().unwrap());
        let downloads = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn({
            let downloads = downloads.clone();
            async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = [0; 4096];
                    socket.read(&mut request).await.unwrap();
                    downloads.fetch_add(1, Ordering::Relaxed);
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 5\r\nConnection: close\r\n\r\nimage").await.unwrap();
                }
            }
        });
        for case in ["remote", "cache", "partial", "failure"] {
            let directory = std::env::temp_dir().join(format!(
                "ricoh-catalog-images-{}-{}-{case}",
                std::process::id(),
                now_ms()
            ));
            let calls = Arc::new(AtomicUsize::new(0));
            let app = MonitorApp::open_with_scheduler_client(
                &directory,
                Arc::new(CatalogImageClient {
                    calls: calls.clone(),
                    fail_once: Arc::new(AtomicBool::new(case == "failure")),
                    url: url.clone(),
                }),
            )
            .unwrap();
            let target_url = (case == "remote").then(|| url.clone());
            let seed_url = url.clone();
            app.call(move |db| {
                let mut config = db.monitor_config()?;
                config.requests.global_requests_per_second = 100.0;
                config.scan.interval = std::time::Duration::from_millis(1);
                db.save_monitor_config(&config)?;
                for &(id, name) in DEFAULT_PRODUCTS
                    .iter()
                    .chain(std::iter::once(&(41, "图片样例商品")))
                {
                    db.save_product_config(
                        ProductIdentity {
                            key: id.to_string(),
                            product_id: id.to_string(),
                            sku_id: None,
                        },
                        name.into(),
                        "fixture".into(),
                        Some(1),
                    )?;
                    db.save_product_metadata(
                        &id.to_string(),
                        &ProductMetadata {
                            image_url: if id == 41 {
                                target_url.clone()
                            } else {
                                Some(seed_url.clone())
                            },
                            price: Some("1.00".into()),
                            ..Default::default()
                        },
                        1,
                    )?;
                }
                db.set_product_enabled("41", true)?;
                Ok(())
            })
            .await
            .unwrap();
            let mut cache = ImageCache::new(
                directory.join("images"),
                false,
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(2),
            )
            .unwrap();
            for &(id, _) in DEFAULT_PRODUCTS {
                cache.cache(id, &url).await.unwrap();
            }
            let saved_path = if case == "cache" {
                Some(cache.cache(41, &url).await.unwrap())
            } else {
                None
            };
            let before = downloads.load(Ordering::Relaxed);
            let initial = app.snapshot().await.unwrap();
            let target = initial
                .products
                .iter()
                .find(|p| p.product_id == "41")
                .unwrap();
            if let Some(path) = &saved_path {
                assert_eq!(
                    target.image_path.as_deref(),
                    path.to_str(),
                    "首次快照恢复持久图片缓存"
                );
            }
            app.hydrate_catalog_metadata().await.unwrap();
            if case == "failure" {
                assert!(
                    app.snapshot().await.unwrap().runtime.last_error.is_some(),
                    "保留签名详情的原始错误"
                );
                assert_eq!(calls.load(Ordering::Relaxed), 1);
                app.hydrate_catalog_metadata().await.unwrap();
            }
            let worker = tokio::spawn({
                let app = app.clone();
                async move { app.image_cache_loop().await }
            });
            let after = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    let snapshot = app.snapshot().await.unwrap();
                    if snapshot
                        .products
                        .iter()
                        .find(|p| p.product_id == "41")
                        .unwrap()
                        .image_path
                        .is_some()
                    {
                        break snapshot;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            worker.abort();
            let _ = worker.await;
            let target = after
                .products
                .iter()
                .find(|p| p.product_id == "41")
                .unwrap();
            assert!(
                target
                    .image_path
                    .as_ref()
                    .is_some_and(|p| std::path::Path::new(p).is_file()),
                "浏览完成后图片落盘"
            );
            assert!(target.enabled, "资料读取保留监控选择");
            assert_eq!(target.check_count, 0, "资料读取不记作监控检查");
            assert_eq!(
                downloads.load(Ordering::Relaxed) - before,
                usize::from(case != "cache")
            );
            assert_eq!(
                calls.load(Ordering::Relaxed),
                match case {
                    "remote" | "cache" => 0,
                    "partial" => 1,
                    _ => 2,
                }
            );
            app.hydrate_catalog_metadata().await.unwrap();
            assert_eq!(
                downloads.load(Ordering::Relaxed) - before,
                usize::from(case != "cache"),
                "再次浏览复用图片"
            );
            if let Some(path) = saved_path {
                std::fs::remove_file(path).unwrap();
                assert!(
                    app.snapshot()
                        .await
                        .unwrap()
                        .products
                        .iter()
                        .find(|p| p.product_id == "41")
                        .unwrap()
                        .image_path
                        .is_none(),
                    "不返回已删除文件"
                );
            }
            app.shutdown();
            drop(app);
            std::fs::remove_dir_all(directory).unwrap();
        }
        server.abort();
    }

    #[tokio::test]
    async fn image_cache_initialization_recovers_after_directory_is_repaired_without_stopping_monitoring(
    ) {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/image.png", listener.local_addr().unwrap());
        let downloads = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn({
            let downloads = downloads.clone();
            async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = [0; 4096];
                    socket.read(&mut request).await.unwrap();
                    downloads.fetch_add(1, Ordering::Relaxed);
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 5\r\nConnection: close\r\n\r\nimage").await.unwrap();
                }
            }
        });
        for (case, concurrent_error) in [
            ("own-error", None),
            ("business-error", Some("业务测试故障")),
        ] {
            let directory = std::env::temp_dir().join(format!(
                "ricoh-image-cache-initialization-{}-{}-{case}",
                std::process::id(),
                now_ms()
            ));
            let app = MonitorApp::open_with_scheduler_client(
                &directory,
                Arc::new(CatalogImageClient {
                    calls: Arc::new(AtomicUsize::new(0)),
                    fail_once: Arc::new(AtomicBool::new(false)),
                    url: url.clone(),
                }),
            )
            .unwrap();
            app.set_platform_notifications_available(true);
            let mut config = detail_app_config();
            config.schedule.start = "00:00".into();
            config.schedule.end = "00:00".into();
            config.rate.interval_min_ms = 3000;
            config.rate.interval_max_ms = 3000;
            config.use_system_proxy = false;
            app.save_config(config).await.unwrap();
            app.add_product(41).await.unwrap();
            app.complete_setup().await.unwrap();
            app.monitoring_action(MonitoringAction::Start)
                .await
                .unwrap();
            let image_root = directory.join("images");
            std::fs::write(&image_root, b"blocked").unwrap();
            let previous_downloads = downloads.load(Ordering::Relaxed);
            let runner = tokio::spawn({
                let app = app.clone();
                async move { app.run().await }
            });
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
            let failed = app.snapshot().await.unwrap();
            let before = failed
                .products
                .iter()
                .find(|p| p.product_id == "41")
                .unwrap()
                .check_count;
            let failure_visible = failed
                .runtime
                .last_error
                .as_deref()
                .is_some_and(|message| message.starts_with("图片缓存初始化失败："));
            assert_eq!(failed.runtime.state, RuntimeState::Monitoring);
            assert!(before > 0, "图片缓存初始化失败时监控仍提交有效结果");
            assert_eq!(downloads.load(Ordering::Relaxed), previous_downloads);
            if let Some(error) = concurrent_error {
                *app.last_error.lock().unwrap() = Some(error.into());
            }
            std::fs::remove_file(&image_root).unwrap();
            std::fs::create_dir(&image_root).unwrap();
            let recovered = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    let snapshot = app.snapshot().await.unwrap();
                    let product = snapshot
                        .products
                        .iter()
                        .find(|p| p.product_id == "41")
                        .unwrap();
                    if product
                        .image_path
                        .as_ref()
                        .is_some_and(|path| Path::new(path).is_file())
                    {
                        break snapshot;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await;
            let monitoring_continues =
                tokio::time::timeout(std::time::Duration::from_secs(4), async {
                    loop {
                        let snapshot = app.snapshot().await.unwrap();
                        if snapshot
                            .products
                            .iter()
                            .find(|p| p.product_id == "41")
                            .unwrap()
                            .check_count
                            > before
                        {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                })
                .await;
            app.shutdown();
            tokio::time::timeout(std::time::Duration::from_secs(3), runner)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            drop(app);
            std::fs::remove_dir_all(directory).unwrap();
            let recovered = recovered.expect("目录修复后应重新初始化缓存并实际下载图片");
            assert_eq!(recovered.runtime.state, RuntimeState::Monitoring);
            assert_eq!(
                recovered.runtime.last_error.as_deref(),
                concurrent_error,
                "恢复应清除本次初始化错误，并保留同时发生的业务错误"
            );
            monitoring_continues.expect("图片缓存恢复后监控应继续提交结果");
            assert!(downloads.load(Ordering::Relaxed) > previous_downloads);
            assert!(failure_visible, "缓存初始化错误应通过既有运行错误状态可见");
        }
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn onboarding_without_validation_exports_prominent_choice_and_claims_zero_stock_listing()
    {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-onboarding-alert-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_onboarding_products(&[65]).await.unwrap();
        app.complete_setup().await.unwrap();
        assert!(app.snapshot().await.unwrap().setup_completed);
        app.set_product_prominent_alert(65, true).await.unwrap();
        let exported = app.export_config().await.unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&exported).unwrap()["products"][0]
                ["prominentAlert"],
            true
        );
        app.import_config(&exported).await.unwrap();
        assert!(app.snapshot().await.unwrap().products[0].prominent_alert);
        let event = app
            .call(|db| {
                db.set_system_notifications_enabled(true)?;
                db.save_product_metadata(
                    "65",
                    &ProductMetadata {
                        price: Some("1234.50".into()),
                        image_url: Some("https://example.com/gr.jpg".into()),
                        ..Default::default()
                    },
                    now_ms(),
                )?;
                let mut reducer = ObservationReducer::new(None);
                let generation = db.product_generation("65")?;
                let PrepareOutcome::Prepared(prepared) = reducer.prepare(
                    AvailabilityResponse {
                        sequence: 1,
                        generation,
                        result: Ok(ObservationResult {
                            availability: Availability::OutOfStock,
                            is_show: 1,
                            stock: Some(0.0),
                            observed_at_ms: now_ms(),
                        }),
                    },
                    RuntimeGate {
                        generation,
                        enabled: true,
                        paused: false,
                    },
                ) else {
                    panic!("expected listing")
                };
                Ok(db
                    .commit_observation(
                        ProductIdentity {
                            key: "65".into(),
                            product_id: "65".into(),
                            sku_id: None,
                        },
                        prepared,
                        &[],
                    )?
                    .event
                    .unwrap())
            })
            .await
            .unwrap();
        let text = app.notification_text(&event).await.unwrap();
        assert!(text.starts_with("【⚠️⚠️⚠️立即抢购⚠️⚠️⚠️】官翻品 GR IIIx"));
        let (_, system) = app.claim_system_notification().await.unwrap().unwrap();
        assert_eq!(system.message, text);
        let image_root = directory.join("images");
        std::fs::create_dir_all(&image_root).unwrap();
        let image_path = image_root.join("65-1.png");
        std::fs::write(&image_path, b"image").unwrap();
        std::fs::write(
            image_root.join("65.json"),
            br#"{"url":"https://example.com/gr.jpg","file_name":"65-1.png"}"#,
        )
        .unwrap();
        assert!(app.image_paths.lock().unwrap().is_empty());
        let alert = app.claim_prominent_alert().await.unwrap().unwrap();
        assert_eq!(
            alert.image_path.as_deref(),
            image_path.to_str(),
            "真实提醒直接恢复持久缓存"
        );
        assert_eq!(alert.event_id, event.id);
        assert_eq!(alert.stock, 0.0);
        assert_eq!(alert.name, "官翻品 GR IIIx");
        assert_eq!(alert.price.as_deref(), Some("1234.50"));
        assert_eq!(
            alert.image_url.as_deref(),
            Some("https://example.com/gr.jpg")
        );
        app.call(|db| {
            db.save_product_metadata(
                "65",
                &ProductMetadata {
                    image_url: Some("https://example.com/changed.jpg".into()),
                    ..Default::default()
                },
                now_ms(),
            )
        })
        .await
        .unwrap();
        assert!(
            app.claim_prominent_alert()
                .await
                .unwrap()
                .unwrap()
                .image_path
                .is_none(),
            "已有网址时排除不匹配缓存"
        );
        app.call(|db| db.save_product_metadata("65", &ProductMetadata::default(), now_ms()))
            .await
            .unwrap();
        let recovered = app.claim_prominent_alert().await.unwrap().unwrap();
        assert_eq!(recovered.image_url, None);
        assert_eq!(
            recovered.image_path.as_deref(),
            image_path.to_str(),
            "缺少网址时提醒沿用持久缓存"
        );
        std::fs::remove_file(&image_path).unwrap();
        assert!(
            app.claim_prominent_alert()
                .await
                .unwrap()
                .unwrap()
                .image_path
                .is_none(),
            "提醒不返回已删除文件"
        );
        assert_eq!(
            app.claim_prominent_alert().await.unwrap().unwrap().event_id,
            alert.event_id
        );
        app.acknowledge_prominent_alert(alert.event_id)
            .await
            .unwrap();
        assert!(app.claim_prominent_alert().await.unwrap().is_none());
        app.set_product_prominent_alert(65, false).await.unwrap();
        assert!(app
            .notification_text(&event)
            .await
            .unwrap()
            .starts_with("【⚠️上架⚠️】官翻品 GR IIIx"));
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn stock_increase_notification_uses_real_delta_and_prominent_prefix() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-stock-increase-copy-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_onboarding_products(&[65]).await.unwrap();
        app.set_system_notifications_enabled(true).await.unwrap();
        app.set_product_prominent_alert(65, true).await.unwrap();
        let base_at_ms = now_ms();
        let event = app
            .call(move |db| {
                let mut increased = None;
                for (sequence, stock, at_ms) in [(1, 3.0, base_at_ms), (2, 5.0, base_at_ms + 100)] {
                    let current = db.observation("65")?.map(|observation| observation.state);
                    let mut reducer = ObservationReducer::new(current);
                    let generation = db.product_generation("65")?;
                    let PrepareOutcome::Prepared(prepared) = reducer.prepare(
                        AvailabilityResponse {
                            sequence,
                            generation,
                            result: Ok(ObservationResult {
                                availability: Availability::InStock,
                                is_show: 1,
                                stock: Some(stock),
                                observed_at_ms: at_ms,
                            }),
                        },
                        RuntimeGate {
                            generation,
                            enabled: true,
                            paused: false,
                        },
                    ) else {
                        panic!("expected stock observation")
                    };
                    if let Some(event) = db
                        .commit_observation(
                            ProductIdentity {
                                key: "65".into(),
                                product_id: "65".into(),
                                sku_id: None,
                            },
                            prepared,
                            &[],
                        )?
                        .event
                    {
                        increased = Some(event);
                    }
                }
                Ok(increased.unwrap())
            })
            .await
            .unwrap();
        let (first_id, _) = app.claim_system_notification().await.unwrap().unwrap();
        app.call(move |db| db.finish_notification(first_id, "accepted", "accepted", now_ms()))
            .await
            .unwrap();
        let (_, system) = app.claim_system_notification().await.unwrap().unwrap();
        assert_eq!(system.id, event.id);
        assert!(system.message.contains("官翻品 GR IIIx"));
        assert!(system.message.contains("补货：3 → 5"));
        assert!(system.message.starts_with("【⚠️⚠️⚠️立即抢购⚠️⚠️⚠️】"));
        let text = app.notification_text(&event).await.unwrap();
        assert!(text.starts_with("【⚠️⚠️⚠️立即抢购⚠️⚠️⚠️】官翻品 GR IIIx"));
        assert!(text.contains("补货：3 → 5"));
        app.set_product_prominent_alert(65, false).await.unwrap();
        assert!(app
            .notification_text(&event)
            .await
            .unwrap()
            .starts_with("【⚠️补货⚠️】官翻品 GR IIIx"));
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn notification_copy_preserves_each_listing_and_restock_after_reopen() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-restock-copy-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_onboarding_products(&[65]).await.unwrap();
        app.set_system_notifications_enabled(true).await.unwrap();
        let at = now_ms();
        app.call(move |db| {
            // 连续放货、抢空、再次补货，以及下架后重新上架。
            for (sequence, is_show, stock) in [
                (1, 1, Some(10.0)),
                (2, 1, Some(0.0)),
                (3, 1, Some(10.0)),
                (4, 1, Some(20.0)),
                (5, 0, None),
                (6, 1, Some(0.0)),
                (7, 1, Some(5.0)),
                (8, 0, Some(2.0)),
                (9, 1, Some(2.0)),
                (10, 1, Some(4.0)),
                (11, 1, Some(4.0)),
                (12, 1, Some(1.0)),
            ] {
                let mut reducer = ObservationReducer::new(db.observation("65")?.map(|o| o.state));
                let generation = db.product_generation("65")?;
                let PrepareOutcome::Prepared(prepared) = reducer.prepare(
                    AvailabilityResponse {
                        sequence,
                        generation,
                        result: Ok(ObservationResult {
                            availability: if stock.is_some_and(|s| s > 0.0) {
                                Availability::InStock
                            } else {
                                Availability::OutOfStock
                            },
                            is_show,
                            stock,
                            observed_at_ms: at + sequence as i64,
                        }),
                    },
                    RuntimeGate {
                        generation,
                        enabled: true,
                        paused: false,
                    },
                ) else {
                    panic!("expected observation")
                };
                db.commit_observation(
                    ProductIdentity {
                        key: "65".into(),
                        product_id: "65".into(),
                        sku_id: None,
                    },
                    prepared,
                    &[],
                )?;
            }
            // 文案应使用入队时的库存，而不依赖仍在保留期内的检查历史。
            db.cleanup(at + 31 * 24 * 60 * 60 * 1000, 100)?;
            Ok(())
        })
        .await
        .unwrap();
        drop(app);
        let app = MonitorApp::open(&directory).unwrap();
        let expected = [
            ("【⚠️上架⚠️】", "记录库存：10"),
            ("【⚠️补货⚠️】", "补货：0 → 10"),
            ("【⚠️补货⚠️】", "补货：10 → 20"),
            ("【⚠️上架⚠️】", "记录库存：0"),
            ("【⚠️补货⚠️】", "补货：0 → 5"),
            ("【⚠️上架⚠️】", "库存变化：2 → 2"),
            ("【⚠️补货⚠️】", "补货：2 → 4"),
        ];
        for (prefix, change) in expected {
            let (id, event) = app.claim_system_notification().await.unwrap().unwrap();
            assert!(event.message.starts_with(prefix), "{}", event.message);
            assert!(event.message.contains(change), "{}", event.message);
            app.finish_system_notification(id, Ok(())).await.unwrap();
        }
        assert!(app.claim_system_notification().await.unwrap().is_none());
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn failure_notification_preserves_reason_count_and_active_duration() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-failure-copy-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_onboarding_products(&[65]).await.unwrap();
        app.set_system_notifications_enabled(true).await.unwrap();
        let at = now_ms();
        app.call(move |db| {
            let generation = db.product_generation("65")?;
            for sequence in 1..=3 {
                db.record_monitoring_runtime(
                    "65",
                    generation,
                    sequence,
                    crate::scheduler::MonitoringHealth {
                        failed_since_ms: Some(at),
                        active_failure_ms: (sequence - 1) * 300_000,
                        failure_reported: sequence == 3,
                    },
                    Some("连接超时"),
                )?;
            }
            db.commit_health_transition(
                ProductIdentity {
                    key: "65".into(),
                    product_id: "65".into(),
                    sku_id: None,
                },
                3,
                crate::scheduler::MonitoringHealthTransition::Failed { since_ms: at },
                at + 600_000,
                &[],
            )?;
            // 发送前已恢复；先前异常事件仍需保留真实故障信息。
            db.record_monitoring_runtime(
                "65",
                generation,
                4,
                crate::scheduler::MonitoringHealth::default(),
                None,
            )?;
            db.commit_health_transition(
                ProductIdentity {
                    key: "65".into(),
                    product_id: "65".into(),
                    sku_id: None,
                },
                4,
                crate::scheduler::MonitoringHealthTransition::Recovered,
                at + 600_001,
                &[],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        drop(app);
        let app = MonitorApp::open(&directory).unwrap();
        let (id, event) = app.claim_system_notification().await.unwrap().unwrap();
        assert!(
            event.message.contains("官翻品 GR IIIx"),
            "{}",
            event.message
        );
        assert!(
            event.message.contains("失败原因：连接超时"),
            "{}",
            event.message
        );
        assert!(
            event.message.contains("连续失败：3 次"),
            "{}",
            event.message
        );
        assert!(
            event
                .message
                .contains("持续时间：10 分钟 0 秒（有效监控时间）"),
            "{}",
            event.message
        );
        let stored = app.call(|db| db.recent_events(10)).await.unwrap();
        let failure = stored
            .iter()
            .find(|e| e.kind == crate::storage::MonitorEventKind::MonitoringFailed)
            .unwrap();
        assert_eq!(event.message, app.notification_text(failure).await.unwrap());
        app.finish_system_notification(id, Ok(())).await.unwrap();
        let (_, recovery) = app.claim_system_notification().await.unwrap().unwrap();
        assert!(recovery.message.starts_with("监控已恢复：官翻品 GR IIIx"));
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn worker_failure_publishes_truthful_state_even_when_storage_is_unavailable() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-worker-fault-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        let before = app.snapshot().await.unwrap();
        let mut updates = app.subscribe();
        let fault = "磁盘写入失败：disk full".to_owned();
        app.report_worker_failure(fault.clone()).await;
        let failed = updates.recv().await.unwrap();
        assert_eq!(
            serde_json::to_value(&failed.runtime).unwrap()["state"],
            "worker_failed"
        );
        assert_eq!(failed.runtime.last_error, Some(fault.clone()));
        assert_eq!(failed.products, before.products);
        assert_eq!(
            app.snapshot().await.unwrap().runtime.last_error,
            Some(fault.clone())
        );
        app.storage.lock().unwrap().fail_reads_for_test();
        app.report_worker_failure(fault.clone()).await;
        assert_eq!(
            updates.recv().await.unwrap().runtime.last_error,
            Some(fault)
        );
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn scan_task_panic_is_reported_instead_of_restarted() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-scan-panic-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async { panic!("扫描内部故障") });
        tokio::task::yield_now().await;
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(1), app.run_loop(&mut tasks))
                .await
                .unwrap();
        assert!(
            matches!(result, Err(AppError::Storage(message)) if message.contains("扫描任务异常退出"))
        );
        assert!(tasks.is_empty());
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn oversized_config_is_rejected_before_parsing_or_changing_state() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-import-limit-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        let before = app.snapshot().await.unwrap();
        assert!(matches!(
            app.import_config(&" ".repeat(MAX_CONFIG_IMPORT_BYTES + 1))
                .await,
            Err(AppError::InvalidInput(_))
        ));
        assert_eq!(app.snapshot().await.unwrap().config, before.config);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn default_rate_matches_monitor_engine() {
        let monitor = MonitorConfig::default();
        let app = AppConfig::from_monitor(monitor.clone());
        assert_eq!(app.rate.interval_min_ms, 1_000);
        assert_eq!(app.rate.interval_max_ms, 2_000);
        assert_eq!(app.rate.failures_before_backoff, 3);
        assert_eq!(app.rate.failure_backoff_seconds, 20);
        assert_eq!(monitor.scan.interval, std::time::Duration::from_millis(500));
    }

    #[tokio::test]
    async fn importing_config_reconfigures_request_gate_without_restart() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-import-gate-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        let monitor = MonitorConfig::default();
        app.request_gate
            .reconfigure(&monitor.requests, std::time::Duration::from_secs(5));
        let imported = serde_json::to_string(&ConfigExport {
            config: detail_app_config(),
            products: vec![],
            channels: vec![],
        })
        .unwrap();
        app.import_config(&imported).await.unwrap();

        let (_control, mut receiver) = ControlToken::new();
        let first = app
            .request_gate
            .acquire_scan(65, &mut receiver)
            .await
            .unwrap();
        drop(first);
        let second = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            app.request_gate.acquire_scan(66, &mut receiver),
        )
        .await
        .expect("导入后应立即使用新的扫描节奏")
        .unwrap();
        drop(second);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn importing_exported_channel_without_credentials_keeps_it_unconfigured() {
        let source_dir = std::env::temp_dir().join(format!(
            "ricoh-export-source-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let target_dir = std::env::temp_dir().join(format!(
            "ricoh-export-target-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let source = MonitorApp::open(&source_dir).unwrap();
        source
            .call(|db| {
                db.save_notification_channel(
                    "channel-1",
                    "通知群",
                    "feishu",
                    Some("source-secret"),
                    &["stock_available".into()],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let exported = source.export_config().await.unwrap();
        let target = MonitorApp::open(&target_dir).unwrap();

        target.import_config(&exported).await.unwrap();
        let channel = &target.snapshot().await.unwrap().channels[0];
        assert_eq!(channel.id, "channel-1");
        assert!(!channel.enabled);
        assert!(channel.configured_field_keys.is_empty());
        drop(source);
        drop(target);
        std::fs::remove_dir_all(source_dir).unwrap();
        std::fs::remove_dir_all(target_dir).unwrap();
    }

    #[tokio::test]
    async fn rejected_import_does_not_partially_change_config_or_products() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-import-atomic-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        let before = app.snapshot().await.unwrap();
        let mut import: serde_json::Value =
            serde_json::from_str(&app.export_config().await.unwrap()).unwrap();
        import["config"]["rate"]["intervalMinMs"] = serde_json::json!(1_200);
        import["products"] = serde_json::json!([{
            "productId": "65", "name": "Fixture 65", "enabled": true, "prominentAlert": false
        }]);
        import["channels"] = serde_json::json!([{
            "id": "invalid-channel", "name": "错误渠道", "providerId": "unknown", "subscriptions": []
        }]);

        assert!(app.import_config(&import.to_string()).await.is_err());
        let after = app.snapshot().await.unwrap();
        assert_eq!(after.config, before.config);
        assert!(after.products.is_empty());
        assert_eq!(after.catalog, before.catalog);
        assert!(after.channels.is_empty());
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn default_catalog_is_visible_without_requests_or_persisted_selection() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-default-catalog-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(FixtureClient(AtomicUsize::new(0)));
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        let snapshot = app.snapshot().await.unwrap();
        assert_eq!(
            snapshot
                .catalog
                .iter()
                .map(|product| product.product_id.as_str())
                .collect::<Vec<_>>(),
            [
                "9", "18", "19", "38", "45", "46", "47", "48", "49", "50", "51", "52", "65", "66",
                "67", "108", "114", "122", "123", "124", "130", "245"
            ]
        );
        assert!(snapshot.catalog.iter().all(|product| !product.enabled
            && product.observation.is_none()
            && product.check_count == 0));
        assert!(snapshot.products.is_empty());
        assert_eq!(snapshot.runtime.state, RuntimeState::Stopped);
        assert_eq!(client.0.load(Ordering::Relaxed), 0);
        app.call(|db| {
            assert!(db.product_configs()?.is_empty());
            Ok(())
        })
        .await
        .unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn enabling_default_product_validates_once_and_preserves_existing_products() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-default-enable-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(FixtureClient(AtomicUsize::new(0)));
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        app.call(|db| {
            for (id, name) in [("65", "用户保存名称"), ("999", "用户自定义商品")] {
                db.save_product_config(
                    ProductIdentity {
                        key: id.into(),
                        product_id: id.into(),
                        sku_id: None,
                    },
                    name.into(),
                    "manual".into(),
                    Some(now_ms()),
                )?;
            }
            db.set_product_enabled("999", true)?;
            Ok(())
        })
        .await
        .unwrap();
        let before = app.snapshot().await.unwrap();
        assert_eq!(
            before
                .catalog
                .iter()
                .filter(|product| product.product_id == "65")
                .count(),
            1
        );
        assert_eq!(
            before
                .catalog
                .iter()
                .find(|product| product.product_id == "65")
                .unwrap()
                .name,
            "用户保存名称"
        );

        app.set_product_enabled(108, true).await.unwrap();
        let after = app.snapshot().await.unwrap();
        assert_eq!(client.0.load(Ordering::Relaxed), 1);
        assert_eq!(
            after
                .products
                .iter()
                .map(|product| product.product_id.as_str())
                .collect::<Vec<_>>(),
            ["108", "999"]
        );
        assert!(after
            .catalog
            .iter()
            .all(|product| product.product_id != "108"));
        assert_eq!(after.runtime.state, RuntimeState::Stopped);
        app.set_product_enabled(108, false).await.unwrap();
        app.set_product_enabled(108, true).await.unwrap();
        assert_eq!(client.0.load(Ordering::Relaxed), 1);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn default_product_validation_failure_keeps_selection_empty_and_unverified_entry_is_rechecked(
    ) {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-default-recheck-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(RetryAfterClient {
            calls: AtomicUsize::new(0),
            starts: std::sync::Mutex::new(Vec::new()),
        });
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        assert!(app.set_product_enabled(108, true).await.is_err());
        let snapshot = app.snapshot().await.unwrap();
        assert!(snapshot.products.is_empty());
        assert_eq!(snapshot.runtime.state, RuntimeState::Stopped);
        app.call(|db| {
            assert!(db.product_configs()?.is_empty());
            db.save_product_config(
                ProductIdentity {
                    key: "108".into(),
                    product_id: "108".into(),
                    sku_id: None,
                },
                "尚待验证的会员卡".into(),
                "import".into(),
                None,
            )
        })
        .await
        .unwrap();

        app.set_product_enabled(108, true).await.unwrap();
        assert_eq!(client.calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            app.snapshot().await.unwrap().products[0].name,
            "Fixture 108"
        );
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    struct FixtureClient(AtomicUsize);

    #[tokio::test]
    async fn committed_stock_event_immediately_wakes_all_outputs_and_isolates_slow_routes() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-notification-latency-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open_with_scheduler_client(
            &directory,
            Arc::new(FixtureClient(AtomicUsize::new(0))),
        )
        .unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        let mut config = detail_app_config();
        config.schedule.start = "00:00".into();
        config.schedule.end = "00:00".into();
        app.save_config(config).await.unwrap();
        app.call(|db| {
            db.save_product_config(ProductIdentity {key:"246".into(),product_id:"246".into(),sku_id:None}, "Fixture".into(),"fixture".into(),Some(now_ms()))?;
            db.set_product_enabled("246",true)?;
            db.set_product_prominent_alert("246",true)?;
            db.set_system_notifications_enabled(true)?;
            for (id, delay) in [("a-slow", 1200), ("b-fast", 0)] {
                db.save_credential("channel",id,&serde_json::json!({"appId":"fixture","appSecret":"fixture-secret","targetId":"chat-a","targetKind":"chat","delayMs":delay.to_string()}).to_string())?;
                db.save_notification_channel(id,id,"feishu",Some(id),&["stock_available".into()])?;
                db.mark_notification_channel_tested(id)?;
                db.set_notification_channel_enabled(id,true)?;
            }
            Ok(())
        }).await.unwrap();
        app.complete_setup().await.unwrap();
        app.configure_notification_runtime(None).await.unwrap();
        let runner = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(
                app.wait_for_system_notification(),
                app.wait_for_prominent_alert()
            );
        })
        .await
        .expect("真实库存提交应唤醒两个桌面出口");
        let system = app.claim_system_notification().await.unwrap().unwrap();
        let prominent = app.claim_prominent_alert().await.unwrap().unwrap();
        assert_eq!(system.1.id, prominent.event_id);
        let event_time = app
            .call(|db| Ok(db.recent_events(1)?.last().unwrap().observed_at_ms))
            .await
            .unwrap();
        let desktop_ms = now_ms() - event_time;
        tokio::time::timeout(std::time::Duration::from_millis(600), async {
            loop {
                let sent = app
                    .call(|db| {
                        Ok(db
                            .notification_delivery("b-fast")?
                            .is_some_and(|d| d.outcome == "accepted"))
                    })
                    .await
                    .unwrap();
                if sent {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("慢路由不应阻塞快路由");
        let fast_ms = now_ms() - event_time;
        assert!(!app
            .call(|db| Ok(db
                .notification_delivery("a-slow")?
                .is_some_and(|d| d.outcome == "accepted")))
            .await
            .unwrap());
        println!(
            "库存结果至桌面领取 {desktop_ms} ms，快渠道接受 {fast_ms} ms（另一路延迟 1200 ms）"
        );
        let shutdown = std::time::Instant::now();
        app.shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(2), runner)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!app.notification_runtime.is_running().await);
        println!("并行发送中退出 {:?}", shutdown.elapsed());
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }
    impl crate::scheduler::SchedulerClient for FixtureClient {
        fn product_detail(
            &self,
            line: LineId,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<ProductDetail, crate::ricoh_api::RicohApiError>>
                    + Send
                    + 'static,
            >,
        > {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                Ok(ProductDetail {
                    metadata: Default::default(),
                    product_id: line.product_id,
                    name: format!("Fixture {}", line.product_id),
                    is_show: 1,
                    stock: serde_json::json!(1).as_number().unwrap().clone(),
                    availability: Availability::InStock,
                })
            })
        }
    }

    #[tokio::test]
    async fn removing_an_unconfigured_default_product_persists_until_explicitly_added() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-remove-default-product-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(FixtureClient(AtomicUsize::new(0)));
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        assert!(app
            .snapshot()
            .await
            .unwrap()
            .catalog
            .iter()
            .any(|product| product.product_id == "65"));
        app.remove_product(65).await.unwrap();
        assert!(!app
            .snapshot()
            .await
            .unwrap()
            .catalog
            .iter()
            .any(|product| product.product_id == "65"));
        drop(app);

        let app = MonitorApp::open_with_scheduler_client(&directory, client).unwrap();
        assert!(!app
            .snapshot()
            .await
            .unwrap()
            .catalog
            .iter()
            .any(|product| product.product_id == "65"));
        app.set_onboarding_products(&[65]).await.unwrap();
        assert!(app
            .snapshot()
            .await
            .unwrap()
            .products
            .iter()
            .any(|product| product.product_id == "65"));
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    struct HeldProductClient {
        first_started: tokio::sync::Notify,
        release_first: Arc<tokio::sync::Notify>,
        first_attempts: AtomicUsize,
        second_attempts: AtomicUsize,
    }

    #[tokio::test]
    async fn cancelling_the_first_add_while_validation_is_pending_does_not_enable_it_later() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-cancel-add-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(HeldProductClient {
            first_started: tokio::sync::Notify::new(),
            release_first: Arc::new(tokio::sync::Notify::new()),
            first_attempts: AtomicUsize::new(0),
            second_attempts: AtomicUsize::new(0),
        });
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        let pending = tokio::spawn({
            let app = app.clone();
            async move { app.add_product(65).await }
        });
        client.first_started.notified().await;
        assert!(app.add_product(65).await.is_err());
        app.remove_product(65).await.unwrap();
        client.release_first.notify_one();
        assert!(pending.await.unwrap().is_err());
        assert!(app.snapshot().await.unwrap().products.is_empty());
        assert!(app.pending_products.lock().unwrap().is_empty());
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    struct MetadataClient(AtomicUsize);
    impl SchedulerClient for MetadataClient {
        fn product_detail(
            &self,
            line: LineId,
        ) -> Pin<
            Box<dyn Future<Output = Result<ProductDetail, crate::ricoh_api::RicohApiError>> + Send>,
        > {
            let attempt = self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                Ok(ProductDetail {
                    product_id: line.product_id,
                    name: "真实资料 Fixture".into(),
                    is_show: 1,
                    stock: serde_json::json!(1).as_number().unwrap().clone(),
                    availability: Availability::InStock,
                    metadata: ProductMetadata {
                        price: Some(if attempt == 0 { "1.00" } else { "2.00" }.into()),
                        ..Default::default()
                    },
                })
            })
        }
    }

    #[tokio::test]
    async fn metadata_and_daily_statistics_share_the_accepted_commit_and_duration_survives_pause_and_reopen(
    ) {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-display-contract-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(MetadataClient(AtomicUsize::new(0)));
        let app = MonitorApp::open_with_scheduler_client(&directory, client).unwrap();
        let added = app.add_product(65).await.unwrap();
        assert!(added.enabled);
        assert_eq!(added.metadata.unwrap().price.as_deref(), Some("1.00"));
        assert_eq!(added.today_check_count, 0);
        let mut config = detail_app_config();
        config.schedule.start = "00:00".into();
        config.schedule.end = "00:00".into();
        config.rate.interval_min_ms = 50;
        config.rate.interval_max_ms = 50;
        app.save_config(config).await.unwrap();
        app.complete_setup().await.unwrap();
        app.monitoring_action(MonitoringAction::Start)
            .await
            .unwrap();
        let runner = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let snapshot = app.snapshot().await.unwrap();
                if snapshot.products[0].today_check_count >= 2
                    && snapshot.products[0].monitoring_ms >= 150
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        app.monitoring_action(MonitoringAction::Pause)
            .await
            .unwrap();
        let paused = app.snapshot().await.unwrap().products.remove(0);
        assert_eq!(paused.today_check_count, paused.today_success_count);
        assert_eq!(paused.today_failure_count, 0);
        assert_eq!(paused.check_count, paused.today_check_count);
        assert_eq!(paused.metadata.unwrap().price.as_deref(), Some("2.00"));
        tokio::time::sleep(std::time::Duration::from_millis(450)).await;
        assert_eq!(
            app.snapshot().await.unwrap().products[0].monitoring_ms,
            paused.monitoring_ms
        );
        app.shutdown();
        runner.await.unwrap().unwrap();
        drop(app);
        let reopened = MonitorApp::open(&directory).unwrap();
        assert_eq!(
            reopened.snapshot().await.unwrap().products[0].monitoring_ms,
            paused.monitoring_ms
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }

    impl crate::scheduler::SchedulerClient for HeldProductClient {
        fn product_detail(
            &self,
            line: LineId,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<ProductDetail, crate::ricoh_api::RicohApiError>>
                    + Send
                    + 'static,
            >,
        > {
            let first = line.product_id == 65;
            if first {
                self.first_attempts.fetch_add(1, Ordering::Relaxed);
                self.first_started.notify_one();
            } else {
                self.second_attempts.fetch_add(1, Ordering::Relaxed);
            }
            let release = self.release_first.clone();
            Box::pin(async move {
                if first {
                    release.notified().await;
                }
                Ok(ProductDetail {
                    metadata: Default::default(),
                    product_id: line.product_id,
                    name: "Fixture".into(),
                    is_show: 0,
                    stock: serde_json::json!(0).as_number().unwrap().clone(),
                    availability: Availability::OutOfStock,
                })
            })
        }
    }

    #[tokio::test]
    async fn enabling_product_does_not_restart_existing_in_flight_request() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-live-products-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(HeldProductClient {
            first_started: tokio::sync::Notify::new(),
            release_first: Arc::new(tokio::sync::Notify::new()),
            first_attempts: AtomicUsize::new(0),
            second_attempts: AtomicUsize::new(0),
        });
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        app.set_platform_notifications_available(true);
        let mut config = detail_app_config();
        config.schedule.start = "00:00".into();
        config.schedule.end = "00:00".into();
        app.save_config(config).await.unwrap();
        app.call(|db| {
            for id in [65, 66] {
                db.save_product_config(
                    ProductIdentity {
                        key: id.to_string(),
                        product_id: id.to_string(),
                        sku_id: None,
                    },
                    "Fixture".into(),
                    "fixture".into(),
                    Some(now_ms()),
                )?;
            }
            db.set_product_enabled("65", true)
        })
        .await
        .unwrap();
        app.complete_setup().await.unwrap();
        app.monitoring_action(MonitoringAction::Start)
            .await
            .unwrap();
        let runner = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.first_started.notified(),
        )
        .await
        .unwrap();
        app.set_product_enabled(66, true).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while client.second_attempts.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(client.first_attempts.load(Ordering::Relaxed), 1);
        app.set_product_enabled(65, false).await.unwrap();
        app.set_product_enabled(65, true).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while client.first_attempts.load(Ordering::Relaxed) < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("快速停用再启用必须启动新的商品请求");
        client.release_first.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while app
                .snapshot()
                .await
                .unwrap()
                .products
                .iter()
                .find(|p| p.product_id == "65")
                .unwrap()
                .check_count
                == 0
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        app.set_product_enabled(66, false).await.unwrap();
        app.runtime_errors
            .lock()
            .unwrap()
            .insert("66".into(), "旧错误".into());
        let snapshot = app.snapshot().await.unwrap();
        assert_eq!(snapshot.runtime.state, RuntimeState::Monitoring);
        assert_eq!(snapshot.catalog[0].runtime_error, None);
        app.shutdown();
        runner.await.unwrap().unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn disabling_last_product_stops_requests_and_clears_its_error() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-disable-last-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(FixtureClient(AtomicUsize::new(0)));
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        app.set_platform_notifications_available(true);
        let mut config = detail_app_config();
        config.schedule.start = "00:00".into();
        config.schedule.end = "00:00".into();
        config.rate.interval_min_ms = 50;
        config.rate.interval_max_ms = 50;
        app.save_config(config).await.unwrap();
        app.call(|db| {
            db.save_product_config(
                ProductIdentity {
                    key: "65".into(),
                    product_id: "65".into(),
                    sku_id: None,
                },
                "Fixture".into(),
                "fixture".into(),
                Some(now_ms()),
            )?;
            db.set_product_enabled("65", true)
        })
        .await
        .unwrap();
        app.complete_setup().await.unwrap();
        app.monitoring_action(MonitoringAction::Start)
            .await
            .unwrap();
        let runner = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while client.0.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        app.runtime_errors
            .lock()
            .unwrap()
            .insert("65".into(), "旧错误".into());
        app.set_product_enabled(65, false).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let count = client.0.load(Ordering::Relaxed);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(client.0.load(Ordering::Relaxed), count);
        assert_eq!(app.snapshot().await.unwrap().catalog[0].runtime_error, None);
        app.shutdown();
        runner.await.unwrap().unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn failed_product_toggle_does_not_advance_lifecycle() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-failed-toggle-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        assert!(app.set_product_enabled(65, false).await.is_err());
        assert!(app.product_generations.lock().unwrap().is_empty());
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    struct RetryAfterClient {
        calls: AtomicUsize,
        starts: std::sync::Mutex<Vec<std::time::Instant>>,
    }

    struct RoutedRetryClient {
        calls: AtomicUsize,
        outlets: std::sync::Mutex<Vec<(String, std::time::Instant)>>,
    }

    struct RouteSuccessClient(std::sync::Mutex<Vec<String>>);

    impl crate::scheduler::SchedulerClient for RouteSuccessClient {
        fn product_detail(
            &self,
            line: LineId,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<ProductDetail, crate::ricoh_api::RicohApiError>>
                    + Send
                    + 'static,
            >,
        > {
            self.0.lock().unwrap().push(line.outlet_id);
            Box::pin(async move {
                Ok(ProductDetail {
                    metadata: Default::default(),
                    product_id: line.product_id,
                    name: "Fixture".into(),
                    is_show: 0,
                    stock: serde_json::json!(0).as_number().unwrap().clone(),
                    availability: Availability::OutOfStock,
                })
            })
        }
    }

    impl crate::scheduler::SchedulerClient for RoutedRetryClient {
        fn product_detail(
            &self,
            line: LineId,
        ) -> Pin<
            Box<dyn Future<Output = Result<ProductDetail, crate::ricoh_api::RicohApiError>> + Send>,
        > {
            let attempt = self.calls.fetch_add(1, Ordering::Relaxed);
            self.outlets
                .lock()
                .unwrap()
                .push((line.outlet_id.clone(), std::time::Instant::now()));
            Box::pin(async move {
                if attempt == 0 {
                    return Err(crate::ricoh_api::RicohApiError::RateLimited {
                        retry_after: Some(crate::ricoh_api::RetryAfter::Delay(
                            std::time::Duration::from_millis(300),
                        )),
                    });
                }
                Ok(ProductDetail {
                    metadata: Default::default(),
                    product_id: line.product_id,
                    name: format!("Fixture {}", line.product_id),
                    is_show: 0,
                    stock: serde_json::json!(0).as_number().unwrap().clone(),
                    availability: Availability::OutOfStock,
                })
            })
        }
    }

    impl crate::scheduler::SchedulerClient for RetryAfterClient {
        fn product_detail(
            &self,
            line: LineId,
        ) -> Pin<
            Box<dyn Future<Output = Result<ProductDetail, crate::ricoh_api::RicohApiError>> + Send>,
        > {
            let attempt = self.calls.fetch_add(1, Ordering::Relaxed);
            self.starts.lock().unwrap().push(std::time::Instant::now());
            Box::pin(async move {
                if attempt == 0 {
                    return Err(crate::ricoh_api::RicohApiError::RateLimited {
                        retry_after: Some(crate::ricoh_api::RetryAfter::Delay(
                            std::time::Duration::from_millis(300),
                        )),
                    });
                }
                Ok(ProductDetail {
                    metadata: Default::default(),
                    product_id: line.product_id,
                    name: format!("Fixture {}", line.product_id),
                    is_show: 1,
                    stock: serde_json::json!(1).as_number().unwrap().clone(),
                    availability: Availability::InStock,
                })
            })
        }
    }

    #[cfg(unix)]
    #[test]
    fn new_app_data_directory_and_database_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-private-data-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&directory), 0o700);
        assert_eq!(mode(&directory.join("monitor.sqlite3")), 0o600);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn restore_defaults_removes_saved_setup_and_credentials() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-restore-defaults-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.call(|db| {
            db.save_product_config(
                ProductIdentity {
                    key: "247".into(),
                    product_id: "247".into(),
                    sku_id: None,
                },
                "Fixture".into(),
                "fixture".into(),
                Some(now_ms()),
            )?;
            db.set_product_enabled("247", true)?;
            db.save_notification_channel("alerts", "Alerts", "feishu", Some("secret"), &[])?;
            db.save_credential(
                "channel",
                "secret",
                "{\"webhookUrl\":\"https://example.invalid/hook\"}",
            )?;
            db.save_proxy(&crate::storage::StoredProxy {
                id: "proxy".into(),
                protocol: "http".into(),
                host: "127.0.0.1".into(),
                port: 8080,
                credential_ref: Some("proxy".into()),
                enabled: true,
                status: "available".into(),
                cooldown_until_ms: None,
                consecutive_failures: 0,
            })?;
            db.save_credential("proxy", "proxy", "password")?;
            db.set_run_intent(RunIntent::Running, false)?;
            db.set_system_notifications_enabled(false)?;
            db.set_scan_active(true)?;
            Ok(())
        })
        .await
        .unwrap();

        app.restore_defaults(false).await.unwrap();
        let snapshot = app.snapshot().await.unwrap();
        assert!(!snapshot.setup_completed);
        assert!(snapshot.products.is_empty());
        assert_eq!(snapshot.catalog.len(), DEFAULT_PRODUCTS.len());
        assert!(snapshot
            .catalog
            .iter()
            .all(|product| product.observation.is_none()));
        assert!(snapshot.channels.is_empty());
        assert!(snapshot.proxies.is_empty());
        assert_eq!(snapshot.runtime.state, RuntimeState::Stopped);
        assert!(snapshot.system_notifications_enabled);
        assert!(snapshot.scan.is_none());
        app.call(|db| {
            assert_eq!(db.credential("channel", "secret")?, None);
            assert_eq!(db.credential("proxy", "proxy")?, None);
            assert!(!db.scan_active()?);
            Ok(())
        })
        .await
        .unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn setup_requires_explicit_completion_and_survives_restart_until_restored() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-setup-completion-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_platform_notifications_available(true);
        app.call(|db| {
            db.save_product_config(
                ProductIdentity {
                    key: "65".into(),
                    product_id: "65".into(),
                    sku_id: None,
                },
                "Fixture 65".into(),
                "fixture".into(),
                Some(now_ms()),
            )?;
            db.set_product_enabled("65", true)
        })
        .await
        .unwrap();
        assert!(!app.snapshot().await.unwrap().setup_completed);
        assert!(app
            .monitoring_action(MonitoringAction::Start)
            .await
            .is_err());
        drop(app);

        let app = MonitorApp::open(&directory).unwrap();
        app.set_platform_notifications_available(true);
        assert!(!app.snapshot().await.unwrap().setup_completed);
        app.complete_setup().await.unwrap();
        assert!(app.snapshot().await.unwrap().setup_completed);
        drop(app);

        let app = MonitorApp::open(&directory).unwrap();
        app.set_platform_notifications_available(true);
        let restarted = app.snapshot().await.unwrap();
        assert!(restarted.setup_completed);
        assert!(matches!(
            restarted.runtime.state,
            RuntimeState::Monitoring | RuntimeState::OutsideSchedule
        ));
        app.restore_defaults(false).await.unwrap();
        assert!(!app.snapshot().await.unwrap().setup_completed);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn startup_monitoring_preference_is_independent_and_persisted() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-startup-monitoring-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        assert!(app.snapshot().await.unwrap().config.auto_start_monitoring);
        app.apply_startup_monitoring().unwrap();
        assert!(!app.snapshot().await.unwrap().setup_completed);
        assert_eq!(
            app.call(|db| db.run_intent()).await.unwrap(),
            RunIntent::Stopped
        );
        app.call(|db| {
            db.save_product_config(
                ProductIdentity {
                    key: "65".into(),
                    product_id: "65".into(),
                    sku_id: None,
                },
                "Fixture".into(),
                "fixture".into(),
                Some(now_ms()),
            )?;
            db.set_product_enabled("65", true)
        })
        .await
        .unwrap();
        app.complete_setup().await.unwrap();
        app.monitoring_action(MonitoringAction::Pause)
            .await
            .unwrap();
        app.apply_startup_monitoring().unwrap();
        assert_eq!(
            app.call(|db| db.run_intent()).await.unwrap(),
            RunIntent::Running
        );
        app.set_auto_start_monitoring(false).await.unwrap();
        assert_eq!(
            app.call(|db| db.run_intent()).await.unwrap(),
            RunIntent::Running
        );
        drop(app);
        let app = MonitorApp::open(&directory).unwrap();
        assert!(!app.snapshot().await.unwrap().config.auto_start_monitoring);
        app.apply_startup_monitoring().unwrap();
        assert_eq!(
            app.snapshot().await.unwrap().runtime.state,
            RuntimeState::Stopped
        );
        app.complete_setup().await.unwrap();
        assert_eq!(
            app.call(|db| db.run_intent()).await.unwrap(),
            RunIntent::Stopped
        );
        app.monitoring_action(MonitoringAction::Start)
            .await
            .unwrap();
        assert_eq!(
            app.call(|db| db.run_intent()).await.unwrap(),
            RunIntent::Running
        );
        let generations = app.call(|db| db.product_generations()).await.unwrap();
        app.complete_setup().await.unwrap();
        assert_eq!(
            app.call(|db| db.run_intent()).await.unwrap(),
            RunIntent::Stopped
        );
        let next = app.call(|db| db.product_generations()).await.unwrap();
        assert!(next
            .iter()
            .all(|(id, generation)| *generation > generations[id]));
        assert_eq!(*app.product_generations.lock().unwrap(), next);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn enabled_product_can_complete_setup_and_monitor_without_notifications() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-no-notifications-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_platform_notifications_available(false);
        app.set_system_notifications_enabled(false).await.unwrap();
        app.call(|db| {
            db.save_product_config(
                ProductIdentity {
                    key: "65".into(),
                    product_id: "65".into(),
                    sku_id: None,
                },
                "Fixture 65".into(),
                "fixture".into(),
                Some(now_ms()),
            )?;
            db.set_product_enabled("65", true)
        })
        .await
        .unwrap();
        assert!(!app.snapshot().await.unwrap().setup_completed);
        assert!(app
            .monitoring_action(MonitoringAction::Start)
            .await
            .is_err());
        app.complete_setup().await.unwrap();
        app.monitoring_action(MonitoringAction::Start)
            .await
            .unwrap();
        assert!(app.snapshot().await.unwrap().setup_completed);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn scan_snapshot_reports_the_actual_in_flight_product_id() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-scan-current-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        let started = app.start_product_scan(300, 301).await.unwrap();
        assert_eq!(started.current_id, None);
        {
            let mut scan = app.scan.lock().await;
            assert_eq!(scan.as_mut().unwrap().next_id(), Some(300));
        }
        assert_eq!(
            app.snapshot().await.unwrap().scan.unwrap().current_id,
            Some("300".into())
        );
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn legacy_references_without_sqlite_credentials_are_incomplete_and_never_send_unauthenticated_requests(
    ) {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-legacy-credential-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(FixtureClient(AtomicUsize::new(0)));
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        app.set_platform_notifications_available(false);
        app.call(|db| {
            db.save_product_config(
                ProductIdentity {
                    key: "247".into(),
                    product_id: "247".into(),
                    sku_id: None,
                },
                "Fixture".into(),
                "fixture".into(),
                Some(now_ms()),
            )?;
            db.set_product_enabled("247", true)?;
            db.save_notification_channel(
                "legacy-channel",
                "Legacy",
                "feishu",
                Some("legacy-channel"),
                &[],
            )?;
            db.mark_notification_channel_tested("legacy-channel")?;
            db.set_notification_channel_enabled("legacy-channel", true)?;
            db.save_proxy(&crate::storage::StoredProxy {
                id: "legacy-proxy".into(),
                protocol: "http".into(),
                host: "127.0.0.1".into(),
                port: 8888,
                credential_ref: Some("legacy-proxy".into()),
                enabled: true,
                status: "available".into(),
                cooldown_until_ms: None,
                consecutive_failures: 0,
            })?;
            db.set_run_intent(RunIntent::Running, false)
        })
        .await
        .unwrap();

        let snapshot = app.snapshot().await.unwrap();
        assert!(!snapshot.setup_completed);
        assert!(snapshot.channels[0].configured_field_keys.is_empty());
        assert_eq!(
            app.test_proxy("legacy-proxy")
                .await
                .unwrap_err()
                .to_string(),
            "代理凭据缺失，请重新添加带认证信息的代理"
        );

        let running = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert_eq!(client.0.load(Ordering::Relaxed), 0);
        app.shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn running_snapshot_outside_schedule_shows_next_beijing_start() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-outside-schedule-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_platform_notifications_available(true);
        app.set_system_notifications_enabled(true).await.unwrap();
        let tomorrow = time::OffsetDateTime::now_utc()
            .to_offset(time::UtcOffset::from_hms(8, 0, 0).unwrap())
            .date()
            .next_day()
            .unwrap();
        let mut config = detail_app_config();
        config.schedule.days = vec![
            [
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
                Weekday::Sat,
                Weekday::Sun,
            ][tomorrow.weekday().number_days_from_monday() as usize],
        ];
        app.save_config(config).await.unwrap();
        app.call(|db| {
            db.save_product_config(
                ProductIdentity {
                    key: "246".into(),
                    product_id: "246".into(),
                    sku_id: None,
                },
                "Fixture 246".into(),
                "fixture".into(),
                Some(now_ms()),
            )?;
            db.set_product_enabled("246", true)
        })
        .await
        .unwrap();
        app.complete_setup().await.unwrap();

        let snapshot = app.snapshot().await.unwrap();
        assert_eq!(snapshot.runtime.state, RuntimeState::OutsideSchedule);
        assert_eq!(
            snapshot.runtime.next_start_at.as_deref(),
            Some(
                format!(
                    "{:04}-{:02}-{:02}T09:00:00+08:00",
                    tomorrow.year(),
                    tomorrow.month() as u8,
                    tomorrow.day()
                )
                .as_str()
            )
        );
        app.monitoring_action(MonitoringAction::Pause)
            .await
            .unwrap();
        let paused = app.snapshot().await.unwrap();
        assert_eq!(paused.runtime.state, RuntimeState::Paused);
        assert_eq!(paused.runtime.next_start_at, None);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn shared_run_commits_fixture_observation_and_emits_snapshot_without_network() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-fixture-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let client = Arc::new(FixtureClient(AtomicUsize::new(0)));
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        app.set_platform_notifications_available(true);
        let mut config = detail_app_config();
        config.schedule.start = "00:00".into();
        config.schedule.end = "23:59".into();
        config.rate.interval_min_ms = 50;
        config.rate.interval_max_ms = 50;
        app.save_config(config).await.unwrap();
        app.call(|db| {
            db.save_product_config(
                ProductIdentity {
                    key: "246".into(),
                    product_id: "246".into(),
                    sku_id: None,
                },
                "Fixture 246".into(),
                "fixture".into(),
                Some(now_ms()),
            )?;
            db.set_product_enabled("246", true)
        })
        .await
        .unwrap();
        app.complete_setup().await.unwrap();
        let task = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if app
                    .snapshot()
                    .await
                    .unwrap()
                    .recent_events
                    .iter()
                    .any(|event| event.kind == "stock_available")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            }
        })
        .await
        .unwrap();
        assert!(client.0.load(Ordering::Relaxed) > 0);
        assert_eq!(
            app.snapshot().await.unwrap().products[0]
                .observation
                .as_ref()
                .unwrap()
                .availability,
            "in_stock"
        );
        app.monitoring_action(MonitoringAction::Stop).await.unwrap();
        app.shutdown();
        task.await.unwrap().unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn restart_does_not_block_fresh_stock_behind_an_interrupted_send() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-outbox-restart-latency-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.call(|db| {
            let product = ProductIdentity{key:"247".into(),product_id:"247".into(),sku_id:None};
            db.save_product_config(product.clone(),"Fixture".into(),"fixture".into(),Some(now_ms()))?;
            db.set_product_enabled("247",true)?;
            db.save_credential("channel","fixture-channel",&serde_json::json!({"appId":"fixture","appSecret":"fixture-secret","targetId":"chat-a","targetKind":"chat"}).to_string())?;
            db.save_notification_channel("fixture-channel","Fixture","feishu",Some("fixture-channel"),&["stock_available".into()])?;
            db.mark_notification_channel_tested("fixture-channel")?;
            db.set_notification_channel_enabled("fixture-channel",true)?;
            for (sequence,stock) in [(1,3.0),(2,5.0)] {
                let mut reducer = ObservationReducer::new(db.observation("247")?.map(|saved|saved.state));
                let PrepareOutcome::Prepared(prepared) = reducer.prepare(AvailabilityResponse{sequence,generation:0,result:Ok(ObservationResult{availability:Availability::InStock,is_show:1,stock:Some(stock),observed_at_ms:now_ms()})},RuntimeGate{generation:0,enabled:true,paused:false}) else {unreachable!()};
                db.commit_observation(product.clone(),prepared,&[])?;
                if sequence == 1 { assert!(db.claim_channel_notification(now_ms())?.is_some()); }
            }
            Ok(())
        }).await.unwrap();
        drop(app);
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        let runner = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::timeout(std::time::Duration::from_millis(700), async {
            loop {
                if app
                    .call(|db| {
                        Ok(db
                            .notification_delivery("fixture-channel")?
                            .is_some_and(|result| result.outcome == "accepted"))
                    })
                    .await
                    .unwrap()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("重开后新库存被旧在途记录的租期阻塞");
        assert_eq!(
            app.notification_runtime
                .request("status", serde_json::json!({}))
                .await
                .unwrap()["sendCounts"]["fixture-channel"],
            1
        );
        app.shutdown();
        runner.await.unwrap().unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn queued_notification_retries_finitely_without_duplicate_events() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-outbox-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        let event = app.call(|db| {
            let product = ProductIdentity {key:"247".into(),product_id:"247".into(),sku_id:None};
            db.save_product_config(product.clone(),"Fixture".into(),"fixture".into(),Some(now_ms()))?;
            db.set_product_enabled("247",true)?;
            db.save_credential("channel","fixture-channel",&serde_json::json!({"appId":"fixture","appSecret":"fixture-secret","targetId":"chat-a","targetKind":"chat","fixtureOutcome":"failed","retryable":"true"}).to_string())?;
            db.save_notification_channel("fixture-channel","Fixture","feishu",Some("fixture-channel"),&["stock_available".into()])?;
            db.mark_notification_channel_tested("fixture-channel")?;
            db.set_notification_channel_enabled("fixture-channel",true)?;
            let mut reducer = ObservationReducer::new(None);
            let PrepareOutcome::Prepared(prepared) = reducer.prepare(AvailabilityResponse {
                sequence:1,generation:0,result:Ok(ObservationResult {availability:Availability::InStock,is_show:1,stock:Some(1.0),observed_at_ms:now_ms()})
            },RuntimeGate{generation:0,enabled:true,paused:false}) else {unreachable!()};
            db.commit_observation(product,prepared,&[])?;
            Ok(db.recent_events(1)?.into_iter().next().unwrap())
        }).await.unwrap();
        let result = app
            .send_event(NotificationJob {
                event,
                subscription: "stock_available",
                channel_id: None,
                generation: None,
                target: None,
                waited_for_connection: false,
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.0, "failed");
        let status = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(status["sendCounts"]["fixture-channel"], 3);
        assert_eq!(
            app.snapshot()
                .await
                .unwrap()
                .recent_events
                .iter()
                .filter(|event| event.kind == "stock_available")
                .count(),
            1
        );
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn channel_edit_merges_application_credentials_and_keeps_secrets_private() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-secret-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.save_channel(ChannelInput {
            id: Some("channel-edit".into()),
            name: "Fixture".into(),
            provider_id: "feishu".into(),
            binding_id: None,
            targets: None,
            values: BTreeMap::from([
                ("appId".into(), "old-app".into()),
                ("appSecret".into(), "private-app-secret".into()),
                ("targetId".into(), "chat-a".into()),
                ("targetKind".into(), "chat".into()),
            ]),
            subscriptions: vec![ChannelEvent::StockAvailable],
        })
        .await
        .unwrap();
        let updated = app
            .save_channel(ChannelInput {
                id: Some("channel-edit".into()),
                name: "Fixture".into(),
                provider_id: "feishu".into(),
                binding_id: None,
                targets: None,
                values: BTreeMap::from([("appId".into(), "new-app".into())]),
                subscriptions: vec![ChannelEvent::StockAvailable],
            })
            .await
            .unwrap();
        assert_eq!(updated.target_id.as_deref(), Some("chat-a"));
        assert_eq!(
            updated.configured_field_keys,
            ["appId", "appSecret", "targetId", "targetKind"]
        );
        assert!(!serde_json::to_string(&app.snapshot().await.unwrap())
            .unwrap()
            .contains("private-app-secret"));
        assert!(!app
            .export_config()
            .await
            .unwrap()
            .contains("private-app-secret"));
        let saved = app
            .call(|db| db.credential("channel", "channel-edit"))
            .await
            .unwrap()
            .unwrap();
        let values: BTreeMap<String, String> = serde_json::from_str(&saved).unwrap();
        assert_eq!(values["appSecret"], "private-app-secret");
        assert_eq!(values["appId"], "new-app");
        assert_eq!(provider_definitions().len(), 4);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn provider_definitions_are_exactly_four_binding_platforms() {
        let providers = provider_definitions();
        assert_eq!(
            providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            ["feishu", "wecom", "dingtalk", "weixin"]
        );
        assert!(providers.iter().all(|provider| provider.supports_binding));
    }

    #[test]
    fn stable_target_identity_merges_platform_aliases_without_merging_namesakes() {
        let values = BTreeMap::from([
            (
                "_sdkCredentials".into(),
                serde_json::json!({"targetAliases":{"opaque":"staff-1"}}).to_string(),
            ),
            (
                "_targets".into(),
                serde_json::json!([
                    {"id":"opaque","kind":"user","label":"成员"},
                    {"id":"staff-1","kind":"user","label":"成员"},
                    {"id":"staff-2","kind":"user","label":"成员"}
                ])
                .to_string(),
            ),
        ]);
        let targets = channel_targets(&values);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].id, "staff-1");
        assert_eq!(targets[1].id, "staff-2");
    }

    #[tokio::test]
    async fn editing_and_metadata_save_keep_enabled_connection_and_test_result() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-edit-no-reconnect-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        app.save_channel(ChannelInput {
            id: Some("unchanged".into()),
            name: "原名称".into(),
            provider_id: "dingtalk".into(),
            binding_id: None,
            targets: Some(vec![NotificationTarget {
                id: "chat-a".into(),
                kind: "chat".into(),
                label: "群聊 A".into(),
            }]),
            values: BTreeMap::from([
                ("appId".into(), "fixture".into()),
                ("appSecret".into(), "fixture-secret".into()),
            ]),
            subscriptions: vec![ChannelEvent::StockAvailable],
        })
        .await
        .unwrap();
        assert_eq!(
            app.test_channel("unchanged").await.unwrap().outcome,
            "accepted"
        );
        app.set_channel_enabled("unchanged", true).await.unwrap();
        app.configure_notification_runtime(None).await.unwrap();
        let before = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        let generation = app.notification_runtime.account_generation("unchanged");
        for _ in 0..10 {
            app.begin_channel_editing("unchanged").await.unwrap();
            app.end_channel_editing("unchanged").await.unwrap();
        }
        let after = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(before["pid"], after["pid"]);
        assert_eq!(before["configured"], after["configured"]);
        let saved = app
            .save_channel(ChannelInput {
                id: Some("unchanged".into()),
                name: "新名称".into(),
                provider_id: "dingtalk".into(),
                binding_id: None,
                targets: None,
                values: BTreeMap::new(),
                subscriptions: vec![ChannelEvent::StockAvailable],
            })
            .await
            .unwrap();
        assert!(saved.enabled);
        assert_eq!(saved.last_test.unwrap().outcome, "accepted");
        assert_eq!(
            app.notification_runtime.account_generation("unchanged"),
            generation
        );
        let after_save = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(before["pid"], after_save["pid"]);
        assert_eq!(before["configured"], after_save["configured"]);
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn platform_identity_update_merges_saved_routes_and_survives_reopen() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-target-alias-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        app.save_channel(ChannelInput {
            id: Some("aliases".into()),
            name: "助手".into(),
            provider_id: "dingtalk".into(),
            binding_id: None,
            targets: Some(
                ["opaque", "staff-1", "staff-2"]
                    .map(|id| NotificationTarget {
                        id: id.into(),
                        kind: "user".into(),
                        label: "同名成员".into(),
                    })
                    .to_vec(),
            ),
            values: BTreeMap::from([
                ("appId".into(), "fixture".into()),
                ("appSecret".into(), "fixture".into()),
            ]),
            subscriptions: vec![ChannelEvent::StockAvailable],
        })
        .await
        .unwrap();
        app.begin_channel_editing("aliases").await.unwrap();
        app.notification_runtime
            .request(
                "fixture_target_alias",
                serde_json::json!({"accountId":"aliases","aliases":{"opaque":"staff-1"}}),
            )
            .await
            .unwrap();
        app.configure_notification_runtime(None).await.unwrap();
        let snapshot = app.snapshot().await.unwrap();
        assert_eq!(snapshot.channels[0].selected_targets.len(), 2);
        assert_eq!(snapshot.channels[0].selected_targets[0].id, "staff-1");
        assert_eq!(snapshot.channels[0].selected_targets[1].id, "staff-2");
        assert_eq!(snapshot.channels[0].targets.len(), 2);
        // 模拟别名更新前打开的表单提交旧身份，保存时仍以最新平台身份为准。
        let generation = app.notification_runtime.account_generation("aliases");
        let saved = app
            .save_channel(ChannelInput {
                id: Some("aliases".into()),
                name: "新名称".into(),
                provider_id: "dingtalk".into(),
                binding_id: None,
                targets: Some(
                    ["opaque", "staff-1", "staff-2"]
                        .map(|id| NotificationTarget {
                            id: id.into(),
                            kind: "user".into(),
                            label: "同名成员".into(),
                        })
                        .to_vec(),
                ),
                values: BTreeMap::new(),
                subscriptions: vec![ChannelEvent::StockAvailable],
            })
            .await
            .unwrap();
        assert_eq!(saved.selected_targets.len(), 2);
        assert_eq!(
            app.notification_runtime.account_generation("aliases"),
            generation
        );
        app.shutdown_notification_runtime().await;
        drop(app);
        let reopened = MonitorApp::open(&directory).unwrap();
        let snapshot = reopened.snapshot().await.unwrap();
        assert_eq!(snapshot.channels[0].selected_targets.len(), 2);
        assert_eq!(snapshot.channels[0].targets.len(), 2);
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn unselected_detected_groups_are_loaded_into_runtime_after_reopen() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-known-groups-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs");
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(fixture.clone());
        app.save_channel(ChannelInput {
            id: Some("known-groups".into()),
            name: "测试通知".into(),
            provider_id: "dingtalk".into(),
            binding_id: None,
            targets: Some(vec![NotificationTarget {
                id: "chat-a".into(),
                kind: "chat".into(),
                label: "群组 A".into(),
            }]),
            values: BTreeMap::from([
                ("appId".into(), "fixture-app".into()),
                ("appSecret".into(), "fixture-secret".into()),
            ]),
            subscriptions: vec![],
        })
        .await
        .unwrap();
        assert_eq!(
            app.detect_channel_groups("known-groups")
                .await
                .unwrap()
                .len(),
            2
        );
        app.shutdown_notification_runtime().await;
        drop(app);
        let reopened = MonitorApp::open(&directory).unwrap();
        reopened.set_notification_runtime_path(fixture);
        reopened
            .begin_channel_editing("known-groups")
            .await
            .unwrap();
        let status = reopened
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            status["configuration"]["accounts"][0]["targets"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            reopened.snapshot().await.unwrap().channels[0]
                .selected_targets
                .len(),
            1
        );
        reopened.end_channel_editing("known-groups").await.unwrap();
        reopened.shutdown_notification_runtime().await;
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn saved_route_name_replaces_an_id_only_target_in_snapshot() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-route-name-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        let channel = app
            .save_channel(ChannelInput {
                id: Some("named-group".into()),
                name: "测试通知".into(),
                provider_id: "dingtalk".into(),
                binding_id: None,
                targets: Some(vec![NotificationTarget {
                    id: "cid-1".into(),
                    kind: "chat".into(),
                    label: "测试群".into(),
                }]),
                values: BTreeMap::from([
                    ("appId".into(), "fixture-app".into()),
                    ("appSecret".into(), "fixture-secret".into()),
                    ("targetId".into(), "cid-1".into()),
                    ("targetKind".into(), "chat".into()),
                ]),
                subscriptions: vec![],
            })
            .await
            .unwrap();
        assert_eq!(channel.targets.len(), 1);
        assert_eq!(channel.targets[0].label, "测试群");
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        app.begin_channel_editing("named-group").await.unwrap();
        app.notification_runtime.request("fixture_target_name", serde_json::json!({"accountId":"named-group","target":{"id":"cid-1","kind":"chat","label":"平台新群名"}})).await.unwrap();
        assert_eq!(
            app.snapshot().await.unwrap().channels[0].selected_targets[0].label,
            "测试群"
        );
        app.shutdown_notification_runtime().await;
        drop(app);
        let reopened = MonitorApp::open(&directory).unwrap();
        assert_eq!(
            reopened.snapshot().await.unwrap().channels[0].targets[0].label,
            "测试群"
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn existing_channel_rebinding_starts_for_all_binding_platforms_without_mutating_accounts()
    {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-rebinding-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        for provider in ["feishu", "weixin", "wecom", "dingtalk"] {
            let provider = provider.to_owned();
            let id = format!("existing-{provider}");
            let saved = serde_json::json!({"_sdkCredentials":serde_json::json!({"appId":"existing-app","appSecret":"existing-private-secret"}).to_string()}).to_string();
            app.call({
                let provider = provider.clone();
                let id = id.clone();
                let saved = saved.clone();
                move |db| {
                    db.save_credential("channel", &id, &saved)?;
                    db.save_notification_channel(
                        &id,
                        "已有账号",
                        &provider,
                        Some(&id),
                        &["stock_available".into()],
                    )?;
                    Ok(())
                }
            })
            .await
            .unwrap();
            let before = app.export_config().await.unwrap();
            let binding = app.begin_channel_rebinding(&id).await.unwrap();
            assert_eq!(binding.provider, provider);
            assert_eq!(binding.status, "waiting");
            let status = app
                .notification_runtime
                .request("status", serde_json::json!({}))
                .await
                .unwrap();
            assert_eq!(status["lastBindingRequest"]["provider"], provider);
            assert_eq!(
                status["lastBindingRequest"]["appId"],
                if provider == "feishu" {
                    serde_json::json!("existing-app")
                } else {
                    serde_json::Value::Null
                }
            );
            assert_eq!(app.export_config().await.unwrap(), before);
            assert_eq!(
                app.call(move |db| db.credential("channel", &id))
                    .await
                    .unwrap(),
                Some(saved)
            );
            app.cancel_channel_binding(&binding.id).await.unwrap();
        }
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn imported_channel_rebinding_starts_without_credentials_for_all_binding_platforms() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-imported-rebinding-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        let imported = ConfigExport {
            config: AppConfig::from_monitor(MonitorConfig::default()),
            products: vec![],
            channels: ["feishu", "weixin", "wecom", "dingtalk"]
                .into_iter()
                .map(|provider| ChannelExport {
                    id: format!("imported-{provider}"),
                    name: "导入账号".into(),
                    provider_id: provider.into(),
                    subscriptions: vec![ChannelEvent::StockAvailable],
                    targets: vec![],
                })
                .collect(),
        };
        app.import_config(&serde_json::to_string(&imported).unwrap())
            .await
            .unwrap();
        for provider in ["feishu", "weixin", "wecom", "dingtalk"] {
            let id = format!("imported-{provider}");
            let before = app.export_config().await.unwrap();
            let binding = app.begin_channel_rebinding(&id).await.unwrap();
            assert_eq!(binding.provider, provider);
            assert_eq!(binding.status, "waiting");
            let status = app
                .notification_runtime
                .request("status", serde_json::json!({}))
                .await
                .unwrap();
            assert!(status["lastBindingRequest"]["appId"].is_null());
            assert_eq!(app.export_config().await.unwrap(), before);
            assert!(app
                .call(move |db| Ok(db
                    .notification_channels()?
                    .into_iter()
                    .find(|channel| channel.id == id)
                    .unwrap()
                    .credential_ref))
                .await
                .unwrap()
                .is_none());
            app.cancel_channel_binding(&binding.id).await.unwrap();
        }
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn notification_system_proxy_errors_block_binding_only_while_enabled() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-system-proxy-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        let mut config = AppConfig::default();
        config.use_system_proxy = true;
        app.save_config(config.clone()).await.unwrap();
        app.set_notification_proxy_url(Err(
            "系统代理使用 PAC，请配置静态 HTTPS 代理或关闭系统代理。".into(),
        ));
        let blocked = app.begin_channel_binding("feishu").await;
        if blocked.is_ok() {
            app.shutdown_notification_runtime().await;
        }
        assert!(blocked.is_err(), "系统代理错误被静默忽略，绑定仍使用直连");
        assert!(blocked.unwrap_err().to_string().contains("PAC"));

        for (enabled, proxy, network, expected_url) in [
            (false, Err("系统代理使用 PAC".into()), "direct", None),
            (true, Ok(None), "direct", None),
            (
                true,
                Ok(Some("http://127.0.0.1:7890/".into())),
                "system_proxy",
                Some("http://127.0.0.1:7890/"),
            ),
        ] {
            config.use_system_proxy = enabled;
            app.save_config(config.clone()).await.unwrap();
            app.set_notification_proxy_url(proxy);
            let binding = app.begin_channel_binding("feishu").await.unwrap();
            let status = app
                .notification_runtime
                .request("status", serde_json::json!({}))
                .await
                .unwrap();
            assert_eq!(status["configuration"]["network"], network);
            assert_eq!(status["configuration"]["proxyUrl"].as_str(), expected_url);
            app.cancel_channel_binding(&binding.id).await.unwrap();
        }
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn first_binding_survives_idle_configuration_before_sdk_reply() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-first-binding-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/slow_binding_runtime.mjs"),
        );
        let binding = tokio::spawn({
            let app = app.clone();
            async move { app.begin_channel_binding("feishu").await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !app.notification_runtime.is_running().await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        app.configure_notification_runtime(None).await.unwrap();
        let result = binding.await.unwrap();
        assert!(result.is_ok(), "首次绑定被后台空闲配置关闭：{result:?}");
        assert_eq!(result.unwrap().status, "waiting");
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn ready_weixin_account_can_test_without_inbound_message() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-await-message-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        app.notification_runtime
            .configure(
                serde_json::json!({"accounts":[{
                    "id":"wx-first", "provider":"weixin", "enabled":true,
                    "credentials":{"connectionStatus":"ready"}, "targets":[]
                }]}),
                true,
            )
            .await
            .unwrap();
        let generation = app.notification_runtime.account_generation("wx-first");
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(750),
            app.wait_notification_account_ready("wx-first", generation),
        )
        .await;
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
        result.expect("微信在线后可直接测试").unwrap();
    }

    #[tokio::test]
    async fn weixin_binding_reports_creator_private_message_without_exposing_context() {
        let directory = std::env::temp_dir().join(format!("ricoh-private-received-{}-{}", std::process::id(), now_ms()));
        let app = MonitorApp::open(&directory).unwrap();
        for (index, contexts, received) in [
            (0, serde_json::json!({}), false),
            (1, serde_json::json!({"other":"private-context"}), false),
            (2, serde_json::json!({"owner":"private-context"}), true),
        ] {
            let binding = app.persist_binding(serde_json::json!({
                "id": format!("wx-inbound-{index}"), "provider":"weixin", "status":"complete",
                "connectionStatus":"ready", "targets":[{"id":"owner","kind":"user","label":"绑定账号"}],
                "credentials":{"userId":"owner","contextTokens":contexts}
            })).await.unwrap();
            let public = serde_json::to_value(binding).unwrap();
            assert_eq!(public["privateMessageReceived"], received);
            assert!(!public.to_string().contains("private-context"));
        }
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn binding_pairing_status_preserves_callbacks_and_feishu_open_chat() {
        let directory = std::env::temp_dir().join(format!("ricoh-pairing-{}-{}", std::process::id(), now_ms()));
        let app = MonitorApp::open(&directory).unwrap();
        for (provider, received, credentials, ready) in [
            ("feishu", false, serde_json::json!({"userOpenId":"owner"}), false),
            ("feishu", false, serde_json::json!({"userOpenId":"owner","p2pChatIds":{"owner":"private"}}), true),
            ("wecom", true, serde_json::json!({}), true),
            ("dingtalk", false, serde_json::json!({}), false),
        ] {
            let binding = app.persist_binding(serde_json::json!({
                "id":format!("{provider}-{ready}"), "provider":provider, "status":"complete",
                "privateMessageReceived":received, "credentials":credentials, "targets":[],
                "pairingReplies":[{"target":{"id":"group","kind":"chat","label":"测试群"},"outcome":"accepted","message":"平台已接受"}]
            })).await.unwrap();
            assert_eq!(binding.private_message_received, Some(received));
            assert_eq!(binding.private_chat_ready, Some(ready));
            assert_eq!(binding.pairing_replies[0].outcome, "accepted");
        }
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn binding_connection_and_missing_staff_id_are_public_without_platform_details() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-binding-state-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        let binding = app
            .persist_binding(serde_json::json!({
                "id":"dt-status", "provider":"dingtalk", "status":"complete",
                "connectionStatus":"ready", "targets":[], "message":"dingtalk_missing_staff_id",
                "botUrl":"https://open-dev.dingtalk.com/fe/app#/corp/robot", "appName":"授权创建的应用名称"
            }))
            .await
            .unwrap();
        assert_eq!(binding.connection_status.as_deref(), Some("ready"));
        assert_eq!(
            binding.bot_url.as_deref(),
            Some("https://open-dev.dingtalk.com/fe/app#/corp/robot")
        );
        assert_eq!(binding.app_name.as_deref(), Some("授权创建的应用名称"));
        assert!(binding.targets.is_empty());
        assert!(binding.message.as_deref().unwrap().contains("已收到私信"));
        let binding = app
            .persist_binding(serde_json::json!({
                "id":"dt-status", "provider":"dingtalk", "status":"complete",
                "connectionStatus":"failed", "targets":[], "message":"private-platform-error"
            }))
            .await
            .unwrap();
        assert_eq!(binding.connection_status.as_deref(), Some("failed"));
        assert!(!serde_json::to_string(&binding)
            .unwrap()
            .contains("private-platform-error"));
        let binding = app
            .persist_binding(serde_json::json!({
                "id":"dt-status", "provider":"dingtalk", "status":"complete",
                "connectionStatus":"ready", "targets":[], "message":"authorized"
            }))
            .await
            .unwrap();
        assert!(binding.message.is_none(), "授权完成的通用文案不能遮住前端的具体初始化步骤");
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn binding_persists_credentials_in_core_and_supports_multiple_targets() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-binding-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        let binding = app.begin_channel_binding("feishu").await.unwrap();
        assert_eq!(binding.status, "waiting");
        app.notification_runtime.remember_binding(serde_json::json!({"id":binding.id,"provider":"feishu","status":"complete","targets":[]}));
        let completed = app.channel_binding_status(&binding.id).await.unwrap();
        assert_eq!(completed.targets.len(), 2);
        assert!(!serde_json::to_string(&completed)
            .unwrap()
            .contains("private-bound-secret"));
        let channel = app
            .save_channel(ChannelInput {
                id: None,
                name: "绑定账户".into(),
                provider_id: "feishu".into(),
                binding_id: Some(binding.id.clone()),
                targets: Some(completed.targets.clone()),
                values: BTreeMap::new(),
                subscriptions: vec![ChannelEvent::StockAvailable],
            })
            .await
            .unwrap();
        assert_eq!(channel.targets.len(), 2);
        assert_eq!(
            app.test_channel(&channel.id).await.unwrap().outcome,
            "accepted"
        );
        app.set_channel_enabled(&channel.id, true).await.unwrap();
        app.configure_notification_runtime(None).await.unwrap();
        let status = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            status["configuration"]["accounts"][0]["credentials"]["appSecret"],
            "private-bound-secret"
        );
        assert_eq!(
            status["configuration"]["accounts"][0]["targets"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(app
            .call(move |db| db.credential("channel", &binding.id))
            .await
            .unwrap()
            .is_none());
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn detected_groups_are_all_tested_and_only_a_failed_recipient_is_retried_for_one_event() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-groups-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open_with_scheduler_client(
            &directory,
            Arc::new(FixtureClient(AtomicUsize::new(0))),
        )
        .unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        let binding = app.begin_channel_binding("feishu").await.unwrap();
        app.channel_binding_status(&binding.id).await.unwrap();
        let groups = app.detect_binding_groups(&binding.id).await.unwrap();
        assert_eq!(groups.len(), 2);
        let refreshed_binding = app.channel_binding_status(&binding.id).await.unwrap();
        assert_eq!(refreshed_binding.targets.len(), 2);
        assert!(refreshed_binding
            .targets
            .iter()
            .any(|target| target.kind == "user"));
        let channel = app
            .save_channel(ChannelInput {
                id: Some("group-account".into()),
                name: "两个群".into(),
                provider_id: "feishu".into(),
                binding_id: Some(binding.id),
                targets: Some(groups),
                values: BTreeMap::new(),
                subscriptions: vec![ChannelEvent::StockAvailable],
            })
            .await
            .unwrap();
        assert_eq!(channel.selected_targets.len(), 2);
        let tested = app.test_channel(&channel.id).await.unwrap();
        assert_eq!(tested.outcome, "accepted");
        assert_eq!(tested.recipients.len(), 2);
        app.set_channel_enabled(&channel.id, true).await.unwrap();
        app.call(|db| {
            let saved = db.credential("channel","group-account")?.unwrap();
            let mut values: BTreeMap<String,String> = serde_json::from_str(&saved).map_err(StorageError::ConfigJson)?;
            values.insert("_sdkCredentials".into(),serde_json::json!({"appId":"bound-app","appSecret":"private-bound-secret","recipientOutcomes":{"chat-a":"accepted","chat-b":"failed"},"retryable":true}).to_string());
            db.save_credential("channel","group-account",&serde_json::to_string(&values).map_err(StorageError::ConfigJson)?)?;
            let product = ProductIdentity{key:"247".into(),product_id:"247".into(),sku_id:None};
            db.save_product_config(product.clone(),"Fixture".into(),"fixture".into(),Some(now_ms()))?;
            db.set_product_enabled("247",true)?;
            let mut reducer = ObservationReducer::new(None);
            let PrepareOutcome::Prepared(prepared) = reducer.prepare(AvailabilityResponse{
                sequence:1,generation:db.product_generation("247")?,result:Ok(ObservationResult{availability:Availability::InStock,is_show:1,stock:Some(1.0),observed_at_ms:now_ms()})
            },RuntimeGate{generation:db.product_generation("247")?,enabled:true,paused:false}) else {unreachable!()};
            db.commit_observation(product,prepared,&[])?;
            Ok(())
        }).await.unwrap();
        let running = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let snapshot = app.snapshot().await.unwrap();
                let deliveries = &snapshot
                    .channels
                    .iter()
                    .find(|channel| channel.id == "group-account")
                    .unwrap()
                    .recipient_deliveries;
                if deliveries.len() == 2
                    && deliveries[0]
                        .last_delivery
                        .as_ref()
                        .is_some_and(|delivery| {
                            delivery.event == "stock_available" && delivery.outcome == "accepted"
                        })
                    && deliveries[1]
                        .last_delivery
                        .as_ref()
                        .is_some_and(|delivery| {
                            delivery.event == "stock_available" && delivery.outcome == "failed"
                        })
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        let status = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(status["recipientCounts"]["group-account:chat:chat-a"], 1);
        assert_eq!(status["recipientCounts"]["group-account:chat:chat-b"], 3);
        assert_eq!(
            app.snapshot()
                .await
                .unwrap()
                .recent_events
                .iter()
                .filter(|event| event.kind == "stock_available")
                .count(),
            1
        );
        let exported = app.export_config().await.unwrap();
        assert!(exported.contains("chat-a") && exported.contains("chat-b"));
        assert!(!exported.contains("private-bound-secret"));
        app.shutdown();
        running.await.unwrap().unwrap();
        drop(app);
        let reopened = MonitorApp::open(&directory).unwrap();
        assert_eq!(
            reopened
                .snapshot()
                .await
                .unwrap()
                .channels
                .iter()
                .find(|channel| channel.id == "group-account")
                .unwrap()
                .selected_targets
                .len(),
            2
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn concurrent_binding_cancel_and_background_configure_preserve_a_test_account_until_ready(
    ) {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-test-lease-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        let binding = app.begin_channel_binding("feishu").await.unwrap();
        app.channel_binding_status(&binding.id).await.unwrap();
        let channel = app
            .save_channel(ChannelInput {
                id: Some("lease-channel".into()),
                name: "测试账户".into(),
                provider_id: "feishu".into(),
                binding_id: Some(binding.id.clone()),
                targets: None,
                values: BTreeMap::from([
                    ("targetId".into(), "chat-a".into()),
                    ("targetKind".into(), "chat".into()),
                ]),
                subscriptions: vec![],
            })
            .await
            .unwrap();
        app.call(|db| {
            let saved = db.credential("channel","lease-channel")?.unwrap();
            let mut values: BTreeMap<String,String> = serde_json::from_str(&saved).map_err(StorageError::ConfigJson)?;
            values.insert("_sdkCredentials".into(),serde_json::json!({"appId":"bound-app","appSecret":"private-bound-secret","connectDelayMs":500}).to_string());
            db.save_credential("channel","lease-channel",&serde_json::to_string(&values).map_err(StorageError::ConfigJson)?)?;
            db.save_credential("channel","keeper",&serde_json::json!({"appId":"keep","appSecret":"keep","targetId":"chat-k","targetKind":"chat"}).to_string())?;
            db.save_notification_channel("keeper","保持连接","feishu",Some("keeper"),&[])?;
            db.mark_notification_channel_tested("keeper")?;
            db.set_notification_channel_enabled("keeper",true)
        }).await.unwrap();
        app.configure_notification_runtime(None).await.unwrap();
        let before = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        let testing = tokio::spawn({
            let app = app.clone();
            let id = channel.id.clone();
            async move { app.test_channel(&id).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !app
                .notification_test_accounts
                .lock()
                .unwrap()
                .contains("lease-channel")
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        app.cancel_channel_binding(&binding.id).await.unwrap();
        for _ in 0..4 {
            app.configure_notification_runtime(None).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(testing.await.unwrap().unwrap().outcome, "accepted");
        let after = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(before["pid"], after["pid"]);
        assert_eq!(after["sendCounts"]["lease-channel"], 1);
        let account = after["configuration"]["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["id"] == "lease-channel")
            .unwrap();
        assert_eq!(account["enabled"], false);
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn channel_edit_lease_keeps_disabled_channel_alive_until_all_editors_end() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-edit-lease-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        let channel = app
            .save_channel(ChannelInput {
                id: Some("edit-lease".into()),
                name: "编辑租约".into(),
                provider_id: "feishu".into(),
                binding_id: None,
                targets: None,
                values: BTreeMap::from([
                    ("appId".into(), "edit-app".into()),
                    ("appSecret".into(), "edit-secret".into()),
                    ("targetId".into(), "chat-a".into()),
                    ("targetKind".into(), "chat".into()),
                ]),
                subscriptions: vec![ChannelEvent::StockAvailable],
            })
            .await
            .unwrap();
        assert!(!channel.enabled);

        app.begin_channel_editing(&channel.id).await.unwrap();
        assert!(app.notification_runtime.is_running().await);
        let before = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(before["configuration"]["accounts"][0]["enabled"], true);
        app.configure_notification_runtime(None).await.unwrap();
        let during_background_configure = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(before["pid"], during_background_configure["pid"]);

        assert_eq!(
            app.detect_channel_groups(&channel.id).await.unwrap().len(),
            2
        );
        let after_detection = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            after_detection["configuration"]["accounts"][0]["enabled"],
            true
        );
        app.begin_channel_editing(&channel.id).await.unwrap();
        app.end_channel_editing(&channel.id).await.unwrap();
        assert!(app.notification_runtime.is_running().await);
        app.end_channel_editing(&channel.id).await.unwrap();
        assert!(!app.notification_runtime.is_running().await);
        app.end_channel_editing(&channel.id).await.unwrap();
        assert!(!app.notification_runtime.is_running().await);

        app.call(|db| db.mark_notification_channel_tested("edit-lease"))
            .await
            .unwrap();
        app.set_channel_enabled(&channel.id, true).await.unwrap();
        app.configure_notification_runtime(None).await.unwrap();
        app.begin_channel_editing(&channel.id).await.unwrap();
        app.end_channel_editing(&channel.id).await.unwrap();
        assert!(app.notification_runtime.is_running().await);
        let enabled_status = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            enabled_status["configuration"]["accounts"][0]["enabled"],
            true
        );

        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn failed_channel_edit_begin_releases_its_lease() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-edit-failure-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        let channel = app
            .save_channel(ChannelInput {
                id: Some("edit-failure".into()),
                name: "编辑失败".into(),
                provider_id: "feishu".into(),
                binding_id: None,
                targets: None,
                values: BTreeMap::from([
                    ("appId".into(), "app".into()),
                    ("appSecret".into(), "secret".into()),
                    ("targetId".into(), "chat-a".into()),
                    ("targetKind".into(), "chat".into()),
                ]),
                subscriptions: vec![],
            })
            .await
            .unwrap();
        app.set_notification_runtime_path(directory.join("missing-runtime"));
        assert!(app.begin_channel_editing(&channel.id).await.is_err());
        assert!(!app
            .notification_edit_accounts
            .lock()
            .unwrap()
            .contains_key(&channel.id));
        assert!(!app.notification_runtime.is_running().await);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn removing_a_channel_cancels_a_test_waiting_for_account_ready() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-test-remove-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        app.call(|db| {
            db.save_credential(
                "channel",
                "remove-test",
                &serde_json::json!({
                    "_sdkCredentials": serde_json::json!({"appId":"fixture","appSecret":"fixture","connectDelayMs":500}).to_string(),
                    "targetId":"chat-a",
                    "targetKind":"chat"
                })
                .to_string(),
            )?;
            db.save_notification_channel(
                "remove-test",
                "移除测试",
                "feishu",
                Some("remove-test"),
                &[],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let testing = tokio::spawn({
            let app = app.clone();
            async move { app.test_channel("remove-test").await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while app
                .notification_runtime
                .account_status("remove-test")
                .is_none_or(|account| account.status != "connecting")
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        app.remove_channel("remove-test").await.unwrap();
        let result = testing.await.unwrap().unwrap();
        assert_eq!(result.outcome, "failed");
        assert_eq!(
            app.notification_runtime.account_generation("remove-test"),
            1
        );
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn cancelled_binding_cannot_be_saved_after_cancellation_completes() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-cancel-save-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        let binding = app.begin_channel_binding("feishu").await.unwrap();
        let completed = app.channel_binding_status(&binding.id).await.unwrap();
        let held = app.notification_binding_operations.lock().await;
        let cancelling = tokio::spawn({
            let app = app.clone();
            let id = binding.id.clone();
            async move { app.cancel_channel_binding(&id).await }
        });
        tokio::task::yield_now().await;
        let saving = tokio::spawn({
            let app = app.clone();
            let id = binding.id.clone();
            async move {
                app.save_channel(ChannelInput {
                    id: None,
                    name: "已取消".into(),
                    provider_id: "feishu".into(),
                    binding_id: Some(id),
                    targets: Some(completed.targets),
                    values: BTreeMap::new(),
                    subscriptions: vec![],
                })
                .await
            }
        });
        tokio::task::yield_now().await;
        drop(held);
        cancelling.await.unwrap().unwrap();
        assert!(saving.await.unwrap().is_err());
        assert!(app.snapshot().await.unwrap().channels.is_empty());
        assert!(app.notification_runtime.binding(&binding.id).is_none());
        assert!(app
            .call(move |db| db.credential("channel", &binding.id))
            .await
            .unwrap()
            .is_none());
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn a_claimed_group_removed_before_send_is_skipped_without_an_sdk_request() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-remove-route-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        let a = NotificationTarget {
            id: "chat-a".into(),
            kind: "chat".into(),
            label: "A".into(),
        };
        let b = NotificationTarget {
            id: "chat-b".into(),
            kind: "chat".into(),
            label: "B".into(),
        };
        app.save_channel(ChannelInput {
            id: Some("route-account".into()),
            name: "路由账户".into(),
            provider_id: "feishu".into(),
            binding_id: None,
            targets: Some(vec![a, b.clone()]),
            values: BTreeMap::from([
                ("appId".into(), "app".into()),
                ("appSecret".into(), "secret".into()),
            ]),
            subscriptions: vec![ChannelEvent::StockAvailable],
        })
        .await
        .unwrap();
        let claimed = app
            .call(|db| {
                db.mark_notification_channel_tested("route-account")?;
                db.set_notification_channel_enabled("route-account", true)?;
                let product = ProductIdentity {
                    key: "247".into(),
                    product_id: "247".into(),
                    sku_id: None,
                };
                db.save_product_config(
                    product.clone(),
                    "Fixture".into(),
                    "fixture".into(),
                    Some(now_ms()),
                )?;
                db.set_product_enabled("247", true)?;
                let mut reducer = ObservationReducer::new(None);
                let generation = db.product_generation("247")?;
                let PrepareOutcome::Prepared(prepared) = reducer.prepare(
                    AvailabilityResponse {
                        sequence: 1,
                        generation,
                        result: Ok(ObservationResult {
                            availability: Availability::InStock,
                            is_show: 1,
                            stock: Some(1.0),
                            observed_at_ms: now_ms(),
                        }),
                    },
                    RuntimeGate {
                        generation,
                        enabled: true,
                        paused: false,
                    },
                ) else {
                    unreachable!()
                };
                db.commit_observation(product, prepared, &[])?;
                Ok(db.claim_channel_notification(now_ms())?.unwrap())
            })
            .await
            .unwrap();
        assert_eq!(claimed.target.as_ref().unwrap().id, "chat-a");
        app.save_channel(ChannelInput {
            id: Some("route-account".into()),
            name: "路由账户".into(),
            provider_id: "feishu".into(),
            binding_id: None,
            targets: Some(vec![b]),
            values: BTreeMap::new(),
            subscriptions: vec![ChannelEvent::StockAvailable],
        })
        .await
        .unwrap();
        app.call(|db| {
            db.mark_notification_channel_tested("route-account")?;
            db.set_notification_channel_enabled("route-account", true)
        })
        .await
        .unwrap();
        let result = app
            .send_event(NotificationJob {
                event: claimed.event,
                subscription: "stock_available",
                channel_id: Some(claimed.channel_id),
                generation: Some(claimed.generation),
                target: claimed.target,
                waited_for_connection: claimed.waited_for_connection,
            })
            .await
            .unwrap();
        assert!(result.is_none());
        let status = app
            .notification_runtime
            .request("status", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(status["sendCounts"], serde_json::json!({}));
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn failed_test_reports_and_persists_the_recipient_and_provider_reason() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-test-reason-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        app.save_channel(ChannelInput {
            id: Some("failed-account".into()),
            name: "研发通知".into(),
            provider_id: "wecom".into(),
            binding_id: None,
            targets: Some(vec![NotificationTarget {
                id: "chat-a".into(),
                kind: "chat".into(),
                label: "研发群".into(),
            }]),
            values: BTreeMap::from([
                ("botId".into(), "bot".into()),
                ("secret".into(), "private-secret".into()),
            ]),
            subscriptions: vec![],
        })
        .await
        .unwrap();
        app.call(|db| {
            let saved = db.credential("channel","failed-account")?.unwrap();
            let mut values: BTreeMap<String,String> = serde_json::from_str(&saved).map_err(StorageError::ConfigJson)?;
            values.insert("_sdkCredentials".into(),serde_json::json!({"botId":"bot","secret":"private-secret","recipientOutcomes":{"chat-a":"failed"},"recipientMessages":{"chat-a":"机器人没有发送权限"}}).to_string());
            db.save_credential("channel","failed-account",&serde_json::to_string(&values).map_err(StorageError::ConfigJson)?)
        }).await.unwrap();
        let tested = app.test_channel("failed-account").await.unwrap();
        assert_eq!(tested.outcome, "failed");
        let message = tested.message.as_deref().unwrap();
        assert!(message.contains("研发群") && message.contains("机器人没有发送权限"));
        assert!(!message.contains("private-secret"));
        let snapshot = app.snapshot().await.unwrap();
        let saved_message = snapshot.channels[0]
            .last_test
            .as_ref()
            .unwrap()
            .message
            .as_deref()
            .unwrap();
        assert!(saved_message.contains("研发群") && saved_message.contains("机器人没有发送权限"));
        app.call(|db| {
            let saved = db.credential("channel", "failed-account")?.unwrap();
            let mut values: BTreeMap<String, String> = serde_json::from_str(&saved).map_err(StorageError::ConfigJson)?;
            values.insert("_sdkCredentials".into(), serde_json::json!({"botId":"bot","secret":"private-secret","recipientOutcomes":{"chat-a":"deferred"}}).to_string());
            db.save_credential("channel", "failed-account", &serde_json::to_string(&values).map_err(StorageError::ConfigJson)?)
        }).await.unwrap();
        let offline_test = app.test_channel("failed-account").await.unwrap();
        assert_eq!(offline_test.outcome, "failed");
        assert_eq!(offline_test.recipients[0].outcome, "failed");
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn sdk_context_updates_survive_reopen_and_stay_private() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-sdk-context-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        app.call(|db| {
            db.save_credential("channel","context-channel",&serde_json::json!({
                "_sdkCredentials":serde_json::json!({"botToken":"private-bot-token","contextTokens":{"other":"retained-context"},"updateContext":true}).to_string(),
                "targetId":"chat-a","targetKind":"chat"
            }).to_string())?;
            db.save_notification_channel("context-channel","微信","weixin",Some("context-channel"),&[])?;
            Ok(())
        }).await.unwrap();
        assert_eq!(
            app.test_channel("context-channel").await.unwrap().outcome,
            "accepted"
        );
        app.shutdown_notification_runtime().await;
        drop(app);
        let reopened = MonitorApp::open(&directory).unwrap();
        let saved = reopened
            .call(|db| db.credential("channel", "context-channel"))
            .await
            .unwrap()
            .unwrap();
        let values: BTreeMap<String, String> = serde_json::from_str(&saved).unwrap();
        let credentials = channel_credentials(&values);
        assert_eq!(
            credentials["contextTokens"]["chat-a"],
            "fresh-private-context"
        );
        assert_eq!(credentials["contextTokens"]["other"], "retained-context");
        let public = serde_json::to_string(&reopened.snapshot().await.unwrap()).unwrap();
        assert!(!public.contains("private-bot-token"));
        assert!(!public.contains("fresh-private-context"));
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn replacing_weixin_token_discards_queued_old_session_snapshots() {
        let directory = std::env::temp_dir().join(format!("ricoh-token-change-{}-{}", std::process::id(), now_ms()));
        let app = MonitorApp::open(&directory).unwrap();
        app.set_notification_runtime_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"));
        let input = |token: &str| ChannelInput {
            id: Some("wx-change".into()), name: "微信".into(), provider_id: "weixin".into(), binding_id: None,
            targets: Some(vec![NotificationTarget { id: "owner".into(), kind: "user".into(), label: "Fixture".into() }]),
            values: BTreeMap::from([("botToken".into(), token.into())]), subscriptions: vec![],
        };
        app.save_channel(input("old-token")).await.unwrap();
        app.configure_notification_runtime(Some("wx-change")).await.unwrap();
        app.notification_runtime.request("fixture_context_update", serde_json::json!({"accountId":"wx-change"})).await.unwrap();
        app.save_channel(input("new-token")).await.unwrap();
        let credentials = app.call(|db| {
            let saved = db.credential("channel", "wx-change")?.unwrap();
            let values = serde_json::from_str(&saved).map_err(StorageError::ConfigJson)?;
            Ok(channel_credentials(&values))
        }).await.unwrap();
        assert_eq!(credentials["botToken"], "new-token");
        assert!(credentials["contextTokens"].is_null());
        assert!(credentials["contextMetadata"].is_null());
        app.shutdown_notification_runtime().await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn run_has_one_process_owner_per_data_directory() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-run-lock-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let app = MonitorApp::open(&directory).unwrap();
        let running = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(app.run().await, Err(AppError::AlreadyRunning));
        app.shutdown();
        running.await.unwrap().unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn scan_retry_after_is_shared_and_prevents_early_followup_requests() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-scan-429-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(RetryAfterClient {
            calls: AtomicUsize::new(0),
            starts: std::sync::Mutex::new(Vec::new()),
        });
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        let scan_config = app
            .call(|db| {
                let mut config = db.monitor_config()?;
                config.scan.interval = std::time::Duration::from_millis(10);
                db.save_monitor_config(&config)?;
                Ok(config)
            })
            .await
            .unwrap();
        app.request_gate
            .reconfigure(&scan_config.requests, scan_config.scan.interval);
        let task = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        app.start_product_scan(700, 701).await.unwrap();
        let scan = tokio::time::timeout(std::time::Duration::from_secs(3), app.wait_for_scan())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(scan.status, ScanStatus::Completed);
        let starts = client.starts.lock().unwrap();
        assert_eq!(starts.len(), 2);
        assert!(starts[1].duration_since(starts[0]) >= std::time::Duration::from_millis(280));
        drop(starts);
        app.shutdown();
        task.await.unwrap().unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn cancelling_scan_during_shared_cooldown_prevents_the_next_request() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-scan-cooldown-cancel-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(RetryAfterClient {
            calls: AtomicUsize::new(0),
            starts: std::sync::Mutex::new(Vec::new()),
        });
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        let config = app
            .call(|db| {
                let mut config = db.monitor_config()?;
                config.scan.interval = std::time::Duration::from_millis(10);
                db.save_monitor_config(&config)?;
                Ok(config)
            })
            .await
            .unwrap();
        app.request_gate
            .reconfigure(&config.requests, config.scan.interval);
        let runner = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        app.start_product_scan(710, 711).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while client.calls.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        app.control_product_scan(ScanAction::Cancel).await.unwrap();
        let scan = tokio::time::timeout(std::time::Duration::from_secs(2), app.wait_for_scan())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(scan.status, ScanStatus::Cancelled);
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        assert_eq!(client.calls.load(Ordering::Relaxed), 1);
        app.shutdown();
        runner.await.unwrap().unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn proxy_pool_runs_each_outlet_and_429_cools_all_of_them_without_counting_failures() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-proxy-pool-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(RoutedRetryClient {
            calls: AtomicUsize::new(0),
            outlets: std::sync::Mutex::new(Vec::new()),
        });
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        app.save_config(AppConfig {
            monitoring_mode: MonitoringMode::ProductDetail,
            schedule: Schedule {
                days: vec![
                    Weekday::Mon,
                    Weekday::Tue,
                    Weekday::Wed,
                    Weekday::Thu,
                    Weekday::Fri,
                    Weekday::Sat,
                    Weekday::Sun,
                ],
                start: "00:00".into(),
                end: "23:59".into(),
            },
            rate: RateConfig {
                interval_min_ms: 1,
                interval_max_ms: 1,
                failures_before_backoff: 3,
                failure_backoff_seconds: 1,
            },
            use_proxy_pool: true,
            ..detail_app_config()
        })
        .await
        .unwrap();
        app.call(|db| {
            let mut config = db.monitor_config()?;
            config.requests.global_requests_per_second = 100.0;
            db.save_monitor_config(&config)?;
            for id in ["proxy-a", "proxy-b"] {
                db.save_proxy(&crate::storage::StoredProxy {
                    id: id.into(),
                    protocol: "http".into(),
                    host: "127.0.0.1".into(),
                    port: 8888,
                    credential_ref: None,
                    enabled: true,
                    status: "available".into(),
                    cooldown_until_ms: None,
                    consecutive_failures: 0,
                })?;
            }
            db.save_product_config(
                ProductIdentity {
                    key: "248".into(),
                    product_id: "248".into(),
                    sku_id: None,
                },
                "Fixture 248".into(),
                "fixture".into(),
                Some(now_ms()),
            )?;
            db.set_product_enabled("248", true)?;
            db.complete_setup()
        })
        .await
        .unwrap();
        let gate_config = app.call(|db| db.monitor_config()).await.unwrap();
        app.request_gate
            .reconfigure(&gate_config.requests, gate_config.scan.interval);
        let running = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            loop {
                let both_outlets_ran = {
                    let requests = client.outlets.lock().unwrap();
                    ["proxy-a", "proxy-b"]
                        .into_iter()
                        .all(|expected| requests.iter().any(|(outlet, _)| outlet == expected))
                };
                if both_outlets_ran {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let requests = client.outlets.lock().unwrap().clone();
        assert_eq!(
            requests
                .iter()
                .map(|(outlet, _)| outlet.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["proxy-a", "proxy-b"])
        );
        assert!(
            requests.iter().skip(1).all(|(_, at)| {
                at.duration_since(requests[0].1) >= std::time::Duration::from_millis(280)
            }),
            "所有出口的后续请求都应遵守共享 429 冷却：{requests:?}"
        );
        let proxies = app.call(|db| db.proxies()).await.unwrap();
        assert!(proxies
            .iter()
            .all(|proxy| proxy.status == "available" && proxy.consecutive_failures == 0));
        app.shutdown();
        running.await.unwrap().unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn expired_proxy_cooldown_is_retested_automatically_by_monitoring() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-proxy-auto-retest-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(RouteSuccessClient(std::sync::Mutex::new(Vec::new())));
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        app.save_config(AppConfig {
            monitoring_mode: MonitoringMode::ProductDetail,
            schedule: Schedule {
                days: vec![
                    Weekday::Mon,
                    Weekday::Tue,
                    Weekday::Wed,
                    Weekday::Thu,
                    Weekday::Fri,
                    Weekday::Sat,
                    Weekday::Sun,
                ],
                start: "00:00".into(),
                end: "23:59".into(),
            },
            use_proxy_pool: true,
            ..detail_app_config()
        })
        .await
        .unwrap();
        app.call(|db| {
            let mut config = db.monitor_config()?;
            config.requests.global_requests_per_second = 100.0;
            db.save_monitor_config(&config)?;
            db.save_proxy(&crate::storage::StoredProxy {
                id: "cooled-proxy".into(),
                protocol: "http".into(),
                host: "127.0.0.1".into(),
                port: 8888,
                credential_ref: None,
                enabled: true,
                status: "cooldown".into(),
                cooldown_until_ms: Some(now_ms() - 1),
                consecutive_failures: 3,
            })?;
            db.save_product_config(
                ProductIdentity {
                    key: "248".into(),
                    product_id: "248".into(),
                    sku_id: None,
                },
                "Fixture 248".into(),
                "fixture".into(),
                Some(now_ms()),
            )?;
            db.set_product_enabled("248", true)?;
            db.complete_setup()
        })
        .await
        .unwrap();
        let config = app.call(|db| db.monitor_config()).await.unwrap();
        app.request_gate
            .reconfigure(&config.requests, config.scan.interval);
        let owner = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let recovered = app
                    .call(|db| {
                        let proxy = db.proxies()?.into_iter().next().unwrap();
                        Ok(proxy.status == "available" && proxy.consecutive_failures == 0)
                    })
                    .await
                    .unwrap();
                if recovered {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(&*client.0.lock().unwrap(), &["cooled-proxy"]);
        let proxy = app
            .call(|db| Ok(db.proxies()?.into_iter().next().unwrap()))
            .await
            .unwrap();
        assert_eq!(proxy.status, "available");
        assert_eq!(proxy.consecutive_failures, 0);
        app.shutdown();
        owner.await.unwrap().unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn enabled_empty_proxy_pool_does_not_fall_back_to_direct_requests() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-app-empty-proxy-pool-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let client = Arc::new(FixtureClient(AtomicUsize::new(0)));
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        app.save_config(AppConfig {
            monitoring_mode: MonitoringMode::ProductDetail,
            schedule: Schedule {
                days: vec![
                    Weekday::Mon,
                    Weekday::Tue,
                    Weekday::Wed,
                    Weekday::Thu,
                    Weekday::Fri,
                    Weekday::Sat,
                    Weekday::Sun,
                ],
                start: "00:00".into(),
                end: "23:59".into(),
            },
            use_proxy_pool: true,
            ..detail_app_config()
        })
        .await
        .unwrap();
        app.call(|db| {
            db.save_product_config(
                ProductIdentity {
                    key: "249".into(),
                    product_id: "249".into(),
                    sku_id: None,
                },
                "Fixture 249".into(),
                "fixture".into(),
                Some(now_ms()),
            )?;
            db.set_product_enabled("249", true)?;
            db.complete_setup()
        })
        .await
        .unwrap();
        let running = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert_eq!(client.0.load(Ordering::Relaxed), 0);
        assert_eq!(
            app.snapshot().await.unwrap().runtime.last_error.as_deref(),
            Some("代理池没有可用出口")
        );
        app.shutdown();
        running.await.unwrap().unwrap();
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

impl AppConfig {
    fn from_monitor(config: MonitorConfig) -> Self {
        let days = config
            .schedule
            .enabled_days
            .iter()
            .enumerate()
            .filter_map(|(i, enabled)| {
                enabled.then_some(
                    [
                        Weekday::Mon,
                        Weekday::Tue,
                        Weekday::Wed,
                        Weekday::Thu,
                        Weekday::Fri,
                        Weekday::Sat,
                        Weekday::Sun,
                    ][i],
                )
            })
            .collect();
        Self {
            auto_start_monitoring: config.auto_start_monitoring,
            monitoring_mode: config.monitoring_mode,
            schedule: Schedule {
                days,
                start: format!(
                    "{:02}:{:02}",
                    config.schedule.start_minute / 60,
                    config.schedule.start_minute % 60
                ),
                end: format!(
                    "{:02}:{:02}",
                    config.schedule.end_minute / 60,
                    config.schedule.end_minute % 60
                ),
            },
            rate: RateConfig {
                interval_min_ms: (config.requests.interval.as_millis() as f64
                    * (1.0 - config.requests.jitter_percent / 100.0))
                    .round() as u64,
                interval_max_ms: (config.requests.interval.as_millis() as f64
                    * (1.0 + config.requests.jitter_percent / 100.0))
                    .round() as u64,
                failures_before_backoff: config.requests.failures_before_backoff,
                failure_backoff_seconds: config.requests.failure_backoff.as_secs(),
            },
            use_system_proxy: config.use_system_proxy,
            use_proxy_pool: config.use_proxy_pool,
            failure_alert_after_minutes: config.failure_alert_after.as_secs().div_ceil(60) as u32,
        }
    }

    fn into_monitor(self) -> Result<MonitorConfig, AppError> {
        let parse_time = |value: &str| -> Result<u16, AppError> {
            let (hour, minute) = value
                .split_once(':')
                .ok_or_else(|| AppError::InvalidInput("计划时间应为 HH:mm".into()))?;
            let hour: u16 = hour
                .parse()
                .map_err(|_| AppError::InvalidInput("计划时间无效".into()))?;
            let minute: u16 = minute
                .parse()
                .map_err(|_| AppError::InvalidInput("计划时间无效".into()))?;
            if hour > 23 || minute > 59 {
                return Err(AppError::InvalidInput("计划时间无效".into()));
            }
            Ok(hour * 60 + minute)
        };
        let mut config = MonitorConfig::default();
        config.auto_start_monitoring = self.auto_start_monitoring;
        config.monitoring_mode = self.monitoring_mode;
        config.schedule.enabled_days = [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ]
        .map(|day| self.schedule.days.contains(&day));
        config.schedule.start_minute = parse_time(&self.schedule.start)?;
        config.schedule.end_minute = parse_time(&self.schedule.end)?;
        let min = self.rate.interval_min_ms;
        let max = self.rate.interval_max_ms;
        if min == 0 || min > max {
            return Err(AppError::InvalidInput("商品请求间隔范围无效".into()));
        }
        config.requests.interval = std::time::Duration::from_millis(min + (max - min) / 2);
        config.requests.jitter_percent = ((max - min) as f64 * 100.0) / (max as f64 + min as f64);
        config.requests.failures_before_backoff = self.rate.failures_before_backoff;
        config.requests.failure_backoff =
            std::time::Duration::from_secs(self.rate.failure_backoff_seconds);
        config.failure_alert_after =
            std::time::Duration::from_secs(u64::from(self.failure_alert_after_minutes) * 60);
        config.use_system_proxy = self.use_system_proxy;
        config.use_proxy_pool = self.use_proxy_pool;
        config
            .validate()
            .map_err(|error| AppError::InvalidInput(error.to_string()))?;
        Ok(config)
    }
}

impl From<StorageError> for AppError {
    fn from(e: StorageError) -> Self {
        Self::Storage(e.to_string())
    }
}
