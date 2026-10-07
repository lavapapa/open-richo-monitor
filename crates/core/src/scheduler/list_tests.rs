use super::*;
use crate::availability::Availability;
use std::sync::atomic::{AtomicBool, AtomicUsize};

struct FastClock(AtomicU64);
impl SchedulerClock for FastClock {
    fn monotonic_now(&self) -> Duration {
        Duration::from_nanos(self.0.load(Ordering::Relaxed))
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + self.monotonic_now()
    }
    fn sleep_until(&self, deadline: Duration) -> ClockFuture<'_> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            self.0
                .fetch_max(deadline.as_nanos() as u64, Ordering::Relaxed);
        })
    }
}
struct NoJitter;
impl JitterSource for NoJitter {
    fn sample(&self) -> f64 {
        0.0
    }
}

// 墙上时间可独立跳变，单调时钟仍按真实时间推进，模拟校时和唤醒。
struct JumpClock {
    monotonic: TokioClock,
    wall_seconds: AtomicU64,
    sleeping: Notify,
}
impl JumpClock {
    fn at(seconds: u64) -> Arc<Self> {
        Arc::new(Self {
            monotonic: TokioClock::default(),
            wall_seconds: AtomicU64::new(seconds),
            sleeping: Notify::new(),
        })
    }
}
impl SchedulerClock for JumpClock {
    fn monotonic_now(&self) -> Duration {
        self.monotonic.monotonic_now()
    }
    fn wall_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
            + Duration::from_secs(self.wall_seconds.load(Ordering::Relaxed))
            + self.monotonic_now()
    }
    fn sleep_until(&self, deadline: Duration) -> ClockFuture<'_> {
        Box::pin(async move {
            self.sleeping.notify_one();
            self.monotonic.sleep_until(deadline).await;
        })
    }
}
struct Pages {
    pages: Vec<Result<Vec<ProductDetail>, RicohApiError>>,
    calls: AtomicUsize,
    details: AtomicUsize,
    hold_second: AtomicBool,
    second_started: Notify,
    release: Arc<Notify>,
}
impl Pages {
    fn new(pages: Vec<Result<Vec<ProductDetail>, RicohApiError>>) -> Arc<Self> {
        Arc::new(Self {
            pages,
            calls: AtomicUsize::new(0),
            details: AtomicUsize::new(0),
            hold_second: AtomicBool::new(false),
            second_started: Notify::new(),
            release: Arc::new(Notify::new()),
        })
    }
}
impl SchedulerClient for Pages {
    fn product_detail(&self, line: LineId) -> BoxFuture<Result<ProductDetail, RicohApiError>> {
        self.details.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move { Ok(detail(line.product_id)) })
    }
    fn product_page(
        &self,
        _outlet: String,
        page: u32,
        limit: u32,
    ) -> BoxFuture<Result<Vec<ProductDetail>, RicohApiError>> {
        assert_eq!(limit, 20);
        self.calls.fetch_add(1, Ordering::Relaxed);
        let result = self
            .pages
            .get((page - 1) as usize)
            .cloned()
            .unwrap_or(Ok(vec![]));
        let hold = page == 2 && self.hold_second.load(Ordering::Relaxed);
        let release = self.release.clone();
        if hold {
            self.second_started.notify_one();
        }
        Box::pin(async move {
            if hold {
                release.notified().await;
            }
            result
        })
    }
}
fn detail(id: u64) -> ProductDetail {
    ProductDetail {
        product_id: id,
        name: format!("商品{id}"),
        is_show: 1,
        stock: 2.into(),
        availability: Availability::InStock,
        metadata: Default::default(),
    }
}
fn scheduler(client: Arc<Pages>, clock: Arc<dyn SchedulerClock>) -> Scheduler {
    let mut config = MonitorConfig::default();
    config.schedule.start_minute = 0;
    config.schedule.end_minute = 0;
    config.requests.interval = Duration::from_secs(30);
    config.requests.jitter_percent = 0.0;
    config.requests.global_requests_per_second = 1000.0;
    Scheduler::new(config, client, clock, Arc::new(NoJitter)).unwrap()
}

