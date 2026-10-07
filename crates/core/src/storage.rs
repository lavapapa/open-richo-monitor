use std::{collections::BTreeMap, fmt, path::Path};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::availability::{
    reducer::{AvailabilityTransition, ObservationState, PreparedObservation},
    Availability,
};
use crate::config::{ConfigError, MonitorConfig};
use crate::scheduler::MonitoringHealthTransition;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorEventKind {
    FirstObservedInStock,
    OutOfStockToInStock,
    StockIncreased,
    MonitoringFailed,
    Recovered,
}

pub type ListingEventKind = MonitorEventKind;

mod catalog_lifecycle;
mod history_storage;
mod notification_outbox;
mod notification_storage;
mod product_metadata;
mod product_statistics;
mod runtime_storage;
pub use history_storage::{HistoryCursor, HistoryItem, HistoryPage};
pub use notification_outbox::OutboxJob;
pub use notification_storage::{
    ChannelDeliveryRecord, ChannelTestRecord, NotificationChannel, NotificationRoute,
};
pub use product_statistics::ProductStatistics;

const HISTORY_LIMIT: usize = 10_000;
const HISTORY_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;
pub const MAX_CATALOG_PRODUCTS: usize = 1000;

fn advance_product_round(tx: &Transaction<'_>, key: &str) -> Result<(), StorageError> {
    tx.execute(
        "UPDATE product_round_sequence SET generation=generation+1 WHERE id=1",
        [],
    )?;
    let generation: i64 = tx.query_row(
        "SELECT generation FROM product_round_sequence WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO product_rounds(product_key,generation) VALUES (?1,?2)
        ON CONFLICT(product_key) DO UPDATE SET generation=excluded.generation",
        params![key, generation],
    )?;
    tx.execute("DELETE FROM monitoring_health WHERE product_key=?1", [key])?;
    Ok(())
}

fn advance_enabled_rounds(tx: &Transaction<'_>) -> Result<(), StorageError> {
    let keys = tx.prepare("SELECT product_key FROM products WHERE is_configured=1 AND enabled=1 ORDER BY product_key")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for key in keys {
        advance_product_round(tx, &key)?;
    }
    Ok(())
}

fn current_scan(
    tx: &Transaction<'_>,
) -> Result<Option<crate::catalog::ProductIdScan>, StorageError> {
    let json: Option<String> = tx.query_row(
        "SELECT scan_checkpoint_json FROM settings WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    json.map(|json| {
        serde_json::from_str::<crate::catalog::ScanCheckpoint>(&json)
            .map(|checkpoint| {
                let scan = crate::catalog::ProductIdScan::restore(checkpoint);
                (scan.status() != crate::catalog::ScanStatus::Completed).then_some(scan)
            })
            .map_err(StorageError::ConfigJson)
    })
    .transpose()
    .map(Option::flatten)
}

fn current_scan_id(tx: &Transaction<'_>) -> Result<Option<u64>, StorageError> {
    Ok(current_scan(tx)?.map(|scan| scan.scan_id()))
}

