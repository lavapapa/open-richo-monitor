use tauri::{
    menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu},
    Emitter,
};

pub fn install(app: &tauri::App) -> tauri::Result<()> {
    let app_menu = Submenu::with_items(
        app,
        "RichoMonitor",
        true,
        &[
            &PredefinedMenuItem::about(
                app,
                Some("关于 RichoMonitor"),
                Some(AboutMetadata {
                    name: Some("RichoMonitor".into()),
                    version: Some(app.package_info().version.to_string()),
                    icon: app.default_window_icon().cloned(),
                    ..Default::default()
                }),
            )?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "desktop-settings", "设置…", true, Some("CmdOrCtrl+,"))?,
            &MenuItem::with_id(
                app,
                "desktop-check-updates",
                "检查更新…",
                true,
                None::<&str>,
            )?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, Some("服务"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, Some("隐藏 RichoMonitor"))?,
            &PredefinedMenuItem::hide_others(app, Some("隐藏其他"))?,
            &PredefinedMenuItem::show_all(app, Some("显示全部"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, Some("退出 RichoMonitor"))?,
        ],
    )?;
    let edit_menu = Submenu::with_items(
        app,
        "编辑",
        true,
        &[
            &PredefinedMenuItem::undo(app, Some("撤销"))?,
            &PredefinedMenuItem::redo(app, Some("重做"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, Some("剪切"))?,
            &PredefinedMenuItem::copy(app, Some("复制"))?,
            &PredefinedMenuItem::paste(app, Some("粘贴"))?,
            &PredefinedMenuItem::select_all(app, Some("全选"))?,
        ],
    )?;
    let view_menu = Submenu::with_items(
        app,
        "显示",
        true,
        &[
            &MenuItem::with_id(app, "desktop-status", "状态", true, Some("CmdOrCtrl+1"))?,
            &MenuItem::with_id(
                app,
                "desktop-notifications",
                "通知",
                true,
                Some("CmdOrCtrl+2"),
            )?,
            &MenuItem::with_id(app, "desktop-rate", "速率", true, Some("CmdOrCtrl+3"))?,
            &MenuItem::with_id(app, "desktop-proxies", "代理池", true, Some("CmdOrCtrl+4"))?,
            &MenuItem::with_id(
                app,
                "desktop-products",
                "监控产品",
                true,
                Some("CmdOrCtrl+5"),
            )?,
            &MenuItem::with_id(
                app,
                "desktop-settings-page",
                "其他设置",
                true,
                Some("CmdOrCtrl+6"),
            )?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(
                app,
                "desktop-find-products",
                "搜索商品…",
                true,
                Some("CmdOrCtrl+F"),
            )?,
        ],
    )?;
    let window_menu = Submenu::with_id_and_items(
        app,
        tauri::menu::WINDOW_SUBMENU_ID,
        "窗口",
        true,
        &[
            &PredefinedMenuItem::minimize(app, Some("最小化"))?,
            &PredefinedMenuItem::maximize(app, Some("缩放"))?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "desktop-show", "打开主窗口", true, None::<&str>)?,
            &PredefinedMenuItem::close_window(app, Some("关闭窗口"))?,
        ],
    )?;
    let help_menu = Submenu::with_id_and_items(
        app,
        tauri::menu::HELP_SUBMENU_ID,
        "帮助",
        true,
        &[&MenuItem::with_id(
            app,
            "desktop-project",
            "项目主页",
            true,
            None::<&str>,
        )?],
    )?;
    app.set_menu(Menu::with_items(
        app,
        &[&app_menu, &edit_menu, &view_menu, &window_menu, &help_menu],
    )?)?;
    app.on_menu_event(|app, event| {
        let Some(action) = event.id().as_ref().strip_prefix("desktop-") else {
            return;
        };
        if action == "show" {
            super::show_main(app);
        } else if action == "project" {
            tauri::async_runtime::spawn(async {
                let _ = super::desktop_notifications::open_external_url(
                    "https://github.com/lavapapa/open-richo-monitor".into(),
                )
                .await;
            });
        } else {
            super::show_main(app);
            let action = if action == "settings-page" {
                "settings"
            } else {
                action
            };
            let _ = app.emit_to("main", "desktop-action", action);
        }
    });
    Ok(())
}

pub fn window_state() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_window_state::{Builder, StateFlags};
    // 隐藏或最小化的状态不持久化，后台重开和更新重启仍由现有生命周期管理。
    Builder::default()
        .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
        .with_filter(|label| label == "main")
        .with_filename(if cfg!(debug_assertions) {
            "dev-window-state.json"
        } else {
            "window-state.json"
        })
        .build()
}
