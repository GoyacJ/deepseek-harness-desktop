mod runtime;
mod update;
mod plugin;

use runtime::{RuntimeController, RuntimeSnapshot};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    Manager, RunEvent, State, Wry, webview::PageLoadEvent, WebviewWindow,
};
use update::{create_update_toast, handle_menu_event, refresh_menu, UpdateCoordinator};

#[tauri::command]
fn runtime_status(controller: State<'_, RuntimeController>) -> RuntimeSnapshot {
    controller.status()
}

#[tauri::command]
fn restart_runtime(
    window: WebviewWindow,
    controller: State<'_, RuntimeController>,
) -> Result<(), String> {
    if is_official_dsh_page(&window, &controller) {
        return Err("官方 DSH 页面不能重启运行时".into());
    }
    controller.restart()
}

#[tauri::command]
fn dismiss_update_toast(app: tauri::AppHandle, window: WebviewWindow) {
    if window.label() != "update-toast" {
        return;
    }
    update::dismiss_update_toast(&app);
}

#[tauri::command]
fn dismiss_plugin_dialog(app: tauri::AppHandle, window: WebviewWindow) {
    if !plugin::is_manager_window(window.label()) {
        return;
    }
    plugin::dismiss_manager(&app);
}

#[tauri::command]
fn resize_plugin_dialog(app: tauri::AppHandle, window: WebviewWindow) {
    if !plugin::is_manager_window(window.label()) {
        return;
    }
    plugin::resize_manager(&app);
}

#[tauri::command]
fn list_user_plugins(app: tauri::AppHandle, window: WebviewWindow) -> Result<Vec<plugin::UserPlugin>, String> {
    if !plugin::is_manager_window(window.label()) {
        return Err("只能从插件窗口读取。".into());
    }
    plugin::list_installed(&app)
}

#[tauri::command]
fn search_hub_plugins(
    window: WebviewWindow,
    query: String,
    category: String,
    sort: String,
    offset: u32,
) -> Result<plugin::HubPage, String> {
    if !plugin::is_manager_window(window.label()) {
        return Err("只能从插件窗口搜索。".into());
    }
    plugin::search_hub(query, category, sort, offset)
}

#[tauri::command]
fn submit_plugin_add(app: tauri::AppHandle, window: WebviewWindow, spec: String) -> Result<(), String> {
    if !plugin::is_manager_window(window.label()) {
        return Err("只能从插件窗口提交。".into());
    }
    plugin::submit_add(&app, spec)
}

#[tauri::command]
fn submit_plugin_remove(app: tauri::AppHandle, window: WebviewWindow, spec: String) -> Result<(), String> {
    if !plugin::is_manager_window(window.label()) {
        return Err("只能从插件窗口提交。".into());
    }
    plugin::submit_remove(&app, spec)
}

#[tauri::command]
fn submit_plugin_set_enabled(
    app: tauri::AppHandle,
    window: WebviewWindow,
    spec: String,
    enabled: bool,
) -> Result<(), String> {
    if !plugin::is_manager_window(window.label()) {
        return Err("只能从插件窗口提交。".into());
    }
    plugin::submit_set_enabled(&app, spec, enabled)
}

#[tauri::command]
fn submit_hub_install(
    app: tauri::AppHandle,
    window: WebviewWindow,
    owner: String,
    name: String,
) -> Result<(), String> {
    if !plugin::is_manager_window(window.label()) {
        return Err("只能从插件窗口提交。".into());
    }
    plugin::submit_hub_install(&app, owner, name)
}

#[tauri::command]
fn get_hub_plugin(
    window: WebviewWindow,
    owner: String,
    name: String,
) -> Result<plugin::HubPluginDetail, String> {
    if !plugin::is_manager_window(window.label()) {
        return Err("只能从插件窗口读取。".into());
    }
    plugin::get_hub(owner, name)
}

fn is_official_dsh_page(window: &WebviewWindow, controller: &RuntimeController) -> bool {
    window
        .url()
        .ok()
        .is_some_and(|url| controller.owns_ready_url(&url))
}

