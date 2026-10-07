use super::*;
use std::{future::Future, pin::Pin, time::Duration};

struct Directory(PathBuf);
impl Directory {
    fn new(label: &str) -> Self {
        Self(std::env::temp_dir().join(format!(
            "ricoh-prominent-wake-{label}-{}-{}",
            std::process::id(),
            now_ms()
        )))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct HeldStockClient {
    started: watch::Sender<usize>,
    release: Arc<Notify>,
}
impl SchedulerClient for HeldStockClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, crate::ricoh_api::RicohApiError>> + Send>>
    {
        let mut stock = 0;
        self.started.send_modify(|count| {
            *count += 1;
            stock = *count;
        });
        let release = self.release.clone();
        Box::pin(async move {
            release.notified().await;
            Ok(ProductDetail {
                product_id: line.product_id,
                name: "唤醒 Fixture".into(),
                is_show: 1,
                stock: (stock as u64).into(),
                availability: Availability::InStock,
                metadata: Default::default(),
            })
        })
    }
}

#[tokio::test]
async fn prominent_wake_retains_and_coalesces_dismiss_permits_and_shutdown_wakes() {
    let directory = Directory::new("permit");
    let app = MonitorApp::open(&directory.0).unwrap();
    app.clone().wake_prominent_alerts();
    app.wake_prominent_alerts();
    tokio::time::timeout(Duration::from_secs(1), app.wait_for_prominent_alert())
        .await
        .expect("关闭信号先于等待时保留许可");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), app.wait_for_prominent_alert())
            .await
            .is_err(),
        "多个关闭信号合并为一次检查"
    );
    app.shutdown();
    tokio::time::timeout(Duration::from_secs(1), app.wait_for_prominent_alert())
        .await
        .expect("退出唤醒等待中的突出提醒消费者");
}

async fn prepare_held_stock(
    label: &str,
    prominent: bool,
) -> (
    Directory,
    MonitorApp,
    Arc<HeldStockClient>,
    watch::Receiver<usize>,
) {
    let directory = Directory::new(label);
    let (started, requests) = watch::channel(0);
    let client = Arc::new(HeldStockClient {
        started,
        release: Arc::new(Notify::new()),
    });
    let app = MonitorApp::open_with_scheduler_client(&directory.0, client.clone()).unwrap();
    app.set_onboarding_products(&[65]).await.unwrap();
    app.call(|db| {
        let mut config = db.monitor_config()?;
        config.monitoring_mode = MonitoringMode::ProductDetail;
        config.schedule.start_minute = 0;
        config.schedule.end_minute = 0;
        config.requests.interval = Duration::from_millis(20);
        config.requests.jitter_percent = 0.0;
        config.requests.global_requests_per_second = 1000.0;
        db.save_monitor_config(&config)
    })
    .await
    .unwrap();
    app.set_product_prominent_alert(65, prominent)
        .await
        .unwrap();
    app.complete_setup().await.unwrap();
    app.monitoring_action(MonitoringAction::Start)
        .await
        .unwrap();
    (directory, app, client, requests)
}

#[tokio::test]
async fn ordinary_listing_does_not_wake_prominent_consumer() {
    let (_directory, app, client, mut requests) = prepare_held_stock("ordinary", false).await;
    let mut snapshots = app.subscribe();
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    tokio::time::timeout(
        Duration::from_secs(4),
        requests.wait_for(|count| *count >= 1),
    )
    .await
    .unwrap()
    .unwrap();
    client.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = snapshots.recv().await.unwrap();
            if snapshot.products[0]
                .observation
                .as_ref()
                .is_some_and(|observation| observation.stock == Some(1.0))
            {
                break;
            }
        }
    })
    .await
    .expect("普通上架观察已持久化并发布");
    assert!(app.claim_prominent_alert().await.unwrap().is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), app.wait_for_prominent_alert())
            .await
            .is_err(),
        "没有突出outbox的真实普通事件不唤醒"
    );
    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn prominent_event_wakes_before_snapshot_and_survives_ack_and_restart() {
    let (directory, app, client, mut requests) = prepare_held_stock("event", true).await;
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    tokio::time::timeout(
        Duration::from_secs(4),
        requests.wait_for(|count| *count >= 1),
    )
    .await
    .unwrap()
    .unwrap();
    // 快照在扫描锁处等待，真实入队仍应立即唤醒消费者。
    let scan = app.scan.lock().await;
    client.release.notify_one();
    let woke = tokio::time::timeout(Duration::from_secs(1), app.wait_for_prominent_alert()).await;
    drop(scan);
    if woke.is_err() {
        app.shutdown();
        runner.await.unwrap().unwrap();
        panic!("监控事务提交后应在快照发布之前唤醒");
    }
    let first = app.claim_prominent_alert().await.unwrap().unwrap();
    assert_eq!(first.stock, 1.0);
    assert_eq!(
        app.claim_prominent_alert().await.unwrap().unwrap().event_id,
        first.event_id
    );
    app.acknowledge_prominent_alert(first.event_id)
        .await
        .unwrap();
    assert!(app.claim_prominent_alert().await.unwrap().is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), app.wait_for_prominent_alert())
            .await
            .is_err(),
        "确认展示保持等待，下一条由新入队或关闭信号唤醒"
    );

    tokio::time::timeout(
        Duration::from_secs(4),
        requests.wait_for(|count| *count >= 2),
    )
    .await
    .unwrap()
    .unwrap();
    let scan = app.scan.lock().await;
    client.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), app.wait_for_prominent_alert())
        .await
        .expect("连续库存事件独立于500ms快照节流");
    let second = app.claim_prominent_alert().await.unwrap().unwrap();
    assert_eq!(second.stock, 2.0);
    assert!(second.event_id > first.event_id);
    assert_eq!(
        app.latest_snapshot
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .products[0]
            .observation
            .as_ref()
            .unwrap()
            .stock,
        Some(1.0),
        "提醒已可领取时快照仍为上一条观察"
    );
    drop(scan);
    tokio::time::timeout(
        Duration::from_secs(4),
        requests.wait_for(|count| *count >= 3),
    )
    .await
    .unwrap()
    .unwrap();
    client.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), app.wait_for_prominent_alert())
        .await
        .expect("同商品待展示任务合并更新也唤醒消费者");
    let updated = app.claim_prominent_alert().await.unwrap().unwrap();
    assert_eq!(updated.stock, 3.0);
    assert!(updated.event_id > second.event_id);
    app.shutdown();
    runner.await.unwrap().unwrap();
    drop(app);
    let reopened = MonitorApp::open(&directory.0).unwrap();
    let pending = reopened.claim_prominent_alert().await.unwrap().unwrap();
    assert_eq!(pending.event_id, updated.event_id, "启动先领取持久任务");
    reopened
        .acknowledge_prominent_alert(pending.event_id)
        .await
        .unwrap();
    assert!(reopened.claim_prominent_alert().await.unwrap().is_none());
}
