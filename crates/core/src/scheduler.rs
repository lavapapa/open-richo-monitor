use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fmt,
    future::{pending, Future},
    io,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, Instant, SystemTime},
};

use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::{mpsc, watch, Mutex, Notify, OwnedSemaphorePermit, Semaphore};

use crate::{
    config::{ConfigError, MonitorConfig, MonitoringMode, RequestConfig, ScheduleConfig},
    ricoh::ProductDetail,
    ricoh_api::{RetryAfter, RicohApiError},
};

const EVENT_BUFFER: usize = 64;
#[cfg(test)]
mod list_tests;
const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
const SCHEDULE_RECHECK_INTERVAL: Duration = Duration::from_secs(1);
const BEIJING_UTC_OFFSET_SECONDS: i128 = 8 * 60 * 60;
const NANOS_PER_SECOND: i128 = 1_000_000_000;
const NANOS_PER_MINUTE: i128 = 60 * NANOS_PER_SECOND;
const NANOS_PER_DAY: i128 = 24 * 60 * 60 * NANOS_PER_SECOND;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
type ClockFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LineId {
    pub product_id: u64,
    pub outlet_id: String,
}

impl LineId {
    pub fn new(product_id: u64, outlet_id: impl Into<String>) -> Self {
        Self {
            product_id,
            outlet_id: outlet_id.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Running,
    Paused,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct ControlToken {
    sender: watch::Sender<RunState>,
}

impl ControlToken {
    pub fn new() -> (Self, watch::Receiver<RunState>) {
        let (sender, receiver) = watch::channel(RunState::Running);
        (Self { sender }, receiver)
    }

    pub fn pause(&self) {
        if *self.sender.borrow() != RunState::Cancelled {
            self.sender.send_replace(RunState::Paused);
        }
    }

    pub fn resume(&self) {
        if *self.sender.borrow() != RunState::Cancelled {
            self.sender.send_replace(RunState::Running);
        }
    }

    pub fn cancel(&self) {
        self.sender.send_replace(RunState::Cancelled);
    }
}

pub trait SchedulerClock: Send + Sync + 'static {
    fn monotonic_now(&self) -> Duration;
    fn wall_now(&self) -> SystemTime;
    fn sleep_until(&self, deadline: Duration) -> ClockFuture<'_>;
}

#[derive(Debug)]
pub struct TokioClock {
    origin: Instant,
}

impl Default for TokioClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl SchedulerClock for TokioClock {
    fn monotonic_now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn wall_now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep_until(&self, deadline: Duration) -> ClockFuture<'_> {
        let delay = deadline.saturating_sub(self.monotonic_now());
        Box::pin(tokio::time::sleep(delay))
    }
}

/// 返回 [-1, 1] 的抖动样本；调度器按配置的百分比缩放基础间隔。
pub trait JitterSource: Send + Sync + 'static {
    fn sample(&self) -> f64;
}

/// 使用当前操作系统的随机源生成均匀抖动。
pub struct OsJitter {
    random: SystemRandom,
}

impl OsJitter {
    /// 初始化并验证操作系统随机源。
    pub fn new() -> io::Result<Self> {
        let random = SystemRandom::new();
        random
            .fill(&mut [0; 8])
            .map_err(|_| io::Error::other("无法读取 OS 随机源"))?;
        Ok(Self { random })
    }
}

impl JitterSource for OsJitter {
    fn sample(&self) -> f64 {
        let mut bytes = [0; 8];
        self.random.fill(&mut bytes).expect("无法读取 OS 随机源");
        let unit = u64::from_ne_bytes(bytes) as f64 / u64::MAX as f64;
        unit.mul_add(2.0, -1.0)
    }
}

