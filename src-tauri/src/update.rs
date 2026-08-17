use crate::runtime::{
    resolve_npm, scoped_runtime_root, RuntimeController, BUNDLED_DSH_VERSION, BUNDLED_NODE_VERSION,
};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{
    AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    menu::MenuItem, webview::Color, Wry,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

const OFFICIAL_DSH_NAME: &str = "@deepseek-ai/dsh";
const DEFAULT_NPM_REGISTRY: &str = "https://registry.npmjs.org";
const NPM_MIRROR_REGISTRY: &str = "https://registry.npmmirror.com";
const JSON_FETCH_TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const DESKTOP_UPDATE_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    Idle,
    Checking,
    Available,
    Downloading,
    Verifying,
    Staging,
    Switching,
    RollingBack,
    Failed,
    PluginInstalling,
    PluginRemoving,
    PluginToggling,
    Installing,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Active,
    Done,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UpdateStep {
    pub label: String,
    pub status: StepStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSource {
    Bundled,
    Sidecar,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeState {
    pub active_version: String,
    pub previous_version: Option<String>,
    #[serde(default = "default_runtime_source")]
    pub source: RuntimeSource,
}

fn default_runtime_source() -> RuntimeSource {
    RuntimeSource::Bundled
}

impl RuntimeState {
    pub fn bundled() -> Self {
        Self {
            active_version: BUNDLED_DSH_VERSION.to_string(),
            previous_version: None,
            source: RuntimeSource::Bundled,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct NpmVersionDocument {
    version: String,
    #[serde(default)]
    engines: NpmEngines,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct NpmEngines {
    #[serde(default)]
    node: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NpmLatest {
    version: String,
    registry: String,
    node: Option<String>,
    source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedRelease {
    pub version: String,
    pub registry: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum UpdateCheck {
    Latest,
    Available(SelectedRelease),
    NeedsDesktop(String),
}

pub struct UpdateCoordinator {
    pending: Mutex<Option<SelectedRelease>>,
    pending_desktop: Mutex<Option<String>>,
    busy: AtomicBool,
    current_item: MenuItem<Wry>,
    check_item: MenuItem<Wry>,
    install_item: MenuItem<Wry>,
    restore_item: MenuItem<Wry>,
    plugins_item: MenuItem<Wry>,
    desktop_current_item: MenuItem<Wry>,
    desktop_check_item: MenuItem<Wry>,
    desktop_install_item: MenuItem<Wry>,
}

impl UpdateCoordinator {
    pub fn new(
        current_item: MenuItem<Wry>,
        check_item: MenuItem<Wry>,
        install_item: MenuItem<Wry>,
        restore_item: MenuItem<Wry>,
        plugins_item: MenuItem<Wry>,
        desktop_current_item: MenuItem<Wry>,
        desktop_check_item: MenuItem<Wry>,
        desktop_install_item: MenuItem<Wry>,
    ) -> Self {
        Self {
            pending: Mutex::new(None),
            pending_desktop: Mutex::new(None),
            busy: AtomicBool::new(false),
            current_item,
            check_item,
            install_item,
            restore_item,
            plugins_item,
            desktop_current_item,
            desktop_check_item,
            desktop_install_item,
        }
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub(crate) fn try_begin(&self) -> bool {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(crate) fn finish(&self) {
        self.busy.store(false, Ordering::SeqCst);
    }
}

pub fn runtime_state_path(app_data: &Path) -> PathBuf {
    app_data.join("runtime-state.json")
}

pub fn sidecar_root(app_data: &Path) -> PathBuf {
    app_data.join("runtimes").join("dsh")
}

pub fn sidecar_app_root(app_data: &Path, version: &str) -> PathBuf {
    sidecar_root(app_data).join(sanitize_version(version))
}

pub fn sidecar_entry(app_root: &Path) -> PathBuf {
    app_root.join("node_modules/@deepseek-ai/dsh/lib/bin.js")
}

pub fn sidecar_is_complete(app_root: &Path) -> bool {
    sidecar_entry(app_root).is_file()
}

pub fn load_runtime_state(app_data: &Path) -> RuntimeState {
    let path = runtime_state_path(app_data);
    let Ok(contents) = fs::read_to_string(path) else {
        return RuntimeState::bundled();
    };
    serde_json::from_str(&contents).unwrap_or_else(|_| RuntimeState::bundled())
}

pub fn save_runtime_state(app_data: &Path, state: &RuntimeState) -> Result<(), String> {
    fs::create_dir_all(app_data).map_err(|error| error.to_string())?;
    let path = runtime_state_path(app_data);
    let temporary = path.with_extension("json.tmp");
    let contents = serde_json::to_string_pretty(state).map_err(|error| error.to_string())?;
    fs::write(&temporary, contents).map_err(|error| error.to_string())?;
    let _ = fs::remove_file(&path);
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

pub fn activate_sidecar(app_data: &Path, version: &str) -> Result<RuntimeState, String> {
    let current = load_runtime_state(app_data);
    let previous = match current.source {
        RuntimeSource::Sidecar if current.active_version != version => {
            Some(current.active_version)
        }
        _ => current.previous_version,
    };
    let state = RuntimeState {
        active_version: version.to_string(),
        previous_version: previous,
        source: RuntimeSource::Sidecar,
    };
    save_runtime_state(app_data, &state)?;
    Ok(state)
}

pub fn restore_bundled_state(app_data: &Path) -> Result<RuntimeState, String> {
    let current = load_runtime_state(app_data);
    let previous = match current.source {
        RuntimeSource::Sidecar => Some(current.active_version),
        RuntimeSource::Bundled => current.previous_version,
    };
    let state = RuntimeState {
        active_version: BUNDLED_DSH_VERSION.to_string(),
        previous_version: previous,
        source: RuntimeSource::Bundled,
    };
    save_runtime_state(app_data, &state)?;
    Ok(state)
}

pub fn rollback_runtime_state(app_data: &Path) -> RuntimeState {
    let current = load_runtime_state(app_data);
    if current.source != RuntimeSource::Sidecar {
        return current;
    }

    if let Some(previous) = current.previous_version.as_deref() {
        if previous != current.active_version && sidecar_is_complete(&sidecar_app_root(app_data, previous))
        {
            let state = RuntimeState {
                active_version: previous.to_string(),
                previous_version: None,
                source: RuntimeSource::Sidecar,
            };
            let _ = save_runtime_state(app_data, &state);
            return state;
        }
    }

    restore_bundled_state(app_data).unwrap_or_else(|_| RuntimeState::bundled())
}

fn select_npm_update(
    latest: &NpmLatest,
    current_version: &str,
    node_version: &str,
) -> UpdateCheck {
    if !version_newer(&latest.version, current_version) {
        return UpdateCheck::Latest;
    }
    if !node_supported(node_version, latest.node.as_deref()) {
        return UpdateCheck::NeedsDesktop(format!(
            "npm latest DSH {} 需要 Node {}，当前随包 Node 为 {}。请先用菜单「检查桌面更新」。",
            latest.version,
            latest.node.as_deref().unwrap_or("未知"),
            node_version
        ));
    }
    UpdateCheck::Available(SelectedRelease {
        version: latest.version.clone(),
        registry: latest.registry.clone(),
    })
}

pub fn handle_menu_event(app: &AppHandle, id: &str) {
    match id {
        "dsh-check" => {
            let app = app.clone();
            std::thread::spawn(move || check_for_update(&app));
        }
        "dsh-install" => {
            let app = app.clone();
            std::thread::spawn(move || confirm_and_install(&app));
        }
        "dsh-restore" => {
            let app = app.clone();
            std::thread::spawn(move || confirm_and_restore(&app));
        }
        "dsh-plugins" => crate::plugin::open_manager(app),
        "desktop-check" => {
            let app = app.clone();
            std::thread::spawn(move || check_desktop_update(&app));
        }
        "desktop-install" => {
            let app = app.clone();
            std::thread::spawn(move || confirm_and_install_desktop(&app));
        }
        _ => {}
    }
}

pub fn refresh_menu(app: &AppHandle) {
    let Ok(app_data) = app_data_dir(app) else {
        return;
    };
    let coordinator = app.state::<UpdateCoordinator>();
    let snapshot = app.state::<RuntimeController>().status();
    let state = load_runtime_state(&app_data);
    let busy = coordinator.busy.load(Ordering::SeqCst);
    let dsh_pending = lock_pending(&coordinator).is_some();
    let desktop_pending = lock_pending_desktop(&coordinator).is_some();
    let _ = coordinator
        .current_item
        .set_text(format!("当前 DSH {}", snapshot.dsh_version));
    let _ = coordinator
        .desktop_current_item
        .set_text(format!("当前桌面 {}", env!("CARGO_PKG_VERSION")));
    let _ = coordinator.check_item.set_enabled(!busy);
    let _ = coordinator.plugins_item.set_enabled(!busy);
    let _ = coordinator.install_item.set_enabled(!busy && dsh_pending);
    let _ = coordinator.desktop_check_item.set_enabled(!busy);
    let _ = coordinator
        .desktop_install_item
        .set_enabled(!busy && desktop_pending);
    let _ = coordinator
        .restore_item
        .set_enabled(!busy && state.source == RuntimeSource::Sidecar);
}

fn check_for_update(app: &AppHandle) {
    let coordinator = app.state::<UpdateCoordinator>();
    if !coordinator.try_begin() {
        return;
    }
    refresh_menu(app);
    let controller = app.state::<RuntimeController>();
    let mut steps = flow_from(&["连接 npm", "读取 latest", "对比", "结果"]);
    set_step(&mut steps, 0, StepStatus::Active, "正在连接 npm registry");
    report(
        app,
        &controller,
        UpdatePhase::Checking,
        "正在检查 npm latest。",
        None,
        &steps,
    );

    let result = (|| {
        let app_data = app_data_dir(app)?;
        let current = {
            let running = controller.status().dsh_version;
            if running.is_empty() {
                load_runtime_state(&app_data).active_version
            } else {
                running
            }
        };
        let latest = fetch_npm_latest(|note| {
            set_step(&mut steps, 0, StepStatus::Active, note);
            report(
                app,
                &controller,
                UpdatePhase::Checking,
                "正在检查 npm latest。",
                None,
                &steps,
            );
        })?;
        set_step(&mut steps, 0, StepStatus::Done, format!("已连接 {}", latest.source));
        set_step(
            &mut steps,
            1,
            StepStatus::Done,
            format!("latest {}", latest.version),
        );
        set_step(
            &mut steps,
            2,
            StepStatus::Active,
            format!("当前 DSH {current}"),
        );
        report(
            app,
            &controller,
            UpdatePhase::Checking,
            format!("npm latest 为 DSH {}。", latest.version),
            None,
            &steps,
        );
        let check = select_npm_update(&latest, &current, BUNDLED_NODE_VERSION);
        set_step(
            &mut steps,
            2,
            StepStatus::Done,
            format!("当前 DSH {current}"),
        );
        Ok::<_, String>((check, latest))
    })();

    match result {
        Ok((UpdateCheck::Latest, latest)) => {
            *lock_pending(&coordinator) = None;
            let message = format!("当前已是 npm latest（DSH {}）。", latest.version);
            set_step(&mut steps, 3, StepStatus::Done, message.clone());
            report(app, &controller, UpdatePhase::Idle, message, None, &steps);
        }
        Ok((UpdateCheck::Available(selected), latest)) => {
            let version = selected.version.clone();
            *lock_pending(&coordinator) = Some(selected);
            let message = format!(
                "发现 npm latest DSH {version}（来自 {}）。可在菜单中安装并重启，进行中的会话会中断。",
                latest.source
            );
            set_step(&mut steps, 3, StepStatus::Done, format!("可安装 {version}"));
            report(
                app,
                &controller,
                UpdatePhase::Available,
                message,
                Some(version),
                &steps,
            );
        }
        Ok((UpdateCheck::NeedsDesktop(message), _)) => {
            *lock_pending(&coordinator) = None;
            set_step(&mut steps, 3, StepStatus::Failed, message.clone());
            report(app, &controller, UpdatePhase::Idle, message, None, &steps);
        }
        Err(error) => {
            set_step(&mut steps, 3, StepStatus::Failed, error.clone());
            fail_remaining(&mut steps);
            report(app, &controller, UpdatePhase::Failed, error, None, &steps);
        }
    }

    coordinator.finish();
    refresh_menu(app);
}

fn confirm_and_install(app: &AppHandle) {
    let coordinator = app.state::<UpdateCoordinator>();
    let Some(selected) = lock_pending(&coordinator).clone() else {
        return;
    };
    let confirmed = app
        .dialog()
        .message(format!(
            "将从 npm 安装 {OFFICIAL_DSH_NAME}@{} 并重启运行时。进行中的会话会中断。",
            selected.version
        ))
        .title("安装 DSH 更新")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancel)
        .blocking_show();
    if !confirmed {
        return;
    }
    apply_pending_update(app, selected);
}

fn confirm_and_restore(app: &AppHandle) {
    let confirmed = app
        .dialog()
        .message(format!(
            "将恢复安装包内置的 DSH {BUNDLED_DSH_VERSION} 并重启运行时。进行中的会话会中断。"
        ))
        .title("恢复随包 DSH")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancel)
        .blocking_show();
    if !confirmed {
        return;
    }
    restore_bundled(app);
}

fn apply_pending_update(app: &AppHandle, selected: SelectedRelease) {
    let coordinator = app.state::<UpdateCoordinator>();
    if !coordinator.try_begin() {
        return;
    }
    refresh_menu(app);
    let controller = app.state::<RuntimeController>();
    let mut steps = flow_from(&["安装", "校验", "写入", "切换"]);
    set_step(
        &mut steps,
        0,
        StepStatus::Active,
        format!("{OFFICIAL_DSH_NAME}@{}", selected.version),
    );
    report(
        app,
        &controller,
        UpdatePhase::Downloading,
        format!("正在从 npm 安装 DSH {}。", selected.version),
        Some(selected.version.clone()),
        &steps,
    );

    let result = (|| {
        let app_data = app_data_dir(app)?;
        if Version::parse(&selected.version).is_err() {
            return Err("DSH 版本号无效".into());
        }
        let staging = app_data.join("runtimes").join(".staging");
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
        npm_install_dsh(app, &selected, &staging, |line| {
            set_step(&mut steps, 0, StepStatus::Active, line);
            report(
                app,
                &controller,
                UpdatePhase::Downloading,
                format!("正在从 npm 安装 DSH {}。", selected.version),
                Some(selected.version.clone()),
                &steps,
            );
        })?;
        set_step(&mut steps, 0, StepStatus::Done, "npm install 完成");
        set_step(&mut steps, 1, StepStatus::Active, "正在校验官方入口");
        report(
            app,
            &controller,
            UpdatePhase::Verifying,
            "正在校验 DSH 入口。",
            Some(selected.version.clone()),
            &steps,
        );
        verify_installed_dsh(&staging, &selected.version)?;
        set_step(&mut steps, 1, StepStatus::Done, "入口校验通过");
        set_step(
            &mut steps,
            2,
            StepStatus::Active,
            format!("正在写入 DSH {}", selected.version),
        );
        report(
            app,
            &controller,
            UpdatePhase::Staging,
            format!("正在安装 DSH {}。", selected.version),
            Some(selected.version.clone()),
            &steps,
        );
        let destination = sidecar_app_root(&app_data, &selected.version);
        if destination.exists() {
            fs::remove_dir_all(&destination).map_err(|error| error.to_string())?;
        }
        fs::create_dir_all(destination.parent().unwrap()).map_err(|error| error.to_string())?;
        fs::rename(&staging, &destination).map_err(|error| error.to_string())?;
        let state = activate_sidecar(&app_data, &selected.version)?;
        prune_sidecars(&app_data, &state)?;
        set_step(&mut steps, 2, StepStatus::Done, "已写入 sidecar");
        Ok::<_, String>(selected.version.clone())
    })();

    match result {
        Ok(version) => {
            set_step(
                &mut steps,
                3,
                StepStatus::Active,
                format!("正在切换到 DSH {version}"),
            );
            report(
                app,
                &controller,
                UpdatePhase::Switching,
                format!("正在切换到 DSH {version}。"),
                Some(version),
                &steps,
            );
            coordinator.finish();
            refresh_menu(app);
            if let Err(error) = controller.restart() {
                set_step(&mut steps, 3, StepStatus::Failed, error.clone());
                fail_remaining(&mut steps);
                report(app, &controller, UpdatePhase::Failed, error, None, &steps);
                refresh_menu(app);
            }
        }
        Err(error) => {
            set_step(&mut steps, 3, StepStatus::Failed, error.clone());
            fail_remaining(&mut steps);
            report(
                app,
                &controller,
                UpdatePhase::Failed,
                error,
                Some(selected.version),
                &steps,
            );
            coordinator.finish();
            refresh_menu(app);
        }
    }
}

fn restore_bundled(app: &AppHandle) {
    let coordinator = app.state::<UpdateCoordinator>();
    if !coordinator.try_begin() {
        return;
    }
    refresh_menu(app);
    let controller = app.state::<RuntimeController>();
    let mut steps = flow_from(&["恢复随包", "切换"]);
    set_step(
        &mut steps,
        0,
        StepStatus::Active,
        format!("DSH {BUNDLED_DSH_VERSION}"),
    );
    report(
        app,
        &controller,
        UpdatePhase::Switching,
        format!("正在恢复随包 DSH {BUNDLED_DSH_VERSION}。"),
        None,
        &steps,
    );
    let result = (|| {
        let app_data = app_data_dir(app)?;
        restore_bundled_state(&app_data)?;
        Ok::<_, String>(())
    })();
    match result {
        Ok(()) => {
            set_step(&mut steps, 0, StepStatus::Done, format!("DSH {BUNDLED_DSH_VERSION}"));
            set_step(&mut steps, 1, StepStatus::Active, "正在重启运行时");
            report(
                app,
                &controller,
                UpdatePhase::Switching,
                format!("正在恢复随包 DSH {BUNDLED_DSH_VERSION}。"),
                None,
                &steps,
            );
            coordinator.finish();
            refresh_menu(app);
            if let Err(error) = controller.restart() {
                set_step(&mut steps, 1, StepStatus::Failed, error.clone());
                report(app, &controller, UpdatePhase::Failed, error, None, &steps);
                refresh_menu(app);
            }
        }
        Err(error) => {
            set_step(&mut steps, 0, StepStatus::Failed, error.clone());
            set_step(&mut steps, 1, StepStatus::Skipped, String::new());
            report(app, &controller, UpdatePhase::Failed, error, None, &steps);
            coordinator.finish();
            refresh_menu(app);
        }
    }
}

fn check_desktop_update(app: &AppHandle) {
    let coordinator = app.state::<UpdateCoordinator>();
    if !coordinator.try_begin() {
        return;
    }
    refresh_menu(app);
    let controller = app.state::<RuntimeController>();
    let current = env!("CARGO_PKG_VERSION");
    let mut steps = flow_from(&["连接发布源", "读取版本", "对比", "结果"]);
    set_step(&mut steps, 0, StepStatus::Active, "正在连接 GitHub Releases");
    report(
        app,
        &controller,
        UpdatePhase::Checking,
        "正在检查桌面更新。",
        None,
        &steps,
    );

    let result = tauri::async_runtime::block_on(async {
        desktop_updater(app)?
            .check()
            .await
            .map_err(classify_desktop_update_error)
    });

    match result {
        Ok(None) => {
            *lock_pending_desktop(&coordinator) = None;
            let message = format!("当前已是最新桌面版本（{current}）。");
            set_step(&mut steps, 0, StepStatus::Done, "GitHub Releases");
            set_step(&mut steps, 1, StepStatus::Done, format!("桌面 {current}"));
            set_step(&mut steps, 2, StepStatus::Done, format!("当前桌面 {current}"));
            set_step(&mut steps, 3, StepStatus::Done, message.clone());
            report(app, &controller, UpdatePhase::Idle, message, None, &steps);
        }
        Ok(Some(update)) => {
            let version = update.version.clone();
            *lock_pending_desktop(&coordinator) = Some(version.clone());
            set_step(&mut steps, 0, StepStatus::Done, "GitHub Releases");
            set_step(&mut steps, 1, StepStatus::Done, format!("桌面 {version}"));
            set_step(&mut steps, 2, StepStatus::Done, format!("当前桌面 {current}"));
            set_step(&mut steps, 3, StepStatus::Done, format!("可安装 {version}"));
            report(
                app,
                &controller,
                UpdatePhase::Available,
                format!("发现桌面 {version}。可在菜单中安装并重启，进行中的会话会中断。"),
                Some(version),
                &steps,
            );
        }
        Err(error) => {
            set_step(&mut steps, 3, StepStatus::Failed, error.clone());
            fail_remaining(&mut steps);
            report(app, &controller, UpdatePhase::Failed, error, None, &steps);
        }
    }

    coordinator.finish();
    refresh_menu(app);
}

fn confirm_and_install_desktop(app: &AppHandle) {
    let coordinator = app.state::<UpdateCoordinator>();
    let Some(version) = lock_pending_desktop(&coordinator).clone() else {
        return;
    };
    let confirmed = app
        .dialog()
        .message(format!(
            "将安装桌面 {version} 并重启应用。进行中的会话会中断。"
        ))
        .title("安装桌面更新")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancel)
        .blocking_show();
    if !confirmed {
        return;
    }
    apply_desktop_update(app, &version);
}

fn apply_desktop_update(app: &AppHandle, expected: &str) {
    let coordinator = app.state::<UpdateCoordinator>();
    if !coordinator.try_begin() {
        return;
    }
    refresh_menu(app);
    let controller = app.state::<RuntimeController>();
    let mut steps = flow_from(&["下载", "安装", "重启"]);
    set_step(
        &mut steps,
        0,
        StepStatus::Active,
        format!("桌面 {expected}"),
    );
    report(
        app,
        &controller,
        UpdatePhase::Downloading,
        format!("正在下载桌面 {expected}。"),
        Some(expected.to_string()),
        &steps,
    );

    let result = tauri::async_runtime::block_on({
        let app = app.clone();
        let expected = expected.to_string();
        async move {
            let Some(update) = desktop_updater(&app)?
                .check()
                .await
                .map_err(classify_desktop_update_error)?
            else {
                return Err("没有可安装的桌面更新。".into());
            };
            if update.version != expected {
                return Err(format!(
                    "发布源版本已变为 {}，期望 {expected}。请重新检查。",
                    update.version
                ));
            }
            let mut read = 0u64;
            let progress_app = app.clone();
            let finish_app = app.clone();
            let finish_version = expected.clone();
            update
                .download_and_install(
                    move |chunk, total| {
                        read += chunk as u64;
                        progress_app
                            .state::<RuntimeController>()
                            .set_update_progress(read, total);
                        sync_update_toast(&progress_app);
                    },
                    move || {
                        let controller = finish_app.state::<RuntimeController>();
                        let steps = vec![
                            UpdateStep {
                                label: "下载".into(),
                                status: StepStatus::Done,
                                detail: "下载完成".into(),
                            },
                            UpdateStep {
                                label: "安装".into(),
                                status: StepStatus::Active,
                                detail: "正在安装".into(),
                            },
                            UpdateStep {
                                label: "重启".into(),
                                status: StepStatus::Pending,
                                detail: String::new(),
                            },
                        ];
                        report(
                            &finish_app,
                            &controller,
                            UpdatePhase::Installing,
                            format!("正在安装桌面 {finish_version}。"),
                            Some(finish_version),
                            &steps,
                        );
                    },
                )
                .await
                .map_err(classify_desktop_update_error)
        }
    });

    match result {
        Ok(()) => {
            set_step(&mut steps, 0, StepStatus::Done, "下载完成");
            set_step(&mut steps, 1, StepStatus::Done, "已安装");
            set_step(&mut steps, 2, StepStatus::Active, "正在重启应用");
            report(
                app,
                &controller,
                UpdatePhase::Installing,
                format!("正在重启到桌面 {expected}。"),
                Some(expected.to_string()),
                &steps,
            );
            coordinator.finish();
            refresh_menu(app);
            app.restart();
        }
        Err(error) => {
            set_step(&mut steps, 2, StepStatus::Failed, error.clone());
            fail_remaining(&mut steps);
            report(
                app,
                &controller,
                UpdatePhase::Failed,
                error,
                Some(expected.to_string()),
                &steps,
            );
            coordinator.finish();
            refresh_menu(app);
        }
    }
}

fn desktop_updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let handle = app.clone();
    let mut builder = app
        .updater_builder()
        .timeout(DESKTOP_UPDATE_TIMEOUT)
        .on_before_exit(move || {
            if let Some(controller) = handle.try_state::<RuntimeController>() {
                controller.shutdown();
            }
            handle.cleanup_before_exit();
        });
    if let Some(proxy) = desktop_update_proxy()? {
        builder = builder.proxy(proxy);
    }
    builder.build().map_err(classify_desktop_update_error)
}

fn desktop_update_proxy() -> Result<Option<tauri::Url>, String> {
    let Some(raw) = desktop_update_proxy_raw() else {
        return Ok(None);
    };
    tauri::Url::parse(&raw)
        .map(Some)
        .map_err(|error| format!("桌面更新代理地址无效（{raw}）：{error}"))
}

fn desktop_update_proxy_raw() -> Option<String> {
    env_proxy_raw().or_else(system_proxy_raw)
}

fn env_proxy_raw() -> Option<String> {
    const KEYS: &[&str] = &[
        "DSH_DESKTOP_UPDATE_PROXY",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ];
    KEYS.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn system_proxy_raw() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        if let Some(proxy) = macos_scutil_web_proxy() {
            return Some(proxy);
        }
    }

    let proxy = sysproxy::Sysproxy::get_system_proxy().ok()?;
    if !proxy.enable {
        return None;
    }
    let host = proxy.host.trim();
    if host.is_empty() || proxy.port == 0 {
        return None;
    }
    Some(format!("http://{host}:{}", proxy.port))
}

#[cfg(target_os = "macos")]
fn macos_scutil_web_proxy() -> Option<String> {
    let output = Command::new("scutil").arg("--proxy").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    proxy_url_from_scutil(&text)
}

#[cfg(target_os = "macos")]
fn proxy_url_from_scutil(text: &str) -> Option<String> {
    let https = scutil_enabled_proxy(text, "HTTPS");
    let http = scutil_enabled_proxy(text, "HTTP");
    https.or(http).map(|(host, port)| format!("http://{host}:{port}"))
}

#[cfg(target_os = "macos")]
fn scutil_enabled_proxy(text: &str, kind: &str) -> Option<(String, u16)> {
    let enable_key = format!("{kind}Enable");
    let host_key = format!("{kind}Proxy");
    let port_key = format!("{kind}Port");
    if scutil_bool(text, &enable_key) != Some(true) {
        return None;
    }
    let host = scutil_string(text, &host_key)?;
    let port = scutil_u16(text, &port_key)?;
    if host.is_empty() || port == 0 {
        return None;
    }
    Some((host, port))
}

#[cfg(target_os = "macos")]
fn scutil_bool(text: &str, key: &str) -> Option<bool> {
    let value = scutil_value(text, key)?;
    match value.as_str() {
        "1" | "true" | "True" | "YES" | "Yes" => Some(true),
        "0" | "false" | "False" | "NO" | "No" => Some(false),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn scutil_string(text: &str, key: &str) -> Option<String> {
    scutil_value(text, key).filter(|value| !value.is_empty())
}

#[cfg(target_os = "macos")]
fn scutil_u16(text: &str, key: &str) -> Option<u16> {
    scutil_value(text, key)?.parse().ok()
}

#[cfg(target_os = "macos")]
fn scutil_value(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == key {
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            return Some(value.to_string());
        }
    }
    None
}

fn classify_desktop_update_error(error: tauri_plugin_updater::Error) -> String {
    match error {
        tauri_plugin_updater::Error::ReleaseNotFound => {
            "还没有桌面更新清单。需要先发布带 updater 产物的桌面版本。".into()
        }
        tauri_plugin_updater::Error::TargetNotFound(_)
        | tauri_plugin_updater::Error::TargetsNotFound(_) => {
            "当前平台还没有对应的桌面更新包。".into()
        }
        other => {
            let text = other.to_string();
            let lower = text.to_ascii_lowercase();
            if lower.contains("404") {
                "还没有桌面更新清单。需要先发布带 updater 产物的桌面版本。".into()
            } else if lower.contains("timed out")
                || lower.contains("timeout")
                || lower.contains("error sending request")
            {
                "连接 GitHub Releases 超时。已尝试系统代理；仍失败可设 DSH_DESKTOP_UPDATE_PROXY，并确认 release 已上传 latest.json。".into()
            } else {
                format!("桌面更新失败：{}", classify_fetch_error(&text))
            }
        }
    }
}

pub fn prune_sidecars(app_data: &Path, state: &RuntimeState) -> Result<(), String> {
    let root = sidecar_root(app_data);
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(());
    };
    let mut keep = vec![sanitize_version(&state.active_version)];
    if let Some(previous) = &state.previous_version {
        keep.push(sanitize_version(previous));
    }
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if keep.iter().any(|item| item == name) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            let _ = fs::remove_dir_all(path);
        }
    }
    Ok(())
}

fn verify_installed_dsh(destination: &Path, version: &str) -> Result<(), String> {
    let package_path = destination.join("node_modules/@deepseek-ai/dsh/package.json");
    let package: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&package_path).map_err(|_| "安装结果缺少官方 package.json".to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let installed = package
        .get("version")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "安装结果缺少版本号".to_string())?;
    if installed != version {
        return Err(format!("安装到的 DSH 版本是 {installed}，期望 {version}"));
    }
    if !sidecar_is_complete(destination) {
        return Err("安装结果缺少 lib/bin.js".into());
    }
    Ok(())
}

fn npm_install_dsh(
    app: &AppHandle,
    selected: &SelectedRelease,
    destination: &Path,
    mut on_line: impl FnMut(String),
) -> Result<(), String> {
    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|error| error.to_string())?;
    let npm = resolve_npm(&resource_dir);
    let cache = scoped_runtime_root(
        &app.path()
            .app_cache_dir()
            .map_err(|error| error.to_string())?,
    )
    .join("npm");
    fs::create_dir_all(&cache).map_err(|error| error.to_string())?;

    let spec = format!("{OFFICIAL_DSH_NAME}@{}", selected.version);
    let mut command = Command::new(&npm.program);
    command.args(&npm.prefix_args);
    command.args([
        "install",
        spec.as_str(),
        "--omit=dev",
        "--no-audit",
        "--no-fund",
        "--no-package-lock",
        "--prefix",
    ]);
    command.arg(destination);
    command
        .env("NPM_CONFIG_REGISTRY", &selected.registry)
        .env("NPM_CONFIG_CACHE", &cache)
        .env("NPM_CONFIG_AUDIT", "false")
        .env("NPM_CONFIG_FUND", "false")
        .env("NPM_CONFIG_UPDATE_NOTIFIER", "false")
        .env("NO_UPDATE_NOTIFIER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动 npm：{error}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, rx) = mpsc::channel::<String>();
    let readers = [
        spawn_pipe_reader(stdout, tx.clone()),
        spawn_pipe_reader(stderr, tx),
    ];
    while let Ok(line) = rx.recv() {
        if !line.is_empty() {
            on_line(line);
        }
    }
    for reader in readers.into_iter().flatten() {
        let _ = reader.join();
    }
    let status = child
        .wait()
        .map_err(|error| format!("npm 安装失败：{error}"))?;
    if !status.success() {
        return Err(format!("npm 安装 {spec} 失败：{status}"));
    }
    Ok(())
}

fn spawn_pipe_reader(
    pipe: Option<impl std::io::Read + Send + 'static>,
    tx: mpsc::Sender<String>,
) -> Option<thread::JoinHandle<()>> {
    let pipe = pipe?;
    Some(thread::spawn(move || {
        let reader = BufReader::new(pipe);
        for line in reader.lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                let _ = tx.send(trimmed.to_string());
            }
        }
    }))
}

fn fetch_npm_latest(mut on_attempt: impl FnMut(String)) -> Result<NpmLatest, String> {
    let mut last_error = "无法获取 npm latest。".to_string();
    for url in npm_latest_urls() {
        let name = source_label(&url);
        on_attempt(format!("正在连接 {name}"));
        match fetch_text(&url, JSON_FETCH_TIMEOUT) {
            Ok(body) => match serde_json::from_str::<NpmVersionDocument>(&body) {
                Ok(document) if !document.version.is_empty() => {
                    on_attempt(format!("{name} 已获取 {}", document.version));
                    return Ok(NpmLatest {
                        version: document.version,
                        registry: registry_origin(&url),
                        node: document.engines.node,
                        source: name.to_string(),
                    });
                }
                Ok(_) => {
                    last_error = format!("{name} 未返回版本号");
                    on_attempt(last_error.clone());
                }
                Err(_) => {
                    last_error = format!("{name} 内容无法解析");
                    on_attempt(last_error.clone());
                }
            },
            Err(error) => {
                last_error = format!("{name} {}", classify_fetch_error(&error));
                on_attempt(last_error.clone());
            }
        }
    }
    Err(last_error)
}

fn fetch_text(url: &str, timeout: Duration) -> Result<String, String> {
    let response = http_agent(CONNECT_TIMEOUT, timeout)
        .get(url)
        .call()
        .map_err(|error| format!("无法获取 npm latest：{error}"))?;
    if response.status() == 404 {
        return Err("无法获取 npm latest：HTTP 404".into());
    }
    if !(200..300).contains(&response.status()) {
        return Err(format!("无法获取 npm latest：HTTP {}", response.status()));
    }
    response
        .into_string()
        .map_err(|error| format!("npm latest 读取失败：{error}"))
}

fn http_agent(connect: Duration, total: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(connect)
        .timeout(total)
        .user_agent(&format!(
            "deepseek-harness-desktop/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
}

fn npm_latest_urls() -> Vec<String> {
    if let Ok(registry) = std::env::var("DSH_DESKTOP_NPM_REGISTRY") {
        let registry = registry.trim_end_matches('/').to_string();
        return vec![format!("{registry}/{OFFICIAL_DSH_NAME}/latest")];
    }
    vec![
        format!("{DEFAULT_NPM_REGISTRY}/{OFFICIAL_DSH_NAME}/latest"),
        format!("{NPM_MIRROR_REGISTRY}/{OFFICIAL_DSH_NAME}/latest"),
    ]
}

fn registry_origin(url: &str) -> String {
    url.split("/@deepseek-ai/")
        .next()
        .unwrap_or(DEFAULT_NPM_REGISTRY)
        .trim_end_matches('/')
        .to_string()
}

fn source_label(url: &str) -> &str {
    if url.contains("npmmirror.com") {
        "npmmirror"
    } else if url.contains("registry.npmjs.org") {
        "npm"
    } else {
        "npm registry"
    }
}

fn classify_fetch_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("404") {
        "返回 404".into()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "连接超时".into()
    } else if let Some(rest) = error.split("HTTP ").nth(1) {
        let code = rest
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if code.is_empty() {
            "连接失败".into()
        } else {
            format!("HTTP {code}")
        }
    } else {
        "连接失败".into()
    }
}

pub(crate) fn flow_from(labels: &[&str]) -> Vec<UpdateStep> {
    labels
        .iter()
        .map(|label| UpdateStep {
            label: (*label).to_string(),
            status: StepStatus::Pending,
            detail: String::new(),
        })
        .collect()
}

pub(crate) fn set_step(steps: &mut [UpdateStep], index: usize, status: StepStatus, detail: impl Into<String>) {
    if let Some(step) = steps.get_mut(index) {
        step.status = status;
        step.detail = detail.into();
    }
}

pub(crate) fn fail_remaining(steps: &mut [UpdateStep]) {
    for step in steps {
        if step.status == StepStatus::Pending {
            step.status = StepStatus::Skipped;
        }
        if step.status == StepStatus::Active {
            step.status = StepStatus::Failed;
        }
    }
}

pub(crate) fn report(
    app: &AppHandle,
    controller: &RuntimeController,
    phase: UpdatePhase,
    message: impl Into<String>,
    available_version: Option<String>,
    steps: &[UpdateStep],
) {
    controller.set_update_flow(phase, message, available_version, steps.to_vec());
    sync_update_toast(app);
}

const TOAST_WIDTH: f64 = 360.0;
const TOAST_HEIGHT: f64 = 92.0;
const TOAST_RADIUS: f64 = 24.0;

pub fn create_update_toast(app: &tauri::App) -> tauri::Result<()> {
    let mut builder = WebviewWindowBuilder::new(
        app,
        "update-toast",
        WebviewUrl::App("toast.html".into()),
    )
    .title("DSH 更新")
    .inner_size(TOAST_WIDTH, TOAST_HEIGHT)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .closable(false)
    .decorations(false)
    .always_on_top(false)
    .skip_taskbar(true)
    .visible(false)
    .focused(false)
    .transparent(true)
    .background_color(Color(0, 0, 0, 0))
    .shadow(true);
    if let Some(main) = app.get_webview_window("main") {
        builder = builder.parent(&main)?;
    }
    let toast = builder.build()?;
    #[cfg(target_os = "macos")]
    clip_rounded_window(&toast, TOAST_RADIUS);
    let handle = app.handle().clone();
    if let Some(main) = app.get_webview_window("main") {
        main.on_window_event(move |event| {
            if matches!(
                event,
                tauri::WindowEvent::Resized(_)
                    | tauri::WindowEvent::Moved(_)
                    | tauri::WindowEvent::ScaleFactorChanged { .. }
            ) {
                sync_update_toast(&handle);
            }
        });
    }
    position_update_toast(app.handle(), &toast);
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn clip_rounded_window(window: &WebviewWindow, radius: f64) {
    let _ = window.with_webview(move |webview| unsafe {
        use objc2::runtime::{AnyObject, Bool};
        use objc2::{class, msg_send};

        let ns_window = webview.ns_window() as *mut AnyObject;
        let wk_webview = webview.inner() as *mut AnyObject;
        if ns_window.is_null() || wk_webview.is_null() {
            return;
        }

        let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![ns_window, setOpaque: Bool::NO];
        let _: () = msg_send![ns_window, setBackgroundColor: clear];
        let _: () = msg_send![wk_webview, setUnderPageBackgroundColor: clear];

        let key: *mut AnyObject =
            msg_send![class!(NSString), stringWithUTF8String: c"drawsBackground".as_ptr()];
        let no: *mut AnyObject = msg_send![class!(NSNumber), numberWithBool: Bool::NO];
        let _: () = msg_send![wk_webview, setValue: no, forKey: key];

        let content: *mut AnyObject = msg_send![ns_window, contentView];
        if content.is_null() {
            return;
        }
        let _: () = msg_send![content, setWantsLayer: Bool::YES];
        let layer: *mut AnyObject = msg_send![content, layer];
        if layer.is_null() {
            return;
        }
        let cg_clear: *mut AnyObject = msg_send![clear, CGColor];
        let _: () = msg_send![layer, setOpaque: Bool::NO];
        let _: () = msg_send![layer, setBackgroundColor: cg_clear];
        let _: () = msg_send![layer, setCornerRadius: radius];
        let _: () = msg_send![layer, setMasksToBounds: Bool::YES];
    });
}

pub fn dismiss_update_toast(app: &AppHandle) {
    let Some(toast) = app.get_webview_window("update-toast") else {
        return;
    };
    if toast_is_busy(&app.state::<RuntimeController>().status()) {
        return;
    }
    let _ = toast.hide();
}

pub fn sync_update_toast(app: &AppHandle) {
    let Some(toast) = app.get_webview_window("update-toast") else {
        return;
    };
    let snapshot = app.state::<RuntimeController>().status();
    if toast_should_show(&snapshot) {
        position_update_toast(app, &toast);
        let _ = toast.show();
    } else {
        let _ = toast.hide();
    }
}

fn toast_should_show(snapshot: &crate::runtime::RuntimeSnapshot) -> bool {
    toast_is_busy(snapshot)
        || matches!(
            snapshot.update_phase,
            UpdatePhase::Available | UpdatePhase::Failed
        )
        || (snapshot.update_phase == UpdatePhase::Idle && !snapshot.update_message.is_empty())
}

fn toast_is_busy(snapshot: &crate::runtime::RuntimeSnapshot) -> bool {
    matches!(
        snapshot.update_phase,
        UpdatePhase::Checking
            | UpdatePhase::Downloading
            | UpdatePhase::Verifying
            | UpdatePhase::Staging
            | UpdatePhase::Switching
            | UpdatePhase::RollingBack
            | UpdatePhase::PluginInstalling
            | UpdatePhase::PluginRemoving
            | UpdatePhase::PluginToggling
            | UpdatePhase::Installing
    )
}

fn position_update_toast(app: &AppHandle, toast: &WebviewWindow) {
    let Some(main) = app.get_webview_window("main") else {
        return;
    };
    let Ok(origin) = main.inner_position() else {
        return;
    };
    let Ok(inner) = main.inner_size() else {
        return;
    };
    let _ = toast.set_size(tauri::LogicalSize::new(TOAST_WIDTH, TOAST_HEIGHT));
    let scale = main.scale_factor().unwrap_or(1.0);
    let size = toast.outer_size().unwrap_or(tauri::PhysicalSize::new(
        (TOAST_WIDTH * scale) as u32,
        (TOAST_HEIGHT * scale) as u32,
    ));
    let margin = (18.0 * scale) as i32;
    let x = origin.x + inner.width as i32 - size.width as i32 - margin;
    let y = origin.y + inner.height as i32 - size.height as i32 - margin;
    let _ = toast.set_position(PhysicalPosition::new(
        x.max(origin.x + margin),
        y.max(origin.y + margin),
    ));
}

pub(crate) fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(scoped_runtime_root(
        &app.path()
            .app_data_dir()
            .map_err(|error| error.to_string())?,
    ))
}

fn lock_pending(coordinator: &UpdateCoordinator) -> std::sync::MutexGuard<'_, Option<SelectedRelease>> {
    coordinator
        .pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_pending_desktop(coordinator: &UpdateCoordinator) -> std::sync::MutexGuard<'_, Option<String>> {
    coordinator
        .pending_desktop
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn sanitize_version(version: &str) -> String {
    if Version::parse(version).is_err() {
        return "_invalid".to_string();
    }
    let sanitized: String = version
        .chars()
        .map(|item| {
            if item.is_ascii_alphanumeric() || item == '.' || item == '-' || item == '+' {
                item
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return "_invalid".to_string();
    }
    sanitized
}

fn parse_version(value: &str) -> Option<Version> {
    Version::parse(value).ok()
}

fn version_newer(left: &str, right: &str) -> bool {
    match (parse_version(left), parse_version(right)) {
        (Some(left), Some(right)) => left > right,
        _ => false,
    }
}

fn node_supported(node_version: &str, requirement: Option<&str>) -> bool {
    let Some(requirement) = requirement.filter(|value| !value.trim().is_empty()) else {
        return true;
    };
    let Ok(version) = Version::parse(node_version) else {
        return false;
    };
    requirement.split("||").any(|part| {
        VersionReq::parse(part.trim())
            .ok()
            .is_some_and(|req| req.matches(&version))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latest(version: &str, node: Option<&str>) -> NpmLatest {
        NpmLatest {
            version: version.into(),
            registry: DEFAULT_NPM_REGISTRY.into(),
            node: node.map(str::to_string),
            source: "npm".into(),
        }
    }

    #[test]
    fn toast_stays_visible_while_busy_or_reporting_a_result() {
        let checking = crate::runtime::RuntimeSnapshot {
            phase: crate::runtime::RuntimePhase::Ready,
            message: String::new(),
            package_spec: String::new(),
            dsh_version: "0.1.0-rc.6".into(),
            runtime_source: "bundled".into(),
            url: None,
            pid: None,
            recent_logs: Vec::new(),
            update_phase: UpdatePhase::Checking,
            update_message: "正在检查 npm latest。".into(),
            available_version: None,
            update_bytes_read: 0,
            update_bytes_total: 0,
            update_steps: Vec::new(),
        };
        assert!(toast_should_show(&checking));
        assert!(toast_is_busy(&checking));

        let mut idle = checking.clone();
        idle.update_phase = UpdatePhase::Idle;
        idle.update_message.clear();
        assert!(!toast_should_show(&idle));
        assert!(!toast_is_busy(&idle));

        idle.update_message = "当前已是 npm latest（DSH 0.1.0-rc.6）。".into();
        assert!(toast_should_show(&idle));

        let mut available = checking.clone();
        available.update_phase = UpdatePhase::Available;
        available.available_version = Some("0.1.0-rc.7".into());
        assert!(toast_should_show(&available));
        assert!(!toast_is_busy(&available));
    }

    #[test]
    fn names_npm_sources_and_classifies_fetch_errors() {
        assert_eq!(
            source_label("https://registry.npmjs.org/@deepseek-ai/dsh/latest"),
            "npm"
        );
        assert_eq!(
            source_label("https://registry.npmmirror.com/@deepseek-ai/dsh/latest"),
            "npmmirror"
        );
        assert_eq!(classify_fetch_error("无法获取 npm latest：HTTP 404"), "返回 404");
        assert_eq!(
            classify_fetch_error("Failed to connect: Connection timed out"),
            "连接超时"
        );
    }

    #[test]
    fn npm_latest_urls_prefer_official_then_mirror() {
        let urls = npm_latest_urls();
        assert_eq!(
            urls[0],
            "https://registry.npmjs.org/@deepseek-ai/dsh/latest"
        );
        assert!(urls.iter().any(|url| url.contains("npmmirror.com")));
        assert!(urls.iter().all(|url| url.starts_with("https://")));
    }

    #[test]
    fn registry_origin_strips_package_path() {
        assert_eq!(
            registry_origin("https://registry.npmmirror.com/@deepseek-ai/dsh/latest"),
            "https://registry.npmmirror.com"
        );
    }

    #[test]
    fn same_or_older_latest_is_current() {
        assert_eq!(
            select_npm_update(&latest("0.1.0-rc.6", Some("^22.19.0 || >=24.0.0")), "0.1.0-rc.6", "22.23.2"),
            UpdateCheck::Latest
        );
        assert_eq!(
            select_npm_update(&latest("0.1.0-rc.5", None), "0.1.0-rc.6", "22.23.2"),
            UpdateCheck::Latest
        );
    }

    #[test]
    fn newer_latest_is_available() {
        match select_npm_update(
            &latest("0.1.0-rc.7", Some("^22.19.0 || >=24.0.0")),
            "0.1.0-rc.6",
            "22.23.2",
        ) {
            UpdateCheck::Available(selected) => {
                assert_eq!(selected.version, "0.1.0-rc.7");
                assert_eq!(selected.registry, DEFAULT_NPM_REGISTRY);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn newer_latest_with_unsupported_node_needs_desktop() {
        match select_npm_update(&latest("0.1.0-rc.7", Some(">=24.0.0")), "0.1.0-rc.6", "22.23.2") {
            UpdateCheck::NeedsDesktop(message) => {
                assert!(message.contains("0.1.0-rc.7"));
                assert!(message.contains("22.23.2"));
                assert!(message.contains("检查桌面更新"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn desktop_update_errors_are_actionable() {
        assert_eq!(
            classify_desktop_update_error(tauri_plugin_updater::Error::ReleaseNotFound),
            "还没有桌面更新清单。需要先发布带 updater 产物的桌面版本。"
        );
        assert!(classify_desktop_update_error(tauri_plugin_updater::Error::TargetNotFound(
            "darwin-aarch64".into()
        ))
        .contains("当前平台"));
        assert!(classify_desktop_update_error(tauri_plugin_updater::Error::Network(
            "error sending request for url: timed out".into()
        ))
        .contains("系统代理"));
    }

    #[test]
    fn desktop_update_proxy_rejects_invalid_urls() {
        let error = tauri::Url::parse("not a proxy")
            .map_err(|error| format!("桌面更新代理地址无效（not a proxy）：{error}"))
            .unwrap_err();
        assert!(error.contains("无效"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scutil_prefers_https_web_proxy() {
        let text = r#"
<dictionary> {
  HTTPEnable : 1
  HTTPPort : 7890
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 1
  HTTPSPort : 10808
  HTTPSProxy : 127.0.0.1
  SOCKSEnable : 1
  SOCKSPort : 10808
  SOCKSProxy : 127.0.0.1
}
"#;
        assert_eq!(
            proxy_url_from_scutil(text).as_deref(),
            Some("http://127.0.0.1:10808")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scutil_falls_back_to_http_web_proxy() {
        let text = r#"
<dictionary> {
  HTTPEnable : 1
  HTTPPort : 7890
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 0
  HTTPSPort : 0
  HTTPSProxy :
}
"#;
        assert_eq!(
            proxy_url_from_scutil(text).as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scutil_ignores_disabled_web_proxy() {
        let text = r#"
<dictionary> {
  HTTPEnable : 0
  HTTPPort : 7890
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 0
  HTTPSPort : 7890
  HTTPSProxy : 127.0.0.1
  SOCKSEnable : 1
  SOCKSPort : 10808
  SOCKSProxy : 127.0.0.1
}
"#;
        assert_eq!(proxy_url_from_scutil(text), None);
    }

    #[test]
    fn sidecar_directory_rejects_parent_path_versions() {
        let root = Path::new("/tmp/dsh-app-data");
        assert_eq!(
            sidecar_app_root(root, "..").file_name().unwrap(),
            std::ffi::OsStr::new("_invalid")
        );
        assert_eq!(
            sidecar_app_root(root, "0.1.0-rc.7").file_name().unwrap(),
            std::ffi::OsStr::new("0.1.0-rc.7")
        );
    }

    #[test]
    fn node_range_accepts_bundled_node() {
        assert!(node_supported("22.23.2", Some("^22.19.0 || >=24.0.0")));
        assert!(!node_supported("20.0.0", Some("^22.19.0 || >=24.0.0")));
    }

    #[test]
    fn runtime_state_roundtrip_and_rollback() {
        let root = std::env::temp_dir().join(format!(
            "dsh-desktop-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let previous_root = sidecar_app_root(&root, "0.1.0-rc.6");
        fs::create_dir_all(sidecar_entry(&previous_root).parent().unwrap()).unwrap();
        fs::write(sidecar_entry(&previous_root), "entry").unwrap();

        save_runtime_state(
            &root,
            &RuntimeState {
                active_version: "0.1.0-rc.7".into(),
                previous_version: Some("0.1.0-rc.6".into()),
                source: RuntimeSource::Sidecar,
            },
        )
        .unwrap();

        let rolled = rollback_runtime_state(&root);
        assert_eq!(rolled.active_version, "0.1.0-rc.6");
        assert_eq!(rolled.source, RuntimeSource::Sidecar);

        let restored = restore_bundled_state(&root).unwrap();
        assert_eq!(restored.source, RuntimeSource::Bundled);
        assert_eq!(restored.active_version, BUNDLED_DSH_VERSION);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn verify_installed_dsh_checks_version_and_entry() {
        let root = std::env::temp_dir().join(format!(
            "dsh-desktop-npm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let package = root.join("node_modules/@deepseek-ai/dsh");
        fs::create_dir_all(package.join("lib")).unwrap();
        fs::write(package.join("package.json"), r#"{"version":"0.1.0-rc.7"}"#).unwrap();
        fs::write(package.join("lib/bin.js"), "entry").unwrap();
        verify_installed_dsh(&root, "0.1.0-rc.7").unwrap();

        fs::write(package.join("package.json"), r#"{"version":"0.1.0-rc.6"}"#).unwrap();
        let error = verify_installed_dsh(&root, "0.1.0-rc.7").unwrap_err();
        assert!(error.contains("0.1.0-rc.6"));
        let _ = fs::remove_dir_all(root);
    }
}
