use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use ricoh_monitor_core::{
    app::{AppConfig, MonitorApp, MonitoringAction, RuntimeState, ScanAction, ScanStatus},
    availability::Availability,
    config::MonitorConfig,
    ricoh::ProductDetail,
    ricoh_api::RicohApiError,
    scheduler::{LineId, SchedulerClient},
    storage::{ProductIdentity, RunIntent, Storage},
};
use rusqlite::Connection;

struct TestDirectory(PathBuf);

static NEXT_DIRECTORY_ID: AtomicUsize = AtomicUsize::new(0);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ricoh-shared-app-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct FixtureClient(AtomicUsize);

impl SchedulerClient for FixtureClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send + 'static>> {
        let call = self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            match call {
                0 | 1 => Ok(detail(line.product_id, 1, 4.0)),
                2 | 3 => Ok(detail(line.product_id, 1, 0.0)),
                _ => Err(RicohApiError::from(
                    reqwest::Client::new().get("not a url").build().unwrap_err(),
                )),
            }
        })
    }
}

struct SlowFirstClient {
    first_started: tokio::sync::watch::Sender<bool>,
    release_first: Arc<tokio::sync::Notify>,
    first_calls: AtomicUsize,
    second_calls: AtomicUsize,
}

struct ScanClient(AtomicUsize);

struct AlternatingStockClient(AtomicUsize);

impl SchedulerClient for AlternatingStockClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send + 'static>> {
        let call = self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            if call % 2 == 0 {
                Ok(detail(line.product_id, 1, 1.0))
            } else {
                Ok(detail(line.product_id, 0, 0.0))
            }
        })
    }
}

impl SchedulerClient for ScanClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send + 'static>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move { Ok(detail(line.product_id, 1, 3.0)) })
    }
}

impl SchedulerClient for SlowFirstClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send + 'static>> {
        let release_first = self.release_first.clone();
        if line.product_id == 246 {
            self.first_calls.fetch_add(1, Ordering::Relaxed);
            self.first_started.send_replace(true);
            Box::pin(async move {
                release_first.notified().await;
                Ok(detail(line.product_id, 1, 2.0))
            })
        } else {
            self.second_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move { Ok(detail(line.product_id, 1, 7.0)) })
        }
    }
}

fn detail(product_id: u64, is_show: u8, stock: f64) -> ProductDetail {
    ProductDetail {
        product_id,
        name: format!("本地样例商品 {product_id}"),
        is_show,
        stock: serde_json::Number::from_f64(stock).unwrap(),
        metadata: Default::default(),
        availability: if is_show == 1 && stock > 0.0 {
            Availability::InStock
        } else {
            Availability::OutOfStock
        },
    }
}

async fn wait_for(
    app: &MonitorApp,
    matches: impl Fn(&ricoh_monitor_core::app::AppSnapshot) -> bool,
) -> ricoh_monitor_core::app::AppSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = app.snapshot().await.unwrap();
            if matches(&snapshot) {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("本地监控流程在时限内完成")
}

