use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use ricoh_monitor_core::{
    app::MonitorApp,
    availability::Availability,
    ricoh::ProductDetail,
    ricoh_api::{RetryAfter, RicohApiError},
    scheduler::{LineId, SchedulerClient},
    storage::{ProductIdentity, Storage, StoredProxy},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct TestDirectory(PathBuf);

struct CapturingClient(Arc<Mutex<Vec<LineId>>>);

struct SequenceClient(Arc<Mutex<Vec<LineId>>>);

impl SchedulerClient for CapturingClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<ProductDetail, RicohApiError>> + Send + 'static,
        >,
    > {
        self.0.lock().unwrap().push(line);
        Box::pin(async { Err(RicohApiError::InvalidConfiguration) })
    }
}

impl SchedulerClient for SequenceClient {
    fn product_detail(
        &self,
        line: LineId,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<ProductDetail, RicohApiError>> + Send + 'static,
        >,
    > {
        let mut calls = self.0.lock().unwrap();
        calls.push(line.clone());
        let number = calls.len();
        Box::pin(async move {
            match number {
                1..=3 => Err(RicohApiError::Transport("本地连接失败".into())),
                5 => Err(RicohApiError::RateLimited {
                    retry_after: Some(RetryAfter::Delay(Duration::from_millis(80))),
                }),
                _ => Ok(ProductDetail {
                    product_id: line.product_id,
                    name: "本地样例商品".into(),
                    is_show: 1,
                    stock: serde_json::json!(1).as_number().unwrap().clone(),
                    availability: Availability::InStock,
                    metadata: Default::default(),
                }),
            }
        })
    }
}

impl TestDirectory {
    fn new() -> Self {
        static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ricoh-proxy-http-acceptance-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn configured_proxy_receives_the_monitor_connect_request() {
    let directory = TestDirectory::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let proxy = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 2048];
        let read = socket.read(&mut request).await.unwrap();
        socket
            .write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        String::from_utf8_lossy(&request[..read]).into_owned()
    });

    let mut config = ricoh_monitor_core::config::MonitorConfig::default();
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.schedule.start_minute = 0;
    config.schedule.end_minute = 23 * 60 + 59;
    config.use_proxy_pool = true;
    config.requests.connect_timeout = Duration::from_secs(1);
    config.requests.total_timeout = Duration::from_secs(2);
    {
        let mut storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        storage.save_monitor_config(&config).unwrap();
        storage
            .save_proxy(&StoredProxy {
                id: "local-proxy".into(),
                protocol: "http".into(),
                host: "127.0.0.1".into(),
                port,
                credential_ref: None,
                enabled: true,
                status: "available".into(),
                cooldown_until_ms: None,
                consecutive_failures: 0,
            })
            .unwrap();
        storage
            .save_product_config(
                ProductIdentity {
                    key: "246".into(),
                    product_id: "246".into(),
                    sku_id: None,
                },
                "本地代理样例商品".into(),
                "fixture".into(),
                Some(1),
            )
            .unwrap();
        storage.set_product_enabled("246", true).unwrap();
        storage.complete_setup().unwrap();
    }

    let app = MonitorApp::open(&directory.0).unwrap();
    app.set_platform_notifications_available(true);
    let runner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    let request = tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("本地代理应收到监控连接")
        .unwrap();
    assert!(
        request.starts_with("CONNECT "),
        "代理请求应为 CONNECT：{request}"
    );
    assert!(request.contains(":443 HTTP/1.1"));

    app.shutdown();
    runner.await.unwrap().unwrap();
}

#[tokio::test]
async fn manual_validation_uses_available_pool_proxy() {
    let directory = TestDirectory::new();
    let selected = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let selected_port = selected.local_addr().unwrap().port();

    let mut config = ricoh_monitor_core::config::MonitorConfig::default();
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.use_proxy_pool = true;
    config.use_system_proxy = false;
    config.requests.connect_timeout = Duration::from_secs(1);
    config.requests.total_timeout = Duration::from_secs(2);
    {
        let mut storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        storage.save_monitor_config(&config).unwrap();
        storage
            .save_proxy(&StoredProxy {
                id: "selected-proxy".into(),
                protocol: "http".into(),
                host: "127.0.0.1".into(),
                port: selected_port,
                credential_ref: None,
                enabled: true,
                status: "available".into(),
                cooldown_until_ms: None,
                consecutive_failures: 0,
            })
            .unwrap();
    }

    let app = MonitorApp::open(&directory.0).unwrap();
    let selected_request = tokio::spawn(async move {
        let (mut socket, _) = selected.accept().await.unwrap();
        let mut request = [0; 2048];
        let read = socket.read(&mut request).await.unwrap();
        socket
            .write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        String::from_utf8_lossy(&request[..read]).into_owned()
    });
    let result = app.validate_product(246).await;
    let error = result.unwrap_err().to_string();
    assert!(error.contains("网络：显式代理"), "{error}");
    let request = tokio::time::timeout(Duration::from_secs(2), selected_request)
        .await
        .expect("手动验证应连接已选代理")
        .unwrap();
    assert!(request.starts_with("CONNECT shop.ricn-mall.com:443 HTTP/1.1"));
}