pub trait SchedulerClient: Send + Sync + 'static {
    fn product_detail(&self, line: LineId) -> BoxFuture<Result<ProductDetail, RicohApiError>>;

    fn product_page(
        &self,
        _outlet_id: String,
        _page: u32,
        _limit: u32,
    ) -> BoxFuture<Result<Vec<ProductDetail>, RicohApiError>> {
        Box::pin(async { Err(RicohApiError::InvalidConfiguration) })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RequestCompleted {
    pub line: LineId,
    pub generation: u64,
    pub sequence: u64,
    pub started_at: Duration,
    pub completed_at: Duration,
    pub completed_at_ms: i64,
    pub health_transition: Option<MonitoringHealthTransition>,
    pub health: MonitoringHealth,
    /// None 表示完整成功列表中未出现此商品，库存未知。
    pub result: Result<Option<ProductDetail>, RicohApiError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitoringHealthTransition {
    Failed { since_ms: i64 },
    Recovered,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonitoringHealth {
    pub failed_since_ms: Option<i64>,
    pub active_failure_ms: u64,
    pub failure_reported: bool,
}

#[derive(Default)]
struct ProductHealth {
    sequence: u64,
    persisted: MonitoringHealth,
    last_failure_at: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerError {
    InvalidConfiguration(ConfigError),
    DuplicateLine,
}

#[derive(Clone)]
pub struct Scheduler {
    monitoring_mode: MonitoringMode,
    schedule: ScheduleConfig,
    polling: watch::Sender<(RequestConfig, Duration)>,
    client: Arc<dyn SchedulerClient>,
    clock: Arc<dyn SchedulerClock>,
    jitter: Arc<dyn JitterSource>,
    gate: Arc<RequestGate>,
    next_sequence: Arc<AtomicU64>,
    health: Arc<StdMutex<HashMap<u64, ProductHealth>>>,
    product_generations: Arc<StdMutex<BTreeMap<u64, u64>>>,
}

impl Scheduler {
    pub fn new(
        config: MonitorConfig,
        client: Arc<dyn SchedulerClient>,
        clock: Arc<dyn SchedulerClock>,
        jitter: Arc<dyn JitterSource>,
    ) -> Result<Self, SchedulerError> {
        config
            .validate()
            .map_err(SchedulerError::InvalidConfiguration)?;
        let gate = Arc::new(RequestGate::new(
            &config.requests,
            config.scan.interval,
            clock.clone(),
        ));
        Self::new_with_gate(config, client, jitter, gate)
    }

    pub fn new_with_gate(
        config: MonitorConfig,
        client: Arc<dyn SchedulerClient>,
        jitter: Arc<dyn JitterSource>,
        gate: Arc<RequestGate>,
    ) -> Result<Self, SchedulerError> {
        config
            .validate()
            .map_err(SchedulerError::InvalidConfiguration)?;
        Ok(Self {
            monitoring_mode: config.monitoring_mode,
            schedule: config.schedule,
            polling: watch::channel((config.requests, config.failure_alert_after)).0,
            client,
            // 截止时间与共享门控使用同一时钟域，暂停恢复后仍沿用 App 的起点。
            clock: gate.clock.clone(),
            jitter,
            gate,
            next_sequence: Arc::new(AtomicU64::new(0)),
            health: Arc::new(StdMutex::new(HashMap::new())),
            product_generations: Arc::new(StdMutex::new(BTreeMap::new())),
        })
    }

    pub fn with_product_generations(
        mut self,
        generations: Arc<StdMutex<BTreeMap<u64, u64>>>,
    ) -> Self {
        self.product_generations = generations;
        self
    }

    pub fn with_health(self, saved: HashMap<u64, (u64, MonitoringHealth)>) -> Self {
        *self.health.lock().expect("商品健康状态锁已中毒") = saved
            .into_iter()
            .map(|(id, (sequence, persisted))| {
                (
                    id,
                    ProductHealth {
                        sequence,
                        persisted,
                        last_failure_at: None,
                    },
                )
            })
            .collect();
        self
    }

    pub fn start(
        &self,
        lines: impl IntoIterator<Item = LineId>,
    ) -> Result<ScheduledMonitor, SchedulerError> {
        self.start_with_sequence_floor(lines, 0)
    }

    /// 以已持久化的最高请求序号为下限，供重启后的 reducer 接受新观察。
    pub fn start_with_sequence_floor(
        &self,
        lines: impl IntoIterator<Item = LineId>,
        sequence_floor: u64,
    ) -> Result<ScheduledMonitor, SchedulerError> {
        let lines = lines.into_iter().collect::<Vec<_>>();
        if lines.iter().collect::<HashSet<_>>().len() != lines.len() {
            return Err(SchedulerError::DuplicateLine);
        }
        self.next_sequence
            .fetch_max(sequence_floor, Ordering::AcqRel);

        let (event_tx, event_rx) = mpsc::channel(EVENT_BUFFER);
        let mut monitor = ScheduledMonitor {
            scheduler: self.clone(),
            controls: HashMap::with_capacity(lines.len()),
            tasks: HashMap::with_capacity(lines.len()),
            event_tx,
            events: event_rx,
            pending: VecDeque::new(),
            lines: watch::channel(lines.clone()).0,
        };
        for line in lines {
            monitor.spawn_line(line);
        }
        Ok(monitor)
    }
}

pub struct ScheduledMonitor {
    scheduler: Scheduler,
    controls: HashMap<LineId, ControlToken>,
    tasks: HashMap<LineId, tokio::task::JoinHandle<()>>,
    event_tx: mpsc::Sender<RequestCompleted>,
    events: mpsc::Receiver<RequestCompleted>,
    pending: VecDeque<RequestCompleted>,
    lines: watch::Sender<Vec<LineId>>,
}

impl ScheduledMonitor {
    pub async fn check_tasks(&mut self) -> Result<(), String> {
        let ended = self
            .tasks
            .iter()
            .find(|(_, task)| task.is_finished())
            .map(|(line, _)| line.clone());
        if let Some(line) = ended {
            let result = self
                .tasks
                .remove(&line)
                .expect("已发现结束的商品任务")
                .await;
            let task_name = if self.scheduler.monitoring_mode == MonitoringMode::ListedProducts {
                format!("出口 {} 的上架列表监控", line.outlet_id)
            } else {
                format!("商品 {} 的监控", line.product_id)
            };
            return Err(match result {
                Ok(()) => format!("{task_name}任务提前结束"),
                Err(error) => format!("{task_name}任务异常结束：{error}"),
            });
        }
        Ok(())
    }

    fn spawn_line(&mut self, line: LineId) {
        let line = if self.scheduler.monitoring_mode == MonitoringMode::ListedProducts {
            LineId::new(0, line.outlet_id)
        } else {
            line
        };
        if self.controls.contains_key(&line) {
            return;
        }
        let (control, receiver) = ControlToken::new();
        if self.scheduler.monitoring_mode == MonitoringMode::ListedProducts {
            self.tasks.insert(
                line.clone(),
                tokio::spawn(run_list(
                    line.outlet_id.clone(),
                    self.scheduler.clone(),
                    self.lines.subscribe(),
                    self.event_tx.clone(),
                    receiver,
                )),
            );
            self.controls.insert(line, control);
            return;
        }
        let generation = self
            .scheduler
            .product_generations
            .lock()
            .expect("商品生命周期锁已中毒")
            .get(&line.product_id)
            .copied()
            .unwrap_or(0);
        self.tasks.insert(
            line.clone(),
            tokio::spawn(run_line(
                line.clone(),
                self.scheduler.schedule.clone(),
                self.scheduler.polling.subscribe(),
                self.scheduler.client.clone(),
                self.scheduler.clock.clone(),
                self.scheduler.jitter.clone(),
                self.scheduler.gate.clone(),
                self.scheduler.next_sequence.clone(),
                self.scheduler.health.clone(),
                self.scheduler.product_generations.clone(),
                generation,
                self.event_tx.clone(),
                receiver,
            )),
        );
        self.controls.insert(line, control);
    }

    pub async fn reconcile_lines(
        &mut self,
        lines: impl IntoIterator<Item = LineId>,
        sequence_floor: u64,
        reset_products: &[u64],
    ) -> Result<(), SchedulerError> {
        let lines = lines.into_iter().collect::<Vec<_>>();
        if lines.iter().collect::<HashSet<_>>().len() != lines.len() {
            return Err(SchedulerError::DuplicateLine);
        }
        let lines = lines.into_iter().collect::<HashSet<_>>();
        let work_lines = lines
            .iter()
            .map(|line| {
                if self.scheduler.monitoring_mode == MonitoringMode::ListedProducts {
                    LineId::new(0, line.outlet_id.clone())
                } else {
                    line.clone()
                }
            })
            .collect::<HashSet<_>>();
        let next_lines = lines.iter().cloned().collect::<Vec<_>>();
        if self.lines.borrow().iter().cloned().collect::<HashSet<_>>() != lines
            || !reset_products.is_empty()
        {
            self.lines.send_replace(next_lines);
        }
        self.scheduler
            .next_sequence
            .fetch_max(sequence_floor, Ordering::AcqRel);
        for line in self
            .controls
            .keys()
            .filter(|line| !work_lines.contains(*line) || reset_products.contains(&line.product_id))
        {
            self.controls.get(line).unwrap().cancel();
        }
        let removed = self
            .tasks
            .keys()
            .filter(|line| !work_lines.contains(*line) || reset_products.contains(&line.product_id))
            .cloned()
            .collect::<Vec<_>>();
        for line in removed {
            self.controls.remove(&line);
            if let Some(task) = self.tasks.remove(&line) {
                let _ = task.await;
            }
        }
        self.pending.retain(|event| {
            lines.contains(&event.line) && !reset_products.contains(&event.line.product_id)
        });
        while let Ok(event) = self.events.try_recv() {
            if lines.contains(&event.line) && !reset_products.contains(&event.line.product_id) {
                self.pending.push_back(event);
            }
        }
        self.scheduler
            .health
            .lock()
            .expect("商品健康状态锁已中毒")
            .retain(|product_id, _| {
                !reset_products.contains(product_id)
                    && lines.iter().any(|line| line.product_id == *product_id)
            });
        for line in lines {
            if !self.controls.contains_key(&line) {
                self.spawn_line(line);
            }
        }
        Ok(())
    }

    pub async fn reconfigure(&mut self, config: &MonitorConfig) {
        if self.scheduler.schedule != config.schedule
            || self.scheduler.monitoring_mode != config.monitoring_mode
        {
            self.cancel();
            for (_, task) in self.tasks.drain() {
                let _ = task.await;
            }
            let lines = self.lines.borrow().clone();
            self.controls.clear();
            self.pending.clear();
            while self.events.try_recv().is_ok() {}
            self.scheduler.schedule = config.schedule.clone();
            self.scheduler.monitoring_mode = config.monitoring_mode;
            for state in self
                .scheduler
                .health
                .lock()
                .expect("商品健康状态锁已中毒")
                .values_mut()
            {
                state.last_failure_at = None;
            }
            self.scheduler
                .polling
                .send_replace((config.requests.clone(), config.failure_alert_after));
            for line in lines {
                self.spawn_line(line);
            }
        } else {
            self.scheduler
                .polling
                .send_replace((config.requests.clone(), config.failure_alert_after));
        }
    }

    pub async fn recv(&mut self) -> Option<RequestCompleted> {
        match self.pending.pop_front() {
            Some(event) => Some(event),
            None => self.events.recv().await,
        }
    }

    pub fn try_recv(&mut self) -> Option<RequestCompleted> {
        self.pending
            .pop_front()
            .or_else(|| self.events.try_recv().ok())
    }

    pub fn pause(&self) {
        for control in self.controls.values() {
            control.pause();
        }
    }

    pub fn resume(&self) {
        for control in self.controls.values() {
            control.resume();
        }
    }

    pub fn cancel(&self) {
        for control in self.controls.values() {
            control.cancel();
        }
    }

    pub async fn shutdown(mut self) {
        self.cancel();
        for (_, task) in self.tasks.drain() {
            let _ = task.await;
        }
    }
}

impl Drop for ScheduledMonitor {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcquireError {
    Cancelled,
    WindowEnded,
}

impl fmt::Display for AcquireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Cancelled => "请求已取消",
            Self::WindowEnded => "请求已超出计划时段",
        })
    }
}

struct GateTiming {
    scan_interval: Duration,
    next_scan: Duration,
    cooldown_until: Option<Duration>,
}

pub struct RequestGate {
    requests: Arc<Semaphore>,
    capacity: u32,
    admission: Mutex<()>,
    timing: StdMutex<GateTiming>,
    monitor_schedule: StdMutex<Option<ScheduleConfig>>,
    changed: Notify,
    clock: Arc<dyn SchedulerClock>,
}

impl RequestGate {
    pub fn new(
        requests: &RequestConfig,
        scan_interval: Duration,
        clock: Arc<dyn SchedulerClock>,
    ) -> Self {
        let now = clock.monotonic_now();
        Self {
            requests: Arc::new(Semaphore::new(requests.max_concurrent_requests as usize)),
            capacity: requests.max_concurrent_requests,
            admission: Mutex::new(()),
            timing: StdMutex::new(GateTiming {
                scan_interval,
                next_scan: now,
                cooldown_until: None,
            }),
            monitor_schedule: StdMutex::new(None),
            changed: Notify::new(),
            clock,
        }
    }

    pub fn reconfigure(&self, scan_interval: Duration) {
        let mut timing = self.timing.lock().expect("请求节奏锁已中毒");
        timing.scan_interval = scan_interval;
        drop(timing);
        self.changed.notify_waiters();
    }

    pub(crate) fn try_reserve_idle(&self) -> Option<OwnedSemaphorePermit> {
        self.requests
            .clone()
            .try_acquire_many_owned(self.capacity)
            .ok()
    }

    pub(crate) fn set_monitor_schedule(&self, schedule: &ScheduleConfig) {
        let mut current = self.monitor_schedule.lock().expect("监控计划锁已中毒");
        if current.as_ref() == Some(schedule) {
            return;
        }
        *current = Some(schedule.clone());
        drop(current);
        self.changed.notify_waiters();
    }

    pub async fn acquire_scan(
        &self,
        _product_id: u64,
        control: &mut watch::Receiver<RunState>,
    ) -> Result<OwnedSemaphorePermit, AcquireError> {
        self.acquire(control, None, true).await
    }

    async fn acquire_monitor_until(
        &self,
        _line: &LineId,
        control: &mut watch::Receiver<RunState>,
        window_end: Duration,
    ) -> Result<OwnedSemaphorePermit, AcquireError> {
        self.acquire(control, Some(window_end), false).await
    }

    pub async fn cool_domain(&self, retry_after: Option<&RetryAfter>) -> Duration {
        let delay = retry_after_delay(retry_after, self.clock.wall_now());
        if delay.is_zero() {
            return delay;
        }
        let until = self.clock.monotonic_now().saturating_add(delay);
        let mut timing = self.timing.lock().expect("请求节奏锁已中毒");
        timing.cooldown_until = Some(timing.cooldown_until.map_or(until, |old| old.max(until)));
        drop(timing);
        self.changed.notify_waiters();
        delay
    }

    async fn acquire(
        &self,
        control: &mut watch::Receiver<RunState>,
        window_end: Option<Duration>,
        scan: bool,
    ) -> Result<OwnedSemaphorePermit, AcquireError> {
        'acquire: loop {
            if window_end.is_some_and(|end| self.clock.monotonic_now() >= end) {
                return Err(AcquireError::WindowEnded);
            }
            wait_for_running(control)
                .await
                .map_err(|()| AcquireError::Cancelled)?;

            let request = self.requests.clone().acquire_owned();
            let permit = tokio::select! {
                biased;
                changed = control.changed() => {
                    if changed.is_err() { return Err(AcquireError::Cancelled); }
                    continue;
                }
                _ = sleep_optional(self.clock.as_ref(), window_end) => {
                    return Err(AcquireError::WindowEnded);
                }
                result = request => result.map_err(|_| AcquireError::Cancelled)?,
            };

            let turn = tokio::select! {
                biased;
                changed = control.changed() => {
                    if changed.is_err() { return Err(AcquireError::Cancelled); }
                    continue;
                }
                _ = sleep_optional(self.clock.as_ref(), window_end) => {
                    return Err(AcquireError::WindowEnded);
                }
                turn = self.admission.lock() => turn,
            };

            loop {
                if window_end.is_some_and(|end| self.clock.monotonic_now() >= end) {
                    return Err(AcquireError::WindowEnded);
                }
                if *control.borrow() != RunState::Running {
                    drop(turn);
                    drop(permit);
                    wait_for_running(control)
                        .await
                        .map_err(|()| AcquireError::Cancelled)?;
                    continue 'acquire;
                }
                let now = self.clock.monotonic_now();
                if scan {
                    let next_scan = self.timing.lock().expect("请求节奏锁已中毒").next_scan;
                    if now < next_scan {
                        drop(turn);
                        drop(permit);
                        tokio::select! {
                            biased;
                            changed = control.changed() => {
                                if changed.is_err() { return Err(AcquireError::Cancelled); }
                            }
                            _ = sleep_optional(self.clock.as_ref(), window_end) => {
                                return Err(AcquireError::WindowEnded);
                            }
                            _ = self.clock.sleep_until(next_scan) => {}
                        }
                        continue 'acquire;
                    }
                }
                let notified = self.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let outside_schedule = (!scan)
                    .then(|| {
                        self.monitor_schedule
                            .lock()
                            .expect("监控计划锁已中毒")
                            .as_ref()
                            .map(|schedule| schedule_state(schedule, self.clock.wall_now()))
                    })
                    .flatten()
                    .filter(|(active, _)| !active);
                if let Some((_, delay)) = outside_schedule {
                    drop(turn);
                    drop(permit);
                    tokio::select! {
                        biased;
                        changed = control.changed() => {
                            if changed.is_err() { return Err(AcquireError::Cancelled); }
                        }
                        _ = notified => {}
                        _ = sleep_optional(self.clock.as_ref(), window_end) => {
                            return Err(AcquireError::WindowEnded);
                        }
                        _ = self.clock.sleep_until(now.saturating_add(delay.min(SCHEDULE_RECHECK_INTERVAL))) => {}
                    }
                    continue 'acquire;
                }
                let deadline = {
                    let mut timing = self.timing.lock().expect("请求节奏锁已中毒");
                    timing.cooldown_until = timing.cooldown_until.filter(|until| *until > now);
                    let mut deadline = timing.cooldown_until.unwrap_or(now);
                    if now >= deadline {
                        if scan {
                            timing.next_scan = now.saturating_add(timing.scan_interval);
                        }
                        return Ok(permit);
                    }
                    if let Some(end) = window_end {
                        deadline = deadline.min(end);
                    }
                    deadline
                };
                tokio::select! {
                    biased;
                    changed = control.changed() => {
                        if changed.is_err() { return Err(AcquireError::Cancelled); }
                    }
                    _ = notified => {}
                    _ = sleep_optional(self.clock.as_ref(), Some(deadline)) => {}
                }
            }
        }
    }
}