fn product_blocked_scan_id(
    conn: &Connection,
    product_key: &str,
) -> Result<Option<u64>, StorageError> {
    let scan_id: Option<Option<i64>> = conn
        .query_row(
            "SELECT blocked_scan_id FROM product_removals WHERE product_key=?1",
            [product_key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(scan_id.flatten().map(|id| id as u64))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductIdentity {
    pub key: String,
    pub product_id: String,
    pub sku_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductConfig {
    pub identity: ProductIdentity,
    pub name: String,
    pub source: String,
    pub verified_at_ms: Option<i64>,
    pub enabled: bool,
    pub prominent_alert: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredObservation {
    pub product: ProductIdentity,
    pub state: ObservationState,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredCheckRun {
    pub product: ProductIdentity,
    pub name: String,
    pub availability: Availability,
    pub is_show: u8,
    pub stock: Option<f64>,
    pub first_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListingEvent {
    pub id: i64,
    pub product: ProductIdentity,
    pub kind: ListingEventKind,
    pub stock: f64,
    pub request_sequence: u64,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredProxy {
    pub id: String,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub credential_ref: Option<String>,
    pub enabled: bool,
    pub status: String,
    pub cooldown_until_ms: Option<i64>,
    pub consecutive_failures: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommitOutcome {
    pub event: Option<ListingEvent>,
}

#[derive(Debug)]
pub enum StorageError {
    Sql(rusqlite::Error),
    Io(std::io::Error),
    ConfigJson(serde_json::Error),
    InvalidConfig(ConfigError),
    MonitorConfigMissing,
    InvalidStock,
    SequenceOutOfRange,
    ProductIdentityConflict,
    ProductConfigMissing,
    NoProductsSelected,
    ProductNotVerified,
    InvalidChannel,
    ChannelMissing,
    ChannelNotTested,
    ChannelBusy,
    CatalogFull,
    ProductNameTooLong,
    ProminentQueueOverflow,
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(error) => write!(formatter, "SQLite 存储错误：{error}"),
            Self::Io(error) => write!(formatter, "数据库文件权限设置失败：{error}"),
            Self::ConfigJson(error) => write!(formatter, "监控配置读写错误：{error}"),
            Self::InvalidConfig(error) => write!(formatter, "监控配置无效：{error}"),
            Self::MonitorConfigMissing => formatter.write_str("数据库中没有已初始化的监控配置"),
            Self::InvalidStock => formatter.write_str("库存数值必须是有限数值"),
            Self::SequenceOutOfRange => formatter.write_str("请求序号超出 SQLite 整数范围"),
            Self::ProductIdentityConflict => formatter.write_str("产品键已对应另一组产品标识"),
            Self::ProductConfigMissing => formatter.write_str("商品目录中没有该产品"),
            Self::NoProductsSelected => formatter.write_str("请至少选择一件监控商品"),
            Self::ProductNotVerified => formatter.write_str("产品验证前不能启用监控"),
            Self::InvalidChannel => formatter.write_str("通知渠道配置无效"),
            Self::ChannelMissing => formatter.write_str("通知渠道不存在"),
            Self::ChannelNotTested => formatter.write_str("请先测试当前版本的通知渠道"),
            Self::ChannelBusy => formatter.write_str("通知渠道正在发送或等待，请稍后再测试"),
            Self::CatalogFull => {
                formatter.write_str("商品目录已达到1000件，新增商品未保存；现有商品仍可继续监控")
            }
            Self::ProductNameTooLong => formatter.write_str("商品名称超过 1024 字节，未保存此商品"),
            Self::ProminentQueueOverflow => {
                formatter.write_str("部分突出提醒因容量上限省略，请查看检查历史")
            }
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sql(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::ConfigJson(error) => Some(error),
            Self::InvalidConfig(error) => Some(error),
            _ => None,
        }
    }
}

fn now_ms_for_proxy() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

impl From<rusqlite::Error> for StorageError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, StorageError> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    let found = columns.filter_map(Result::ok).any(|name| name == column);
    Ok(found)
}

#[cfg(unix)]
fn secure_database_files(path: &Path, include_database: bool) -> Result<(), StorageError> {
    use std::{
        fs,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };

    if path == Path::new(":memory:") {
        return Ok(());
    }

    if include_database {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(path)
            .map_err(StorageError::Io)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(StorageError::Io)?;
    }
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        let sidecar = Path::new(&sidecar);
        match fs::metadata(sidecar) {
            Ok(metadata) if metadata.is_file() => {
                fs::set_permissions(sidecar, fs::Permissions::from_mode(0o600))
                    .map_err(StorageError::Io)?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StorageError::Io(error)),
        }
    }
    Ok(())
}

pub struct Storage {
    conn: Connection,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunIntent {
    #[default]
    Stopped,
    Running,
    Paused,
}

impl RunIntent {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Running => "running",
            Self::Paused => "paused",
        }
    }
}

impl Storage {
    #[cfg(test)]
    pub(crate) fn fail_reads_for_test(&mut self) {
        self.conn.execute("DELETE FROM settings", []).unwrap();
    }

    pub fn save_credential(
        &mut self,
        kind: &str,
        reference: &str,
        value: &str,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO credentials(kind,reference,value) VALUES (?1,?2,?3) ON CONFLICT(kind,reference) DO UPDATE SET value=excluded.value",
            params![kind, reference, value],
        )?;
        Ok(())
    }

    pub fn credential(&self, kind: &str, reference: &str) -> Result<Option<String>, StorageError> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM credentials WHERE kind=?1 AND reference=?2",
                params![kind, reference],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn delete_credential(&mut self, kind: &str, reference: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "DELETE FROM credentials WHERE kind=?1 AND reference=?2",
            params![kind, reference],
        )?;
        Ok(())
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        #[cfg(unix)]
        secure_database_files(path, true)?;
        let conn = Connection::open(path)?;
        let storage = Self::from_connection(conn)?;
        #[cfg(unix)]
        secure_database_files(path, false)?;
        Ok(storage)
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn)
    }

    fn from_connection(conn: Connection) -> Result<Self, StorageError> {
        let new_database = conn.query_row(
            "SELECT NOT EXISTS (
                 SELECT 1 FROM sqlite_master
                 WHERE type = 'table' AND substr(name, 1, 7) != 'sqlite_'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS settings (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 monitor_config_json TEXT NOT NULL,
                 setup_confirmed INTEGER NOT NULL DEFAULT 0 CHECK (setup_confirmed IN (0, 1)),
                 system_notifications_enabled INTEGER NOT NULL DEFAULT 1 CHECK (system_notifications_enabled IN (0, 1)),
                 run_intent TEXT NOT NULL DEFAULT 'stopped' CHECK (run_intent IN ('stopped', 'running', 'paused')),
                 scan_checkpoint_json TEXT,
                 scan_active INTEGER NOT NULL DEFAULT 0 CHECK (scan_active IN (0, 1))
             );
             CREATE TABLE IF NOT EXISTS products (
                 product_key TEXT PRIMARY KEY,
                 product_id TEXT NOT NULL,
                 sku_id TEXT,
                 name TEXT NOT NULL DEFAULT '',
                 source TEXT NOT NULL DEFAULT '',
                 verified_at_ms INTEGER,
                 enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
                 is_configured INTEGER NOT NULL DEFAULT 0 CHECK (is_configured IN (0, 1)),
                 check_count INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS product_removals (
                 product_key TEXT PRIMARY KEY,
                 blocked_scan_id INTEGER
             );
             CREATE TABLE IF NOT EXISTS product_alert_settings (
                 product_key TEXT PRIMARY KEY REFERENCES products(product_key) ON DELETE CASCADE,
                 prominent_alert INTEGER NOT NULL DEFAULT 0 CHECK(prominent_alert IN (0,1))
             );
             CREATE TABLE IF NOT EXISTS scan_failure (
                 id INTEGER PRIMARY KEY CHECK(id=1),
                 scan_id TEXT NOT NULL,
                 message TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS product_rounds (
                 product_key TEXT PRIMARY KEY,
                 generation INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS product_round_sequence (
                 id INTEGER PRIMARY KEY CHECK(id=1),
                 generation INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS proxies (
                 id TEXT PRIMARY KEY, protocol TEXT NOT NULL CHECK(protocol IN ('http','https','socks5')),
                 host TEXT NOT NULL, port INTEGER NOT NULL, credential_ref TEXT,
                 enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
                 status TEXT NOT NULL DEFAULT 'untested', cooldown_until_ms INTEGER,
                 consecutive_failures INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS credentials (
                 kind TEXT NOT NULL CHECK(kind IN ('channel','proxy')),
                 reference TEXT NOT NULL, value TEXT NOT NULL,
                 PRIMARY KEY(kind, reference)
             );
             CREATE TABLE IF NOT EXISTS observations (
                 product_key TEXT PRIMARY KEY REFERENCES products(product_key),
                 availability TEXT NOT NULL CHECK (availability IN ('in_stock', 'out_of_stock')),
                 is_show INTEGER NOT NULL DEFAULT 0 CHECK (is_show IN (0, 1)),
                 stock REAL NOT NULL,
                 request_sequence INTEGER NOT NULL CHECK (request_sequence >= 0),
                 observed_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS check_runs (
                 id INTEGER PRIMARY KEY,
                 product_key TEXT NOT NULL REFERENCES products(product_key),
                 availability TEXT NOT NULL CHECK (availability IN ('in_stock', 'out_of_stock')),
                 is_show INTEGER NOT NULL,
                 stock REAL NOT NULL,
                 checks INTEGER NOT NULL,
                 first_at_ms INTEGER NOT NULL,
                 last_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS observation_stock_absences (
                 product_key TEXT PRIMARY KEY REFERENCES observations(product_key) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS check_run_stock_absences (
                 check_run_id INTEGER PRIMARY KEY REFERENCES check_runs(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS check_runs_by_product ON check_runs(product_key, id DESC);
             CREATE INDEX IF NOT EXISTS check_runs_by_time ON check_runs(first_at_ms DESC, id DESC);
             CREATE TABLE IF NOT EXISTS events (
                 id INTEGER PRIMARY KEY,
                 product_key TEXT NOT NULL REFERENCES products(product_key),
                 event_kind TEXT NOT NULL CHECK (
                     event_kind IN (
                         'first_observed_in_stock', 'out_of_stock_to_in_stock',
                         'monitoring_failed', 'recovered'
                     )
                 ),
                 stock REAL NOT NULL,
                 request_sequence INTEGER NOT NULL CHECK (request_sequence >= 0),
                 observed_at_ms INTEGER NOT NULL,
                 UNIQUE (product_key, request_sequence, event_kind)
             );
             CREATE TABLE IF NOT EXISTS event_sequence (
                 singleton INTEGER PRIMARY KEY CHECK(singleton=1), value INTEGER NOT NULL
             );
             CREATE TRIGGER IF NOT EXISTS event_sequence_advance AFTER INSERT ON events
             BEGIN UPDATE event_sequence SET value=MAX(value,NEW.id) WHERE singleton=1; END;
             CREATE INDEX IF NOT EXISTS events_by_time ON events(observed_at_ms DESC, id DESC);
             CREATE INDEX IF NOT EXISTS events_by_product_time
                 ON events(product_key, observed_at_ms DESC, id DESC);",
        )?;
        conn.execute(
            "INSERT INTO product_round_sequence(id,generation)
             VALUES(1, COALESCE((SELECT MAX(generation) FROM product_rounds),0))
             ON CONFLICT(id) DO UPDATE SET generation=MAX(
                 product_round_sequence.generation,excluded.generation)",
            [],
        )?;
        conn.execute("INSERT INTO event_sequence(singleton,value) VALUES(1,(SELECT COALESCE(MAX(id),0) FROM events))
            ON CONFLICT(singleton) DO UPDATE SET value=MAX(value,excluded.value)", [])?;
        runtime_storage::initialize(&conn)?;
        catalog_lifecycle::initialize(&conn)?;
        product_metadata::initialize(&conn)?;
        product_statistics::initialize(&conn)?;
        if !column_exists(&conn, "check_runs", "product_name")? {
            conn.execute("ALTER TABLE check_runs ADD COLUMN product_name TEXT", [])?;
        }
        if !column_exists(&conn, "settings", "setup_confirmed")? {
            conn.execute(
                "ALTER TABLE settings ADD COLUMN setup_confirmed INTEGER NOT NULL DEFAULT 0 CHECK (setup_confirmed IN (0, 1))",
                [],
            )?;
        }
        if !column_exists(&conn, "observations", "is_show")? {
            conn.execute(
                "ALTER TABLE observations ADD COLUMN is_show INTEGER NOT NULL DEFAULT 0 CHECK (is_show IN (0, 1))",
                [],
            )?;
        }
        if !column_exists(&conn, "products", "check_count")? {
            conn.execute(
                "ALTER TABLE products ADD COLUMN check_count INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
            conn.execute(
                "UPDATE products SET check_count = COALESCE((SELECT SUM(checks) FROM check_runs WHERE product_key = products.product_key), 0)",
                [],
            )?;
        }
        notification_storage::initialize(&conn)?;
        notification_outbox::initialize(&conn)?;
        if new_database {
            let config_json = serde_json::to_string(&MonitorConfig::default())
                .map_err(StorageError::ConfigJson)?;
            conn.execute(
                "INSERT INTO settings (id, monitor_config_json) VALUES (1, ?1)",
                [config_json],
            )?;
        }
        Ok(Self { conn })
    }

    /// 返回已保存的监控配置；缺失、不可解码或无效时返回错误。
    pub fn monitor_config(&self) -> Result<MonitorConfig, StorageError> {
        let json = self
            .conn
            .query_row(
                "SELECT monitor_config_json FROM settings WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StorageError::MonitorConfigMissing)?;
        let config: MonitorConfig =
            serde_json::from_str(&json).map_err(StorageError::ConfigJson)?;
        config.validate().map_err(StorageError::InvalidConfig)?;
        Ok(config)
    }

    /// 原子替换完整监控配置。
    pub fn save_monitor_config(&mut self, config: &MonitorConfig) -> Result<(), StorageError> {
        config.validate().map_err(StorageError::InvalidConfig)?;
        let config_json = serde_json::to_string(config).map_err(StorageError::ConfigJson)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous_json = tx
            .query_row(
                "SELECT monitor_config_json FROM settings WHERE id=1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StorageError::MonitorConfigMissing)?;
        let previous: MonitorConfig =
            serde_json::from_str(&previous_json).map_err(StorageError::ConfigJson)?;
        let changed = tx.execute(
            "UPDATE settings SET monitor_config_json = ?1 WHERE id = 1",
            [config_json],
        )?;
        if changed == 0 {
            return Err(StorageError::MonitorConfigMissing);
        }
        if previous.monitoring_mode != config.monitoring_mode {
            advance_enabled_rounds(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn import_configuration(
        &mut self,
        config: &MonitorConfig,
        products: Vec<(ProductIdentity, String, bool, bool)>,
        channels: Vec<(String, String, String, Vec<String>)>,
        verified_at_ms: i64,
    ) -> Result<(), StorageError> {
        config.validate().map_err(StorageError::InvalidConfig)?;
        let config_json = serde_json::to_string(config).map_err(StorageError::ConfigJson)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        advance_enabled_rounds(&tx)?;
        if tx.execute(
            "UPDATE settings SET monitor_config_json=?1, run_intent='stopped' WHERE id=1",
            [config_json],
        )? == 0
        {
            return Err(StorageError::MonitorConfigMissing);
        }
        for (identity, name, enabled, prominent_alert) in products {
            check_catalog_entry(&tx, &identity, &name)?;
            ensure_product(&tx, &identity)?;
            tx.execute(
                "UPDATE products SET name=?2, source='import', verified_at_ms=?3,
                 enabled=?4, is_configured=1 WHERE product_key=?1",
                params![identity.key, name, verified_at_ms, enabled],
            )?;
            tx.execute(
                "INSERT INTO product_alert_settings(product_key,prominent_alert) VALUES(?1,?2)
                ON CONFLICT(product_key) DO UPDATE SET prominent_alert=excluded.prominent_alert",
                params![identity.key, prominent_alert],
            )?;
        }
        for (id, name, provider, subscriptions) in channels {
            if id.trim().is_empty()
                || name.trim().is_empty()
                || !["system", "feishu", "wecom", "dingtalk", "weixin"].contains(&provider.as_str())
                || subscriptions.iter().any(|value| {
                    !["stock_available", "monitoring_failed", "recovered"].contains(&value.as_str())
                })
            {
                return Err(StorageError::InvalidChannel);
            }
            let credential_ref = tx
                .query_row(
                    "SELECT credential_ref FROM notification_channels WHERE id=?1 AND provider=?2",
                    params![id, provider],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
            let subscriptions =
                serde_json::to_string(&subscriptions).map_err(StorageError::ConfigJson)?;
            tx.execute(
                "INSERT INTO notification_channels(id,name,provider,credential_ref,subscriptions)
                 VALUES (?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET
                 name=excluded.name,provider=excluded.provider,credential_ref=excluded.credential_ref,
                 subscriptions=excluded.subscriptions,tested=0,enabled=0",
                params![id, name, provider, credential_ref, subscriptions],
            )?;
            tx.execute("DELETE FROM notification_tests WHERE channel_id=?1", [&id])?;
        }
        catalog_lifecycle::advance(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub fn system_notifications_enabled(&self) -> Result<bool, StorageError> {
        self.conn
            .query_row(
                "SELECT system_notifications_enabled FROM settings WHERE id=1",
                [],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StorageError::MonitorConfigMissing)
    }

    pub fn setup_confirmed(&self) -> Result<bool, StorageError> {
        self.conn
            .query_row(
                "SELECT setup_confirmed FROM settings WHERE id=1",
                [],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StorageError::MonitorConfigMissing)
    }

    pub fn complete_setup(&mut self) -> Result<(), StorageError> {
        let intent = if self.monitor_config()?.auto_start_monitoring { "running" } else { "stopped" };
        let end_round = intent == "stopped" && self.run_intent()? != RunIntent::Stopped;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE settings SET setup_confirmed=1, run_intent=?1 WHERE id=1",
            [intent],
        )?;
        if changed == 0 {
            return Err(StorageError::MonitorConfigMissing);
        }
        if end_round { advance_enabled_rounds(&tx)?; }
        tx.commit()?;
        Ok(())
    }

    pub fn set_system_notifications_enabled(&mut self, enabled: bool) -> Result<(), StorageError> {
        let changed = self.conn.execute(
            "UPDATE settings SET system_notifications_enabled=?1 WHERE id=1",
            [enabled],
        )?;
        if changed == 0 {
            return Err(StorageError::MonitorConfigMissing);
        }
        Ok(())
    }

    pub fn run_intent(&self) -> Result<RunIntent, StorageError> {
        let value = self
            .conn
            .query_row("SELECT run_intent FROM settings WHERE id=1", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
            .ok_or(StorageError::MonitorConfigMissing)?;
        match value.as_str() {
            "stopped" => Ok(RunIntent::Stopped),
            "running" => Ok(RunIntent::Running),
            "paused" => Ok(RunIntent::Paused),
            _ => unreachable!("run_intent is constrained by SQLite"),
        }
    }

    pub fn set_run_intent(
        &mut self,
        intent: RunIntent,
        end_round: bool,
    ) -> Result<(), StorageError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE settings SET run_intent=?1 WHERE id=1",
            [intent.as_str()],
        )?;
        if changed == 0 {
            return Err(StorageError::MonitorConfigMissing);
        }
        if end_round {
            advance_enabled_rounds(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn product_generation(&self, key: &str) -> Result<u64, StorageError> {
        Ok(self
            .conn
            .query_row(
                "SELECT generation FROM product_rounds WHERE product_key=?1",
                [key],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    pub fn product_generations(&self) -> Result<BTreeMap<u64, u64>, StorageError> {
        let mut query = self
            .conn
            .prepare("SELECT product_key,generation FROM product_rounds")?;
        let rows = query.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?;
        let mut result = BTreeMap::new();
        for row in rows {
            let (key, generation) = row?;
            if let Ok(id) = key.parse() {
                result.insert(id, generation);
            }
        }
        Ok(result)
    }

    pub fn proxies(&self) -> Result<Vec<StoredProxy>, StorageError> {
        let mut query = self.conn.prepare("SELECT id,protocol,host,port,credential_ref,enabled,CASE WHEN status='cooldown' AND cooldown_until_ms<=?1 THEN 'available' ELSE status END,cooldown_until_ms,consecutive_failures FROM proxies ORDER BY id")?;
        let proxies = query
            .query_map([now_ms_for_proxy()], |row| {
                Ok(StoredProxy {
                    id: row.get(0)?,
                    protocol: row.get(1)?,
                    host: row.get(2)?,
                    port: row.get(3)?,
                    credential_ref: row.get(4)?,
                    enabled: row.get(5)?,
                    status: row.get(6)?,
                    cooldown_until_ms: row.get(7)?,
                    consecutive_failures: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(proxies)
    }

    pub fn save_proxy(&mut self, proxy: &StoredProxy) -> Result<(), StorageError> {
        self.conn.execute("INSERT INTO proxies(id,protocol,host,port,credential_ref,enabled,status,cooldown_until_ms,consecutive_failures) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(id) DO UPDATE SET protocol=excluded.protocol,host=excluded.host,port=excluded.port,credential_ref=excluded.credential_ref,enabled=excluded.enabled,status=excluded.status,cooldown_until_ms=excluded.cooldown_until_ms,consecutive_failures=excluded.consecutive_failures", params![proxy.id, proxy.protocol, proxy.host, proxy.port, proxy.credential_ref, proxy.enabled, proxy.status, proxy.cooldown_until_ms, proxy.consecutive_failures])?;
        Ok(())
    }

    pub fn set_proxy_enabled(&mut self, id: &str, enabled: bool) -> Result<(), StorageError> {
        let changed = self.conn.execute(
            "UPDATE proxies SET enabled=?2,
             status=CASE WHEN ?2=0 THEN 'manually_disabled'
                         WHEN status='manually_disabled' THEN 'untested' ELSE status END,
             cooldown_until_ms=CASE WHEN ?2=0 THEN NULL ELSE cooldown_until_ms END
             WHERE id=?1",
            params![id, enabled],
        )?;
        if changed == 0 {
            return Err(StorageError::ChannelMissing);
        }
        Ok(())
    }

    pub fn record_proxy_failure(&mut self, id: &str) -> Result<(), StorageError> {
        let failures: u32 = self
            .conn
            .query_row(
                "SELECT consecutive_failures FROM proxies WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StorageError::ChannelMissing)?;
        let failures = failures.saturating_add(1);
        if failures >= 5 {
            self.conn.execute(
                "UPDATE proxies SET consecutive_failures=?2,enabled=0,status='auto_disabled',cooldown_until_ms=NULL WHERE id=?1",
                params![id, failures],
            )?;
        } else if failures >= 3 {
            self.conn.execute(
                "UPDATE proxies SET consecutive_failures=?2,status='cooldown',cooldown_until_ms=?3 WHERE id=?1",
                params![id, failures, now_ms_for_proxy().saturating_add(60 * 60 * 1000)],
            )?;
        } else {
            self.conn.execute(
                "UPDATE proxies SET consecutive_failures=?2,status='available',cooldown_until_ms=NULL WHERE id=?1",
                params![id, failures],
            )?;
        }
        Ok(())
    }

    pub fn record_proxy_success(&mut self, id: &str) -> Result<(), StorageError> {
        let changed = self.conn.execute(
            "UPDATE proxies SET consecutive_failures=0,status='available',cooldown_until_ms=NULL WHERE id=?1",
            [id],
        )?;
        if changed == 0 {
            return Err(StorageError::ChannelMissing);
        }
        Ok(())
    }

    pub fn remove_proxy(&mut self, id: &str) -> Result<(), StorageError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("DELETE FROM proxies WHERE id=?1", [id])?;
        if changed == 0 {
            return Err(StorageError::ChannelMissing);
        }
        tx.execute(
            "DELETE FROM credentials WHERE kind='proxy' AND reference=?1",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn scan_checkpoint(&self) -> Result<Option<crate::catalog::ScanCheckpoint>, StorageError> {
        let json = self
            .conn
            .query_row(
                "SELECT scan_checkpoint_json FROM settings WHERE id=1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or(StorageError::MonitorConfigMissing)?;
        json.map(|json| serde_json::from_str(&json).map_err(StorageError::ConfigJson))
            .transpose()
    }

    pub fn save_scan_checkpoint(
        &mut self,
        checkpoint: &crate::catalog::ScanCheckpoint,
    ) -> Result<(), StorageError> {
        let json = serde_json::to_string(&checkpoint).map_err(StorageError::ConfigJson)?;
        let changed = self.conn.execute(
            "UPDATE settings SET scan_checkpoint_json=?1 WHERE id=1",
            [json],
        )?;
        if changed == 0 {
            return Err(StorageError::MonitorConfigMissing);
        }
        Ok(())
    }

    pub fn clear_scan_checkpoint(&mut self) -> Result<(), StorageError> {
        let changed = self.conn.execute(
            "UPDATE settings SET scan_checkpoint_json=NULL WHERE id=1",
            [],
        )?;
        if changed == 0 {
            return Err(StorageError::MonitorConfigMissing);
        }
        Ok(())
    }

    pub fn scan_active(&self) -> Result<bool, StorageError> {
        self.conn
            .query_row("SELECT scan_active FROM settings WHERE id=1", [], |row| {
                row.get(0)
            })
            .map_err(StorageError::from)
    }

    pub fn set_scan_active(&mut self, active: bool) -> Result<(), StorageError> {
        let changed = self
            .conn
            .execute("UPDATE settings SET scan_active=?1 WHERE id=1", [active])?;
        if changed == 0 {
            return Err(StorageError::MonitorConfigMissing);
        }
        Ok(())
    }

    pub fn start_scan_if_idle(
        &mut self,
        checkpoint: &crate::catalog::ScanCheckpoint,
    ) -> Result<bool, StorageError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: bool =
            tx.query_row("SELECT scan_active FROM settings WHERE id=1", [], |row| {
                row.get(0)
            })?;
        if active {
            return Ok(false);
        }
        let json = serde_json::to_string(checkpoint).map_err(StorageError::ConfigJson)?;
        tx.execute(
            "UPDATE settings SET scan_checkpoint_json=?1, scan_active=1 WHERE id=1",
            [json],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn control_scan_if_current(
        &mut self,
        scan_id: u64,
        active: bool,
        cancel: bool,
    ) -> Result<bool, StorageError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if current_scan_id(&tx)? != Some(scan_id) {
            return Ok(false);
        }
        if active || cancel {
            tx.execute("DELETE FROM scan_failure", [])?;
        }
        tx.execute(
            "UPDATE settings SET scan_checkpoint_json=CASE WHEN ?1 THEN NULL ELSE scan_checkpoint_json END, scan_active=?2 WHERE id=1",
            params![cancel, active],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn record_scan_failure(&mut self, scan_id: u64, message: &str) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        if current_scan_id(&tx)? == Some(scan_id) {
            tx.execute("UPDATE settings SET scan_active=0 WHERE id=1", [])?;
            tx.execute("INSERT INTO scan_failure(id,scan_id,message) VALUES(1,?1,?2) ON CONFLICT(id) DO UPDATE SET scan_id=excluded.scan_id,message=excluded.message", params![scan_id.to_string(), message])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn scan_failure(&self, scan_id: u64) -> Result<Option<String>, StorageError> {
        Ok(self
            .conn
            .query_row(
                "SELECT message FROM scan_failure WHERE id=1 AND scan_id=?1",
                [scan_id.to_string()],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn commit_scan_item(
        &mut self,
        scan_id: u64,
        mut scan: crate::catalog::ProductIdScan,
        detail: Option<crate::ricoh::ProductDetail>,
        verified_at_ms: i64,
    ) -> Result<Option<(crate::catalog::ProductIdScan, bool)>, StorageError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if current_scan_id(&tx)? != Some(scan_id) {
            return Ok(None);
        }
        let id = scan.in_flight_id().expect("扫描结果必须对应在途商品");
        let can_save = product_blocked_scan_id(&tx, &id.to_string())? != Some(scan_id);
        if let Some(detail) = detail.filter(|detail| can_save && detail.product_id == id) {
            let identity = ProductIdentity {
                key: id.to_string(),
                product_id: id.to_string(),
                sku_id: None,
            };
            let name = detail.name.clone();
            check_catalog_entry(&tx, &identity, &name)?;
            ensure_product(&tx, &identity)?;
            tx.execute(
                "UPDATE products SET name=?2, source='scan', verified_at_ms=?3, is_configured=1 WHERE product_key=?1",
                params![identity.key, name, verified_at_ms],
            )?;
            tx.execute(
                "DELETE FROM product_removals WHERE product_key=?1",
                [&identity.key],
            )?;
            let enabled: bool = tx.query_row(
                "SELECT enabled FROM products WHERE product_key=?1",
                [&identity.key],
                |row| row.get(0),
            )?;
            let cached: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM product_metadata WHERE product_key=?1)",
                [&identity.key],
                |row| row.get(0),
            )?;
            if !enabled || !cached {
                product_metadata::save(&tx, &identity.key, &detail.metadata, verified_at_ms)?;
            }
            scan.record_found(detail).expect("商品 ID 已检查");
        } else {
            scan.record_failure().expect("扫描结果必须对应在途商品");
        }
        let checkpoint = scan.checkpoint().expect("扫描尚未取消");
        let completed = scan.status() == crate::catalog::ScanStatus::Completed;
        let json = serde_json::to_string(&checkpoint).map_err(StorageError::ConfigJson)?;
        tx.execute("UPDATE settings SET scan_checkpoint_json=?1, scan_active=CASE WHEN ?2 THEN 0 ELSE scan_active END WHERE id=1", params![json, completed])?;
        let active: bool =
            tx.query_row("SELECT scan_active FROM settings WHERE id=1", [], |row| {
                row.get(0)
            })?;
        tx.commit()?;
        Ok(Some((scan, active)))
    }

    /// 保存商品目录信息；新增商品默认停用，目录刷新保留用户的启用选择。
    pub fn save_product_config(
        &mut self,
        identity: ProductIdentity,
        name: String,
        source: String,
        verified_at_ms: Option<i64>,
    ) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        check_catalog_entry(&tx, &identity, &name)?;
        ensure_product(&tx, &identity)?;
        tx.execute(
            "UPDATE products SET
                 name = ?2,
                 source = ?3,
                 verified_at_ms = ?4,
                 enabled = CASE WHEN ?4 IS NULL THEN 0 ELSE enabled END,
                 is_configured = 1
             WHERE product_key = ?1",
            params![identity.key, name, source, verified_at_ms],
        )?;
        tx.execute(
            "DELETE FROM product_removals WHERE product_key=?1",
            [&identity.key],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn product_removed(&self, product_key: &str) -> Result<bool, StorageError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_removals WHERE product_key=?1)",
            [product_key],
            |row| row.get(0),
        )?)
    }

    pub fn product_configs(&self) -> Result<Vec<ProductConfig>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT product_key, product_id, sku_id, name, source, verified_at_ms, enabled,
             COALESCE((SELECT prominent_alert FROM product_alert_settings a WHERE a.product_key=products.product_key),0)
             FROM products
             WHERE is_configured = 1
             ORDER BY product_id, COALESCE(sku_id, ''), product_key",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ProductConfig {
                identity: ProductIdentity {
                    key: row.get(0)?,
                    product_id: row.get(1)?,
                    sku_id: row.get(2)?,
                },
                name: row.get(3)?,
                source: row.get(4)?,
                verified_at_ms: row.get(5)?,
                enabled: row.get(6)?,
                prominent_alert: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn record_check_attempt(&mut self, product_key: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE products SET check_count = CASE WHEN check_count < 9223372036854775807 THEN check_count + 1 ELSE check_count END WHERE product_key = ?1",
            [product_key],
        )?;
        Ok(())
    }

    pub(crate) fn commit_monitoring_attempt<T>(
        &mut self,
        product_key: &str,
        generation: u64,
        operation: impl FnOnce(&mut Self) -> Result<T, StorageError>,
    ) -> Result<Option<T>, StorageError> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            if self.product_generation(product_key)? != generation
                || !self.monitoring_product_active(product_key)?
            {
                return Ok(None);
            }
            operation(self).map(Some)
        })();
        match result {
            Ok(value) => {
                if let Err(error) = self.conn.execute_batch("COMMIT") {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(error.into());
                }
                Ok(value)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub fn monitoring_product_active(&self, product_key: &str) -> Result<bool, StorageError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM products p JOIN settings s ON s.id = 1
                WHERE p.product_key = ?1 AND p.is_configured = 1 AND p.enabled = 1
                  AND s.run_intent = 'running'
            )",
            [product_key],
            |row| row.get(0),
        )?)
    }

    pub fn check_count(&self, product_key: &str) -> Result<u64, StorageError> {
        let count: i64 = self.conn.query_row(
            "SELECT check_count FROM products WHERE product_key = ?1",
            [product_key],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    pub fn set_product_enabled(
        &mut self,
        product_key: &str,
        enabled: bool,
    ) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        let verified_at_ms = tx
            .query_row(
                "SELECT verified_at_ms FROM products
                 WHERE product_key = ?1 AND is_configured = 1",
                [product_key],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .ok_or(StorageError::ProductConfigMissing)?;
        if enabled && verified_at_ms.is_none() {
            return Err(StorageError::ProductNotVerified);
        }
        tx.execute(
            "UPDATE products SET enabled = ?2 WHERE product_key = ?1",
            params![product_key, enabled],
        )?;
        advance_product_round(&tx, product_key)?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_product_prominent_alert(
        &mut self,
        product_key: &str,
        enabled: bool,
    ) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        let configured: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM products WHERE product_key=?1 AND is_configured=1)",
            [product_key],
            |row| row.get(0),
        )?;
        if !configured {
            return Err(StorageError::ProductConfigMissing);
        }
        tx.execute(
            "INSERT INTO product_alert_settings(product_key,prominent_alert) VALUES(?1,?2)
            ON CONFLICT(product_key) DO UPDATE SET prominent_alert=excluded.prominent_alert",
            params![product_key, enabled],
        )?;
        if !enabled {
            tx.execute(
                "DELETE FROM notification_outbox WHERE product_key=?1 AND channel_id='__prominent__'",
                [product_key],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_onboarding_products(&mut self, ids: &[u64]) -> Result<(), StorageError> {
        if ids.is_empty() {
            return Err(StorageError::NoProductsSelected);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for id in ids {
            let key = id.to_string();
            let existing: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM products WHERE product_key=?1 AND is_configured=1)",
                [&key],
                |row| row.get(0),
            )?;
            if !existing {
                let name = crate::catalog::DEFAULT_PRODUCTS
                    .iter()
                    .find(|&&(known, _)| known == *id)
                    .map(|&(_, name)| name)
                    .ok_or(StorageError::ProductConfigMissing)?;
                let identity = ProductIdentity {
                    key: key.clone(),
                    product_id: key.clone(),
                    sku_id: None,
                };
                check_catalog_entry(&tx, &identity, name)?;
                ensure_product(&tx, &identity)?;
                tx.execute("UPDATE products SET name=?2,source='default',is_configured=1 WHERE product_key=?1", params![key,name])?;
            }
            tx.execute("DELETE FROM product_removals WHERE product_key=?1", [&key])?;
        }
        let products = tx
            .prepare("SELECT product_key,product_id,enabled FROM products WHERE is_configured=1")?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (key, id, enabled) in products {
            let selected = id.parse::<u64>().ok().is_some_and(|id| ids.contains(&id));
            if selected != enabled {
                tx.execute(
                    "UPDATE products SET enabled=?2 WHERE product_key=?1",
                    params![key, selected],
                )?;
                advance_product_round(&tx, &key)?;
            }
        }
        catalog_lifecycle::advance(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// 从用户目录移除商品，保留商品身份及事件历史。
    pub fn remove_product(&mut self, product_key: &str) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM products WHERE product_key=?1)",
            [product_key],
            |row| row.get(0),
        )?;
        let blocked_scan_id = current_scan(&tx)?
            .filter(|scan| {
                product_key
                    .parse::<u64>()
                    .ok()
                    .is_some_and(|id| scan.start_id() <= id && id <= scan.end_id())
            })
            .map(|scan| scan.scan_id());
        let default_product = crate::catalog::DEFAULT_PRODUCTS
            .iter()
            .any(|(id, _)| id.to_string() == product_key);
        if !exists && blocked_scan_id.is_none() && !default_product {
            catalog_lifecycle::advance(&tx)?;
            tx.commit()?;
            return Ok(());
        }
        advance_product_round(&tx, product_key)?;
        tx.execute(
            "INSERT INTO product_removals(product_key, blocked_scan_id) VALUES (?1, ?2)
             ON CONFLICT(product_key) DO UPDATE SET blocked_scan_id=excluded.blocked_scan_id",
            params![product_key, blocked_scan_id],
        )?;
        tx.execute(
            "UPDATE products SET is_configured = 0, enabled = 0 WHERE product_key = ?1",
            [product_key],
        )?;
        tx.execute(
            "DELETE FROM product_alert_settings WHERE product_key=?1",
            [product_key],
        )?;
        tx.execute(
            "DELETE FROM product_metadata WHERE product_key=?1",
            [product_key],
        )?;
        tx.execute(
            "DELETE FROM product_statistics WHERE product_key=?1",
            [product_key],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn commit_observation(
        &mut self,
        product: ProductIdentity,
        prepared: PreparedObservation<'_>,
        _channel_ids: &[&str],
    ) -> Result<CommitOutcome, StorageError> {
        let state = prepared.state();
        let transition = prepared.transition();
        if state.stock.is_some_and(|stock| !stock.is_finite()) {
            return Err(StorageError::InvalidStock);
        }
        let sequence =
            i64::try_from(state.request_sequence).map_err(|_| StorageError::SequenceOutOfRange)?;

        let tx = self.conn.savepoint()?;
        ensure_product(&tx, &product)?;
        let previous = tx
            .query_row(
                "SELECT availability, is_show,
                        CASE WHEN EXISTS (SELECT 1 FROM observation_stock_absences a WHERE a.product_key=o.product_key)
                             THEN NULL ELSE stock END
                 FROM observations o WHERE product_key = ?1",
                [&product.key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u8>(1)?,
                        row.get::<_, Option<f64>>(2)?,
                    ))
                },
            )
            .optional()?;
        tx.execute(
            "INSERT INTO observations (
                 product_key, availability, is_show, stock, request_sequence, observed_at_ms
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(product_key) DO UPDATE SET
                 availability = excluded.availability,
                 is_show = excluded.is_show,
                 stock = excluded.stock,
                 request_sequence = excluded.request_sequence,
                 observed_at_ms = excluded.observed_at_ms",
            params![
                product.key,
                availability_to_db(state.availability),
                state.is_show,
                state.stock.unwrap_or(0.0),
                sequence,
                state.observed_at_ms,
            ],
        )?;
        if state.stock.is_none() {
            tx.execute(
                "INSERT OR IGNORE INTO observation_stock_absences(product_key) VALUES (?1)",
                [&product.key],
            )?;
        } else {
            tx.execute(
                "DELETE FROM observation_stock_absences WHERE product_key=?1",
                [&product.key],
            )?;
        }

        if previous.is_none_or(|(availability, is_show, stock)| {
            availability != availability_to_db(state.availability)
                || is_show != state.is_show
                || stock != state.stock
        }) {
            tx.execute(
                "INSERT INTO check_runs (product_key, product_name, availability, is_show, stock, checks, first_at_ms, last_at_ms)
                 VALUES (?1, (SELECT name FROM products WHERE product_key=?1), ?2, ?3, ?4, 1, ?5, ?5)",
                params![product.key, availability_to_db(state.availability), state.is_show, state.stock.unwrap_or(0.0), state.observed_at_ms],
            )?;
            if state.stock.is_none() {
                tx.execute(
                    "INSERT INTO check_run_stock_absences(check_run_id) VALUES (last_insert_rowid())",
                    [],
                )?;
            }
            tx.execute(
                "DELETE FROM check_runs WHERE id=(SELECT id FROM check_runs ORDER BY first_at_ms,id LIMIT 1)
                 AND (SELECT COUNT(*) FROM check_runs)>?1",
                [HISTORY_LIMIT as i64],
            )?;
        }

        let event = if let Some(transition) = transition {
            let stock = state.stock.expect("上架或有货跃迁包含真实库存");
            let kind = match transition {
                AvailabilityTransition::FirstObservedInStock => {
                    MonitorEventKind::FirstObservedInStock
                }
                AvailabilityTransition::OutOfStockToInStock => {
                    MonitorEventKind::OutOfStockToInStock
                }
                AvailabilityTransition::StockIncreased => MonitorEventKind::StockIncreased,
            };
            // 库存增量已由 check_runs 记录，复用事件序列交付突出提醒。
            let (inserted, event_id) = if kind == MonitorEventKind::StockIncreased {
                tx.execute(
                    "UPDATE event_sequence SET value=value+1 WHERE singleton=1",
                    [],
                )?;
                (
                    1,
                    tx.query_row(
                        "SELECT value FROM event_sequence WHERE singleton=1",
                        [],
                        |row| row.get(0),
                    )?,
                )
            } else {
                let inserted = tx.execute(
                    "INSERT INTO events (
                     id, product_key, event_kind, stock, request_sequence, observed_at_ms
                 )
                 VALUES ((SELECT value+1 FROM event_sequence WHERE singleton=1), ?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(product_key, request_sequence, event_kind) DO NOTHING",
                    params![
                        product.key,
                        event_kind_to_db(kind),
                        stock,
                        sequence,
                        state.observed_at_ms,
                    ],
                )?;
                let event_id = tx.query_row(
                    "SELECT id FROM events
                 WHERE product_key = ?1 AND request_sequence = ?2 AND event_kind = ?3",
                    params![product.key, sequence, event_kind_to_db(kind)],
                    |row| row.get(0),
                )?;
                (inserted, event_id)
            };
            let event = ListingEvent {
                id: event_id,
                product: product.clone(),
                kind,
                stock,
                request_sequence: state.request_sequence,
                observed_at_ms: state.observed_at_ms,
            };
            if inserted != 0 {
                notification_outbox::enqueue_event(&tx, &event)?;
                tx.execute(
                    "DELETE FROM events WHERE id=(SELECT id FROM events ORDER BY observed_at_ms,id LIMIT 1)
                     AND (SELECT COUNT(*) FROM events)>?1",
                    [HISTORY_LIMIT as i64],
                )?;
            }
            Some(event)
        } else {
            None
        };

        tx.commit()?;
        prepared.commit();
        Ok(CommitOutcome { event })
    }

    /// 将 Core 产生的失败/恢复跃迁写入事件表。
    pub fn commit_health_transition(
        &mut self,
        product: ProductIdentity,
        sequence: u64,
        transition: MonitoringHealthTransition,
        at_ms: i64,
        _channel_ids: &[&str],
    ) -> Result<Option<ListingEvent>, StorageError> {
        let sequence = i64::try_from(sequence).map_err(|_| StorageError::SequenceOutOfRange)?;
        let kind = match transition {
            MonitoringHealthTransition::Failed { .. } => MonitorEventKind::MonitoringFailed,
            MonitoringHealthTransition::Recovered => MonitorEventKind::Recovered,
        };
        let tx = self.conn.savepoint()?;
        ensure_product(&tx, &product)?;
        let stock = tx
            .query_row(
                "SELECT stock FROM observations WHERE product_key=?1",
                [&product.key],
                |row| row.get::<_, f64>(0),
            )
            .optional()?
            .unwrap_or(0.0);
        let inserted = tx.execute(
            "INSERT INTO events (id,product_key,event_kind,stock,request_sequence,observed_at_ms)
             VALUES ((SELECT value+1 FROM event_sequence WHERE singleton=1),?1,?2,?3,?4,?5)
             ON CONFLICT(product_key,request_sequence,event_kind) DO NOTHING",
            params![product.key, event_kind_to_db(kind), stock, sequence, at_ms],
        )?;
        let event = tx.query_row(
            "SELECT e.id,p.product_key,p.product_id,p.sku_id,e.event_kind,e.stock,
                    e.request_sequence,e.observed_at_ms
             FROM events e JOIN products p USING(product_key)
             WHERE e.product_key=?1 AND e.request_sequence=?2 AND e.event_kind=?3",
            params![product.key, sequence, event_kind_to_db(kind)],
            event_from_row,
        )?;
        if inserted != 0 {
            notification_outbox::enqueue_event(&tx, &event)?;
            tx.execute(
                "DELETE FROM events WHERE id=(SELECT id FROM events ORDER BY observed_at_ms,id LIMIT 1)
                 AND (SELECT COUNT(*) FROM events)>?1",
                [HISTORY_LIMIT as i64],
            )?;
        }
        tx.commit()?;
        Ok(Some(event))
    }

    pub fn observation(
        &self,
        product_key: &str,
    ) -> Result<Option<StoredObservation>, StorageError> {
        self.conn
            .query_row(
                "SELECT p.product_key, p.product_id, p.sku_id, o.availability, o.is_show,
                        CASE WHEN EXISTS (SELECT 1 FROM observation_stock_absences a WHERE a.product_key=o.product_key)
                             THEN NULL ELSE o.stock END,
                        o.request_sequence, o.observed_at_ms
                 FROM observations o JOIN products p USING (product_key)
                 WHERE p.product_key = ?1",
                [product_key],
                observation_from_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn events_for_product(
        &self,
        product_key: &str,
        start_at_ms: Option<i64>,
        end_before_ms: Option<i64>,
        limit: usize,
    ) -> Result<Vec<ListingEvent>, StorageError> {
        let limit = limit.min(HISTORY_LIMIT) as i64;
        let mut statement = self.conn.prepare(
            "SELECT e.id, p.product_key, p.product_id, p.sku_id, e.event_kind, e.stock,
                    e.request_sequence, e.observed_at_ms
             FROM events e JOIN products p USING (product_key)
             WHERE e.product_key = ?1
               AND (?2 IS NULL OR e.observed_at_ms >= ?2)
               AND (?3 IS NULL OR e.observed_at_ms < ?3)
             ORDER BY e.observed_at_ms DESC, e.id DESC
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![product_key, start_at_ms, end_before_ms, limit],
            event_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn recent_events(&self, limit: usize) -> Result<Vec<ListingEvent>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT e.id, p.product_key, p.product_id, p.sku_id, e.event_kind, e.stock,
                    e.request_sequence, e.observed_at_ms
             FROM events e JOIN products p USING (product_key)
             ORDER BY e.observed_at_ms DESC, e.id DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit.min(HISTORY_LIMIT) as i64], event_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn recent_check_runs(&self, limit: usize) -> Result<Vec<StoredCheckRun>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT p.product_key, p.product_id, p.sku_id, COALESCE(r.product_name, p.name), r.availability, r.is_show,
                    CASE WHEN EXISTS (SELECT 1 FROM check_run_stock_absences a WHERE a.check_run_id=r.id)
                         THEN NULL ELSE r.stock END, r.first_at_ms
             FROM check_runs r JOIN products p USING (product_key)
             ORDER BY r.id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit.min(HISTORY_LIMIT) as i64], |row| {
            Ok(StoredCheckRun {
                product: ProductIdentity {
                    key: row.get(0)?,
                    product_id: row.get(1)?,
                    sku_id: row.get(2)?,
                },
                name: row.get(3)?,
                availability: availability_from_db(&row.get::<_, String>(4)?)?,
                is_show: row.get(5)?,
                stock: row.get(6)?,
                first_at_ms: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn stock_before_check_run(
        &self,
        product_key: &str,
        observed_at_ms: i64,
        stock: f64,
    ) -> Result<Option<f64>, StorageError> {
        Ok(self.conn.query_row(
            "SELECT previous.stock FROM check_runs current
             JOIN check_runs previous ON previous.product_key=current.product_key
                AND previous.id < current.id
             WHERE current.product_key=?1 AND current.first_at_ms=?2 AND current.stock=?3
               AND NOT EXISTS(SELECT 1 FROM check_run_stock_absences a WHERE a.check_run_id=previous.id)
             ORDER BY current.id DESC, previous.id DESC LIMIT 1",
            params![product_key, observed_at_ms, stock],
            |row| row.get(0),
        ).optional()?)
    }

    pub fn cleanup(&mut self, now_ms: i64, batch_size: usize) -> Result<usize, StorageError> {
        if batch_size == 0 {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let cutoff = now_ms.saturating_sub(HISTORY_WINDOW_MS);
        let mut remaining = batch_size.min(HISTORY_LIMIT) as i64;
        let mut removed = tx.execute(
            "DELETE FROM events
             WHERE id IN (
                 SELECT e.id FROM events e
                 WHERE e.observed_at_ms < ?1
                 ORDER BY e.observed_at_ms, e.id
                 LIMIT ?2
             )",
            params![cutoff, remaining],
        )?;
        remaining -= removed as i64;
        if remaining > 0 {
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
            let excess = count.saturating_sub(HISTORY_LIMIT as i64).max(0);
            let to_remove = excess.min(remaining);
            let deleted = tx.execute(
                "DELETE FROM events
                 WHERE id IN (SELECT id FROM events ORDER BY observed_at_ms, id LIMIT ?1)",
                [to_remove],
            )?;
            removed += deleted;
            remaining -= deleted as i64;
        }
        if remaining > 0 {
            let deleted = tx.execute(
                "DELETE FROM check_runs WHERE id IN (
                    SELECT id FROM check_runs WHERE first_at_ms<?1 ORDER BY first_at_ms,id LIMIT ?2)",
                params![cutoff, remaining],
            )?;
            removed += deleted;
            remaining -= deleted as i64;
        }
        if remaining > 0 {
            let count: i64 =
                tx.query_row("SELECT COUNT(*) FROM check_runs", [], |row| row.get(0))?;
            let excess = count.saturating_sub(HISTORY_LIMIT as i64).max(0);
            let deleted = tx.execute(
                "DELETE FROM check_runs WHERE id IN (SELECT id FROM check_runs ORDER BY first_at_ms,id LIMIT ?1)",
                [excess.min(remaining)],
            )?;
            removed += deleted;
            remaining -= deleted as i64;
        }
        if remaining > 0 {
            let scan_id = current_scan_id(&tx)?.map(|id| id as i64);
            let keys = tx.prepare(
                "SELECT p.product_key FROM products p
                 LEFT JOIN product_removals r USING(product_key)
                 WHERE p.is_configured=0 AND r.product_key IS NOT NULL
                   AND NOT EXISTS(SELECT 1 FROM events e WHERE e.product_key=p.product_key)
                   AND NOT EXISTS(SELECT 1 FROM check_runs c WHERE c.product_key=p.product_key)
                   AND NOT EXISTS(SELECT 1 FROM notification_outbox o WHERE o.product_key=p.product_key)
                   AND (r.blocked_scan_id IS NULL OR r.blocked_scan_id IS NOT ?1)
                 ORDER BY p.product_key"
            )?.query_map([scan_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|key| !crate::catalog::DEFAULT_PRODUCTS.iter().any(|(id, _)| id.to_string().as_str() == key.as_str()))
                .take(remaining as usize)
                .collect::<Vec<_>>();
            for key in keys {
                tx.execute("DELETE FROM observations WHERE product_key=?1", [&key])?;
                tx.execute("DELETE FROM monitoring_health WHERE product_key=?1", [&key])?;
                tx.execute("DELETE FROM product_removals WHERE product_key=?1", [&key])?;
                tx.execute("DELETE FROM product_rounds WHERE product_key=?1", [&key])?;
                tx.execute("DELETE FROM products WHERE product_key=?1", [&key])?;
                removed += 1;
                remaining -= 1;
            }
            if remaining > 0 {
                let keys = tx
                    .prepare(
                        "SELECT r.product_key FROM product_removals r
                     WHERE NOT EXISTS(SELECT 1 FROM products p WHERE p.product_key=r.product_key)
                       AND (r.blocked_scan_id IS NULL OR r.blocked_scan_id IS NOT ?1)
                     ORDER BY r.product_key",
                    )?
                    .query_map([scan_id], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .filter(|key| {
                        !crate::catalog::DEFAULT_PRODUCTS
                            .iter()
                            .any(|(id, _)| id.to_string().as_str() == key.as_str())
                    })
                    .take(remaining as usize)
                    .collect::<Vec<_>>();
                for key in keys {
                    removed +=
                        tx.execute("DELETE FROM product_removals WHERE product_key=?1", [key])?;
                    remaining -= 1;
                }
            }
            if remaining > 0 {
                let deleted = tx.execute(
                    "DELETE FROM product_rounds WHERE product_key IN (
                        SELECT r.product_key FROM product_rounds r
                        WHERE NOT EXISTS(SELECT 1 FROM products p WHERE p.product_key=r.product_key)
                          AND NOT EXISTS(SELECT 1 FROM product_removals m WHERE m.product_key=r.product_key)
                          AND NOT EXISTS(SELECT 1 FROM notification_outbox o WHERE o.product_key=r.product_key)
                        LIMIT ?1)",
                    [remaining],
                )?;
                removed += deleted;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    pub fn clear_history(&mut self) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM events", [])?;
        tx.execute("DELETE FROM check_runs", [])?;
        tx.execute("UPDATE products SET check_count = 0", [])?;
        tx.execute("DELETE FROM product_statistics", [])?;
        tx.commit()?;
        Ok(())
    }

    pub fn restore_defaults(
        &mut self,
        config: &MonitorConfig,
        clear_history: bool,
    ) -> Result<(), StorageError> {
        config.validate().map_err(StorageError::InvalidConfig)?;
        let config_json = serde_json::to_string(config).map_err(StorageError::ConfigJson)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        advance_enabled_rounds(&tx)?;
        tx.execute(
            "UPDATE settings SET monitor_config_json=?1, run_intent='stopped', setup_confirmed=0,
             system_notifications_enabled=1, scan_checkpoint_json=NULL, scan_active=0 WHERE id=1",
            [config_json],
        )?;
        tx.execute("DELETE FROM notification_channels", [])?;
        tx.execute("DELETE FROM proxies", [])?;
        tx.execute("DELETE FROM credentials", [])?;
        tx.execute("DELETE FROM observations", [])?;
        tx.execute("DELETE FROM product_metadata", [])?;
        tx.execute("DELETE FROM product_statistics", [])?;
        if clear_history {
            tx.execute("DELETE FROM events", [])?;
            tx.execute("DELETE FROM check_runs", [])?;
            tx.execute("DELETE FROM products", [])?;
        } else {
            tx.execute(
                "UPDATE products SET is_configured=0, enabled=0, source='', verified_at_ms=NULL, check_count=0",
                [],
            )?;
        }
        tx.execute("DELETE FROM product_alert_settings", [])?;
        tx.execute(
            "DELETE FROM notification_outbox WHERE channel_id='__prominent__'",
            [],
        )?;
        catalog_lifecycle::advance(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub fn checkpoint(&mut self) -> Result<(), StorageError> {
        let _: (i64, i64, i64) =
            self.conn
                .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
        Ok(())
    }
}

fn check_catalog_entry(
    conn: &Connection,
    product: &ProductIdentity,
    name: &str,
) -> Result<(), StorageError> {
    if name.len() > crate::ricoh::MAX_PRODUCT_NAME_BYTES {
        return Err(StorageError::ProductNameTooLong);
    }
    let configured: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM products WHERE product_key=?1 AND is_configured=1)",
        [&product.key],
        |row| row.get(0),
    )?;
    if !configured {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM products WHERE is_configured=1",
            [],
            |row| row.get(0),
        )?;
        if count >= MAX_CATALOG_PRODUCTS as i64 {
            return Err(StorageError::CatalogFull);
        }
    }
    Ok(())
}

fn ensure_product(tx: &Connection, product: &ProductIdentity) -> Result<(), StorageError> {
    let stored = tx
        .query_row(
            "SELECT product_id, sku_id FROM products WHERE product_key = ?1",
            [&product.key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    match stored {
        Some((product_id, sku_id))
            if product_id != product.product_id || sku_id != product.sku_id =>
        {
            Err(StorageError::ProductIdentityConflict)
        }
        Some(_) => Ok(()),
        None => {
            tx.execute(
                "INSERT INTO products (product_key, product_id, sku_id) VALUES (?1, ?2, ?3)",
                params![product.key, product.product_id, product.sku_id],
            )?;
            Ok(())
        }
    }
}

fn availability_to_db(value: Availability) -> &'static str {
    match value {
        Availability::InStock => "in_stock",
        Availability::OutOfStock => "out_of_stock",
    }
}

fn availability_from_db(value: &str) -> rusqlite::Result<Availability> {
    match value {
        "in_stock" => Ok(Availability::InStock),
        "out_of_stock" => Ok(Availability::OutOfStock),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn event_kind_to_db(value: ListingEventKind) -> &'static str {
    match value {
        ListingEventKind::FirstObservedInStock => "first_observed_in_stock",
        ListingEventKind::OutOfStockToInStock => "out_of_stock_to_in_stock",
        ListingEventKind::StockIncreased => "stock_increased",
        ListingEventKind::MonitoringFailed => "monitoring_failed",
        ListingEventKind::Recovered => "recovered",
    }
}

fn event_kind_from_db(value: &str) -> rusqlite::Result<ListingEventKind> {
    match value {
        "first_observed_in_stock" => Ok(ListingEventKind::FirstObservedInStock),
        "out_of_stock_to_in_stock" => Ok(ListingEventKind::OutOfStockToInStock),
        "stock_increased" => Ok(ListingEventKind::StockIncreased),
        "monitoring_failed" => Ok(ListingEventKind::MonitoringFailed),
        "recovered" => Ok(ListingEventKind::Recovered),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn observation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredObservation> {
    let sequence: i64 = row.get(6)?;
    Ok(StoredObservation {
        product: ProductIdentity {
            key: row.get(0)?,
            product_id: row.get(1)?,
            sku_id: row.get(2)?,
        },
        state: ObservationState {
            availability: availability_from_db(&row.get::<_, String>(3)?)?,
            is_show: row.get(4)?,
            stock: row.get(5)?,
            request_sequence: u64::try_from(sequence).map_err(|_| rusqlite::Error::InvalidQuery)?,
            observed_at_ms: row.get(7)?,
        },
    })
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ListingEvent> {
    let sequence: i64 = row.get(6)?;
    Ok(ListingEvent {
        id: row.get(0)?,
        product: ProductIdentity {
            key: row.get(1)?,
            product_id: row.get(2)?,
            sku_id: row.get(3)?,
        },
        kind: event_kind_from_db(&row.get::<_, String>(4)?)?,
        stock: row.get(5)?,
        request_sequence: u64::try_from(sequence).map_err(|_| rusqlite::Error::InvalidQuery)?,
        observed_at_ms: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::reducer::{
        AvailabilityResponse, ObservationReducer, ObservationResult, PrepareOutcome, RuntimeGate,
    };
    use crate::catalog::ProductIdScan;
    use crate::config::MonitorConfig;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static NEXT_DB: AtomicU64 = AtomicU64::new(0);
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    #[test]
    fn onboarding_selection_is_atomic_trusted_and_preserves_metadata() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage.set_onboarding_products(&[65, 66]).unwrap();
        let products = storage.product_configs().unwrap();
        assert_eq!(products.len(), 2);
        assert!(products
            .iter()
            .all(|p| p.enabled && p.verified_at_ms.is_none()));
        storage.complete_setup().unwrap();
        assert_eq!(storage.run_intent().unwrap(), RunIntent::Running);
        storage
            .save_product_metadata(
                "65",
                &crate::ricoh::ProductMetadata {
                    price: Some("999".into()),
                    ..Default::default()
                },
                100,
            )
            .unwrap();
        storage.set_product_prominent_alert("65", true).unwrap();
        storage.set_onboarding_products(&[65]).unwrap();
        assert!(storage.product_metadata("65").unwrap().is_some());
        assert!(storage
            .product_configs()
            .unwrap()
            .iter()
            .any(|p| p.identity.key == "65" && p.enabled && p.prominent_alert));
        let before = storage.product_configs().unwrap();
        let epoch = storage.catalog_epoch().unwrap();
        assert!(storage.set_onboarding_products(&[]).is_err());
        assert_eq!(storage.product_configs().unwrap(), before);
        assert_eq!(storage.catalog_epoch().unwrap(), epoch);
        assert!(storage.set_onboarding_products(&[66, 999999]).is_err());
        assert_eq!(storage.product_configs().unwrap(), before);
        assert_eq!(storage.catalog_epoch().unwrap(), epoch);
        storage.set_onboarding_products(&[66]).unwrap();
        assert!(storage
            .product_configs()
            .unwrap()
            .iter()
            .all(|p| p.enabled == (p.identity.key == "66")));
    }

    #[test]
    fn prominent_setting_survives_reopen_import_and_clears_with_product_lifecycle() {
        let database = TempDatabase::new();
        let mut storage = Storage::open(&database.0).unwrap();
        storage.set_onboarding_products(&[65]).unwrap();
        storage.set_product_prominent_alert("65", true).unwrap();
        drop(storage);
        let mut storage = Storage::open(&database.0).unwrap();
        assert!(storage.product_configs().unwrap()[0].prominent_alert);
        storage
            .import_configuration(
                &MonitorConfig::default(),
                vec![(
                    ProductIdentity {
                        key: "65".into(),
                        product_id: "65".into(),
                        sku_id: None,
                    },
                    "GR".into(),
                    true,
                    false,
                )],
                vec![],
                1,
            )
            .unwrap();
        assert!(!storage.product_configs().unwrap()[0].prominent_alert);
        storage.set_product_prominent_alert("65", true).unwrap();
        storage.remove_product("65").unwrap();
        storage.set_onboarding_products(&[65]).unwrap();
        assert!(!storage.product_configs().unwrap()[0].prominent_alert);
        storage.set_product_prominent_alert("65", true).unwrap();
        storage
            .restore_defaults(&MonitorConfig::default(), false)
            .unwrap();
        storage.set_onboarding_products(&[65]).unwrap();
        assert!(!storage.product_configs().unwrap()[0].prominent_alert);
    }

    #[test]
    fn mode_changes_invalidate_old_rounds_across_connections_even_after_switching_back() {
        use crate::config::MonitoringMode;
        let database = TempDatabase::new();
        let mut worker = Storage::open(&database.0).unwrap();
        worker
            .save_product_config(product("p1"), "商品".into(), "manual".into(), Some(1))
            .unwrap();
        worker.set_product_enabled("p1", true).unwrap();
        worker.set_run_intent(RunIntent::Running, false).unwrap();
        let original = worker.product_generation("p1").unwrap();
        let mut editor = Storage::open(&database.0).unwrap();
        let mut config = editor.monitor_config().unwrap();
        editor.save_monitor_config(&config).unwrap();
        assert_eq!(worker.product_generation("p1").unwrap(), original);
        config.monitoring_mode = MonitoringMode::ProductDetail;
        editor.save_monitor_config(&config).unwrap();
        let detail_round = worker.product_generation("p1").unwrap();
        assert!(detail_round > original);
        config.monitoring_mode = MonitoringMode::ListedProducts;
        editor.save_monitor_config(&config).unwrap();
        assert!(worker.product_generation("p1").unwrap() > detail_round);
        let outcome = worker
            .commit_monitoring_attempt("p1", original, |db| db.record_check_attempt("p1"))
            .unwrap();
        assert_eq!(outcome, None);
        assert_eq!(worker.check_count("p1").unwrap(), 0);
        assert_eq!(
            worker.monitor_config().unwrap().monitoring_mode,
            MonitoringMode::ListedProducts
        );
    }

    #[test]
    fn mode_change_and_round_advance_roll_back_together() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_product_config(product("p1"), "商品".into(), "manual".into(), Some(1))
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        let original = storage.monitor_config().unwrap();
        let generation = storage.product_generation("p1").unwrap();
        storage.conn.execute_batch("CREATE TRIGGER fail_mode_round BEFORE UPDATE ON product_round_sequence BEGIN SELECT RAISE(ABORT,'injected round failure'); END;").unwrap();
        let mut changed = original.clone();
        changed.monitoring_mode = crate::config::MonitoringMode::ProductDetail;
        assert!(storage.save_monitor_config(&changed).is_err());
        assert_eq!(storage.monitor_config().unwrap(), original);
        assert_eq!(storage.product_generation("p1").unwrap(), generation);
    }

    #[test]
    fn check_count_stays_integer_at_database_limit() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_product_config(product("p1"), "GR".into(), "fixture".into(), Some(1))
            .unwrap();
        storage
            .conn
            .execute("UPDATE products SET check_count=?1", [i64::MAX])
            .unwrap();
        storage.record_check_attempt("p1").unwrap();
        assert_eq!(storage.check_count("p1").unwrap(), i64::MAX as u64);
    }

    #[test]
    fn catalog_rejects_growth_but_allows_refresh_at_capacity() {
        let mut storage = Storage::open_in_memory().unwrap();
        for id in 0..1000 {
            storage
                .save_product_config(
                    product(&format!("p{id}")),
                    "GR".into(),
                    "fixture".into(),
                    Some(1),
                )
                .unwrap();
        }
        assert!(storage
            .save_product_config(product("overflow"), "GR".into(), "fixture".into(), Some(1))
            .is_err());
        storage
            .save_product_config(product("p0"), "GR IV".into(), "fixture".into(), Some(2))
            .unwrap();
        assert_eq!(storage.product_configs().unwrap().len(), 1000);
    }

    #[test]
    fn product_name_limit_preserves_previous_value_on_failure() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_product_config(product("p1"), "GR".into(), "fixture".into(), Some(1))
            .unwrap();
        assert!(storage
            .save_product_config(product("p1"), "a".repeat(1025), "fixture".into(), Some(2))
            .is_err());
        assert_eq!(storage.product_configs().unwrap()[0].name, "GR");
    }

    #[test]
    fn scan_failure_survives_reopen_and_resume_clears_it() {
        let database = TempDatabase::new();
        let mut storage = Storage::open(&database.0).unwrap();
        let scan = ProductIdScan::new(1, 2).unwrap();
        let id = scan.scan_id();
        storage
            .start_scan_if_idle(&scan.checkpoint().unwrap())
            .unwrap();
        storage
            .record_scan_failure(id, "商品目录已达到容量上限")
            .unwrap();
        drop(storage);
        let mut storage = Storage::open(&database.0).unwrap();
        assert!(!storage.scan_active().unwrap());
        assert_eq!(
            storage.scan_failure(id).unwrap().as_deref(),
            Some("商品目录已达到容量上限")
        );
        assert!(storage.control_scan_if_current(id, true, false).unwrap());
        assert!(storage.scan_failure(id).unwrap().is_none());
    }

    #[test]
    fn import_catalog_overflow_rolls_back_all_changes() {
        let mut storage = Storage::open_in_memory().unwrap();
        let old_config = storage.monitor_config().unwrap();
        let mut changed = old_config.clone();
        changed.use_system_proxy = true;
        let products = (0..1001)
            .map(|id| (product(&format!("p{id}")), "GR".into(), false, false))
            .collect();
        assert!(matches!(
            storage.import_configuration(&changed, products, vec![], 1),
            Err(StorageError::CatalogFull)
        ));
        assert_eq!(storage.monitor_config().unwrap(), old_config);
        assert!(storage.product_configs().unwrap().is_empty());
    }

    #[test]
    fn monitoring_commit_serializes_control_and_rejects_the_previous_round() {
        let database = TempDatabase::new();
        let mut storage = Storage::open(&database.0).unwrap();
        storage
            .save_product_config(product("p1"), "样例".into(), "fixture".into(), Some(1))
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        storage.set_run_intent(RunIntent::Running, false).unwrap();
        let generation = storage.product_generation("p1").unwrap();
        let mut controller = Storage::open(&database.0).unwrap();
        controller.conn.busy_timeout(Duration::ZERO).unwrap();
        storage
            .commit_monitoring_attempt("p1", generation, |db| {
                assert!(
                    controller.set_run_intent(RunIntent::Paused, true).is_err(),
                    "轮次校验与结果写入之间，另一实例不能提交控制操作"
                );
                db.record_check_attempt("p1")
            })
            .unwrap()
            .unwrap();
        controller.set_run_intent(RunIntent::Paused, true).unwrap();
        controller
            .set_run_intent(RunIntent::Running, false)
            .unwrap();
        assert!(storage
            .commit_monitoring_attempt("p1", generation, |db| db.record_check_attempt("p1"))
            .unwrap()
            .is_none());
        assert_eq!(storage.check_count("p1").unwrap(), 1);
    }

    #[test]
    fn monitoring_commit_rolls_back_partial_result_on_failure() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_product_config(product("p1"), "样例".into(), "fixture".into(), Some(1))
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        storage.set_run_intent(RunIntent::Running, false).unwrap();
        let generation = storage.product_generation("p1").unwrap();
        let result: Result<Option<()>, _> =
            storage.commit_monitoring_attempt("p1", generation, |db| {
                db.record_check_attempt("p1")?;
                Err(StorageError::InvalidStock)
            });
        assert!(result.is_err());
        assert_eq!(storage.check_count("p1").unwrap(), 0);
        assert!(storage.conn.is_autocommit());
    }

    #[test]
    fn monitor_config_defaults_once_and_saved_value_survives_reopen() {
        let database = TempDatabase::new();
        let mut saved = MonitorConfig::default();
        saved.schedule.start_minute = 8 * 60 + 30;
        saved.requests.interval = Duration::from_millis(7_500);
        saved.use_proxy_pool = true;

        {
            let mut storage = Storage::open(&database.0).unwrap();
            assert_eq!(storage.monitor_config().unwrap(), MonitorConfig::default());
            storage.save_monitor_config(&saved).unwrap();
        }

        let reopened = Storage::open(&database.0).unwrap();
        assert_eq!(reopened.monitor_config().unwrap(), saved);
    }

    #[test]
    fn system_notification_choice_and_scan_checkpoint_survive_reopen() {
        let database = TempDatabase::new();
        let mut storage = Storage::open(&database.0).unwrap();
        assert!(storage.system_notifications_enabled().unwrap());
        assert_eq!(storage.run_intent().unwrap(), RunIntent::Stopped);
        storage.set_system_notifications_enabled(false).unwrap();
        storage.set_run_intent(RunIntent::Running, false).unwrap();
        assert_eq!(storage.run_intent().unwrap(), RunIntent::Running);
        storage.set_run_intent(RunIntent::Paused, true).unwrap();
        let mut scan = ProductIdScan::new(129, 131).unwrap();
        assert_eq!(scan.next_id(), Some(129));
        scan.record_failure().unwrap();
        let checkpoint = scan.checkpoint().unwrap();
        storage.save_scan_checkpoint(&checkpoint).unwrap();

        let mut reopened = Storage::open(&database.0).unwrap();
        assert!(!reopened.system_notifications_enabled().unwrap());
        assert_eq!(reopened.run_intent().unwrap(), RunIntent::Paused);
        assert_eq!(reopened.scan_checkpoint().unwrap(), Some(checkpoint));
        reopened.clear_scan_checkpoint().unwrap();
        assert_eq!(reopened.scan_checkpoint().unwrap(), None);
    }

    #[test]
    fn health_transition_is_written_once_and_queued_by_subscription() {
        let mut storage = Storage::open_in_memory().unwrap();
        let identity = product("p1");
        storage
            .save_notification_channel(
                "failure-channel",
                "failure alerts",
                "feishu",
                Some("credential-ref"),
                &["monitoring_failed".into()],
            )
            .unwrap();
        storage
            .mark_notification_channel_tested("failure-channel")
            .unwrap();
        storage
            .set_notification_channel_enabled("failure-channel", true)
            .unwrap();

        let event = storage
            .commit_health_transition(
                identity.clone(),
                7,
                MonitoringHealthTransition::Failed { since_ms: 100 },
                10_000,
                &["failure-channel"],
            )
            .unwrap()
            .unwrap();
        assert_eq!(event.kind, ListingEventKind::MonitoringFailed);
        assert_eq!(event.observed_at_ms, 10_000);

        let repeated = storage
            .commit_health_transition(
                identity,
                7,
                MonitoringHealthTransition::Failed { since_ms: 100 },
                10_000,
                &["failure-channel"],
            )
            .unwrap()
            .unwrap();
        assert_eq!(repeated.id, event.id);
    }

    #[test]
    fn reopening_database_without_config_does_not_seed_defaults() {
        let database = TempDatabase::new();
        {
            let storage = Storage::open(&database.0).unwrap();
            storage
                .conn
                .execute("DELETE FROM settings WHERE id = 1", [])
                .unwrap();
        }

        let reopened = Storage::open(&database.0).unwrap();
        assert!(matches!(
            reopened.monitor_config(),
            Err(StorageError::MonitorConfigMissing)
        ));
    }

    #[test]
    fn invalid_monitor_config_is_rejected_without_changing_value() {
        let mut storage = Storage::open_in_memory().unwrap();
        let original = storage.monitor_config().unwrap();
        let mut invalid = MonitorConfig::default();
        invalid.requests.interval = Duration::ZERO;

        assert!(storage.save_monitor_config(&invalid).is_err());
        assert_eq!(storage.monitor_config().unwrap(), original);
    }

    #[test]
    fn proxy_transport_failures_cool_after_three_and_disable_after_five() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_proxy(&StoredProxy {
                id: "proxy-a".into(),
                protocol: "http".into(),
                host: "127.0.0.1".into(),
                port: 8888,
                credential_ref: None,
                enabled: true,
                status: "available".into(),
                cooldown_until_ms: None,
                consecutive_failures: 0,
            })
            .unwrap();
        storage.record_proxy_failure("proxy-a").unwrap();
        storage.record_proxy_failure("proxy-a").unwrap();
        storage.record_proxy_failure("proxy-a").unwrap();
        let proxy = storage.proxies().unwrap().remove(0);
        assert_eq!(proxy.status, "cooldown");
        assert_eq!(proxy.consecutive_failures, 3);
        assert!(proxy.cooldown_until_ms.unwrap() > now_ms_for_proxy());

        storage
            .conn
            .execute(
                "UPDATE proxies SET cooldown_until_ms=0 WHERE id='proxy-a'",
                [],
            )
            .unwrap();
        assert_eq!(storage.proxies().unwrap()[0].status, "available");
        storage.record_proxy_failure("proxy-a").unwrap();
        assert_eq!(storage.proxies().unwrap()[0].consecutive_failures, 4);
        storage.record_proxy_failure("proxy-a").unwrap();
        let proxy = storage.proxies().unwrap().remove(0);
        assert_eq!(proxy.status, "auto_disabled");
        assert!(!proxy.enabled);
        assert_eq!(proxy.consecutive_failures, 5);

        storage.record_proxy_success("proxy-a").unwrap();
        let proxy = storage.proxies().unwrap().remove(0);
        assert_eq!(proxy.status, "available");
        assert_eq!(proxy.consecutive_failures, 0);
        assert!(!proxy.enabled);
    }

    #[test]
    fn sqlite_write_failure_preserves_monitor_config() {
        let mut storage = Storage::open_in_memory().unwrap();
        let original = storage.monitor_config().unwrap();
        storage
            .conn
            .execute_batch(
                "CREATE TRIGGER fail_monitor_config_update BEFORE UPDATE ON settings
                 BEGIN SELECT RAISE(ABORT, 'injected config write failure'); END;",
            )
            .unwrap();
        let mut changed = original.clone();
        changed.requests.interval = Duration::from_secs(10);

        assert!(storage.save_monitor_config(&changed).is_err());
        assert_eq!(storage.monitor_config().unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn opening_database_restricts_database_and_existing_wal_files() {
        use std::os::unix::fs::PermissionsExt;

        let database = TempDatabase::new();
        let storage = Storage::open(&database.0).unwrap();
        storage
            .conn
            .execute("INSERT INTO credentials(kind,reference,value) VALUES('channel','fixture','fixture-secret')", [])
            .unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&database.0), 0o600);
        let mut sidecars = Vec::new();
        for suffix in ["-wal", "-shm"] {
            let mut path = database.0.as_os_str().to_owned();
            path.push(suffix);
            let path = PathBuf::from(path);
            if path.exists() {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
                sidecars.push(path);
            }
        }
        assert!(
            !sidecars.is_empty(),
            "WAL mode should create sidecar files while open"
        );
        std::fs::set_permissions(&database.0, std::fs::Permissions::from_mode(0o644)).unwrap();

        let reopened = Storage::open(&database.0).unwrap();
        assert_eq!(mode(&database.0), 0o600);
        for sidecar in sidecars {
            assert_eq!(mode(&sidecar), 0o600);
        }
        assert_eq!(
            reopened
                .credential("channel", "fixture")
                .unwrap()
                .as_deref(),
            Some("fixture-secret")
        );
    }

    #[derive(Clone, Debug, PartialEq)]
    struct StockObservation {
        product: ProductIdentity,
        availability: Availability,
        is_show: u8,
        stock: Option<f64>,
        request_sequence: u64,
        observed_at_ms: i64,
    }

    #[derive(Clone)]
    enum ObservationAttempt {
        Valid(StockObservation),
        Failed,
    }

    #[derive(Clone, Debug, PartialEq)]
    enum ApplyOutcome {
        Applied { event: Option<ListingEvent> },
        FailedResponse,
        Stale,
    }

    impl Storage {
        fn apply_attempt(
            &mut self,
            attempt: ObservationAttempt,
            channel_ids: &[&str],
        ) -> Result<ApplyOutcome, StorageError> {
            // 测试场景显式建立当前已测试渠道，再走正式入队校验。
            for id in channel_ids {
                if !self
                    .notification_channels()?
                    .iter()
                    .any(|channel| channel.id == *id)
                {
                    self.save_notification_channel(
                        id,
                        id,
                        "system",
                        None,
                        &["stock_available".into()],
                    )?;
                    self.mark_notification_channel_tested(id)?;
                    self.set_notification_channel_enabled(id, true)?;
                }
            }
            let observation = match attempt {
                ObservationAttempt::Failed => return Ok(ApplyOutcome::FailedResponse),
                ObservationAttempt::Valid(observation) => observation,
            };
            let initial = self
                .observation(&observation.product.key)?
                .map(|stored| stored.state);
            let mut reducer = ObservationReducer::new(initial);
            let gate = RuntimeGate {
                generation: 1,
                enabled: true,
                paused: false,
            };
            let response = AvailabilityResponse {
                sequence: observation.request_sequence,
                generation: gate.generation,
                result: Ok(ObservationResult {
                    availability: observation.availability,
                    is_show: observation.is_show,
                    stock: observation.stock,
                    observed_at_ms: observation.observed_at_ms,
                }),
            };
            let PrepareOutcome::Prepared(prepared) = reducer.prepare(response, gate) else {
                return Ok(ApplyOutcome::Stale);
            };
            self.commit_observation(observation.product, prepared, channel_ids)
                .map(|committed| ApplyOutcome::Applied {
                    event: committed.event,
                })
        }
    }

    #[test]
    fn restore_defaults_preserves_history_unless_requested() {
        for clear_history in [false, true] {
            let mut storage = Storage::open_in_memory().unwrap();
            storage
                .save_product_config(product("p1"), "Fixture".into(), "fixture".into(), Some(100))
                .unwrap();
            storage.set_product_enabled("p1", true).unwrap();
            storage.record_check_attempt("p1").unwrap();
            storage
                .apply_attempt(
                    valid(observation("p1", Availability::InStock, 2.0, 1, 100)),
                    &[],
                )
                .unwrap();
            assert_eq!(storage.recent_events(10).unwrap().len(), 1);
            storage
                .restore_defaults(&MonitorConfig::default(), clear_history)
                .unwrap();
            assert!(storage.product_configs().unwrap().is_empty());
            assert!(storage.observation("p1").unwrap().is_none());
            assert_eq!(
                storage.recent_events(10).unwrap().len(),
                usize::from(!clear_history)
            );
            assert_eq!(
                storage.recent_check_runs(10).unwrap().len(),
                usize::from(!clear_history)
            );
            if !clear_history {
                assert_eq!(storage.recent_check_runs(10).unwrap()[0].name, "Fixture");
                let current: (bool, bool, Option<i64>, u64) = storage.conn.query_row(
                    "SELECT is_configured, enabled, verified_at_ms, check_count FROM products WHERE product_key='p1'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                ).unwrap();
                assert_eq!(current, (false, false, None, 0));
            }
        }
    }

    #[test]
    fn failed_transaction_does_not_advance_reducer_and_retry_commits_its_transition() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut reducer = ObservationReducer::new(None);
        let initial = AvailabilityResponse {
            sequence: 1,
            generation: 7,
            result: Ok(ObservationResult {
                availability: Availability::OutOfStock,
                is_show: 0,
                stock: Some(0.0),
                observed_at_ms: 100,
            }),
        };
        let gate = RuntimeGate {
            generation: 7,
            enabled: true,
            paused: false,
        };
        let PrepareOutcome::Prepared(prepared) = reducer.prepare(initial, gate) else {
            panic!("首次有效观察应通过 reducer 判定");
        };
        storage
            .commit_observation(product("p1"), prepared, &[])
            .unwrap();

        storage
            .conn
            .execute_batch(
                "CREATE TRIGGER fail_event BEFORE INSERT ON events
                 BEGIN SELECT RAISE(ABORT, 'injected event failure'); END;",
            )
            .unwrap();
        let response = AvailabilityResponse {
            sequence: 2,
            generation: 7,
            result: Ok(ObservationResult {
                availability: Availability::InStock,
                is_show: 1,
                stock: Some(3.0),
                observed_at_ms: 200,
            }),
        };
        let PrepareOutcome::Prepared(prepared) = reducer.prepare(response, gate) else {
            panic!("有效响应应先通过 reducer 判定");
        };
        assert!(storage
            .commit_observation(product("p1"), prepared, &["channel-a"])
            .is_err());
        assert_eq!(reducer.state().unwrap().request_sequence, 1);
        assert_eq!(
            reducer.state().unwrap().availability,
            Availability::OutOfStock
        );
        assert_eq!(
            storage
                .observation("p1")
                .unwrap()
                .unwrap()
                .state
                .request_sequence,
            1
        );

        storage
            .conn
            .execute_batch("DROP TRIGGER fail_event;")
            .unwrap();
        let PrepareOutcome::Prepared(prepared) = reducer.prepare(response, gate) else {
            panic!("事务回滚后，同一有效结果仍应可以归并");
        };
        let committed = storage
            .commit_observation(product("p1"), prepared, &["channel-a"])
            .unwrap();
        assert_eq!(
            committed.event.unwrap().kind,
            ListingEventKind::OutOfStockToInStock
        );
        assert_eq!(reducer.state().unwrap().request_sequence, 2);
        assert_eq!(reducer.state().unwrap().stock, Some(3.0));
        assert_eq!(
            storage
                .observation("p1")
                .unwrap()
                .unwrap()
                .state
                .request_sequence,
            2
        );
    }

    pub(super) struct TempDatabase(pub(super) PathBuf);

    impl TempDatabase {
        pub(super) fn new() -> Self {
            let id = NEXT_DB.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ricoh-observation-storage-{}-{id}.sqlite",
                std::process::id()
            ));
            Self(path)
        }
    }

    impl Drop for TempDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
            let _ = fs::remove_file(format!("{}-wal", self.0.display()));
            let _ = fs::remove_file(format!("{}-shm", self.0.display()));
        }
    }

    fn product(key: &str) -> ProductIdentity {
        ProductIdentity {
            key: key.to_owned(),
            product_id: format!("product-{key}"),
            sku_id: Some(format!("sku-{key}")),
        }
    }

    fn observation(
        key: &str,
        availability: Availability,
        stock: f64,
        sequence: u64,
        observed_at_ms: i64,
    ) -> StockObservation {
        StockObservation {
            product: product(key),
            availability,
            is_show: u8::from(availability == Availability::InStock),
            stock: Some(stock),
            request_sequence: sequence,
            observed_at_ms,
        }
    }

    fn valid(value: StockObservation) -> ObservationAttempt {
        ObservationAttempt::Valid(value)
    }

    fn insert_event(
        storage: &Storage,
        key: &str,
        sequence: i64,
        stock: f64,
        observed_at_ms: i64,
    ) -> i64 {
        storage
            .conn
            .execute(
                "INSERT INTO events (
                     product_key, event_kind, stock, request_sequence, observed_at_ms
                 )
                 VALUES (?1, 'out_of_stock_to_in_stock', ?2, ?3, ?4)",
                rusqlite::params![key, stock, sequence, observed_at_ms],
            )
            .unwrap();
        storage.conn.last_insert_rowid()
    }

    #[test]
    fn persists_product_stock_sequence_time_and_first_listing_event() {
        let mut storage = Storage::open_in_memory().unwrap();

        assert_eq!(storage.observation("p1").unwrap(), None);
        let outcome = storage
            .apply_attempt(
                valid(observation(
                    "p1",
                    Availability::InStock,
                    4.25,
                    1,
                    1_790_000_000_123,
                )),
                &["channel-a"],
            )
            .unwrap();

        let ApplyOutcome::Applied { event: Some(event) } = outcome else {
            panic!("首次有货应产生上架事件");
        };
        assert_eq!(event.product, product("p1"));
        assert_eq!(event.kind, ListingEventKind::FirstObservedInStock);
        assert_eq!(event.stock, 4.25);
        assert_eq!(event.request_sequence, 1);
        assert_eq!(event.observed_at_ms, 1_790_000_000_123);
        assert_eq!(
            storage.observation("p1").unwrap(),
            Some(StoredObservation {
                product: product("p1"),
                state: ObservationState {
                    availability: Availability::InStock,
                    is_show: 1,
                    stock: Some(4.25),
                    request_sequence: 1,
                    observed_at_ms: 1_790_000_000_123,
                },
            })
        );
    }

    #[test]
    fn stock_changes_emit_alert_events_and_keep_listing_history() {
        let mut storage = Storage::open_in_memory().unwrap();

        let first_out = storage
            .apply_attempt(
                valid(observation("p1", Availability::OutOfStock, 0.0, 1, 100)),
                &["channel-a"],
            )
            .unwrap();
        assert_eq!(first_out, ApplyOutcome::Applied { event: None });
        assert!(storage
            .events_for_product("p1", None, None, 500)
            .unwrap()
            .is_empty());

        let listed = storage
            .apply_attempt(
                valid(observation("p1", Availability::InStock, 5.0, 2, 200)),
                &["channel-a", "channel-b"],
            )
            .unwrap();
        let ApplyOutcome::Applied { event: Some(event) } = listed else {
            panic!("无货转有货应产生事件");
        };
        assert_eq!(event.kind, ListingEventKind::OutOfStockToInStock);

        let continuous = storage
            .apply_attempt(
                valid(observation("p1", Availability::InStock, 8.0, 3, 300)),
                &["channel-a"],
            )
            .unwrap();
        let ApplyOutcome::Applied {
            event: Some(increased),
        } = continuous
        else {
            panic!("库存增加应产生提醒事件");
        };
        assert_eq!(increased.kind, ListingEventKind::StockIncreased);
        assert_eq!(increased.stock, 8.0);
        assert!(increased.id > event.id);
        assert_eq!(
            storage.observation("p1").unwrap().unwrap().state.stock,
            Some(8.0)
        );
        assert_eq!(
            storage
                .events_for_product("p1", None, None, 500)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn only_changed_stock_creates_message_and_check_attempts_survive_reopen() {
        let database = TempDatabase::new();
        let mut storage = Storage::open(&database.0).unwrap();
        for (sequence, stock, at) in [(1, 0.0, 100), (2, 0.0, 200), (3, 2.0, 300)] {
            let availability = if stock > 0.0 {
                Availability::InStock
            } else {
                Availability::OutOfStock
            };
            storage
                .apply_attempt(
                    valid(observation("p1", availability, stock, sequence, at)),
                    &[],
                )
                .unwrap();
            storage.record_check_attempt("p1").unwrap();
        }
        let runs = storage.recent_check_runs(10).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].stock, runs[0].first_at_ms), (Some(2.0), 300));
        assert_eq!((runs[1].stock, runs[1].first_at_ms), (Some(0.0), 100));
        assert_eq!(storage.check_count("p1").unwrap(), 3);
        drop(storage);
        let mut reopened = Storage::open(&database.0).unwrap();
        let runs = reopened.recent_check_runs(10).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(reopened.check_count("p1").unwrap(), 3);
        reopened.record_check_attempt("p1").unwrap();
        reopened
            .apply_attempt(
                valid(observation("p1", Availability::InStock, 2.0, 4, 400)),
                &[],
            )
            .unwrap();
        let runs = reopened.recent_check_runs(10).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].first_at_ms, 300);
        assert_eq!(reopened.check_count("p1").unwrap(), 4);
        reopened.clear_history().unwrap();
        assert!(reopened.recent_check_runs(10).unwrap().is_empty());
        assert_eq!(reopened.check_count("p1").unwrap(), 0);
    }

    #[test]
    fn absent_stock_survives_reopen_history_and_dedup_without_erasing_metadata() {
        let database = TempDatabase::new();
        let mut storage = Storage::open(&database.0).unwrap();
        storage
            .apply_attempt(
                valid(observation("p1", Availability::OutOfStock, 0.0, 1, 100)),
                &[],
            )
            .unwrap();
        let metadata = crate::ricoh::ProductMetadata {
            image_url: Some("https://example.com/product.jpg".into()),
            price: Some("123.45".into()),
            ..Default::default()
        };
        storage.save_product_metadata("p1", &metadata, 100).unwrap();
        for sequence in [2, 3] {
            let mut absent = observation(
                "p1",
                Availability::OutOfStock,
                0.0,
                sequence,
                sequence as i64 * 100,
            );
            absent.stock = None;
            assert_eq!(
                storage.apply_attempt(valid(absent), &[]).unwrap(),
                ApplyOutcome::Applied { event: None }
            );
        }
        assert_eq!(
            storage.observation("p1").unwrap().unwrap().state.stock,
            None
        );
        let physical: (f64, bool) = storage
            .conn
            .query_row(
                "SELECT stock,EXISTS(SELECT 1 FROM observation_stock_absences WHERE product_key='p1') FROM observations WHERE product_key='p1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(physical, (0.0, true));
        assert_eq!(
            storage
                .recent_check_runs(10)
                .unwrap()
                .iter()
                .map(|run| run.stock)
                .collect::<Vec<_>>(),
            vec![None, Some(0.0)]
        );
        assert_eq!(
            storage
                .query_history(0, 1000, None, None, 100)
                .unwrap()
                .items
                .iter()
                .map(|item| item.stock)
                .collect::<Vec<_>>(),
            vec![None, Some(0.0)]
        );
        assert!(storage.recent_events(10).unwrap().is_empty());
        drop(storage);

        let mut reopened = Storage::open(&database.0).unwrap();
        assert_eq!(
            reopened.observation("p1").unwrap().unwrap().state.stock,
            None
        );
        assert_eq!(
            reopened.product_metadata("p1").unwrap(),
            Some((metadata, 100))
        );
        let mut absent = observation("p1", Availability::OutOfStock, 0.0, 4, 400);
        absent.stock = None;
        reopened.apply_attempt(valid(absent), &[]).unwrap();
        assert_eq!(reopened.recent_check_runs(10).unwrap().len(), 2);
        let positive = reopened
            .apply_attempt(
                valid(observation("p1", Availability::InStock, 3.0, 5, 500)),
                &["channel-a"],
            )
            .unwrap();
        let ApplyOutcome::Applied { event: Some(event) } = positive else {
            panic!("本轮未见后有货应通知一次")
        };
        assert_eq!(event.stock, 3.0);
        assert_eq!(event.kind, ListingEventKind::OutOfStockToInStock);
        assert_eq!(
            reopened
                .apply_attempt(
                    valid(observation("p1", Availability::InStock, 3.0, 6, 600)),
                    &["channel-a"]
                )
                .unwrap(),
            ApplyOutcome::Applied { event: None }
        );
        assert_eq!(reopened.recent_events(10).unwrap().len(), 1);
        assert_eq!(reopened.recent_check_runs(10).unwrap().len(), 3);
        assert_eq!(
            reopened
                .query_history(0, 1000, None, None, 100)
                .unwrap()
                .items
                .iter()
                .map(|item| item.stock)
                .collect::<Vec<_>>(),
            vec![Some(3.0), None, Some(0.0)]
        );
    }

    #[test]
    fn absence_markers_cascade_when_history_or_observations_are_deleted() {
        let mut storage = Storage::open_in_memory().unwrap();
        for key in ["p1", "p2"] {
            let mut absent = observation(key, Availability::OutOfStock, 0.0, 1, 100);
            absent.stock = None;
            storage.apply_attempt(valid(absent), &[]).unwrap();
        }
        let count = |db: &Storage, table: &str| {
            db.conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, usize>(0)
                })
                .unwrap()
        };
        assert_eq!(count(&storage, "observation_stock_absences"), 2);
        assert_eq!(count(&storage, "check_run_stock_absences"), 2);
        assert_eq!(storage.cleanup(HISTORY_WINDOW_MS + 1000, 10).unwrap(), 2);
        assert_eq!(count(&storage, "check_run_stock_absences"), 0);
        assert_eq!(count(&storage, "observation_stock_absences"), 2);
        assert_eq!(
            storage.observation("p1").unwrap().unwrap().state.stock,
            None
        );
        storage
            .apply_attempt(
                valid(observation("p1", Availability::OutOfStock, 0.0, 2, 200)),
                &[],
            )
            .unwrap();
        assert_eq!(count(&storage, "observation_stock_absences"), 1);
        let mut absent = observation("p1", Availability::OutOfStock, 0.0, 3, 300);
        absent.stock = None;
        storage.apply_attempt(valid(absent), &[]).unwrap();
        assert_eq!(count(&storage, "check_run_stock_absences"), 1);
        storage.clear_history().unwrap();
        assert_eq!(count(&storage, "check_run_stock_absences"), 0);
        assert_eq!(count(&storage, "observation_stock_absences"), 2);
        storage
            .restore_defaults(&MonitorConfig::default(), false)
            .unwrap();
        assert_eq!(count(&storage, "observation_stock_absences"), 0);
    }

    #[test]
    fn known_zero_after_absence_creates_a_distinct_history_row_without_notification() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut absent = observation("p1", Availability::OutOfStock, 0.0, 1, 100);
        absent.stock = None;
        storage.apply_attempt(valid(absent), &[]).unwrap();
        for sequence in [2, 3] {
            let result = storage
                .apply_attempt(
                    valid(observation(
                        "p1",
                        Availability::OutOfStock,
                        0.0,
                        sequence,
                        sequence as i64 * 100,
                    )),
                    &[],
                )
                .unwrap();
            assert_eq!(result, ApplyOutcome::Applied { event: None });
        }
        assert_eq!(
            storage.observation("p1").unwrap().unwrap().state.stock,
            Some(0.0)
        );
        assert_eq!(
            storage
                .recent_check_runs(10)
                .unwrap()
                .iter()
                .map(|run| run.stock)
                .collect::<Vec<_>>(),
            vec![Some(0.0), None]
        );
        assert_eq!(
            storage
                .query_history(0, 1000, None, None, 100)
                .unwrap()
                .items
                .iter()
                .map(|item| item.stock)
                .collect::<Vec<_>>(),
            vec![Some(0.0), None]
        );
        assert!(storage.recent_events(10).unwrap().is_empty());
    }

    #[test]
    fn failed_and_stale_responses_do_not_advance_valid_observation() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .apply_attempt(
                valid(observation("p1", Availability::OutOfStock, 6.0, 20, 2_000)),
                &[],
            )
            .unwrap();

        assert_eq!(
            storage
                .apply_attempt(ObservationAttempt::Failed, &[],)
                .unwrap(),
            ApplyOutcome::FailedResponse
        );
        let before = storage.observation("p1").unwrap().unwrap();
        assert_eq!(before.state.request_sequence, 20);
        assert_eq!(before.state.observed_at_ms, 2_000);
        assert_eq!(before.state.stock, Some(6.0));

        assert!(matches!(
            storage
                .apply_attempt(
                    valid(observation("p1", Availability::InStock, 4.0, 21, 2_100)),
                    &[],
                )
                .unwrap(),
            ApplyOutcome::Applied { event: Some(_) }
        ));
        assert_eq!(
            storage
                .apply_attempt(
                    valid(observation("p1", Availability::OutOfStock, 0.0, 19, 2_200)),
                    &[],
                )
                .unwrap(),
            ApplyOutcome::Stale
        );
        let after = storage.observation("p1").unwrap().unwrap();
        assert_eq!(after.state.availability, Availability::InStock);
        assert_eq!(after.state.request_sequence, 21);
        assert_eq!(after.state.observed_at_ms, 2_100);
    }

    #[test]
    fn failed_event_transaction_keeps_previous_observation() {
        let temp = TempDatabase::new();
        let mut storage = Storage::open(&temp.0).unwrap();
        storage
            .apply_attempt(
                valid(observation("p1", Availability::OutOfStock, 0.0, 1, 100)),
                &[],
            )
            .unwrap();

        storage
            .conn
            .execute_batch(
                "CREATE TRIGGER fail_event BEFORE INSERT ON events
                 BEGIN SELECT RAISE(ABORT, 'injected event failure'); END;",
            )
            .unwrap();
        assert!(storage
            .apply_attempt(
                valid(observation("p1", Availability::InStock, 3.0, 2, 200)),
                &["channel-a"],
            )
            .is_err());
        storage
            .conn
            .execute_batch("DROP TRIGGER fail_event;")
            .unwrap();
        drop(storage);

        let mut storage = Storage::open(&temp.0).unwrap();
        let old = storage.observation("p1").unwrap().unwrap();
        assert_eq!(old.state.availability, Availability::OutOfStock);
        assert_eq!(old.state.stock, Some(0.0));
        assert_eq!(old.state.request_sequence, 1);
        assert_eq!(old.state.observed_at_ms, 100);
        assert!(storage
            .events_for_product("p1", None, None, 500)
            .unwrap()
            .is_empty());

        assert!(matches!(
            storage
                .apply_attempt(
                    valid(observation("p1", Availability::InStock, 3.0, 2, 200)),
                    &["channel-a"],
                )
                .unwrap(),
            ApplyOutcome::Applied { event: Some(_) }
        ));
        let events = storage.events_for_product("p1", None, None, 500).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn committed_retry_is_idempotent_for_events_and_channel_registrations() {
        let temp = TempDatabase::new();
        let mut storage = Storage::open(&temp.0).unwrap();
        storage
            .apply_attempt(
                valid(observation("p1", Availability::OutOfStock, 0.0, 1, 100)),
                &[],
            )
            .unwrap();
        let transition = valid(observation("p1", Availability::InStock, 3.0, 2, 200));

        let first = storage
            .apply_attempt(transition.clone(), &["channel-a", "channel-a"])
            .unwrap();
        let ApplyOutcome::Applied { event: Some(_) } = first else {
            panic!("首次提交应产生事件");
        };
        drop(storage);

        let mut storage = Storage::open(&temp.0).unwrap();
        assert_eq!(
            storage.apply_attempt(transition, &["channel-a"]).unwrap(),
            ApplyOutcome::Stale
        );
        assert_eq!(
            storage
                .events_for_product("p1", None, None, 500)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn reopened_database_restores_observation_and_event() {
        let temp = TempDatabase::new();
        let mut storage = Storage::open(&temp.0).unwrap();
        let first = storage
            .apply_attempt(
                valid(observation("p1", Availability::InStock, 4.0, 3, 3_000)),
                &["channel-a"],
            )
            .unwrap();
        let ApplyOutcome::Applied { event: Some(_) } = first else {
            panic!("首次有货应产生事件");
        };
        drop(storage);

        let mut storage = Storage::open(&temp.0).unwrap();
        assert_eq!(
            storage.observation("p1").unwrap().unwrap(),
            StoredObservation {
                product: product("p1"),
                state: ObservationState {
                    availability: Availability::InStock,
                    is_show: 1,
                    stock: Some(4.0),
                    request_sequence: 3,
                    observed_at_ms: 3_000,
                },
            }
        );
        assert_eq!(
            storage
                .events_for_product("p1", None, None, 500)
                .unwrap()
                .len(),
            1
        );
        let increased = storage
            .apply_attempt(
                valid(observation("p1", Availability::InStock, 7.0, 4, 4_000)),
                &["channel-a"],
            )
            .unwrap();
        let ApplyOutcome::Applied {
            event: Some(increased),
        } = increased
        else {
            panic!("重开后库存增加应产生提醒事件");
        };
        assert_eq!(increased.kind, ListingEventKind::StockIncreased);
        drop(storage);
        let mut storage = Storage::open(&temp.0).unwrap();
        assert_eq!(
            storage
                .apply_attempt(
                    valid(observation("p1", Availability::OutOfStock, 0.0, 2, 5_000)),
                    &[],
                )
                .unwrap(),
            ApplyOutcome::Stale
        );
        storage
            .apply_attempt(
                valid(observation("p1", Availability::OutOfStock, 0.0, 5, 5_000)),
                &[],
            )
            .unwrap();
        let restocked = storage
            .apply_attempt(
                valid(observation("p1", Availability::InStock, 7.0, 6, 6_000)),
                &[],
            )
            .unwrap();
        let ApplyOutcome::Applied {
            event: Some(restocked),
        } = restocked
        else {
            panic!("重开后补货应产生事件");
        };
        assert!(restocked.id > increased.id);
    }

    #[test]
    fn product_and_time_queries_do_not_leak_other_rows() {
        let mut storage = Storage::open_in_memory().unwrap();
        for (key, first_time) in [("p1", 100), ("p2", 300)] {
            storage
                .apply_attempt(
                    valid(observation(
                        key,
                        Availability::OutOfStock,
                        0.0,
                        1,
                        first_time,
                    )),
                    &[],
                )
                .unwrap();
            storage
                .apply_attempt(
                    valid(observation(
                        key,
                        Availability::InStock,
                        2.0,
                        2,
                        first_time + 10,
                    )),
                    &["channel-a"],
                )
                .unwrap();
        }

        let p1_events = storage
            .events_for_product("p1", Some(100), Some(200), 500)
            .unwrap();
        assert_eq!(p1_events.len(), 1);
        assert_eq!(p1_events[0].product.key, "p1");
        assert_eq!(p1_events[0].observed_at_ms, 110);
        assert!(storage
            .events_for_product("p1", Some(200), Some(300), 500)
            .unwrap()
            .is_empty());
        assert_eq!(
            storage.observation("p1").unwrap().unwrap().product,
            product("p1")
        );
        assert_eq!(
            storage.observation("p2").unwrap().unwrap().product,
            product("p2")
        );

        let recent = storage.recent_events(1).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].product.key, "p2");
    }

    #[test]
    fn recent_events_returns_stored_fields_newest_first_and_respects_limit() {
        let mut storage = Storage::open_in_memory().unwrap();
        for (key, stock, observed_at_ms) in [("p1", 2.5, 1_000), ("p2", 7.0, 2_000)] {
            storage
                .apply_attempt(
                    valid(observation(
                        key,
                        Availability::InStock,
                        stock,
                        3,
                        observed_at_ms,
                    )),
                    &[],
                )
                .unwrap();
        }

        let recent = storage.recent_events(1).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, 2);
        assert_eq!(recent[0].product, product("p2"));
        assert_eq!(recent[0].kind, ListingEventKind::FirstObservedInStock);
        assert_eq!(recent[0].stock, 7.0);
        assert_eq!(recent[0].request_sequence, 3);
        assert_eq!(recent[0].observed_at_ms, 2_000);

        let all = storage.recent_events(2).unwrap();
        assert_eq!(
            all.iter()
                .map(|event| event.product.key.as_str())
                .collect::<Vec<_>>(),
            ["p2", "p1"]
        );
    }

    #[test]
    fn product_identity_preserves_an_absent_sku() {
        let mut storage = Storage::open_in_memory().unwrap();
        let identity = ProductIdentity {
            key: "product-without-sku".to_owned(),
            product_id: "product-id-1".to_owned(),
            sku_id: None,
        };
        storage
            .apply_attempt(
                valid(StockObservation {
                    product: identity.clone(),
                    availability: Availability::OutOfStock,
                    is_show: 0,
                    stock: Some(0.0),
                    request_sequence: 1,
                    observed_at_ms: 1_000,
                }),
                &[],
            )
            .unwrap();
        assert_eq!(
            storage.observation(&identity.key).unwrap().unwrap().product,
            identity
        );
    }

    #[test]
    fn persists_raw_listing_state_separately_from_availability_and_stock() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .apply_attempt(
                valid(StockObservation {
                    product: product("unlisted-positive-stock"),
                    availability: Availability::OutOfStock,
                    is_show: 0,
                    stock: Some(3.0),
                    request_sequence: 1,
                    observed_at_ms: 1_000,
                }),
                &[],
            )
            .unwrap();
        let observation = storage
            .observation("unlisted-positive-stock")
            .unwrap()
            .unwrap();
        assert_eq!(observation.state.is_show, 0);
        assert_eq!(observation.state.stock, Some(3.0));
        assert_eq!(observation.state.availability, Availability::OutOfStock);
    }

    #[test]
    fn product_config_persists_name_source_verification_and_disabled_default() {
        let database = TempDatabase::new();
        let identity = product("p1");
        {
            let mut storage = Storage::open(&database.0).unwrap();
            storage
                .save_product_config(
                    identity.clone(),
                    "GR IV".to_owned(),
                    "manual".to_owned(),
                    Some(1_790_000_000_123),
                )
                .unwrap();
        }

        let storage = Storage::open(&database.0).unwrap();
        assert_eq!(
            storage.product_configs().unwrap(),
            vec![ProductConfig {
                identity,
                name: "GR IV".to_owned(),
                source: "manual".to_owned(),
                verified_at_ms: Some(1_790_000_000_123),
                enabled: false,
                prominent_alert: false,
            }]
        );
    }

    #[test]
    fn product_cannot_be_enabled_until_verified_and_can_be_toggled() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_product_config(product("p1"), "GR IV".to_owned(), "manual".to_owned(), None)
            .unwrap();

        assert!(matches!(
            storage.set_product_enabled("p1", true),
            Err(StorageError::ProductNotVerified)
        ));
        assert!(!storage.product_configs().unwrap()[0].enabled);

        storage
            .save_product_config(
                product("p1"),
                "GR IV".to_owned(),
                "manual".to_owned(),
                Some(1_790_000_000_123),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        assert!(storage.product_configs().unwrap()[0].enabled);
        storage.set_product_enabled("p1", false).unwrap();
        assert!(!storage.product_configs().unwrap()[0].enabled);
    }

    #[test]
    fn refreshing_product_metadata_preserves_enabled_choice() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_product_config(
                product("p1"),
                "GR IV".to_owned(),
                "catalog".to_owned(),
                Some(100),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();

        storage
            .save_product_config(
                product("p1"),
                "GR IV HDF".to_owned(),
                "catalog".to_owned(),
                Some(200),
            )
            .unwrap();

        assert_eq!(
            storage.product_configs().unwrap(),
            vec![ProductConfig {
                identity: product("p1"),
                name: "GR IV HDF".to_owned(),
                source: "catalog".to_owned(),
                verified_at_ms: Some(200),
                enabled: true,
                prominent_alert: false,
            }]
        );
    }

    #[test]
    fn removing_product_hides_config_but_preserves_observation_and_event() {
        let mut storage = Storage::open_in_memory().unwrap();
        let identity = product("p1");
        storage
            .save_product_config(
                identity.clone(),
                "GR IV".to_owned(),
                "manual".to_owned(),
                Some(100),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        let outcome = storage
            .apply_attempt(
                valid(observation("p1", Availability::InStock, 3.0, 1, 200)),
                &["channel-a"],
            )
            .unwrap();
        let ApplyOutcome::Applied { event: Some(event) } = outcome else {
            panic!("首次有货应产生上架事件");
        };

        storage.remove_product("p1").unwrap();

        assert!(storage.product_configs().unwrap().is_empty());
        assert_eq!(
            storage.observation("p1").unwrap().unwrap().product,
            identity
        );
        assert_eq!(
            storage.events_for_product("p1", None, None, 10).unwrap(),
            [event.clone()]
        );

        storage
            .save_product_config(
                product("p1"),
                "GR IV".to_owned(),
                "manual".to_owned(),
                Some(300),
            )
            .unwrap();
        assert_eq!(storage.product_configs().unwrap().len(), 1);
        assert_eq!(
            storage.events_for_product("p1", None, None, 10).unwrap(),
            [event]
        );
    }

    #[test]
    fn removing_missing_product_does_not_create_round_or_tombstone() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage.remove_product("missing").unwrap();
        for table in ["product_rounds", "product_removals"] {
            let count: i64 = storage
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
    }

    #[test]
    fn unknown_product_is_blocked_only_inside_resumable_scan_range() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut scan = crate::catalog::ProductIdScan::new(700, 700).unwrap();
        storage
            .save_scan_checkpoint(&scan.checkpoint().unwrap())
            .unwrap();
        storage.remove_product("701").unwrap();
        assert_eq!(storage.product_generation("701").unwrap(), 0);
        storage.remove_product("700").unwrap();
        assert!(storage.product_generation("700").unwrap() > 0);
        assert_eq!(
            product_blocked_scan_id(&storage.conn, "700").unwrap(),
            Some(scan.scan_id())
        );
        assert_eq!(storage.cleanup(1_000, 10).unwrap(), 0);
        assert_eq!(scan.next_id(), Some(700));
        scan.record_failure().unwrap();
        storage
            .save_scan_checkpoint(&scan.checkpoint().unwrap())
            .unwrap();
        assert!(storage.cleanup(1_000, 10).unwrap() > 0);
        assert_eq!(product_blocked_scan_id(&storage.conn, "700").unwrap(), None);
    }

    #[test]
    fn cleanup_reclaims_only_removed_identity_without_history_or_outbox() {
        let mut storage = Storage::open_in_memory().unwrap();
        for key in ["p1", "p2", "p3", "p4"] {
            storage
                .save_product_config(product(key), "Fixture".into(), "manual".into(), Some(1))
                .unwrap();
            storage.remove_product(key).unwrap();
        }
        storage
            .conn
            .execute_batch(
                "INSERT INTO events(product_key,event_kind,stock,request_sequence,observed_at_ms)
             VALUES('p1','monitoring_failed',0,1,100);
             INSERT INTO notification_outbox(event_id,channel_id,product_key,product_id,event_kind,
                 stock,request_sequence,product_generation,observed_at_ms,status)
             VALUES(1,'c1','p2','product-p2','monitoring_failed',0,2,1,100,'pending'),
                   (2,'c1','p3','product-p3','monitoring_failed',0,3,1,100,'accepted');",
            )
            .unwrap();
        assert_eq!(storage.cleanup(1_000, 10).unwrap(), 1);
        for key in ["p1", "p2", "p3"] {
            assert!(storage
                .conn
                .query_row(
                    "SELECT 1 FROM products WHERE product_key=?1",
                    [key],
                    |_| Ok(())
                )
                .optional()
                .unwrap()
                .is_some());
        }
        for table in ["products", "product_rounds", "product_removals"] {
            let count: i64 = storage
                .conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE product_key='p4'"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
    }

    #[test]
    fn reclaimed_product_gets_new_round_and_repeated_removal_stays_bounded() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut previous = 0;
        for _ in 0..30 {
            storage
                .save_product_config(product("p1"), "Fixture".into(), "manual".into(), Some(1))
                .unwrap();
            storage.set_product_enabled("p1", true).unwrap();
            let generation = storage.product_generation("p1").unwrap();
            assert!(generation > previous);
            storage.remove_product("p1").unwrap();
            previous = storage.product_generation("p1").unwrap();
            assert_eq!(storage.cleanup(1_000, 1).unwrap(), 1);
            for table in [
                "products",
                "product_rounds",
                "product_removals",
                "observations",
            ] {
                let count: i64 = storage
                    .conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(count, 0, "{table}");
            }
        }
    }

    #[test]
    fn completed_scan_releases_removed_product_tombstone() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut scan = crate::catalog::ProductIdScan::new(1, 1).unwrap();
        storage
            .save_scan_checkpoint(&scan.checkpoint().unwrap())
            .unwrap();
        storage
            .save_product_config(product("1"), "Fixture".into(), "manual".into(), Some(1))
            .unwrap();
        storage.remove_product("1").unwrap();
        assert_eq!(storage.cleanup(1_000, 1).unwrap(), 0);
        assert_eq!(scan.next_id(), Some(1));
        scan.record_failure().unwrap();
        storage
            .save_scan_checkpoint(&scan.checkpoint().unwrap())
            .unwrap();
        assert_eq!(storage.cleanup(1_000, 1).unwrap(), 1);
    }

    #[test]
    fn observation_identity_api_keeps_unconfigured_products_out_of_catalog() {
        let mut storage = Storage::open_in_memory().unwrap();
        let identity = product("p1");
        storage
            .apply_attempt(
                valid(observation("p1", Availability::OutOfStock, 0.0, 1, 100)),
                &[],
            )
            .unwrap();

        assert!(storage.product_configs().unwrap().is_empty());
        assert_eq!(
            storage.observation("p1").unwrap().unwrap().product,
            identity
        );
    }

    #[test]
    fn cleanup_enforces_age_and_count_while_retaining_observation_state() {
        let mut storage = Storage::open_in_memory().unwrap();
        let now_ms = 1_800_000_000_000;
        storage
            .apply_attempt(
                valid(observation(
                    "p1",
                    Availability::InStock,
                    9.0,
                    1,
                    now_ms - DAY_MS,
                )),
                &[],
            )
            .unwrap();
        let cutoff = now_ms - 30 * DAY_MS;
        let old_event_a = insert_event(&storage, "p1", 2, 1.0, cutoff - 1);
        let old_event_b = insert_event(&storage, "p1", 3, 1.0, cutoff - 1);
        let at_cutoff = insert_event(&storage, "p1", 4, 1.0, cutoff);
        assert_eq!(storage.cleanup(now_ms, 1).unwrap(), 1);
        assert_eq!(storage.cleanup(now_ms, 1).unwrap(), 1);
        assert_eq!(storage.cleanup(now_ms, 1).unwrap(), 0);
        let cutoff_exists: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE id = ?1",
                [at_cutoff],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cutoff_exists, 1);
        let old_events_remaining: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE id IN (?1,?2)",
                [old_event_a, old_event_b],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_events_remaining, 0);
        for index in 0..10_001_i64 {
            insert_event(&storage, "p1", 100 + index, 1.0, cutoff + 100 + index);
        }

        for _ in 0..10 {
            let removed = storage.cleanup(now_ms, 1).unwrap();
            assert!(removed <= 1);
            if removed == 0 {
                break;
            }
        }
        let count: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 10_000);
        assert_eq!(
            storage.observation("p1").unwrap().unwrap().state.stock,
            Some(9.0)
        );
    }

    #[test]
    fn thirty_day_cleanup_across_beijing_midnight_survives_reopen() {
        let temp = TempDatabase::new();
        let anchor = 1_800_000_000_000_i64;
        let beijing_midnight =
            (anchor + 8 * 60 * 60 * 1000).div_euclid(DAY_MS) * DAY_MS - 8 * 60 * 60 * 1000;
        let mut storage = Storage::open(&temp.0).unwrap();
        storage
            .apply_attempt(
                valid(observation(
                    "p1",
                    Availability::OutOfStock,
                    0.0,
                    1,
                    beijing_midnight + 1,
                )),
                &[],
            )
            .unwrap();
        let ids = [
            beijing_midnight - 2,
            beijing_midnight - 1,
            beijing_midnight,
            beijing_midnight + 1,
        ]
        .map(|at_ms| insert_event(&storage, "p1", at_ms, 1.0, at_ms));
        let before_cutoff_advance = beijing_midnight + 30 * DAY_MS - 1;
        assert_eq!(storage.cleanup(before_cutoff_advance, 10).unwrap(), 1);
        drop(storage);

        let mut storage = Storage::open(&temp.0).unwrap();
        assert_eq!(storage.cleanup(before_cutoff_advance + 1, 10).unwrap(), 1);
        let mut statement = storage
            .conn
            .prepare("SELECT id FROM events ORDER BY observed_at_ms,id")
            .unwrap();
        let retained: Vec<i64> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(retained, [ids[2], ids[3]]);
    }

    #[test]
    fn each_new_history_row_keeps_both_sources_bounded_and_outbox_intact() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .apply_attempt(
                valid(observation("p1", Availability::OutOfStock, 0.0, 1, 1)),
                &[],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "UPDATE settings SET system_notifications_enabled=1 WHERE id=1",
                [],
            )
            .unwrap();
        storage.conn.execute_batch(
            "WITH RECURSIVE n(x) AS (SELECT 2 UNION ALL SELECT x+1 FROM n WHERE x<10000)
             INSERT INTO check_runs(id,product_key,product_name,availability,is_show,stock,checks,first_at_ms,last_at_ms)
             SELECT x,'p1','旧名称','out_of_stock',0,0,1,x,x FROM n;
             WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10000)
             INSERT INTO events(id,product_key,event_kind,stock,request_sequence,observed_at_ms)
             SELECT x,'p1','monitoring_failed',0,x,x FROM n;"
        ).unwrap();
        storage
            .apply_attempt(
                valid(observation("p1", Availability::InStock, 1.0, 10001, 10001)),
                &[],
            )
            .unwrap();
        let count = |storage: &Storage, table: &str| -> i64 {
            storage
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        assert_eq!(count(&storage, "check_runs"), HISTORY_LIMIT as i64);
        assert_eq!(count(&storage, "events"), HISTORY_LIMIT as i64);
        assert_eq!(
            storage
                .conn
                .query_row("SELECT MIN(id) FROM check_runs", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            storage
                .conn
                .query_row("SELECT MIN(id) FROM events", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(storage.conn.query_row("SELECT event_id FROM notification_outbox WHERE channel_id='__system__' ORDER BY id DESC LIMIT 1", [], |row| row.get::<_, i64>(0)).unwrap(), 10001);
        storage
            .commit_health_transition(
                product("p1"),
                10002,
                MonitoringHealthTransition::Recovered,
                10002,
                &[],
            )
            .unwrap();
        assert_eq!(count(&storage, "events"), HISTORY_LIMIT as i64);
        assert_eq!(
            storage
                .conn
                .query_row("SELECT MIN(id) FROM events", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(storage.conn.query_row("SELECT event_id FROM notification_outbox WHERE channel_id='__system__' ORDER BY id DESC LIMIT 1", [], |row| row.get::<_, i64>(0)).unwrap(), 10002);
    }

    #[test]
    fn bounded_rewrites_can_checkpoint_without_losing_the_latest_state() {
        let temp = TempDatabase::new();
        let mut storage = Storage::open(&temp.0).unwrap();
        let now_ms = 1_800_000_000_000;
        for sequence in 1..=120 {
            let availability = if sequence % 2 == 1 {
                Availability::OutOfStock
            } else {
                Availability::InStock
            };
            storage
                .apply_attempt(
                    valid(observation(
                        "p1",
                        availability,
                        if availability == Availability::InStock {
                            2.0
                        } else {
                            0.0
                        },
                        sequence,
                        now_ms - DAY_MS + sequence as i64 * 100,
                    )),
                    &[],
                )
                .unwrap();
            if sequence % 40 == 0 {
                storage.cleanup(now_ms, 100).unwrap();
                storage.checkpoint().unwrap();
            }
        }
        let event_count: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(event_count, 60);
        storage.cleanup(now_ms, 100).unwrap();
        let after_cleanup: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after_cleanup, 60);
        storage.checkpoint().unwrap();
        assert_eq!(
            storage
                .observation("p1")
                .unwrap()
                .unwrap()
                .state
                .request_sequence,
            120
        );
        assert_eq!(storage.recent_events(500).unwrap().len(), 60);
    }
}
