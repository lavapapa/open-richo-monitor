//! 隔离数据端到端验收；真实上报标为 development，商城与通知对象使用 fixture。
use ricoh_monitor_core::{
    app::{MonitorApp, MonitoringAction},
    notifications::NotificationTarget,
    ricoh::ProductDetail,
    ricoh_api::RicohApiError,
    scheduler::{LineId, SchedulerClient},
    storage::Storage,
};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration};

struct Fixture;
impl SchedulerClient for Fixture {
    fn product_detail(
        &self,
        _: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send>> {
        Box::pin(async { Err(RicohApiError::InvalidConfiguration) })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(std::env::args().nth(1).expect("请指定新的隔离目录"));
    std::fs::create_dir_all(&directory)?;
    let database = directory.join("monitor.sqlite3");
    assert!(!database.exists(), "请使用新的隔离目录");
    let mut db = Storage::open(&database)?;
    let mut config = db.monitor_config()?;
    config.auto_start_monitoring = false;
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.schedule.start_minute = 0;
    config.schedule.end_minute = 0;
    db.save_monitor_config(&config)?;
    db.set_onboarding_products(&[65])?;
    db.complete_setup()?;
    db.set_system_notifications_enabled(false)?;
    for provider in ["feishu", "dingtalk"] {
        db.save_notification_channel(provider, "fixture", provider, Some(provider), &[])?;
        db.mark_notification_channel_tested(provider)?;
        db.set_notification_channel_enabled(provider, true)?;
        db.save_notification_routes(
            provider,
            &[
                NotificationTarget {
                    id: "fixture-private".into(),
                    kind: "user".into(),
                    label: "fixture".into(),
                },
                NotificationTarget {
                    id: "fixture-group".into(),
                    kind: "chat".into(),
                    label: "fixture".into(),
                },
            ],
        )?;
    }
    drop(db);
    let app = MonitorApp::open_with_scheduler_client(&directory, Arc::new(Fixture))?;
    assert!(!app.snapshot().await?.usage_statistics_enabled);
    let worker = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    tokio::time::sleep(Duration::from_secs(2)).await;
    app.set_usage_statistics_enabled(true).await?;
    app.monitoring_action(MonitoringAction::Start).await?;
    app.monitoring_action(MonitoringAction::Pause).await?;
    app.set_product_enabled(65, false).await?;
    tokio::time::sleep(Duration::from_secs(8)).await;
    app.shutdown();
    worker.await??;
    drop(app);
    let identity = Storage::open(&database)?
        .usage_statistics_identity()?
        .unwrap();
    let reopened = MonitorApp::open_with_scheduler_client(&directory, Arc::new(Fixture))?;
    assert!(reopened.snapshot().await?.usage_statistics_enabled);
    let worker = tokio::spawn({
        let app = reopened.clone();
        async move { app.run().await }
    });
    tokio::time::sleep(Duration::from_secs(6)).await;
    reopened.set_usage_statistics_enabled(false).await?;
    reopened.shutdown();
    worker.await??;
    assert!(!reopened.snapshot().await?.usage_statistics_enabled);
    drop(reopened);
    println!("development 验收安装标识：{identity}");
    println!("独立数据目录：{}", directory.display());
    Ok(())
}
