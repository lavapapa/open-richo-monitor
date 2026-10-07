use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use ricoh_monitor_core::{
    app::{MonitorApp, ScanAction, ScanStatus},
    availability::Availability,
    config::MonitorConfig,
    ricoh::ProductDetail,
    ricoh_api::RicohApiError,
    scheduler::{LineId, SchedulerClient},
    storage::Storage,
};

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ricoh-scan-lifecycle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        let mut db = Storage::open(path.join("monitor.sqlite3")).unwrap();
        let mut config = MonitorConfig::default();
        config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
        config.scan.interval = Duration::from_millis(10);
        config.requests.interval = Duration::from_millis(10);
        config.requests.jitter_percent = 0.0;
        db.save_monitor_config(&config).unwrap();
        Self(path)
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct SlowClient {
    started: tokio::sync::watch::Sender<Option<u64>>,
    release: Arc<tokio::sync::Notify>,
    calls: Arc<AtomicUsize>,
}
impl SchedulerClient for SlowClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.started.send_replace(Some(line.product_id));
        let release = self.release.clone();
        Box::pin(async move {
            if line.product_id == 700 {
                release.notified().await;
            }
            Ok(ProductDetail {
                product_id: line.product_id,
                name: format!("商品 {}", line.product_id),
                is_show: 1,
                stock: serde_json::json!(1).as_number().unwrap().clone(),
                availability: Availability::InStock,
                metadata: Default::default(),
            })
        })
    }
}

