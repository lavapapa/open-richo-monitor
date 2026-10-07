use super::{Storage, StorageError};
use rusqlite::params;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryCursor {
    pub at_ms: i64,
    pub source: i64,
    pub id: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryItem {
    pub cursor: HistoryCursor,
    pub product_id: String,
    pub name: String,
    pub is_show: Option<u8>,
    pub stock: Option<f64>,
    pub kind: String,
    pub detail: String,
}

pub struct HistoryPage {
    pub items: Vec<HistoryItem>,
    pub next_cursor: Option<HistoryCursor>,
}

impl Storage {
    pub fn query_history(
        &self,
        from_ms: i64,
        to_ms: i64,
        product_id: Option<&str>,
        cursor: Option<HistoryCursor>,
        limit: usize,
    ) -> Result<HistoryPage, StorageError> {
        let limit = limit.clamp(1, 100);
        let mut statement = self.conn.prepare(
            "SELECT at_ms, source, id, product_id, name, is_show, stock, kind, previous_show, previous_stock, previous_id FROM (
                SELECT r.first_at_ms AS at_ms, 1 AS source, r.id, p.product_id,
                       COALESCE(r.product_name,p.name) AS name, r.is_show,
                       CASE WHEN EXISTS (SELECT 1 FROM check_run_stock_absences a WHERE a.check_run_id=r.id)
                            THEN NULL ELSE r.stock END AS stock,
                       'state_change' AS kind, prev.is_show AS previous_show,
                       CASE WHEN EXISTS (SELECT 1 FROM check_run_stock_absences a WHERE a.check_run_id=prev.id)
                            THEN NULL ELSE prev.stock END AS previous_stock, prev.id AS previous_id
                FROM check_runs r JOIN products p ON p.product_key=r.product_key
                LEFT JOIN check_runs prev ON prev.id=(SELECT id FROM check_runs
                    WHERE product_key=r.product_key AND id<r.id ORDER BY id DESC LIMIT 1)
                WHERE r.first_at_ms>=?1 AND r.first_at_ms<?2 AND (?3 IS NULL OR p.product_id=?3)
                UNION ALL
                SELECT e.observed_at_ms AS at_ms, 0 AS source, e.id, p.product_id,
                       p.name,
                       CASE WHEN e.event_kind IN ('monitoring_failed','recovered') THEN NULL ELSE 1 END AS is_show,
                       CASE WHEN e.event_kind IN ('monitoring_failed','recovered') THEN NULL ELSE e.stock END AS stock,
                       e.event_kind AS kind,
                       NULL AS previous_show, NULL AS previous_stock, NULL AS previous_id
                FROM events e JOIN products p ON p.product_key=e.product_key
                WHERE e.observed_at_ms>=?1 AND e.observed_at_ms<?2 AND (?3 IS NULL OR p.product_id=?3)
                  AND (e.event_kind IN ('monitoring_failed','recovered') OR NOT EXISTS (
                       SELECT 1 FROM check_runs r WHERE r.product_key=e.product_key
                         AND r.first_at_ms=e.observed_at_ms))
             ) WHERE ?4 IS NULL OR at_ms<?4 OR
                 (at_ms=?4 AND (source<?5 OR (source=?5 AND id<?6)))
             ORDER BY at_ms DESC, source DESC, id DESC LIMIT ?7",
        )?;
        let rows = statement.query_map(
            params![
                from_ms,
                to_ms,
                product_id,
                cursor.map(|c| c.at_ms),
                cursor.map(|c| c.source),
                cursor.map(|c| c.id),
                (limit + 1) as i64
            ],
            |row| {
                let is_show = row.get(5)?;
                let stock = row.get(6)?;
                let kind: String = row.get(7)?;
                let detail = history_detail(
                    &kind,
                    is_show,
                    stock,
                    row.get(8)?,
                    row.get(9)?,
                    row.get::<_, Option<i64>>(10)?.is_some(),
                );
                Ok(HistoryItem {
                    cursor: HistoryCursor {
                        at_ms: row.get(0)?,
                        source: row.get(1)?,
                        id: row.get(2)?,
                    },
                    product_id: row.get(3)?,
                    name: row.get(4)?,
                    is_show,
                    stock,
                    kind,
                    detail,
                })
            },
        )?;
        let mut items = rows.collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if items.len() > limit {
            items.truncate(limit);
            items.last().map(|item| item.cursor)
        } else {
            None
        };
        Ok(HistoryPage { items, next_cursor })
    }
}

