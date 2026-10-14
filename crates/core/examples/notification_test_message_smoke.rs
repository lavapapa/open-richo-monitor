//! 隔离渠道验收：cargo run --manifest-path crates/core/Cargo.toml --example notification_test_message_smoke -- <新数据目录>
use ricoh_monitor_core::{
    app::{MonitorApp, Weekday},
    notifications::NotificationTarget,
    storage::Storage,
};
use serde_json::{json, Value};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(std::env::args().nth(1).expect("请指定新的隔离目录"));
    assert!(
        !directory.join("monitor.sqlite3").exists(),
        "请换用新的隔离目录"
    );
    std::fs::create_dir_all(&directory)?;
    let mut db = Storage::open(directory.join("monitor.sqlite3"))?;
    let mut config = db.monitor_config()?;
    config.auto_start_monitoring = false;
    config.notification_use_system_proxy = false;
    db.save_monitor_config(&config)?;
    db.set_onboarding_products(&[65, 130])?;
    db.save_notification_channel(
        "fixture",
        "验收渠道",
        "feishu",
        Some("fixture"),
        &["stock_available".into()],
    )?;
    db.save_credential(
        "channel",
        "fixture",
        &json!({"appId":"fixture","appSecret":"fixture"}).to_string(),
    )?;
    db.save_notification_routes(
        "fixture",
        &[NotificationTarget {
            id: "private".into(),
            kind: "user".into(),
            label: "验收接收人".into(),
        }],
    )?;
    drop(db);

    let app = MonitorApp::open(&directory)?;
    let mut config = app.snapshot().await?.config;
    config.schedule.days = vec![Weekday::Mon, Weekday::Wed];
    config.schedule.start = "22:00".into();
    config.schedule.end = "02:00".into();
    config.rate.interval_min_ms = 300;
    config.rate.interval_max_ms = 1000;
    app.save_config(config.clone()).await?;
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs");
    // 记录实际 send RPC，既有 fixture 同时接收 stdin 并返回模拟回执。
    let trace = r#"const fs = await import('node:fs');
let buffer = '';
process.stdin.on('data', chunk => {
  buffer += chunk;
  let end;
  while ((end = buffer.indexOf('\n')) >= 0) {
    const request = JSON.parse(buffer.slice(0, end));
    buffer = buffer.slice(end + 1);
    if (request.method === 'send') fs.appendFileSync(process.argv[2], JSON.stringify(request) + '\n');
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
    let result = app.test_channel("fixture").await?;
    assert_eq!(result.outcome, "accepted");
    let records = std::fs::read_to_string(directory.join("runtime-sends.jsonl"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()?;
    assert_eq!(records.len(), 1);
    let params = &records[0]["params"];
    assert_eq!(params["accountId"], "fixture");
    assert_eq!(params["target"]["id"], "private");
    let text = params["text"].as_str().expect("RPC 正文");
    let lines = text.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0], "【测试消息】");
    let mut products = lines[1]
        .strip_prefix("监控产品：")
        .expect("商品字段")
        .split('、')
        .collect::<Vec<_>>();
    products.sort_unstable();
    assert_eq!(products, vec!["官翻品 GR IIIx", "官翻品 GR IV"]);
    assert_eq!(lines[2], "日期：星期一、星期三");
    assert_eq!(lines[3], "时间：22:00–次日 02:00");
    assert_eq!(lines[4], "时间间隔：随机0.3～1秒");
    let evidence = json!({"scenario":"公开 test_channel 单渠道发送", "config":config, "outcome":result.outcome, "runtimeSend":records[0], "message":text});
    std::fs::write(
        directory.join("evidence.json"),
        serde_json::to_string_pretty(&evidence)?,
    )?;
    app.shutdown();
    println!(
        "通过：公开渠道测试 RPC 正文与当前商品、星期、跨夜计划、随机等待区间一致。证据目录：{}",
        directory.display()
    );
    Ok(())
}
