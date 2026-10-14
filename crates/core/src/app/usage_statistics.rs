use super::{AppError, AppSnapshot, MonitorApp, RunIntent};
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch, Mutex};

const SERVICE: &str = include_str!("../../../../apps/desktop/usage-statistics.json");
const APP_PACKAGE: &str = include_str!("../../../../apps/desktop/package.json");
const EVENT_CAPACITY: usize = 64;

#[derive(Clone)]
pub(super) struct UsageStatistics {
    consent: watch::Sender<Option<String>>,
    events: mpsc::Sender<(String, &'static str)>,
    receiver: Arc<Mutex<Option<mpsc::Receiver<(String, &'static str)>>>>,
}

impl UsageStatistics {
    pub(super) fn disable(&self) {
        self.consent.send_replace(None);
    }

    pub(super) fn new(identity: Option<String>) -> Self {
        let (consent, _) = watch::channel(identity);
        let (events, receiver) = mpsc::channel(EVENT_CAPACITY);
        Self {
            consent,
            events,
            receiver: Arc::new(Mutex::new(Some(receiver))),
        }
    }

    pub(super) fn monitoring_started(&self) {
        if let Some(identity) = self.consent.borrow().clone() {
            // ponytail: 统计拥塞时丢弃事件，监控操作始终优先。
            let _ = self.events.try_send((identity, "monitoring_started"));
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Counts(Value);

impl Counts {
    fn from_snapshot(snapshot: &AppSnapshot) -> Self {
        let mut groups = 0usize;
        let mut private = 0usize;
        let mut discovered_groups = 0usize;
        let mut discovered_private = 0usize;
        let mut providers = serde_json::Map::new();
        for channel in &snapshot.channels {
            let known = channel
                .targets
                .iter()
                .chain(&channel.selected_targets)
                .map(|target| (target.kind.as_str(), target.id.as_str()))
                .collect::<BTreeSet<_>>();
            discovered_groups += known.iter().filter(|(kind, _)| *kind == "chat").count();
            discovered_private += known.iter().filter(|(kind, _)| *kind == "user").count();
            let counts = providers
                .entry(channel.provider_id.clone())
                .or_insert(json!({"channels":0,"groups":0,"private":0}));
            counts["channels"] = json!(counts["channels"].as_u64().unwrap_or(0) + 1);
            if channel.enabled {
                for target in &channel.selected_targets {
                    let field = match target.kind.as_str() {
                        "chat" => {
                            groups += 1;
                            "groups"
                        }
                        "user" => {
                            private += 1;
                            "private"
                        }
                        _ => continue,
                    };
                    counts[field] = json!(counts[field].as_u64().unwrap_or(0) + 1);
                }
            }
        }
        Self(json!({
            "monitored_products": snapshot.products.len(),
            "configured_channels": snapshot.channels.len(),
            "enabled_channels": snapshot.channels.iter().filter(|channel| channel.enabled).count(),
            "group_targets": groups,
            "private_targets": private,
            "discovered_group_targets": discovered_groups,
            "discovered_private_targets": discovered_private,
            "providers": providers
        }))
    }

    fn event(&self, identity: &str, event: &str, token: &str) -> Value {
        let mut properties = self.0.clone();
        properties["$process_person_profile"] = json!(false);
        properties["$geoip_disable"] = json!(true);
        properties["platform"] = json!(std::env::consts::OS);
        properties["architecture"] = json!(std::env::consts::ARCH);
        properties["app_version"] =
            serde_json::from_str::<Value>(APP_PACKAGE).unwrap()["version"].clone();
        properties["environment"] = json!(if cfg!(debug_assertions) {
            "development"
        } else {
            "production"
        });
        json!({"api_key":token,"distinct_id":identity,"event":event,"properties":properties})
    }
}

impl MonitorApp {
    pub async fn set_usage_statistics_enabled(&self, enabled: bool) -> Result<(), AppError> {
        let identity = self
            .call(move |db| {
                db.set_usage_statistics_enabled(enabled)?;
                db.usage_statistics_identity()
            })
            .await?;
        self.usage_statistics.consent.send_replace(identity);
        self.snapshot().await?;
        Ok(())
    }

    pub(super) async fn usage_statistics_loop(&self) {
        self.usage_statistics_loop_to(
            &serde_json::from_str(SERVICE).unwrap(),
            Duration::from_secs(3600),
        )
        .await;
    }

    async fn usage_statistics_loop_to(&self, service: &Value, period: Duration) {
        let mut snapshots = self.subscribe();
        let Ok(initial) = self.snapshot().await else {
            return;
        };
        let mut counts = Counts::from_snapshot(&initial);
        let mut consent = self.usage_statistics.consent.subscribe();
        let Some(mut events) = self.usage_statistics.receiver.lock().await.take() else {
            return;
        };
        let token = service["projectToken"].as_str().unwrap();
        let endpoint = service["endpoint"].as_str().unwrap();
        let mut heartbeat = tokio::time::interval(period);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut configuration_due = None;
        if consent.borrow().is_some()
            && self.storage.lock().unwrap().run_intent().ok() == Some(RunIntent::Running)
            && self.usage_statistics.events.capacity() == EVENT_CAPACITY
        {
            self.usage_statistics.monitoring_started();
        }
        loop {
            let event = tokio::select! {
                _ = consent.changed() => {
                    // 关闭时清空待发统计，并更换下次同意时的随机标识。
                    configuration_due = None;
                    if consent.borrow().is_some() { "app_active" } else {
                        while events.try_recv().is_ok() {}
                        continue;
                    }
                }
                snapshot = snapshots.recv() => {
                    if let Ok(snapshot) = snapshot {
                        let next = Counts::from_snapshot(&snapshot);
                        if next != counts {
                            counts = next;
                            configuration_due = consent.borrow().is_some().then(|| tokio::time::Instant::now() + Duration::from_secs(5));
                        }
                    }
                    continue;
                }
                _ = heartbeat.tick() => "app_active",
                _ = async {
                    if let Some(deadline) = configuration_due { tokio::time::sleep_until(deadline).await; }
                    else { std::future::pending::<()>().await; }
                } => { configuration_due = None; "notification_configuration_changed" }
                event = events.recv() => {
                    let Some((identity, event)) = event else { return };
                    if consent.borrow().as_ref() != Some(&identity) { continue; }
                    event
                }
            };
            let Some(identity) = consent.borrow().clone() else {
                continue;
            };
            // 有效的同意标识必须仍存在于已保存偏好中。
            if self
                .storage
                .lock()
                .unwrap()
                .usage_statistics_identity()
                .ok()
                .flatten()
                .as_ref()
                != Some(&identity)
            {
                continue;
            }
            if token.is_empty() {
                continue;
            }
            let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(5));
            if let Ok(Some(proxy)) = self.notification_proxy_url.lock().unwrap().clone() {
                if let Ok(proxy) = reqwest::Proxy::all(proxy) {
                    builder = builder.proxy(proxy);
                }
            }
            let Ok(client) = builder.build() else {
                continue;
            };
            let request = client
                .post(endpoint)
                .header("Content-Type", "application/json")
                .body(counts.event(&identity, event, token).to_string())
                .send();
            tokio::select! {
                _ = request => {},
                _ = consent.changed() => {
                    configuration_due = None;
                    if consent.borrow().is_some() { heartbeat.reset_immediately(); }
                    else { while events.try_recv().is_ok() {} }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{notifications::NotificationTarget, storage::Storage};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn usage_capture_obeys_consent_counts_only_and_cancels_slow_requests() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/capture", listener.local_addr().unwrap());
        let (captured, mut received) = mpsc::channel::<Value>(32);
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let captured = captured.clone();
                tokio::spawn(async move {
                    let mut data = Vec::new();
                    let mut buffer = [0; 4096];
                    let event = loop {
                        let n = socket.read(&mut buffer).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        data.extend_from_slice(&buffer[..n]);
                        if let Some(end) = data.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&data[..end]);
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length: ")
                                        .and_then(|s| s.parse::<usize>().ok())
                                })
                                .unwrap();
                            if data.len() >= end + 4 + length {
                                break serde_json::from_slice::<Value>(
                                    &data[end + 4..end + 4 + length],
                                )
                                .unwrap();
                            }
                        }
                    };
                    captured.send(event).await.unwrap();
                    // 保持响应挂起，验收 opt-out 中止在途请求且不会卡住本地操作。
                    let _ = socket.read(&mut buffer).await;
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                        .await;
                });
            }
        });
        let directory =
            std::env::temp_dir().join(format!("richomonitor-usage-capture-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut db = Storage::open(directory.join("monitor.sqlite3")).unwrap();
        db.save_notification_channel(
            "secret-channel-id",
            "secret-bot-name",
            "feishu",
            Some("secret-credential"),
            &[],
        )
        .unwrap();
        db.mark_notification_channel_tested("secret-channel-id")
            .unwrap();
        db.set_notification_channel_enabled("secret-channel-id", true)
            .unwrap();
        db.save_notification_routes(
            "secret-channel-id",
            &[
                NotificationTarget {
                    id: "secret-group-id".into(),
                    kind: "chat".into(),
                    label: "secret-group-name".into(),
                },
                NotificationTarget {
                    id: "secret-person-id".into(),
                    kind: "user".into(),
                    label: "secret-person-name".into(),
                },
            ],
        )
        .unwrap();
        drop(db);
        let app = MonitorApp::open(&directory).unwrap();
        let service = json!({"endpoint":endpoint,"projectToken":"public-fixture-token"});
        let worker = tokio::spawn({
            let app = app.clone();
            async move {
                app.usage_statistics_loop_to(&service, Duration::from_secs(3600))
                    .await
            }
        });
        app.usage_statistics.monitoring_started();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), received.recv())
                .await
                .is_err()
        );
        app.set_usage_statistics_enabled(true).await.unwrap();
        let event = tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event["event"], "app_active");
        assert_eq!(event["properties"]["group_targets"], 1);
        assert_eq!(event["properties"]["private_targets"], 1);
        assert_eq!(event["properties"]["discovered_group_targets"], 1);
        assert_eq!(event["properties"]["discovered_private_targets"], 1);
        assert_eq!(event["properties"]["environment"], "development");
        assert_eq!(event["properties"]["$process_person_profile"], false);
        assert!(!event.to_string().contains("secret-"));
        app.usage_statistics.monitoring_started();
        tokio::time::timeout(
            Duration::from_millis(250),
            app.set_usage_statistics_enabled(false),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), received.recv())
                .await
                .is_err()
        );
        assert!(!app.snapshot().await.unwrap().usage_statistics_enabled);
        worker.abort();
        server.abort();
        let _ = worker.await;
        let _ = server.await;
        drop(app);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
