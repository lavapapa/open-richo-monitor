//! Core 持有 SDK 子进程及凭据，桌面界面仅接收连接与绑定状态。
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{oneshot, Mutex as AsyncMutex, Notify},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct NotificationTarget {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChannelBinding {
    pub id: String,
    pub provider: String,
    pub status: String,
    pub qr_url: Option<String>,
    #[serde(default)]
    pub targets: Vec<NotificationTarget>,
    pub message: Option<String>,
    pub bot_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AccountStatus {
    pub id: String,
    pub status: String,
    pub message: Option<String>,
    #[serde(default)]
    pub targets: Vec<NotificationTarget>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SendResult {
    pub outcome: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl SendResult {
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            outcome: "failed".into(),
            message: message.into(),
            retryable: false,
        }
    }
    fn unknown() -> Self {
        Self {
            outcome: "unknown".into(),
            message: "通知连接中断，接收结果未知".into(),
            retryable: false,
        }
    }
}

struct Process {
    child: Child,
    stdin: Option<ChildStdin>,
    reader: tokio::task::JoinHandle<()>,
    stdout_closed: Arc<AtomicBool>,
}

type Pending = Arc<Mutex<BTreeMap<u64, oneshot::Sender<Result<Value, String>>>>>;
type Reply = oneshot::Receiver<Result<Value, String>>;

#[derive(Default)]
struct AccountCancellation {
    generation: u64,
}

#[derive(Default)]
struct State {
    process: Option<Process>,
    next_id: u64,
    configuration: Option<Value>,
    failures: u8,
    retry_at: Option<Instant>,
    stopped: bool,
}

#[derive(Clone, Default)]
pub struct NotificationRuntime {
    path: Arc<Mutex<Option<PathBuf>>>,
    arguments: Arc<Mutex<Vec<String>>>,
    state: Arc<AsyncMutex<State>>,
    pending: Pending,
    accounts: Arc<Mutex<BTreeMap<String, AccountStatus>>>,
    status_changed: Arc<AtomicBool>,
    bindings: Arc<Mutex<BTreeMap<String, Value>>>,
    credential_updates: Arc<Mutex<BTreeMap<String, Value>>>,
    account_cancellations: Arc<Mutex<BTreeMap<String, AccountCancellation>>>,
    closing: Arc<AtomicBool>,
    shutdown_signal: Arc<Notify>,
}

impl NotificationRuntime {
    pub fn set_path(&self, path: impl AsRef<Path>) {
        *self.path.lock().unwrap() = Some(path.as_ref().to_path_buf());
    }

    pub fn set_arguments(&self, arguments: Vec<String>) {
        *self.arguments.lock().unwrap() = arguments;
    }

    pub fn account_status(&self, id: &str) -> Option<AccountStatus> {
        self.accounts.lock().unwrap().get(id).cloned()
    }

    #[cfg(test)]
    pub async fn is_running(&self) -> bool {
        self.state.lock().await.process.is_some()
    }

    pub fn take_status_changed(&self) -> bool {
        self.status_changed.swap(false, Ordering::Relaxed)
    }

    pub fn binding(&self, id: &str) -> Option<Value> {
        self.bindings.lock().unwrap().get(id).cloned()
    }

    pub fn forget_binding(&self, id: &str) {
        self.bindings.lock().unwrap().remove(id);
    }

    pub fn cancel_pending(&self) {
        self.closing.store(true, Ordering::Relaxed);
        self.shutdown_signal.notify_waiters();
    }

    pub fn take_credential_updates(&self) -> BTreeMap<String, Value> {
        std::mem::take(&mut *self.credential_updates.lock().unwrap())
    }

    pub fn account_generation(&self, account_id: &str) -> u64 {
        self.account_cancellations
            .lock()
            .unwrap()
            .entry(account_id.to_owned())
            .or_default()
            .generation
    }

    pub fn cancel_account(&self, account_id: &str) -> u64 {
        let mut accounts = self.account_cancellations.lock().unwrap();
        let account = accounts.entry(account_id.to_owned()).or_default();
        account.generation = account.generation.wrapping_add(1);
        account.generation
    }

    async fn start(&self, state: &mut State) -> Result<(), String> {
        if state.stopped || self.closing.load(Ordering::Relaxed) {
            return Err("通知服务已关闭".into());
        }
        if let Some(process) = state.process.as_mut() {
            let exited = process
                .child
                .try_wait()
                .map_err(|_| "无法检查通知服务进程")?
                .is_some();
            if !exited && !process.stdout_closed.load(Ordering::Acquire) {
                return Ok(());
            }
            if !exited {
                let _ = process.child.kill().await;
                let _ = process.child.wait().await;
            }
            process.reader.abort();
            state.process = None;
            state.failures += 1;
            state.retry_at = Some(Instant::now() + Duration::from_secs(2));
            self.mark_disconnected(state.failures >= 3);
        }
        if state.failures >= 3 {
            return Err("通知服务连续退出，请重新启动应用".into());
        }
        if state.retry_at.is_some_and(|at| Instant::now() < at) {
            return Err("通知服务正在重新连接".into());
        }
        let path = self
            .path
            .lock()
            .unwrap()
            .clone()
            .ok_or("通知服务运行文件尚未配置")?;
        let mut command = Command::new(&path);
        #[cfg(test)]
        if path.extension().is_some_and(|extension| extension == "mjs") {
            command = Command::new("node");
            command.arg(&path);
        }
        #[cfg(target_os = "windows")]
        {
            // CREATE_NO_WINDOW：通知模块通过管道通信，无需控制台窗口。
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .args(self.arguments.lock().unwrap().clone())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "无法启动通知服务，请检查运行文件")?;
        let stdin = child.stdin.take().ok_or("通知服务输入管道不可用")?;
        let stdout = child.stdout.take().ok_or("通知服务输出管道不可用")?;
        let pending = self.pending.clone();
        let accounts = self.accounts.clone();
        let status_changed = self.status_changed.clone();
        let bindings = self.bindings.clone();
        let credential_updates = self.credential_updates.clone();
        let stdout_closed = Arc::new(AtomicBool::new(false));
        let reader_closed = stdout_closed.clone();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    if let Some(sender) = pending.lock().unwrap().remove(&id) {
                        // 平台报错可能包含凭据；公共诊断使用固定文案。
                        let result = if value.get("error").is_some() {
                            Err("通知服务无法完成请求，请检查账户授权与连接状态".into())
                        } else {
                            Ok(value.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(result);
                    }
                } else {
                    receive_event(
                        &value,
                        &accounts,
                        &bindings,
                        &credential_updates,
                        &status_changed,
                    );
                }
            }
            // EOF 可先于进程退出，先标记管道失效再唤醒调用方。
            reader_closed.store(true, Ordering::Release);
            for (_, sender) in std::mem::take(&mut *pending.lock().unwrap()) {
                let _ = sender.send(Err("通知服务连接中断".into()));
            }
            for status in accounts.lock().unwrap().values_mut() {
                status.status = "reconnecting".into();
                status.message = Some("通知服务连接中断".into());
            }
            status_changed.store(true, Ordering::Relaxed);
            for binding in bindings.lock().unwrap().values_mut() {
                if matches!(binding["status"].as_str(), Some("waiting" | "scanned")) {
                    binding["status"] = json!("failed");
                    binding["message"] = json!("通知服务连接中断，请重新开始绑定");
                }
            }
        });
        state.process = Some(Process {
            child,
            stdin: Some(stdin),
            reader,
            stdout_closed,
        });
        // 子进程重启后仅恢复账户连接；已发出的消息从不重放。
        if let Some(config) = state.configuration.clone() {
            self.exchange(state, "configure", config).await?;
        }
        Ok(())
    }

    fn mark_disconnected(&self, failed: bool) {
        self.status_changed.store(true, Ordering::Relaxed);
        for status in self.accounts.lock().unwrap().values_mut() {
            status.status = if failed { "failed" } else { "reconnecting" }.into();
            status.message = Some(
                if failed {
                    "通知服务连续退出，请重新启动应用"
                } else {
                    "通知服务正在重新连接"
                }
                .into(),
            );
        }
    }

    async fn exchange(
        &self,
        state: &mut State,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let (id, receiver) = self.submit(state, method, params).await?;
        self.receive_reply(id, receiver).await
    }

    async fn submit(
        &self,
        state: &mut State,
        method: &str,
        params: Value,
    ) -> Result<(u64, Reply), String> {
        state.next_id += 1;
        let id = state.next_id;
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, sender);
        let mut line = serde_json::to_vec(&json!({"id":id,"method":method,"params":params}))
            .map_err(|_| "通知请求格式无效")?;
        line.push(b'\n');
        let process = state.process.as_mut().ok_or("通知服务未启动")?;
        if process.stdout_closed.load(Ordering::Acquire) {
            self.pending.lock().unwrap().remove(&id);
            return Err("通知服务连接中断".into());
        }
        let write = process
            .stdin
            .as_mut()
            .ok_or("通知服务已关闭")?
            .write_all(&line)
            .await;
        if write.is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err("通知服务连接中断".into());
        }
        Ok((id, receiver))
    }

    async fn receive_reply(&self, id: u64, receiver: Reply) -> Result<Value, String> {
        let cancelled = self.shutdown_signal.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.closing.load(Ordering::Relaxed) {
            self.pending.lock().unwrap().remove(&id);
            return Err("通知服务已关闭".into());
        }
        let result = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(20), receiver) => result,
            _ = &mut cancelled => {
                self.pending.lock().unwrap().remove(&id);
                return Err("通知服务已关闭".into());
            }
        };
        self.pending.lock().unwrap().remove(&id);
        match result {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("通知服务连接中断".into()),
            Err(_) => Err("通知服务响应超时".into()),
        }
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut state = self.state.lock().await;
        self.start(&mut state).await?;
        let (id, receiver) = self.submit(&mut state, method, params).await?;
        drop(state);
        self.receive_reply(id, receiver).await
    }

    pub async fn configure(&self, configuration: Value, force: bool) -> Result<(), String> {
        let mut state = self.state.lock().await;
        let needed = force
            || configuration["accounts"]
                .as_array()
                .is_some_and(|accounts| accounts.iter().any(|a| a["enabled"] == true))
            || self.bindings.lock().unwrap().values().any(|b| {
                matches!(
                    b["status"].as_str(),
                    Some("waiting" | "scanned" | "complete")
                )
            });
        if !needed {
            state.configuration = Some(configuration);
            self.stop_process(&mut state).await;
            return Ok(());
        }
        let changed = state.configuration.as_ref() != Some(&configuration);
        if let Some(accounts) = configuration["accounts"].as_array() {
            let mut statuses = self.accounts.lock().unwrap();
            statuses.retain(|id, _| {
                accounts
                    .iter()
                    .any(|account| account["id"].as_str() == Some(id.as_str()))
            });
            for account in accounts {
                if let Some(id) = account["id"].as_str() {
                    let status = statuses
                        .entry(id.to_owned())
                        .or_insert_with(|| AccountStatus {
                            id: id.to_owned(),
                            status: "connecting".into(),
                            message: None,
                            targets: Vec::new(),
                        });
                    if account["enabled"] != true {
                        status.status = "stopped".into();
                    }
                }
            }
        }
        if let Err(message) = self.start(&mut state).await {
            self.status_changed.store(true, Ordering::Relaxed);
            for status in self
                .accounts
                .lock()
                .unwrap()
                .values_mut()
                .filter(|status| status.status != "stopped")
            {
                status.status = if state.retry_at.is_some() && state.failures < 3 {
                    "reconnecting"
                } else {
                    "failed"
                }
                .into();
                status.message = Some(message.clone());
            }
            return Err(message);
        }
        if changed {
            self.exchange(&mut state, "configure", configuration.clone())
                .await?;
            state.configuration = Some(configuration);
        }
        Ok(())
    }

    pub async fn send(
        &self,
        account_id: &str,
        target: &NotificationTarget,
        text: &str,
    ) -> SendResult {
        let generation = self.account_generation(account_id);
        self.send_at_generation(account_id, target, text, generation, || Ok(true))
            .await
    }

    pub async fn send_at_generation(
        &self,
        account_id: &str,
        target: &NotificationTarget,
        text: &str,
        generation: u64,
        can_send: impl FnOnce() -> Result<bool, String> + Send,
    ) -> SendResult {
        if text.trim().is_empty() {
            return SendResult::failed("通知正文为空");
        }
        let mut state = self.state.lock().await;
        if let Err(message) = self.start(&mut state).await {
            return SendResult::failed(message);
        }
        match can_send() {
            Ok(true) => {}
            Err(error) => return SendResult::failed(error),
            Ok(false) => {
                return SendResult {
                    outcome: "skipped".into(),
                    message: "商品监控轮次已结束".into(),
                    retryable: false,
                }
            }
        }
        if self.account_generation(account_id) != generation {
            return SendResult::failed("渠道已停用");
        }
        if self.closing.load(Ordering::Relaxed) {
            return SendResult::failed("通知服务已关闭");
        }
        let submitted = self.submit(&mut state, "send", json!({"accountId":account_id,"target":{"id":target.id,"kind":target.kind},"text":text})).await;
        // 管道写入有序；平台回执可并行等待，慢渠道不占用进程状态锁。
        drop(state);
        let result = match submitted {
            Ok((id, receiver)) => self.receive_reply(id, receiver).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(value) => serde_json::from_value::<SendResult>(value)
                .ok()
                .filter(|result| {
                    matches!(result.outcome.as_str(), "accepted" | "failed" | "unknown")
                })
                .unwrap_or_else(SendResult::unknown),
            Err(message) if message == "通知服务无法完成请求，请检查账户授权与连接状态" => {
                SendResult::failed(message)
            }
            Err(_) => SendResult::unknown(),
        }
    }

    async fn stop_process(&self, state: &mut State) {
        let Some(mut process) = state.process.take() else {
            return;
        };
        // EOF 是 SDK 的关闭信号；超时后杀死并回收子进程。
        drop(process.stdin.take());
        if tokio::time::timeout(Duration::from_secs(3), process.child.wait())
            .await
            .is_err()
        {
            let _ = process.child.kill().await;
            let _ = process.child.wait().await;
        }
        process.reader.abort();
        let _ = process.reader.await;
        for status in self.accounts.lock().unwrap().values_mut() {
            status.status = "stopped".into();
        }
        self.status_changed.store(true, Ordering::Relaxed);
    }

    pub async fn shutdown(&self) {
        self.cancel_pending();
        let mut state = self.state.lock().await;
        state.stopped = true;
        self.stop_process(&mut state).await;
    }

    pub fn remember_binding(&self, value: Value) {
        receive_event(
            &json!({"event":"binding","data":value}),
            &self.accounts,
            &self.bindings,
            &self.credential_updates,
            &self.status_changed,
        );
    }
}

