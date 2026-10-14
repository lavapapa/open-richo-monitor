use super::{
    EventNotificationDetails, ListingEvent, MonitorEventKind, ProductIdentity, StorageError,
};
use crate::config::{MonitorConfig, MonitoringMode, ScheduleConfig};
use rusqlite::{Connection, OptionalExtension};

const DEBOUNCE_MS: i64 = 5_000;

pub(super) fn initialize(conn: &Connection, new_database: bool) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pending_configuration_change (
        singleton INTEGER PRIMARY KEY CHECK(singleton=1),
        previous_config_json TEXT NOT NULL, previous_products_json TEXT NOT NULL,
        due_at_ms INTEGER NOT NULL
    )",
    )?;
    if !super::column_exists(
        conn,
        "settings",
        "system_configuration_change_notifications_enabled",
    )? {
        conn.execute("ALTER TABLE settings ADD COLUMN system_configuration_change_notifications_enabled INTEGER NOT NULL DEFAULT 0", [])?;
    }
    if !super::column_exists(conn, "settings", "configuration_change_intro_pending")? {
        conn.execute("ALTER TABLE settings ADD COLUMN configuration_change_intro_pending INTEGER NOT NULL DEFAULT 0", [])?;
        if !new_database {
            conn.execute("UPDATE settings SET configuration_change_intro_pending=EXISTS(SELECT 1 FROM notification_channels)", [])?;
        }
    }
    Ok(())
}