#[tokio::test]
async fn manual_validation_rejects_an_empty_proxy_pool() {
    let directory = TestDirectory::new();
    let mut config = ricoh_monitor_core::config::MonitorConfig::default();
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.use_proxy_pool = true;
    Storage::open(directory.0.join("monitor.sqlite3"))
        .unwrap()
        .save_monitor_config(&config)
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let app = MonitorApp::open_with_scheduler_client(
        &directory.0,
        Arc::new(CapturingClient(calls.clone())),
    )
    .unwrap();
    let result = app.validate_product(246).await.unwrap_err();
    assert!(result.to_string().contains("代理池没有可用出口"));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn manual_validation_passes_selected_proxy_id_to_override() {
    let directory = TestDirectory::new();
    let mut config = ricoh_monitor_core::config::MonitorConfig::default();
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.use_proxy_pool = true;
    {
        let mut storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        storage.save_monitor_config(&config).unwrap();
        storage
            .save_proxy(&StoredProxy {
                id: "selected-proxy".into(),
                protocol: "http".into(),
                host: "127.0.0.1".into(),
                port: 1,
                credential_ref: None,
                enabled: true,
                status: "available".into(),
                cooldown_until_ms: None,
                consecutive_failures: 0,
            })
            .unwrap();
    }
    let calls = Arc::new(Mutex::new(Vec::new()));
    let app = MonitorApp::open_with_scheduler_client(
        &directory.0,
        Arc::new(CapturingClient(calls.clone())),
    )
    .unwrap();
    assert!(app.validate_product(246).await.is_err());
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[LineId::new(246, "selected-proxy")]
    );
}

#[tokio::test]
async fn manual_validation_updates_proxy_health_without_fallback() {
    let directory = TestDirectory::new();
    let mut config = ricoh_monitor_core::config::MonitorConfig::default();
    config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
    config.use_proxy_pool = true;
    config.scan.interval = Duration::from_millis(1);
    config.requests.global_requests_per_second = 1_000.0;
    config.requests.global_burst = 10;
    {
        let mut storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
        storage.save_monitor_config(&config).unwrap();
        for (id, failures) in [("first-proxy", 0), ("second-proxy", 2)] {
            storage
                .save_proxy(&StoredProxy {
                    id: id.into(),
                    protocol: "http".into(),
                    host: "127.0.0.1".into(),
                    port: 1,
                    credential_ref: None,
                    enabled: true,
                    status: "available".into(),
                    cooldown_until_ms: None,
                    consecutive_failures: failures,
                })
                .unwrap();
        }
    }
    let calls = Arc::new(Mutex::new(Vec::new()));
    let app = MonitorApp::open_with_scheduler_client(
        &directory.0,
        Arc::new(SequenceClient(calls.clone())),
    )
    .unwrap();
    for _ in 0..3 {
        assert!(app.validate_product(246).await.is_err());
    }
    let storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
    let first = storage.proxies().unwrap().into_iter().next().unwrap();
    assert_eq!(first.id, "first-proxy");
    assert_eq!(first.status, "cooldown");
    assert_eq!(first.consecutive_failures, 3);
    drop(storage);

    assert!(app.validate_product(246).await.is_ok());
    let mut storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
    let second = storage.proxies().unwrap().into_iter().nth(1).unwrap();
    assert_eq!(second.consecutive_failures, 0);
    assert_eq!(second.status, "available");
    storage.record_proxy_failure("second-proxy").unwrap();
    storage.record_proxy_failure("second-proxy").unwrap();
    drop(storage);

    assert!(app.validate_product(246).await.is_err());
    let storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
    let second = storage.proxies().unwrap().into_iter().nth(1).unwrap();
    assert_eq!(second.consecutive_failures, 2, "429 不改变代理健康计数");
    drop(storage);
    let started = std::time::Instant::now();
    assert!(app.validate_product(246).await.is_ok());
    assert!(started.elapsed() >= Duration::from_millis(40));
    let storage = Storage::open(directory.0.join("monitor.sqlite3")).unwrap();
    let second = storage.proxies().unwrap().into_iter().nth(1).unwrap();
    assert_eq!(second.consecutive_failures, 0);

    let outlets = calls
        .lock()
        .unwrap()
        .iter()
        .map(|line| line.outlet_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        outlets,
        [
            "first-proxy",
            "first-proxy",
            "first-proxy",
            "second-proxy",
            "second-proxy",
            "second-proxy",
        ]
    );
}
