//! 系统代理的静态解析及 Windows 原生目标解析，供 HTTP 客户端和通知宿主共用。
const STATIC_PROXY_ERROR: &str =
    "系统代理地址无效。请检查静态 HTTP 或 HTTPS 代理，或关闭应用中对应的系统代理选项。";

pub fn parse_static(enabled: bool, value: &str) -> Result<Option<String>, String> {
    parse_static_protocol(enabled, value, "https")
}

fn parse_static_protocol(
    enabled: bool,
    value: &str,
    target_protocol: &str,
) -> Result<Option<String>, String> {
    if !enabled {
        return Ok(None);
    }
    // HTTPS/WSS 使用 https 映射；未分协议的地址适用于全部协议。
    // Windows 的 https= 表示目标协议，普通静态代理仍通过 HTTP CONNECT 连接。
    let address = if value.contains('=') {
        let address = value.split(';').find_map(|entry| {
            let (protocol, address) = entry.split_once('=')?;
            protocol
                .trim()
                .eq_ignore_ascii_case(target_protocol)
                .then_some(address)
        });
        // 未为目标协议配置代理时，沿用系统的直连行为。
        let Some(address) = address else {
            return Ok(None);
        };
        address
    } else {
        value
    }
    .trim();
    if address.is_empty() || address.ends_with(':') {
        return Err(STATIC_PROXY_ERROR.into());
    }
    let address = if address.contains("://") {
        address.to_owned()
    } else {
        format!("http://{address}")
    };
    let url = reqwest::Url::parse(&address).map_err(|_| STATIC_PROXY_ERROR.to_string())?;
    if matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.port() != Some(0)
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
    {
        Ok(Some(url.to_string()))
    } else {
        Err(STATIC_PROXY_ERROR.into())
    }
}

#[cfg(target_os = "windows")]
pub mod windows {
    //! WinHTTP 按实际目标解析 PAC、WPAD 与静态绕过规则；由隔离的短命子进程调用。
    use windows::{
        core::{w, PCWSTR},
        Win32::{
            Foundation::{GlobalFree, HGLOBAL},
            Networking::WinHttp::*,
        },
    };

    pub(crate) const NATIVE_PROXY: &str = "windows-system://current";

