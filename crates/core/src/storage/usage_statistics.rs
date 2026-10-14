use super::{Storage, StorageError};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::Connection;

pub(super) fn initialize(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_statistics (
        id INTEGER PRIMARY KEY CHECK(id=1), installation_id TEXT
    ); INSERT OR IGNORE INTO usage_statistics(id) VALUES(1);",
    )?;
    if !super::column_exists(conn, "usage_statistics", "prompt_pending")? {
        conn.execute("ALTER TABLE usage_statistics ADD COLUMN prompt_pending INTEGER NOT NULL DEFAULT 0", [])?;
        conn.execute("UPDATE usage_statistics SET prompt_pending=installation_id IS NULL AND EXISTS(SELECT 1 FROM settings WHERE setup_confirmed=1)", [])?;
    }
    if !super::column_exists(conn, "usage_statistics", "choice_recorded")? {
        conn.execute("ALTER TABLE usage_statistics ADD COLUMN choice_recorded INTEGER NOT NULL DEFAULT 0", [])?;
        conn.execute("UPDATE usage_statistics SET choice_recorded=installation_id IS NOT NULL OR (prompt_pending=0 AND EXISTS(SELECT 1 FROM settings WHERE setup_confirmed=1))", [])?;
    }
    Ok(())
}

impl Storage {
    pub fn usage_statistics_choice_recorded(&self) -> Result<bool, StorageError> {
        Ok(self.conn.query_row("SELECT choice_recorded FROM usage_statistics WHERE id=1", [], |row| row.get(0))?)
    }
    pub fn usage_statistics_intro_pending(&self) -> Result<bool, StorageError> {
        Ok(self.conn.query_row("SELECT prompt_pending FROM usage_statistics WHERE id=1", [], |row| row.get(0))?)
    }
    pub fn usage_statistics_identity(&self) -> Result<Option<String>, StorageError> {
        Ok(self.conn.query_row(
            "SELECT installation_id FROM usage_statistics WHERE id=1",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn set_usage_statistics_enabled(&mut self, enabled: bool) -> Result<(), StorageError> {
        let identity = if enabled {
            match self.usage_statistics_identity()? {
                Some(existing) => Some(existing),
                None => {
                    let mut bytes = [0u8; 16];
                    SystemRandom::new().fill(&mut bytes).map_err(|_| {
                        StorageError::Io(std::io::Error::other("无法生成统计安装标识"))
                    })?;
                    Some(
                        bytes
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>(),
                    )
                }
            }
        } else {
            None
        };
        self.conn.execute(
            "UPDATE usage_statistics SET installation_id=?1, prompt_pending=0, choice_recorded=1 WHERE id=1",
            [identity],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrade_asks_once_and_new_install_uses_onboarding() {
        let mut db = Storage::open_in_memory().unwrap();
        assert!(!db.usage_statistics_intro_pending().unwrap());
        db.conn.execute("UPDATE settings SET setup_confirmed=1", []).unwrap();
        // 模拟已使用正式版本的数据库，尚未包含统计模块。
        db.conn.execute("DROP TABLE usage_statistics", []).unwrap();
        initialize(&db.conn).unwrap();
        assert!(db.usage_statistics_intro_pending().unwrap());
        assert_eq!(db.usage_statistics_identity().unwrap(), None);
        assert!(!db.usage_statistics_choice_recorded().unwrap());
        db.set_usage_statistics_enabled(false).unwrap();
        initialize(&db.conn).unwrap();
        assert!(!db.usage_statistics_intro_pending().unwrap());
        assert_eq!(db.usage_statistics_identity().unwrap(), None);
        assert!(db.usage_statistics_choice_recorded().unwrap());
        db.set_usage_statistics_enabled(true).unwrap();
        assert!(db.usage_statistics_choice_recorded().unwrap());
        assert!(!db.usage_statistics_intro_pending().unwrap());
        assert!(db.usage_statistics_identity().unwrap().is_some());
    }

    #[test]
    fn usage_consent_is_optional_persistent_and_resettable() {
        let path =
            std::env::temp_dir().join(format!("richomonitor-usage-{}.sqlite3", std::process::id()));
        let mut db = Storage::open(&path).unwrap();
        assert_eq!(db.usage_statistics_identity().unwrap(), None);
        db.set_usage_statistics_enabled(true).unwrap();
        let identity = db.usage_statistics_identity().unwrap().unwrap();
        db.set_usage_statistics_enabled(true).unwrap();
        assert_eq!(db.usage_statistics_identity().unwrap().unwrap(), identity);
        drop(db);
        let mut db = Storage::open(&path).unwrap();
        assert_eq!(db.usage_statistics_identity().unwrap().unwrap(), identity);
        db.set_usage_statistics_enabled(false).unwrap();
        assert_eq!(db.usage_statistics_identity().unwrap(), None);
        assert!(db.usage_statistics_choice_recorded().unwrap());
        drop(db);
        let mut db = Storage::open(&path).unwrap();
        assert!(db.usage_statistics_choice_recorded().unwrap());
        assert_eq!(db.usage_statistics_identity().unwrap(), None);
        db.set_usage_statistics_enabled(true).unwrap();
        assert_ne!(db.usage_statistics_identity().unwrap().unwrap(), identity);
        db.restore_defaults(&crate::config::MonitorConfig::default(), false)
            .unwrap();
        assert_eq!(db.usage_statistics_identity().unwrap(), None);
        assert!(db.usage_statistics_choice_recorded().unwrap());
        drop(db);
        std::fs::remove_file(path).unwrap();
    }
}
