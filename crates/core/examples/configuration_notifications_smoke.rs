//! 隔离数据的通知端到端验收：cargo run --example configuration_notifications_smoke -- <数据目录> [--prepare-legacy]
use ricoh_monitor_core::{app::MonitorApp, notifications::NotificationTarget, storage::Storage};
use std::{path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(std::env::args().nth(1).expect("请指定独立的验收数据目录"));
    std::fs::create_dir_all(&directory)?;
    let database = directory.join("monitor.sqlite3");
    assert!(!database.exists(), "请使用新的验收目录，避免改写已有数据");
    let prepare = std::env::args().any(|arg| arg == "--prepare-legacy");
    let mut db = Storage::open(&database)?;
    let mut config = db.monitor_config()?;
    config.auto_start_monitoring = false;
    db.save_monitor_config(&config)?;
    db.set_onboarding_products(&[65])?;
    db.complete_setup()?;
    db.set_system_notifications_enabled(!prepare)?;
    for id in ["fixture-feishu", "fixture-dingtalk"] {
        db.save_notification_channel(
            id,
            id,
            if id.ends_with("feishu") {
                "feishu"
            } else {
                "dingtalk"
            },
            Some(id),
            &["stock_available".into()],
        )?;
        if !prepare {
            db.save_credential("channel", id, &serde_json::json!({"appId":"fixture","appSecret":"fixture","targetId":"private","targetKind":"user"}).to_string())?;
            db.save_notification_routes(
                id,
                &[
                    NotificationTarget {
                        id: "private".into(),
                        kind: "user".into(),
                        label: "创建人".into(),
                    },
                    NotificationTarget {
                        id: "group".into(),
                        kind: "chat".into(),
                        label: "验收群".into(),
                    },
                ],
            )?;
            db.mark_notification_channel_tested(id)?;
            db.set_notification_channel_enabled(id, true)?;
        }
    }
    drop(db);
    rusqlite::Connection::open(&database)?.execute_batch("ALTER TABLE settings DROP COLUMN configuration_change_intro_pending; ALTER TABLE settings DROP COLUMN system_configuration_change_notifications_enabled;")?;
    if prepare {
        println!("旧用户隔离数据已准备：{}", directory.display());
        return Ok(());
    }

    let app = MonitorApp::open(&directory)?;
    app.set_notification_runtime_path("node");
    app.set_notification_runtime_arguments(vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/notification_runtime.mjs")
        .to_string_lossy()
        .into_owned()]);
    app.set_notification_proxy_url(Ok(None));
    app.set_platform_notifications_available(true);
    assert!(app.snapshot().await?.configuration_change_intro_pending);
    app.complete_configuration_change_intro(true).await?;
    assert!(app.snapshot().await?.channels.iter().all(|channel| channel
        .subscriptions
        .contains(&ricoh_monitor_core::app::ChannelEvent::ConfigurationChanged)));
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    let mut config = app.snapshot().await?.config;
    config.schedule.start = "22:00".into();
    config.schedule.end = "02:00".into();
    config.rate.interval_min_ms = 0;
    config.rate.interval_max_ms = 0;
    app.save_config(config.clone()).await?;
    assert!(
        app.claim_system_notification().await?.is_none(),
        "防抖期间保持安静"
    );
    let (id, event) = wait_system(&app).await?;
    assert!(event
        .message
        .contains("监控计划变更：每天 09:00–19:00 → 每天 22:00–次日 02:00"));
    assert!(event.message.contains("检查间隔变更：1–2 秒 → 0–0 秒"));
    assert_eq!(event.message.lines().last().unwrap().len(), 23);
    app.finish_system_notification(id, Ok(())).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = app.snapshot().await.unwrap();
            if state.channels.iter().all(|channel| {
                channel.recipient_deliveries.len() == 2
                    && channel.recipient_deliveries.iter().all(|recipient| {
                        recipient.last_delivery.as_ref().is_some_and(|delivery| {
                            delivery.event == "configuration_changed"
                                && delivery.outcome == "accepted"
                        })
                    })
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    app.save_config(config).await?;
    assert!(app.claim_system_notification().await?.is_none());
    assert!(app.claim_prominent_alert().await?.is_none());
    app.shutdown();
    runner.await??;
    app.remove_product(65).await?;
    drop(app);
    let reopened = MonitorApp::open(&directory)?;
    reopened.set_platform_notifications_available(true);
    assert!(
        !reopened
            .snapshot()
            .await?
            .configuration_change_intro_pending
    );
    let (_, event) = wait_system(&reopened).await?;
    assert!(event
        .message
        .contains("监控产品变更：减少【官翻品 GR IIIx】\n当前监控范围：无"));
    println!("通过：旧库升级选择、五秒防抖、两渠道四路投递、系统正文、零等待、跨夜计划、重复保存、突出提醒隔离、退出重开与最后商品移除");
    Ok(())
}

async fn wait_system(
    app: &MonitorApp,
) -> Result<(i64, ricoh_monitor_core::app::RecentEvent), Box<dyn std::error::Error>> {
    Ok(tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(job) = app.claim_system_notification().await? {
                return Ok::<_, ricoh_monitor_core::app::AppError>(job);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??)
}
