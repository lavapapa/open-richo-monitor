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
    use windows::Win32::{
        Foundation::{GlobalFree, HGLOBAL},
        Networking::WinHttp::{
            WinHttpGetIEProxyConfigForCurrentUser, WINHTTP_CURRENT_USER_IE_PROXY_CONFIG,
        },
    };
    let mut configuration = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
    let result = match unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut configuration) } {
        Ok(()) => {
            let proxy = if configuration.lpszProxy.is_null() {
                Ok(None)
            } else {
                unsafe { configuration.lpszProxy.to_string() }
                    .map(Some)
                    .map_err(|_| {
                        "无法读取 Windows 系统代理，请检查网络代理设置后重试。".to_string()
                    })
            };
            proxy.and_then(|proxy| {
                parse_windows_configuration(
                    configuration.fAutoDetect.as_bool(),
                    !configuration.lpszAutoConfigUrl.is_null(),
                    proxy.as_deref(),
                )
            })
        }
        Err(error) if error.code().0 == 0x80070002u32 as i32 => Ok(None),
        Err(_) => Err("无法读取 Windows 系统代理，请检查网络代理设置后重试。".into()),
    };
    // WinHTTP 为这三个字符串分配内存；复制所需字段后统一释放。
    for value in [
        configuration.lpszAutoConfigUrl,
        configuration.lpszProxy,
        configuration.lpszProxyBypass,
    ] {
        if !value.is_null() {
            let _ = unsafe { GlobalFree(Some(HGLOBAL(value.0.cast()))) };
        }
    }
    result
}

#[cfg(any(target_os = "windows", test))]
fn parse_windows_configuration(
    auto_detect: bool,
    pac_present: bool,
    proxy: Option<&str>,
) -> Result<Option<String>, String> {
    if auto_detect || pac_present {
        Err(AUTOMATIC_PROXY_ERROR.into())
    } else if let Some(proxy) = proxy {
        parse_windows(true, proxy)
    } else {
        Ok(None)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn current() -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
const AUTOMATIC_PROXY_ERROR: &str = "系统代理使用 PAC 或自动发现。请在系统设置改用静态 HTTP 或 HTTPS 代理，或在应用的代理池页面关闭系统代理。";
#[cfg(any(target_os = "macos", target_os = "windows", test))]
const STATIC_PROXY_ERROR: &str = "系统代理地址无法用于通知。请配置有效的静态 HTTP 或 HTTPS 代理，或在应用的代理池页面关闭系统代理。";

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn parse_windows(enabled: bool, value: &str) -> Result<Option<String>, String> {
    if !enabled {
        return Ok(None);
    }
    // HTTPS/WSS 使用 https 映射；未分协议的地址适用于全部协议。
    // Windows 的 https= 表示目标协议，普通静态代理仍通过 HTTP CONNECT 连接。
    let address = if value.contains('=') {
        value
            .split(';')
            .find_map(|entry| {
                let (protocol, address) = entry.split_once('=')?;
                protocol
                    .trim()
                    .eq_ignore_ascii_case("https")
                    .then_some(address)
            })
            .ok_or_else(|| STATIC_PROXY_ERROR.to_string())?
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
    let url = url::Url::parse(&address).map_err(|_| STATIC_PROXY_ERROR.to_string())?;
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
        return if settings.get("SOCKSEnable") == Some(&"1")
            || settings.get("HTTPEnable") == Some(&"1")
        {
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
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_native_proxy_modes_distinguish_automatic_static_and_direct() {
        for (auto_detect, pac_present) in [(true, false), (false, true), (true, true)] {
            for proxy in [None, Some("127.0.0.1:7890")] {
                assert_eq!(
                    super::parse_windows_configuration(auto_detect, pac_present, proxy),
                    Err(super::AUTOMATIC_PROXY_ERROR.into())
                );
            }
        }
        assert_eq!(
            super::parse_windows_configuration(false, false, None),
            Ok(None)
        );
        assert_eq!(
            super::parse_windows_configuration(false, false, Some("https=127.0.0.1:7890")),
            Ok(Some("http://127.0.0.1:7890/".into()))
        );
        assert!(super::parse_windows_configuration(false, false, Some("")).is_err());
        assert!(
            super::parse_windows_configuration(false, false, Some("socks=127.0.0.1:1080")).is_err()
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_current_reports_native_automatic_proxy_configuration() {
        use windows::Win32::{
            Foundation::{GlobalFree, HGLOBAL},
            Networking::WinHttp::{
                WinHttpGetIEProxyConfigForCurrentUser, WINHTTP_CURRENT_USER_IE_PROXY_CONFIG,
            },
        };
        let mut configuration = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
        unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut configuration) }.unwrap();
        let automatic =
            configuration.fAutoDetect.as_bool() || !configuration.lpszAutoConfigUrl.is_null();
        let static_proxy_present = !configuration.lpszProxy.is_null();
        for value in [
            configuration.lpszAutoConfigUrl,
            configuration.lpszProxy,
            configuration.lpszProxyBypass,
        ] {
            if !value.is_null() {
                let _ = unsafe { GlobalFree(Some(HGLOBAL(value.0.cast()))) };
            }
        }
        if automatic {
            assert_eq!(super::current(), Err(super::AUTOMATIC_PROXY_ERROR.into()));
        } else if !static_proxy_present {
            assert_eq!(super::current(), Ok(None));
        }
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
    fn windows_static_proxy_rejects_missing_https_and_invalid_addresses() {
        for settings in [
            "",
            " ",
            "https=",
            "http=proxy.example:8080",
            "socks=proxy.example:1080",
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
            "HTTPEnable : 1\nHTTPSEnable : 0",
            "HTTPSEnable : 1",
            "HTTPSEnable : 1\nHTTPSProxy : \nHTTPSPort : 7890",
            "HTTPSEnable : 1\nHTTPSProxy : 127.0.0.1\nHTTPSPort : 0",
        ] {
            let error = super::parse(settings).unwrap_err();
            assert!(error.contains("代理池页面关闭系统代理"));
        }
        assert_eq!(
            super::parse(
                "ProxyAutoConfigEnable : 0\nProxyAutoDiscoveryEnable : 0\nHTTPSEnable : 0"
            ),
            Ok(None)
        );
    }
}
