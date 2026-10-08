//! 有界实网采样：复用生产调度器、限流和理光客户端，不写用户数据或发送通知。
use ricoh_monitor_core::{
    config::{MonitorConfig, MonitoringMode},
    ricoh::ProductDetail,
    ricoh_api::RicohApiError,
    runtime::RicohSchedulerClient,
    scheduler::{LineId, OsJitter, Scheduler, SchedulerClient, TokioClock},
};
use serde_json::{json, Value};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

type ApiFuture<T> = Pin<Box<dyn Future<Output = Result<T, RicohApiError>> + Send>>;

struct SampledClient {
    inner: Arc<RicohSchedulerClient>,
    start: Instant,
    samples: Arc<Mutex<Vec<Value>>>,
    limit: usize,
}

impl SampledClient {
    fn record<T: Send + 'static>(&self, label: String, future: ApiFuture<T>) -> ApiFuture<T> {
        let start = self.start;
        let samples = self.samples.clone();
        let limit = self.limit;
        Box::pin(async move {
            {
                let rows = samples.lock().unwrap();
                if rows.len() >= limit || stop_for_errors(&rows) {
                    return Err(RicohApiError::Transport("采样已结束".into()));
                }
            }
            let began = start.elapsed();
            let result = future.await;
            let row = json!({ "endpoint": label, "start_ms": began.as_millis(),
                "latency_ms": start.elapsed().saturating_sub(began).as_millis(),
                "error": result.as_ref().err().map(ToString::to_string) });
            println!("{row}");
            samples.lock().unwrap().push(row);
            result
        })
    }
}

impl SchedulerClient for SampledClient {
    fn product_detail(&self, line: LineId) -> ApiFuture<ProductDetail> {
        self.record(
            format!("detail:{}", line.product_id),
            self.inner.product_detail(line),
        )
    }
    fn product_page(&self, outlet: String, page: u32, limit: u32) -> ApiFuture<Vec<ProductDetail>> {
        self.record(
            format!("list:{page}"),
            self.inner.product_page(outlet, page, limit),
        )
    }
}

fn summarize(rows: &[Value]) -> Value {
    let errors = rows.iter().filter(|row| !row["error"].is_null()).count();
    let mut latencies: Vec<_> = rows
        .iter()
        .map(|row| row["latency_ms"].as_u64().unwrap())
        .collect();
    latencies.sort_unstable();
    let gaps: Vec<_> = rows
        .windows(2)
        .map(|pair| pair[1]["start_ms"].as_u64().unwrap() - pair[0]["start_ms"].as_u64().unwrap())
        .collect();
    let percentile =
        |percent: usize| latencies.get((latencies.len() * percent).div_ceil(100).saturating_sub(1));
    json!({ "requests": rows.len(), "errors": errors,
        "error_percent": if rows.is_empty() { 0.0 } else { errors as f64 * 100.0 / rows.len() as f64 },
        "latency_p50_ms": percentile(50), "latency_p95_ms": percentile(95),
        "minimum_start_gap_ms": gaps.iter().min(),
        "mean_start_gap_ms": if gaps.is_empty() { None } else { Some(gaps.iter().sum::<u64>() / gaps.len() as u64) },
    })
}

