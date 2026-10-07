use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    process,
};

use ricoh_monitor_cli::{default_data_dir, service_unit};
use ricoh_monitor_core::app::{
    AppError, ChannelEvent, ChannelInput, MonitorApp, MonitoringAction, ProxyInput, ProxyProtocol,
    ScanAction,
};
use serde_json::{json, Value};

#[tokio::main]
async fn main() {
    #[cfg(target_os = "windows")]
    if ricoh_monitor_core::system_proxy::windows::run_helper() { return; }
    if let Err(error) = execute().await {
        eprintln!("{error}");
        process::exit(2);
    }
}

async fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        return Err(usage().into());
    };
    if matches!(command.as_str(), "--help" | "help") {
        println!("ricoh-monitor <setup|run|status|start|pause|resume|stop|config|products|notifications|proxies|service>");
        return Ok(());
    }

    let mut data_dir = None;
    let mut output = None;
    let mut positional = Vec::new();
    let mut json_mode = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_mode = true,
            "--data-dir" => data_dir = Some(PathBuf::from(args.next().ok_or_else(usage)?)),
            "--output" => output = Some(PathBuf::from(args.next().ok_or_else(usage)?)),
            _ if arg.starts_with('-') => return Err(usage().into()),
            _ => positional.push(arg),
        }
    }
    let data_dir = data_dir.map(Ok).unwrap_or_else(default_data_dir)?;

    if command == "service" {
        if positional.as_slice() != ["unit"] {
            return Err(usage().into());
        }
        if let Some(path) = output {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, service_unit())?;
            print(&json!({"written": path}))?;
            return Ok(());
        }
        print(&json!({"unit": service_unit()}))?;
        return Ok(());
    }

    let app = MonitorApp::open(&data_dir)?;
    let notification_runtime = env::var_os("RM_NOTIFICATION_RUNTIME_PATH")
        .map(PathBuf::from)
        .unwrap_or(env::current_exe()?.with_file_name("notification-runtime"));
    let notification_script = env::var_os("RM_NOTIFICATION_RUNTIME_SCRIPT")
        .map(PathBuf::from)
        .unwrap_or(notification_runtime.with_file_name("notification-runtime.mjs"));
    app.set_notification_runtime_path(notification_runtime);
    let runtime_arguments = vec![
        "--no-env-file".into(),
        "--no-install".into(),
        notification_script.to_string_lossy().into_owned(),
    ];
    #[cfg(target_os = "windows")]
    let runtime_arguments = {
        let mut args = runtime_arguments;
        args.extend(["--system-proxy-helper".into(), env::current_exe()?.to_string_lossy().into_owned()]);
        app.set_notification_proxy_url(ricoh_monitor_core::system_proxy::windows::current());
        args
    };
    app.set_notification_runtime_arguments(runtime_arguments);
    match command.as_str() {
        "setup" => setup(&app, !json_mode).await?,
        "run" if positional.is_empty() => run(&app).await?,
        "status" if positional.is_empty() => print(&json!(app.snapshot().await?))?,
        "start" if positional.is_empty() => action(&app, MonitoringAction::Start).await?,
        "pause" if positional.is_empty() => action(&app, MonitoringAction::Pause).await?,
        "resume" if positional.is_empty() => action(&app, MonitoringAction::Resume).await?,
        "stop" if positional.is_empty() => action(&app, MonitoringAction::Stop).await?,
        "config" => config(&app, &positional).await?,
        "products" => products(&app, &positional).await?,
        "notifications" => notifications(&app, &positional).await?,
        "proxies" => proxies(&app, &positional).await?,
        _ => return Err(usage().into()),
    }
    Ok(())
}

