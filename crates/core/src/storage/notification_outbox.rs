use super::{
    event_kind_from_db, event_kind_to_db, ListingEvent, ProductIdentity, Storage, StorageError,
};
use rusqlite::{params, Connection, OptionalExtension};

const PENDING_PER_CHANNEL: i64 = 256;
const TOTAL_PENDING: i64 = 8192;
const MAX_AGE_MS: i64 = 24 * 60 * 60 * 1000;
const PROMINENT_MAX_AGE_MS: i64 = 5 * 60 * 1000;
const PROMINENT_CAPACITY_ERROR: &str = "待发送通知已达容量上限（每渠道 256 条，全局 8192 条）";
const RESULT_AGE_MS: i64 = 7 * MAX_AGE_MS;
// 一次发送最多三次 20 秒请求及 1、2 秒重试等待，领取租期覆盖完整发送。
const LEASE_MS: i64 = 90 * 1000;
pub(crate) const SYSTEM_CHANNEL: &str = "__system__";
pub(crate) const PROMINENT_CHANNEL: &str = "__prominent__";

#[derive(Clone, Debug)]
pub struct OutboxJob {
    pub id: i64,
    pub channel_id: String,
    pub generation: u64,
    pub event: ListingEvent,
    pub target: Option<crate::notifications::NotificationTarget>,
    pub waited_for_connection: bool,
}

pub(super) fn initialize(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS notification_outbox (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id INTEGER NOT NULL,
            channel_id TEXT NOT NULL,
            product_key TEXT NOT NULL,
            product_id TEXT NOT NULL,
            sku_id TEXT,
            event_kind TEXT NOT NULL,
            stock REAL NOT NULL,
            request_sequence INTEGER NOT NULL,
            product_generation INTEGER NOT NULL,
            observed_at_ms INTEGER NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            claimed_at_ms INTEGER,
            finished_at_ms INTEGER,
            error TEXT,
            UNIQUE(event_id, channel_id)
        );
        CREATE INDEX IF NOT EXISTS notification_outbox_pending
            ON notification_outbox(channel_id,status,id);",
    )?;
    if !super::column_exists(conn, "notification_outbox", "notification_details_json")? {
        conn.execute(
            "ALTER TABLE notification_outbox ADD COLUMN notification_details_json TEXT",
            [],
        )?;
    }
    Ok(())
}

fn subscription(event: &ListingEvent) -> &'static str {
    match event.kind {
        super::MonitorEventKind::FirstObservedInStock
        | super::MonitorEventKind::OutOfStockToInStock
        | super::MonitorEventKind::StockIncreased => "stock_available",
        super::MonitorEventKind::MonitoringFailed => "monitoring_failed",
        super::MonitorEventKind::Recovered => "recovered",
    }
}

fn subscription_from_db(kind: &str) -> &'static str {
    match kind {
        "first_observed_in_stock" | "out_of_stock_to_in_stock" => "stock_available",
        "monitoring_failed" => "monitoring_failed",
        "recovered" => "recovered",
        _ => "stock_available",
    }
}

fn delivery_account_id(conn: &Connection, key: &str) -> Result<String, StorageError> {
    if let Some(route_id) = key
        .strip_prefix("route:")
        .and_then(|id| id.parse::<i64>().ok())
    {
        Ok(conn
            .query_row(
                "SELECT account_id FROM notification_routes WHERE id=?1",
                [route_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_else(|| key.to_owned()))
    } else {
        Ok(key.to_owned())
    }
}

fn record_route_delivery(
    conn: &Connection,
    key: &str,
    outcome: &str,
    event: &str,
    at_ms: i64,
    message: &str,
) -> Result<(), StorageError> {
    if let Some(route_id) = key
        .strip_prefix("route:")
        .and_then(|id| id.parse::<i64>().ok())
    {
        conn.execute("UPDATE notification_routes SET outcome=?2,event_kind=?3,at_ms=?4,message=?5 WHERE id=?1",params![route_id,outcome,event,at_ms,message])?;
    }
    Ok(())
}

pub(super) fn enqueue_event(conn: &Connection, event: &ListingEvent) -> Result<(), StorageError> {
    let now_ms = event.observed_at_ms;
    let details_json =
        serde_json::to_string(&event.notification_details).map_err(StorageError::ConfigJson)?;
    prune_results(conn, now_ms)?;
    prune_prominent_alerts(conn, now_ms)?;
    let enabled_accounts = conn
        .prepare("SELECT id FROM notification_channels WHERE enabled=1 AND tested=1")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut channels = Vec::new();
    for account in enabled_accounts {
        let routes = conn
            .prepare("SELECT id FROM notification_routes WHERE account_id=?1 ORDER BY id")?
            .query_map([&account], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if routes.is_empty() {
            channels.push((account.clone(), account));
        } else {
            channels.extend(
                routes
                    .into_iter()
                    .map(|route| (account.clone(), format!("route:{route}"))),
            );
        }
    }
    if conn.query_row(
        "SELECT system_notifications_enabled FROM settings WHERE id=1",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        channels.push((SYSTEM_CHANNEL.into(), SYSTEM_CHANNEL.into()));
    }
    if subscription(event) == "stock_available" && conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM products p JOIN product_alert_settings a USING(product_key)
         JOIN observations o USING(product_key) WHERE p.product_key=?1 AND p.is_configured=1
         AND p.enabled=1 AND a.prominent_alert=1 AND o.is_show=1)",
        [&event.product.key], |row| row.get::<_,bool>(0))? {
        channels.push((PROMINENT_CHANNEL.into(),PROMINENT_CHANNEL.into()));
    }
    for (account_id, channel_id) in channels {
        if channel_id != SYSTEM_CHANNEL && channel_id != PROMINENT_CHANNEL {
            let enabled = super::notification_storage::channel_subscribes(
                conn,
                &account_id,
                subscription(event),
            )?;
            if !enabled {
                continue;
            }
        }
        if channel_id == PROMINENT_CHANNEL {
            let generation: i64 = conn.query_row(
                "SELECT COALESCE((SELECT generation FROM product_rounds WHERE product_key=?1),0)",
                [&event.product.key],
                |row| row.get(0),
            )?;
            let updated = conn.execute(
                "UPDATE notification_outbox SET event_id=?1,product_id=?2,sku_id=?3,
                    event_kind=?4,stock=?5,request_sequence=?6,product_generation=?7,
                    observed_at_ms=?8,notification_details_json=?11
                 WHERE id=(SELECT id FROM notification_outbox WHERE channel_id=?9
                    AND product_key=?10 AND product_generation=?7 AND status='pending'
                    ORDER BY id LIMIT 1)",
                params![
                    event.id,
                    event.product.product_id,
                    event.product.sku_id,
                    event_kind_to_db(event.kind),
                    event.stock,
                    event.request_sequence as i64,
                    generation,
                    event.observed_at_ms,
                    channel_id,
                    event.product.key,
                    details_json,
                ],
            )?;
            if updated > 0 {
                continue;
            }
        }
        let pending: i64 = conn.query_row(
            "SELECT COUNT(*) FROM notification_outbox WHERE channel_id=?1 AND status IN ('pending','inflight')",
            [&channel_id], |row| row.get(0))?;
        let total_pending: i64 = conn.query_row(
            "SELECT COUNT(*) FROM notification_outbox WHERE status IN ('pending','inflight')",
            [],
            |row| row.get(0),
        )?;
        let (status, error) = if pending >= PENDING_PER_CHANNEL || total_pending >= TOTAL_PENDING {
            ("failed", Some(PROMINENT_CAPACITY_ERROR))
        } else {
            ("pending", None)
        };
        conn.execute("INSERT OR IGNORE INTO notification_outbox(
            event_id,channel_id,product_key,product_id,sku_id,event_kind,stock,request_sequence,
            product_generation,observed_at_ms,status,finished_at_ms,error,notification_details_json)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,
                COALESCE((SELECT generation FROM product_rounds WHERE product_key=?3),0),?9,?10,?11,?12,?13)",
            params![event.id, channel_id, event.product.key, event.product.product_id,
                event.product.sku_id, event_kind_to_db(event.kind), event.stock,
                event.request_sequence as i64, event.observed_at_ms, status,
                if error.is_some() { Some(now_ms) } else { None }, error, details_json])?;
        if let Some(error) = error {
            if channel_id != SYSTEM_CHANNEL && channel_id != PROMINENT_CHANNEL {
                conn.execute("INSERT INTO notification_deliveries(channel_id,outcome,event_kind,at_ms,message)
                    VALUES(?1,'failed',?2,?3,?4) ON CONFLICT(channel_id) DO UPDATE SET
                    outcome='failed',event_kind=excluded.event_kind,at_ms=excluded.at_ms,message=excluded.message",
                    params![account_id, subscription(event), now_ms, error])?;
            }
        }
    }
    Ok(())
}

