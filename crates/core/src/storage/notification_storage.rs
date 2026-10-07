use super::{Storage, StorageError};
use crate::notifications::NotificationTarget;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationChannel {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub credential_ref: Option<String>,
    pub subscriptions: Vec<String>,
    pub tested: bool,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelTestRecord {
    pub outcome: String,
    pub tested_at_ms: i64,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelDeliveryRecord {
    pub outcome: String,
    pub event_kind: String,
    pub at_ms: i64,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationRoute {
    pub id: i64,
    pub account_id: String,
    pub target: NotificationTarget,
    pub last_delivery: Option<ChannelDeliveryRecord>,
}

pub(super) fn initialize(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS notification_channels (
            id TEXT PRIMARY KEY, name TEXT NOT NULL, provider TEXT NOT NULL,
            credential_ref TEXT, subscriptions TEXT NOT NULL,
            tested INTEGER NOT NULL DEFAULT 0 CHECK (tested IN (0, 1)),
            enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
            next_send_at_ms INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS notification_tests (
            channel_id TEXT PRIMARY KEY REFERENCES notification_channels(id) ON DELETE CASCADE,
            outcome TEXT NOT NULL CHECK(outcome IN ('accepted','failed','unknown')),
            tested_at_ms INTEGER NOT NULL, message TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS notification_deliveries (
            channel_id TEXT PRIMARY KEY REFERENCES notification_channels(id) ON DELETE CASCADE,
            outcome TEXT NOT NULL CHECK(outcome IN ('accepted','failed','unknown')),
            event_kind TEXT NOT NULL, at_ms INTEGER NOT NULL, message TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS notification_routes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id TEXT NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
            target_id TEXT NOT NULL,target_kind TEXT NOT NULL,label TEXT NOT NULL,
            outcome TEXT,event_kind TEXT,at_ms INTEGER,message TEXT,
            UNIQUE(account_id,target_id,target_kind)
        );
        ",
    )?;
    Ok(())
}

pub(super) fn channel_subscribes(
    conn: &Connection,
    id: &str,
    event: &str,
) -> Result<bool, StorageError> {
    let subscriptions: String = conn.query_row(
        "SELECT subscriptions FROM notification_channels WHERE id=?1",
        [id],
        |row| row.get(0),
    )?;
    let subscriptions: Vec<String> =
        serde_json::from_str(&subscriptions).map_err(StorageError::ConfigJson)?;
    Ok(subscriptions.iter().any(|value| value == event))
}

fn channel_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NotificationChannel> {
    Ok(NotificationChannel {
        id: row.get(0)?,
        name: row.get(1)?,
        provider: row.get(2)?,
        credential_ref: row.get(3)?,
        subscriptions: serde_json::from_str(&row.get::<_, String>(4)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        tested: row.get(5)?,
        enabled: row.get(6)?,
    })
}

fn get_channel(conn: &Connection, id: &str) -> Result<NotificationChannel, StorageError> {
    conn.query_row(
        "SELECT id,name,provider,credential_ref,subscriptions,tested,enabled
         FROM notification_channels WHERE id=?1",
        [id],
        channel_from_row,
    )
    .optional()?
    .ok_or(StorageError::ChannelMissing)
}

impl Storage {
    pub fn notification_routes(
        &self,
        account_id: &str,
    ) -> Result<Vec<NotificationRoute>, StorageError> {
        let mut query = self.conn.prepare("SELECT id,account_id,target_id,target_kind,label,outcome,event_kind,at_ms,message FROM notification_routes WHERE account_id=?1 ORDER BY id")?;
        let routes = query
            .query_map([account_id], |row| {
                let outcome: Option<String> = row.get(5)?;
                Ok(NotificationRoute {
                    id: row.get(0)?,
                    account_id: row.get(1)?,
                    target: NotificationTarget {
                        id: row.get(2)?,
                        kind: row.get(3)?,
                        label: row.get(4)?,
                    },
                    last_delivery: outcome
                        .map(|outcome| {
                            Ok::<_, rusqlite::Error>(ChannelDeliveryRecord {
                                outcome,
                                event_kind: row.get(6)?,
                                at_ms: row.get(7)?,
                                message: row.get(8)?,
                            })
                        })
                        .transpose()?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(routes)
    }

    pub fn save_notification_routes(
        &mut self,
        account_id: &str,
        targets: &[NotificationTarget],
    ) -> Result<(), StorageError> {
        let existing = self.notification_routes(account_id)?;
        let tx = self.conn.transaction()?;
        for route in existing {
            if !targets
                .iter()
                .any(|target| target.id == route.target.id && target.kind == route.target.kind)
            {
                tx.execute("DELETE FROM notification_routes WHERE id=?1", [route.id])?;
            }
        }
        for target in targets {
            if target.id.trim().is_empty() || target.kind.trim().is_empty() {
                return Err(StorageError::InvalidChannel);
            }
            tx.execute("INSERT INTO notification_routes(account_id,target_id,target_kind,label) VALUES(?1,?2,?3,?4) ON CONFLICT(account_id,target_id,target_kind) DO UPDATE SET label=excluded.label",params![account_id,target.id,target.kind,target.label])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn record_recipient_delivery(
        &mut self,
        account_id: &str,
        target: &NotificationTarget,
        outcome: &str,
        event: &str,
        at_ms: i64,
        message: &str,
    ) -> Result<(), StorageError> {
        self.conn.execute("UPDATE notification_routes SET outcome=?4,event_kind=?5,at_ms=?6,message=?7 WHERE account_id=?1 AND target_id=?2 AND target_kind=?3",params![account_id,target.id,target.kind,outcome,event,at_ms,message])?;
        Ok(())
    }

    pub fn notification_event_is_enabled(
        &self,
        channel_id: &str,
        subscription: &str,
        product_key: &str,
        generation: Option<u64>,
    ) -> Result<bool, StorageError> {
        if let Some(expected) = generation {
            if self.product_generation(product_key)? != expected {
                return Ok(false);
            }
        }
        let channel = self
            .notification_channels()?
            .into_iter()
            .find(|channel| channel.id == channel_id);
        let Some(channel) = channel else {
            return Ok(false);
        };
        if !channel.enabled
            || !channel.tested
            || !channel
                .subscriptions
                .iter()
                .any(|item| item == subscription)
        {
            return Ok(false);
        }
        let product = self
            .product_configs()?
            .into_iter()
            .find(|product| product.identity.key == product_key);
        if !product.is_some_and(|product| product.enabled) {
            return Ok(false);
        }
        Ok(true)
    }

    pub fn record_notification_delivery(
        &mut self,
        id: &str,
        outcome: &str,
        event_kind: &str,
        at_ms: i64,
        message: &str,
    ) -> Result<(), StorageError> {
        if !["accepted", "failed", "unknown"].contains(&outcome) {
            return Err(StorageError::InvalidChannel);
        }
        self.conn.execute(
            "INSERT INTO notification_deliveries(channel_id,outcome,event_kind,at_ms,message)
             SELECT id,?2,?3,?4,?5 FROM notification_channels WHERE id=?1
             ON CONFLICT(channel_id) DO UPDATE SET
             outcome=excluded.outcome,event_kind=excluded.event_kind,
             at_ms=excluded.at_ms,message=excluded.message",
            params![id, outcome, event_kind, at_ms, message],
        )?;
        Ok(())
    }

    pub fn notification_delivery(
        &self,
        id: &str,
    ) -> Result<Option<ChannelDeliveryRecord>, StorageError> {
        Ok(self.conn.query_row(
            "SELECT outcome,event_kind,at_ms,message FROM notification_deliveries WHERE channel_id=?1",
            [id],
            |row| Ok(ChannelDeliveryRecord {
                outcome: row.get(0)?, event_kind: row.get(1)?, at_ms: row.get(2)?, message: row.get(3)?,
            }),
        ).optional()?)
    }

    pub fn reserve_notification_test(&mut self, id: &str, now_ms: i64) -> Result<(), StorageError> {
        get_channel(&self.conn, id)?;
        self.conn.execute(
            "UPDATE notification_channels SET next_send_at_ms=MAX(next_send_at_ms,?2) WHERE id=?1",
            params![id, now_ms.saturating_add(3100)],
        )?;
        Ok(())
    }

    pub fn record_notification_test(
        &mut self,
        id: &str,
        outcome: &str,
        now_ms: i64,
        message: &str,
        _retry_after_ms: Option<i64>,
    ) -> Result<(), StorageError> {
        if !["accepted", "failed", "unknown"].contains(&outcome) {
            return Err(StorageError::InvalidChannel);
        }
        let channel = match get_channel(&self.conn, id) {
            Ok(channel) => channel,
            Err(StorageError::ChannelMissing) => return Ok(()),
            Err(error) => return Err(error),
        };
        self.conn.execute(
            "INSERT INTO notification_tests(channel_id,outcome,tested_at_ms,message)
             VALUES (?1,?2,?3,?4) ON CONFLICT(channel_id) DO UPDATE SET
             outcome=excluded.outcome,tested_at_ms=excluded.tested_at_ms,message=excluded.message",
            params![
                id,
                outcome,
                now_ms,
                message.chars().take(240).collect::<String>()
            ],
        )?;
        self.conn.execute(
            "UPDATE notification_channels SET tested=?2,enabled=?3 WHERE id=?1",
            params![
                id,
                outcome == "accepted",
                outcome == "accepted" && channel.enabled
            ],
        )?;
        Ok(())
    }

    pub fn notification_channel_test(
        &self,
        id: &str,
    ) -> Result<Option<ChannelTestRecord>, StorageError> {
        Ok(self
            .conn
            .query_row(
                "SELECT outcome,tested_at_ms,message FROM notification_tests WHERE channel_id=?1",
                [id],
                |row| {
                    Ok(ChannelTestRecord {
                        outcome: row.get(0)?,
                        tested_at_ms: row.get(1)?,
                        message: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn notification_channels(&self) -> Result<Vec<NotificationChannel>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT id,name,provider,credential_ref,subscriptions,tested,enabled
             FROM notification_channels ORDER BY id",
        )?;
        let channels = statement
            .query_map([], channel_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(channels)
    }

    pub fn save_notification_channel(
        &mut self,
        id: &str,
        name: &str,
        provider: &str,
        credential_ref: Option<&str>,
        subscriptions: &[String],
    ) -> Result<NotificationChannel, StorageError> {
        if id.trim().is_empty()
            || name.trim().is_empty()
            || !["system", "feishu", "wecom", "dingtalk", "weixin"].contains(&provider)
            || (provider != "system" && credential_ref.is_none_or(|value| value.trim().is_empty()))
            || subscriptions.iter().any(|value| {
                !["stock_available", "monitoring_failed", "recovered"].contains(&value.as_str())
            })
        {
            return Err(StorageError::InvalidChannel);
        }
        let encoded = serde_json::to_string(subscriptions).map_err(StorageError::ConfigJson)?;
        self.conn.execute(
            "INSERT INTO notification_channels(id,name,provider,credential_ref,subscriptions)
             VALUES (?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET
             name=excluded.name,provider=excluded.provider,credential_ref=excluded.credential_ref,
             subscriptions=excluded.subscriptions,tested=0,enabled=0",
            params![id, name, provider, credential_ref, encoded],
        )?;
        self.conn
            .execute("DELETE FROM notification_tests WHERE channel_id=?1", [id])?;
        get_channel(&self.conn, id)
    }

    pub fn update_notification_channel_details(
        &mut self,
        id: &str,
        name: &str,
        subscriptions: &[String],
    ) -> Result<(), StorageError> {
        if name.trim().is_empty()
            || subscriptions.iter().any(|value| {
                !["stock_available", "monitoring_failed", "recovered"].contains(&value.as_str())
            })
        {
            return Err(StorageError::InvalidChannel);
        }
        let encoded = serde_json::to_string(subscriptions).map_err(StorageError::ConfigJson)?;
        if self.conn.execute(
            "UPDATE notification_channels SET name=?2,subscriptions=?3 WHERE id=?1",
            params![id, name, encoded],
        )? == 0
        {
            return Err(StorageError::ChannelMissing);
        }
        Ok(())
    }

    pub fn mark_notification_channel_tested(&mut self, id: &str) -> Result<(), StorageError> {
        if self.conn.execute(
            "UPDATE notification_channels SET tested=1 WHERE id=?1",
            [id],
        )? == 0
        {
            return Err(StorageError::ChannelNotTested);
        }
        Ok(())
    }

    pub fn set_notification_channel_enabled(
        &mut self,
        id: &str,
        enabled: bool,
    ) -> Result<(), StorageError> {
        let channel = get_channel(&self.conn, id)?;
        if enabled && !channel.tested {
            return Err(StorageError::ChannelNotTested);
        }
        self.conn.execute(
            "UPDATE notification_channels SET enabled=?2 WHERE id=?1",
            params![id, enabled],
        )?;
        Ok(())
    }

    pub fn delete_notification_channel(&mut self, id: &str) -> Result<(), StorageError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM notification_channels WHERE id=?1", [id])?;
        tx.execute(
            "DELETE FROM credentials WHERE kind='channel' AND reference=?1",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_notification_delivery_is_visible_without_credentials() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_notification_channel("c1", "alerts", "feishu", Some("private-ref"), &[])
            .unwrap();
        storage
            .record_notification_delivery("c1", "failed", "stock_available", 42, "HTTP 503")
            .unwrap();
        assert_eq!(
            storage.notification_delivery("c1").unwrap(),
            Some(ChannelDeliveryRecord {
                outcome: "failed".into(),
                event_kind: "stock_available".into(),
                at_ms: 42,
                message: "HTTP 503".into(),
            })
        );
    }

    #[test]
    fn credential_scopes_do_not_overlap_and_deletion_cleans_up_atomically() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage
            .save_notification_channel("same", "alerts", "feishu", Some("same"), &[])
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO proxies(id,protocol,host,port,credential_ref) VALUES('same','http','127.0.0.1',8080,'same')",
                [],
            )
            .unwrap();
        storage
            .save_credential("channel", "same", "channel-secret")
            .unwrap();
        storage
            .save_credential("proxy", "same", "proxy-secret")
            .unwrap();

        storage.delete_notification_channel("same").unwrap();
        assert_eq!(storage.credential("channel", "same").unwrap(), None);
        assert_eq!(
            storage.credential("proxy", "same").unwrap().as_deref(),
            Some("proxy-secret")
        );
        storage.remove_proxy("same").unwrap();
        assert_eq!(storage.credential("proxy", "same").unwrap(), None);
    }
}
