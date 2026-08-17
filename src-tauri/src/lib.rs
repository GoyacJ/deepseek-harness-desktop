mod runtime;
mod update;

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
    let app_menu = Submenu::with_items(
        app,
        "DeepSeek Harness Desktop",
        true,
        &[
            &PredefinedMenuItem::about(app, Some("关于 DeepSeek Harness Desktop"), None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, Some("退出"))?,
        ],
    )?;
    let menu = Menu::with_items(app, &[&app_menu, &dsh_menu])?;
    Ok((
        menu,
        UpdateCoordinator::new(current, check, install, restore),
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
            dismiss_update_toast
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
        .expect("failed to build DeepSeek Harness Desktop");

    app.run(|app_handle, event| {
        if matches!(event, RunEvent::ExitRequested { .. }) {
            app_handle.state::<RuntimeController>().shutdown();
        }
    });
}