fn prune_results(conn: &Connection, now_ms: i64) -> Result<(), StorageError> {
    conn.execute("DELETE FROM notification_outbox WHERE status NOT IN ('pending','inflight') AND finished_at_ms < ?1", [now_ms.saturating_sub(RESULT_AGE_MS)])?;
    conn.execute(
        "DELETE FROM notification_outbox WHERE id IN (
        SELECT id FROM notification_outbox WHERE status NOT IN ('pending','inflight')
        ORDER BY id DESC LIMIT -1 OFFSET 1024)",
        [],
    )?;
    Ok(())
}

fn prune_prominent_alerts(conn: &Connection, now_ms: i64) -> Result<(), StorageError> {
    conn.execute(
        "DELETE FROM notification_outbox WHERE channel_id='__prominent__' AND (
        (status!='pending' AND NOT (status='failed' AND error=?2))
        OR observed_at_ms < ?1 OR NOT EXISTS (
            SELECT 1 FROM products p JOIN product_alert_settings a USING(product_key)
            JOIN observations o USING(product_key) LEFT JOIN product_rounds r USING(product_key)
            WHERE p.product_key=notification_outbox.product_key
            AND p.is_configured=1 AND p.enabled=1 AND a.prominent_alert=1 AND o.is_show=1
            AND (notification_outbox.stock=0 OR o.stock>0)
            AND COALESCE(r.generation,0)=notification_outbox.product_generation))",
        params![
            now_ms.saturating_sub(PROMINENT_MAX_AGE_MS),
            PROMINENT_CAPACITY_ERROR
        ],
    )?;
    Ok(())
}

fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxJob> {
    let kind: String = row.get(6)?;
    Ok(OutboxJob {
        id: row.get(0)?,
        channel_id: row.get(2)?,
        target: None,
        waited_for_connection: false,
        generation: row.get::<_, i64>(10)? as u64,
        event: ListingEvent {
            id: row.get(1)?,
            product: ProductIdentity {
                key: row.get(3)?,
                product_id: row.get(4)?,
                sku_id: row.get(5)?,
            },
            kind: event_kind_from_db(&kind)?,
            stock: row.get(7)?,
            request_sequence: row.get::<_, i64>(8)? as u64,
            observed_at_ms: row.get(9)?,
            notification_details: super::notification_details_from_json(row.get(11)?)?,
        },
    })
}

impl Storage {
    /// 运行锁的新拥有者将上次未确认的投递结为未知，释放路由顺序占位。
    pub(crate) fn recover_channel_notifications(
        &mut self,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let ids = self
            .conn
            .prepare(
                "SELECT id FROM notification_outbox WHERE status='inflight'
            AND channel_id NOT IN ('__system__','__prominent__')",
            )?
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for id in ids {
            self.finish_notification(
                id,
                "unknown",
                "上次发送在确认结果前中断，接收结果未知",
                now_ms,
            )?;
        }
        Ok(())
    }

