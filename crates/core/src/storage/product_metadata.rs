use rusqlite::{params, Connection, OptionalExtension};

use super::{Storage, StorageError};
use crate::ricoh::ProductMetadata;

pub(super) fn initialize(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS product_metadata (
            product_key TEXT PRIMARY KEY REFERENCES products(product_key) ON DELETE CASCADE,
            metadata_json TEXT NOT NULL,
            updated_at_ms INTEGER NOT NULL
        );",
    )?;
    Ok(())
}

fn product_metadata(
    conn: &Connection,
    key: &str,
) -> Result<Option<(ProductMetadata, i64)>, StorageError> {
    let saved: Option<(String, i64)> = conn
        .query_row(
            "SELECT metadata_json, updated_at_ms FROM product_metadata WHERE product_key=?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    saved
        .map(|(json, at_ms)| {
            serde_json::from_str(&json)
                .map(|metadata| (metadata, at_ms))
                .map_err(StorageError::ConfigJson)
        })
        .transpose()
}

pub(super) fn save(
    conn: &Connection,
    key: &str,
    metadata: &ProductMetadata,
    at_ms: i64,
) -> Result<bool, StorageError> {
    if product_metadata(conn, key)?.is_some_and(|(saved, _)| saved == *metadata) {
        return Ok(false);
    }
    let json = serde_json::to_string(metadata).map_err(StorageError::ConfigJson)?;
    conn.execute(
        "INSERT INTO product_metadata(product_key, metadata_json, updated_at_ms) VALUES(?1, ?2, ?3)
         ON CONFLICT(product_key) DO UPDATE SET metadata_json=excluded.metadata_json,
             updated_at_ms=excluded.updated_at_ms",
        params![key, json, at_ms],
    )?;
    Ok(true)
}

impl Storage {
    pub fn record_product_detail(
        &mut self,
        key: &str,
        detail: &crate::ricoh::ProductDetail,
        at_ms: i64,
    ) -> Result<bool, StorageError> {
        let metadata_changed = self.save_product_metadata(key, &detail.metadata, at_ms)?;
        let name_changed = if detail.name.len() <= crate::ricoh::MAX_PRODUCT_NAME_BYTES {
            self.conn.execute(
                "UPDATE products SET name=?2 WHERE product_key=?1 AND name<>?2",
                params![key, detail.name],
            )? > 0
        } else {
            false
        };
        Ok(metadata_changed || name_changed)
    }
    /// 验证请求返回后的目录提交；与用户选择、元数据和运行代际一起提交。
    pub fn commit_verified_product(
        &mut self,
        detail: &crate::ricoh::ProductDetail,
        expected_generation: u64,
        expected_catalog_epoch: u64,
        enable: bool,
        expected_metadata: Option<&ProductMetadata>,
        at_ms: i64,
    ) -> Result<bool, StorageError> {
        let key = detail.product_id.to_string();
        let tx = self.conn.transaction()?;
        let epoch: u64 = tx.query_row(
            "SELECT revision FROM catalog_lifecycle WHERE id=1",
            [],
            |row| row.get(0),
        )?;
        if epoch != expected_catalog_epoch {
            return Ok(false);
        }
        let removed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_removals WHERE product_key=?1)",
            [&key],
            |row| row.get(0),
        )?;
        if removed && !enable {
            return Ok(false);
        }
        let generation = tx
            .query_row(
                "SELECT generation FROM product_rounds WHERE product_key=?1",
                [&key],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0) as u64;
        if generation != expected_generation {
            return Ok(false);
        }
        if !enable && product_metadata(&tx, &key)?.as_ref().map(|(m, _)| m) != expected_metadata {
            return Ok(false);
        }
        let identity = super::ProductIdentity {
            key: key.clone(),
            product_id: key.clone(),
            sku_id: None,
        };
        super::check_catalog_entry(&tx, &identity, &detail.name)?;
        super::ensure_product(&tx, &identity)?;
        tx.execute(
            "UPDATE products SET name=?2,source=CASE WHEN ?3 THEN 'manual' ELSE source END,
             verified_at_ms=?4,is_configured=1,enabled=CASE WHEN ?3 THEN 1 ELSE enabled END
             WHERE product_key=?1",
            params![key, detail.name, enable, at_ms],
        )?;
        tx.execute("DELETE FROM product_removals WHERE product_key=?1", [&key])?;
        save(&tx, &key, &detail.metadata, at_ms)?;
        if enable {
            super::advance_product_round(&tx, &key)?;
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn product_metadata(
        &self,
        key: &str,
    ) -> Result<Option<(ProductMetadata, i64)>, StorageError> {
        product_metadata(&self.conn, key)
    }

    pub fn save_product_metadata(
        &mut self,
        key: &str,
        metadata: &ProductMetadata,
        at_ms: i64,
    ) -> Result<bool, StorageError> {
        save(&self.conn, key, metadata, at_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        availability::Availability, catalog::ProductIdScan, config::MonitorConfig,
        ricoh::ProductDetail, storage::ProductIdentity,
    };

    fn metadata(image: &str, price: &str) -> ProductMetadata {
        ProductMetadata {
            image_url: Some(format!("https://example.com/{image}.jpg")),
            price: Some(price.into()),
            ..ProductMetadata::default()
        }
    }

    fn configure(db: &mut Storage, enabled: bool) {
        db.save_product_config(
            ProductIdentity {
                key: "41".into(),
                product_id: "41".into(),
                sku_id: None,
            },
            "商品".into(),
            "manual".into(),
            Some(1),
        )
        .unwrap();
        db.set_product_enabled("41", enabled).unwrap();
    }

    fn scan_detail(db: &mut Storage, value: ProductMetadata, at_ms: i64) -> bool {
        let mut scan = ProductIdScan::new(41, 41).unwrap();
        db.start_scan_if_idle(&scan.checkpoint().unwrap()).unwrap();
        assert_eq!(scan.next_id(), Some(41));
        let id = scan.scan_id();
        db.commit_scan_item(
            id,
            scan,
            Some(ProductDetail {
                product_id: 41,
                name: "扫描商品".into(),
                is_show: 1,
                stock: serde_json::json!(3).as_number().unwrap().clone(),
                availability: Availability::InStock,
                metadata: value,
            }),
            at_ms,
        )
        .unwrap()
        .is_some()
    }

    #[test]
    fn monitoring_from_another_connection_wins_over_a_late_catalog_refresh() {
        let path = super::super::tests::TempDatabase::new();
        let mut refreshing = Storage::open(&path.0).unwrap();
        configure(&mut refreshing, true);
        let generation = refreshing.product_generation("41").unwrap();
        let epoch = refreshing.catalog_epoch().unwrap();
        assert!(refreshing.product_metadata("41").unwrap().is_none());
        let mut monitoring = Storage::open(&path.0).unwrap();
        let fresh = metadata("new", "29.99");
        monitoring.save_product_metadata("41", &fresh, 20).unwrap();
        let old = ProductDetail {
            product_id: 41,
            name: "迟到目录资料".into(),
            is_show: 1,
            stock: serde_json::Number::from(1),
            availability: Availability::InStock,
            metadata: metadata("old", "19.99"),
        };
        assert!(!refreshing
            .commit_verified_product(&old, generation, epoch, false, None, 30)
            .unwrap());
        assert_eq!(
            refreshing.product_metadata("41").unwrap(),
            Some((fresh, 20))
        );
        assert_eq!(refreshing.product_configs().unwrap()[0].name, "商品");
        assert!(refreshing
            .commit_verified_product(&old, generation, epoch, true, None, 40)
            .unwrap());
        assert_eq!(
            monitoring.product_metadata("41").unwrap(),
            Some((old.metadata, 40))
        );
    }

    #[test]
    fn partial_catalog_refresh_compares_existing_metadata_and_preserves_selection() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db, true);
        let expected = ProductMetadata {
            price: Some("1.00".into()),
            ..Default::default()
        };
        db.save_product_metadata("41", &expected, 10).unwrap();
        let current = ProductMetadata {
            price: Some("2.00".into()),
            ..Default::default()
        };
        db.save_product_metadata("41", &current, 20).unwrap();
        let generation = db.product_generation("41").unwrap();
        let epoch = db.catalog_epoch().unwrap();
        let detail = ProductDetail {
            product_id: 41,
            name: "图片样例商品".into(),
            is_show: 1,
            stock: serde_json::Number::from(1),
            availability: Availability::InStock,
            metadata: metadata("image", "3.00"),
        };
        assert!(!db
            .commit_verified_product(&detail, generation, epoch, false, Some(&expected), 30)
            .unwrap());
        assert_eq!(
            db.product_metadata("41").unwrap(),
            Some((current.clone(), 20))
        );
        assert!(db
            .commit_verified_product(&detail, generation, epoch, false, Some(&current), 40)
            .unwrap());
        assert!(db.product_configs().unwrap()[0].enabled);
        assert_eq!(db.product_generation("41").unwrap(), generation);
        assert_eq!(db.observation("41").unwrap(), None);
    }

    #[test]
    fn metadata_changes_replace_fields_and_equal_values_keep_the_update_time() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db, false);
        let first = metadata("a", "19.9900");
        assert!(db.save_product_metadata("41", &first, 10).unwrap());
        assert_eq!(
            db.product_metadata("41").unwrap(),
            Some((first.clone(), 10))
        );
        assert!(!db.save_product_metadata("41", &first, 20).unwrap());
        assert_eq!(db.product_metadata("41").unwrap(), Some((first, 10)));
        let changed = metadata("b", "20.00");
        assert!(db.save_product_metadata("41", &changed, 30).unwrap());
        assert_eq!(db.product_metadata("41").unwrap(), Some((changed, 30)));
        assert!(db
            .save_product_metadata("41", &ProductMetadata::default(), 40)
            .unwrap());
        assert_eq!(
            db.product_metadata("41").unwrap(),
            Some((ProductMetadata::default(), 40))
        );
        assert_eq!(db.observation("41").unwrap(), None);
        assert_eq!(db.product_configs().unwrap()[0].name, "商品");
    }

    #[test]
    fn monitoring_detail_refreshes_name_and_metadata_without_changing_selection() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db, true);
        let generation = db.product_generation("41").unwrap();
        let mut detail = ProductDetail {
            product_id: 41,
            name: "更新名称".into(),
            is_show: 1,
            stock: serde_json::Number::from(1),
            availability: Availability::InStock,
            metadata: metadata("new", "19.9900"),
        };
        assert!(db.record_product_detail("41", &detail, 10).unwrap());
        assert_eq!(db.product_configs().unwrap()[0].name, "更新名称");
        assert!(db.product_configs().unwrap()[0].enabled);
        assert_eq!(db.product_generation("41").unwrap(), generation);
        assert!(!db.record_product_detail("41", &detail, 20).unwrap());
        assert_eq!(
            db.product_metadata("41").unwrap(),
            Some((detail.metadata.clone(), 10))
        );
        detail.name = "a".repeat(crate::ricoh::MAX_PRODUCT_NAME_BYTES + 1);
        detail.metadata = metadata("newer", "20.00");
        assert!(db.record_product_detail("41", &detail, 30).unwrap());
        assert_eq!(db.product_configs().unwrap()[0].name, "更新名称");
        assert_eq!(
            db.product_metadata("41").unwrap(),
            Some((detail.metadata, 30))
        );
    }

    #[test]
    fn metadata_write_rolls_back_with_the_monitoring_attempt() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db, true);
        db.set_run_intent(super::super::RunIntent::Running, false)
            .unwrap();
        let generation = db.product_generation("41").unwrap();
        let result = db.commit_monitoring_attempt("41", generation, |db| {
            db.save_product_metadata("41", &metadata("new", "19.99"), 1)?;
            Err::<(), _>(StorageError::InvalidStock)
        });
        assert!(matches!(result, Err(StorageError::InvalidStock)));
        assert_eq!(db.product_metadata("41").unwrap(), None);
    }

    #[test]
    fn scan_refreshes_disabled_products_and_only_fills_empty_enabled_caches() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db, true);
        assert!(scan_detail(&mut db, metadata("scan", "10"), 10));
        assert_eq!(
            db.product_metadata("41").unwrap(),
            Some((metadata("scan", "10"), 10))
        );
        db.save_product_metadata("41", &metadata("monitor", "20"), 20)
            .unwrap();
        assert!(scan_detail(&mut db, metadata("old-scan", "9"), 30));
        assert_eq!(
            db.product_metadata("41").unwrap(),
            Some((metadata("monitor", "20"), 20))
        );
        db.set_product_enabled("41", false).unwrap();
        assert!(scan_detail(&mut db, metadata("new-scan", "30"), 40));
        assert_eq!(
            db.product_metadata("41").unwrap(),
            Some((metadata("new-scan", "30"), 40))
        );
    }

    #[test]
    fn cancelled_scan_cannot_save_metadata() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db, false);
        let mut scan = ProductIdScan::new(41, 41).unwrap();
        db.start_scan_if_idle(&scan.checkpoint().unwrap()).unwrap();
        scan.next_id();
        db.clear_scan_checkpoint().unwrap();
        assert!(db
            .commit_scan_item(
                scan.scan_id(),
                scan,
                Some(ProductDetail {
                    product_id: 41,
                    name: "商品".into(),
                    is_show: 1,
                    stock: serde_json::json!(1).as_number().unwrap().clone(),
                    availability: Availability::InStock,
                    metadata: metadata("old", "1"),
                }),
                10
            )
            .unwrap()
            .is_none());
        assert_eq!(db.product_metadata("41").unwrap(), None);
    }

    #[test]
    fn deleting_or_restoring_clears_metadata_and_history_clear_preserves_it() {
        let mut db = Storage::open_in_memory().unwrap();
        configure(&mut db, false);
        db.save_product_metadata("41", &metadata("a", "10"), 1)
            .unwrap();
        db.clear_history().unwrap();
        assert!(db.product_metadata("41").unwrap().is_some());
        db.remove_product("41").unwrap();
        assert!(db.product_metadata("41").unwrap().is_none());
        for clear_history in [false, true] {
            configure(&mut db, false);
            db.save_product_metadata("41", &metadata("a", "10"), 1)
                .unwrap();
            db.restore_defaults(&MonitorConfig::default(), clear_history)
                .unwrap();
            assert!(db.product_metadata("41").unwrap().is_none());
        }
    }

    #[test]
    fn metadata_survives_reopening_and_foreign_key_deletes_do_not_block_products() {
        let path = super::super::tests::TempDatabase::new();
        let mut db = Storage::open(&path.0).unwrap();
        configure(&mut db, false);
        db.save_product_metadata("41", &metadata("a", "10"), 1)
            .unwrap();
        drop(db);
        let db = Storage::open(&path.0).unwrap();
        assert_eq!(
            db.product_metadata("41").unwrap(),
            Some((metadata("a", "10"), 1))
        );
        db.conn
            .execute("DELETE FROM products WHERE product_key='41'", [])
            .unwrap();
        assert_eq!(db.product_metadata("41").unwrap(), None);
    }
}
