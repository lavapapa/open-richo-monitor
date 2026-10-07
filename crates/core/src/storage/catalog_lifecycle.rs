use rusqlite::{Connection, Transaction};

use super::{Storage, StorageError};

pub(super) fn initialize(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS catalog_lifecycle (
            id INTEGER PRIMARY KEY CHECK(id=1),
            revision INTEGER NOT NULL DEFAULT 0
        );
        INSERT INTO catalog_lifecycle(id,revision) VALUES(1,0) ON CONFLICT(id) DO NOTHING;",
    )?;
    Ok(())
}

pub(super) fn advance(tx: &Transaction<'_>) -> Result<(), StorageError> {
    tx.execute(
        "UPDATE catalog_lifecycle SET revision=revision+1 WHERE id=1",
        [],
    )?;
    Ok(())
}

impl Storage {
    pub fn catalog_epoch(&self) -> Result<u64, StorageError> {
        self.conn
            .query_row(
                "SELECT revision FROM catalog_lifecycle WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(StorageError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        availability::Availability,
        config::MonitorConfig,
        ricoh::{ProductDetail, ProductMetadata},
        storage::ProductIdentity,
    };

    fn detail() -> ProductDetail {
        ProductDetail {
            product_id: 41,
            name: "旧请求".into(),
            is_show: 1,
            stock: serde_json::Number::from(1),
            availability: Availability::InStock,
            metadata: ProductMetadata {
                price: Some("19.99".into()),
                ..Default::default()
            },
        }
    }

    #[test]
    fn another_instance_catalog_changes_reject_late_add_and_hydrate_results() {
        for (action, configured) in [
            ("restore", false),
            ("restore-clear", false),
            ("import", false),
            ("remove", false),
            ("restore", true),
            ("import", true),
        ] {
            for enable in [false, true] {
                let path = super::super::tests::TempDatabase::new();
                let mut reader = Storage::open(&path.0).unwrap();
                if configured {
                    reader
                        .save_product_config(
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
                    reader.set_product_enabled("41", false).unwrap();
                }
                let generation = reader.product_generation("41").unwrap();
                let epoch = reader.catalog_epoch().unwrap();
                let mut controller = Storage::open(&path.0).unwrap();
                match action {
                    "restore" => controller
                        .restore_defaults(&MonitorConfig::default(), false)
                        .unwrap(),
                    "restore-clear" => controller
                        .restore_defaults(&MonitorConfig::default(), true)
                        .unwrap(),
                    "import" => controller
                        .import_configuration(&MonitorConfig::default(), vec![], vec![], 2)
                        .unwrap(),
                    "remove" => controller.remove_product("41").unwrap(),
                    _ => unreachable!(),
                }
                let before = controller.product_configs().unwrap();
                assert!(
                    !reader
                        .commit_verified_product(&detail(), generation, epoch, enable, None, 3)
                        .unwrap(),
                    "{action}, configured={configured}, enable={enable}"
                );
                assert_eq!(controller.product_configs().unwrap(), before);
                assert_eq!(controller.product_metadata("41").unwrap(), None);
            }
        }
    }

    #[test]
    fn catalog_revision_is_persistent_and_failed_import_does_not_advance_it() {
        let path = super::super::tests::TempDatabase::new();
        let mut db = Storage::open(&path.0).unwrap();
        assert_eq!(db.catalog_epoch().unwrap(), 0);
        assert!(db
            .import_configuration(
                &MonitorConfig::default(),
                vec![],
                vec![("bad".into(), "bad".into(), "missing".into(), vec![])],
                1
            )
            .is_err());
        assert_eq!(db.catalog_epoch().unwrap(), 0);
        db.import_configuration(&MonitorConfig::default(), vec![], vec![], 1)
            .unwrap();
        assert_eq!(db.catalog_epoch().unwrap(), 1);
        db.restore_defaults(&MonitorConfig::default(), false)
            .unwrap();
        assert_eq!(db.catalog_epoch().unwrap(), 2);
        db.remove_product("41").unwrap();
        assert_eq!(db.catalog_epoch().unwrap(), 3);
        drop(db);
        let db = Storage::open(&path.0).unwrap();
        assert_eq!(db.catalog_epoch().unwrap(), 3);
        let rows: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM catalog_lifecycle", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
    }
}