#[tokio::test]
async fn shared_app_runs_transitions_pauses_resumes_stops_and_keeps_last_observation() {
    let directory = TestDirectory::new();
    {
        let mut storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "246".into(),
                    product_id: "246".into(),
                    sku_id: None,
                },
                "本地样例商品 246".into(),
                "fixture".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("246", true).unwrap();
        assert_eq!(storage.run_intent().unwrap(), RunIntent::Stopped);
    }

    let client = Arc::new(FixtureClient(AtomicUsize::new(0)));
    let app = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
    app.set_platform_notifications_available(true);
    let mut config = AppConfig::default();
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.schedule.start = "00:00".into();
    config.schedule.end = "23:59".into();
    config.rate.interval_min_ms = 100;
    config.rate.interval_max_ms = 100;
    app.save_config(config).await.unwrap();

    app.complete_setup().await.unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });

    let first = wait_for(&app, |snapshot| {
        snapshot.runtime.state == RuntimeState::Monitoring
            && snapshot
                .products
                .first()
                .and_then(|product| product.observation.as_ref())
                .is_some_and(|observation| observation.availability == "in_stock")
    })
    .await;
    assert!(first.setup_completed);
    wait_for(&app, |snapshot| snapshot.products[0].check_count >= 2).await;
    let repeated_stock = app.snapshot().await.unwrap();
    assert_eq!(repeated_stock.recent_events.len(), 1);

    app.monitoring_action(MonitoringAction::Pause)
        .await
        .unwrap();
    assert_eq!(
        app.snapshot().await.unwrap().runtime.state,
        RuntimeState::Paused
    );
    tokio::time::sleep(Duration::from_millis(450)).await;
    let calls_while_paused = client.0.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(client.0.load(Ordering::Relaxed), calls_while_paused);

    app.monitoring_action(MonitoringAction::Resume)
        .await
        .unwrap();
    wait_for(&app, |snapshot| {
        snapshot
            .products
            .first()
            .and_then(|product| product.observation.as_ref())
            .is_some_and(|observation| observation.availability == "out_of_stock")
    })
    .await;
    let failed_request = wait_for(&app, |snapshot| {
        snapshot.products[0].runtime_error.is_some()
    })
    .await;
    let product = failed_request.products.first().unwrap();
    assert_eq!(
        product.observation.as_ref().unwrap().availability,
        "out_of_stock"
    );
    assert!(product.runtime_error.is_some());
    assert!(product.check_count >= 4, "失败的请求也应计入检查次数");
    assert!(
        failed_request.runtime.last_error.is_some(),
        "请求错误应显示，同时保留最近一次有效库存"
    );

    app.monitoring_action(MonitoringAction::Stop).await.unwrap();
    assert_eq!(
        app.snapshot().await.unwrap().runtime.state,
        RuntimeState::Stopped
    );
    app.shutdown();
    runner.await.unwrap().unwrap();
    drop(app);

    let reopened = MonitorApp::open(&directory.0).unwrap();
    let after_restart = reopened.snapshot().await.unwrap();
    assert_eq!(after_restart.runtime.state, RuntimeState::Stopped);
    assert_eq!(
        after_restart.products[0]
            .observation
            .as_ref()
            .unwrap()
            .availability,
        "out_of_stock"
    );
}

#[tokio::test]
async fn paused_restart_uses_enabled_persisted_observations_for_last_success() {
    let directory = TestDirectory::new();
    {
        let mut storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "246".into(),
                    product_id: "246".into(),
                    sku_id: None,
                },
                "本地样例商品 246".into(),
                "fixture".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("246", true).unwrap();
    }

    let app = MonitorApp::open_with_scheduler_client(
        &directory.0,
        Arc::new(ScanClient(AtomicUsize::new(0))),
    )
    .unwrap();
    assert_eq!(app.snapshot().await.unwrap().runtime.last_success_at, None);
    let mut config = AppConfig::default();
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.schedule.start = "00:00".into();
    config.schedule.end = "00:00".into();
    app.save_config(config).await.unwrap();
    app.complete_setup().await.unwrap();
    app.monitoring_action(MonitoringAction::Start)
        .await
        .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    wait_for(&app, |snapshot| snapshot.products[0].observation.is_some()).await;
    app.monitoring_action(MonitoringAction::Pause)
        .await
        .unwrap();
    let checked_at = app.snapshot().await.unwrap().products[0]
        .observation
        .as_ref()
        .unwrap()
        .checked_at
        .clone();
    app.shutdown();
    runner.await.unwrap().unwrap();
    drop(app);

    let reopened = MonitorApp::open(&directory.0).unwrap();
    let snapshot = reopened.snapshot().await.unwrap();
    assert_eq!(snapshot.runtime.state, RuntimeState::Paused);
    assert_eq!(snapshot.runtime.last_success_at, checked_at);

    reopened.set_product_enabled(246, false).await.unwrap();
    let disabled = reopened.snapshot().await.unwrap();
    assert_eq!(disabled.runtime.last_success_at, None);
    reopened.set_product_enabled(246, true).await.unwrap();
    assert_eq!(
        reopened.snapshot().await.unwrap().runtime.last_success_at,
        checked_at
    );
}

