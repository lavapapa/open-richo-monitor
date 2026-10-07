use rusqlite::{params, Connection, OptionalExtension};

use super::{Storage, StorageError};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductStatistics {
    pub today_check_count: u64,
    pub today_success_count: u64,
    pub today_failure_count: u64,
    pub monitoring_ms: u64,
}

pub(super) fn initialize(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS product_statistics (
            product_key TEXT PRIMARY KEY REFERENCES products(product_key) ON DELETE CASCADE,
            today_day INTEGER,
            today_check_count INTEGER NOT NULL DEFAULT 0,
            today_success_count INTEGER NOT NULL DEFAULT 0,
            today_failure_count INTEGER NOT NULL DEFAULT 0,
            monitoring_ms INTEGER NOT NULL DEFAULT 0
        );",
    )?;
    Ok(())
}

fn beijing_day(at_ms: i64) -> i64 {
    ((i128::from(at_ms) + 8 * 60 * 60 * 1000).div_euclid(86_400_000)) as i64
}

impl Storage {
    pub fn add_monitoring_durations(
        &mut self,
        durations: &[(String, u64)],
    ) -> Result<(), StorageError> {
        if durations.is_empty() {
            return Ok(());
        }
        self.commit_time_settlement(|db| {
            for (key, delta_ms) in durations {
                db.add_monitoring_duration(key, *delta_ms)?;
            }
            Ok(())
        })
    }

    pub fn commit_time_settlement<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = operation(self);
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
    pub fn product_statistics(
        &self,
        key: &str,
        now_ms: i64,
    ) -> Result<ProductStatistics, StorageError> {
        let saved = self.conn.query_row(
            "SELECT today_day,today_check_count,today_success_count,today_failure_count,monitoring_ms
             FROM product_statistics WHERE product_key=?1",
            [key], |row| Ok((row.get::<_, Option<i64>>(0)?, ProductStatistics {
                today_check_count: row.get::<_, i64>(1)? as u64,
                today_success_count: row.get::<_, i64>(2)? as u64,
                today_failure_count: row.get::<_, i64>(3)? as u64,
                monitoring_ms: row.get::<_, i64>(4)? as u64,
            })),
        ).optional()?;
        let Some((day, mut statistics)) = saved else {
            return Ok(ProductStatistics::default());
        };
        if day != Some(beijing_day(now_ms)) {
            statistics.today_check_count = 0;
            statistics.today_success_count = 0;
            statistics.today_failure_count = 0;
        }
        Ok(statistics)
    }