async fn setup(app: &MonitorApp, interactive: bool) -> Result<(), Box<dyn std::error::Error>> {
    let snapshot = app.snapshot().await?;
    if !snapshot.setup_completed && interactive && io::stdin().is_terminal() {
        let mut config = snapshot.config;
        eprintln!("默认监控时间为北京时间每日 09:00–19:00，默认直连。按提示可添加要监控的商品和通知渠道；商品与渠道测试成功后才会开始监控。");
        let start = prompt("监控开始时间（回车采用默认）", &config.schedule.start)?;
        let end = prompt("监控结束时间（回车采用默认）", &config.schedule.end)?;
        let mut value = serde_json::to_value(&config)?;
        value["schedule"]["start"] = json!(start);
        value["schedule"]["end"] = json!(end);
        config = serde_json::from_value(value)?;
        app.save_config(config).await?;

        let product_id = prompt("理光 Product ID（留空跳过）", "")?;
        if !product_id.is_empty() {
            let product_id = product_id.parse::<u64>()?;
            app.add_product(product_id).await?;
        }

        let channel_id = prompt("通知渠道 ID（留空跳过）", "")?;
        if !channel_id.is_empty() {
            let providers = app.snapshot().await?.providers;
            for provider in &providers {
                eprintln!("{}：{}", provider.id, provider.name);
            }
            let name = prompt("通知渠道名称", &channel_id)?;
            let provider_id = prompt("通知提供商 ID", "feishu")?;
            let values_path = prompt("渠道字段 JSON 文件路径", "")?;
            let values = serde_json::from_str(&fs::read_to_string(values_path)?)?;
            app.save_channel(ChannelInput {
                id: Some(channel_id.clone()),
                binding_id: None,
                targets: None,
                name,
                provider_id,
                values,
                subscriptions: vec![
                    ChannelEvent::StockAvailable,
                    ChannelEvent::MonitoringFailed,
                    ChannelEvent::Recovered,
                ],
            })
            .await?;
            let test = app.test_channel(&channel_id).await?;
            eprintln!("通知测试结果：{}", test.outcome);
            if test.outcome == "accepted" {
                app.set_channel_enabled(&channel_id, true).await?;
            }
        }

        match app.complete_setup().await {
            Ok(()) => {}
            Err(AppError::InvalidInput(_)) => eprintln!("尚未启用有效商品，监控保持停止。"),
            Err(error) => return Err(error.into()),
        }
    }
    let result = app.snapshot().await?;
    if !result.setup_completed {
        eprintln!("基础配置尚未完成：需至少添加一个启用商品并确认设置；监控保持停止。可用 products add 添加商品，通知渠道可稍后配置。");
    }
    print(&json!(result))?;
    Ok(())
}

async fn config(app: &MonitorApp, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args {
        [action] if action == "show" => print(&json!(app.snapshot().await?.config))?,
        [action, key, value] if action == "set" => {
            let mut config = serde_json::to_value(app.snapshot().await?.config)?;
            set_config_value(&mut config, key, value)?;
            app.save_config(serde_json::from_value(config)?).await?;
            print(&json!(app.snapshot().await?.config))?;
        }
        [action, path] if action == "import" => {
            app.import_config(&fs::read_to_string(path)?).await?;
            print(&json!(app.snapshot().await?.config))?;
        }
        [action, path] if action == "export" => {
            let content = app.export_config().await?;
            if path == "-" {
                print(&json!({"config": serde_json::from_str::<Value>(&content)?}))?;
            } else {
                fs::write(path, content)?;
                print(&json!({"written": path}))?;
            }
        }
        [action] if action == "defaults" => {
            app.restore_defaults(false).await?;
            print(&json!(app.snapshot().await?.config))?;
        }
        _ => return Err(usage().into()),
    }
    Ok(())
}