fn history_detail(
    kind: &str,
    listed: Option<u8>,
    stock: Option<f64>,
    previous_listed: Option<u8>,
    previous_stock: Option<f64>,
    has_previous: bool,
) -> String {
    match kind {
        "monitoring_failed" => return "持续检查失败".into(),
        "recovered" => return "检查恢复正常".into(),
        "first_observed_in_stock" => {
            return format!(
                "首次发现{} · 库存 {}",
                if stock == Some(0.0) {
                    "上架"
                } else {
                    "有货"
                },
                stock.unwrap_or(0.0)
            )
        }
        "out_of_stock_to_in_stock" => {
            return format!(
                "{} · 库存 {}",
                if stock == Some(0.0) {
                    "商品上架"
                } else {
                    "商品有货"
                },
                stock.unwrap_or(0.0)
            )
        }
        _ => {}
    }
    let mut changes = Vec::new();
    if has_previous {
        if listed != previous_listed {
            changes.push(
                if listed == Some(1) {
                    "商品上架"
                } else {
                    "商品下架"
                }
                .into(),
            );
        }
        if stock != previous_stock {
            changes.push(match (previous_stock, stock) {
                (Some(before), Some(after)) => format!(
                    "{} {before} → {after}",
                    if after > before {
                        "补货"
                    } else {
                        "库存减少"
                    }
                ),
                (None, Some(after)) => format!("库存已获取：{after}"),
                (_, None) => "接口未提供库存".into(),
            });
        }
    }
    if changes.is_empty() {
        changes.push(format!(
            "检查结果：{}",
            if listed == Some(1) {
                "已上架"
            } else {
                "未上架"
            }
        ));
        if let Some(stock) = stock {
            changes.push(format!("库存 {stock}"));
        }
    }
    changes.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_describe_listing_and_stock_changes_across_pages() {
        let storage = Storage::open_in_memory().unwrap();
        storage.conn.execute("INSERT INTO products(product_key,product_id,name,source,is_configured,enabled) VALUES ('a','1','A','manual',1,0)", []).unwrap();
        for (at, listed, stock) in [
            (1000, 0, 0),
            (2000, 1, 3),
            (3000, 1, 5),
            (4000, 1, 2),
            (5000, 0, 0),
        ] {
            storage.conn.execute("INSERT INTO check_runs(product_key,product_name,availability,is_show,stock,checks,first_at_ms,last_at_ms) VALUES ('a','A','out_of_stock',?1,?2,1,?3,?3)", params![listed,stock,at]).unwrap();
        }
        let page = storage
            .query_history(3000, 6000, Some("1"), None, 2)
            .unwrap();
        assert_eq!(page.items[0].detail, "商品下架 · 库存减少 2 → 0");
        assert_eq!(page.items[1].detail, "库存减少 5 → 2");
        let next = storage
            .query_history(3000, 6000, Some("1"), page.next_cursor, 2)
            .unwrap();
        assert_eq!(next.items[0].detail, "补货 3 → 5");
        let all = storage.query_history(0, 6000, None, None, 100).unwrap();
        assert_eq!(all.items[3].detail, "商品上架 · 补货 0 → 3");
        assert_eq!(all.items[4].detail, "检查结果：未上架 · 库存 0");
    }

    #[test]
    fn filters_before_pagination_and_keeps_equal_timestamps_stable() {
        let storage = Storage::open_in_memory().unwrap();
        storage.conn.execute("INSERT INTO products(product_key,product_id,name,source,is_configured,enabled) VALUES ('a','1','A','manual',1,0),('b','2','B','manual',1,0)", []).unwrap();
        for id in 1..=105 {
            storage.conn.execute("INSERT INTO check_runs(id,product_key,product_name,availability,is_show,stock,checks,first_at_ms,last_at_ms) VALUES (?1,CASE WHEN ?1 <= 100 THEN 'a' ELSE 'b' END,'B','out_of_stock',0,0,1,1000,1000)", [id]).unwrap();
        }
        let first = storage.query_history(0, 2000, Some("2"), None, 3).unwrap();
        assert_eq!(first.items.len(), 3);
        assert_eq!(first.items[0].cursor.id, 105);
        let second = storage
            .query_history(0, 2000, Some("2"), first.next_cursor, 3)
            .unwrap();
        assert_eq!(
            second
                .items
                .iter()
                .map(|item| item.cursor.id)
                .collect::<Vec<_>>(),
            vec![102, 101]
        );
    }

    #[test]
    fn date_bounds_and_duplicate_stock_notice_apply_to_both_sources() {
        let storage = Storage::open_in_memory().unwrap();
        storage.conn.execute("INSERT INTO products(product_key,product_id,name,source,is_configured,enabled) VALUES ('a','1','A','manual',1,0)", []).unwrap();
        storage.conn.execute("INSERT INTO check_runs(product_key,product_name,availability,is_show,stock,checks,first_at_ms,last_at_ms) VALUES ('a','A','in_stock',1,2,1,1000,1000)", []).unwrap();
        storage.conn.execute("INSERT INTO events(product_key,event_kind,stock,request_sequence,observed_at_ms) VALUES ('a','first_observed_in_stock',2,1,1000),('a','monitoring_failed',0,2,1000),('a','recovered',0,3,2000)", []).unwrap();
        let page = storage.query_history(1000, 2000, None, None, 100).unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|item| item.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["state_change", "monitoring_failed"]
        );
        assert_eq!(
            storage
                .query_history(2000, 3000, None, None, 100)
                .unwrap()
                .items[0]
                .kind,
            "recovered"
        );
    }

    #[test]
    fn clearing_between_pages_returns_empty_and_names_remain_historical() {
        let mut storage = Storage::open_in_memory().unwrap();
        storage.conn.execute("INSERT INTO products(product_key,product_id,name,source,is_configured,enabled) VALUES ('a','1','新名称','manual',1,0)", []).unwrap();
        storage.conn.execute("INSERT INTO check_runs(product_key,product_name,availability,is_show,stock,checks,first_at_ms,last_at_ms) VALUES ('a','旧名称','out_of_stock',0,0,1,2000,2000),('a','旧名称','in_stock',1,1,1,1000,1000)", []).unwrap();
        let first = storage.query_history(0, 3000, None, None, 1).unwrap();
        assert_eq!(first.items[0].name, "旧名称");
        storage.clear_history().unwrap();
        assert!(storage
            .query_history(0, 3000, None, first.next_cursor, 1)
            .unwrap()
            .items
            .is_empty());
    }
}
