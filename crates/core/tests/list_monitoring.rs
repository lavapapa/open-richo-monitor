use ricoh_monitor_core::{
    app::{MonitorApp, MonitoringAction},
    availability::Availability,
    config::{MonitorConfig, MonitoringMode},
    ricoh::ProductDetail,
    ricoh_api::RicohApiError,
    scheduler::{LineId, SchedulerClient},
    storage::{ProductIdentity, Storage},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

type FutureResult<T> = Pin<Box<dyn Future<Output = Result<T, RicohApiError>> + Send>>;
struct Pages {
    pages: Mutex<Vec<Result<Vec<ProductDetail>, RicohApiError>>>,
    requests: AtomicUsize,
    response_limit: AtomicUsize,
    next_round_waiting: tokio::sync::Notify,
    details: AtomicUsize,
    detail_stock: AtomicUsize,
}
fn detail(id: u64, stock: u64) -> ProductDetail {
    ProductDetail {
        product_id: id,
        name: format!("商品{id}"),
        is_show: 1,
        stock: stock.into(),
        availability: if stock > 0 {
            Availability::InStock
        } else {
            Availability::OutOfStock
        },
        metadata: Default::default(),
    }
}
impl SchedulerClient for Pages {
    fn product_detail(&self, line: LineId) -> FutureResult<ProductDetail> {
        self.details.fetch_add(1, Ordering::Relaxed);
        let stock = self.detail_stock.load(Ordering::Relaxed) as u64;
        Box::pin(async move { Ok(detail(line.product_id, stock)) })
    }
    fn product_page(
        &self,
        _outlet: String,
        page: u32,
        limit: u32,
    ) -> FutureResult<Vec<ProductDetail>> {
        assert_eq!(limit, 20);
        let request = self.requests.fetch_add(1, Ordering::Relaxed) + 1;
        if request > self.response_limit.load(Ordering::Relaxed) {
            self.next_round_waiting.notify_one();
            return Box::pin(std::future::pending());
        }
        let result = self
            .pages
            .lock()
            .unwrap()
            .get((page - 1) as usize)
            .cloned()
            .unwrap_or(Ok(vec![]));
        Box::pin(async move { result })
    }
}
struct Directory(std::path::PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
static NEXT: AtomicUsize = AtomicUsize::new(0);
async fn prepare(
    ids: &[u64],
    pages: Vec<Result<Vec<ProductDetail>, RicohApiError>>,
) -> (Directory, MonitorApp, Arc<Pages>) {
    let directory = Directory(std::env::temp_dir().join(format!(
        "ricoh-list-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir_all(&directory.0).unwrap();
    let mut db = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
    let mut config = MonitorConfig::default();
    config.schedule.start_minute = 0;
    config.schedule.end_minute = 0;
    config.requests.interval = Duration::from_millis(100);
    config.requests.jitter_percent = 0.0;
    config.requests.global_requests_per_second = 1000.0;
    db.save_monitor_config(&config).unwrap();
    for id in ids {
        db.save_product_config(
            ProductIdentity {
                key: id.to_string(),
                product_id: id.to_string(),
                sku_id: None,
            },
            format!("商品{id}"),
            "fixture".into(),
            Some(1),
        )
        .unwrap();
        db.set_product_enabled(&id.to_string(), true).unwrap();
    }
    drop(db);
    let client = Arc::new(Pages {
        pages: Mutex::new(pages),
        requests: AtomicUsize::new(0),
        response_limit: AtomicUsize::new(usize::MAX),
        next_round_waiting: tokio::sync::Notify::new(),
        details: AtomicUsize::new(0),
        detail_stock: AtomicUsize::new(7),
    });
    let app = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
    app.complete_setup().await.unwrap();
    app.monitoring_action(MonitoringAction::Start)
        .await
        .unwrap();
    (directory, app, client)
}
async fn wait_checks(app: &MonitorApp, count: u64) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if app
                .snapshot()
                .await
                .unwrap()
                .products
                .iter()
                .filter(|p| p.enabled)
                .all(|p| p.today_check_count >= count)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn both_modes_emit_one_prominent_alert_for_a_zero_stock_listing_without_notifications() {
    for mode in [
        MonitoringMode::ListedProducts,
        MonitoringMode::ProductDetail,
    ] {
        let (_directory, app, client) =
            prepare(&[65], vec![Ok(vec![detail(65, 0)]), Ok(vec![])]).await;
        client.detail_stock.store(0, Ordering::Relaxed);
        let mut config = app.snapshot().await.unwrap().config;
        config.monitoring_mode = mode;
        app.save_config(config).await.unwrap();
        app.set_system_notifications_enabled(false).await.unwrap();
        app.set_product_prominent_alert(65, true).await.unwrap();
        let runner = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        tokio::time::timeout(Duration::from_secs(4), app.wait_for_prominent_alert())
            .await
            .expect("两种监控方案均在真实入队后唤醒突出提醒消费者");
        let alert = app.claim_prominent_alert().await.unwrap().unwrap();
        assert_eq!(alert.stock, 0.0);
        assert_eq!(alert.product_id, "65");
        app.acknowledge_prominent_alert(alert.event_id)
            .await
            .unwrap();
        wait_checks(&app, 2).await;
        assert!(app.claim_prominent_alert().await.unwrap().is_none());
        assert!(app.claim_system_notification().await.unwrap().is_none());
        assert_eq!(
            app.snapshot()
                .await
                .unwrap()
                .recent_events
                .iter()
                .filter(|event| event.kind == "stock_available")
                .count(),
            1
        );
        app.shutdown();
        runner.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn both_modes_deliver_stock_increase_to_prominent_alerts() {
    for mode in [
        MonitoringMode::ListedProducts,
        MonitoringMode::ProductDetail,
    ] {
        let (_directory, app, client) =
            prepare(&[65], vec![Ok(vec![detail(65, 3)]), Ok(vec![])]).await;
        client.detail_stock.store(3, Ordering::Relaxed);
        let mut config = app.snapshot().await.unwrap().config;
        config.monitoring_mode = mode;
        app.save_config(config).await.unwrap();
        app.set_system_notifications_enabled(false).await.unwrap();
        app.set_product_prominent_alert(65, true).await.unwrap();
        let runner = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        wait_checks(&app, 2).await;
        let first = app.claim_prominent_alert().await.unwrap().unwrap();
        assert_eq!(first.stock, 3.0);
        app.acknowledge_prominent_alert(first.event_id)
            .await
            .unwrap();
        client.detail_stock.store(5, Ordering::Relaxed);
        *client.pages.lock().unwrap() = vec![Ok(vec![detail(65, 5)]), Ok(vec![])];
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let snapshot = app.snapshot().await.unwrap();
                if snapshot
                    .products
                    .iter()
                    .find(|p| p.product_id == "65")
                    .and_then(|p| p.observation.as_ref())
                    .is_some_and(|o| o.stock == Some(5.0))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let increased = app.claim_prominent_alert().await.unwrap().unwrap();
        assert_eq!(increased.stock, 5.0);
        assert!(increased.event_id > first.event_id);
        app.acknowledge_prominent_alert(increased.event_id)
            .await
            .unwrap();
        wait_checks(&app, 4).await;
        assert!(app.claim_prominent_alert().await.unwrap().is_none());
        assert!(app.claim_system_notification().await.unwrap().is_none());
        let history = app
            .query_messages(ricoh_monitor_core::app::MessageQuery {
                date: None,
                product_id: Some("65".into()),
                cursor: None,
                limit: 100,
            })
            .await
            .unwrap();
        assert_eq!(
            history
                .items
                .iter()
                .filter(|item| item.detail == "库存增加 3 → 5")
                .count(),
            1
        );
        app.shutdown();
        runner.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn one_complete_list_checks_all_selected_and_keeps_unknown_stock_distinct() {
    let (_directory, app, client) = prepare(
        &[1, 2, 3],
        vec![Ok(vec![detail(1, 4), detail(99, 3)]), Ok(vec![])],
    )
    .await;
    // 首轮返回两页后，后续请求保持在途，暂停不依赖主机调度速度。
    client.response_limit.store(2, Ordering::Relaxed);
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    wait_checks(&app, 1).await;
    tokio::time::timeout(Duration::from_secs(4), client.next_round_waiting.notified())
        .await
        .unwrap();
    app.monitoring_action(MonitoringAction::Pause)
        .await
        .unwrap();
    let snapshot = app.snapshot().await.unwrap();
    let selected = snapshot
        .products
        .iter()
        .filter(|p| p.enabled)
        .collect::<Vec<_>>();
    assert_eq!(selected.len(), 3);
    assert_eq!(client.details.load(Ordering::Relaxed), 0);
    assert_eq!(client.requests.load(Ordering::Relaxed), 3);
    for product in &selected {
        assert_eq!(product.today_check_count, 1);
        assert_eq!(product.today_success_count, 1);
    }
    assert_eq!(selected[0].observation.as_ref().unwrap().stock, Some(4.0));
    for p in &selected[1..] {
        assert_eq!(p.observation.as_ref().unwrap().stock, None);
        assert_eq!(p.observation.as_ref().unwrap().is_show, 0);
    }
    assert!(!snapshot.products.iter().any(|p| p.product_id == "99"));
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn later_page_failure_publishes_no_partial_success_or_false_absence() {
    let (_directory, app, client) = prepare(
        &[1, 2],
        vec![Ok(vec![detail(1, 4)]), Err(RicohApiError::Forbidden)],
    )
    .await;
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    wait_checks(&app, 1).await;
    let snapshot = app.snapshot().await.unwrap();
    for p in snapshot.products.iter().filter(|p| p.enabled) {
        assert!(p.observation.is_none());
        assert!(p.runtime_error.is_some());
        assert_eq!(p.today_success_count, 0);
    }
    *client.pages.lock().unwrap() = vec![Ok(vec![detail(1, 4)]), Ok(vec![])];
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let s = app.snapshot().await.unwrap();
            if s.products
                .iter()
                .filter(|p| p.enabled)
                .all(|p| p.observation.is_some() && p.runtime_error.is_none())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn switching_modes_changes_requests_and_preserves_statistics() {
    let (_directory, app, client) =
        prepare(&[1, 2], vec![Ok(vec![detail(1, 4)]), Ok(vec![])]).await;
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    wait_checks(&app, 1).await;
    let mut config = app.snapshot().await.unwrap().config;
    config.monitoring_mode = MonitoringMode::ProductDetail;
    config.rate.interval_min_ms = 100;
    config.rate.interval_max_ms = 100;
    app.save_config(config.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let s = app.snapshot().await.unwrap();
            if s.products
                .iter()
                .filter(|p| p.enabled)
                .all(|p| p.observation.as_ref().is_some_and(|o| o.stock == Some(7.0)))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(client.details.load(Ordering::Relaxed) >= 2);
    config.monitoring_mode = MonitoringMode::ListedProducts;
    app.save_config(config).await.unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let s = app.snapshot().await.unwrap();
            if s.products.iter().any(|p| {
                p.product_id == "2" && p.observation.as_ref().is_some_and(|o| o.stock.is_none())
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn list_failure_counts_once_for_outlet_and_parse_error_does_not_disable_it() {
    for (error, expected_failures) in [
        (RicohApiError::Transport("本地连接失败 fixture".into()), 1),
        (
            RicohApiError::Parse(ricoh_monitor_core::ricoh::RicohParseError::InvalidValue {
                field: "productId",
                expected: "完整列表中商品 ID 唯一",
            }),
            0,
        ),
    ] {
        let (directory, app, client) = prepare(&[1, 2, 3, 4, 5], vec![Err(error)]).await;
        let mut config = app.snapshot().await.unwrap().config;
        config.use_proxy_pool = true;
        config.rate.interval_min_ms = 30_000;
        config.rate.interval_max_ms = 30_000;
        app.save_config(config).await.unwrap();
        let mut db = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        db.save_proxy(&ricoh_monitor_core::storage::StoredProxy {
            id: "fixture-outlet".into(),
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
        let runner = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        wait_checks(&app, 1).await;
        app.monitoring_action(MonitoringAction::Pause)
            .await
            .unwrap();
        let proxy = db.proxies().unwrap().remove(0);
        assert_eq!(client.requests.load(Ordering::Relaxed), 1);
        assert_eq!(proxy.consecutive_failures, expected_failures);
        assert_eq!(proxy.status, "available");
        app.shutdown();
        runner.await.unwrap().unwrap();
    }
}
