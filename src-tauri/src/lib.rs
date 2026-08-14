mod runtime;

use runtime::{RuntimeController, RuntimeSnapshot};
use tauri::{Manager, RunEvent, State};

#[tauri::command]
fn runtime_status(controller: State<'_, RuntimeController>) -> RuntimeSnapshot {
    controller.status()
}

#[tauri::command]
fn restart_runtime(controller: State<'_, RuntimeController>) -> Result<(), String> {
    controller.restart()
}

pub fn run() {
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
        .plugin(navigation_guard)
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