    pub(crate) fn has_prominent_alert_for_event(
        &self,
        event_id: i64,
    ) -> Result<bool, StorageError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM notification_outbox WHERE event_id=?1 AND channel_id=?2)",
            params![event_id, PROMINENT_CHANNEL],
            |row| row.get(0),
        )?)
    }

    pub fn claim_prominent_alert(
        &mut self,
        now_ms: i64,
    ) -> Result<Option<OutboxJob>, StorageError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        prune_prominent_alerts(&tx, now_ms)?;
        let overflow_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM notification_outbox
             WHERE channel_id='__prominent__' AND status='failed' AND error=?1",
            [PROMINENT_CAPACITY_ERROR],
            |row| row.get(0),
        )?;
        if overflow_count > 0 {
            tx.execute(
                "DELETE FROM notification_outbox
                 WHERE channel_id='__prominent__' AND status='failed' AND error=?1",
                [PROMINENT_CAPACITY_ERROR],
            )?;
            tx.commit()?;
            return Err(StorageError::ProminentQueueOverflow);
        }
        let job = tx.query_row("SELECT id,event_id,channel_id,product_key,product_id,sku_id,
            event_kind,stock,request_sequence,observed_at_ms,product_generation,notification_details_json FROM notification_outbox
            WHERE channel_id='__prominent__' AND status='pending' ORDER BY id LIMIT 1", [], job_from_row).optional()?;
        tx.commit()?;
        Ok(job)
    }

    pub fn acknowledge_prominent_alert(&mut self, event_id: i64) -> Result<(), StorageError> {
        self.conn.execute(
            "DELETE FROM notification_outbox WHERE event_id=?1 AND channel_id='__prominent__'",
            [event_id],
        )?;
        Ok(())
    }
    pub fn claim_channel_notification(
        &mut self,
        now_ms: i64,
    ) -> Result<Option<OutboxJob>, StorageError> {
        self.claim_channel_notification_inner(now_ms, None)
    }

    pub fn claim_ready_channel_notification(
        &mut self,
        now_ms: i64,
        ready_accounts: &[String],
    ) -> Result<Option<OutboxJob>, StorageError> {
        self.claim_channel_notification_inner(now_ms, Some(ready_accounts))
    }

    fn claim_channel_notification_inner(
        &mut self,
        now_ms: i64,
        ready_accounts: Option<&[String]>,
    ) -> Result<Option<OutboxJob>, StorageError> {
        loop {
            let job = self.claim_notification(now_ms, false, ready_accounts)?;
            let Some(mut job) = job else { return Ok(None) };
            if self.product_generation(&job.event.product.key)? != job.generation {
                self.finish_notification(job.id, "skipped", "", now_ms)?;
                continue;
            }
            if let Some(route_id) = job
                .channel_id
                .strip_prefix("route:")
                .and_then(|id| id.parse::<i64>().ok())
            {
                let route = self.conn.query_row("SELECT account_id,target_id,target_kind,label FROM notification_routes WHERE id=?1",[route_id],|row| Ok((row.get::<_,String>(0)?,crate::notifications::NotificationTarget{id:row.get(1)?,kind:row.get(2)?,label:row.get(3)?}))).optional()?;
                if let Some((account, target)) = route {
                    job.channel_id = account;
                    job.target = Some(target);
                } else {
                    self.finish_notification(job.id, "skipped", "接收群组已取消", now_ms)?;
                    continue;
                }
            }
            job.waited_for_connection = self.conn.query_row(
                "SELECT error='等待连接恢复' FROM notification_outbox WHERE id=?1",
                [job.id], |row| Ok(row.get::<_, Option<bool>>(0)?.unwrap_or(false)),
            )?;
            if ready_accounts.is_some() && (!self.notification_event_is_enabled(&job.channel_id, subscription(&job.event), &job.event.product.key, Some(job.generation))?
                || (job.waited_for_connection && !self.waiting_notification_is_current(&job.event, now_ms)?))
            {
                self.finish_notification(job.id, "skipped", "事件已过期或通知范围已取消", now_ms)?;
                continue;
            }
            return Ok(Some(job));
        }
    }

    pub fn waiting_notification_is_current(&self, event: &ListingEvent, now_ms: i64) -> Result<bool, StorageError> {
        if subscription(event) != "stock_available" { return Ok(true); }
        if event.observed_at_ms < now_ms.saturating_sub(PROMINENT_MAX_AGE_MS) { return Ok(false); }
        Ok(self.observation(&event.product.key)?.is_some_and(|latest|
            latest.state.is_show == 1 && (event.stock == 0.0 || latest.state.stock.is_some_and(|stock| stock > 0.0))))
    }

    pub fn claim_system_notification(
        &mut self,
        now_ms: i64,
    ) -> Result<Option<OutboxJob>, StorageError> {
        loop {
            let job = self.claim_notification(now_ms, true, None)?;
            let Some(job) = job else { return Ok(None) };
            let product_enabled = self
                .product_configs()?
                .into_iter()
                .any(|product| product.identity.key == job.event.product.key && product.enabled);
            if !self.system_notifications_enabled()?
                || !product_enabled
                || self.product_generation(&job.event.product.key)? != job.generation
            {
                self.finish_notification(job.id, "skipped", "", now_ms)?;
                continue;
            }
            return Ok(Some(job));
        }
    }

    fn claim_notification(
        &mut self,
        now_ms: i64,
        system: bool,
        ready_accounts: Option<&[String]>,
    ) -> Result<Option<OutboxJob>, StorageError> {
        let tx = self.conn.transaction()?;
        let ready_json = ready_accounts.map(|accounts| serde_json::to_string(accounts).expect("字符串数组可序列化"));
        if let Some(ref ready) = ready_json {
            tx.execute("UPDATE notification_outbox SET error='等待连接恢复'
                WHERE status='pending' AND channel_id NOT IN ('__system__','__prominent__')
                AND COALESCE((SELECT account_id FROM notification_routes WHERE 'route:'||id=channel_id),channel_id)
                    NOT IN (SELECT value FROM json_each(?1))", [ready])?;
        }
        prune_results(&tx, now_ms)?;
        let interrupted = tx
            .prepare(
                "SELECT id,channel_id,event_kind FROM notification_outbox
            WHERE status='inflight' AND claimed_at_ms <= ?1 AND (channel_id='__system__')=?2 AND channel_id!='__prominent__'",
            )?
            .query_map(params![now_ms.saturating_sub(LEASE_MS), system], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        tx.execute(
            "UPDATE notification_outbox SET status='unknown',finished_at_ms=?1,
            error='上次发送在确认结果前中断，接收结果未知'
            WHERE status='inflight' AND claimed_at_ms <= ?2 AND (channel_id='__system__')=?3 AND channel_id!='__prominent__'",
            params![now_ms, now_ms.saturating_sub(LEASE_MS), system],
        )?;
        for (_, channel_id, kind) in interrupted {
            if channel_id != SYSTEM_CHANNEL {
                record_route_delivery(
                    &tx,
                    &channel_id,
                    "unknown",
                    subscription_from_db(&kind),
                    now_ms,
                    "上次发送在确认结果前中断，接收结果未知",
                )?;
                let channel_id = delivery_account_id(&tx, &channel_id)?;
                tx.execute("INSERT INTO notification_deliveries(channel_id,outcome,event_kind,at_ms,message)
                    SELECT id,'unknown',?2,?3,'上次发送在确认结果前中断，接收结果未知'
                    FROM notification_channels WHERE id=?1
                    ON CONFLICT(channel_id) DO UPDATE SET outcome='unknown',event_kind=excluded.event_kind,
                    at_ms=excluded.at_ms,message=excluded.message",
                    params![channel_id,subscription_from_db(&kind),now_ms])?;
            }
        }
        let expired = tx
            .prepare(
                "SELECT id,channel_id,event_kind FROM notification_outbox
            WHERE status='pending' AND observed_at_ms < ?1",
            )?
            .query_map([now_ms.saturating_sub(MAX_AGE_MS)], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (id, channel_id, kind) in expired {
            tx.execute("UPDATE notification_outbox SET status='failed',finished_at_ms=?2,error='通知超过 24 小时发送期限' WHERE id=?1",
                params![id,now_ms])?;
            if channel_id != SYSTEM_CHANNEL {
                record_route_delivery(
                    &tx,
                    &channel_id,
                    "failed",
                    subscription_from_db(&kind),
                    now_ms,
                    "通知超过 24 小时发送期限",
                )?;
                let channel_id = delivery_account_id(&tx, &channel_id)?;
                tx.execute("INSERT INTO notification_deliveries(channel_id,outcome,event_kind,at_ms,message)
                    SELECT id,'failed',?2,?3,'通知超过 24 小时发送期限'
                    FROM notification_channels WHERE id=?1
                    ON CONFLICT(channel_id) DO UPDATE SET outcome='failed',event_kind=excluded.event_kind,
                    at_ms=excluded.at_ms,message=excluded.message",
                    params![channel_id,subscription_from_db(&kind),now_ms])?;
            }
        }
        let job = tx.query_row("SELECT id,event_id,channel_id,product_key,product_id,sku_id,
            event_kind,stock,request_sequence,observed_at_ms,product_generation,notification_details_json FROM notification_outbox
            WHERE status='pending'
            AND (channel_id='__system__')=?2 AND channel_id!='__prominent__'
            AND (?3 IS NULL OR COALESCE((SELECT account_id FROM notification_routes WHERE 'route:'||id=channel_id),channel_id)
                IN (SELECT value FROM json_each(?3)) OR NOT EXISTS (
                    SELECT 1 FROM notification_channels c WHERE c.enabled=1 AND c.tested=1
                    AND c.id=COALESCE((SELECT account_id FROM notification_routes WHERE 'route:'||id=channel_id),channel_id)))
            AND (?2 OR NOT EXISTS(SELECT 1 FROM notification_outbox active
                WHERE active.channel_id=notification_outbox.channel_id AND active.status='inflight'))
            ORDER BY id LIMIT 1",
            params![now_ms.saturating_sub(LEASE_MS),system,ready_json], job_from_row).optional()?;
        if let Some(ref job) = job {
            tx.execute(
                "UPDATE notification_outbox SET status='inflight',claimed_at_ms=?2 WHERE id=?1",
                params![job.id, now_ms],
            )?;
        }
        tx.commit()?;
        Ok(job)
    }

    pub fn finish_notification(
        &mut self,
        id: i64,
        outcome: &str,
        message: &str,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        if outcome == "deferred" {
            self.conn.execute("UPDATE notification_outbox SET status='pending',claimed_at_ms=NULL,
                finished_at_ms=NULL,error='等待连接恢复' WHERE id=?1 AND status='inflight'", [id])?;
            return Ok(());
        }
        let status = match outcome {
            "accepted" => "sent",
            "unknown" => "unknown",
            "skipped" => "skipped",
            _ => "failed",
        };
        let tx = self.conn.transaction()?;
        let row: Option<(String,String)> = tx.query_row(
            "SELECT channel_id,event_kind FROM notification_outbox WHERE id=?1 AND status='inflight'",
            [id], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
        if let Some((channel_id, kind)) = row {
            tx.execute(
                "UPDATE notification_outbox SET status=?2,finished_at_ms=?3,error=?4 WHERE id=?1",
                params![
                    id,
                    status,
                    now_ms,
                    if outcome == "accepted" {
                        None
                    } else {
                        Some(message)
                    }
                ],
            )?;
            if channel_id != SYSTEM_CHANNEL && outcome != "skipped" {
                record_route_delivery(
                    &tx,
                    &channel_id,
                    outcome,
                    subscription_from_db(&kind),
                    now_ms,
                    message,
                )?;
                let channel_id = delivery_account_id(&tx, &channel_id)?;
                tx.execute("INSERT INTO notification_deliveries(channel_id,outcome,event_kind,at_ms,message)
                    SELECT id,?2,?3,?4,?5 FROM notification_channels WHERE id=?1
                    ON CONFLICT(channel_id) DO UPDATE SET
                    outcome=excluded.outcome,event_kind=excluded.event_kind,at_ms=excluded.at_ms,message=excluded.message",
                    params![channel_id,outcome,subscription_from_db(&kind),now_ms,message])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn system_notification_delivery(
        &self,
    ) -> Result<Option<super::ChannelDeliveryRecord>, StorageError> {
        let record = self
            .conn
            .query_row(
                "SELECT status,event_kind,finished_at_ms,error
            FROM notification_outbox WHERE channel_id='__system__'
            AND status IN ('sent','failed','unknown') ORDER BY id DESC LIMIT 1",
                [],
                |row| {
                    let status: String = row.get(0)?;
                    Ok(super::ChannelDeliveryRecord {
                        outcome: if status == "sent" {
                            "accepted".into()
                        } else {
                            status
                        },
                        event_kind: subscription_from_db(&row.get::<_, String>(1)?).into(),
                        at_ms: row.get(2)?,
                        message: row
                            .get::<_, Option<String>>(3)?
                            .unwrap_or_else(|| "系统已接受通知".into()),
                    })
                },
            )
            .optional()?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::{reducer::ObservationReducer, Availability};

    fn waiting_fixture() -> Storage {
        let mut db = Storage::open_in_memory().unwrap();
        db.save_product_config(ProductIdentity { key: "p1".into(), product_id: "1".into(), sku_id: None }, "Fixture".into(), "fixture".into(), Some(1)).unwrap();
        db.set_product_enabled("p1", true).unwrap();
        db.save_notification_channel("a", "a", "feishu", Some("a"), &["stock_available".into()]).unwrap();
        db.mark_notification_channel_tested("a").unwrap();
        db.set_notification_channel_enabled("a", true).unwrap();
        db
    }

    #[test]
    fn waiting_events_expire_and_disabled_accounts_release_pending_jobs() {
        let mut db = waiting_fixture();
        observe(&mut db, "p1", "1", 1, 3.0, true, 100);
        assert!(db.claim_ready_channel_notification(101, &[]).unwrap().is_none());
        assert!(db.claim_ready_channel_notification(100 + PROMINENT_MAX_AGE_MS + 1, &["a".into()]).unwrap().is_none());
        observe(&mut db, "p1", "1", 2, 5.0, true, 400_000);
        assert!(db.claim_ready_channel_notification(400_001, &[]).unwrap().is_none());
        db.set_notification_channel_enabled("a", false).unwrap();
        assert!(db.claim_ready_channel_notification(400_002, &[]).unwrap().is_none());
        let pending: i64 = db.conn.query_row("SELECT count(*) FROM notification_outbox WHERE channel_id='a' AND status='pending'", [], |row|row.get(0)).unwrap();
        assert_eq!(pending, 0);
    }

    #[test]
    fn online_stock_events_are_not_discarded_by_later_sellout() {
        let mut db = waiting_fixture();
        observe(&mut db, "p1", "1", 1, 3.0, true, 100);
        observe(&mut db, "p1", "1", 2, 0.0, true, 101);
        let job = db.claim_ready_channel_notification(102, &["a".into()]).unwrap().unwrap();
        assert!(!job.waited_for_connection);
        assert_eq!(job.event.stock, 3.0);
    }

    #[test]
    fn disconnect_after_claim_requeues_only_confirmed_unsubmitted_jobs() {
        let mut db = waiting_fixture();
        observe(&mut db, "p1", "1", 1, 3.0, true, 100);
        let claimed = db.claim_ready_channel_notification(101, &["a".into()]).unwrap().unwrap();
        db.finish_notification(claimed.id, "deferred", "等待连接恢复", 102).unwrap();
        assert!(db.claim_ready_channel_notification(103, &[]).unwrap().is_none());
        let restored = db.claim_ready_channel_notification(104, &["a".into()]).unwrap().unwrap();
        assert_eq!(restored.id, claimed.id);
        assert!(restored.waited_for_connection);
        db.finish_notification(restored.id, "unknown", "未知结果", 105).unwrap();
        assert!(db.claim_ready_channel_notification(106, &["a".into()]).unwrap().is_none());
    }

    #[test]
    fn waiting_event_is_checked_again_before_submission_and_listing_zero_is_preserved() {
        let mut db = waiting_fixture();
        observe(&mut db, "p1", "1", 1, 3.0, true, 100);
        db.claim_ready_channel_notification(101, &[]).unwrap();
        let job = db.claim_ready_channel_notification(102, &["a".into()]).unwrap().unwrap();
        assert!(job.waited_for_connection);
        assert!(db.waiting_notification_is_current(&job.event, 102).unwrap());
        observe(&mut db, "p1", "1", 2, 0.0, true, 103);
        assert!(!db.waiting_notification_is_current(&job.event, 104).unwrap());
        let mut listing = job.event;
        listing.stock = 0.0;
        assert!(db.waiting_notification_is_current(&listing, 104).unwrap());
        observe(&mut db, "p1", "1", 3, 0.0, false, 105);
        assert!(!db.waiting_notification_is_current(&listing, 106).unwrap());
    }

    #[test]
    fn offline_routes_wait_without_blocking_online_routes_and_skip_sold_out_events() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage.save_product_config(ProductIdentity { key: "p1".into(), product_id: "1".into(), sku_id: None }, "Fixture".into(), "fixture".into(), Some(1)).unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        for id in ["a", "b"] {
            storage.save_notification_channel(id,id,"feishu",Some(id),&["stock_available".into()]).unwrap();
            storage.mark_notification_channel_tested(id).unwrap();
            storage.set_notification_channel_enabled(id,true).unwrap();
        }
        observe(&mut storage, "p1", "1", 1, 3.0, true, 100);
        let fast = storage.claim_ready_channel_notification(101, &["b".into()]).unwrap().unwrap();
        assert_eq!(fast.channel_id, "b");
        storage.finish_notification(fast.id,"accepted","",102).unwrap();
        assert!(storage.claim_ready_channel_notification(103, &[]).unwrap().is_none());
        observe(&mut storage, "p1", "1", 2, 0.0, true, 110);
        assert!(storage.claim_ready_channel_notification(111, &["a".into()]).unwrap().is_none());
        let status: String = storage.conn.query_row("SELECT status FROM notification_outbox WHERE channel_id='a'",[],|row|row.get(0)).unwrap();
        assert_eq!(status,"skipped");
    }

    #[test]
    fn reconnect_delivers_waiting_events_when_goods_remain_available() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage.save_product_config(ProductIdentity { key: "p1".into(), product_id: "1".into(), sku_id: None }, "Fixture".into(), "fixture".into(), Some(1)).unwrap();
        storage.set_product_enabled("p1",true).unwrap();
        storage.save_notification_channel("a","a","feishu",Some("a"),&["stock_available".into()]).unwrap();
        storage.mark_notification_channel_tested("a").unwrap();
        storage.set_notification_channel_enabled("a",true).unwrap();
        observe(&mut storage,"p1","1",1,3.0,true,100);
        assert!(storage.claim_ready_channel_notification(101,&[]).unwrap().is_none());
        let resumed = storage.claim_ready_channel_notification(150,&["a".into()]).unwrap().unwrap();
        assert_eq!(resumed.event.stock,3.0);
    }

    #[test]
    fn different_routes_claim_in_parallel_while_each_route_keeps_event_order() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "p1".into(),
                    product_id: "1".into(),
                    sku_id: None,
                },
                "Fixture".into(),
                "fixture".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        for id in ["a", "b"] {
            storage
                .save_notification_channel(id, id, "feishu", Some(id), &["stock_available".into()])
                .unwrap();
            storage.mark_notification_channel_tested(id).unwrap();
            storage.set_notification_channel_enabled(id, true).unwrap();
        }
        observe(&mut storage, "p1", "1", 1, 3.0, true, 100);
        let slow = storage.claim_channel_notification(101).unwrap().unwrap();
        assert_eq!(slow.channel_id, "a");
        observe(&mut storage, "p1", "1", 2, 5.0, true, 110);
        let fast = storage.claim_channel_notification(111).unwrap().unwrap();
        assert_eq!(fast.channel_id, "b");
        assert_eq!(fast.event.id, slow.event.id);
        assert!(storage.claim_channel_notification(112).unwrap().is_none());
        storage
            .finish_notification(fast.id, "accepted", "", 113)
            .unwrap();
        let fast_next = storage.claim_channel_notification(114).unwrap().unwrap();
        assert_eq!(fast_next.channel_id, "b");
        assert_eq!(fast_next.event.stock, 5.0);
        storage
            .finish_notification(slow.id, "accepted", "", 115)
            .unwrap();
        let slow_next = storage.claim_channel_notification(116).unwrap().unwrap();
        assert_eq!(slow_next.channel_id, "a");
        assert_eq!(slow_next.event.id, fast_next.event.id);
    }

    #[test]
    fn prominent_claim_keeps_pending_alert_until_platform_acknowledges_it() {
        let temp = super::super::tests::TempDatabase::new();
        let mut storage = Storage::open(&temp.0).unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "p1".into(),
                    product_id: "1".into(),
                    sku_id: None,
                },
                "GR".into(),
                "manual".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        storage.set_product_prominent_alert("p1", true).unwrap();
        let first = event(&mut storage, 1, 100);
        assert_eq!(
            storage
                .claim_prominent_alert(101)
                .unwrap()
                .unwrap()
                .event
                .id,
            first.id
        );
        drop(storage);
        let mut storage = Storage::open(&temp.0).unwrap();
        assert_eq!(
            storage
                .claim_prominent_alert(102)
                .unwrap()
                .unwrap()
                .event
                .id,
            first.id
        );
        storage.acknowledge_prominent_alert(first.id).unwrap();
        storage.acknowledge_prominent_alert(first.id).unwrap();
        assert!(storage.claim_prominent_alert(103).unwrap().is_none());
        assert_eq!(
            storage
                .claim_system_notification(103)
                .unwrap()
                .unwrap()
                .event
                .id,
            first.id
        );
    }

    #[test]
    fn unacknowledged_alert_is_revalidated_before_retrying_platform_presentation() {
        for action in ["disable", "unmark", "unlist", "expire"] {
            let mut storage = Storage::open_in_memory().unwrap();
            storage
                .save_product_config(
                    ProductIdentity {
                        key: "p1".into(),
                        product_id: "1".into(),
                        sku_id: None,
                    },
                    "GR".into(),
                    "manual".into(),
                    Some(1),
                )
                .unwrap();
            storage.set_product_enabled("p1", true).unwrap();
            storage.set_product_prominent_alert("p1", true).unwrap();
            event(&mut storage, 1, 100);
            assert!(storage.claim_prominent_alert(101).unwrap().is_some());
            match action {
                "disable" => storage.set_product_enabled("p1", false).unwrap(),
                "unmark" => storage.set_product_prominent_alert("p1", false).unwrap(),
                "unlist" => {
                    storage
                        .conn
                        .execute(
                            "UPDATE observations SET is_show=0 WHERE product_key='p1'",
                            [],
                        )
                        .unwrap();
                }
                "expire" => {}
                _ => unreachable!(),
            }
            let retry_at = if action == "expire" {
                101 + PROMINENT_MAX_AGE_MS
            } else {
                102
            };
            assert!(
                storage.claim_prominent_alert(retry_at).unwrap().is_none(),
                "{action}"
            );
        }
    }

    #[test]
    fn prominent_delivery_is_independent_exclusive_and_invalidated() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "p1".into(),
                    product_id: "1".into(),
                    sku_id: None,
                },
                "GR".into(),
                "manual".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        storage.set_product_prominent_alert("p1", true).unwrap();
        storage.set_system_notifications_enabled(false).unwrap();
        let first = event(&mut storage, 1, 100);
        storage.set_product_prominent_alert("p1", true).unwrap();
        assert!(storage.claim_channel_notification(101).unwrap().is_none());
        assert!(storage.claim_system_notification(101).unwrap().is_none());
        assert_eq!(
            storage
                .claim_prominent_alert(101)
                .unwrap()
                .unwrap()
                .event
                .id,
            first.id
        );
        storage.acknowledge_prominent_alert(first.id).unwrap();
        assert!(storage.claim_prominent_alert(102).unwrap().is_none());
        storage.clear_history().unwrap();
        storage
            .conn
            .execute("DELETE FROM observations", [])
            .unwrap();
        event(&mut storage, 2, 200);
        storage.set_product_prominent_alert("p1", false).unwrap();
        storage.set_product_prominent_alert("p1", true).unwrap();
        assert!(storage.claim_prominent_alert(201).unwrap().is_none());
        storage.clear_history().unwrap();
        storage
            .conn
            .execute("DELETE FROM observations", [])
            .unwrap();
        event(&mut storage, 3, 300);
        storage.set_product_enabled("p1", false).unwrap();
        assert!(storage.claim_prominent_alert(301).unwrap().is_none());
    }

    #[test]
    fn prominent_burst_preserves_event_stock_and_interleaved_product_order() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage.set_system_notifications_enabled(false).unwrap();
        for (key, id) in [("a", "1"), ("b", "2"), ("c", "3")] {
            storage
                .save_product_config(
                    ProductIdentity {
                        key: key.into(),
                        product_id: id.into(),
                        sku_id: None,
                    },
                    key.into(),
                    "manual".into(),
                    Some(1),
                )
                .unwrap();
            storage.set_product_enabled(key, true).unwrap();
            storage.set_product_prominent_alert(key, true).unwrap();
            observe(&mut storage, key, id, 1, 0.0, false, 100);
        }

        observe(&mut storage, "a", "1", 2, 3.0, true, 200);
        observe(&mut storage, "b", "2", 2, 1.0, true, 201);
        observe(&mut storage, "c", "3", 2, 2.0, true, 202);
        observe(&mut storage, "a", "1", 3, 0.0, true, 300);
        observe(&mut storage, "a", "1", 4, 4.0, true, 400);
        observe(&mut storage, "a", "1", 5, 0.0, true, 500);
        observe(&mut storage, "a", "1", 6, 5.0, true, 600);

        let mut claimed = Vec::new();
        while let Some(job) = storage.claim_prominent_alert(601).unwrap() {
            claimed.push((
                job.event.product.key.clone(),
                job.event.stock,
                job.event.observed_at_ms,
            ));
            storage.acknowledge_prominent_alert(job.event.id).unwrap();
        }
        assert_eq!(
            claimed,
            [
                ("a".into(), 5.0, 600),
                ("b".into(), 1.0, 201),
                ("c".into(), 2.0, 202),
            ]
        );
        assert_eq!(
            storage
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM notification_outbox WHERE channel_id='__system__'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn acknowledging_claimed_event_does_not_delete_its_merged_restock() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage.set_system_notifications_enabled(false).unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "p1".into(),
                    product_id: "1".into(),
                    sku_id: None,
                },
                "GR".into(),
                "manual".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        storage.set_product_prominent_alert("p1", true).unwrap();
        observe(&mut storage, "p1", "1", 1, 0.0, false, 100);
        observe(&mut storage, "p1", "1", 2, 3.0, true, 200);
        let old = storage.claim_prominent_alert(201).unwrap().unwrap();
        let row_id: i64 = storage
            .conn
            .query_row(
                "SELECT id FROM notification_outbox WHERE channel_id='__prominent__'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        observe(&mut storage, "p1", "1", 3, 0.0, true, 300);
        observe(&mut storage, "p1", "1", 4, 4.0, true, 400);
        let updated_row: (i64, i64) = storage
            .conn
            .query_row(
                "SELECT id,event_id FROM notification_outbox WHERE channel_id='__prominent__'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(updated_row.0, row_id);
        assert_ne!(updated_row.1, old.event.id);

        storage.acknowledge_prominent_alert(old.event.id).unwrap();
        let latest = storage.claim_prominent_alert(401).unwrap().unwrap();
        assert_eq!(latest.id, row_id);
        assert_eq!(latest.event.stock, 4.0);
        assert_eq!(latest.event.id, updated_row.1);
    }

    #[test]
    fn stock_increases_route_to_notifications_and_prominent_alerts() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage.set_system_notifications_enabled(true).unwrap();
        storage
            .save_notification_channel(
                "c1",
                "Alerts",
                "feishu",
                Some("c1"),
                &["stock_available".into()],
            )
            .unwrap();
        storage.mark_notification_channel_tested("c1").unwrap();
        storage
            .set_notification_channel_enabled("c1", true)
            .unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "p1".into(),
                    product_id: "1".into(),
                    sku_id: None,
                },
                "GR".into(),
                "manual".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        storage.set_product_prominent_alert("p1", true).unwrap();
        observe(&mut storage, "p1", "1", 1, 0.0, false, 100);
        observe(&mut storage, "p1", "1", 2, 3.0, true, 200);

        let channel = storage.claim_channel_notification(201).unwrap().unwrap();
        let system = storage.claim_system_notification(201).unwrap().unwrap();
        let prominent = storage.claim_prominent_alert(201).unwrap().unwrap();
        assert_eq!(
            prominent.event.kind,
            crate::storage::MonitorEventKind::OutOfStockToInStock
        );
        storage
            .finish_notification(channel.id, "accepted", "accepted", 202)
            .unwrap();
        storage
            .finish_notification(system.id, "accepted", "accepted", 202)
            .unwrap();
        storage
            .acknowledge_prominent_alert(prominent.event.id)
            .unwrap();

        let generation = storage.product_generation("p1").unwrap();
        observe(&mut storage, "p1", "1", 3, 5.0, true, 300);
        let channel = storage.claim_channel_notification(301).unwrap().unwrap();
        let system = storage.claim_system_notification(301).unwrap().unwrap();
        let increased = storage.claim_prominent_alert(301).unwrap().unwrap();
        assert_eq!(
            increased.event.kind,
            crate::storage::MonitorEventKind::StockIncreased
        );
        assert_eq!(increased.event.stock, 5.0);
        assert_eq!(increased.generation, generation);
        storage
            .finish_notification(channel.id, "accepted", "accepted", 302)
            .unwrap();
        storage
            .finish_notification(system.id, "accepted", "accepted", 302)
            .unwrap();
        storage
            .acknowledge_prominent_alert(increased.event.id)
            .unwrap();

        observe(&mut storage, "p1", "1", 4, 5.0, true, 400);
        observe(&mut storage, "p1", "1", 5, 4.0, true, 500);
        assert!(storage.claim_prominent_alert(501).unwrap().is_none());
        assert_eq!(storage.recent_check_runs(10).unwrap().len(), 4);

        observe(&mut storage, "p1", "1", 6, 6.0, true, 600);
        let row_id: i64 = storage
            .conn
            .query_row(
                "SELECT id FROM notification_outbox WHERE channel_id='__prominent__'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        observe(&mut storage, "p1", "1", 7, 7.0, true, 700);
        let merged = storage.claim_prominent_alert(701).unwrap().unwrap();
        assert_eq!(merged.id, row_id);
        assert_eq!(
            merged.event.kind,
            crate::storage::MonitorEventKind::StockIncreased
        );
        assert_eq!(merged.event.stock, 7.0);
        assert_eq!(merged.generation, generation);
        storage
            .acknowledge_prominent_alert(merged.event.id)
            .unwrap();

        observe(&mut storage, "p1", "1", 8, 6.0, true, 800);
        storage.set_product_prominent_alert("p1", false).unwrap();
        observe(&mut storage, "p1", "1", 9, 8.0, true, 900);
        assert!(storage.claim_prominent_alert(901).unwrap().is_none());
        while let Some(channel) = storage.claim_channel_notification(901).unwrap() {
            storage
                .finish_notification(channel.id, "accepted", "accepted", 902)
                .unwrap();
        }
        while let Some(system) = storage.claim_system_notification(901).unwrap() {
            storage
                .finish_notification(system.id, "accepted", "accepted", 902)
                .unwrap();
        }

        observe_missing_stock(&mut storage, "p1", "1", 10, 1_000);
        assert!(storage.claim_prominent_alert(1_001).unwrap().is_none());
        assert!(storage.claim_channel_notification(1_001).unwrap().is_none());
        assert!(storage.claim_system_notification(1_001).unwrap().is_none());
    }

    #[test]
    fn stock_increase_uses_valid_stock_subscription_and_persists_one_job_per_target() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-stock-increase-outbox-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("monitor.sqlite3");
        let mut storage = Storage::open(&path).unwrap();
        storage.set_system_notifications_enabled(true).unwrap();
        for (id, subscriptions) in [
            ("stock", vec!["stock_available".to_owned()]),
            ("other", vec!["monitoring_failed".to_owned()]),
        ] {
            storage
                .save_notification_channel(id, id, "feishu", Some(id), &subscriptions)
                .unwrap();
            storage.mark_notification_channel_tested(id).unwrap();
            storage.set_notification_channel_enabled(id, true).unwrap();
        }
        storage
            .save_product_config(
                ProductIdentity {
                    key: "p1".into(),
                    product_id: "1".into(),
                    sku_id: None,
                },
                "GR".into(),
                "manual".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        observe(&mut storage, "p1", "1", 1, 3.0, true, 100);
        let first_channel = storage.claim_channel_notification(101).unwrap().unwrap();
        let first_system = storage.claim_system_notification(101).unwrap().unwrap();
        storage
            .finish_notification(first_channel.id, "accepted", "accepted", 102)
            .unwrap();
        storage
            .finish_notification(first_system.id, "accepted", "accepted", 102)
            .unwrap();
        observe(&mut storage, "p1", "1", 2, 5.0, true, 200);
        let event_id: i64 = storage
            .conn
            .query_row(
                "SELECT event_id FROM notification_outbox WHERE event_kind='stock_increased' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let jobs: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM notification_outbox WHERE event_id=?1",
                [event_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(jobs, 2, "system 与唯一有效渠道各持有一条 job");

        drop(storage);
        let mut storage = Storage::open(&path).unwrap();
        assert_eq!(
            storage
                .claim_system_notification(201)
                .unwrap()
                .unwrap()
                .event
                .id,
            event_id
        );
        let channel = storage.claim_channel_notification(201).unwrap().unwrap();
        assert_eq!(channel.channel_id, "stock");
        assert_eq!(
            channel.event.kind,
            crate::storage::MonitorEventKind::StockIncreased
        );
        assert!(storage.claim_channel_notification(202).unwrap().is_none());
        assert!(storage.claim_prominent_alert(202).unwrap().is_none());
        drop(storage);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn continuous_stock_increase_burst_keeps_one_latest_alert_and_all_check_history() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage.set_system_notifications_enabled(false).unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "p1".into(),
                    product_id: "1".into(),
                    sku_id: None,
                },
                "GR".into(),
                "manual".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        storage.set_product_prominent_alert("p1", true).unwrap();
        observe(&mut storage, "p1", "1", 1, 0.0, false, 100);
        observe(&mut storage, "p1", "1", 2, 1.0, true, 101);
        let first = storage.claim_prominent_alert(102).unwrap().unwrap();
        let row_id: i64 = storage
            .conn
            .query_row(
                "SELECT id FROM notification_outbox WHERE channel_id='__prominent__'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        for stock in 2..=1_400 {
            observe(
                &mut storage,
                "p1",
                "1",
                stock + 1,
                stock as f64,
                true,
                100 + stock as i64,
            );
        }
        storage.acknowledge_prominent_alert(first.event.id).unwrap();
        let pending: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM notification_outbox
                 WHERE channel_id='__prominent__' AND status='pending'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1);
        let latest = storage.claim_prominent_alert(1_501).unwrap().unwrap();
        assert_eq!(latest.id, row_id);
        assert_eq!(latest.event.id, 1_400);
        assert_eq!(
            latest.event.kind,
            crate::storage::MonitorEventKind::StockIncreased
        );
        assert_eq!(latest.event.stock, 1_400.0);
        assert_eq!(
            storage
                .conn
                .query_row("SELECT COUNT(*) FROM check_runs", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1_401
        );
        storage
            .acknowledge_prominent_alert(latest.event.id)
            .unwrap();
        assert!(storage.claim_prominent_alert(1_502).unwrap().is_none());
    }

    fn observe(
        storage: &mut Storage,
        key: &str,
        product_id: &str,
        sequence: u64,
        stock: f64,
        listed: bool,
        at_ms: i64,
    ) {
        let product = ProductIdentity {
            key: key.into(),
            product_id: product_id.into(),
            sku_id: None,
        };
        let prior = storage
            .observation(key)
            .unwrap()
            .map(|observation| observation.state);
        let mut reducer = ObservationReducer::new(prior);
        let availability = if stock > 0.0 {
            Availability::InStock
        } else {
            Availability::OutOfStock
        };
        let crate::availability::reducer::PrepareOutcome::Prepared(prepared) = reducer.prepare(
            crate::availability::reducer::AvailabilityResponse {
                sequence,
                generation: 0,
                result: Ok(crate::availability::reducer::ObservationResult {
                    availability,
                    is_show: u8::from(listed),
                    stock: Some(stock),
                    observed_at_ms: at_ms,
                }),
            },
            crate::availability::reducer::RuntimeGate {
                generation: 0,
                enabled: true,
                paused: false,
            },
        ) else {
            panic!("fixture observation should be accepted");
        };
        storage.commit_observation(product, prepared, &[]).unwrap();
    }

    fn observe_missing_stock(
        storage: &mut Storage,
        key: &str,
        product_id: &str,
        sequence: u64,
        at_ms: i64,
    ) {
        let product = ProductIdentity {
            key: key.into(),
            product_id: product_id.into(),
            sku_id: None,
        };
        let prior = storage
            .observation(key)
            .unwrap()
            .map(|observation| observation.state);
        let mut reducer = ObservationReducer::new(prior);
        let crate::availability::reducer::PrepareOutcome::Prepared(prepared) = reducer.prepare(
            crate::availability::reducer::AvailabilityResponse {
                sequence,
                generation: 0,
                result: Ok(crate::availability::reducer::ObservationResult {
                    availability: Availability::OutOfStock,
                    is_show: 0,
                    stock: None,
                    observed_at_ms: at_ms,
                }),
            },
            crate::availability::reducer::RuntimeGate {
                generation: 0,
                enabled: true,
                paused: false,
            },
        ) else {
            panic!("fixture missing-stock observation should be accepted");
        };
        storage.commit_observation(product, prepared, &[]).unwrap();
    }

    #[test]
    fn prominent_queue_is_bounded_by_distinct_products() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage.set_system_notifications_enabled(false).unwrap();
        for number in 1..=300 {
            let key = format!("p{number}");
            let product_id = number.to_string();
            storage
                .save_product_config(
                    ProductIdentity {
                        key: key.clone(),
                        product_id: product_id.clone(),
                        sku_id: None,
                    },
                    key.clone(),
                    "manual".into(),
                    Some(1),
                )
                .unwrap();
            storage.set_product_enabled(&key, true).unwrap();
            storage.set_product_prominent_alert(&key, true).unwrap();
            observe(&mut storage, &key, &product_id, 1, 0.0, false, 100);
            observe(&mut storage, &key, &product_id, 2, 1.0, true, 101);
        }
        let pending: i64 = storage.conn.query_row("SELECT COUNT(*) FROM notification_outbox WHERE channel_id='__prominent__' AND status='pending'",[],|row|row.get(0)).unwrap();
        assert_eq!(pending, PENDING_PER_CHANNEL);
        let events: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(events, 300);
        let overflow = storage.claim_prominent_alert(102).unwrap_err();
        assert!(overflow.to_string().contains("部分突出提醒因容量上限省略"));
        let pending = |storage: &Storage| {
            storage
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM notification_outbox
                     WHERE channel_id='__prominent__' AND status='pending'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
        };
        assert_eq!(pending(&storage), PENDING_PER_CHANNEL);
        let first = storage.claim_prominent_alert(102).unwrap().unwrap();
        storage.acknowledge_prominent_alert(first.event.id).unwrap();
        assert_eq!(pending(&storage), PENDING_PER_CHANNEL - 1);
        assert!(storage.claim_prominent_alert(102).unwrap().is_some());
        assert!(storage
            .claim_prominent_alert(PROMINENT_MAX_AGE_MS + 102)
            .unwrap()
            .is_none());
    }

    #[test]
    fn prominent_same_product_burst_keeps_latest_event_and_full_history() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage.set_system_notifications_enabled(false).unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "p1".into(),
                    product_id: "1".into(),
                    sku_id: None,
                },
                "GR".into(),
                "manual".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        storage.set_product_prominent_alert("p1", true).unwrap();
        observe(&mut storage, "p1", "1", 1, 0.0, false, 100);
        let mut sequence = 2;
        for cycle in 1..=1_400 {
            if cycle > 1 {
                observe(
                    &mut storage,
                    "p1",
                    "1",
                    sequence,
                    0.0,
                    true,
                    100 + sequence as i64,
                );
                sequence += 1;
            }
            observe(
                &mut storage,
                "p1",
                "1",
                sequence,
                cycle as f64,
                true,
                100 + sequence as i64,
            );
            sequence += 1;
        }
        let pending: i64 = storage.conn.query_row("SELECT COUNT(*) FROM notification_outbox WHERE channel_id='__prominent__' AND status='pending'",[],|row|row.get(0)).unwrap();
        assert_eq!(pending, 1);
        let history: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE product_key='p1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history, 1_400);
        let latest = storage.claim_prominent_alert(3_000).unwrap().unwrap();
        assert_eq!(latest.event.id, 1_400);
        assert_eq!(latest.event.stock, 1_400.0);
        assert_eq!(latest.event.request_sequence, 2_800);
        storage
            .acknowledge_prominent_alert(latest.event.id)
            .unwrap();
        assert!(storage.claim_prominent_alert(3_000).unwrap().is_none());
    }

    #[test]
    fn prominent_alerts_require_a_recent_still_listed_observation_and_current_stock() {
        for (event_stock, current_is_show, current_stock, age, delivered) in [
            (1.0, 1, 1.0, 100, true),
            (1.0, 0, 1.0, 100, false),
            (1.0, 1, 0.0, 100, false),
            (0.0, 1, 0.0, 100, true),
            (0.0, 0, 0.0, 100, false),
            (1.0, 1, 1.0, PROMINENT_MAX_AGE_MS + 1, false),
        ] {
            let mut storage = Storage::open_in_memory().unwrap();
            storage
                .save_product_config(
                    ProductIdentity {
                        key: "p1".into(),
                        product_id: "1".into(),
                        sku_id: None,
                    },
                    "GR".into(),
                    "manual".into(),
                    Some(1),
                )
                .unwrap();
            storage.set_product_enabled("p1", true).unwrap();
            storage.set_product_prominent_alert("p1", true).unwrap();
            event(&mut storage, 1, 100);
            storage
                .conn
                .execute(
                    "UPDATE notification_outbox SET stock=?1 WHERE channel_id='__prominent__'",
                    [event_stock],
                )
                .unwrap();
            storage
                .conn
                .execute(
                    "UPDATE observations SET is_show=?1,stock=?2 WHERE product_key='p1'",
                    params![current_is_show, current_stock],
                )
                .unwrap();
            let alert = storage.claim_prominent_alert(100 + age).unwrap();
            assert_eq!(alert.is_some(), delivered);
            if let Some(alert) = alert {
                storage.acknowledge_prominent_alert(alert.event.id).unwrap();
            }
            assert!(storage.claim_prominent_alert(100 + age).unwrap().is_none());
            assert!(storage
                .claim_system_notification(100 + age)
                .unwrap()
                .is_some());
        }
    }

    fn event(storage: &mut Storage, sequence: u64, at_ms: i64) -> ListingEvent {
        let product = super::super::ProductIdentity {
            key: "p1".into(),
            product_id: "1".into(),
            sku_id: None,
        };
        let prior = storage
            .observation("p1")
            .unwrap()
            .map(|stored| stored.state);
        let mut reducer = ObservationReducer::new(prior);
        let response = crate::availability::reducer::AvailabilityResponse {
            sequence,
            generation: 0,
            result: Ok(crate::availability::reducer::ObservationResult {
                availability: Availability::InStock,
                is_show: 1,
                stock: Some(1.0),
                observed_at_ms: at_ms,
            }),
        };
        let crate::availability::reducer::PrepareOutcome::Prepared(prepared) = reducer.prepare(
            response,
            crate::availability::reducer::RuntimeGate {
                generation: 0,
                enabled: true,
                paused: false,
            },
        ) else {
            panic!("expected event")
        };
        storage
            .commit_observation(product, prepared, &[])
            .unwrap()
            .event
            .unwrap()
    }

    #[test]
    fn event_ids_do_not_reuse_after_history_clear() {
        let mut storage = Storage::open_in_memory().unwrap();
        let first = event(&mut storage, 1, 100);
        storage.clear_history().unwrap();
        let next = storage
            .commit_health_transition(
                super::super::ProductIdentity {
                    key: "p1".into(),
                    product_id: "1".into(),
                    sku_id: None,
                },
                2,
                crate::scheduler::MonitoringHealthTransition::Recovered,
                200,
                &[],
            )
            .unwrap()
            .unwrap();
        assert!(next.id > first.id);
    }

    #[test]
    fn pending_channel_job_survives_reopen() {
        let temp = super::super::tests::TempDatabase::new();
        let mut storage = Storage::open(&temp.0).unwrap();
        storage
            .save_notification_channel(
                "c1",
                "Alerts",
                "feishu",
                Some("c1"),
                &["stock_available".into()],
            )
            .unwrap();
        storage.mark_notification_channel_tested("c1").unwrap();
        storage
            .set_notification_channel_enabled("c1", true)
            .unwrap();
        let first = event(&mut storage, 1, 100);
        drop(storage);
        let mut reopened = Storage::open(&temp.0).unwrap();
        let job = reopened.claim_channel_notification(101).unwrap().unwrap();
        assert_eq!(job.event.id, first.id);
        assert_eq!(job.channel_id, "c1");
    }

    #[test]
    fn interrupted_delivery_is_unknown_and_never_resent() {
        let temp = super::super::tests::TempDatabase::new();
        let mut storage = Storage::open(&temp.0).unwrap();
        storage
            .save_notification_channel(
                "c1",
                "Alerts",
                "feishu",
                Some("c1"),
                &["stock_available".into()],
            )
            .unwrap();
        storage.mark_notification_channel_tested("c1").unwrap();
        storage
            .set_notification_channel_enabled("c1", true)
            .unwrap();
        event(&mut storage, 1, 100);
        let claimed = storage.claim_channel_notification(101).unwrap().unwrap();
        drop(storage);
        let mut reopened = Storage::open(&temp.0).unwrap();
        assert!(reopened
            .claim_channel_notification(LEASE_MS + 102)
            .unwrap()
            .is_none());
        let delivery = reopened.notification_delivery("c1").unwrap().unwrap();
        assert_eq!(delivery.outcome, "unknown");
        assert_eq!(delivery.event_kind, "stock_available");
        assert_eq!(
            reopened
                .conn
                .query_row(
                    "SELECT status FROM notification_outbox WHERE id=?1",
                    [claimed.id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "unknown"
        );
    }

    #[test]
    fn disabled_then_reenabled_product_does_not_receive_old_jobs() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_product_config(
                super::super::ProductIdentity {
                    key: "p1".into(),
                    product_id: "1".into(),
                    sku_id: None,
                },
                "Fixture".into(),
                "manual".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        storage
            .save_notification_channel(
                "c1",
                "Alerts",
                "feishu",
                Some("c1"),
                &["stock_available".into()],
            )
            .unwrap();
        storage.mark_notification_channel_tested("c1").unwrap();
        storage
            .set_notification_channel_enabled("c1", true)
            .unwrap();
        event(&mut storage, 1, 100);
        storage.set_product_enabled("p1", false).unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        assert!(storage.claim_channel_notification(101).unwrap().is_none());
        assert!(storage.claim_system_notification(101).unwrap().is_none());
        let skipped: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM notification_outbox WHERE status='skipped'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(skipped, 2);
    }

    #[test]
    fn one_event_has_independent_recipient_results_and_accepted_recipient_is_never_replayed() {
        let temp = super::super::tests::TempDatabase::new();
        let mut storage = Storage::open(&temp.0).unwrap();
        storage
            .save_notification_channel(
                "account",
                "通知账户",
                "feishu",
                Some("account"),
                &["stock_available".into()],
            )
            .unwrap();
        let targets = vec![
            crate::notifications::NotificationTarget {
                id: "chat-a".into(),
                kind: "chat".into(),
                label: "群 A".into(),
            },
            crate::notifications::NotificationTarget {
                id: "chat-b".into(),
                kind: "chat".into(),
                label: "群 B".into(),
            },
        ];
        storage
            .save_notification_routes("account", &targets)
            .unwrap();
        storage.mark_notification_channel_tested("account").unwrap();
        storage
            .set_notification_channel_enabled("account", true)
            .unwrap();
        let sample = event(&mut storage, 1, 100);
        let first = storage.claim_channel_notification(101).unwrap().unwrap();
        assert_eq!(first.channel_id, "account");
        assert_eq!(first.event.id, sample.id);
        assert_eq!(first.target.unwrap().id, "chat-a");
        storage
            .finish_notification(first.id, "accepted", "群 A 已接受", 102)
            .unwrap();
        let second = storage.claim_channel_notification(103).unwrap().unwrap();
        assert_eq!(second.event.id, sample.id);
        assert_eq!(second.target.unwrap().id, "chat-b");
        storage
            .finish_notification(second.id, "failed", "群 B 无权限", 104)
            .unwrap();
        drop(storage);
        let mut reopened = Storage::open(&temp.0).unwrap();
        assert!(reopened.claim_channel_notification(105).unwrap().is_none());
        let routes = reopened.notification_routes("account").unwrap();
        assert_eq!(
            routes[0].last_delivery.as_ref().unwrap().outcome,
            "accepted"
        );
        assert_eq!(routes[1].last_delivery.as_ref().unwrap().outcome, "failed");
        reopened
            .save_notification_routes("account", &targets)
            .unwrap();
        assert!(reopened.claim_channel_notification(106).unwrap().is_none());
    }

    #[test]
    fn channel_capacity_is_bounded_and_failure_is_visible() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_notification_channel(
                "c1",
                "Alerts",
                "feishu",
                Some("c1"),
                &["stock_available".into()],
            )
            .unwrap();
        storage.mark_notification_channel_tested("c1").unwrap();
        storage
            .set_notification_channel_enabled("c1", true)
            .unwrap();
        let mut sample = event(&mut storage, 1, 100);
        for id in 2..=257 {
            sample.id = id;
            sample.observed_at_ms = 100 + id;
            enqueue_event(&storage.conn, &sample).unwrap();
        }
        let pending: i64 = storage.conn.query_row(
            "SELECT COUNT(*) FROM notification_outbox WHERE channel_id='c1' AND status='pending'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(pending, 256);
        assert_eq!(
            storage
                .notification_delivery("c1")
                .unwrap()
                .unwrap()
                .outcome,
            "failed"
        );
        assert_eq!(
            storage
                .system_notification_delivery()
                .unwrap()
                .unwrap()
                .outcome,
            "failed"
        );
    }

    #[test]
    fn expired_pending_job_reports_failure() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_notification_channel(
                "c1",
                "Alerts",
                "feishu",
                Some("c1"),
                &["stock_available".into()],
            )
            .unwrap();
        storage.mark_notification_channel_tested("c1").unwrap();
        storage
            .set_notification_channel_enabled("c1", true)
            .unwrap();
        event(&mut storage, 1, 100);
        let late = 100 + MAX_AGE_MS + 1;
        assert!(storage.claim_channel_notification(late).unwrap().is_none());
        assert!(storage.claim_system_notification(late).unwrap().is_none());
        assert!(storage
            .notification_delivery("c1")
            .unwrap()
            .unwrap()
            .message
            .contains("24 小时"));
        assert_eq!(
            storage
                .system_notification_delivery()
                .unwrap()
                .unwrap()
                .outcome,
            "failed"
        );
    }

    #[test]
    fn system_result_uses_public_event_kind() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_product_config(
                super::super::ProductIdentity {
                    key: "p1".into(),
                    product_id: "1".into(),
                    sku_id: None,
                },
                "Fixture".into(),
                "manual".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("p1", true).unwrap();
        event(&mut storage, 1, 100);
        let job = storage.claim_system_notification(101).unwrap().unwrap();
        storage
            .finish_notification(job.id, "accepted", "系统已接受通知", 102)
            .unwrap();
        let result = storage.system_notification_delivery().unwrap().unwrap();
        assert_eq!(result.outcome, "accepted");
        assert_eq!(result.event_kind, "stock_available");
    }

    #[test]
    fn global_capacity_is_bounded() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_notification_channel(
                "c1",
                "Alerts",
                "feishu",
                Some("c1"),
                &["stock_available".into()],
            )
            .unwrap();
        storage.mark_notification_channel_tested("c1").unwrap();
        storage
            .set_notification_channel_enabled("c1", true)
            .unwrap();
        for id in 1..=TOTAL_PENDING {
            storage.conn.execute("INSERT INTO notification_outbox(event_id,channel_id,product_key,product_id,event_kind,stock,request_sequence,product_generation,observed_at_ms)
                VALUES(?1,'fixture','p1','1','first_observed_in_stock',1,1,0,100)", [id]).unwrap();
        }
        event(&mut storage, 1, 100);
        let pending: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM notification_outbox WHERE status='pending'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, TOTAL_PENDING);
        assert_eq!(
            storage
                .notification_delivery("c1")
                .unwrap()
                .unwrap()
                .outcome,
            "failed"
        );
    }

    #[test]
    fn deleted_channel_does_not_block_completed_job() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_notification_channel(
                "c1",
                "Alerts",
                "feishu",
                Some("c1"),
                &["stock_available".into()],
            )
            .unwrap();
        storage.mark_notification_channel_tested("c1").unwrap();
        storage
            .set_notification_channel_enabled("c1", true)
            .unwrap();
        event(&mut storage, 1, 100);
        let job = storage.claim_channel_notification(101).unwrap().unwrap();
        storage.delete_notification_channel("c1").unwrap();
        storage
            .record_notification_delivery(
                "c1",
                "accepted",
                "stock_available",
                102,
                "服务已接受通知",
            )
            .unwrap();
        storage
            .finish_notification(job.id, "accepted", "服务已接受通知", 102)
            .unwrap();
        assert!(storage.notification_delivery("c1").unwrap().is_none());
        assert_eq!(
            storage
                .conn
                .query_row(
                    "SELECT status FROM notification_outbox WHERE id=?1",
                    [job.id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "sent"
        );
    }

    #[test]
    fn deleted_channel_does_not_block_expiry_or_interrupted_recovery() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_notification_channel(
                "c1",
                "Alerts",
                "feishu",
                Some("c1"),
                &["stock_available".into()],
            )
            .unwrap();
        storage.mark_notification_channel_tested("c1").unwrap();
        storage
            .set_notification_channel_enabled("c1", true)
            .unwrap();
        let mut sample = event(&mut storage, 1, 100);
        let claimed = storage.claim_channel_notification(101).unwrap().unwrap();
        sample.id += 1;
        enqueue_event(&storage.conn, &sample).unwrap();
        storage.delete_notification_channel("c1").unwrap();
        assert!(storage
            .claim_channel_notification(100 + MAX_AGE_MS + 1)
            .unwrap()
            .is_none());
        let statuses = storage
            .conn
            .prepare("SELECT status FROM notification_outbox WHERE channel_id='c1' ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(statuses, ["unknown", "failed"]);
        assert_eq!(claimed.channel_id, "c1");
        assert!(storage.notification_delivery("c1").unwrap().is_none());
    }
}