impl super::Storage {
    pub fn system_configuration_change_notifications_enabled(&self) -> Result<bool, StorageError> {
        Ok(self.conn.query_row(
            "SELECT system_configuration_change_notifications_enabled FROM settings WHERE id=1",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn set_system_configuration_change_notifications_enabled(
        &mut self,
        enabled: bool,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE settings SET system_configuration_change_notifications_enabled=?1 WHERE id=1",
            [enabled],
        )?;
        Ok(())
    }
    pub fn configuration_change_intro_pending(&self) -> Result<bool, StorageError> {
        Ok(self.conn.query_row("SELECT configuration_change_intro_pending AND EXISTS(SELECT 1 FROM notification_channels) FROM settings WHERE id=1", [], |row| row.get(0))?)
    }

    pub fn complete_configuration_change_intro(
        &mut self,
        enabled: bool,
    ) -> Result<(), StorageError> {
        let tx = self.conn.transaction()?;
        let pending: bool = tx.query_row(
            "SELECT configuration_change_intro_pending FROM settings WHERE id=1",
            [],
            |row| row.get(0),
        )?;
        if !pending {
            return Ok(());
        }
        let channels = tx
            .prepare("SELECT id,subscriptions FROM notification_channels")?
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, encoded) in channels {
            let mut scopes: Vec<String> =
                serde_json::from_str(&encoded).map_err(StorageError::ConfigJson)?;
            scopes.retain(|scope| scope != "configuration_changed");
            if enabled {
                scopes.push("configuration_changed".into());
            }
            tx.execute(
                "UPDATE notification_channels SET subscriptions=?2 WHERE id=?1",
                rusqlite::params![
                    id,
                    serde_json::to_string(&scopes).map_err(StorageError::ConfigJson)?
                ],
            )?;
        }
        tx.execute(
            "UPDATE settings SET configuration_change_intro_pending=0 WHERE id=1",
            [],
        )?;
        tx.execute(
            "UPDATE settings SET system_configuration_change_notifications_enabled=?1 WHERE id=1",
            [enabled],
        )?;
        tx.commit()?;
        Ok(())
    }
}

pub(super) fn monitored_products(conn: &Connection) -> Result<Vec<(String, String)>, StorageError> {
    Ok(conn.prepare("SELECT product_key,name FROM products WHERE is_configured=1 AND enabled=1 ORDER BY product_key")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?)
}

pub(super) fn notify_product_changes(
    conn: &Connection,
    previous: &[(String, String)],
) -> Result<(), StorageError> {
    let current = monitored_products(conn)?;
    if product_change(previous, &current).is_some() {
        stage_change(conn, &monitor_config(conn)?, previous)?;
    }
    Ok(())
}

fn product_change(previous: &[(String, String)], current: &[(String, String)]) -> Option<String> {
    let added = current
        .iter()
        .filter(|(key, _)| !previous.iter().any(|(old, _)| old == key))
        .map(|(_, name)| name.as_str())
        .collect::<Vec<_>>();
    let removed = previous
        .iter()
        .filter(|(key, _)| !current.iter().any(|(new, _)| new == key))
        .map(|(_, name)| name.as_str())
        .collect::<Vec<_>>();
    let mut changes = Vec::new();
    if !added.is_empty() {
        changes.push(format!("新增【{}】", added.join("、")));
    }
    if !removed.is_empty() {
        changes.push(format!("减少【{}】", removed.join("、")));
    }
    (!changes.is_empty()).then(|| format!("监控产品变更：{}", changes.join("；")))
}

fn range(products: &[(String, String)]) -> String {
    if products.is_empty() {
        "无".into()
    } else {
        products
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>()
            .join("、")
    }
}

fn schedule(value: &ScheduleConfig) -> String {
    let weekdays = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
    let days = if value.enabled_days.iter().all(|day| *day) {
        "每天".into()
    } else {
        weekdays
            .into_iter()
            .zip(value.enabled_days)
            .filter_map(|(day, enabled)| enabled.then_some(day))
            .collect::<Vec<_>>()
            .join("、")
    };
    if value.start_minute == value.end_minute {
        format!("{days} 全天")
    } else {
        format!(
            "{days} {:02}:{:02}–{}{:02}:{:02}",
            value.start_minute / 60,
            value.start_minute % 60,
            if value.end_minute < value.start_minute {
                "次日 "
            } else {
                ""
            },
            value.end_minute / 60,
            value.end_minute % 60
        )
    }
}

pub(super) fn notify_config_changes(
    conn: &Connection,
    previous: &MonitorConfig,
    current: &MonitorConfig,
) -> Result<(), StorageError> {
    if !config_changes(previous, current).is_empty() {
        stage_change(conn, previous, &monitored_products(conn)?)?;
    }
    Ok(())
}

fn config_changes(previous: &MonitorConfig, current: &MonitorConfig) -> Vec<String> {
    let mut changes = Vec::new();
    if previous.schedule != current.schedule {
        changes.push(format!(
            "监控计划变更：{} → {}",
            schedule(&previous.schedule),
            schedule(&current.schedule)
        ));
    }
    if previous.monitoring_mode != current.monitoring_mode {
        let mode = |value| match value {
            MonitoringMode::ListedProducts => "全站上架列表",
            MonitoringMode::ProductDetail => "逐商品详情",
        };
        changes.push(format!(
            "监控方式变更：{} → {}",
            mode(previous.monitoring_mode),
            mode(current.monitoring_mode)
        ));
    }
    if previous.requests.interval != current.requests.interval
        || previous.requests.jitter_percent != current.requests.jitter_percent
    {
        // 和 AppConfig 的最短/最长等待使用同一换算。
        let wait = |value: &MonitorConfig| {
            let ms = value.requests.interval.as_millis() as f64;
            let jitter = value.requests.jitter_percent / 100.0;
            format!(
                "{}–{} 秒",
                (ms * (1.0 - jitter)).round() / 1000.0,
                (ms * (1.0 + jitter)).round() / 1000.0
            )
        };
        changes.push(format!(
            "检查间隔变更：{} → {}",
            wait(previous),
            wait(current)
        ));
    }
    if previous.failure_alert_after != current.failure_alert_after {
        changes.push(format!(
            "异常提醒阈值变更：{} → {} 分钟",
            previous.failure_alert_after.as_secs() / 60,
            current.failure_alert_after.as_secs() / 60
        ));
    }
    if previous.requests.failures_before_backoff != current.requests.failures_before_backoff
        || previous.requests.failure_backoff != current.requests.failure_backoff
    {
        changes.push(format!(
            "失败等待变更：连续失败 {} 次后等待 {} 秒 → 连续失败 {} 次后等待 {} 秒",
            previous.requests.failures_before_backoff,
            previous.requests.failure_backoff.as_secs(),
            current.requests.failures_before_backoff,
            current.requests.failure_backoff.as_secs()
        ));
    }
    let on = |enabled| if enabled { "开启" } else { "关闭" };
    for (name, old, new) in [
        (
            "商城系统代理",
            previous.use_system_proxy,
            current.use_system_proxy,
        ),
        ("代理池", previous.use_proxy_pool, current.use_proxy_pool),
        (
            "通知系统代理",
            previous.notification_use_system_proxy,
            current.notification_use_system_proxy,
        ),
        (
            "启动后自动监控",
            previous.auto_start_monitoring,
            current.auto_start_monitoring,
        ),
    ] {
        if old != new {
            changes.push(format!("{name}变更：{} → {}", on(old), on(new)));
        }
    }
    changes
}

fn monitor_config(conn: &Connection) -> Result<MonitorConfig, StorageError> {
    let json: String = conn.query_row(
        "SELECT monitor_config_json FROM settings WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    serde_json::from_str(&json).map_err(StorageError::ConfigJson)
}

fn stage_change(
    conn: &Connection,
    previous: &MonitorConfig,
    products: &[(String, String)],
) -> Result<(), StorageError> {
    if !conn.query_row(
        "SELECT setup_confirmed FROM settings WHERE id=1",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    let due = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间有效")
        .as_millis() as i64
        + DEBOUNCE_MS;
    // 保留第一步之前的状态，后续保存仅顺延；投递时计算净变化。
    conn.execute(
        "INSERT INTO pending_configuration_change VALUES(1,?1,?2,?3)
        ON CONFLICT(singleton) DO UPDATE SET due_at_ms=excluded.due_at_ms",
        rusqlite::params![
            serde_json::to_string(previous).map_err(StorageError::ConfigJson)?,
            serde_json::to_string(products).map_err(StorageError::ConfigJson)?,
            due
        ],
    )?;
    Ok(())
}

pub(super) fn flush_due(conn: &Connection, now_ms: i64) -> Result<(), StorageError> {
    let pending: Option<(String, String, i64)> = conn.query_row(
        "SELECT previous_config_json,previous_products_json,due_at_ms FROM pending_configuration_change WHERE singleton=1 AND due_at_ms<=?1",
        [now_ms], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
    let Some((config, products, due)) = pending else {
        return Ok(());
    };
    let previous: MonitorConfig =
        serde_json::from_str(&config).map_err(StorageError::ConfigJson)?;
    let previous_products: Vec<(String, String)> =
        serde_json::from_str(&products).map_err(StorageError::ConfigJson)?;
    let current_products = monitored_products(conn)?;
    let mut changes = Vec::new();
    if let Some(text) = product_change(&previous_products, &current_products) {
        changes.push(text);
    }
    changes.extend(config_changes(&previous, &monitor_config(conn)?));
    conn.execute("DELETE FROM pending_configuration_change", [])?;
    if !changes.is_empty() {
        changes.push(format!("当前监控范围：{}", range(&current_products)));
        enqueue_change(conn, changes.join("\n"), due - DEBOUNCE_MS)?;
    }
    Ok(())
}

fn enqueue_change(conn: &Connection, text: String, at: i64) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE event_sequence SET value=value+1 WHERE singleton=1",
        [],
    )?;
    let id = conn.query_row(
        "SELECT value FROM event_sequence WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    // 配置事件属于应用级通知，合并正文在入队事务中固定。
    let event = ListingEvent {
        id,
        product: ProductIdentity {
            key: "__configuration__".into(),
            product_id: String::new(),
            sku_id: None,
        },
        kind: MonitorEventKind::ConfigurationChanged,
        stock: 0.0,
        request_sequence: 0,
        observed_at_ms: at,
        notification_details: EventNotificationDetails {
            configuration_change: Some(text),
            ..Default::default()
        },
    };
    super::notification_outbox::enqueue_event(conn, &event)
}

#[cfg(test)]
mod tests {
    use super::super::{ProductIdentity, Storage};

    fn fixture() -> Storage {
        let mut db = Storage::open_in_memory().unwrap();
        for (id, name) in [("65", "官翻品 GR IIIx"), ("130", "官翻品 GR IV")] {
            db.save_product_config(
                ProductIdentity {
                    key: id.into(),
                    product_id: id.into(),
                    sku_id: None,
                },
                name.into(),
                "manual".into(),
                Some(1),
            )
            .unwrap();
        }
        db.set_product_enabled("65", true).unwrap();
        db.complete_setup().unwrap();
        db.set_system_notifications_enabled(true).unwrap();
        db
    }

    fn at() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 5_100
    }

    #[test]
    fn rapid_changes_wait_and_report_only_the_final_range() {
        let mut db = fixture();
        let start = at() - 5_100;
        db.set_product_enabled("130", true).unwrap();
        db.set_product_enabled("65", false).unwrap();
        assert!(db
            .claim_system_notification(start + 4_000)
            .unwrap()
            .is_none());
        let job = db
            .claim_system_notification(start + 5_100)
            .unwrap()
            .unwrap();
        assert_eq!(job.event.notification_details.configuration_change.as_deref(), Some("监控产品变更：新增【官翻品 GR IV】；减少【官翻品 GR IIIx】\n当前监控范围：官翻品 GR IV"));
        assert!(db
            .claim_system_notification(start + 6_000)
            .unwrap()
            .is_none());
    }

    #[test]
    fn stock_notifications_bypass_the_configuration_debounce() {
        let mut db = fixture();
        db.set_product_enabled("130", true).unwrap();
        let now = at() - 5_100;
        let event = super::ListingEvent {
            id: 100,
            product: ProductIdentity {
                key: "65".into(),
                product_id: "65".into(),
                sku_id: None,
            },
            kind: super::MonitorEventKind::StockIncreased,
            stock: 5.0,
            request_sequence: 1,
            observed_at_ms: now,
            notification_details: Default::default(),
        };
        super::super::notification_outbox::enqueue_event(&db.conn, &event).unwrap();
        let job = db.claim_system_notification(now).unwrap().unwrap();
        assert_eq!(job.event.kind, super::MonitorEventKind::StockIncreased);
        assert!(db.claim_system_notification(now).unwrap().is_none());
        assert!(db.has_pending_configuration_notifications().unwrap());
        assert!(db.claim_system_notification(at()).unwrap().is_some());
    }

    #[test]
    fn reverting_all_changes_cancels_the_batch_and_unchanged_save_keeps_deadline() {
        let mut db = fixture();
        let original = db.monitor_config().unwrap();
        let mut changed = original.clone();
        changed.schedule.start_minute = 600;
        db.save_monitor_config(&changed).unwrap();
        let due = db
            .conn
            .query_row(
                "SELECT due_at_ms FROM pending_configuration_change",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        db.save_monitor_config(&changed).unwrap();
        assert_eq!(
            db.conn
                .query_row(
                    "SELECT due_at_ms FROM pending_configuration_change",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            due
        );
        db.set_product_enabled("130", true).unwrap();
        db.set_product_enabled("130", false).unwrap();
        db.save_monitor_config(&original).unwrap();
        assert!(db.claim_system_notification(at()).unwrap().is_none());
        assert!(!db.has_pending_configuration_notifications().unwrap());
    }

    #[test]
    fn later_edit_extends_deadline_and_mixes_product_and_setting_changes() {
        let mut db = fixture();
        db.set_product_enabled("130", true).unwrap();
        let old_due = db
            .conn
            .query_row(
                "SELECT due_at_ms FROM pending_configuration_change",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        db.conn
            .execute(
                "UPDATE pending_configuration_change SET due_at_ms=due_at_ms-2000",
                [],
            )
            .unwrap();
        let mut config = db.monitor_config().unwrap();
        config.requests.interval = std::time::Duration::ZERO;
        db.save_monitor_config(&config).unwrap();
        assert!(db.claim_system_notification(old_due - 1).unwrap().is_none());
        let job = db.claim_system_notification(at()).unwrap().unwrap();
        let text = job.event.notification_details.configuration_change.unwrap();
        assert!(text.contains("新增【官翻品 GR IV】"));
        assert!(text.contains("检查间隔变更：1–2 秒 → 0–0 秒"));
        assert_eq!(text.matches("当前监控范围：").count(), 1);
        assert!(db.claim_system_notification(at()).unwrap().is_none());
    }

    #[test]
    fn debounced_changes_survive_reopening_and_reset_clears_them() {
        let file = super::super::tests::TempDatabase::new();
        let mut db = Storage::open(&file.0).unwrap();
        db.set_onboarding_products(&[65]).unwrap();
        db.complete_setup().unwrap();
        db.set_system_notifications_enabled(true).unwrap();
        db.set_product_enabled("65", false).unwrap();
        drop(db);
        let mut db = Storage::open(&file.0).unwrap();
        assert!(db
            .claim_system_notification(at() - 5_100)
            .unwrap()
            .is_none());
        let job = db.claim_system_notification(at()).unwrap().unwrap();
        assert!(job
            .event
            .notification_details
            .configuration_change
            .unwrap()
            .contains("当前监控范围：无"));
        let mut config = db.monitor_config().unwrap();
        config.requests.interval = std::time::Duration::ZERO;
        db.save_monitor_config(&config).unwrap();
        db.restore_defaults(&crate::config::MonitorConfig::default(), false)
            .unwrap();
        assert!(!db.has_pending_configuration_notifications().unwrap());
    }

    #[test]
    fn upgrade_intro_is_only_for_existing_databases_with_channels_and_choice_is_durable() {
        for enable in [true, false] {
            let file = super::super::tests::TempDatabase::new();
            let mut db = Storage::open(&file.0).unwrap();
            for id in ["on", "off"] {
                db.save_notification_channel(
                    id,
                    id,
                    "feishu",
                    Some(id),
                    &["stock_available".into()],
                )
                .unwrap();
                db.mark_notification_channel_tested(id).unwrap();
            }
            db.set_notification_channel_enabled("on", true).unwrap();
            assert!(!db.configuration_change_intro_pending().unwrap());
            db.conn
                .execute(
                    "ALTER TABLE settings DROP COLUMN configuration_change_intro_pending",
                    [],
                )
                .unwrap();
            drop(db);
            let mut db = Storage::open(&file.0).unwrap();
            assert!(db.configuration_change_intro_pending().unwrap());
            let original = db.notification_channels().unwrap();
            db.complete_configuration_change_intro(enable).unwrap();
            assert_eq!(
                db.system_configuration_change_notifications_enabled()
                    .unwrap(),
                enable
            );
            assert!(!db.configuration_change_intro_pending().unwrap());
            for (old, new) in original.iter().zip(db.notification_channels().unwrap()) {
                assert_eq!(old.enabled, new.enabled);
                assert_eq!(old.tested, new.tested);
                assert_eq!(old.credential_ref, new.credential_ref);
                assert_eq!(
                    new.subscriptions.contains(&"configuration_changed".into()),
                    enable
                );
                assert!(new.subscriptions.contains(&"stock_available".into()));
            }
            // 已处理的弹窗请求不重新覆盖用户后来逐渠道设置的选择。
            db.complete_configuration_change_intro(!enable).unwrap();
            drop(db);
            let db = Storage::open(&file.0).unwrap();
            assert!(!db.configuration_change_intro_pending().unwrap());
            assert!(db
                .notification_channels()
                .unwrap()
                .iter()
                .all(|channel| channel
                    .subscriptions
                    .contains(&"configuration_changed".into())
                    == enable));
        }
        let file = super::super::tests::TempDatabase::new();
        let db = Storage::open(&file.0).unwrap();
        db.conn
            .execute(
                "ALTER TABLE settings DROP COLUMN configuration_change_intro_pending",
                [],
            )
            .unwrap();
        drop(db);
        let mut db = Storage::open(&file.0).unwrap();
        assert!(!db.configuration_change_intro_pending().unwrap());
        db.save_notification_channel("later", "later", "feishu", Some("later"), &[])
            .unwrap();
        assert!(!db.configuration_change_intro_pending().unwrap());
    }

    #[test]
    fn failed_upgrade_choice_rolls_back_all_channels_and_keeps_intro_pending() {
        let mut db = fixture();
        for id in ["a", "b"] {
            db.save_notification_channel(id, id, "feishu", Some(id), &["stock_available".into()])
                .unwrap();
        }
        db.conn
            .execute(
                "UPDATE settings SET configuration_change_intro_pending=1",
                [],
            )
            .unwrap();
        db.conn.execute_batch("CREATE TRIGGER reject_intro BEFORE UPDATE OF subscriptions ON notification_channels WHEN OLD.id='b' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(db.complete_configuration_change_intro(true).is_err());
        assert!(db.configuration_change_intro_pending().unwrap());
        assert!(db
            .notification_channels()
            .unwrap()
            .iter()
            .all(|channel| channel.subscriptions == ["stock_available"]));
        db.conn.execute_batch("DROP TRIGGER reject_intro").unwrap();
        db.complete_configuration_change_intro(true).unwrap();
        assert!(!db.configuration_change_intro_pending().unwrap());
    }

    #[test]
    fn system_config_scope_can_cancel_pending_changes_without_cancelling_stock_scope() {
        let mut db = fixture();
        assert!(db
            .system_configuration_change_notifications_enabled()
            .unwrap());
        db.set_product_enabled("130", true).unwrap();
        db.set_system_configuration_change_notifications_enabled(false)
            .unwrap();
        assert!(db.claim_system_notification(at()).unwrap().is_none());
        db.remove_product("130").unwrap();
        assert!(db.claim_system_notification(at()).unwrap().is_none());
        assert!(db.system_notifications_enabled().unwrap());
        db.set_system_configuration_change_notifications_enabled(true)
            .unwrap();
        db.set_product_enabled("65", false).unwrap();
        assert!(db.claim_system_notification(at()).unwrap().is_some());
    }

    #[test]
    fn configuration_changes_queue_once_after_success_and_survive_pause() {
        let mut db = fixture();
        let original = db.monitor_config().unwrap();
        db.save_monitor_config(&original).unwrap();
        assert!(db.claim_system_notification(at()).unwrap().is_none());
        let mut changed = original.clone();
        changed.schedule.start_minute = 600;
        changed.schedule.end_minute = 660;
        changed.requests.interval = std::time::Duration::ZERO;
        db.save_monitor_config(&changed).unwrap();
        db.set_run_intent(super::super::RunIntent::Paused, true)
            .unwrap();
        let job = db.claim_system_notification(at()).unwrap().unwrap();
        let text = job.event.notification_details.configuration_change.unwrap();
        assert_eq!(text, "监控计划变更：每天 09:00–19:00 → 每天 10:00–11:00\n检查间隔变更：1–2 秒 → 0–0 秒\n当前监控范围：官翻品 GR IIIx");
        db.save_monitor_config(&changed).unwrap();
        assert!(db.claim_system_notification(at()).unwrap().is_none());
        changed.schedule.enabled_days = [false; 7];
        assert!(db.save_monitor_config(&changed).is_err());
        assert!(db.claim_system_notification(at()).unwrap().is_none());
    }

    #[test]
    fn range_changes_include_disabling_and_deleting_last_product() {
        let mut db = fixture();
        db.set_product_enabled("130", true).unwrap();
        let job = db.claim_system_notification(at()).unwrap().unwrap();
        assert_eq!(
            job.event
                .notification_details
                .configuration_change
                .as_deref(),
            Some("监控产品变更：新增【官翻品 GR IV】\n当前监控范围：官翻品 GR IV、官翻品 GR IIIx")
        );
        db.set_product_enabled("130", true).unwrap();
        assert!(db.claim_system_notification(at()).unwrap().is_none());
        db.set_product_enabled("65", false).unwrap();
        let job = db.claim_system_notification(at()).unwrap().unwrap();
        assert_eq!(
            job.event
                .notification_details
                .configuration_change
                .as_deref(),
            Some("监控产品变更：减少【官翻品 GR IIIx】\n当前监控范围：官翻品 GR IV")
        );
        db.remove_product("130").unwrap();
        let job = db.claim_system_notification(at()).unwrap().unwrap();
        assert_eq!(
            job.event
                .notification_details
                .configuration_change
                .as_deref(),
            Some("监控产品变更：减少【官翻品 GR IV】\n当前监控范围：无")
        );
        db.remove_product("130").unwrap();
        assert!(db.claim_system_notification(at()).unwrap().is_none());
    }

    #[test]
    fn config_subscription_routes_are_independent_and_remain_deliverable_without_products() {
        let mut db = fixture();
        for (id, scopes) in [
            ("on", vec!["configuration_changed".into()]),
            ("off", vec!["stock_available".into()]),
        ] {
            db.save_notification_channel(id, id, "feishu", Some(id), &scopes)
                .unwrap();
            db.mark_notification_channel_tested(id).unwrap();
            db.set_notification_channel_enabled(id, true).unwrap();
        }
        let targets = ["private", "group"].map(|id| crate::notifications::NotificationTarget {
            id: id.into(),
            kind: if id == "private" { "user" } else { "chat" }.into(),
            label: id.into(),
        });
        db.save_notification_routes("on", &targets).unwrap();
        db.remove_product("65").unwrap();
        let job = db
            .claim_ready_channel_notification(at(), &["on".into(), "off".into()])
            .unwrap()
            .unwrap();
        assert_eq!(job.channel_id, "on");
        assert!(db
            .notification_event_is_enabled(
                "on",
                "configuration_changed",
                &job.event.product.key,
                Some(job.generation)
            )
            .unwrap());
        assert!(db
            .waiting_notification_is_current(&job.event, at())
            .unwrap());
        let text = job.event.notification_details.configuration_change.clone();
        db.finish_notification(job.id, "accepted", "平台已接受", at())
            .unwrap();
        let next = db
            .claim_ready_channel_notification(at(), &["on".into(), "off".into()])
            .unwrap()
            .unwrap();
        assert_eq!(next.event.notification_details.configuration_change, text);
        assert_ne!(next.target, job.target);
        db.finish_notification(next.id, "accepted", "平台已接受", at())
            .unwrap();
        assert!(db.claim_channel_notification(at()).unwrap().is_none());
    }

    #[test]
    fn setup_and_metadata_do_not_send_config_changes_and_cross_midnight_copy_is_explicit() {
        let mut db = Storage::open_in_memory().unwrap();
        let mut config = db.monitor_config().unwrap();
        config.schedule.start_minute = 22 * 60;
        config.schedule.end_minute = 120;
        db.save_monitor_config(&config).unwrap();
        assert!(db.claim_system_notification(at()).unwrap().is_none());
        assert_eq!(super::schedule(&config.schedule), "每天 22:00–次日 02:00");
        config.schedule.end_minute = config.schedule.start_minute;
        assert_eq!(super::schedule(&config.schedule), "每天 全天");
    }
}