#[tokio::test]
async fn wall_clock_forward_and_midnight_wake_resume_both_monitoring_modes_without_catchup() {
    let monday_eight = 1_704_067_200;
    for mode in [
        MonitoringMode::ListedProducts,
        MonitoringMode::ProductDetail,
    ] {
        for (start_hour, resumed_hour, start_minute, end_minute) in
            [(0, 2, 9 * 60, 19 * 60), (13, 17, 22 * 60, 2 * 60)]
        {
            let clock = JumpClock::at(monday_eight + start_hour * 3600);
            let client = Pages::new(vec![Ok(vec![])]);
            let mut scheduler = scheduler(client.clone(), clock.clone());
            scheduler.monitoring_mode = mode;
            scheduler.schedule = ScheduleConfig {
                enabled_days: [true, false, false, false, false, false, false],
                start_minute,
                end_minute,
            };
            scheduler.gate.set_monitor_schedule(&scheduler.schedule);
            let mut monitor = scheduler.start([LineId::new(65, "direct")]).unwrap();
            clock.sleeping.notified().await;
            assert_eq!(
                client.calls.load(Ordering::Relaxed) + client.details.load(Ordering::Relaxed),
                0
            );
            clock
                .wall_seconds
                .store(monday_eight + resumed_hour * 3600, Ordering::Relaxed);
            let result = tokio::time::timeout(Duration::from_millis(1500), monitor.recv()).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            monitor.shutdown().await;
            assert!(
                result.is_ok_and(|event| event.is_some()),
                "{mode:?} 未及时恢复实际检查"
            );
            assert_eq!(
                client.calls.load(Ordering::Relaxed) + client.details.load(Ordering::Relaxed),
                1
            );
        }
    }
}

#[tokio::test]
async fn wall_clock_backward_pauses_and_forward_resumes_shared_admission() {
    let monday_eight = 1_704_067_200;
    let clock = JumpClock::at(monday_eight + 2 * 3600);
    let config = MonitorConfig::default();
    let gate = Arc::new(RequestGate::new(
        &config.requests,
        config.scan.interval,
        clock.clone(),
    ));
    gate.set_monitor_schedule(&config.schedule);
    let (_control, mut receiver) = ControlToken::new();
    drop(
        gate.acquire_monitor_until(
            &LineId::new(65, "direct"),
            &mut receiver,
            Duration::from_secs(3600),
        )
        .await
        .unwrap(),
    );
    clock.wall_seconds.store(monday_eight, Ordering::Relaxed);
    let worker = tokio::spawn({
        let gate = gate.clone();
        async move {
            gate.acquire_monitor_until(
                &LineId::new(65, "direct"),
                &mut receiver,
                Duration::from_secs(3600),
            )
            .await
        }
    });
    clock.sleeping.notified().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!worker.is_finished(), "后拨至计划外时仍准入请求");
    clock
        .wall_seconds
        .store(monday_eight + 2 * 3600, Ordering::Relaxed);
    let result = tokio::time::timeout(Duration::from_millis(1500), worker).await;
    assert!(
        result.is_ok_and(|result| result.is_ok_and(|permit| permit.is_ok())),
        "计划重新开始后准入仍在等待旧边界"
    );
}
async fn read(client: Arc<Pages>) -> Result<BTreeMap<u64, ProductDetail>, RicohApiError> {
    let scheduler = scheduler(client, Arc::new(FastClock(AtomicU64::new(0))));
    let (_control, mut receiver) = ControlToken::new();
    read_list(
        "direct",
        &scheduler,
        Duration::from_secs(3600),
        &mut receiver,
    )
    .await
}

#[tokio::test]
async fn empty_full_and_short_pages_require_an_empty_terminator() {
    for (pages, ids, calls) in [
        (vec![Ok(vec![])], 0, 1),
        (vec![Ok((1..=20).map(detail).collect()), Ok(vec![])], 20, 2),
        (
            vec![Ok(vec![detail(1)]), Ok(vec![detail(2)]), Ok(vec![])],
            2,
            3,
        ),
    ] {
        let client = Pages::new(pages);
        assert_eq!(read(client.clone()).await.unwrap().len(), ids);
        assert_eq!(client.calls.load(Ordering::Relaxed), calls);
        assert_eq!(client.details.load(Ordering::Relaxed), 0);
    }
}

#[tokio::test]
async fn duplicate_large_page_and_missing_end_are_parse_failures() {
    for pages in [
        vec![Ok(vec![detail(1)]), Ok(vec![detail(1)])],
        vec![Ok((1..=21).map(detail).collect())],
        (1..=51).map(|id| Ok(vec![detail(id)])).collect(),
    ] {
        assert!(matches!(
            read(Pages::new(pages)).await,
            Err(RicohApiError::Parse(_))
        ));
    }
}

#[tokio::test]
async fn thousand_items_plus_empty_page_pass_but_item_1001_fails() {
    let pages = (0..50)
        .map(|page| Ok(((page * 20 + 1)..=(page * 20 + 20)).map(detail).collect()))
        .collect::<Vec<_>>();
    let client = Pages::new(pages.clone());
    assert_eq!(read(client.clone()).await.unwrap().len(), 1000);
    assert_eq!(client.calls.load(Ordering::Relaxed), 51);
    let mut extra = pages;
    extra.push(Ok(vec![detail(1001)]));
    assert!(matches!(
        read(Pages::new(extra)).await,
        Err(RicohApiError::Parse(_))
    ));
}