fn build_menu(app: &tauri::App) -> tauri::Result<(tauri::menu::Menu<Wry>, UpdateCoordinator)> {
    let current = MenuItem::with_id(
        app,
        "dsh-current",
        format!("当前 DSH {}", runtime::BUNDLED_DSH_VERSION),
        false,
        None::<&str>,
    )?;
    let check = MenuItem::with_id(app, "dsh-check", "检查 DSH 更新", true, None::<&str>)?;
    let install = MenuItem::with_id(app, "dsh-install", "安装并重启 DSH 更新", false, None::<&str>)?;
    let restore = MenuItem::with_id(app, "dsh-restore", "恢复随包版本", false, None::<&str>)?;
    let plugins = MenuItem::with_id(app, "dsh-plugins", "插件管理", true, None::<&str>)?;
    let desktop_current = MenuItem::with_id(
        app,
        "desktop-current",
        format!("当前桌面 {}", env!("CARGO_PKG_VERSION")),
        false,
        None::<&str>,
    )?;
    let desktop_check = MenuItem::with_id(app, "desktop-check", "检查桌面更新", true, None::<&str>)?;
    let desktop_install =
        MenuItem::with_id(app, "desktop-install", "安装并重启桌面更新", false, None::<&str>)?;
    let dsh_menu = Submenu::with_items(
        app,
        "DSH",
        true,
        &[
            &current,
            &PredefinedMenuItem::separator(app)?,
            &check,
            &install,
            &restore,
        ],
    )?;
    let plugin_menu = Submenu::with_items(app, "插件", true, &[&plugins])?;
    let app_menu = Submenu::with_items(
        app,
        "dsh-dk",
        true,
        &[
            &PredefinedMenuItem::about(app, Some("关于 dsh-dk"), None)?,
            &PredefinedMenuItem::separator(app)?,
            &desktop_current,
            &desktop_check,
            &desktop_install,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, Some("退出"))?,
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
    let menu = Menu::with_items(app, &[&app_menu, &edit_menu, &dsh_menu, &plugin_menu])?;
    Ok((
        menu,
        UpdateCoordinator::new(
            current,
            check,
            install,
            restore,
            plugins,
            desktop_current,
            desktop_check,
            desktop_install,
        ),
    ))
}

pub fn run() {
    let single_instance = tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(window) = app.get_webview_window("main")
            && window.is_visible().unwrap_or(false)
        {
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    });
    let navigation_guard = tauri::plugin::Builder::<tauri::Wry>::new("navigation-guard")
        .on_navigation(|_webview, url| {
            matches!(url.scheme(), "tauri" | "asset")
                || matches!(
                    (url.scheme(), url.host_str()),
                    ("http" | "https", Some("tauri.localhost" | "127.0.0.1"))
                )
        })
        .build();

    let app = tauri::Builder::default()
        .plugin(single_instance)
        .plugin(navigation_guard)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .on_page_load(|webview, payload| {
            let controller = webview.app_handle().try_state::<RuntimeController>();
            let is_official_ui = webview.label() == "main"
                && payload.event() == PageLoadEvent::Finished
                && controller
                    .as_deref()
                    .is_some_and(|runtime| runtime.owns_ready_url(payload.url()));
            if is_official_ui {
                let window = webview.window();
                let _ = window.show();
                let _ = window.set_focus();
            }
        })
        .on_menu_event(|app, event| {
            handle_menu_event(app, event.id().as_ref());
        })
        .invoke_handler(tauri::generate_handler![
            runtime_status,
            restart_runtime,
            dismiss_update_toast,
            dismiss_plugin_dialog,
            resize_plugin_dialog,
            list_user_plugins,
            search_hub_plugins,
            get_hub_plugin,
            submit_plugin_add,
            submit_plugin_remove,
            submit_plugin_set_enabled,
            submit_hub_install
        ])
        .setup(|app| {
            let (menu, coordinator) = build_menu(app)?;
            app.set_menu(menu)?;
            let window = app
                .get_webview_window("main")
                .ok_or("main webview window is missing")?;
            let landing_url = window.url()?;
            let controller = RuntimeController::start(app.handle(), window, landing_url)?;
            app.manage(controller);
            app.manage(coordinator);
            create_update_toast(app)?;
            refresh_menu(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build dsh-dk");

    app.run(|app_handle, event| {
        if matches!(event, RunEvent::ExitRequested { .. }) {
            app_handle.state::<RuntimeController>().shutdown();
        }
    });
}
