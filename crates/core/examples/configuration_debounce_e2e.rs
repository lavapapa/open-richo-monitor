//! 独立闭环验收：cargo run --manifest-path crates/core/Cargo.toml --example configuration_debounce_e2e -- <新数据目录>
use ricoh_monitor_core::{
    app::{MonitorApp, MonitoringAction, RecentEvent, RuntimeState},
    availability::Availability,
    config::MonitoringMode,
    notifications::NotificationTarget,
    ricoh::ProductDetail,
    ricoh_api::RicohApiError,
    scheduler::{LineId, SchedulerClient},
    storage::{ProductIdentity, Storage},
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Error = Box<dyn std::error::Error>;

// 商品接口模拟；调度、观察归约、通知队列与运行时均由实际 MonitorApp 执行。
struct Catalog(AtomicU64);
impl SchedulerClient for Catalog {
    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send>> {
        let stock = self.0.load(Ordering::Relaxed);
        Box::pin(async move {
            Ok(ProductDetail {
                product_id: line.product_id,
                name: "官翻品 GR IV".into(),
                is_show: u8::from(stock > 0),
                stock: stock.into(),
                metadata: Default::default(),
                availability: if stock > 0 {
                    Availability::InStock
                } else {
                    Availability::OutOfStock
                },
            })
        })
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
async fn pause(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}
fn db(directory: &Path) -> Result<Connection, Error> {
    Ok(Connection::open_with_flags(
        directory.join("monitor.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}
fn scalar(directory: &Path, sql: &str) -> Result<i64, Error> {
    Ok(db(directory)?.query_row(sql, [], |row| row.get(0))?)
}
fn due(directory: &Path) -> Result<i64, Error> {
    scalar(
        directory,
        "SELECT COALESCE((SELECT due_at_ms FROM pending_configuration_change),0)",
    )
}
fn checked_due(directory: &Path, started: i64, completed: i64) -> Result<i64, Error> {
    let deadline = due(directory)?;
    assert!(
        (started + 5_000..=completed + 5_000).contains(&deadline),
        "截止时间必须位于实际接口调用时间加五秒的区间内"
    );
    Ok(deadline)
}
fn sends(directory: &Path) -> Result<Vec<Value>, Error> {
    let path = directory.join("runtime-sends.jsonl");
    if !path.exists() {
        return Ok(vec![]);
    }
    Ok(std::fs::read_to_string(path)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?)
}
fn record(records: &mut Vec<Value>, value: Value) {
    println!("{}", value);
    records.push(value);
}

fn open(directory: &Path, catalog: Arc<Catalog>) -> Result<MonitorApp, Error> {
    let app = MonitorApp::open_with_scheduler_client(directory, catalog)?;
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs");
    // 旁路记录真实 RPC send；既有 fixture 接收同一 stdin 并返回平台模拟回执。
    let trace = r#"const fs = await import('node:fs');
let buffer = '';
process.stdin.on('data', chunk => {
  buffer += chunk;
  let end;
  while ((end = buffer.indexOf('\n')) >= 0) {
    const request = JSON.parse(buffer.slice(0, end));
    buffer = buffer.slice(end + 1);
    if (request.method === 'send') fs.appendFileSync(process.argv[2], JSON.stringify({atMs: Date.now(), pid: process.pid, params: request.params}) + '\n');
  }
});
await import(process.argv[1]);"#;
    app.set_notification_runtime_path("node");
    app.set_notification_runtime_arguments(vec![
        "--input-type=module".into(),
        "-e".into(),
        trace.into(),
        fixture.to_string_lossy().into_owned(),
        directory
            .join("runtime-sends.jsonl")
            .to_string_lossy()
            .into_owned(),
    ]);
    app.set_notification_proxy_url(Ok(None));
    app.set_platform_notifications_available(true);
    Ok(app)
}

fn run(app: &MonitorApp) -> tokio::task::JoinHandle<Result<(), ricoh_monitor_core::app::AppError>> {
    let app = app.clone();
    tokio::spawn(async move { app.run().await })
}

async fn quiet(
    app: &MonitorApp,
    directory: &Path,
    until: i64,
    expected_sends: usize,
) -> Result<(), Error> {
    while now() < until {
        assert!(
            app.claim_system_notification().await?.is_none(),
            "静默期内出现系统通知"
        );
        assert_eq!(
            sends(directory)?.len(),
            expected_sends,
            "静默期内运行时已经 send"
        );
        pause(20).await;
    }
    Ok(())
}

async fn receive(
    app: &MonitorApp,
    directory: &Path,
    kind: &str,
    records: &mut Vec<Value>,
) -> Result<(RecentEvent, i64), Error> {
    let (job, event) = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(job) = app.claim_system_notification().await? {
                return Ok::<_, ricoh_monitor_core::app::AppError>(job);
            }
            pause(20).await;
        }
    })
    .await??;
    let claimed = now();
    assert_eq!(event.kind, kind);
    // 系统平台模拟接受；真实队列领取和完成均调用公开 App API。
    app.finish_system_notification(job, Ok(())).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let conn = db(directory)?;
            let (total, sent): (i64, i64) = conn.query_row(
                "SELECT count(*),sum(status='sent') FROM notification_outbox WHERE event_id=?1",
                [event.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if total == 3 && sent == 3 {
                break Ok::<_, Error>(());
            }
            pause(20).await;
        }
    })
    .await??;
    let routes = sends(directory)?
        .into_iter()
        .filter(|send| send["params"]["text"] == event.message)
        .collect::<Vec<_>>();
    assert_eq!(routes.len(), 2, "每个事件应有两路真实运行时调用");
    record(
        records,
        json!({"eventId":event.id,"kind":event.kind,"claimedAtMs":claimed,"runtimeSends":routes,"message":event.message}),
    );
    Ok((event, claimed))
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let directory = PathBuf::from(std::env::args().nth(1).expect("请指定新的隔离目录"));
    assert!(
        !directory.join("monitor.sqlite3").exists(),
        "验收目录已有数据库，请换用新目录"
    );
    std::fs::create_dir_all(&directory)?;
    let mut seed = Storage::open(directory.join("monitor.sqlite3"))?;
    let mut config = seed.monitor_config()?;
    config.auto_start_monitoring = false;
    config.notification_use_system_proxy = false;
    config.monitoring_mode = MonitoringMode::ProductDetail;
    seed.save_monitor_config(&config)?;
    for (id, name) in [(65, "官翻品 GR IIIx"), (130, "官翻品 GR IV")] {
        seed.save_product_config(
            ProductIdentity {
                key: id.to_string(),
                product_id: id.to_string(),
                sku_id: None,
            },
            name.into(),
            "fixture".into(),
            Some(1),
        )?;
    }
    seed.set_onboarding_products(&[65])?;
    for (id, provider, target) in [
        ("fixture-feishu", "feishu", "private"),
        ("fixture-dingtalk", "dingtalk", "group"),
    ] {
        seed.save_notification_channel(
            id,
            id,
            provider,
            Some(id),
            &["stock_available".into(), "configuration_changed".into()],
        )?;
        seed.save_credential(
            "channel",
            id,
            &json!({"appId":"fixture","appSecret":"fixture","targetId":target,"targetKind":"user"})
                .to_string(),
        )?;
        seed.save_notification_routes(
            id,
            &[NotificationTarget {
                id: target.into(),
                kind: "user".into(),
                label: id.into(),
            }],
        )?;
        seed.mark_notification_channel_tested(id)?;
        seed.set_notification_channel_enabled(id, true)?;
    }
    seed.complete_setup()?;
    drop(seed);

    let catalog = Arc::new(Catalog(AtomicU64::new(0)));
    let app = open(&directory, catalog.clone())?;
    let runner = run(&app);
    let mut records = Vec::new();
    let started = now();
    let mut config = app.snapshot().await?.config;
    config.schedule.start = "10:00".into();
    config.schedule.end = "20:00".into();
    config.rate.interval_min_ms = 2_000;
    config.rate.interval_max_ms = 2_000;
    app.save_config(config.clone()).await?;
    pause(180).await;
    app.set_product_enabled(130, true).await?;
    pause(180).await;
    app.set_product_enabled(65, false).await?;
    pause(180).await;
    config.schedule.start = "22:00".into();
    config.schedule.end = "02:00".into();
    config.rate.interval_min_ms = 3_000;
    config.rate.interval_max_ms = 3_000;
    let last_started = now();
    app.save_config(config.clone()).await?;
    let last_completed = now();
    let deadline = checked_due(&directory, last_started, last_completed)?;
    record(
        &mut records,
        json!({"scenario":"连续商品与配置合并","startedAtMs":started,"lastOperationStartedAtMs":last_started,"lastOperationCompletedAtMs":last_completed,"dueAtMs":deadline,"runtimeState":format!("{:?}",app.snapshot().await?.runtime.state)}),
    );
    quiet(&app, &directory, deadline - 2_400, 0).await?;
    app.save_config(config.clone()).await?;
    app.set_product_enabled(130, true).await?;
    assert_eq!(
        due(&directory)?,
        deadline,
        "相同配置/商品选择重复保存推迟了截止时间"
    );
    record(
        &mut records,
        json!({"scenario":"重复保存不推迟","duplicateAtMs":now(),"dueAtMs":due(&directory)?}),
    );
    quiet(&app, &directory, deadline, 0).await?;
    let (merged, received) =
        receive(&app, &directory, "configuration_changed", &mut records).await?;
    assert!(received >= deadline);
    assert!(merged
        .message
        .contains("新增【官翻品 GR IV】；减少【官翻品 GR IIIx】"));
    assert!(merged
        .message
        .contains("每天 09:00–19:00 → 每天 22:00–次日 02:00"));
    assert!(merged.message.contains("检查间隔变更：1–2 秒 → 3–3 秒"));
    assert!(merged.message.contains("当前监控范围：官翻品 GR IV"));
    assert!(!merged.message.contains("10:00") && !merged.message.contains("20:00"));
    assert!(sends(&directory)?
        .iter()
        .all(|send| send["atMs"].as_i64().unwrap() >= deadline));
    assert_eq!(app.snapshot().await?.runtime.state, RuntimeState::Stopped);

    // 连续改变商品和时间/速率后全部恢复，整批净变化为零。
    let baseline = config.clone();
    config.schedule.start = "13:00".into();
    config.rate.interval_min_ms = 1_000;
    config.rate.interval_max_ms = 1_000;
    app.save_config(config).await?;
    app.set_product_enabled(65, true).await?;
    app.set_product_enabled(130, false).await?;
    pause(300).await;
    app.save_config(baseline.clone()).await?;
    app.set_product_enabled(130, true).await?;
    let revert_started = now();
    app.set_product_enabled(65, false).await?;
    let revert_completed = now();
    let reverted_deadline = checked_due(&directory, revert_started, revert_completed)?;
    quiet(&app, &directory, reverted_deadline + 350, 2).await?;
    assert_eq!(due(&directory)?, 0);
    assert_eq!(scalar(&directory,"SELECT count(DISTINCT event_id) FROM notification_outbox WHERE event_kind='configuration_changed'")?, 1);
    app.save_config(baseline.clone()).await?;
    assert_eq!(due(&directory)?, 0, "完成后的相同保存创建了新批次");
    record(
        &mut records,
        json!({"scenario":"恢复原值与完成后重复保存","lastOperationStartedAtMs":revert_started,"lastOperationCompletedAtMs":revert_completed,"dueAtMs":reverted_deadline,"checkedAtMs":now(),"runtimeSendCount":sends(&directory)?.len(),"configurationEventCount":1}),
    );

    // 退出时批次仍在五秒内；重开沿用原截止时间和第一步之前的状态。
    config = baseline;
    config.schedule.start = "00:00".into();
    config.schedule.end = "00:00".into();
    config.rate.interval_min_ms = 100;
    config.rate.interval_max_ms = 100;
    let restart_started = now();
    app.save_config(config.clone()).await?;
    let restart_completed = now();
    let restart_deadline = checked_due(&directory, restart_started, restart_completed)?;
    pause(1_100).await;
    app.shutdown();
    runner.await??;
    drop(app);
    pause(600).await;
    let app = open(&directory, catalog.clone())?;
    let reopened = now();
    assert!(
        reopened < restart_deadline,
        "需在静默期结束前重开以验证原截止时间"
    );
    assert_eq!(due(&directory)?, restart_deadline);
    let runner = run(&app);
    quiet(&app, &directory, restart_deadline, 2).await?;
    let (restarted, received) =
        receive(&app, &directory, "configuration_changed", &mut records).await?;
    assert!(
        received >= restart_deadline && received < reopened + 5_000,
        "重开重新开始了五秒计时"
    );
    assert!(restarted
        .message
        .contains("每天 22:00–次日 02:00 → 每天 全天"));
    assert!(restarted
        .message
        .contains("检查间隔变更：3–3 秒 → 0.1–0.1 秒"));
    assert!(sends(&directory)?
        .iter()
        .filter(|send| send["params"]["text"] == restarted.message)
        .all(|send| send["atMs"].as_i64().unwrap() >= restart_deadline));
    record(
        &mut records,
        json!({"scenario":"退出重开","lastOperationStartedAtMs":restart_started,"lastOperationCompletedAtMs":restart_completed,"reopenedAtMs":reopened,"dueAtMs":restart_deadline,"claimedAtMs":received}),
    );

    // 一个配置批次尚在防抖期间，监控真实产生上架和补货事件。
    config.failure_alert_after_minutes += 1;
    let stock_config_started = now();
    app.save_config(config).await?;
    let stock_config_completed = now();
    let stock_deadline = checked_due(&directory, stock_config_started, stock_config_completed)?;
    record(
        &mut records,
        json!({"scenario":"库存期间配置批次","lastOperationStartedAtMs":stock_config_started,"lastOperationCompletedAtMs":stock_config_completed,"dueAtMs":stock_deadline}),
    );
    app.monitoring_action(MonitoringAction::Start).await?;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if app.snapshot().await?.products.iter().any(|product| {
                product.product_id == "130"
                    && product
                        .observation
                        .as_ref()
                        .is_some_and(|observation| observation.availability == "out_of_stock")
            }) {
                break Ok::<_, ricoh_monitor_core::app::AppError>(());
            }
            pause(20).await;
        }
    })
    .await??;
    for (stock, kind) in [(1, "stock_available"), (5, "stock_increased")] {
        let changed = now();
        catalog.0.store(stock, Ordering::Relaxed);
        let (_, claimed) = receive(&app, &directory, kind, &mut records).await?;
        let delay = claimed - changed;
        assert!(
            delay < 1_500 && claimed < stock_deadline,
            "库存事件受到配置五秒防抖影响"
        );
        record(
            &mut records,
            json!({"scenario":"库存即时投递","kind":kind,"stock":stock,"fixtureChangedAtMs":changed,"claimedAfterMs":delay,"configurationDueAtMs":stock_deadline}),
        );
    }
    app.monitoring_action(MonitoringAction::Pause).await?;
    assert_eq!(app.snapshot().await?.runtime.state, RuntimeState::Paused);
    quiet(&app, &directory, stock_deadline, 8).await?;
    let (paused, _) = receive(&app, &directory, "configuration_changed", &mut records).await?;
    assert!(paused.message.contains("异常提醒阈值变更：10 → 11 分钟"));
    assert!(sends(&directory)?
        .iter()
        .filter(|send| send["params"]["text"] == paused.message)
        .all(|send| send["atMs"].as_i64().unwrap() >= stock_deadline));
    assert_eq!(app.snapshot().await?.runtime.state, RuntimeState::Paused);
    assert!(app.claim_prominent_alert().await?.is_none());
    app.monitoring_action(MonitoringAction::Stop).await?;
    app.shutdown();
    runner.await??;
    assert_eq!(sends(&directory)?.len(), 10);
    assert_eq!(scalar(&directory,"SELECT count(DISTINCT event_id) FROM notification_outbox WHERE event_kind='configuration_changed'")?,3);
    assert_eq!(
        scalar(
            &directory,
            "SELECT count(*) FROM notification_outbox WHERE status='sent'"
        )?,
        15
    );
    assert_eq!(
        scalar(
            &directory,
            "SELECT count(*) FROM notification_outbox WHERE status!='sent'"
        )?,
        0
    );
    record(
        &mut records,
        json!({"scenario":"汇总","configurationEvents":3,"stockEvents":2,"runtimeSendCalls":10,"sentOutboxRows":15,"elapsedMs":now()-started,"platform":"商品 SchedulerClient 与通知平台 fixture；系统通知以公开领取/完成 API 模拟"}),
    );
    std::fs::write(
        directory.join("evidence.json"),
        serde_json::to_string_pretty(&records)?,
    )?;
    println!("通过：五秒静默、连续合并、恢复原值、相同保存、退出重开、停止/暂停时分发和库存即时投递。证据目录：{}",directory.display());
    Ok(())
}