#[tokio::test]
async fn later_http_failure_stops_and_rate_limit_cools_shared_gate() {
    let client = Pages::new(vec![
        Ok(vec![detail(1)]),
        Err(RicohApiError::Forbidden),
        Ok(vec![detail(2)]),
    ]);
    assert_eq!(
        read(client.clone()).await.unwrap_err(),
        RicohApiError::Forbidden
    );
    assert_eq!(client.calls.load(Ordering::Relaxed), 2);
    let error = RicohApiError::RateLimited {
        retry_after: Some(RetryAfter::Delay(Duration::from_secs(10))),
    };
    let client = Pages::new(vec![Err(error.clone())]);
    let scheduler = scheduler(client.clone(), Arc::new(FastClock(AtomicU64::new(0))));
    let (_control, mut receiver) = ControlToken::new();
    assert_eq!(
        read_list(
            "direct",
            &scheduler,
            Duration::from_secs(3600),
            &mut receiver
        )
        .await
        .unwrap_err(),
        error
    );
    assert_eq!(
        scheduler.gate.timing.lock().unwrap().cooldown_until,
        Some(Duration::from_secs(10))
    );
    assert_eq!(client.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn ten_thousand_local_rounds_have_constant_collection_size() {
    let client = Pages::new(vec![Ok((1..=20).map(detail).collect()), Ok(vec![])]);
    let scheduler = scheduler(client.clone(), Arc::new(FastClock(AtomicU64::new(0))));
    let (_control, mut receiver) = ControlToken::new();
    for _ in 0..10_000 {
        assert_eq!(
            read_list(
                "direct",
                &scheduler,
                Duration::from_secs(3600),
                &mut receiver
            )
            .await
            .unwrap()
            .len(),
            20
        );
    }
    assert_eq!(client.calls.load(Ordering::Relaxed), 20_000);
    assert!(scheduler.health.lock().unwrap().is_empty());
    assert_eq!(scheduler.next_sequence.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn thousand_selected_items_use_one_worker_and_bounded_event_queue() {
    let client = Pages::new(vec![Ok(vec![detail(1)]), Ok(vec![])]);
    let scheduler = scheduler(client.clone(), Arc::new(TokioClock::default()));
    let monitor = scheduler
        .start((1..=1000).map(|id| LineId::new(id, "direct")))
        .unwrap();
    assert_eq!(monitor.tasks.len(), 1);
    assert_eq!(monitor.controls.len(), 1);
    tokio::time::timeout(Duration::from_secs(2), async {
        while monitor.events.len() < EVENT_BUFFER {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(monitor.events.len(), EVENT_BUFFER);
    assert_eq!(client.calls.load(Ordering::Relaxed), 2);
    monitor.shutdown().await;
}

#[tokio::test]
async fn pause_mid_page_discards_partial_round_then_resume_reads_fresh() {
    let client = Pages::new(vec![Ok(vec![detail(1)]), Ok(vec![])]);
    client.hold_second.store(true, Ordering::Relaxed);
    let mut monitor = scheduler(client.clone(), Arc::new(TokioClock::default()))
        .start([LineId::new(1, "direct")])
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), client.second_started.notified())
        .await
        .unwrap();
    monitor.pause();
    client.hold_second.store(false, Ordering::Relaxed);
    client.release.notify_waiters();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(monitor.try_recv().is_none());
    monitor.resume();
    let result = tokio::time::timeout(Duration::from_secs(2), monitor.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.sequence, 2);
    assert_eq!(client.calls.load(Ordering::Relaxed), 4);
    monitor.shutdown().await;
}

#[tokio::test]
async fn selection_change_mid_page_never_publishes_removed_product() {
    let client = Pages::new(vec![Ok(vec![detail(1)]), Ok(vec![])]);
    client.hold_second.store(true, Ordering::Relaxed);
    let mut monitor = scheduler(client.clone(), Arc::new(TokioClock::default()))
        .start([LineId::new(1, "direct")])
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), client.second_started.notified())
        .await
        .unwrap();
    client.hold_second.store(false, Ordering::Relaxed);
    monitor
        .reconcile_lines(vec![LineId::new(2, "direct")], 0, &[])
        .await
        .unwrap();
    client.release.notify_waiters();
    let result = tokio::time::timeout(Duration::from_secs(2), monitor.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.line.product_id, 2);
    assert_eq!(result.sequence, 2);
    assert_eq!(result.result.unwrap(), None);
    assert!(monitor.try_recv().is_none());
    monitor.shutdown().await;
}