fn stop_for_errors(rows: &[Value]) -> bool {
    rows.iter().any(|row| {
        row["error"].as_str().is_some_and(|message| {
            message.contains("限制请求")
                || message.contains("拒绝访问")
                || message.contains("身份验证")
        })
    }) || rows
        .iter()
        .rev()
        .take(3)
        .filter(|row| !row["error"].is_null())
        .count()
        == 3
        || (rows.len() >= 20
            && rows
                .iter()
                .rev()
                .take(20)
                .filter(|row| !row["error"].is_null())
                .count()
                >= 4)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "windows")]
    if ricoh_monitor_core::system_proxy::windows::run_helper() {
        return Ok(());
    }
    // 采样沿用生产调度，单线实验保持串行请求。
    if !std::env::args().any(|arg| arg == "--live") {
        return Err(
            "真实采样需要 --live；总量不超过 200 请求，403/429 或连续 3 次失败即停止".into(),
        );
    }
    let mut report = Vec::new();
    let use_system_proxy = std::env::args().any(|arg| arg == "--system-proxy");
    let single_line_ceiling = std::env::args().any(|arg| arg == "--single-line-ceiling");
    let stages = if single_line_ceiling {
        vec![(MonitoringMode::ProductDetail, 0, 0, 200)]
    } else {
        vec![
            (MonitoringMode::ProductDetail, 1000, 1000, 20),
            (MonitoringMode::ProductDetail, 300, 1000, 40),
            (MonitoringMode::ProductDetail, 300, 300, 40),
            (MonitoringMode::ProductDetail, 50, 50, 40),
            (MonitoringMode::ProductDetail, 0, 0, 20),
            (MonitoringMode::ListedProducts, 300, 1000, 40),
        ]
    };
    for (mode, min_ms, max_ms, count) in stages {
        let mut config = MonitorConfig::default();
        config.monitoring_mode = mode;
        config.use_system_proxy = use_system_proxy;
        config.schedule.start_minute = 0;
        config.schedule.end_minute = 0;
        config.requests.interval =
            Duration::from_millis(min_ms) + Duration::from_millis(max_ms - min_ms) / 2;
        config.requests.jitter_percent = if max_ms == 0 {
            0.0
        } else {
            (max_ms - min_ms) as f64 * 100.0 / (max_ms + min_ms) as f64
        };
        let samples = Arc::new(Mutex::new(Vec::new()));
        let client = Arc::new(SampledClient {
            inner: Arc::new(RicohSchedulerClient::new(&config)?),
            start: Instant::now(),
            samples: samples.clone(),
            limit: count,
        });
        let scheduler = Scheduler::new(
            config,
            client,
            Arc::new(TokioClock::default()),
            Arc::new(OsJitter::new()?),
        )
        .map_err(|error| format!("调度配置无效：{error:?}"))?;
        println!(
            "{}",
            json!({ "stage": mode, "min_ms": min_ms, "max_ms": max_ms, "limit": count, "system_proxy": use_system_proxy })
        );
        let mut monitor = scheduler
            .start([LineId::new(65, "direct")])
            .map_err(|error| format!("无法开始采样：{error:?}"))?;
        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(if single_line_ceiling { 600 } else { 90 });
        loop {
            let Ok(Some(_event)) = tokio::time::timeout_at(deadline, monitor.recv()).await else {
                break;
            };
            let rows = samples.lock().unwrap();
            if rows.len() >= count || stop_for_errors(&rows) {
                break;
            }
        }
        monitor.shutdown().await;
        let rows = samples.lock().unwrap().clone();
        let mut summary = summarize(&rows);
        summary["mode"] = json!(mode);
        summary["min_ms"] = json!(min_ms);
        summary["max_ms"] = json!(max_ms);
        summary["system_proxy"] = json!(use_system_proxy);
        summary["interval_semantics"] = json!("completion_delay");
        println!("{}", json!({ "summary": summary }));
        report.push(json!({ "summary": summary, "samples": rows }));
        if stop_for_errors(&rows) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    if let Some(path) = std::env::var_os("RM_RATE_PROBE_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    println!("{}", json!({ "finished": true, "stages": report.len() }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summary_and_stop_rules_use_actual_responses() {
        let rows = vec![
            json!({"start_ms":0,"latency_ms":100,"error":null}),
            json!({"start_ms":500,"latency_ms":200,"error":"超时"}),
            json!({"start_ms":1000,"latency_ms":300,"error":null}),
        ];
        let summary = summarize(&rows);
        assert_eq!(summary["errors"], 1);
        assert_eq!(summary["minimum_start_gap_ms"], 500);
        assert_eq!(summary["latency_p95_ms"], 300);
        assert!(!stop_for_errors(&rows));
        assert!(stop_for_errors(&[json!({"error":"理光接口暂时限制请求"})]));
        assert!(stop_for_errors(&vec![json!({"error":"超时"}); 3]));
        assert_eq!(summarize(&[])["requests"], 0);
    }

    #[tokio::test]
    async fn reaching_sample_budget_does_not_poll_another_request() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let called = Arc::new(AtomicBool::new(false));
        let capture = called.clone();
        let client = SampledClient {
            inner: Arc::new(RicohSchedulerClient::new(&MonitorConfig::default()).unwrap()),
            start: Instant::now(),
            samples: Default::default(),
            limit: 0,
        };
        let result = client
            .record(
                "budget".into(),
                Box::pin(async move {
                    capture.store(true, Ordering::Relaxed);
                    Ok(())
                }),
            )
            .await;
        assert!(result.is_err());
        assert!(!called.load(Ordering::Relaxed));
        assert!(client.samples.lock().unwrap().is_empty());
    }
}
