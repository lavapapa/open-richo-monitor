use ricoh_monitor_core::{
    app::{MonitorApp, MonitoringAction, Weekday},
    availability::Availability,
    config::MonitorConfig,
    ricoh::{ProductDetail, ProductMetadata},
    ricoh_api::RicohApiError,
    scheduler::{LineId, SchedulerClient},
    storage::{ProductIdentity, ProductStatistics, Storage},
};
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
use tokio::sync::{watch, Semaphore};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ricoh-catalog-lifecycle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        let mut db = Storage::open(path.join("monitor.sqlite3")).unwrap();
        let mut config = MonitorConfig::default();
        config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
        config.schedule.start_minute = 0;
        config.schedule.end_minute = 0;
        config.requests.interval = Duration::from_millis(10);
        config.requests.jitter_percent = 0.0;
        config.requests.global_requests_per_second = 100.0;
        config.scan.interval = Duration::from_millis(10);
        db.save_monitor_config(&config).unwrap();
        Self(path)
    }
    fn db(&self) -> Storage {
        Storage::open(self.0.join("monitor.sqlite3")).unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct HeldClient {
    calls: AtomicUsize,
    started: watch::Sender<usize>,
    release: Arc<Semaphore>,
}
impl HeldClient {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            started: watch::channel(0).0,
            release: Arc::new(Semaphore::new(0)),
        })
    }
    async fn wait_for_first(&self) {
        let mut calls = self.started.subscribe();
        tokio::time::timeout(Duration::from_secs(3), async {
            while *calls.borrow_and_update() == 0 {
                calls.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
}
impl SchedulerClient for HeldClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send>> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        self.started.send_replace(call + 1);
        let release = self.release.clone();
        Box::pin(async move {
            if call == 0 {
                release.acquire().await.unwrap().forget();
            }
            Ok(ProductDetail {
                product_id: line.product_id,
                name: "样例商品".into(),
                is_show: 1,
                stock: serde_json::Number::from(1),
                availability: Availability::InStock,
                metadata: ProductMetadata {
                    price: Some("19.99".into()),
                    ..Default::default()
                },
            })
        })
    }
}

#[tokio::test]
async fn another_instance_reset_or_import_stops_the_entire_in_flight_catalog_refresh() {
    for import in [false, true] {
        let directory = Directory::new();
        let client = HeldClient::new();
        let app = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
        let controller = MonitorApp::open(&directory.0).unwrap();
        app.refresh_catalog_metadata();
        client.wait_for_first().await;
        app.refresh_catalog_metadata();
        assert_eq!(client.calls.load(Ordering::Relaxed), 1);
        if import {
            let exported = controller.export_config().await.unwrap();
            controller.import_config(&exported).await.unwrap();
        } else {
            controller.restore_defaults(false).await.unwrap();
        }
        client.release.add_permits(1);
        tokio::time::sleep(Duration::from_millis(400)).await;
        app.shutdown();
        controller.shutdown();
        assert_eq!(client.calls.load(Ordering::Relaxed), 1);
        let db = directory.db();
        assert!(db.product_configs().unwrap().is_empty());
        assert!(db.product_metadata("9").unwrap().is_none());
    }
}

#[tokio::test]
async fn another_instance_reset_or_remove_does_not_restore_old_monitoring_statistics() {
    for remove in [false, true] {
        let directory = Directory::new();
        {
            let mut db = directory.db();
            db.save_product_config(
                ProductIdentity {
                    key: "65".into(),
                    product_id: "65".into(),
                    sku_id: None,
                },
                "样例商品".into(),
                "fixture".into(),
                Some(1),
            )
            .unwrap();
            db.set_product_enabled("65", true).unwrap();
            db.add_monitoring_duration("65", 12_000).unwrap();
        }
        let client = HeldClient::new();
        let app = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
        app.complete_setup().await.unwrap();
        app.monitoring_action(MonitoringAction::Start)
            .await
            .unwrap();
        let runner = tokio::spawn({
            let app = app.clone();
            async move { app.run().await }
        });
        client.wait_for_first().await;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if app.snapshot().await.unwrap().products[0].monitoring_ms >= 12_100 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let controller = MonitorApp::open(&directory.0).unwrap();
        if remove {
            controller.remove_product(65).await.unwrap();
        } else {
            controller.restore_defaults(false).await.unwrap();
        }
        client.release.add_permits(1);
        tokio::time::sleep(Duration::from_millis(1_300)).await;
        let before_shutdown = directory.db().product_statistics("65", now_ms()).unwrap();
        app.shutdown();
        controller.shutdown();
        runner.await.unwrap().unwrap();
        assert_eq!(before_shutdown, ProductStatistics::default());
        let db = directory.db();
        assert_eq!(
            db.product_statistics("65", now_ms()).unwrap(),
            ProductStatistics::default()
        );
        assert!(db.product_configs().unwrap().iter().all(|p| !p.enabled));
        assert!(db.product_metadata("65").unwrap().is_none());
        assert!(db.observation("65").unwrap().is_none());
    }
}

#[tokio::test]
async fn changing_to_an_outside_schedule_keeps_the_started_result_and_stops_new_requests() {
    let directory = Directory::new();
    {
        let mut db = directory.db();
        db.save_product_config(
            ProductIdentity {
                key: "65".into(),
                product_id: "65".into(),
                sku_id: None,
            },
            "样例商品".into(),
            "fixture".into(),
            Some(1),
        )
        .unwrap();
        db.set_product_enabled("65", true).unwrap();
    }
    let client = HeldClient::new();
    let app = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
    app.complete_setup().await.unwrap();
    app.monitoring_action(MonitoringAction::Start)
        .await
        .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    client.wait_for_first().await;
    let days = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];
    let today = (((now_ms() / 1000 + 8 * 3600) / 86400 + 3) % 7) as usize;
    let mut config = app.snapshot().await.unwrap().config;
    config.schedule.days = vec![days[(today + 1) % 7]];
    app.save_config(config).await.unwrap();
    client.release.add_permits(1);
    tokio::time::sleep(Duration::from_millis(150)).await;
    app.shutdown();
    runner.await.unwrap().unwrap();
    let db = directory.db();
    assert_eq!(client.calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        db.product_statistics("65", now_ms())
            .unwrap()
            .today_check_count,
        1
    );
    assert!(db.observation("65").unwrap().is_some());
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
