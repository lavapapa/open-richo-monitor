#[cfg(target_os = "macos")]
pub(crate) fn current() -> Result<Option<String>, String> {
    let output = std::process::Command::new("/usr/sbin/scutil")
        .arg("--proxy")
        .output()
        .map_err(|_| "无法读取系统代理，请检查系统网络代理设置后重试。".to_string())?;
    if !output.status.success() {
        return Err("无法读取系统代理，请检查系统网络代理设置后重试。".into());
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "windows")]
pub(crate) fn current() -> Result<Option<String>, String> {
    ricoh_monitor_core::system_proxy::windows::current()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn current() -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
const AUTOMATIC_PROXY_ERROR: &str = "系统代理使用 PAC 或自动发现，当前通知连接暂不支持。请在其他设置关闭通知系统代理，或使用静态 HTTPS 代理。";
#[cfg(any(target_os = "macos", target_os = "windows", test))]
const STATIC_PROXY_ERROR: &str =
    "系统代理地址无法用于通知。请检查静态 HTTPS 代理，或在其他设置关闭通知系统代理。";

fn parse_windows(enabled: bool, value: &str) -> Result<Option<String>, String> {
    ricoh_monitor_core::system_proxy::parse_static(enabled, value)
}

#[cfg(any(target_os = "macos", test))]
fn parse(value: &str) -> Result<Option<String>, String> {
    let settings = value
        .lines()
        .filter_map(|line| line.trim().split_once(" : "))
        .collect::<std::collections::BTreeMap<_, _>>();
    if settings.get("ProxyAutoConfigEnable") == Some(&"1")
        || settings.get("ProxyAutoDiscoveryEnable") == Some(&"1")
    {
        return Err(AUTOMATIC_PROXY_ERROR.into());
    }
    // 通知平台使用 HTTPS/WSS，复用系统的 HTTPS 代理地址。
    if settings.get("HTTPSEnable") != Some(&"1") {
        return if settings.get("SOCKSEnable") == Some(&"1") {
            Err(STATIC_PROXY_ERROR.into())
        } else {
            Ok(None)
        };
    }
    let host = settings
        .get("HTTPSProxy")
        .ok_or_else(|| STATIC_PROXY_ERROR.to_string())?;
    let port = settings
        .get("HTTPSPort")
        .ok_or_else(|| STATIC_PROXY_ERROR.to_string())?
        .parse::<u16>()
        .map_err(|_| STATIC_PROXY_ERROR.to_string())?;
    let host = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    parse_windows(true, &format!("http://{host}:{port}"))
        .map_err(|_| STATIC_PROXY_ERROR.to_string())
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_native_configuration_is_resolved_per_target() {
        let result = super::current().unwrap();
        assert!(result.is_none() || result.as_deref() == Some("windows-system://current"));
    }

    #[test]
    fn windows_static_proxy_uses_https_mapping_or_shared_address() {
        for (settings, expected) in [
            ("127.0.0.1:7890", "http://127.0.0.1:7890/"),
            ("proxy.example", "http://proxy.example/"),
            (
                "http=127.0.0.1:8080;https=127.0.0.1:8443",
                "http://127.0.0.1:8443/",
            ),
            (
                " HTTPS = proxy.example:7890 ; http=other.example:8080 ",
                "http://proxy.example:7890/",
            ),
            (
                "https=https://proxy.example:8443",
                "https://proxy.example:8443/",
            ),
            ("[::1]:7890", "http://[::1]:7890/"),
            ("https=[2001:db8::1]:7890", "http://[2001:db8::1]:7890/"),
        ] {
            assert_eq!(
                super::parse_windows(true, settings).unwrap().as_deref(),
                Some(expected)
            );
            assert_eq!(super::parse_windows(false, settings), Ok(None));
        }
    }

    #[test]
    fn windows_static_proxy_without_https_mapping_is_direct() {
        assert_eq!(
            super::parse_windows(true, "http=proxy.example:8080"),
            Ok(None)
        );
        assert_eq!(
            super::parse_windows(true, "socks=proxy.example:1080"),
            Ok(None)
        );
    }

    #[test]
    fn windows_static_proxy_rejects_invalid_addresses() {
        for settings in [
            "",
            " ",
            "https=",
            "https=proxy.example:0",
            "https=proxy.example:65536",
            "https=proxy.example:abc",
            "https=proxy.example:",
            "https=::1:7890",
            "https=socks5://proxy.example:1080",
            "https=http:///",
        ] {
            assert!(super::parse_windows(true, settings).is_err(), "{settings}");
            assert_eq!(super::parse_windows(false, settings), Ok(None));
        }
    }

    #[test]
    fn https_system_proxy_is_resolved_and_disabled_setting_is_direct() {
        assert_eq!(
            super::parse("HTTPSEnable : 1\nHTTPSProxy : 127.0.0.1\nHTTPSPort : 7890"),
            Ok(Some("http://127.0.0.1:7890/".into()))
        );
        assert_eq!(
            super::parse("HTTPSEnable : 0\nHTTPSProxy : 127.0.0.1\nHTTPSPort : 7890"),
            Ok(None)
        );
        assert_eq!(super::parse("HTTPEnable : 1\nHTTPSEnable : 0"), Ok(None));
        assert_eq!(
            super::parse("HTTPSEnable : 1\nHTTPSProxy : ::1\nHTTPSPort : 7890"),
            Ok(Some("http://[::1]:7890/".into()))
        );
    }

    #[test]
    fn macos_unsupported_and_invalid_proxy_settings_report_actionable_errors() {
        for settings in [
            "ProxyAutoConfigEnable : 1",
            "ProxyAutoDiscoveryEnable : 1",
            "SOCKSEnable : 1",
            "HTTPSEnable : 1",
            "HTTPSEnable : 1\nHTTPSProxy : \nHTTPSPort : 7890",
            "HTTPSEnable : 1\nHTTPSProxy : 127.0.0.1\nHTTPSPort : 0",
        ] {
            let error = super::parse(settings).unwrap_err();
            assert!(error.contains("其他设置关闭通知系统代理"));
        }
        assert_eq!(
            super::parse(
                "ProxyAutoConfigEnable : 0\nProxyAutoDiscoveryEnable : 0\nHTTPSEnable : 0"
            ),
            Ok(None)
        );
    }
}