const LIST_PAGE_LIMIT: u32 = 20;
const MAX_LIST_PRODUCTS: usize = 1000;
const MAX_LIST_PAGES: u32 = 51;
const MAX_LIST_ROUND_TIME: Duration = Duration::from_secs(60);

async fn read_list(
    outlet: &str,
    scheduler: &Scheduler,
    window_end: Duration,
    control: &mut watch::Receiver<RunState>,
) -> Result<Option<BTreeMap<u64, ProductDetail>>, RicohApiError> {
    let mut products = BTreeMap::new();
    for page in 1..=MAX_LIST_PAGES {
        let permit = match scheduler
            .gate
            .acquire(control, Some(window_end), false)
            .await
        {
            Ok(permit) => permit,
            // 计划结束或用户停止时丢弃未完整读取的列表，不计入网络失败。
            Err(AcquireError::WindowEnded | AcquireError::Cancelled) => return Ok(None),
        };
        let result = scheduler
            .client
            .product_page(outlet.to_owned(), page, LIST_PAGE_LIMIT)
            .await;
        drop(permit);
        if let Err(RicohApiError::RateLimited { retry_after }) = &result {
            scheduler.gate.cool_domain(retry_after.as_ref()).await;
        }
        let items = result?;
        if items.is_empty() {
            return Ok(Some(products));
        }
        if items.len() > LIST_PAGE_LIMIT as usize {
            return Err(RicohApiError::Parse(
                crate::ricoh::RicohParseError::InvalidValue {
                    field: "data",
                    expected: "单页不超过 20 件商品，整轮结果未采用",
                },
            ));
        }
        for detail in items {
            if products.insert(detail.product_id, detail).is_some() {
                return Err(RicohApiError::Parse(
                    crate::ricoh::RicohParseError::InvalidValue {
                        field: "productId",
                        expected: "完整列表中商品 ID 唯一，整轮结果未采用",
                    },
                ));
            }
        }
        if products.len() > MAX_LIST_PRODUCTS {
            return Err(RicohApiError::Parse(
                crate::ricoh::RicohParseError::InvalidValue {
                    field: "data",
                    expected: "完整列表不超过 1000 件商品，整轮结果未采用",
                },
            ));
        }
    }
    Err(RicohApiError::Parse(
        crate::ricoh::RicohParseError::InvalidValue {
            field: "page",
            expected: "51 页内出现结束空页，整轮结果未采用",
        },
    ))
}