fn receive_event(
    value: &Value,
    accounts: &Mutex<BTreeMap<String, AccountStatus>>,
    bindings: &Mutex<BTreeMap<String, Value>>,
    credential_updates: &Mutex<BTreeMap<String, Value>>,
    status_changed: &AtomicBool,
) {
    let Some(data) = value.get("data") else {
        return;
    };
    match value["event"].as_str() {
        Some("account") => {
            if let Ok(mut status) = serde_json::from_value::<AccountStatus>(data.clone()) {
                status.message = status.message.map(|_| "请检查账户授权与连接状态".into());
                let mut accounts = accounts.lock().unwrap();
                if accounts.get(&status.id) != Some(&status) {
                    accounts.insert(status.id.clone(), status);
                    status_changed.store(true, Ordering::Relaxed);
                }
            }
        }
        Some("binding") => {
            if let Some(id) = data["id"].as_str() {
                let mut bindings = bindings.lock().unwrap();
                let mut binding = data.clone();
                if let Some(credentials) = bindings.get(id).and_then(|old| old.get("credentials")) {
                    if binding.get("credentials").is_none() {
                        binding["credentials"] = credentials.clone();
                    } else {
                        merge_credentials(&mut binding["credentials"], credentials);
                    }
                }
                bindings.insert(id.to_owned(), binding);
            }
        }
        Some("credentials") => {
            if let (Some(id), Some(credentials)) = (
                data["accountId"].as_str(),
                data.get("credentials").filter(|value| value.is_object()),
            ) {
                if let Some(binding) = bindings.lock().unwrap().get_mut(id) {
                    if !binding["credentials"].is_object() {
                        binding["credentials"] = json!({});
                    }
                    merge_credentials(&mut binding["credentials"], credentials);
                }
                let mut updates = credential_updates.lock().unwrap();
                merge_credentials(
                    updates.entry(id.to_owned()).or_insert_with(|| json!({})),
                    credentials,
                );
            }
        }
        Some("target") => {
            if let (Some(id), Ok(target)) = (
                data["accountId"].as_str(),
                serde_json::from_value::<NotificationTarget>(data["target"].clone()),
            ) {
                if let Some(account) = accounts.lock().unwrap().get_mut(id) {
                    account
                        .targets
                        .retain(|old| old.id != target.id || old.kind != target.kind);
                    account.targets.push(target);
                    status_changed.store(true, Ordering::Relaxed);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn merge_credentials(current: &mut Value, update: &Value) {
    if let (Some(current), Some(update)) = (current.as_object_mut(), update.as_object()) {
        for (key, value) in update {
            if value.is_object() {
                merge_credentials(
                    current.entry(key.clone()).or_insert_with(|| json!({})),
                    value,
                );
            } else {
                current.insert(key.clone(), value.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> NotificationRuntime {
        let runtime = NotificationRuntime::default();
        runtime.set_path(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notification_runtime.mjs"),
        );
        runtime
    }

    fn configuration(credentials: Value) -> Value {
        json!({"accounts":[{"id":"a","provider":"feishu","credentials":credentials,"targets":[{"id":"chat-a","kind":"chat","label":"A"}],"enabled":true}],"network":"direct"})
    }

    #[tokio::test]
    async fn script_fixture_runs_through_node_with_a_chinese_and_spaced_path() {
        let directory = std::env::temp_dir().join(format!(
            "ricoh-notification-fixture 中文-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("通知 fixture.mjs");
        std::fs::write(
            &path,
            include_str!("../tests/fixtures/notification_runtime.mjs"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let runtime = NotificationRuntime::default();
        runtime.set_path("node");
        runtime.set_arguments(vec![path.to_string_lossy().into_owned()]);
        let result = runtime.request("status", json!({})).await;
        runtime.shutdown().await;
        std::fs::remove_dir_all(directory).unwrap();
        assert!(result.unwrap()["pid"].as_u64().is_some());
    }

    #[tokio::test]
    async fn disabled_accounts_do_not_start_a_process_and_binding_keeps_it_alive() {
        let runtime = fixture();
        let empty = json!({"accounts":[],"network":"direct"});
        runtime.configure(empty.clone(), false).await.unwrap();
        assert!(runtime.state.lock().await.process.is_none());
        runtime
            .request(
                "begin_binding",
                json!({"bindingId":"binding-a","provider":"feishu"}),
            )
            .await
            .unwrap();
        let pid = runtime
            .state
            .lock()
            .await
            .process
            .as_ref()
            .unwrap()
            .child
            .id();
        runtime.configure(empty.clone(), false).await.unwrap();
        assert_eq!(
            runtime
                .state
                .lock()
                .await
                .process
                .as_ref()
                .unwrap()
                .child
                .id(),
            pid
        );
        runtime.forget_binding("binding-a");
        runtime.configure(empty, false).await.unwrap();
        assert!(runtime.state.lock().await.process.is_none());
    }

    #[tokio::test]
    async fn shutdown_cancels_pending_requests_and_reaps_a_hung_child() {
        let runtime = fixture();
        runtime.set_path("node");
        runtime.set_arguments(vec![Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/notification_runtime.mjs")
            .to_string_lossy()
            .into_owned()]);
        runtime
            .configure(configuration(json!({"ignoreEof":true})), false)
            .await
            .unwrap();
        let hanging = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.request("hang", json!({})).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while runtime.pending.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let started = Instant::now();
        runtime.shutdown().await;
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(hanging.await.unwrap().is_err());
        assert!(runtime.state.lock().await.process.is_none());
        assert!(runtime.request("status", json!({})).await.is_err());
    }

    #[tokio::test]
    async fn crash_marks_send_unknown_and_restores_accounts_without_replaying_messages() {
        let runtime = fixture();
        // 确定复现 stdout 已关闭、操作系统仍报告进程存活的时序。
        let directory =
            std::env::temp_dir().join(format!("ricoh-notification-crash-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("launcher.rs");
        let launcher = directory.join(format!("launcher{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(
            &source,
            r#"fn main() {
    let fixture = std::env::args_os().nth(1).unwrap();
    assert!(std::process::Command::new("node").arg(fixture).status().unwrap().success());
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        drop(std::fs::File::from_raw_handle(std::io::stdout().as_raw_handle()));
    }
    #[cfg(unix)]
    unsafe {
        use std::os::fd::{AsRawFd, FromRawFd};
        drop(std::fs::File::from_raw_fd(std::io::stdout().as_raw_fd()));
    }
    loop { std::thread::park(); }
}"#,
        )
        .unwrap();
        let compiled = std::process::Command::new("rustc")
            .args([
                "--edition=2021",
                "--crate-name=notification_fixture_launcher",
            ])
            .arg(&source)
            .arg("-o")
            .arg(&launcher)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "测试 launcher 编译失败：{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        runtime.set_path(&launcher);
        runtime.set_arguments(vec![Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/notification_runtime.mjs")
            .to_string_lossy()
            .into_owned()]);
        runtime
            .configure(configuration(json!({"dropConnection":true})), false)
            .await
            .unwrap();
        let target = NotificationTarget {
            id: "chat-a".into(),
            kind: "chat".into(),
            label: "A".into(),
        };
        let result = runtime.send("a", &target, "message").await;
        assert_eq!(result.outcome, "unknown");
        assert!(!result.retryable);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if runtime
                    .state
                    .lock()
                    .await
                    .process
                    .as_ref()
                    .unwrap()
                    .reader
                    .is_finished()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(runtime
            .state
            .lock()
            .await
            .process
            .as_mut()
            .unwrap()
            .child
            .try_wait()
            .unwrap()
            .is_none());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), runtime.request("status", json!({})))
                .await
                .expect("读取管道关闭后应立即进入重连")
                .unwrap_err(),
            "通知服务正在重新连接"
        );
        assert_eq!(runtime.account_status("a").unwrap().status, "reconnecting");
        let status = tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                match runtime.request("status", json!({})).await {
                    Ok(status) => break status,
                    Err(message) => {
                        assert_eq!(message, "通知服务正在重新连接");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        })
        .await
        .expect("通知账户应在有界时间内恢复连接");
        assert_eq!(status["configured"], 1);
        assert_eq!(status["sendCounts"], json!({}));
        runtime.shutdown().await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn rpc_errors_do_not_expose_credentials_and_context_updates_preserve_all_targets() {
        let runtime = fixture();
        let message = runtime.request("reject", json!({})).await.unwrap_err();
        assert!(!message.contains("private-bound-secret"));
        let mut credentials = json!({"botToken":"private","contextTokens":{"a":"old"}});
        merge_credentials(&mut credentials, &json!({"contextTokens":{"b":"new"}}));
        assert_eq!(credentials["contextTokens"], json!({"a":"old","b":"new"}));
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn repeated_sends_to_one_recipient_have_no_artificial_spacing() {
        let runtime = fixture();
        runtime
            .configure(configuration(json!({})), false)
            .await
            .unwrap();
        let target = NotificationTarget {
            id: "chat-a".into(),
            kind: "chat".into(),
            label: "A".into(),
        };
        assert_eq!(
            runtime.send("a", &target, "first").await.outcome,
            "accepted"
        );
        let started = Instant::now();
        assert_eq!(
            runtime.send("a", &target, "second").await.outcome,
            "accepted"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "发送被人为延迟：{:?}",
            started.elapsed()
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn cancelled_account_generation_is_rejected_before_ipc() {
        let runtime = fixture();
        runtime
            .configure(configuration(json!({})), false)
            .await
            .unwrap();
        let target = NotificationTarget {
            id: "chat-a".into(),
            kind: "chat".into(),
            label: "A".into(),
        };
        assert_eq!(
            runtime.send("a", &target, "first").await.outcome,
            "accepted"
        );
        let generation = runtime.account_generation("a");
        runtime.cancel_account("a");
        let cancelled = runtime
            .send_at_generation("a", &target, "second", generation, || Ok(true))
            .await;
        assert_eq!(cancelled.outcome, "failed");
        let status = runtime.request("status", json!({})).await.unwrap();
        assert_eq!(status["sendCounts"]["a"], 1);
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn slow_recipient_does_not_block_another_account_or_status_request() {
        let runtime = fixture();
        let mut config = configuration(json!({"delayMs":600}));
        let mut fast = config["accounts"][0].clone();
        fast["id"] = json!("fast");
        fast["credentials"] = json!({});
        config["accounts"].as_array_mut().unwrap().push(fast);
        runtime.configure(config, false).await.unwrap();
        let target = NotificationTarget {
            id: "chat-a".into(),
            kind: "chat".into(),
            label: "A".into(),
        };
        let slow = tokio::spawn({
            let runtime = runtime.clone();
            let target = target.clone();
            async move { runtime.send("a", &target, "slow").await }
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        let fast = tokio::time::timeout(
            Duration::from_millis(300),
            runtime.send("fast", &target, "fast"),
        )
        .await
        .expect("慢渠道阻塞其他渠道");
        assert_eq!(fast.outcome, "accepted");
        let status = runtime.request("status", json!({})).await.unwrap();
        assert_eq!(status["sendCounts"]["a"], 1);
        assert!(!slow.is_finished());
        assert_eq!(slow.await.unwrap().outcome, "accepted");
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn stock_generation_is_rechecked_after_waiting_for_process_state() {
        let runtime = fixture();
        runtime
            .configure(configuration(json!({})), false)
            .await
            .unwrap();
        let state = runtime.state.lock().await;
        let current = Arc::new(AtomicBool::new(true));
        let sending = tokio::spawn({
            let runtime = runtime.clone();
            let current = current.clone();
            async move {
                let target = NotificationTarget {
                    id: "chat-a".into(),
                    kind: "chat".into(),
                    label: "A".into(),
                };
                runtime
                    .send_at_generation("a", &target, "旧轮次", 0, || {
                        Ok(current.load(Ordering::Relaxed))
                    })
                    .await
            }
        });
        tokio::task::yield_now().await;
        current.store(false, Ordering::Relaxed);
        drop(state);
        assert_eq!(sending.await.unwrap().outcome, "skipped");
        assert_eq!(
            runtime.request("status", json!({})).await.unwrap()["sendCounts"],
            json!({})
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn admission_check_that_stops_the_account_or_app_does_not_submit() {
        for shutdown in [false, true] {
            let runtime = fixture();
            runtime
                .configure(configuration(json!({})), false)
                .await
                .unwrap();
            let target = NotificationTarget {
                id: "chat-a".into(),
                kind: "chat".into(),
                label: "A".into(),
            };
            let result = runtime
                .send_at_generation("a", &target, "旧请求", 0, || {
                    if shutdown {
                        runtime.cancel_pending();
                    } else {
                        runtime.cancel_account("a");
                    }
                    Ok(true)
                })
                .await;
            assert_eq!(result.outcome, "failed");
            if !shutdown {
                assert_eq!(
                    runtime.request("status", json!({})).await.unwrap()["sendCounts"],
                    json!({})
                );
            }
            runtime.shutdown().await;
        }
    }
}
