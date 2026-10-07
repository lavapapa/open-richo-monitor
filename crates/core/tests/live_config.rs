use ricoh_monitor_core::{
    app::{AppConfig, MonitorApp, MonitoringAction, RuntimeState, Weekday},
    availability::Availability,
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
        Arc,
    },
    time::Duration,
};

struct HeldClient {
    attempts: AtomicUsize,
    started: tokio::sync::Notify,
    release: Arc<tokio::sync::Notify>,
}
impl SchedulerClient for HeldClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send>> {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed);
        self.started.notify_one();
        let release = self.release.clone();
        Box::pin(async move {
            if attempt == 0 {
                release.notified().await;
            }
            Ok(ProductDetail {
                product_id: line.product_id,
                name: "样例".into(),
                is_show: 0,
                stock: serde_json::Number::from(0),
                availability: Availability::OutOfStock,
                metadata: Default::default(),
            })
        })
    }
}
struct Directory(std::path::PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn prepare(name: &str) -> (Directory, MonitorApp, Arc<HeldClient>, AppConfig) {
    let directory = Directory(
        std::env::temp_dir().join(format!("ricoh-live-config-{name}-{}", std::process::id())),
    );
    std::fs::create_dir_all(&directory.0).unwrap();
    {
        let mut db = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        db.save_product_config(
            ProductIdentity {
                key: "65".into(),
                product_id: "65".into(),
                sku_id: None,
            },
            "样例".into(),
            "fixture".into(),
            Some(1),
        )
        .unwrap();
        db.set_product_enabled("65", true).unwrap();
    }
    let client = Arc::new(HeldClient {
        attempts: AtomicUsize::new(0),
        started: tokio::sync::Notify::new(),
        release: Arc::new(tokio::sync::Notify::new()),
    });
    let app = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
    let mut config = AppConfig::default();
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.schedule.start = "00:00".into();
    config.schedule.end = "00:00".into();
    (directory, app, client, config)
}
#[tokio::test]
async fn saving_rate_and_alert_keeps_in_flight_request_and_applies_next_interval() {
    let (_directory, app, client, mut config) = prepare("rate");
    app.save_config(config.clone()).await.unwrap();
    app.complete_setup().await.unwrap();
    app.monitoring_action(MonitoringAction::Start)
        .await
        .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    tokio::time::timeout(Duration::from_secs(2), client.started.notified())
        .await
        .unwrap();
    config.rate.interval_min_ms = 100;
    config.rate.interval_max_ms = 100;
    config.rate.failures_before_backoff = 4;
    config.rate.failure_backoff_seconds = 5;
    config.failure_alert_after_minutes = 3;
    app.save_config(config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let attempts_before_release = client.attempts.load(Ordering::Relaxed);
    client.release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        while app.snapshot().await.unwrap().products[0].check_count < 4 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    app.shutdown();
    runner.await.unwrap().unwrap();
    assert_eq!(
        attempts_before_release, 1,
        "仅修改检查节奏与提醒阈值应保留在途请求"
    );
}
#[tokio::test]
async fn changing_schedule_stops_in_flight_work_and_reentering_resumes() {
    let (_directory, app, client, mut config) = prepare("schedule");
    config.rate.interval_min_ms = 100;
    config.rate.interval_max_ms = 100;
    app.save_config(config.clone()).await.unwrap();
    app.complete_setup().await.unwrap();
    app.monitoring_action(MonitoringAction::Start)
        .await
        .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    tokio::time::timeout(Duration::from_secs(2), client.started.notified())
        .await
        .unwrap();
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let today = ((seconds + 8 * 3600) / 86400 + 3) % 7;
    let days = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];
    config.schedule.days = vec![days[((today + 1) % 7) as usize]];
    app.save_config(config.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(650)).await;
    client.release.notify_one();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let outside = app.snapshot().await.unwrap();
    config.schedule.days = days.to_vec();
    app.save_config(config).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while app.snapshot().await.unwrap().products[0].check_count == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    app.shutdown();
    runner.await.unwrap().unwrap();
    assert_eq!(outside.runtime.state, RuntimeState::OutsideSchedule);
    assert_eq!(outside.products[0].check_count, 0, "计划外不应提交旧响应");
}

#[tokio::test]
async fn pausing_then_immediately_resuming_starts_a_fresh_request() {
    let (_directory, app, client, config) = prepare("quick-pause");
    app.save_config(config).await.unwrap();
    app.complete_setup().await.unwrap();
    app.monitoring_action(MonitoringAction::Start)
        .await
        .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    tokio::time::timeout(Duration::from_secs(2), client.started.notified())
        .await
        .unwrap();
    app.monitoring_action(MonitoringAction::Pause)
        .await
        .unwrap();
    app.monitoring_action(MonitoringAction::Resume)
        .await
        .unwrap();
    let restarted = tokio::time::timeout(Duration::from_secs(2), async {
        while client.attempts.load(Ordering::Relaxed) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    app.shutdown();
    runner.await.unwrap().unwrap();
    assert!(restarted.is_ok(), "暂停再恢复必须取消旧轮次的在途请求");
}

#[tokio::test]
async fn importing_then_immediately_resuming_discards_the_old_request() {
    let (_directory, app, client, config) = prepare("quick-import");
    app.save_config(config).await.unwrap();
    app.complete_setup().await.unwrap();
    let exported = app.export_config().await.unwrap();
    app.monitoring_action(MonitoringAction::Start)
        .await
        .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    tokio::time::timeout(Duration::from_secs(2), client.started.notified())
        .await
        .unwrap();
    app.import_config(&exported).await.unwrap();
    assert_eq!(
        app.snapshot().await.unwrap().runtime.state,
        RuntimeState::Stopped
    );
    app.monitoring_action(MonitoringAction::Resume)
        .await
        .unwrap();
    let restarted = tokio::time::timeout(Duration::from_secs(2), async {
        while client.attempts.load(Ordering::Relaxed) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    app.shutdown();
    runner.await.unwrap().unwrap();
    assert!(
        restarted.is_ok(),
        "导入配置停止旧轮次，立即恢复也必须启动新请求"
    );
}

async fn cross_instance_restart(name: &str) {
    let (directory, app, client, config) = prepare(name);
    app.save_config(config).await.unwrap();
    app.complete_setup().await.unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    tokio::time::timeout(Duration::from_secs(2), client.started.notified())
        .await
        .unwrap();
    let controller = MonitorApp::open(&directory.0).unwrap();
    match name {
        "cross-pause" => {
            controller
                .monitoring_action(MonitoringAction::Pause)
                .await
                .unwrap();
            controller
                .monitoring_action(MonitoringAction::Resume)
                .await
                .unwrap();
        }
        "cross-toggle" => {
            controller.set_product_enabled(65, false).await.unwrap();
            controller.set_product_enabled(65, true).await.unwrap();
        }
        _ => {
            let exported = controller.export_config().await.unwrap();
            controller.import_config(&exported).await.unwrap();
            controller
                .monitoring_action(MonitoringAction::Resume)
                .await
                .unwrap();
        }
    }
    let restarted = tokio::time::timeout(Duration::from_secs(2), async {
        while client.attempts.load(Ordering::Relaxed) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    app.shutdown();
    runner.await.unwrap().unwrap();
    assert!(
        restarted.is_ok(),
        "独立实例的快速控制也应取消旧轮次请求：{name}"
    );
}
#[tokio::test]
async fn another_instance_can_pause_and_immediately_resume() {
    cross_instance_restart("cross-pause").await;
}
#[tokio::test]
async fn another_instance_can_toggle_off_and_immediately_on() {
    cross_instance_restart("cross-toggle").await;
}
#[tokio::test]
async fn another_instance_can_import_and_immediately_resume() {
    cross_instance_restart("cross-import").await;
}

struct MixedClient {
    failing: std::sync::atomic::AtomicBool,
}
impl SchedulerClient for MixedClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send>> {
        let fails = line.product_id == 65 && self.failing.load(Ordering::Relaxed);
        Box::pin(async move {
            if fails {
                return Err(reqwest::Client::new()
                    .get("not a url")
                    .build()
                    .unwrap_err()
                    .into());
            }
            Ok(ProductDetail {
                product_id: line.product_id,
                name: "样例".into(),
                is_show: 0,
                stock: serde_json::Number::from(0),
                availability: Availability::OutOfStock,
                metadata: Default::default(),
            })
        })
    }
}
#[tokio::test]
async fn live_rate_changes_keep_failure_health_and_other_product_working() {
    let (directory, unused_app, _, _) = prepare("mixed");
    drop(unused_app);
    {
        let mut db = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        db.save_product_config(
            ProductIdentity {
                key: "66".into(),
                product_id: "66".into(),
                sku_id: None,
            },
            "第二个样例".into(),
            "fixture".into(),
            Some(1),
        )
        .unwrap();
        db.set_product_enabled("66", true).unwrap();
        let mut config = ricoh_monitor_core::config::MonitorConfig::default();
        config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
        config.schedule.start_minute = 0;
        config.schedule.end_minute = 0;
        config.requests.interval = Duration::from_millis(20);
        config.requests.jitter_percent = 0.;
        config.requests.failures_before_backoff = 100;
        config.failure_alert_after = Duration::from_millis(50);
        db.save_monitor_config(&config).unwrap();
    }
    let client = Arc::new(MixedClient {
        failing: std::sync::atomic::AtomicBool::new(true),
    });
    let app = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
    app.complete_setup().await.unwrap();
    app.monitoring_action(MonitoringAction::Start)
        .await
        .unwrap();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = app.snapshot().await.unwrap();
            let failed = snapshot
                .recent_events
                .iter()
                .any(|e| e.kind == "monitoring_failed");
            let second_checked = snapshot
                .products
                .iter()
                .any(|product| product.product_id == "66" && product.observation.is_some());
            if failed && second_checked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let failed = app.snapshot().await.unwrap();
    assert_eq!(failed.runtime.state, RuntimeState::PartialError);
    assert!(failed
        .products
        .iter()
        .find(|p| p.product_id == "66")
        .unwrap()
        .observation
        .is_some());
    assert!(failed
        .products
        .iter()
        .find(|p| p.product_id == "65")
        .unwrap()
        .runtime_error
        .is_some());
    let mut config = failed.config;
    config.rate.interval_min_ms = 40;
    config.rate.interval_max_ms = 40;
    config.failure_alert_after_minutes = 1;
    app.save_config(config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    client.failing.store(false, Ordering::Relaxed);
    let recovered = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let s = app.snapshot().await.unwrap();
            if s.recent_events.iter().any(|e| e.kind == "recovered") {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    app.shutdown();
    runner.await.unwrap().unwrap();
    let recovered = recovered.expect("调整节奏必须保留故障健康计时，成功时发出恢复事件");
    assert_eq!(recovered.runtime.state, RuntimeState::Monitoring);
    assert_eq!(
        recovered
            .recent_events
            .iter()
            .filter(|e| e.kind == "monitoring_failed")
            .count(),
        1
    );
    assert!(
        recovered.recent_checks.len() >= 2,
        "没有通知渠道也应保留本地状态消息"
    );
}
