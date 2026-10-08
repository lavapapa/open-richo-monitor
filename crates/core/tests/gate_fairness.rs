use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use ricoh_monitor_core::{
    availability::Availability,
    config::MonitorConfig,
    ricoh::ProductDetail,
    ricoh_api::RicohApiError,
    scheduler::{
        ControlToken, JitterSource, LineId, RequestGate, Scheduler, SchedulerClient, TokioClock,
    },
};

struct NoJitter;
impl JitterSource for NoJitter {
    fn sample(&self) -> f64 {
        0.0
    }
}

struct CountingClient {
    first: AtomicUsize,
    second: AtomicUsize,
}
impl SchedulerClient for CountingClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send>> {
        match line.product_id {
            65 => {
                self.first.fetch_add(1, Ordering::Relaxed);
            }
            66 => {
                self.second.fetch_add(1, Ordering::Relaxed);
            }
            _ => panic!("意外商品"),
        }
        Box::pin(async move {
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
async fn two_products_and_scan_each_receive_repeated_admission() {
    let mut config = MonitorConfig::default();
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.schedule.start_minute = 0;
    config.schedule.end_minute = 0;
    config.requests.interval = Duration::from_millis(1);
    config.requests.jitter_percent = 0.0;
    config.scan.interval = Duration::from_millis(1);
    let clock = Arc::new(TokioClock::default());
    let gate = Arc::new(RequestGate::new(
        &config.requests,
        config.scan.interval,
        clock.clone(),
    ));
    let client = Arc::new(CountingClient {
        first: AtomicUsize::new(0),
        second: AtomicUsize::new(0),
    });
    let scheduler =
        Scheduler::new_with_gate(config, client.clone(), Arc::new(NoJitter), gate.clone()).unwrap();
    let mut monitor = scheduler
        .start([LineId::new(65, "direct"), LineId::new(66, "direct")])
        .unwrap();
    let (scan_control, mut scan_receiver) = ControlToken::new();
    let scans = Arc::new(AtomicUsize::new(0));
    let scan_count = scans.clone();
    let scan_task = tokio::spawn(async move {
        while let Ok(permit) = gate.acquire_scan(67, &mut scan_receiver).await {
            scan_count.fetch_add(1, Ordering::Relaxed);
            drop(permit);
        }
    });
    let observed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if client.first.load(Ordering::Relaxed) >= 3
                && client.second.load(Ordering::Relaxed) >= 3
                && scans.load(Ordering::Relaxed) >= 3
            {
                break true;
            }
            let _ = monitor.recv().await;
        }
    })
    .await;
    scan_control.cancel();
    scan_task.await.unwrap();
    monitor.shutdown().await;
    assert!(
        observed.is_ok(),
        "启动数：商品65={}、商品66={}、扫描={}",
        client.first.load(Ordering::Relaxed),
        client.second.load(Ordering::Relaxed),
        scans.load(Ordering::Relaxed)
    );
}
