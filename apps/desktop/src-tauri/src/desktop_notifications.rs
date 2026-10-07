use ricoh_monitor_core::app::RecentEvent;
use tauri::{plugin::PermissionState, AppHandle};
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use tauri_plugin_notification::NotificationExt;

#[cfg(target_os = "macos")]
use objc2_foundation::{NSObject, NSObjectProtocol};
#[cfg(target_os = "macos")]
use objc2_user_notifications::UNUserNotificationCenterDelegate;

#[cfg(target_os = "macos")]
objc2::define_class!(
    #[unsafe(super(NSObject))]
    struct ForegroundNotificationDelegate;

    unsafe impl NSObjectProtocol for ForegroundNotificationDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for ForegroundNotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &objc2_user_notifications::UNUserNotificationCenter,
            _notification: &objc2_user_notifications::UNNotification,
            completion: &block2::DynBlock<
                dyn Fn(objc2_user_notifications::UNNotificationPresentationOptions),
            >,
        ) {
            use objc2_user_notifications::UNNotificationPresentationOptions;
            completion
                .call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
        }
    }
);

#[cfg(target_os = "macos")]
pub(crate) fn install_foreground_delegate() {
    use objc2::{rc::Retained, runtime::ProtocolObject, AnyThread};
    use objc2_user_notifications::UNUserNotificationCenter;

    // 通知中心弱引用 delegate；进程存续期间保留它，才能显示前台通知。
    let delegate: Retained<ForegroundNotificationDelegate> =
        unsafe { objc2::msg_send![ForegroundNotificationDelegate::alloc(), init] };
    UNUserNotificationCenter::currentNotificationCenter()
        .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    std::mem::forget(delegate);
}