async fn run_list(
    outlet: String,
    scheduler: Scheduler,
    mut lines: watch::Receiver<Vec<LineId>>,
    events: mpsc::Sender<RequestCompleted>,
    mut control: watch::Receiver<RunState>,
) {
    let mut polling = scheduler.polling.subscribe();
    let mut due = scheduler.clock.monotonic_now();
    let mut last_completed = None;
    let mut failures = 0_u32;
    let mut previous_window_end_wall: Option<SystemTime> = None;
    'round: loop {
        let Some(window_end) =
            wait_until_schedule(&scheduler.schedule, scheduler.clock.as_ref(), &mut control).await
        else {
            return;
        };
        let targets = lines
            .borrow_and_update()
            .iter()
            .filter(|line| line.outlet_id == outlet)
            .cloned()
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return;
        }
        let wall_now = scheduler.clock.wall_now();
        if previous_window_end_wall.is_some_and(|boundary| {
            wall_now >= boundary
                && boundary
                    .checked_add(Duration::from_secs(1))
                    .is_some_and(|after| !schedule_state(&scheduler.schedule, after).0)
        }) {
            for target in &targets {
                clear_health_anchor(&scheduler.health, target.product_id);
            }
        }
        previous_window_end_wall =
            wall_now.checked_add(window_end.saturating_sub(scheduler.clock.monotonic_now()));
        tokio::select! {
            biased;
            changed = lines.changed() => { if changed.is_err() { return; } due = scheduler.clock.monotonic_now(); continue; }
            changed = polling.changed() => {
                if changed.is_err() { return; }
                let requests = polling.borrow_and_update().0.clone();
                let now = scheduler.clock.monotonic_now();
                due = match last_completed {
                    Some(completed) => failure_backoff_due(next_due(completed, jittered_interval(requests.interval,
                        requests.jitter_percent, scheduler.jitter.sample())), completed, failures,
                        requests.failures_before_backoff, requests.failure_backoff),
                    None => now,
                };
                continue;
            }
            running = wait_until(scheduler.clock.as_ref(), due, &mut control) => { if !running { return; } }
        }
        if scheduler.clock.monotonic_now() >= window_end {
            continue;
        }
        let generations = {
            let current = scheduler
                .product_generations
                .lock()
                .expect("商品生命周期锁已中毒");
            targets
                .iter()
                .map(|line| {
                    (
                        line.product_id,
                        current.get(&line.product_id).copied().unwrap_or(0),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        let started_at = scheduler.clock.monotonic_now();
        let request_sequence = scheduler
            .next_sequence
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let mut page_control = control.clone();
        let result = tokio::select! {
            biased;
            changed = control.changed() => {
                if changed.is_err() || *control.borrow() == RunState::Cancelled { return; }
                for target in &targets { clear_health_anchor(&scheduler.health, target.product_id); }
                continue;
            }
            changed = lines.changed() => { if changed.is_err() { return; } due = scheduler.clock.monotonic_now(); continue; }
            _ = scheduler.clock.sleep_until(window_end) => { continue; }
            _ = scheduler.clock.sleep_until(started_at.saturating_add(MAX_LIST_ROUND_TIME)) => {
                Err(RicohApiError::Transport("上架列表整轮读取超过 60 秒，整轮结果未采用".into()))
            }
            result = read_list(&outlet, &scheduler, window_end, &mut page_control) => match result {
                Ok(Some(products)) => Ok(products),
                Ok(None) => continue,
                Err(error) => Err(error),
            },
        };
        let completed_at = scheduler.clock.monotonic_now();
        last_completed = Some(completed_at);
        let completed_at_ms = unix_time_ms(scheduler.clock.wall_now());
        let (requests, alert_after) = polling.borrow_and_update().clone();
        failures = if result.is_ok() {
            0
        } else {
            failures.saturating_add(1)
        };
        // 完成后再等待一次轮次间隔，翻页较慢时也不会连续突发下一轮。
        due = failure_backoff_due(
            completed_at.saturating_add(jittered_interval(
                requests.interval,
                requests.jitter_percent,
                scheduler.jitter.sample(),
            )),
            completed_at,
            failures,
            requests.failures_before_backoff,
            requests.failure_backoff,
        );
        for line in targets {
            let generation = generations[&line.product_id];
            let (transition, health) = {
                let current = scheduler
                    .product_generations
                    .lock()
                    .expect("商品生命周期锁已中毒");
                if current.get(&line.product_id).copied().unwrap_or(0) != generation {
                    continue;
                }
                record_health(
                    &scheduler.health,
                    line.product_id,
                    request_sequence,
                    completed_at_ms,
                    completed_at,
                    result.is_ok(),
                    alert_after,
                )
            };
            let item = match &result {
                Ok(products) => Ok(products.get(&line.product_id).cloned()),
                Err(error) => Err(error.clone()),
            };
            let event = RequestCompleted {
                line,
                generation,
                sequence: request_sequence,
                started_at,
                completed_at,
                completed_at_ms,
                health_transition: transition,
                health,
                result: item,
            };
            tokio::select! {
                biased;
                changed = lines.changed() => { if changed.is_err() { return; } due = scheduler.clock.monotonic_now(); continue 'round; }
                sent = send_event(&events, event, &mut control) => { if !sent { return; } }
            }
        }
    }
}

async fn run_line(
    line: LineId,
    schedule: ScheduleConfig,
    mut polling: watch::Receiver<(RequestConfig, Duration)>,
    client: Arc<dyn SchedulerClient>,
    clock: Arc<dyn SchedulerClock>,
    jitter: Arc<dyn JitterSource>,
    gate: Arc<RequestGate>,
    sequence: Arc<AtomicU64>,
    health: Arc<StdMutex<HashMap<u64, ProductHealth>>>,
    product_generations: Arc<StdMutex<BTreeMap<u64, u64>>>,
    generation: u64,
    events: mpsc::Sender<RequestCompleted>,
    mut control: watch::Receiver<RunState>,
) {
    let mut due = clock.monotonic_now();
    let mut last_completed = None;
    let mut consecutive_failures = 0_u32;
    let mut previous_window_end_wall: Option<SystemTime> = None;
    loop {
        let window_end = match wait_until_schedule(&schedule, clock.as_ref(), &mut control).await {
            Some(deadline) => deadline,
            None => return,
        };
        let wall_now = clock.wall_now();
        if previous_window_end_wall.is_some_and(|boundary| {
            wall_now >= boundary
                && boundary
                    .checked_add(Duration::from_secs(1))
                    .is_some_and(|after| !schedule_state(&schedule, after).0)
        }) {
            clear_health_anchor(&health, line.product_id);
        }
        previous_window_end_wall =
            wall_now.checked_add(window_end.saturating_sub(clock.monotonic_now()));
        tokio::select! {
            changed = polling.changed() => {
                if changed.is_err() { return; }
                let requests = polling.borrow_and_update().0.clone();
                let now = clock.monotonic_now();
                due = match last_completed {
                    Some(completed) => failure_backoff_due(
                        next_due(completed, jittered_interval(requests.interval, requests.jitter_percent, jitter.sample())),
                        completed, consecutive_failures, requests.failures_before_backoff, requests.failure_backoff,
                    ),
                    None => now,
                };
                continue;
            }
            running = wait_until(clock.as_ref(), due, &mut control) => {
                if !running { return; }
            }
        }

        let permit = match gate
            .acquire_monitor_until(&line, &mut control, window_end)
            .await
        {
            Ok(permit) => permit,
            Err(AcquireError::Cancelled) => return,
            Err(AcquireError::WindowEnded) => continue,
        };
        if clock.monotonic_now() >= window_end {
            drop(permit);
            continue;
        }
        if product_generations
            .lock()
            .expect("商品生命周期锁已中毒")
            .get(&line.product_id)
            .copied()
            .unwrap_or(0)
            != generation
        {
            return;
        }
        let started_at = clock.monotonic_now();
        let request_sequence = sequence.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let request = client.product_detail(line.clone());
        let result = tokio::select! {
            biased;
            changed = control.changed() => {
                drop(permit);
                if changed.is_err() || *control.borrow() == RunState::Cancelled {
                    return;
                }
                continue;
            }
            _ = clock.sleep_until(window_end) => {
                drop(permit);
                continue;
            }
            result = request => result,
        };
        let completed_at = clock.monotonic_now();
        last_completed = Some(completed_at);
        let completed_at_ms = unix_time_ms(clock.wall_now());
        let (requests, failure_alert_after) = polling.borrow_and_update().clone();
        let interval =
            jittered_interval(requests.interval, requests.jitter_percent, jitter.sample());
        let (generation_current, health_transition, health_snapshot) = {
            let current_generation = product_generations.lock().expect("商品生命周期锁已中毒");
            if current_generation
                .get(&line.product_id)
                .copied()
                .unwrap_or(0)
                == generation
            {
                let recorded = record_health(
                    &health,
                    line.product_id,
                    request_sequence,
                    completed_at_ms,
                    completed_at,
                    result
                        .as_ref()
                        .is_ok_and(|detail| detail.product_id == line.product_id),
                    failure_alert_after,
                );
                (true, recorded.0, recorded.1)
            } else {
                (false, None, MonitoringHealth::default())
            }
        };
        if generation_current {
            if let Err(RicohApiError::RateLimited { retry_after }) = &result {
                gate.cool_domain(retry_after.as_ref()).await;
            }
        }
        due = next_due(completed_at, interval);
        if result.is_ok() {
            consecutive_failures = 0;
        } else {
            consecutive_failures = consecutive_failures.saturating_add(1);
        }
        due = failure_backoff_due(
            due,
            completed_at,
            consecutive_failures,
            requests.failures_before_backoff,
            requests.failure_backoff,
        );
        drop(permit);

        if !send_event(
            &events,
            RequestCompleted {
                line: line.clone(),
                generation,
                sequence: request_sequence,
                started_at,
                completed_at,
                completed_at_ms,
                health_transition,
                health: health_snapshot,
                result: result.map(Some),
            },
            &mut control,
        )
        .await
        {
            return;
        }
    }
}

fn clear_health_anchor(health: &StdMutex<HashMap<u64, ProductHealth>>, product_id: u64) {
    if let Some(state) = health
        .lock()
        .expect("商品健康状态锁已中毒")
        .get_mut(&product_id)
    {
        state.last_failure_at = None;
    }
}

fn record_health(
    health: &StdMutex<HashMap<u64, ProductHealth>>,
    product_id: u64,
    sequence: u64,
    at_ms: i64,
    at: Duration,
    succeeded: bool,
    alert_after: Duration,
) -> (Option<MonitoringHealthTransition>, MonitoringHealth) {
    let mut health = health.lock().expect("商品健康状态锁已中毒");
    let state = health.entry(product_id).or_default();
    if sequence <= state.sequence {
        return (None, state.persisted);
    }
    state.sequence = sequence;
    if succeeded {
        let transition = state
            .persisted
            .failure_reported
            .then_some(MonitoringHealthTransition::Recovered);
        state.persisted = MonitoringHealth::default();
        state.last_failure_at = None;
        return (transition, state.persisted);
    }

    let since_ms = *state.persisted.failed_since_ms.get_or_insert(at_ms);
    if let Some(previous) = state.last_failure_at {
        let active_gap = at.saturating_sub(previous);
        state.persisted.active_failure_ms = state
            .persisted
            .active_failure_ms
            .saturating_add(active_gap.as_millis().min(u64::MAX as u128) as u64);
    }
    state.last_failure_at = Some(at);
    if !state.persisted.failure_reported
        && state.persisted.active_failure_ms >= alert_after.as_millis() as u64
    {
        state.persisted.failure_reported = true;
        (
            Some(MonitoringHealthTransition::Failed { since_ms }),
            state.persisted,
        )
    } else {
        (None, state.persisted)
    }
}

fn unix_time_ms(time: SystemTime) -> i64 {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

async fn wait_until_running(control: &mut watch::Receiver<RunState>) -> bool {
    loop {
        let state = *control.borrow();
        match state {
            RunState::Running => return true,
            RunState::Cancelled => return false,
            RunState::Paused => {
                if control.changed().await.is_err() {
                    return false;
                }
            }
        }
    }
}

async fn wait_until_schedule(
    schedule: &ScheduleConfig,
    clock: &dyn SchedulerClock,
    control: &mut watch::Receiver<RunState>,
) -> Option<Duration> {
    loop {
        if !wait_until_running(control).await {
            return None;
        }
        let (active, until_transition) = schedule_state(schedule, clock.wall_now());
        let deadline = clock.monotonic_now().saturating_add(until_transition);
        if active {
            return Some(deadline);
        }
        tokio::select! {
            biased;
            changed = control.changed() => {
                if changed.is_err() || *control.borrow() == RunState::Cancelled {
                    return None;
                }
            }
            // 校时和睡眠恢复可能改变墙上时间，定期重算计划边界。
            _ = clock.sleep_until(clock.monotonic_now().saturating_add(until_transition.min(SCHEDULE_RECHECK_INTERVAL))) => {}
        }
    }
}

pub(crate) fn schedule_state(schedule: &ScheduleConfig, wall_now: SystemTime) -> (bool, Duration) {
    // 北京固定为 UTC+08:00，不受运行主机本地时区或夏令时影响。
    let local_now = system_time_nanos(wall_now) + BEIJING_UTC_OFFSET_SECONDS * NANOS_PER_SECOND;
    let local_day = local_now.div_euclid(NANOS_PER_DAY);
    let weekday = (local_day + 3).rem_euclid(7) as u8;
    let minute = (local_now.rem_euclid(NANOS_PER_DAY) / NANOS_PER_MINUTE) as u16;
    let active = schedule.contains_local_time(weekday, minute);
    let mut next_transition = None;

    for offset in 0..=7 {
        let day = local_day + offset;
        let day_weekday = (day + 3).rem_euclid(7) as usize;
        let day_start = day * NANOS_PER_DAY;
        let start = schedule.start_minute as i128 * NANOS_PER_MINUTE;
        let end = schedule.end_minute as i128 * NANOS_PER_MINUTE;

        if schedule.start_minute == schedule.end_minute {
            if schedule.enabled_days[day_weekday] {
                add_future_boundary(&mut next_transition, day_start, local_now);
                add_future_boundary(&mut next_transition, day_start + NANOS_PER_DAY, local_now);
            }
        } else if schedule.enabled_days[day_weekday] {
            add_future_boundary(&mut next_transition, day_start + start, local_now);
            if schedule.start_minute < schedule.end_minute {
                add_future_boundary(&mut next_transition, day_start + end, local_now);
            }
        }
        if schedule.start_minute > schedule.end_minute
            && schedule.enabled_days[(day_weekday + 6) % 7]
        {
            add_future_boundary(&mut next_transition, day_start + end, local_now);
        }
    }

    let remaining = next_transition.expect("有效计划应有下一次时段边界") - local_now;
    let seconds = (remaining / NANOS_PER_SECOND) as u64;
    let nanoseconds = (remaining % NANOS_PER_SECOND) as u32;
    (active, Duration::new(seconds, nanoseconds))
}

fn add_future_boundary(next: &mut Option<i128>, candidate: i128, now: i128) {
    if candidate > now && next.is_none_or(|current| candidate < current) {
        *next = Some(candidate);
    }
}

fn system_time_nanos(time: SystemTime) -> i128 {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => duration_to_nanos(duration),
        Err(error) => -duration_to_nanos(error.duration()),
    }
}

fn duration_to_nanos(duration: Duration) -> i128 {
    duration.as_secs() as i128 * NANOS_PER_SECOND + i128::from(duration.subsec_nanos())
}

async fn wait_until(
    clock: &dyn SchedulerClock,
    deadline: Duration,
    control: &mut watch::Receiver<RunState>,
) -> bool {
    loop {
        if !wait_until_running(control).await {
            return false;
        }
        if clock.monotonic_now() >= deadline {
            return true;
        }
        tokio::select! {
            _ = clock.sleep_until(deadline) => return true,
            changed = control.changed() => {
                if changed.is_err() {
                    return false;
                }
            }
        }
    }
}

async fn wait_for_running(control: &mut watch::Receiver<RunState>) -> Result<(), ()> {
    if wait_until_running(control).await {
        Ok(())
    } else {
        Err(())
    }
}

async fn send_event(
    events: &mpsc::Sender<RequestCompleted>,
    event: RequestCompleted,
    control: &mut watch::Receiver<RunState>,
) -> bool {
    loop {
        if !wait_until_running(control).await {
            return false;
        }
        tokio::select! {
            reserved = events.reserve() => match reserved {
                Ok(permit) => {
                    permit.send(event);
                    return true;
                }
                Err(_) => return false,
            },
            changed = control.changed() => {
                if changed.is_err() || *control.borrow() == RunState::Cancelled {
                    return false;
                }
            }
        }
    }
}

async fn sleep_optional(clock: &dyn SchedulerClock, deadline: Option<Duration>) {
    match deadline {
        Some(deadline) => clock.sleep_until(deadline).await,
        None => pending().await,
    }
}

fn jittered_interval(base: Duration, jitter_percent: f64, sample: f64) -> Duration {
    let scale = 1.0 + sample * jitter_percent / 100.0;
    Duration::from_secs_f64(base.as_secs_f64() * scale)
}

fn next_due(completed_at: Duration, interval: Duration) -> Duration {
    completed_at.saturating_add(interval)
}

fn failure_backoff_due(
    scheduled_due: Duration,
    completed_at: Duration,
    failures: u32,
    threshold: u32,
    backoff: Duration,
) -> Duration {
    if failures >= threshold {
        scheduled_due.max(completed_at.saturating_add(backoff))
    } else {
        scheduled_due
    }
}

fn retry_after_delay(retry_after: Option<&RetryAfter>, wall_now: SystemTime) -> Duration {
    match retry_after {
        Some(RetryAfter::Delay(delay)) => *delay,
        Some(RetryAfter::At(at)) if *at > wall_now => at
            .duration_since(wall_now)
            .unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN),
        Some(RetryAfter::At(_)) | None => DEFAULT_RATE_LIMIT_COOLDOWN,
    }
}

#[cfg(test)]
mod gate_tests {
    #[test]
    fn os_jitter_uses_platform_random_source_with_bounded_samples() {
        let jitter = OsJitter::new().unwrap();
        let first = jitter.sample();
        let samples: Vec<_> = (0..64).map(|_| jitter.sample()).collect();
        assert!(samples.iter().all(|value| (-1.0..=1.0).contains(value)));
        assert!(samples.iter().any(|value| *value != first));
    }

    fn detail_config() -> MonitorConfig {
        let mut config = MonitorConfig::default();
        config.monitoring_mode = MonitoringMode::ProductDetail;
        config
    }
    use super::*;

    struct PanicClient;

    impl SchedulerClient for PanicClient {
        fn product_detail(&self, _line: LineId) -> BoxFuture<Result<ProductDetail, RicohApiError>> {
            Box::pin(async { panic!("fixture request panic") })
        }
    }

    #[tokio::test]
    async fn unexpected_line_task_exit_is_reported() {
        let mut config = detail_config();
        config.schedule.start_minute = 0;
        config.schedule.end_minute = 0;
        let scheduler = Scheduler::new(
            config,
            Arc::new(PanicClient),
            Arc::new(TokioClock::default()),
            Arc::new(OsJitter::new().unwrap()),
        )
        .unwrap();
        let mut monitor = scheduler.start([LineId::new(7, "direct")]).unwrap();
        let failure = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Err(error) = monitor.check_tasks().await {
                    break error;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(failure.contains("商品 7"));
    }

    struct HoldingClient {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl SchedulerClient for HoldingClient {
        fn product_detail(&self, line: LineId) -> BoxFuture<Result<ProductDetail, RicohApiError>> {
            let started = self.started.clone();
            let release = self.release.clone();
            Box::pin(async move {
                if line.product_id == 65 {
                    started.notify_one();
                    release.notified().await;
                }
                Ok(ProductDetail {
                    product_id: line.product_id,
                    name: "Fixture".into(),
                    is_show: 0,
                    stock: serde_json::json!(0).as_number().unwrap().clone(),
                    availability: crate::availability::Availability::OutOfStock,
                    metadata: Default::default(),
                })
            })
        }
    }

    #[tokio::test]
    async fn adding_line_keeps_existing_request_running() {
        let mut config = detail_config();
        config.schedule.start_minute = 0;
        config.schedule.end_minute = 0;
        let client = Arc::new(HoldingClient {
            started: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        });
        let scheduler = Scheduler::new(
            config,
            client.clone(),
            Arc::new(TokioClock::default()),
            Arc::new(OsJitter::new().unwrap()),
        )
        .unwrap();
        let mut monitor = scheduler.start([LineId::new(65, "direct")]).unwrap();
        tokio::time::timeout(Duration::from_secs(2), client.started.notified())
            .await
            .unwrap();
        monitor
            .reconcile_lines(
                [LineId::new(65, "direct"), LineId::new(66, "direct")],
                0,
                &[],
            )
            .await
            .unwrap();
        client.release.notify_one();
        let mut completed = Vec::new();
        for _ in 0..2 {
            completed.push(
                tokio::time::timeout(Duration::from_secs(2), monitor.recv())
                    .await
                    .unwrap()
                    .unwrap()
                    .line
                    .product_id,
            );
        }
        assert!(completed.contains(&65));
        assert!(completed.contains(&66));
        monitor.shutdown().await;
    }

    #[tokio::test]
    async fn removing_line_discards_only_its_queued_results() {
        let scheduler = Scheduler::new(
            detail_config(),
            Arc::new(HoldingClient {
                started: Arc::new(Notify::new()),
                release: Arc::new(Notify::new()),
            }),
            Arc::new(TokioClock::default()),
            Arc::new(OsJitter::new().unwrap()),
        )
        .unwrap();
        let old = LineId::new(65, "direct");
        let retained = LineId::new(66, "direct");
        let mut monitor = scheduler.start([old.clone(), retained.clone()]).unwrap();
        monitor.pause();
        for line in [old.clone(), retained.clone()] {
            monitor
                .event_tx
                .send(RequestCompleted {
                    line,
                    generation: 0,
                    sequence: 1,
                    started_at: Duration::ZERO,
                    completed_at: Duration::ZERO,
                    completed_at_ms: 1,
                    health_transition: None,
                    health: MonitoringHealth::default(),
                    result: Err(RicohApiError::InvalidConfiguration),
                })
                .await
                .unwrap();
        }
        monitor
            .reconcile_lines([retained.clone()], 0, &[])
            .await
            .unwrap();
        assert_eq!(monitor.try_recv().unwrap().line, retained);
        assert!(monitor.try_recv().is_none());
        monitor.shutdown().await;
    }

    #[tokio::test]
    async fn reenabled_product_starts_with_fresh_health_but_keeps_other_health() {
        let scheduler = Scheduler::new(
            detail_config(),
            Arc::new(HoldingClient {
                started: Arc::new(Notify::new()),
                release: Arc::new(Notify::new()),
            }),
            Arc::new(TokioClock::default()),
            Arc::new(OsJitter::new().unwrap()),
        )
        .unwrap();
        let old = LineId::new(65, "direct");
        let retained = LineId::new(66, "direct");
        let mut monitor = scheduler.start([old.clone(), retained.clone()]).unwrap();
        monitor.pause();
        let alert_after = Duration::from_millis(1);
        record_health(
            &scheduler.health,
            65,
            1,
            1,
            Duration::from_millis(1),
            false,
            alert_after,
        );
        assert_eq!(
            record_health(
                &scheduler.health,
                65,
                2,
                2,
                Duration::from_millis(2),
                false,
                alert_after
            )
            .0,
            Some(MonitoringHealthTransition::Failed { since_ms: 1 })
        );
        record_health(
            &scheduler.health,
            66,
            3,
            2,
            Duration::from_millis(2),
            false,
            alert_after,
        );
        monitor
            .reconcile_lines([retained.clone()], 0, &[])
            .await
            .unwrap();
        monitor
            .reconcile_lines([old, retained], 0, &[])
            .await
            .unwrap();
        assert_eq!(
            record_health(
                &scheduler.health,
                65,
                4,
                3,
                Duration::from_millis(3),
                true,
                alert_after
            )
            .0,
            None
        );
        assert_eq!(
            record_health(
                &scheduler.health,
                66,
                5,
                3,
                Duration::from_millis(3),
                false,
                alert_after
            )
            .0,
            Some(MonitoringHealthTransition::Failed { since_ms: 2 })
        );
        monitor.shutdown().await;
    }

    #[tokio::test]
    async fn resetting_one_product_rejects_old_results_and_starts_new_generation() {
        let mut config = detail_config();
        config.schedule.start_minute = 0;
        config.schedule.end_minute = 0;
        let client = Arc::new(HoldingClient {
            started: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        });
        let generations = Arc::new(StdMutex::new(BTreeMap::new()));
        let scheduler = Scheduler::new(
            config,
            client.clone(),
            Arc::new(TokioClock::default()),
            Arc::new(OsJitter::new().unwrap()),
        )
        .unwrap()
        .with_product_generations(generations.clone());
        let old = LineId::new(65, "direct");
        let retained = LineId::new(66, "direct");
        let mut monitor = scheduler.start([old.clone(), retained.clone()]).unwrap();
        monitor.pause();
        for line in [old.clone(), retained.clone()] {
            monitor
                .event_tx
                .send(RequestCompleted {
                    line,
                    generation: 0,
                    sequence: 1,
                    started_at: Duration::ZERO,
                    completed_at: Duration::ZERO,
                    completed_at_ms: 1,
                    health_transition: None,
                    health: MonitoringHealth::default(),
                    result: Err(RicohApiError::InvalidConfiguration),
                })
                .await
                .unwrap();
        }
        generations.lock().unwrap().insert(65, 1);
        monitor
            .reconcile_lines([old, retained.clone()], 0, &[65])
            .await
            .unwrap();
        assert_eq!(monitor.try_recv().unwrap().line, retained);
        tokio::time::timeout(Duration::from_secs(2), client.started.notified())
            .await
            .unwrap();
        client.release.notify_one();
        let completed = tokio::time::timeout(Duration::from_secs(2), monitor.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.line.product_id, 65);
        assert_eq!(completed.generation, 1);
        assert_eq!(completed.health_transition, None);
        monitor.shutdown().await;
    }

    #[test]
    fn default_polling_matches_the_configured_success_range_and_failure_wait() {
        let config = detail_config().requests;
        assert_eq!(
            jittered_interval(config.interval, config.jitter_percent, -1.0),
            Duration::from_millis(1_000)
        );
        assert_eq!(
            jittered_interval(config.interval, config.jitter_percent, 1.0),
            Duration::from_millis(2_000)
        );
        assert_eq!(
            next_due(Duration::from_millis(250), Duration::from_millis(900)),
            Duration::from_millis(1_150)
        );
        assert_eq!(
            next_due(Duration::from_millis(250), Duration::ZERO),
            Duration::from_millis(250)
        );
        assert_eq!(
            jittered_interval(Duration::from_millis(500), 100.0, -1.0),
            Duration::ZERO
        );
        assert_eq!(
            jittered_interval(Duration::from_millis(500), 100.0, 1.0),
            Duration::from_secs(1)
        );
        assert_eq!(
            failure_backoff_due(
                Duration::from_secs(1),
                Duration::from_secs(1),
                config.failures_before_backoff,
                config.failures_before_backoff,
                config.failure_backoff,
            ),
            Duration::from_secs(21)
        );
    }

    #[test]
    fn schedule_uses_beijing_time_and_an_exclusive_end() {
        let schedule = ScheduleConfig {
            enabled_days: [true, false, false, false, false, false, false],
            start_minute: 9 * 60,
            end_minute: 9 * 60 + 1,
        };
        let monday_utc = SystemTime::UNIX_EPOCH + Duration::from_secs(1_704_067_200);
        assert_eq!(
            schedule_state(&schedule, monday_utc + Duration::from_secs(59 * 60 + 59)),
            (false, Duration::from_secs(1))
        );
        assert_eq!(
            schedule_state(&schedule, monday_utc + Duration::from_secs(60 * 60)),
            (true, Duration::from_secs(60))
        );
    }

    #[test]
    fn inactive_schedule_next_boundary_is_next_enabled_start() {
        let schedule = ScheduleConfig {
            enabled_days: [true, false, true, false, false, false, false],
            start_minute: 9 * 60,
            end_minute: 19 * 60,
        };
        let monday_utc = SystemTime::UNIX_EPOCH + Duration::from_secs(1_704_067_200);
        assert_eq!(
            schedule_state(&schedule, monday_utc + Duration::from_secs(13 * 60 * 60)),
            (false, Duration::from_secs(36 * 60 * 60)),
        );
        let full_day = ScheduleConfig {
            enabled_days: [false, false, true, false, false, false, false],
            start_minute: 0,
            end_minute: 0,
        };
        assert_eq!(
            schedule_state(&full_day, monday_utc + Duration::from_secs(13 * 60 * 60)),
            (false, Duration::from_secs(27 * 60 * 60)),
        );
    }

    #[tokio::test]
    async fn shared_gate_honors_domain_cooldown() {
        let mut config = detail_config();
        config.scan.interval = Duration::from_millis(1);
        let gate = RequestGate::new(
            &config.requests,
            config.scan.interval,
            Arc::new(TokioClock::default()),
        );
        let (_control, mut receiver) = ControlToken::new();
        drop(gate.acquire_scan(1, &mut receiver).await.unwrap());

        drop(gate.acquire_scan(2, &mut receiver).await.unwrap());

        gate.cool_domain(Some(&RetryAfter::Delay(Duration::from_millis(70))))
            .await;
        let before_retry = Instant::now();
        drop(gate.acquire_scan(3, &mut receiver).await.unwrap());
        assert!(before_retry.elapsed() >= Duration::from_millis(60));
    }

    #[tokio::test]
    async fn monitor_can_start_immediately_after_scan() {
        let mut config = detail_config();
        config.scan.interval = Duration::from_millis(1);
        let gate = RequestGate::new(
            &config.requests,
            config.scan.interval,
            Arc::new(TokioClock::default()),
        );
        let (_control, mut receiver) = ControlToken::new();
        drop(gate.acquire_scan(1, &mut receiver).await.unwrap());
        drop(
            tokio::time::timeout(
                Duration::from_millis(80),
                gate.acquire_monitor_until(
                    &LineId::new(2, "direct"),
                    &mut receiver,
                    Duration::from_secs(10),
                ),
            )
            .await
            .expect("监控请求应立即准入，不受固定每秒速率限制")
            .unwrap(),
        );
    }

    #[test]
    fn failure_duration_uses_active_monotonic_time_across_clock_and_sleep_gaps() {
        let health = StdMutex::new(HashMap::new());
        let threshold = Duration::from_secs(10);
        assert_eq!(
            record_health(
                &health,
                7,
                1,
                1_000,
                Duration::from_secs(1),
                false,
                threshold
            )
            .0,
            None
        );
        assert_eq!(
            record_health(&health, 7, 2, 500, Duration::from_secs(5), false, threshold)
                .1
                .active_failure_ms,
            4_000
        );
        clear_health_anchor(&health, 7);
        assert_eq!(
            record_health(
                &health,
                7,
                3,
                100_000,
                Duration::from_secs(3_605),
                false,
                threshold
            )
            .1
            .active_failure_ms,
            4_000
        );
        let (transition, snapshot) = record_health(
            &health,
            7,
            4,
            101_000,
            Duration::from_secs(3_609),
            false,
            threshold,
        );
        assert_eq!(transition, None);
        assert_eq!(snapshot.failed_since_ms, Some(1_000));
        assert_eq!(snapshot.active_failure_ms, 8_000);
        assert_eq!(
            record_health(
                &health,
                7,
                5,
                102_000,
                Duration::from_secs(3_611),
                false,
                threshold
            )
            .0,
            Some(MonitoringHealthTransition::Failed { since_ms: 1_000 })
        );
    }

    #[test]
    fn slow_global_queue_still_counts_active_monitoring_time() {
        let health = StdMutex::new(HashMap::new());
        let threshold = Duration::from_secs(600);
        record_health(&health, 7, 1, 1_000, Duration::ZERO, false, threshold);
        assert_eq!(
            record_health(
                &health,
                7,
                2,
                501_000,
                Duration::from_secs(500),
                false,
                threshold
            )
            .0,
            None
        );
        assert_eq!(
            record_health(
                &health,
                7,
                3,
                1_001_000,
                Duration::from_secs(1_000),
                false,
                threshold
            )
            .0,
            Some(MonitoringHealthTransition::Failed { since_ms: 1_000 })
        );
    }

    #[tokio::test]
    async fn scan_interval_is_applied_once_per_request() {
        let mut config = detail_config();
        config.scan.interval = Duration::from_millis(200);
        let gate = RequestGate::new(
            &config.requests,
            config.scan.interval,
            Arc::new(TokioClock::default()),
        );
        let (_control, mut receiver) = ControlToken::new();
        drop(gate.acquire_scan(1, &mut receiver).await.unwrap());
        drop(
            tokio::time::timeout(
                Duration::from_millis(350),
                gate.acquire_scan(2, &mut receiver),
            )
            .await
            .expect("扫描请求应只等待一个扫描间隔")
            .unwrap(),
        );
    }

    #[tokio::test]
    async fn pausing_a_queued_scan_releases_admission_for_other_products() {
        let config = detail_config();
        let gate = Arc::new(RequestGate::new(
            &config.requests,
            config.scan.interval,
            Arc::new(TokioClock::default()),
        ));
        gate.cool_domain(Some(&RetryAfter::Delay(Duration::from_millis(100))))
            .await;
        let (scan_control, mut scan_receiver) = ControlToken::new();
        let scan_gate = gate.clone();
        let scan = tokio::spawn(async move { scan_gate.acquire_scan(1, &mut scan_receiver).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while gate.admission.try_lock().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        scan_control.pause();
        let (_monitor_control, mut monitor_receiver) = ControlToken::new();
        let monitor = tokio::time::timeout(
            Duration::from_millis(300),
            gate.acquire_monitor_until(
                &LineId::new(2, "direct"),
                &mut monitor_receiver,
                Duration::from_secs(1),
            ),
        )
        .await;
        scan_control.cancel();
        scan.await.unwrap().unwrap_err();
        assert!(monitor.is_ok(), "暂停的扫描不应占有其他商品的准入队列");
    }

    #[tokio::test]
    async fn cancelling_a_waiting_request_releases_its_gate_slot() {
        let mut config = detail_config();
        config.scan.interval = Duration::from_secs(2);
        let gate = Arc::new(RequestGate::new(
            &config.requests,
            config.scan.interval,
            Arc::new(TokioClock::default()),
        ));
        let (_first_control, mut first_receiver) = ControlToken::new();
        drop(gate.acquire_scan(1, &mut first_receiver).await.unwrap());

        let (waiting_control, mut waiting_receiver) = ControlToken::new();
        let waiting =
            tokio::spawn(async move { gate.acquire_scan(2, &mut waiting_receiver).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        waiting_control.cancel();
        assert!(matches!(
            waiting.await.unwrap(),
            Err(AcquireError::Cancelled)
        ));
    }
}