async fn products(app: &MonitorApp, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args {
        [action] if action == "list" => {
            let snapshot = app.snapshot().await?;
            let mut products = snapshot.products;
            products.extend(snapshot.catalog);
            products.sort_by_key(|product| product.product_id.parse::<u64>().unwrap_or_default());
            print(&json!(products))?;
        }
        [action, id] if action == "add" => {
            let product_id = id.parse::<u64>()?;
            app.add_product(product_id).await?;
            print(&json!({"productId": product_id, "added": true, "enabled": true}))?;
        }
        [action, id] if action == "remove" => {
            let id = id.parse::<u64>()?;
            app.remove_product(id).await?;
            print(&json!({"removed": id}))?;
        }
        [action, id, state]
            if action == "set" && matches!(state.as_str(), "enabled" | "disabled") =>
        {
            let id = id.parse::<u64>()?;
            let enabled = state == "enabled";
            app.set_product_enabled(id, enabled).await?;
            print(&json!({"productId": id, "enabled": enabled}))?;
        }
        [action, start, end] if action == "scan" => {
            app.start_product_scan(start.parse()?, end.parse()?).await?;
            print(&json!(wait_for_scan_owner(app).await?))?;
        }
        [action, control] if action == "scan" => {
            let action = match control.as_str() {
                "pause" => ScanAction::Pause,
                "resume" => ScanAction::Resume,
                "cancel" => ScanAction::Cancel,
                _ => return Err(usage().into()),
            };
            let scan = app.control_product_scan(action).await?;
            if action == ScanAction::Resume {
                print(&json!(wait_for_scan_owner(app).await?))?;
            } else {
                print(&json!(scan))?;
            }
        }
        _ => return Err(usage().into()),
    }
    Ok(())
}

async fn wait_for_scan_owner(
    app: &MonitorApp,
) -> Result<ricoh_monitor_core::app::ProductScan, Box<dyn std::error::Error>> {
    let mut owner = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    tokio::select! {
        result = app.wait_for_scan() => {
            let scan = result?;
            app.shutdown();
            match owner.await? {
                Ok(()) | Err(ricoh_monitor_core::app::AppError::AlreadyRunning) => Ok(scan),
                Err(error) => Err(error.into()),
            }
        }
        result = &mut owner => {
            match result? {
                Err(ricoh_monitor_core::app::AppError::AlreadyRunning) => {
                    app.wait_for_scan().await.map_err(Into::into)
                }
                Err(error) => Err(error.into()),
                Ok(()) => Err("扫描执行进程提前退出".into()),
            }
        }
    }
}

async fn notifications(
    app: &MonitorApp,
    args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    match args {
        [action] if action == "list" => print(&json!(app.snapshot().await?.channels))?,
        [action] if action == "providers" => print(&json!(app.snapshot().await?.providers))?,
        [action, id, name, provider, secret_file, events] if action == "add" => {
            let values: std::collections::BTreeMap<String, String> =
                serde_json::from_str(&fs::read_to_string(secret_file)?)?;
            let input = ChannelInput {
                id: Some(id.clone()),
                binding_id: None,
                targets: None,
                name: name.clone(),
                provider_id: provider.clone(),
                values,
                subscriptions: parse_channel_events(events)?,
            };
            let result = app.save_channel(input).await?;
            print(&json!(result))?;
        }
        [action, id, name, provider, secret_file] if action == "add" => {
            let values = serde_json::from_str(&fs::read_to_string(secret_file)?)?;
            let result = app
                .save_channel(ChannelInput {
                    id: Some(id.clone()),
                    binding_id: None,
                    targets: None,
                    name: name.clone(),
                    provider_id: provider.clone(),
                    values,
                    subscriptions: vec![ChannelEvent::StockAvailable],
                })
                .await?;
            print(&json!(result))?;
        }
        [action, id] if action == "test" => print(&json!(app.test_channel(id).await?))?,
        [action, id] if matches!(action.as_str(), "enable" | "disable") => {
            let enabled = action == "enable";
            app.set_channel_enabled(id, enabled).await?;
            print(&json!({"id": id, "enabled": enabled}))?;
        }
        [action, id] if action == "remove" => {
            app.remove_channel(id).await?;
            print(&json!({"removed": id}))?;
        }
        _ => return Err(usage().into()),
    }
    Ok(())
}