#[cfg(target_os = "macos")]
pub(crate) async fn permission_state(_app: &AppHandle) -> Result<PermissionState, String> {
    tauri::async_runtime::spawn_blocking(|| {
        use objc2_user_notifications::{
            UNAuthorizationStatus, UNNotificationSettings, UNUserNotificationCenter,
        };
        use std::{ptr::NonNull, sync::mpsc, time::Duration};

        let (sender, receiver) = mpsc::sync_channel(1);
        let completion = block2::RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            let _ = sender.send(status);
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&completion);
        let status = receiver
            .recv_timeout(Duration::from_secs(3))
            .map_err(|_| "无法读取 macOS 通知权限，请稍后重试。".to_string())?;
        match status {
            UNAuthorizationStatus::NotDetermined => Ok(PermissionState::Prompt),
            UNAuthorizationStatus::Denied => Ok(PermissionState::Denied),
            UNAuthorizationStatus::Authorized
            | UNAuthorizationStatus::Provisional
            | UNAuthorizationStatus::Ephemeral => Ok(PermissionState::Granted),
            _ => Err("macOS 返回了未知的通知权限状态。".into()),
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(target_os = "windows")]
pub(crate) async fn permission_state(app: &AppHandle) -> Result<PermissionState, String> {
    let app_id = windows_app_id(&app.config().identifier, tauri::is_dev()).to_owned();
    let display_name = app
        .config()
        .product_name
        .clone()
        .unwrap_or_else(|| "RichoMonitor".into());
    tauri::async_runtime::spawn_blocking(move || {
        let setting = windows_notifier(&app_id, tauri::is_dev(), &display_name)
            .and_then(|notifier| windows_notification_setting(&notifier, &app_id, tauri::is_dev()))
            .map_err(|error| format!("Windows 通知权限读取失败，请重新检查：{error}"))?;
        windows_permission_from_setting(setting)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) async fn permission_state(app: &AppHandle) -> Result<PermissionState, String> {
    app.notification()
        .permission_state()
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
pub(crate) async fn request_permission(app: &AppHandle) -> Result<PermissionState, String> {
    use objc2::runtime::Bool;
    use objc2_user_notifications::{UNAuthorizationOptions, UNUserNotificationCenter};

    let current = permission_state(app).await?;
    if !permission_request_is_available(current) {
        return Ok(current);
    }
    let granted = tauri::async_runtime::spawn_blocking(|| {
        use std::{sync::mpsc, time::Duration};

        let (sender, receiver) = mpsc::sync_channel(1);
        let completion = block2::RcBlock::new(
            move |granted: Bool, error: *mut objc2_foundation::NSError| {
                let result = if error.is_null() {
                    Ok(granted.is_true())
                } else {
                    let error = unsafe { &*error };
                    Err(format!(
                        "macOS 通知授权请求失败（domain: {}, code: {}）：{}",
                        error.domain(),
                        error.code(),
                        error.localizedDescription()
                    ))
                };
                let _ = sender.send(result);
            },
        );
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert
                    | UNAuthorizationOptions::Badge
                    | UNAuthorizationOptions::Sound,
                &completion,
            );
        receiver
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| {
                "尚未收到 macOS 授权结果。请完成系统提示，再点击“检查权限”。".to_string()
            })?
    })
    .await
    .map_err(|error| error.to_string())??;
    let state = permission_state(app).await?;
    if !granted && state == PermissionState::Prompt {
        return Err(
            "macOS 通知授权未完成（granted: false，error: nil，status: NotDetermined）。".into(),
        );
    }
    Ok(state)
}

#[cfg(any(target_os = "macos", test))]
fn permission_request_is_available(permission: PermissionState) -> bool {
    permission == PermissionState::Prompt
}

#[cfg(any(target_os = "windows", test))]
fn windows_app_id(identifier: &str, is_dev: bool) -> &str {
    if is_dev {
        // 开发版没有安装程序注册的快捷方式，通知归属 PowerShell。
        "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe"
    } else {
        identifier
    }
}

#[cfg(target_os = "windows")]
fn windows_notifier(
    app_id: &str,
    is_dev: bool,
    display_name: &str,
) -> windows::core::Result<windows::UI::Notifications::ToastNotifier> {
    if !is_dev {
        // Win32 通知使用 Windows 的 stub CLSID 与协议激活，身份独立于快捷方式索引。
        let key = windows_registry::CURRENT_USER
            .create(format!(r"Software\Classes\AppUserModelId\{app_id}"))?;
        key.set_string("DisplayName", display_name)?;
        key.set_string("CustomActivator", "{DCBCE77E-8D71-4EF1-BF02-3B46C923DA47}")?;
        let protocol =
            windows_registry::CURRENT_USER.create(format!(r"Software\Classes\{app_id}"))?;
        protocol.set_string("", "URL:RichoMonitor")?;
        protocol.set_string("URL Protocol", "")?;
        protocol.create(r"shell\open\command")?.set_string(
            "",
            format!("\"{}\" \"%1\"", std::env::current_exe()?.display()),
        )?;
    }
    windows::UI::Notifications::ToastNotificationManager::CreateToastNotifierWithId(
        &windows::core::HSTRING::from(app_id),
    )
}

#[cfg(target_os = "windows")]
fn windows_notification_setting(
    notifier: &windows::UI::Notifications::ToastNotifier,
    app_id: &str,
    is_dev: bool,
) -> windows::core::Result<windows::UI::Notifications::NotificationSetting> {
    use windows::{
        core::{h, Interface, HRESULT, HSTRING},
        Foundation::{DateTime, IReference, PropertyValue},
        UI::Notifications::{ToastNotification, ToastNotificationManager},
    };

    // 焦点刷新与后台权限检查可能同时进入；共用身份初始化与清理过程。
    static SETTING_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = SETTING_LOCK.lock().unwrap();

    match notifier.Setting() {
        // 首次投递前 Win32 应用尚无通知平台记录；按微软 Toolkit 的方式无横幅初始化。
        Err(error) if error.code() == HRESULT(0x80070490u32 as i32) => {
            let document = windows_toast_document("RichoMonitor", "通知初始化", app_id, is_dev)?;
            let notification = ToastNotification::CreateToastNotification(&document)?;
            notification.SetSuppressPopup(true)?;
            notification.SetTag(h!("initialize"))?;
            notification.SetGroup(h!("identity"))?;
            let unix_seconds = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            // WinRT 使用自 1601 年起的 100 纳秒刻度；清理失败时初始化消息也会在 15 秒后过期。
            let expires = PropertyValue::CreateDateTime(DateTime {
                UniversalTime: ((unix_seconds + 11_644_473_600 + 15) * 10_000_000) as i64,
            })?
            .cast::<IReference<DateTime>>()?;
            notification.SetExpirationTime(&expires)?;
            notifier.Show(&notification)?;
            let setting = wait_for_windows_notification_setting(
                || notifier.Setting(),
                std::time::Instant::now() + std::time::Duration::from_secs(2),
            );
            let cleanup = ToastNotificationManager::History().and_then(|history| {
                history.RemoveGroupedTagWithId(
                    h!("initialize"),
                    h!("identity"),
                    &HSTRING::from(app_id),
                )
            });
            if let Err(error) = cleanup {
                eprintln!("Windows 通知初始化记录清理失败：{error}");
            }
            setting
        }
        result => result,
    }
}

#[cfg(target_os = "windows")]
fn wait_for_windows_notification_setting(
    mut read: impl FnMut() -> windows::core::Result<windows::UI::Notifications::NotificationSetting>,
    deadline: std::time::Instant,
) -> windows::core::Result<windows::UI::Notifications::NotificationSetting> {
    loop {
        let result = read();
        if !matches!(&result, Err(error) if error.code() == windows::core::HRESULT(0x80070490u32 as i32))
        {
            return result;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return result;
        }
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(50)));
    }
}

#[cfg(target_os = "windows")]
fn windows_permission_from_setting(
    setting: windows::UI::Notifications::NotificationSetting,
) -> Result<PermissionState, String> {
    use windows::UI::Notifications::NotificationSetting;

    match setting {
        NotificationSetting::Enabled => Ok(PermissionState::Granted),
        NotificationSetting::DisabledForApplication
        | NotificationSetting::DisabledForUser
        | NotificationSetting::DisabledByGroupPolicy
        | NotificationSetting::DisabledByManifest => Ok(PermissionState::Denied),
        _ => Err(format!(
            "Windows 返回了未知的通知权限状态（{}），请重新检查。",
            setting.0
        )),
    }
}

#[cfg(target_os = "windows")]
fn windows_toast_document(
    title: &str,
    body: &str,
    app_id: &str,
    is_dev: bool,
) -> windows::core::Result<windows::Data::Xml::Dom::XmlDocument> {
    use windows::{
        core::{h, HSTRING},
        UI::Notifications::{ToastNotificationManager, ToastTemplateType},
    };

    let document = ToastNotificationManager::GetTemplateContent(ToastTemplateType::ToastText02)?;
    let nodes = document.GetElementsByTagName(h!("text"))?;
    for (index, text) in [(0, title), (1, body)] {
        nodes
            .Item(index)?
            .AppendChild(&document.CreateTextNode(&HSTRING::from(text))?)?;
    }
    let audio = document.CreateElement(h!("audio"))?;
    audio.SetAttribute(h!("silent"), h!("true"))?;
    let root = document.DocumentElement()?;
    root.AppendChild(&audio)?;
    if !is_dev {
        root.SetAttribute(h!("activationType"), h!("protocol"))?;
        root.SetAttribute(h!("launch"), &HSTRING::from(format!("{app_id}://open")))?;
    }
    Ok(document)
}

#[cfg(target_os = "windows")]
async fn send_windows(app: &AppHandle, title: &'static str, body: String) -> Result<(), String> {
    let app_id = windows_app_id(&app.config().identifier, tauri::is_dev()).to_owned();
    let display_name = app
        .config()
        .product_name
        .clone()
        .unwrap_or_else(|| "RichoMonitor".into());
    tauri::async_runtime::spawn_blocking(move || {
        let notifier = windows_notifier(&app_id, tauri::is_dev(), &display_name)
            .map_err(|error| format!("Windows 通知连接失败，请重新发送：{error}"))?;
        let setting = windows_notification_setting(&notifier, &app_id, tauri::is_dev())
            .map_err(|error| format!("Windows 通知权限读取失败，请重新检查：{error}"))?;
        if windows_permission_from_setting(setting)? != PermissionState::Granted {
            return Err("Windows 通知已关闭。请在系统设置中开启本应用通知。".into());
        }
        let notification = windows_toast_document(title, &body, &app_id, tauri::is_dev())
            .and_then(|document| {
                windows::UI::Notifications::ToastNotification::CreateToastNotification(&document)
            })
            .map_err(|error| format!("Windows 通知内容创建失败，请重新发送：{error}"))?;
        notifier
            .Show(&notification)
            .map_err(|error| format!("Windows 通知提交失败，请重新发送：{error}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::permission_request_is_available;
    use tauri::plugin::PermissionState;

    #[test]
    fn macos_permission_request_only_runs_before_the_first_decision() {
        assert!(permission_request_is_available(PermissionState::Prompt));
        assert!(!permission_request_is_available(PermissionState::Denied));
        assert!(!permission_request_is_available(PermissionState::Granted));
        assert!(!permission_request_is_available(
            PermissionState::PromptWithRationale
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_first_setting_waits_for_registration_but_preserves_other_errors() {
        use windows::{
            core::{Error, HRESULT},
            UI::Notifications::NotificationSetting,
        };
        let missing = HRESULT(0x80070490u32 as i32);
        let mut attempts = 0;
        let result = super::wait_for_windows_notification_setting(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(Error::from_hresult(missing))
                } else {
                    Ok(NotificationSetting::Enabled)
                }
            },
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        );
        assert_eq!(result.unwrap(), NotificationSetting::Enabled);
        assert_eq!(attempts, 3);
        for result in [
            Ok(NotificationSetting::DisabledForApplication),
            Err(Error::from_hresult(HRESULT(0x80070005u32 as i32))),
        ] {
            let mut reads = 0;
            let actual = super::wait_for_windows_notification_setting(
                || {
                    reads += 1;
                    result.clone()
                },
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            );
            assert_eq!(reads, 1);
            match result {
                Ok(setting) => assert_eq!(actual.unwrap(), setting),
                Err(error) => assert_eq!(actual.unwrap_err().code(), error.code()),
            }
        }
        let failed = super::wait_for_windows_notification_setting(
            || Err(Error::from_hresult(missing)),
            std::time::Instant::now(),
        );
        assert_eq!(failed.unwrap_err().code(), missing);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_concurrent_first_permission_reads_keep_identity_ready() {
        let app_id = format!("dev.ricohmonitor.concurrent-test.{}", std::process::id());
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let notifier = super::windows_notifier(
                            &app_id,
                            false,
                            "RichoMonitor notification test",
                        )
                        .unwrap();
                        super::windows_notification_setting(&notifier, &app_id, false).unwrap()
                    })
                })
                .collect();
            for worker in workers {
                assert_eq!(
                    worker.join().unwrap(),
                    windows::UI::Notifications::NotificationSetting::Enabled
                );
            }
        });
        let history = windows::UI::Notifications::ToastNotificationManager::History().unwrap();
        assert_eq!(
            history
                .GetHistoryWithId(&windows::core::HSTRING::from(&app_id))
                .unwrap()
                .Size()
                .unwrap(),
            0
        );
        windows_registry::CURRENT_USER
            .remove_tree(format!(r"Software\Classes\AppUserModelId\{app_id}"))
            .unwrap();
        windows_registry::CURRENT_USER
            .remove_tree(format!(r"Software\Classes\{app_id}"))
            .unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_notifier_registers_identity_without_a_start_menu_shortcut() {
        let app_id = format!("dev.ricohmonitor.notification-test.{}", std::process::id());
        let path = format!(r"Software\Classes\AppUserModelId\{app_id}");
        let notifier =
            super::windows_notifier(&app_id, false, "RichoMonitor notification test").unwrap();
        let result = (|| {
            let key = windows_registry::CURRENT_USER.open(&path)?;
            assert_eq!(key.get_string("DisplayName")?, "RichoMonitor notification test");
            super::windows_permission_from_setting(super::windows_notification_setting(
                &notifier, &app_id, false,
            )?)
            .unwrap();
            super::windows_permission_from_setting(notifier.Setting()?).unwrap();
            assert_eq!(
                windows::UI::Notifications::ToastNotificationManager::History()?
                    .GetHistoryWithId(&windows::core::HSTRING::from(&app_id))?
                    .Size()?,
                0
            );
            Ok::<_, windows::core::Error>(())
        })();
        let _ = windows_registry::CURRENT_USER.remove_tree(&path);
        let _ = windows_registry::CURRENT_USER.remove_tree(format!(r"Software\Classes\{app_id}"));
        result.unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_notification_setting_reports_every_disabled_state() {
        use super::windows_permission_from_setting;
        use windows::UI::Notifications::NotificationSetting;

        assert_eq!(
            windows_permission_from_setting(NotificationSetting::Enabled).unwrap(),
            PermissionState::Granted
        );
        for setting in [
            NotificationSetting::DisabledForApplication,
            NotificationSetting::DisabledForUser,
            NotificationSetting::DisabledByGroupPolicy,
            NotificationSetting::DisabledByManifest,
        ] {
            assert_eq!(
                windows_permission_from_setting(setting).unwrap(),
                PermissionState::Denied
            );
        }
        assert!(windows_permission_from_setting(NotificationSetting(99)).is_err());
    }

    #[test]
    fn windows_installed_app_and_debug_notifications_use_separate_app_ids() {
        assert_eq!(
            super::windows_app_id("dev.ricohmonitor.desktop", false),
            "dev.ricohmonitor.desktop"
        );
        assert_eq!(
            super::windows_app_id("dev.ricohmonitor.debug", true),
            "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_toast_dom_preserves_chinese_and_xml_special_characters() {
        let title = "库存 <GR> & \"提醒\"";
        let body = "中文、换行\n商品 A & B <新品> '可购买' 📷";
        let document =
            super::windows_toast_document(title, body, "dev.ricohmonitor.desktop", false).unwrap();
        let nodes = document
            .GetElementsByTagName(&windows::core::HSTRING::from("text"))
            .unwrap();
        assert_eq!(nodes.Length().unwrap(), 2);
        assert_eq!(nodes.Item(0).unwrap().InnerText().unwrap(), title);
        assert_eq!(nodes.Item(1).unwrap().InnerText().unwrap(), body);
        let root = document.DocumentElement().unwrap();
        assert_eq!(
            root.GetAttribute(&windows::core::HSTRING::from("activationType"))
                .unwrap(),
            "protocol"
        );
        assert_eq!(
            root.GetAttribute(&windows::core::HSTRING::from("launch"))
                .unwrap(),
            "dev.ricohmonitor.desktop://open"
        );
        let debug =
            super::windows_toast_document(title, body, super::windows_app_id("", true), true)
                .unwrap();
        assert!(debug
            .DocumentElement()
            .unwrap()
            .GetAttribute(&windows::core::HSTRING::from("launch"))
            .unwrap()
            .is_empty());
    }
}

#[cfg(target_os = "windows")]
pub(crate) async fn request_permission(app: &AppHandle) -> Result<PermissionState, String> {
    std::process::Command::new("explorer.exe")
        .arg("ms-settings:notifications")
        .spawn()
        .map_err(|error| format!("无法打开 Windows 通知设置：{error}"))?;
    permission_state(app).await
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) async fn request_permission(app: &AppHandle) -> Result<PermissionState, String> {
    app.notification()
        .request_permission()
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
pub(crate) async fn show(app: &AppHandle, event: &RecentEvent) -> Result<(), String> {
    send_windows(app, "RichoMonitor", event.message.clone()).await
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) async fn show(app: &AppHandle, event: &RecentEvent) -> Result<(), String> {
    app.notification()
        .builder()
        .title("RichoMonitor")
        .body(&event.message)
        .show()
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
pub(crate) async fn show_test(app: &AppHandle) -> Result<(), String> {
    send_windows(
        app,
        "RichoMonitor · 测试消息",
        "这是通知通道测试。请确认是否在通知中心看到此消息。".into(),
    )
    .await
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) async fn show_test(app: &AppHandle) -> Result<(), String> {
    app.notification()
        .builder()
        .title("RichoMonitor · 测试消息")
        .body("这是通知通道测试。请确认是否在通知中心看到此消息。")
        .show()
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
async fn send(title: &'static str, body: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        use objc2_foundation::NSString;
        use objc2_user_notifications::{
            UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
        };
        use std::{
            sync::mpsc,
            time::{Duration, SystemTime, UNIX_EPOCH},
        };

        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(&body));
        let identifier = format!(
            "ricoh-monitor-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&identifier),
            &content,
            None,
        );
        let (sender, receiver) = mpsc::sync_channel(1);
        let completion = block2::RcBlock::new(move |error: *mut objc2_foundation::NSError| {
            let result = if error.is_null() {
                Ok(())
            } else {
                Err(unsafe { &*error }.localizedDescription().to_string())
            };
            let _ = sender.send(result);
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
        receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "macOS 通知投递超时，请稍后重试。".to_string())?
            .map_err(|error| format!("macOS 通知投递失败：{error}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(target_os = "macos")]
pub(crate) async fn show(_app: &AppHandle, event: &RecentEvent) -> Result<(), String> {
    send("RichoMonitor", event.message.clone()).await
}

#[cfg(target_os = "macos")]
pub(crate) async fn show_test(_app: &AppHandle) -> Result<(), String> {
    send(
        "RichoMonitor · 测试消息",
        "这是通知通道测试。请确认是否在通知中心看到此消息。".into(),
    )
    .await
}

#[tauri::command]
pub async fn open_external_url(
    url: String,
) -> Result<crate::desktop_backend::OperationResult, String> {
    let parsed = url::Url::parse(&url).map_err(|_| "链接格式无效。".to_string())?;
    if !matches!(parsed.scheme(), "https" | "http") {
        return Err("仅支持打开 HTTP 或 HTTPS 链接。".into());
    }
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("explorer.exe")
    } else {
        std::process::Command::new("xdg-open")
    };
    command
        .arg(parsed.as_str())
        .spawn()
        .map_err(|error| format!("无法打开链接：{error}"))?;
    Ok(crate::desktop_backend::OperationResult::ok())
}