    pub fn record_daily_check(
        &mut self,
        key: &str,
        at_ms: i64,
        success: bool,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO product_statistics
             (product_key,today_day,today_check_count,today_success_count,today_failure_count)
             VALUES(?1,?2,1,?3,?4)
             ON CONFLICT(product_key) DO UPDATE SET
               today_check_count=CASE WHEN product_statistics.today_day=excluded.today_day
                 THEN CASE WHEN product_statistics.today_check_count<9223372036854775807
                   THEN product_statistics.today_check_count+1 ELSE product_statistics.today_check_count END
                 ELSE 1 END,
               today_success_count=CASE WHEN product_statistics.today_day=excluded.today_day
                 THEN CASE WHEN product_statistics.today_success_count<9223372036854775807
                   THEN product_statistics.today_success_count+excluded.today_success_count
                   ELSE product_statistics.today_success_count END
                 ELSE excluded.today_success_count END,
               today_failure_count=CASE WHEN product_statistics.today_day=excluded.today_day
                 THEN CASE WHEN product_statistics.today_failure_count<9223372036854775807
                   THEN product_statistics.today_failure_count+excluded.today_failure_count
                   ELSE product_statistics.today_failure_count END
                 ELSE excluded.today_failure_count END,
               today_day=excluded.today_day",
            params![key, beijing_day(at_ms), i64::from(success), i64::from(!success)],
        )?;
        Ok(())
    }

    pub fn add_monitoring_duration(
        &mut self,
        key: &str,
        delta_ms: u64,
    ) -> Result<(), StorageError> {
        if delta_ms == 0 {
            return Ok(());
        }
        let delta_ms = delta_ms.min(i64::MAX as u64) as i64;
        self.conn.execute(
            "INSERT INTO product_statistics(product_key,monitoring_ms) VALUES(?1,?2)
             ON CONFLICT(product_key) DO UPDATE SET monitoring_ms=
               CASE WHEN product_statistics.monitoring_ms>=9223372036854775807-excluded.monitoring_ms
                 THEN 9223372036854775807
                 ELSE product_statistics.monitoring_ms+excluded.monitoring_ms END",
            params![key, delta_ms],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::MonitorConfig,
        storage::{ProductIdentity, RunIntent},
    };

    const BEIJING_MIDNIGHT_MS: i64 = 16 * 60 * 60 * 1000;

    fn identity() -> ProductIdentity {
        ProductIdentity {
            key: "41".into(),
            product_id: "41".into(),
            sku_id: None,
        }
    }

    fn configure(db: &mut Storage) {
        db.save_product_config(identity(), "商品".into(), "manual".into(), Some(1))
            .unwrap();
        db.set_product_enabled("41", true).unwrap();
    }

    #[test]
    fn beijing_midnight_resets_daily_counts_even_without_a_new_request() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db);
        db.record_daily_check("41", BEIJING_MIDNIGHT_MS - 1, true)
            .unwrap();
        db.record_daily_check("41", BEIJING_MIDNIGHT_MS - 1, false)
            .unwrap();
        db.add_monitoring_duration("41", 9_000).unwrap();
        assert_eq!(
            db.product_statistics("41", BEIJING_MIDNIGHT_MS - 1)
                .unwrap(),
            ProductStatistics {
                today_check_count: 2,
                today_success_count: 1,
                today_failure_count: 1,
                monitoring_ms: 9_000,
            }
        );
        assert_eq!(
            db.product_statistics("41", BEIJING_MIDNIGHT_MS).unwrap(),
            ProductStatistics {
                monitoring_ms: 9_000,
                ..Default::default()
            }
        );
        db.record_daily_check("41", BEIJING_MIDNIGHT_MS + 1, false)
            .unwrap();
        db.add_monitoring_duration("41", 1_000).unwrap();
        assert_eq!(
            db.product_statistics("41", BEIJING_MIDNIGHT_MS + 1)
                .unwrap(),
            ProductStatistics {
                today_check_count: 1,
                today_failure_count: 1,
                today_success_count: 0,
                monitoring_ms: 10_000,
            }
        );
    }

    #[test]
    fn statistics_survive_database_reopen_without_counting_offline_time() {
        let path = super::super::tests::TempDatabase::new();
        let mut db = Storage::open(&path.0).unwrap();
        configure(&mut db);
        db.record_daily_check("41", 1, true).unwrap();
        db.add_monitoring_duration("41", 5_123).unwrap();
        drop(db);
        let db = Storage::open(&path.0).unwrap();
        assert_eq!(
            db.product_statistics("41", 1).unwrap(),
            ProductStatistics {
                today_check_count: 1,
                today_success_count: 1,
                today_failure_count: 0,
                monitoring_ms: 5_123,
            }
        );
        assert_eq!(
            db.product_statistics("41", BEIJING_MIDNIGHT_MS)
                .unwrap()
                .monitoring_ms,
            5_123
        );
    }

    #[test]
    fn daily_counts_and_duration_saturate_as_sqlite_integers() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db);
        db.record_daily_check("41", 1, true).unwrap();
        db.conn
            .execute(
                "UPDATE product_statistics SET today_check_count=?1, today_success_count=?1,
            today_failure_count=?1, monitoring_ms=?1",
                [i64::MAX - 1],
            )
            .unwrap();
        db.record_daily_check("41", 1, true).unwrap();
        db.record_daily_check("41", 1, false).unwrap();
        db.record_daily_check("41", 1, true).unwrap();
        db.add_monitoring_duration("41", u64::MAX).unwrap();
        db.add_monitoring_duration("41", 1).unwrap();
        assert_eq!(
            db.product_statistics("41", 1).unwrap(),
            ProductStatistics {
                today_check_count: i64::MAX as u64,
                today_success_count: i64::MAX as u64,
                today_failure_count: i64::MAX as u64,
                monitoring_ms: i64::MAX as u64,
            }
        );
        let types: (String, String, String, String) = db.conn.query_row(
            "SELECT typeof(today_check_count),typeof(today_success_count),typeof(today_failure_count),
             typeof(monitoring_ms) FROM product_statistics WHERE product_key='41'", [],
             |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).unwrap();
        assert_eq!(
            types,
            (
                "integer".into(),
                "integer".into(),
                "integer".into(),
                "integer".into()
            )
        );
    }

    #[test]
    fn failed_monitoring_transaction_rolls_back_statistics_together() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db);
        db.set_run_intent(RunIntent::Running, false).unwrap();
        let generation = db.product_generation("41").unwrap();
        let result = db.commit_monitoring_attempt("41", generation, |db| {
            db.record_daily_check("41", 1, true)?;
            db.add_monitoring_duration("41", 500)?;
            Err::<(), _>(StorageError::InvalidStock)
        });
        assert!(matches!(result, Err(StorageError::InvalidStock)));
        assert_eq!(
            db.product_statistics("41", 1).unwrap(),
            ProductStatistics::default()
        );
    }

    #[test]
    fn second_product_failure_rolls_back_the_entire_duration_settlement() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db);
        db.add_monitoring_duration("41", 25).unwrap();
        assert!(db
            .add_monitoring_durations(&[("41".into(), 100), ("missing".into(), 100)])
            .is_err());
        assert_eq!(db.product_statistics("41", 1).unwrap().monitoring_ms, 25);
        db.add_monitoring_durations(&[("41".into(), 100)]).unwrap();
        assert_eq!(db.product_statistics("41", 1).unwrap().monitoring_ms, 125);
    }

    #[test]
    fn another_connection_cannot_reset_between_version_read_and_duration_write() {
        let path = super::super::tests::TempDatabase::new();
        let mut owner = Storage::open(&path.0).unwrap();
        configure(&mut owner);
        let mut controller = Storage::open(&path.0).unwrap();
        controller
            .conn
            .busy_timeout(std::time::Duration::ZERO)
            .unwrap();
        let epoch = owner
            .commit_time_settlement(|db| {
                let epoch = db.catalog_epoch()?;
                assert!(controller
                    .restore_defaults(&MonitorConfig::default(), false)
                    .is_err());
                assert_eq!(db.catalog_epoch()?, epoch);
                db.add_monitoring_duration("41", 100)?;
                Ok(epoch)
            })
            .unwrap();
        assert_eq!(
            owner.product_statistics("41", 1).unwrap().monitoring_ms,
            100
        );
        controller
            .restore_defaults(&MonitorConfig::default(), false)
            .unwrap();
        let written = owner
            .commit_time_settlement(|db| {
                if db.catalog_epoch()? != epoch {
                    return Ok(false);
                }
                db.add_monitoring_duration("41", 100)?;
                Ok(true)
            })
            .unwrap();
        assert!(!written);
        assert_eq!(
            owner.product_statistics("41", 1).unwrap(),
            ProductStatistics::default()
        );
    }

    #[test]
    fn clearing_history_removing_products_and_restoring_defaults_clear_statistics() {
        let mut db = Storage::open_in_memory().unwrap();
        for action in 0..4 {
            configure(&mut db);
            db.record_daily_check("41", 1, true).unwrap();
            db.add_monitoring_duration("41", 500).unwrap();
            match action {
                0 => db.clear_history().unwrap(),
                1 => db.remove_product("41").unwrap(),
                2 => db
                    .restore_defaults(&MonitorConfig::default(), false)
                    .unwrap(),
                3 => db
                    .restore_defaults(&MonitorConfig::default(), true)
                    .unwrap(),
                _ => unreachable!(),
            }
            assert_eq!(
                db.product_statistics("41", 1).unwrap(),
                ProductStatistics::default()
            );
        }
    }

    #[test]
    fn importing_configuration_preserves_existing_statistics() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db);
        db.record_daily_check("41", 1, false).unwrap();
        db.add_monitoring_duration("41", 500).unwrap();
        let before = db.product_statistics("41", 1).unwrap();
        db.import_configuration(
            &MonitorConfig::default(),
            vec![(identity(), "导入商品".into(), true, false)],
            vec![],
            2,
        )
        .unwrap();
        assert_eq!(db.product_statistics("41", 1).unwrap(), before);
        assert!(db
            .import_configuration(
                &MonitorConfig::default(),
                vec![(identity(), "失败导入".into(), true, false)],
                vec![("bad".into(), "bad".into(), "missing".into(), vec![])],
                3
            )
            .is_err());
        assert_eq!(db.product_statistics("41", 1).unwrap(), before);
        assert_eq!(before.monitoring_ms, 500);
    }

    #[test]
    fn day_computation_handles_timestamp_extremes_and_has_one_row_per_product() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db);
        for at_ms in [
            i64::MIN,
            -8 * 60 * 60 * 1000 - 1,
            -8 * 60 * 60 * 1000,
            i64::MAX,
        ] {
            db.record_daily_check("41", at_ms, true).unwrap();
            assert_eq!(
                db.product_statistics("41", at_ms)
                    .unwrap()
                    .today_check_count,
                1
            );
        }
        for day in 0..1000 {
            db.record_daily_check("41", day * 86_400_000, true).unwrap();
        }
        let rows: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM product_statistics", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
        db.conn
            .execute("DELETE FROM products WHERE product_key='41'", [])
            .unwrap();
        assert_eq!(
            db.product_statistics("41", 1).unwrap(),
            ProductStatistics::default()
        );
    }
}