async fn proxies(app: &MonitorApp, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args {
        [action] if action == "list" => print(&json!(app.snapshot().await?.proxies))?,
        [action, protocol, host, port, auth_file] if action == "add" => {
            let protocol = match protocol.as_str() {
                "http" => ProxyProtocol::Http,
                "https" => ProxyProtocol::Https,
                "socks5" => ProxyProtocol::Socks5,
                _ => return Err(usage().into()),
            };
            let auth: std::collections::BTreeMap<String, String> =
                serde_json::from_str(&fs::read_to_string(auth_file)?)?;
            let proxy = ProxyInput {
                protocol,
                host: host.clone(),
                port: port.parse()?,
                username: auth.get("username").cloned(),
                password: auth.get("password").cloned(),
            };
            print(&json!(app.add_proxy(proxy).await?))?;
        }
        [action, protocol, host, port] if action == "add" => {
            let protocol = match protocol.as_str() {
                "http" => ProxyProtocol::Http,
                "https" => ProxyProtocol::Https,
                "socks5" => ProxyProtocol::Socks5,
                _ => return Err(usage().into()),
            };
            let proxy = ProxyInput {
                protocol,
                host: host.clone(),
                port: port.parse()?,
                username: None,
                password: None,
            };
            print(&json!(app.add_proxy(proxy).await?))?;
        }
        [action, path] if action == "import" => {
            print(&json!(
                app.import_proxies(&fs::read_to_string(path)?).await?
            ))?;
        }
        [action, id] if action == "test" => print(&json!(app.test_proxy(id).await?))?,
        [action, id] if matches!(action.as_str(), "enable" | "disable") => {
            let enabled = action == "enable";
            app.set_proxy_enabled(id, enabled).await?;
            print(&json!({"id": id, "enabled": enabled}))?;
        }
        [action, id] if action == "remove" => {
            app.remove_proxy(id).await?;
            print(&json!({"removed": id}))?;
        }
        _ => return Err(usage().into()),
    }
    Ok(())
}

async fn action(
    app: &MonitorApp,
    action: MonitoringAction,
) -> Result<(), Box<dyn std::error::Error>> {
    app.monitoring_action(action).await?;
    print(&json!(app.snapshot().await?))?;
    Ok(())
}

async fn run(app: &MonitorApp) -> Result<(), Box<dyn std::error::Error>> {
    print(&json!({
        "mode": "foreground",
        "message": "Ctrl+C 或 systemd stop 结束进程；start、pause、resume、stop 控制监控意愿"
    }))?;
    let run_loop = app.run();
    tokio::pin!(run_loop);
    let result = {
        #[cfg(unix)]
        {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                result = &mut run_loop => result,
                _ = tokio::signal::ctrl_c() => {
                    app.shutdown();
                    run_loop.await
                },
                _ = terminate.recv() => {
                    app.shutdown();
                    run_loop.await
                },
            }
        }
        #[cfg(not(unix))]
        tokio::select! {
            result = &mut run_loop => result,
            _ = tokio::signal::ctrl_c() => {
                app.shutdown();
                run_loop.await
            },
        }
    };
    app.shutdown();
    result?;
    Ok(())
}

fn set_config_value(
    config: &mut Value,
    key: &str,
    input: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (section, field, value) = match key {
        "start" | "end" => ("schedule", key, json!(input)),
        "days" => (
            "schedule",
            key,
            Value::Array(input.split(',').map(|day| json!(day.trim())).collect()),
        ),
        "interval-min-ms" => ("rate", "intervalMinMs", json!(input.parse::<u64>()?)),
        "interval-max-ms" => ("rate", "intervalMaxMs", json!(input.parse::<u64>()?)),
        "failures-before-backoff" => (
            "rate",
            "failuresBeforeBackoff",
            json!(input.parse::<u32>()?),
        ),
        "failure-backoff-seconds" => (
            "rate",
            "failureBackoffSeconds",
            json!(input.parse::<u64>()?),
        ),
        "use-system-proxy" => ("", "useSystemProxy", json!(input.parse::<bool>()?)),
        "use-proxy-pool" => ("", "useProxyPool", json!(input.parse::<bool>()?)),
        "failure-alert-after-minutes" => {
            ("", "failureAlertAfterMinutes", json!(input.parse::<u32>()?))
        }
        _ => return Err(usage().into()),
    };
    if section.is_empty() {
        config[field] = value;
    } else {
        config[section][field] = value;
    }
    Ok(())
}

