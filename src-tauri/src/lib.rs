mod runtime;

use runtime::{RuntimeController, RuntimeSnapshot};
use tauri::{Manager, RunEvent, State, webview::PageLoadEvent};

#[tauri::command]
fn runtime_status(controller: State<'_, RuntimeController>) -> RuntimeSnapshot {
    controller.status()
}

#[tauri::command]
fn restart_runtime(controller: State<'_, RuntimeController>) -> Result<(), String> {
    controller.restart()
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
        .invoke_handler(tauri::generate_handler![runtime_status, restart_runtime])
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .ok_or("main webview window is missing")?;
            let landing_url = window.url()?;
            let controller = RuntimeController::start(app.handle(), window, landing_url)?;
            app.manage(controller);
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