async fn wait_started(rx: &mut tokio::sync::watch::Receiver<Option<u64>>, id: u64) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while *rx.borrow_and_update() != Some(id) {
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelled_in_flight_scan_cannot_write_into_next_range() {
    let dir = TestDirectory::new();
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let release = Arc::new(tokio::sync::Notify::new());
    let app = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release: release.clone(),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    app.start_product_scan(700, 700).await.unwrap();
    wait_started(&mut rx, 700).await;
    app.control_product_scan(ScanAction::Cancel).await.unwrap();
    app.start_product_scan(800, 800).await.unwrap();
    release.notify_waiters();
    let scan = tokio::time::timeout(Duration::from_secs(3), app.wait_for_scan())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan.status, ScanStatus::Completed);
    assert_eq!(scan.start_id, "800");
    assert_eq!(scan.checked, 1);
    let products = app.snapshot().await.unwrap().catalog;
    assert!(products.iter().any(|p| p.product_id == "800"));
    assert!(!products.iter().any(|p| p.product_id == "700"));
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn another_instance_can_cancel_and_replace_an_in_flight_scan() {
    let dir = TestDirectory::new();
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let app = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release: Arc::new(tokio::sync::Notify::new()),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    app.start_product_scan(700, 700).await.unwrap();
    wait_started(&mut rx, 700).await;
    let controller = MonitorApp::open(&dir.0).unwrap();
    controller
        .control_product_scan(ScanAction::Cancel)
        .await
        .unwrap();
    controller.start_product_scan(800, 800).await.unwrap();
    let scan = tokio::time::timeout(Duration::from_secs(3), controller.wait_for_scan())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan.status, ScanStatus::Completed);
    assert_eq!(scan.start_id, "800");
    assert_eq!(scan.results[0].product_id, "800");
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn snapshot_uses_new_checkpoint_over_stale_in_flight_scan() {
    let dir = TestDirectory::new();
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let app = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release: Arc::new(tokio::sync::Notify::new()),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    app.start_product_scan(700, 700).await.unwrap();
    wait_started(&mut rx, 700).await;
    let controller = MonitorApp::open(&dir.0).unwrap();
    controller
        .control_product_scan(ScanAction::Cancel)
        .await
        .unwrap();
    controller.start_product_scan(800, 800).await.unwrap();
    assert_eq!(app.snapshot().await.unwrap().scan.unwrap().start_id, "800");
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn stale_instance_pause_does_not_restore_an_old_checkpoint() {
    let dir = TestDirectory::new();
    let first = MonitorApp::open(&dir.0).unwrap();
    first.start_product_scan(700, 700).await.unwrap();
    let stale = MonitorApp::open(&dir.0).unwrap();
    first
        .control_product_scan(ScanAction::Cancel)
        .await
        .unwrap();
    first.start_product_scan(800, 800).await.unwrap();
    stale.control_product_scan(ScanAction::Pause).await.unwrap();
    let checkpoint = Storage::open(dir.0.join("monitor.sqlite3"))
        .unwrap()
        .scan_checkpoint()
        .unwrap()
        .unwrap();
    assert_eq!(
        ricoh_monitor_core::catalog::ProductIdScan::restore(checkpoint).start_id(),
        800
    );
}

#[tokio::test]
async fn stale_instance_resume_keeps_new_progress_of_the_same_scan() {
    let dir = TestDirectory::new();
    let first = MonitorApp::open(&dir.0).unwrap();
    first.start_product_scan(700, 701).await.unwrap();
    let stale = MonitorApp::open(&dir.0).unwrap();
    let mut db = Storage::open(dir.0.join("monitor.sqlite3")).unwrap();
    let mut progressed =
        ricoh_monitor_core::catalog::ProductIdScan::restore(db.scan_checkpoint().unwrap().unwrap());
    progressed.continue_scan();
    assert_eq!(progressed.next_id(), Some(700));
    progressed.record_failure().unwrap();
    db.save_scan_checkpoint(&progressed.checkpoint().unwrap())
        .unwrap();
    drop(db);
    stale
        .control_product_scan(ScanAction::Resume)
        .await
        .unwrap();
    let checkpoint = Storage::open(dir.0.join("monitor.sqlite3"))
        .unwrap()
        .scan_checkpoint()
        .unwrap()
        .unwrap();
    assert_eq!(
        ricoh_monitor_core::catalog::ProductIdScan::restore(checkpoint)
            .completed()
            .len(),
        1
    );
}

#[test]
fn stale_scan_commit_cannot_save_product_or_checkpoint() {
    use ricoh_monitor_core::catalog::ProductIdScan;
    let dir = TestDirectory::new();
    let mut db = Storage::open(dir.0.join("monitor.sqlite3")).unwrap();
    let mut old = ProductIdScan::new(700, 700).unwrap();
    assert!(db.start_scan_if_idle(&old.checkpoint().unwrap()).unwrap());
    assert_eq!(old.next_id(), Some(700));
    assert!(db
        .control_scan_if_current(old.scan_id(), false, true)
        .unwrap());
    let new = ProductIdScan::new(800, 800).unwrap();
    assert!(db.start_scan_if_idle(&new.checkpoint().unwrap()).unwrap());
    let detail = ProductDetail {
        product_id: 700,
        name: "旧结果".into(),
        is_show: 1,
        stock: serde_json::json!(1).as_number().unwrap().clone(),
        availability: Availability::InStock,
        metadata: Default::default(),
    };
    let committed = db
        .commit_scan_item(old.scan_id(), old, Some(detail), 1)
        .unwrap();
    assert!(committed.is_none());
    assert_eq!(
        db.scan_checkpoint().unwrap().unwrap().scan_id(),
        new.scan_id()
    );
    assert!(db.product_configs().unwrap().is_empty());
}

#[tokio::test]
async fn cancelling_from_another_instance_broadcasts_to_runner_subscribers() {
    let dir = TestDirectory::new();
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let runner_app = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release: Arc::new(tokio::sync::Notify::new()),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .unwrap();
    let controller = MonitorApp::open(&dir.0).unwrap();
    let mut snapshots = runner_app.subscribe();
    let runner = tokio::spawn({
        let app = runner_app.clone();
        async move { app.run().await }
    });
    controller.start_product_scan(700, 700).await.unwrap();
    wait_started(&mut rx, 700).await;
    controller
        .control_product_scan(ScanAction::Cancel)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = snapshots.recv().await.unwrap();
            if snapshot
                .scan
                .as_ref()
                .is_some_and(|scan| scan.status == ScanStatus::Cancelled)
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    runner_app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn controller_snapshots_stay_out_of_an_idle_runner_subscription() {
    let dir = TestDirectory::new();
    let runner_app = MonitorApp::open(&dir.0).unwrap();
    let controller = MonitorApp::open(&dir.0).unwrap();
    let mut snapshots = runner_app.subscribe();
    controller.start_product_scan(700, 700).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), snapshots.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn removing_an_in_flight_product_keeps_it_removed() {
    let dir = TestDirectory::new();
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let release = Arc::new(tokio::sync::Notify::new());
    let app = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release: release.clone(),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    app.start_product_scan(700, 700).await.unwrap();
    wait_started(&mut rx, 700).await;
    app.remove_product(700).await.unwrap();
    release.notify_waiters();
    let scan = tokio::time::timeout(Duration::from_secs(3), app.wait_for_scan())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan.status, ScanStatus::Completed);
    assert_eq!(scan.found, 0);
    assert!(scan.results.is_empty());
    assert!(!app
        .snapshot()
        .await
        .unwrap()
        .catalog
        .iter()
        .any(|p| p.product_id == "700"));
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn another_instance_removing_an_in_flight_product_keeps_it_removed() {
    let dir = TestDirectory::new();
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let release = Arc::new(tokio::sync::Notify::new());
    let app = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release: release.clone(),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    app.start_product_scan(700, 700).await.unwrap();
    wait_started(&mut rx, 700).await;
    MonitorApp::open(&dir.0)
        .unwrap()
        .remove_product(700)
        .await
        .unwrap();
    release.notify_waiters();
    let scan = tokio::time::timeout(Duration::from_secs(3), app.wait_for_scan())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan.status, ScanStatus::Completed);
    assert_eq!(scan.found, 0);
    assert!(scan.results.is_empty());
    assert!(!app
        .snapshot()
        .await
        .unwrap()
        .catalog
        .iter()
        .any(|p| p.product_id == "700"));
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn removing_a_queued_product_blocks_it_for_the_current_scan() {
    let dir = TestDirectory::new();
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let release = Arc::new(tokio::sync::Notify::new());
    let app = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release: release.clone(),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    app.start_product_scan(700, 701).await.unwrap();
    wait_started(&mut rx, 700).await;
    MonitorApp::open(&dir.0)
        .unwrap()
        .remove_product(701)
        .await
        .unwrap();
    release.notify_waiters();
    let scan = tokio::time::timeout(Duration::from_secs(3), app.wait_for_scan())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan.status, ScanStatus::Completed);
    assert_eq!(scan.checked, 2);
    assert_eq!(scan.found, 1);
    assert_eq!(scan.results[0].product_id, "700");
    assert!(!app
        .snapshot()
        .await
        .unwrap()
        .catalog
        .iter()
        .any(|p| p.product_id == "701"));
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_new_scan_can_find_a_previously_removed_product() {
    let dir = TestDirectory::new();
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let release = Arc::new(tokio::sync::Notify::new());
    let app = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release: release.clone(),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .unwrap();
    app.remove_product(700).await.unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    app.start_product_scan(700, 700).await.unwrap();
    wait_started(&mut rx, 700).await;
    release.notify_waiters();
    let scan = tokio::time::timeout(Duration::from_secs(3), app.wait_for_scan())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan.found, 1);
    assert!(app
        .snapshot()
        .await
        .unwrap()
        .catalog
        .iter()
        .any(|p| p.product_id == "700"));
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn replacing_a_paused_in_flight_scan_starts_the_new_range() {
    let dir = TestDirectory::new();
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let release = Arc::new(tokio::sync::Notify::new());
    let app = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release: release.clone(),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    app.start_product_scan(700, 700).await.unwrap();
    wait_started(&mut rx, 700).await;
    app.control_product_scan(ScanAction::Pause).await.unwrap();
    app.start_product_scan(800, 800).await.unwrap();
    release.notify_waiters();
    let scan = tokio::time::timeout(Duration::from_secs(3), app.wait_for_scan())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan.status, ScanStatus::Completed);
    assert_eq!(scan.start_id, "800");
    assert_eq!(scan.results[0].product_id, "800");
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn network_change_restarts_the_in_flight_scan() {
    let dir = TestDirectory::new();
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let app = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release: release.clone(),
            calls: calls.clone(),
        }),
    )
    .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    app.start_product_scan(700, 700).await.unwrap();
    wait_started(&mut rx, 700).await;
    let mut config = app.snapshot().await.unwrap().config;
    config.use_system_proxy = !config.use_system_proxy;
    app.save_config(config).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while calls.load(Ordering::Relaxed) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    release.notify_waiters();
    let scan = tokio::time::timeout(Duration::from_secs(3), app.wait_for_scan())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan.status, ScanStatus::Completed);
    assert_eq!(scan.checked, 1);
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn restart_resumes_the_in_flight_checkpoint() {
    let dir = TestDirectory::new();
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    let release = Arc::new(tokio::sync::Notify::new());
    let app = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release,
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    app.start_product_scan(700, 701).await.unwrap();
    wait_started(&mut rx, 700).await;
    app.control_product_scan(ScanAction::Pause).await.unwrap();
    app.shutdown();
    runner.await.unwrap().unwrap();
    drop(app);
    let (tx, mut restarted_rx) = tokio::sync::watch::channel(None);
    let release = Arc::new(tokio::sync::Notify::new());
    let restarted = MonitorApp::open_with_scheduler_client(
        &dir.0,
        Arc::new(SlowClient {
            started: tx,
            release: release.clone(),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    )
    .unwrap();
    let runner = tokio::spawn({
        let app = restarted.clone();
        async move { app.run().await }
    });
    restarted
        .control_product_scan(ScanAction::Resume)
        .await
        .unwrap();
    // 检查点保留 current_id；等新请求真正进入夹具后再释放它。
    wait_started(&mut restarted_rx, 700).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if restarted
                .snapshot()
                .await
                .unwrap()
                .scan
                .as_ref()
                .and_then(|scan| scan.current_id.as_deref())
                == Some("700")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // notify_one 保留许可，覆盖请求已开始但 Future 尚未进入等待的交错。
    release.notify_one();
    let scan = tokio::time::timeout(Duration::from_secs(3), restarted.wait_for_scan())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan.status, ScanStatus::Completed);
    assert_eq!(scan.checked, 2);
    restarted.shutdown();
    runner.await.unwrap().unwrap();
}