fn parse_channel_events(value: &str) -> Result<Vec<ChannelEvent>, io::Error> {
    value
        .split(',')
        .map(|event| match event.trim() {
            "stock_available" => Ok(ChannelEvent::StockAvailable),
            "monitoring_failed" => Ok(ChannelEvent::MonitoringFailed),
            "recovered" => Ok(ChannelEvent::Recovered),
            _ => Err(usage()),
        })
        .collect()
}

fn prompt(label: &str, default: &str) -> io::Result<String> {
    eprint!("{label} [{default}]: ");
    io::stderr().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    let value = value.trim();
    Ok(if value.is_empty() {
        default.to_owned()
    } else {
        value.to_owned()
    })
}

fn print(value: &Value) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn usage() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "用法：ricoh-monitor <setup|run|status|start|pause|resume|stop|config|products|notifications|proxies|service>；执行 --help 查看命令格式")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ricoh_monitor_core::{
        availability::Availability,
        ricoh::ProductDetail,
        scheduler::{LineId, SchedulerClient},
    };
    use std::{future::Future, pin::Pin, sync::Arc};

    struct FixtureClient(std::sync::atomic::AtomicUsize);

    impl SchedulerClient for FixtureClient {
        fn product_detail(
            &self,
            line: LineId,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            ProductDetail,
                            ricoh_monitor_core::ricoh_api::RicohApiError,
                        >,
                    > + Send,
            >,
        > {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::pin(async move {
                Ok(ProductDetail {
                    metadata: Default::default(),
                    product_id: line.product_id,
                    name: format!("Fixture {}", line.product_id),
                    is_show: 0,
                    stock: serde_json::json!(0).as_number().unwrap().clone(),
                    availability: Availability::OutOfStock,
                })
            })
        }
    }

    #[tokio::test]
    async fn cli_scan_starts_and_awaits_its_local_run_owner() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-cli-scan-owner-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let mut storage =
            ricoh_monitor_core::storage::Storage::open(directory.join("monitor.sqlite3")).unwrap();
        let mut config = storage.monitor_config().unwrap();
        config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
        config.scan.interval = std::time::Duration::from_millis(10);
        config.requests.global_requests_per_second = 100.0;
        storage.save_monitor_config(&config).unwrap();
        drop(storage);
        let client = Arc::new(FixtureClient(std::sync::atomic::AtomicUsize::new(0)));
        let app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        app.start_product_scan(700, 701).await.unwrap();
        let scan =
            tokio::time::timeout(std::time::Duration::from_secs(5), wait_for_scan_owner(&app))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(scan.status, ricoh_monitor_core::app::ScanStatus::Completed);
        assert_eq!(scan.checked, 2);
        assert_eq!(client.0.load(std::sync::atomic::Ordering::Relaxed), 2);
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn cli_scan_uses_an_existing_run_owner_without_starting_a_second_worker() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-cli-existing-scan-owner-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let mut storage =
            ricoh_monitor_core::storage::Storage::open(directory.join("monitor.sqlite3")).unwrap();
        let mut config = storage.monitor_config().unwrap();
        config.monitoring_mode = ricoh_monitor_core::config::MonitoringMode::ProductDetail;
        config.scan.interval = std::time::Duration::from_millis(10);
        config.requests.global_requests_per_second = 100.0;
        storage.save_monitor_config(&config).unwrap();
        drop(storage);
        let client = Arc::new(FixtureClient(std::sync::atomic::AtomicUsize::new(0)));
        let owner_app = MonitorApp::open_with_scheduler_client(&directory, client.clone()).unwrap();
        owner_app.start_product_scan(700, 701).await.unwrap();
        let owner = tokio::spawn({
            let app = owner_app.clone();
            async move { app.run().await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let cli_app = MonitorApp::open(&directory).unwrap();
        let scan = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            wait_for_scan_owner(&cli_app),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(scan.status, ricoh_monitor_core::app::ScanStatus::Completed);
        assert_eq!(client.0.load(std::sync::atomic::Ordering::Relaxed), 2);
        owner_app.shutdown();
        owner.await.unwrap().unwrap();
        drop(cli_app);
        drop(owner_app);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
