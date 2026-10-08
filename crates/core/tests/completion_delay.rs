use ricoh_monitor_core::{
    config::{MonitorConfig, MonitoringMode},
    ricoh::{parse_product_response, ProductDetail},
    ricoh_api::RicohApiError,
    scheduler::{JitterSource, LineId, Scheduler, SchedulerClient, SchedulerClock, TokioClock},
};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

struct NoJitter;
impl JitterSource for NoJitter {
    fn sample(&self) -> f64 {
        0.0
    }
}

struct FrozenClock;
impl SchedulerClock for FrozenClock {
    fn monotonic_now(&self) -> Duration {
        Duration::ZERO
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }
    fn sleep_until(&self, deadline: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if !deadline.is_zero() {
                std::future::pending::<()>().await;
            }
        })
    }
}

struct SlowClient;
impl SchedulerClient for SlowClient {
    fn product_detail(
        &self,
        _line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(parse_product_response(65, include_bytes!("fixtures/product-65.json")).unwrap())
        })
    }
    fn product_page(
        &self,
        _outlet: String,
        page: u32,
        _limit: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ProductDetail>, RicohApiError>> + Send>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(if page == 1 {
                vec![
                    parse_product_response(65, include_bytes!("fixtures/product-65.json")).unwrap(),
                ]
            } else {
                vec![]
            })
        })
    }
}

#[tokio::test]
async fn zero_delay_in_both_modes_needs_no_clock_advance() {
    for mode in [
        MonitoringMode::ProductDetail,
        MonitoringMode::ListedProducts,
    ] {
        let mut config = MonitorConfig::default();
        config.monitoring_mode = mode;
        config.schedule.start_minute = 0;
        config.schedule.end_minute = 0;
        config.requests.interval = Duration::ZERO;
        config.requests.jitter_percent = 0.0;
        let scheduler = Scheduler::new(
            config,
            Arc::new(SlowClient),
            Arc::new(FrozenClock),
            Arc::new(NoJitter),
        )
        .unwrap();
        let mut monitor = scheduler.start([LineId::new(65, "local")]).unwrap();
        let observations = tokio::time::timeout(Duration::from_secs(1), async {
            (monitor.recv().await.unwrap(), monitor.recv().await.unwrap())
        })
        .await;
        monitor.shutdown().await;
        let (first, second) = observations.expect("零等待不应依赖任何正时长计时器");
        assert!(first.result.is_ok() && second.result.is_ok());
        assert_eq!(second.started_at, first.completed_at, "{mode:?}");
    }
}

#[tokio::test]
async fn both_modes_wait_from_completion_and_accept_zero_delay() {
    for mode in [
        MonitoringMode::ProductDetail,
        MonitoringMode::ListedProducts,
    ] {
        for delay_ms in [0, 30] {
            let mut config = MonitorConfig::default();
            config.monitoring_mode = mode;
            config.schedule.start_minute = 0;
            config.schedule.end_minute = 0;
            config.requests.interval = Duration::from_millis(delay_ms);
            config.requests.jitter_percent = 0.0;
            let scheduler = Scheduler::new(
                config,
                Arc::new(SlowClient),
                Arc::new(TokioClock::default()),
                Arc::new(NoJitter),
            )
            .unwrap();
            let mut monitor = scheduler.start([LineId::new(65, "local")]).unwrap();
            let first = tokio::time::timeout(Duration::from_secs(2), monitor.recv())
                .await
                .unwrap()
                .unwrap();
            let second = tokio::time::timeout(Duration::from_secs(2), monitor.recv())
                .await
                .unwrap()
                .unwrap();
            monitor.shutdown().await;
            assert!(first.result.is_ok() && second.result.is_ok());
            assert!(
                second.started_at >= first.completed_at + Duration::from_millis(delay_ms),
                "{mode:?}：应从完成时刻等待 {delay_ms} ms"
            );
        }
    }
}

#[tokio::test]
async fn shortening_wait_keeps_the_last_completion_as_its_anchor() {
    for mode in [
        MonitoringMode::ProductDetail,
        MonitoringMode::ListedProducts,
    ] {
        let mut config = MonitorConfig::default();
        config.monitoring_mode = mode;
        config.schedule.start_minute = 0;
        config.schedule.end_minute = 0;
        config.requests.interval = Duration::from_secs(1);
        config.requests.jitter_percent = 0.0;
        let scheduler = Scheduler::new(
            config.clone(),
            Arc::new(SlowClient),
            Arc::new(TokioClock::default()),
            Arc::new(NoJitter),
        )
        .unwrap();
        let mut monitor = scheduler.start([LineId::new(65, "local")]).unwrap();
        tokio::time::timeout(Duration::from_secs(2), monitor.recv())
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        config.requests.interval = Duration::from_millis(100);
        monitor.reconfigure(&config).await;
        let next = tokio::time::timeout(Duration::from_millis(80), monitor.recv()).await;
        monitor.shutdown().await;
        assert!(next.is_ok(), "{mode:?}：上次完成后的新等待已过，应立即检查");
    }
}