#[tokio::test]
async fn shared_app_updates_fast_product_while_another_waits_and_cancels_on_run_abort() {
    let directory = TestDirectory::new();
    {
        let mut storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        for product_id in [246, 247] {
            let key = product_id.to_string();
            storage
                .save_product_config(
                    ProductIdentity {
                        key: key.clone(),
                        product_id: key.clone(),
                        sku_id: None,
                    },
                    format!("本地样例商品 {product_id}"),
                    "fixture".into(),
                    Some(1),
                )
                .unwrap();
            storage.set_product_enabled(&key, true).unwrap();
        }
    }

    let (first_started, mut started) = tokio::sync::watch::channel(false);
    let client = Arc::new(SlowFirstClient {
        first_started,
        release_first: Arc::new(tokio::sync::Notify::new()),
        first_calls: AtomicUsize::new(0),
        second_calls: AtomicUsize::new(0),
    });
    let app = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
    app.set_platform_notifications_available(true);
    let mut config = AppConfig::default();
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.schedule.start = "00:00".into();
    config.schedule.end = "23:59".into();
    config.rate.interval_min_ms = 100;
    config.rate.interval_max_ms = 100;
    app.save_config(config).await.unwrap();
    app.complete_setup().await.unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        while !*started.borrow() {
            started.changed().await.unwrap();
        }
    })
    .await
    .expect("第一件商品请求进入等待状态");
    wait_for(&app, |snapshot| {
        snapshot.products.iter().any(|product| {
            product.product_id == "247"
                && product
                    .observation
                    .as_ref()
                    .is_some_and(|observation| observation.availability == "in_stock")
        })
    })
    .await;
    wait_for(&app, |_| client.second_calls.load(Ordering::Relaxed) >= 2).await;
    let while_first_waits = app.snapshot().await.unwrap();
    assert!(while_first_waits
        .products
        .iter()
        .any(|product| { product.product_id == "246" && product.observation.is_none() }));
    assert!(while_first_waits.products.iter().any(|product| {
        product.product_id == "247"
            && product
                .observation
                .as_ref()
                .is_some_and(|observation| observation.stock == Some(7.0))
    }));

    runner.abort();
    assert!(runner.await.unwrap_err().is_cancelled());
    let stopped_counts = (
        client.first_calls.load(Ordering::Relaxed),
        client.second_calls.load(Ordering::Relaxed),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        stopped_counts,
        (
            client.first_calls.load(Ordering::Relaxed),
            client.second_calls.load(Ordering::Relaxed)
        ),
        "取消run后不应继续发起商品请求"
    );
    assert!(app
        .snapshot()
        .await
        .unwrap()
        .products
        .iter()
        .any(|product| { product.product_id == "246" && product.observation.is_none() }));
}

