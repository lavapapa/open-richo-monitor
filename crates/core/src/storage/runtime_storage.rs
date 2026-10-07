use std::collections::{BTreeMap, HashMap};

use rusqlite::{params, Connection};

use super::{Storage, StorageError};
use crate::scheduler::MonitoringHealth;

pub(super) fn initialize(conn: &Connection) -> Result<(), StorageError> {
    if !super::column_exists(conn, "settings", "request_sequence_floor")? {
        conn.execute(
            "ALTER TABLE settings ADD COLUMN request_sequence_floor INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS monitoring_health (
            product_key TEXT PRIMARY KEY REFERENCES products(product_key),
            generation INTEGER NOT NULL,
            request_sequence INTEGER NOT NULL,
            failed_since_ms INTEGER,
            active_failure_ms INTEGER NOT NULL DEFAULT 0,
            failure_reported INTEGER NOT NULL DEFAULT 0,
            last_error TEXT
        );
        UPDATE settings SET request_sequence_floor = MAX(
            request_sequence_floor,
            COALESCE((SELECT MAX(request_sequence) FROM observations), 0),
            COALESCE((SELECT MAX(request_sequence) FROM events), 0)
        ) WHERE id=1;",
    )?;
    Ok(())
}

impl Storage {
    pub fn monitoring_errors(&self) -> Result<BTreeMap<String, String>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT p.product_id,h.last_error FROM monitoring_health h
             JOIN products p USING(product_key)
             JOIN product_rounds r USING(product_key)
             WHERE p.enabled=1 AND p.is_configured=1 AND h.generation=r.generation
               AND h.last_error IS NOT NULL",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(StorageError::from)
    }

    pub fn monitoring_sequence_floor(&self) -> Result<u64, StorageError> {
        let sequence: i64 = self.conn.query_row(
            "SELECT request_sequence_floor FROM settings WHERE id=1",
            [],
            |row| row.get(0),
        )?;
        Ok(sequence as u64)
    }

    pub fn monitoring_health(&self) -> Result<HashMap<u64, (u64, MonitoringHealth)>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT p.product_id, h.request_sequence, h.failed_since_ms,
                    h.active_failure_ms, h.failure_reported
             FROM monitoring_health h
             JOIN products p USING(product_key)
             JOIN product_rounds r USING(product_key)
             WHERE p.enabled=1 AND p.is_configured=1 AND h.generation=r.generation",
        )?;
        let rows = statement.query_map([], |row| {
            let product_id: String = row.get(0)?;
            Ok((
                product_id.parse::<u64>().unwrap_or_default(),
                (
                    row.get::<_, i64>(1)? as u64,
                    MonitoringHealth {
                        failed_since_ms: row.get(2)?,
                        active_failure_ms: row.get::<_, i64>(3)? as u64,
                        failure_reported: row.get(4)?,
                    },
                ),
            ))
        })?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(StorageError::from)
    }

    /// 须在 commit_monitoring_attempt 的事务中调用。
    pub fn record_monitoring_runtime(
        &mut self,
        product_key: &str,
        generation: u64,
        sequence: u64,
        health: MonitoringHealth,
        last_error: Option<&str>,
    ) -> Result<bool, StorageError> {
        let sequence = i64::try_from(sequence).map_err(|_| StorageError::SequenceOutOfRange)?;
        let generation = i64::try_from(generation).map_err(|_| StorageError::SequenceOutOfRange)?;
        let active_ms = i64::try_from(health.active_failure_ms)
            .map_err(|_| StorageError::SequenceOutOfRange)?;
        self.conn.execute(
            "UPDATE settings SET request_sequence_floor=MAX(request_sequence_floor,?1) WHERE id=1",
            [sequence],
        )?;
        let updated = self.conn.execute(
            "INSERT INTO monitoring_health
             (product_key,generation,request_sequence,failed_since_ms,active_failure_ms,failure_reported,last_error)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(product_key) DO UPDATE SET
               generation=excluded.generation,
               request_sequence=excluded.request_sequence,
               failed_since_ms=excluded.failed_since_ms,
               active_failure_ms=excluded.active_failure_ms,
               failure_reported=excluded.failure_reported,
               last_error=excluded.last_error
             WHERE excluded.generation > monitoring_health.generation
                OR (excluded.generation = monitoring_health.generation
                    AND excluded.request_sequence > monitoring_health.request_sequence)",
            params![product_key, generation, sequence, health.failed_since_ms, active_ms, health.failure_reported, last_error],
        )?;
        Ok(updated > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_attempt_runtime_survives_reopen_and_round_reset_is_per_product() {
        let path = super::super::tests::TempDatabase::new();
        let mut storage = Storage::open(&path.0).unwrap();
        storage
            .conn
            .execute_batch(
                "INSERT INTO products(product_key,product_id,verified_at_ms,enabled,is_configured)
             VALUES ('p7','7',1,1,1),('p8','8',1,1,1);
             INSERT INTO product_rounds(product_key,generation) VALUES ('p7',1),('p8',1);
             UPDATE settings SET run_intent='running' WHERE id=1;",
            )
            .unwrap();
        let failed = MonitoringHealth {
            failed_since_ms: Some(1_000),
            active_failure_ms: 4_000,
            failure_reported: false,
        };
        storage
            .commit_monitoring_attempt("p7", 1, |db| {
                db.record_monitoring_runtime("p7", 1, 42, failed, Some("连接超时"))
            })
            .unwrap();
        storage
            .commit_monitoring_attempt("p8", 1, |db| {
                db.record_monitoring_runtime("p8", 1, 43, failed, Some("连接超时"))
            })
            .unwrap();
        drop(storage);

        let mut reopened = Storage::open(&path.0).unwrap();
        assert_eq!(reopened.monitoring_sequence_floor().unwrap(), 43);
        assert_eq!(
            reopened.monitoring_health().unwrap().get(&7),
            Some(&(42, failed))
        );
        assert_eq!(
            reopened
                .monitoring_errors()
                .unwrap()
                .get("7")
                .map(String::as_str),
            Some("连接超时")
        );
        reopened.set_product_enabled("p7", false).unwrap();
        reopened.set_product_enabled("p7", true).unwrap();
        assert!(!reopened.monitoring_health().unwrap().contains_key(&7));
        assert_eq!(
            reopened.monitoring_health().unwrap().get(&8),
            Some(&(43, failed))
        );
        assert_eq!(reopened.monitoring_sequence_floor().unwrap(), 43);
    }
}