    struct Configuration(WINHTTP_CURRENT_USER_IE_PROXY_CONFIG);
    impl Configuration {
        fn read() -> Result<Self, String> {
            let mut value = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
            match unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut value) } {
                Ok(()) => Ok(Self(value)),
                Err(error) if error.code().0 == 0x80070002u32 as i32 => Ok(Self(value)),
                Err(error) => Err(format!("无法读取 Windows 系统代理：{}", error.code())),
            }
        }
    }
    impl Drop for Configuration {
        fn drop(&mut self) {
            free(&[
                self.0.lpszProxy,
                self.0.lpszProxyBypass,
                self.0.lpszAutoConfigUrl,
            ]);
        }
    }
    fn free(values: &[windows::core::PWSTR]) {
        for value in values {
            if !value.is_null() {
                let _ = unsafe { GlobalFree(Some(HGLOBAL(value.0.cast()))) };
            }
        }
    }
    fn text(value: windows::core::PWSTR) -> String {
        if value.is_null() {
            String::new()
        } else {
            unsafe { value.to_string() }.unwrap_or_default()
        }
    }

    pub fn current() -> Result<Option<String>, String> {
        let config = Configuration::read()?;
        Ok((config.0.fAutoDetect.as_bool()
            || !config.0.lpszAutoConfigUrl.is_null()
            || !config.0.lpszProxy.is_null())
        .then(|| NATIVE_PROXY.to_owned()))
    }

    fn bypass(host: &str, list: &str) -> bool {
        list.split(';').map(str::trim).any(|pattern| {
            if pattern.eq_ignore_ascii_case("<local>") {
                return !host.contains('.');
            }
            let host = host.to_ascii_lowercase();
            let pattern = pattern.to_ascii_lowercase();
            let parts: Vec<_> = pattern.split('*').collect();
            if parts.len() == 1 {
                return host == pattern;
            }
            let mut remaining = host.as_str();
            for (index, part) in parts.iter().enumerate() {
                let Some(position) = remaining.find(part) else {
                    return false;
                };
                if index == 0 && position != 0 {
                    return false;
                }
                remaining = &remaining[position + part.len()..];
            }
            pattern.ends_with('*') || remaining.is_empty()
        })
    }

    fn static_proxy(
        config: &WINHTTP_CURRENT_USER_IE_PROXY_CONFIG,
        target: &reqwest::Url,
    ) -> Result<Option<String>, String> {
        if bypass(
            target.host_str().unwrap_or_default(),
            &text(config.lpszProxyBypass),
        ) {
            return Ok(None);
        }
        let proxy = text(config.lpszProxy);
        if proxy.is_empty() {
            Ok(None)
        } else {
            super::parse_static_protocol(true, &proxy, target.scheme())
        }
    }

    pub fn resolve(target: &str) -> Result<Option<String>, String> {
        let target = reqwest::Url::parse(target).map_err(|_| "通知目标地址无效".to_string())?;
        if !matches!(target.scheme(), "https" | "http") || target.host_str().is_none() {
            return Err("通知目标协议无效".into());
        }
        let config = Configuration::read()?;
        resolve_configuration(&target, &config.0)
    }

    fn resolve_configuration(
        target: &reqwest::Url,
        config: &WINHTTP_CURRENT_USER_IE_PROXY_CONFIG,
    ) -> Result<Option<String>, String> {
        if !config.fAutoDetect.as_bool() && config.lpszAutoConfigUrl.is_null() {
            return static_proxy(config, target);
        }
        unsafe {
            let session = WinHttpOpen(
                w!("RichoMonitor proxy resolver"),
                WINHTTP_ACCESS_TYPE_NO_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            );
            if session.is_null() {
                return Err("无法启动 Windows 自动代理解析".into());
            }
            let mut options = WINHTTP_AUTOPROXY_OPTIONS {
                fAutoLogonIfChallenged: true.into(),
                ..Default::default()
            };
            if config.fAutoDetect.as_bool() {
                options.dwFlags |= WINHTTP_AUTOPROXY_AUTO_DETECT;
                options.dwAutoDetectFlags =
                    WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A;
            }
            if !config.lpszAutoConfigUrl.is_null() {
                options.dwFlags |= WINHTTP_AUTOPROXY_CONFIG_URL;
                options.lpszAutoConfigUrl = PCWSTR(config.lpszAutoConfigUrl.0);
            }
            let wide_target: Vec<u16> = target.as_str().encode_utf16().chain(Some(0)).collect();
            let mut result = WINHTTP_PROXY_INFO::default();
            let call = WinHttpGetProxyForUrl(
                session,
                PCWSTR(wide_target.as_ptr()),
                &mut options,
                &mut result,
            );
            let _ = WinHttpCloseHandle(session);
            let proxy = text(result.lpszProxy);
            free(&[result.lpszProxy, result.lpszProxyBypass]);
            match call {
                Ok(()) if result.dwAccessType == WINHTTP_ACCESS_TYPE_NO_PROXY => Ok(None),
                Ok(()) => super::parse_static(true, proxy.split(';').next().unwrap_or_default()),
                // 自动检测开启但网络未部署 WPAD 时，沿用用户的静态配置或直连规则。
                Err(error)
                    if config.lpszAutoConfigUrl.is_null()
                        && (error.code().0 as u32 & 0xFFFF)
                            == ERROR_WINHTTP_AUTODETECTION_FAILED =>
                {
                    static_proxy(config, target)
                }
                Err(error) => Err(format!("Windows 自动代理解析失败：{}", error.code())),
            }
        }
    }

    pub fn run_helper() -> bool {
        let mut args = std::env::args();
        if args.nth(1).as_deref() != Some("--rm-resolve-system-proxy") {
            return false;
        }
        let result = args
            .next()
            .ok_or_else(|| "缺少请求目标地址".to_owned())
            .and_then(|target| resolve(&target));
        println!(
            "{}",
            match result {
                Ok(proxy) => serde_json::json!({"proxyUrl":proxy}),
                Err(error) => serde_json::json!({"error":error}),
            }
        );
        true
    }

    #[derive(Clone)]
    pub(crate) struct HttpProxy {
        enabled: bool,
        cache: std::sync::Arc<std::sync::Mutex<Cache>>,
    }

    #[derive(Default)]
    struct Cache {
        targets: std::collections::BTreeMap<String, (std::time::Instant, Option<String>)>,
        clients: std::collections::BTreeMap<String, reqwest::Client>,
    }

    impl HttpProxy {
        pub(crate) fn new(enabled: bool) -> Self {
            Self {
                enabled,
                cache: Default::default(),
            }
        }

        pub(crate) async fn client(
            &self,
            target: &str,
            direct: &reqwest::Client,
            build: impl FnOnce(&str) -> Result<reqwest::Client, String>,
            budget: std::time::Duration,
        ) -> Result<reqwest::Client, String> {
            if !self.enabled {
                return Ok(direct.clone());
            }
            let cached = self
                .cache
                .lock()
                .unwrap()
                .targets
                .get(target)
                .filter(|(at, _)| at.elapsed() < std::time::Duration::from_secs(30))
                .cloned();
            let proxy = match cached {
                Some((_, value)) => value,
                None => {
                    use std::process::Stdio;
                    let mut command = tokio::process::Command::new(
                        std::env::current_exe().map_err(|_| "无法定位系统代理解析程序")?,
                    );
                    command
                        .args(["--rm-resolve-system-proxy", target])
                        .creation_flags(0x08000000)
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .kill_on_drop(true);
                    let output = tokio::time::timeout(
                        budget.min(std::time::Duration::from_secs(10)),
                        command.output(),
                    )
                    .await
                    .map_err(|_| "Windows 系统代理解析超时")?
                    .map_err(|_| "无法启动系统代理解析程序")?;
                    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
                        .map_err(|_| "系统代理解析程序返回无效结果")?;
                    if let Some(error) = value["error"].as_str() {
                        return Err(error.to_owned());
                    }
                    let proxy = value["proxyUrl"].as_str().map(str::to_owned);
                    let mut cache = self.cache.lock().unwrap();
                    // 精确 URL 缓存有界，大范围扫描仍按实际目标解析。
                    if cache.targets.len() >= 1000 {
                        cache.targets.pop_first();
                    }
                    cache.targets.insert(
                        target.to_owned(),
                        (std::time::Instant::now(), proxy.clone()),
                    );
                    proxy
                }
            };
            let Some(proxy) = proxy else {
                return Ok(direct.clone());
            };
            let mut cache = self.cache.lock().unwrap();
            if let Some(client) = cache.clients.get(&proxy) {
                return Ok(client.clone());
            }
            let client = build(&proxy)?;
            if cache.clients.len() >= 16 {
                cache.clients.pop_first();
            }
            cache.clients.insert(proxy, client.clone());
            Ok(client)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[tokio::test]
        async fn resolved_http_proxy_is_used_and_client_is_reused() {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy = format!("http://{}", listener.local_addr().unwrap());
            let worker = tokio::spawn(async move {
                for _ in 0..2 {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = vec![0; 4096];
                    let count = socket.read(&mut request).await.unwrap();
                    assert!(String::from_utf8_lossy(&request[..count])
                        .starts_with("GET http://fixture.example.test/item"));
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .await
                        .unwrap();
                }
            });
            let target = "http://fixture.example.test/item";
            let transport = HttpProxy::new(true);
            transport
                .cache
                .lock()
                .unwrap()
                .targets
                .insert(target.into(), (std::time::Instant::now(), Some(proxy)));
            let direct = reqwest::Client::builder().no_proxy().build().unwrap();
            let created = std::sync::atomic::AtomicUsize::new(0);
            for _ in 0..2 {
                let client = transport
                    .client(
                        target,
                        &direct,
                        |proxy| {
                            created.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            reqwest::Client::builder()
                                .no_proxy()
                                .proxy(reqwest::Proxy::all(proxy).unwrap())
                                .build()
                                .map_err(|_| "构建失败".into())
                        },
                        std::time::Duration::from_secs(5),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    client
                        .get(target)
                        .send()
                        .await
                        .unwrap()
                        .text()
                        .await
                        .unwrap(),
                    "ok"
                );
            }
            assert_eq!(created.load(std::sync::atomic::Ordering::Relaxed), 1);
            worker.await.unwrap();
        }

        #[test]
        fn native_pac_routes_direct_and_proxy_by_target() {
            use std::{
                io::{Read, Write},
                net::TcpListener,
                sync::{
                    atomic::{AtomicBool, Ordering},
                    Arc,
                },
                time::{Duration, Instant},
            };
            let server = TcpListener::bind("127.0.0.1:0").unwrap();
            server.set_nonblocking(true).unwrap();
            let address = server.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let worker = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(20);
                while !stopped.load(Ordering::Relaxed) && Instant::now() < deadline {
                    if let Ok((mut socket, _)) = server.accept() {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = [0u8; 4096];
                        let _ = socket.read(&mut request);
                        let pac = "function FindProxyForURL(url, host) { if (host === 'direct.example.test') return 'DIRECT'; return 'PROXY 127.0.0.1:9999'; }";
                        let _ = write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", pac.len(), pac);
                    } else {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            });
            let mut pac: Vec<u16> = format!("http://{address}/proxy.pac")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let config = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG {
                lpszAutoConfigUrl: windows::core::PWSTR(pac.as_mut_ptr()),
                ..Default::default()
            };
            let direct = resolve_configuration(
                &reqwest::Url::parse("https://direct.example.test/path").unwrap(),
                &config,
            );
            let proxied = resolve_configuration(
                &reqwest::Url::parse("https://proxy.example.test/path").unwrap(),
                &config,
            );
            stop.store(true, Ordering::Relaxed);
            worker.join().unwrap();
            assert_eq!(direct.unwrap(), None);
            assert_eq!(proxied.unwrap().as_deref(), Some("http://127.0.0.1:9999/"));
        }

        #[test]
        fn static_bypass_matches_exact_wildcard_and_local_hosts() {
            assert!(bypass("localhost", "<local>;*.example.com"));
            assert!(bypass("api.example.com", "<local>;*.example.com"));
            assert!(bypass("API.EXAMPLE.COM", "api.example.com"));
            assert!(!bypass("example.com.attacker.test", "*.example.com"));
            assert!(!bypass("api.example.com", "other.example.com"));
        }
    }
}