#[tokio::test]
async fn restoring_defaults_ignores_a_request_already_in_flight() {
    let directory = TestDirectory::new();
    {
        let mut storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "246".into(),
                    product_id: "246".into(),
                    sku_id: None,
                },
                "本地样例商品 246".into(),
                "fixture".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("246", true).unwrap();
    }

    let (first_started, mut started) = tokio::sync::watch::channel(false);
    let client = Arc::new(SlowFirstClient {
        first_started,
        release_first: Arc::new(tokio::sync::Notify::new()),
        first_calls: AtomicUsize::new(0),
        second_calls: AtomicUsize::new(0),
    });
    let app = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
    app.set_platform_notifications_available(true);
    let mut config = AppConfig::default();
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.schedule.start = "00:00".into();
    config.schedule.end = "23:59".into();
    app.save_config(config).await.unwrap();
    app.complete_setup().await.unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        while !*started.borrow() {
            started.changed().await.unwrap();
        }
    })
    .await
    .expect("商品请求已发出");
    app.restore_defaults(false).await.unwrap();
    client.release_first.notify_one();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let snapshot = app.snapshot().await.unwrap();
    assert_eq!(snapshot.runtime.state, RuntimeState::Stopped);
    assert!(snapshot.recent_events.is_empty());
    assert!(snapshot.recent_checks.is_empty());
    let storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
    assert!(storage.observation("246").unwrap().is_none());
    assert_eq!(storage.check_count("246").unwrap(), 0);
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn product_scan_can_pause_resume_and_cancel_through_another_app_instance() {
    let directory = TestDirectory::new();
    {
        let mut storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        let mut config = MonitorConfig::default();
        config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
        config.scan.interval = Duration::from_secs(1);
        storage.save_monitor_config(&config).unwrap();
    }

    let client = Arc::new(ScanClient(AtomicUsize::new(0)));
    let app = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    let started = app.start_product_scan(300, 305).await.unwrap();
    assert_eq!(started.status, ScanStatus::Running);
    wait_for(&app, |snapshot| {
        snapshot.scan.as_ref().is_some_and(|scan| scan.checked >= 1)
    })
    .await;

    let controller = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
    let paused = controller
        .control_product_scan(ScanAction::Pause)
        .await
        .unwrap();
    assert_eq!(paused.status, ScanStatus::Paused);
    wait_for(&app, |snapshot| {
        snapshot
            .scan
            .as_ref()
            .is_some_and(|scan| scan.status == ScanStatus::Paused)
    })
    .await;
    let calls_while_paused = client.0.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(client.0.load(Ordering::Relaxed), calls_while_paused);

    controller
        .control_product_scan(ScanAction::Resume)
        .await
        .unwrap();
    wait_for(&app, |snapshot| {
        snapshot.scan.as_ref().is_some_and(|scan| scan.checked >= 2)
    })
    .await;
    let mut snapshots = app.subscribe();
    while snapshots.try_recv().is_ok() {}
    let cancelled = controller
        .control_product_scan(ScanAction::Cancel)
        .await
        .unwrap();
    assert_eq!(cancelled.status, ScanStatus::Cancelled);
    let cancelled_snapshot = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = snapshots.recv().await.unwrap();
            if snapshot
                .scan
                .as_ref()
                .is_some_and(|scan| scan.status == ScanStatus::Cancelled)
            {
                break snapshot;
            }
        }
    })
    .await;
    let cancelled_snapshot = cancelled_snapshot.expect("扫描取消应广播给原应用订阅者");
    assert_eq!(
        cancelled_snapshot.scan.as_ref().unwrap().status,
        ScanStatus::Cancelled
    );
    let calls_after_cancel = client.0.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(client.0.load(Ordering::Relaxed), calls_after_cancel);

    app.start_product_scan(400, 410).await.unwrap();
    wait_for(&app, |snapshot| {
        snapshot.scan.as_ref().is_some_and(|scan| scan.checked >= 1)
    })
    .await;
    let calls_before_shutdown = client.0.load(Ordering::Relaxed);
    app.shutdown();
    runner.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        client.0.load(Ordering::Relaxed),
        calls_before_shutdown,
        "run退出后不应遗留扫描请求"
    );
}

