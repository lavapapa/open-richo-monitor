use std::{
    future::{pending, Future},
    path::{Path, PathBuf},
    pin::Pin,
    process::{Output, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ricoh_monitor_core::{
    app::MonitorApp,
    ricoh::ProductDetail,
    ricoh_api::RicohApiError,
    scheduler::{LineId, SchedulerClient},
    storage::{ProductIdentity, RunIntent, Storage},
};
use tokio::process::Command;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("ricoh-cli-{}-{nonce}-{serial}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct FixtureClient;

impl SchedulerClient for FixtureClient {
    fn product_detail(
        &self,
        _line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send + 'static>> {
        Box::pin(pending())
    }
}

fn seed_ready_setup(data_dir: &Path) {
    let mut storage = Storage::open(data_dir.join("monitor.sqlite3")).unwrap();
    let mut config = storage.monitor_config().unwrap();
    config.schedule.start_minute = 0;
    config.schedule.end_minute = 0;
    storage.save_monitor_config(&config).unwrap();
    storage
        .save_product_config(
            ProductIdentity {
                key: "fixture-product".into(),
                product_id: "245".into(),
                sku_id: None,
            },
            "本地测试商品".into(),
            "fixture".into(),
            Some(1),
        )
        .unwrap();
    storage
        .set_product_enabled("fixture-product", true)
        .unwrap();
    storage
        .save_credential(
            "channel",
            "fixture-credential",
            r#"{"webhookUrl":"https://open.feishu.cn/open-apis/bot/v2/hook/local-fixture-token"}"#,
        )
        .unwrap();
    storage
        .save_notification_channel(
            "fixture-channel",
            "本地测试渠道",
            "feishu",
            Some("fixture-credential"),
            &["stock_available".into()],
        )
        .unwrap();
    storage
        .record_notification_test("fixture-channel", "accepted", 1, "local fixture", None)
        .unwrap();
    storage
        .set_notification_channel_enabled("fixture-channel", true)
        .unwrap();
    storage.complete_setup().unwrap();
    storage.set_run_intent(RunIntent::Stopped, true).unwrap();
}

async fn invoke(binary: &str, data_dir: &Path, args: &[&str]) -> Output {
    tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(binary)
            .args(args)
            .arg("--data-dir")
            .arg(data_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}

fn json(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    serde_json::from_str(stdout.trim()).unwrap()
}

#[tokio::test]
async fn setup_config_and_monitor_intent_use_the_shared_app_store() {
    let temp = TempDir::new();
    let binary = env!("CARGO_BIN_EXE_ricoh-monitor");

    let setup_output = invoke(binary, &temp.0, &["setup"]).await;
    assert!(setup_output.status.success());
    assert!(
        !setup_output.stderr.is_empty(),
        "未配置商品/通知时应说明仍未完成"
    );
    let setup = serde_json::from_slice::<serde_json::Value>(&setup_output.stdout).unwrap();
    assert_eq!(setup["config"]["schedule"]["start"], "09:00");
    assert_eq!(setup["config"]["schedule"]["end"], "19:00");
    assert_eq!(setup["config"]["useSystemProxy"], false);
    assert_eq!(setup["setupCompleted"], false);

    let updated = json(invoke(binary, &temp.0, &["config", "set", "start", "10:30"]).await);
    assert_eq!(updated["schedule"]["start"], "10:30");

    let status = json(invoke(binary, &temp.0, &["status"]).await);
    assert_eq!(status["config"]["schedule"]["start"], "10:30");
}

#[tokio::test]
async fn invalid_command_keeps_json_stdout_empty_and_reports_stderr() {
    let temp = TempDir::new();
    let result = invoke(env!("CARGO_BIN_EXE_ricoh-monitor"), &temp.0, &["unknown"]).await;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!result.stderr.is_empty());
}

#[tokio::test]
async fn service_unit_is_generated_without_starting_a_service() {
    let temp = TempDir::new();
    let output = invoke(
        env!("CARGO_BIN_EXE_ricoh-monitor"),
        &temp.0,
        &["service", "unit"],
    )
    .await;
    let value = json(output);
    let unit = value["unit"].as_str().unwrap();
    assert!(unit.contains("ExecStart=%h/.local/bin/ricoh-monitor run"));
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("TimeoutStopSec=10s"));
}

#[cfg(unix)]
#[tokio::test]
async fn cli_intent_commands_control_a_fixture_runtime_across_processes() {
    let temp = TempDir::new();
    let binary = env!("CARGO_BIN_EXE_ricoh-monitor");
    let setup = invoke(binary, &temp.0, &["setup"]).await;
    assert!(
        setup.status.success(),
        "setup failed at {}: {}",
        temp.0.display(),
        String::from_utf8_lossy(&setup.stderr)
    );
    seed_ready_setup(&temp.0);
    let prepared = json(invoke(binary, &temp.0, &["status"]).await);
    assert_eq!(prepared["setupCompleted"], true);
    assert_eq!(prepared["runtime"]["state"], "stopped");
    let app = MonitorApp::open_with_scheduler_client(&temp.0, Arc::new(FixtureClient)).unwrap();
    let runtime = {
        let app = app.clone();
        tokio::spawn(async move { app.run().await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;

    for (action, expected) in [
        ("start", "monitoring"),
        ("pause", "paused"),
        ("resume", "monitoring"),
        ("stop", "stopped"),
    ] {
        let _ = json(invoke(binary, &temp.0, &[action]).await);
        let status = json(invoke(binary, &temp.0, &["status"]).await);
        assert_eq!(status["runtime"]["state"], expected);
        assert!(!runtime.is_finished(), "{action} 不应结束前台服务");
    }

    app.shutdown();
    tokio::time::timeout(Duration::from_secs(3), runtime)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let status = json(invoke(binary, &temp.0, &["status"]).await);
    assert_eq!(status["runtime"]["state"], "stopped");
}

#[cfg(unix)]
#[tokio::test]
async fn cli_run_waits_at_default_stopped_and_signal_preserves_intent() {
    let temp = TempDir::new();
    let binary = env!("CARGO_BIN_EXE_ricoh-monitor");
    let setup = invoke(binary, &temp.0, &["setup"]).await;
    assert!(
        setup.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let mut run = Command::new(binary)
        .args(["run", "--data-dir"])
        .arg(&temp.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        run.try_wait().unwrap().is_none(),
        "Stopped 意愿下 run 应驻留等待"
    );

    let pid = run.id().unwrap().to_string();
    let signal = std::process::Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .unwrap();
    assert!(signal.success());
    let output = tokio::time::timeout(Duration::from_secs(3), run.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 1);
    let status = json(invoke(binary, &temp.0, &["status"]).await);
    assert_eq!(status["runtime"]["state"], "stopped");
}