#[tokio::test]
#[ignore = "手动本地耐久检查，约 6 分钟；只使用注入 fixture"]
async fn local_fixture_soak_records_progress_and_shuts_down() {
    let data_dir = std::env::temp_dir().join(format!(
        "ricoh-monitor-soak-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    std::fs::create_dir_all(&data_dir).unwrap();
    let database = data_dir.join("monitor.sqlite3");
    {
        let mut storage = Storage::open(&database).unwrap();
        let mut config = MonitorConfig::default();
        config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
        config.schedule.start_minute = 0;
        config.schedule.end_minute = 23 * 60 + 59;
        config.requests.interval = Duration::from_millis(10);
        config.requests.jitter_percent = 0.0;
        config.requests.global_requests_per_second = 90.0;
        config.requests.max_concurrent_requests = 4;
        storage.save_monitor_config(&config).unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "900001".into(),
                    product_id: "900001".into(),
                    sku_id: None,
                },
                "本地耐久样例商品".into(),
                "fixture".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("900001", true).unwrap();
    }

    let client = Arc::new(AlternatingStockClient(AtomicUsize::new(0)));
    let app = MonitorApp::open_with_scheduler_client(&data_dir, client.clone()).unwrap();
    app.set_platform_notifications_available(true);
    app.complete_setup().await.unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });

    tokio::time::timeout(Duration::from_secs(10), async {
        while client.0.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture 调度器应开始检查");
    let rss_before_kib = process_rss_kib();
    eprintln!(
        "soak_start pid={} data_dir={} rss_before_kib={} requests={}",
        std::process::id(),
        data_dir.display(),
        rss_before_kib,
        client.0.load(Ordering::Relaxed)
    );

    tokio::time::sleep(Duration::from_secs(180)).await;
    let rss_mid_kib = process_rss_kib();
    let requests_mid = client.0.load(Ordering::Relaxed);
    eprintln!(
        "soak_mid elapsed_seconds=180 rss_kib={} requests={}",
        rss_mid_kib, requests_mid
    );
    tokio::time::sleep(Duration::from_secs(185)).await;

    app.monitoring_action(MonitoringAction::Stop).await.unwrap();
    app.shutdown();
    tokio::time::timeout(Duration::from_secs(10), runner)
        .await
        .expect("停止后 run 应在 10 秒内退出")
        .unwrap()
        .unwrap();
    let requests_after_stop = client.0.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        requests_after_stop,
        client.0.load(Ordering::Relaxed),
        "run 退出后不应继续发起 fixture 请求"
    );
    drop(app);

    let rss_after_kib = process_rss_kib();
    let connection = Connection::open(&database).unwrap();
    let request_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
        .unwrap();
    let event_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    let check_run_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM check_runs", [], |row| row.get(0))
        .unwrap();
    let oldest_event_id: i64 = connection
        .query_row("SELECT COALESCE(MIN(id), 0) FROM events", [], |row| {
            row.get(0)
        })
        .unwrap();
    let product_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM products WHERE is_configured = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    eprintln!(
        "soak_end elapsed_seconds=365 pid={} rss_after_kib={} requests={} observations={} products={} events={} check_runs={} oldest_event_id={} data_dir={}",
        std::process::id(),
        rss_after_kib,
        requests_after_stop,
        request_count,
        product_count,
        event_count,
        check_run_count,
        oldest_event_id,
        data_dir.display()
    );

    assert!(requests_mid > 0 && requests_after_stop > requests_mid, "两个采样阶段都应持续产生检查");
    assert_eq!(request_count, 1);
    assert_eq!(product_count, 1);
    // 超过保留阈值后的淘汰由 Storage 的定量测试独立验收；这里记录耐久吞吐。
    if requests_after_stop > 20_100 {
        assert!(oldest_event_id > 1, "跨过事件阈值后应清理较早事件");
    }
    assert!(
        event_count <= 10_000 && check_run_count <= 10_000,
        "每次写入后两种历史记录都应保持在数量上限内"
    );
    assert!(rss_before_kib > 0 && rss_mid_kib > 0 && rss_after_kib > 0);
}

#[test]
fn process_rss_can_sample_current_test_process() {
    assert!(process_rss_kib() > 0);
}

fn process_rss_kib() -> u64 {
    #[cfg(windows)]
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "[Diagnostics.Process]::GetProcessById({}).WorkingSet64 -shr 10",
                std::process::id()
            ),
        ])
        .output();
    #[cfg(not(windows))]
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output();
    let output = output.expect("系统命令可读取当前测试进程的驻留内存");
    assert!(output.status.success(), "应成功读取当前测试进程的驻留内存");
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .expect("驻留内存输出应为 KiB 数值")
}
