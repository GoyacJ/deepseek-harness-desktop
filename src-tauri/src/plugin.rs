use crate::runtime::{
    bundled_node_executable, dsh_cli_command, resolve_current_launcher, scoped_runtime_root,
    RuntimeController,
};
use crate::update::{
    app_data_dir, fail_remaining, flow_from, refresh_menu, report, set_step, StepStatus,
    UpdateCoordinator, UpdatePhase,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use tauri::{
    AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    webview::Color,
};

const PLUGIN_WIDTH: f64 = 860.0;
const PLUGIN_HEIGHT: f64 = 720.0;
const PLUGIN_RADIUS: f64 = 20.0;
const PLUGIN_PROFILE: &str = "web";
const PLUGIN_WINDOW: &str = "plugin-manager";
const BUILTIN_BUNDLES: &[&str] = &["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"];
const HUB_API: &str = "https://dsh-hub.cc/api/v1";
const HUB_LIMIT: u32 = 20;
const USER_PLUGINS_FILE: &str = "user-plugins.json";
const DISABLE_PATCH_FILE: &str = "desktop.user-plugins.patch.yml";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct UserPlugin {
    pub name: String,
    pub version: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct HubPlugin {
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub description: String,
    pub stars: u64,
    pub forks: u64,
    pub category: String,
    pub homepage: String,
    pub repository_url: String,
    pub topics: Vec<String>,
    pub pushed_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct HubPluginDetail {
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub description: String,
    pub stars: u64,
    pub forks: u64,
    pub category: String,
    pub license: String,
    pub homepage: String,
    pub repository_url: String,
    pub topics: Vec<String>,
    pub package_name: String,
    pub version: String,
    pub pushed_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct HubPage {
    pub items: Vec<HubPlugin>,
    pub total: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct UserPluginState {
    #[serde(default)]
    disabled: Vec<String>,
}

#[derive(Deserialize)]
struct HubListResponse {
    #[serde(default)]
    items: Vec<HubListItem>,
    #[serde(default)]
    total: u32,
}

#[derive(Deserialize)]
struct HubListItem {
    #[serde(default)]
    owner: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "fullName", default)]
    full_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    stars: Option<u64>,
    #[serde(default)]
    forks: Option<u64>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(rename = "repositoryUrl", default)]
    repository_url: Option<String>,
    #[serde(default)]
    topics: Option<Vec<String>>,
    #[serde(rename = "pushedAt", default)]
    pushed_at: Option<String>,
}

#[derive(Deserialize)]
struct HubDetail {
    #[serde(default)]
    owner: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "fullName", default)]
    full_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    stars: Option<u64>,
    #[serde(default)]
    forks: Option<u64>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(rename = "repositoryUrl", default)]
    repository_url: Option<String>,
    #[serde(default)]
    topics: Option<Vec<String>>,
    #[serde(rename = "pushedAt", default)]
    pushed_at: Option<String>,
    manifest: Option<HubManifest>,
}

#[derive(Deserialize)]
struct HubManifest {
    name: Option<String>,
    version: Option<String>,
}

pub fn create_manager(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let mut builder = WebviewWindowBuilder::new(
        app,
        PLUGIN_WINDOW,
        WebviewUrl::App("plugin.html".into()),
    )
    .title("插件管理")
    .inner_size(PLUGIN_WIDTH, PLUGIN_HEIGHT)
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
    let dialog = builder.build()?;
    let _ = dialog.hide();
    #[cfg(target_os = "macos")]
    crate::update::clip_rounded_window(&dialog, PLUGIN_RADIUS);
    Ok(dialog)
}

pub fn open_manager(app: &AppHandle) {
    if app.state::<UpdateCoordinator>().is_busy() {
        return;
    }
    let dialog = match app.get_webview_window(PLUGIN_WINDOW) {
        Some(dialog) => dialog,
        None => match create_manager(app) {
            Ok(dialog) => dialog,
            Err(_) => return,
        },
    };
    resize_manager(app);
    let _ = dialog.show();
    let _ = dialog.set_focus();
    let _ = dialog.eval("typeof refreshAll === 'function' && refreshAll()");
}

pub fn dismiss_manager(app: &AppHandle) {
    if let Some(dialog) = app.get_webview_window(PLUGIN_WINDOW) {
        let _ = dialog.hide();
    }
}

pub fn is_manager_window(label: &str) -> bool {
    label == PLUGIN_WINDOW
}

pub fn resize_manager(app: &AppHandle) {
    let Some(dialog) = app.get_webview_window(PLUGIN_WINDOW) else {
        return;
    };
    let _ = dialog.set_size(tauri::LogicalSize::new(PLUGIN_WIDTH, PLUGIN_HEIGHT));
    #[cfg(target_os = "macos")]
    crate::update::clip_rounded_window(&dialog, PLUGIN_RADIUS);
    position_manager(app, &dialog, PLUGIN_WIDTH, PLUGIN_HEIGHT);
}

pub fn list_installed(app: &AppHandle) -> Result<Vec<UserPlugin>, String> {
    Ok(read_user_plugins(&app_data_dir(app)?))
}

pub fn search_hub(
    query: String,
    category: String,
    sort: String,
    offset: u32,
) -> Result<HubPage, String> {
    let query = query.trim();
    if query.len() > 80 {
        return Err("搜索词过长。".into());
    }
    let category = category.trim();
    if category.len() > 32 {
        return Err("分类无效。".into());
    }
    let sort = normalize_hub_sort(&sort)?;
    let mut url = format!(
        "{HUB_API}/plugins?scope=verified&sort={sort}&limit={HUB_LIMIT}&offset={offset}"
    );
    if !query.is_empty() {
        url.push_str("&q=");
        url.push_str(&query_encode(query));
    }
    if !category.is_empty() && category != "全部" {
        url.push_str("&category=");
        url.push_str(&query_encode(category));
    }
    let parsed: HubListResponse = hub_get(&url)?;
    Ok(HubPage {
        items: parsed
            .items
            .into_iter()
            .filter(|item| is_hub_segment(&item.owner) && is_hub_segment(&item.name))
            .map(|item| HubPlugin {
                owner: item.owner,
                name: item.name,
                full_name: item.full_name.unwrap_or_default(),
                description: item.description.unwrap_or_default(),
                stars: item.stars.unwrap_or(0),
                forks: item.forks.unwrap_or(0),
                category: item.category.unwrap_or_default(),
                homepage: item.homepage.unwrap_or_default(),
                repository_url: item.repository_url.unwrap_or_default(),
                topics: item.topics.unwrap_or_default(),
                pushed_at: item.pushed_at.unwrap_or_default(),
            })
            .collect(),
        total: parsed.total,
    })
}

pub fn submit_add(app: &AppHandle, spec: String) -> Result<(), String> {
    let spec = parse_registry_spec(&spec)?;
    let app = app.clone();
    std::thread::spawn(move || confirm_and_add(&app, spec));
    Ok(())
}

pub fn submit_remove(app: &AppHandle, spec: String) -> Result<(), String> {
    let name = registry_name(&spec)?;
    let app = app.clone();
    std::thread::spawn(move || confirm_and_remove(&app, name));
    Ok(())
}

pub fn submit_set_enabled(app: &AppHandle, spec: String, enabled: bool) -> Result<(), String> {
    let name = registry_name(&spec)?;
    let app = app.clone();
    std::thread::spawn(move || confirm_and_set_enabled(&app, name, enabled));
    Ok(())
}

pub fn submit_hub_install(app: &AppHandle, owner: String, name: String) -> Result<(), String> {
    if !is_hub_segment(&owner) || !is_hub_segment(&name) {
        return Err("插件标识无效。".into());
    }
    let spec = hub_registry_spec(&owner, &name)?;
    submit_add(app, spec)
}

pub fn get_hub(owner: String, name: String) -> Result<HubPluginDetail, String> {
    if !is_hub_segment(&owner) || !is_hub_segment(&name) {
        return Err("插件标识无效。".into());
    }
    let detail = fetch_hub_detail(&owner, &name)?;
    let manifest = detail.manifest.unwrap_or(HubManifest {
        name: None,
        version: None,
    });
    Ok(HubPluginDetail {
        owner: detail.owner,
        name: detail.name,
        full_name: detail.full_name.unwrap_or_default(),
        description: detail.description.unwrap_or_default(),
        stars: detail.stars.unwrap_or(0),
        forks: detail.forks.unwrap_or(0),
        category: detail.category.unwrap_or_default(),
        license: detail.license.unwrap_or_default(),
        homepage: detail.homepage.unwrap_or_default(),
        repository_url: detail.repository_url.unwrap_or_default(),
        topics: detail.topics.unwrap_or_default(),
        package_name: manifest.name.unwrap_or_default(),
        version: manifest.version.unwrap_or_default(),
        pushed_at: detail.pushed_at.unwrap_or_default(),
    })
}

pub fn parse_registry_spec(raw: &str) -> Result<String, String> {
    let spec = raw.trim();
    if spec.is_empty() {
        return Err("请输入 npm 包名。".into());
    }
    if spec.len() > 214 {
        return Err("包名过长。".into());
    }
    if spec.contains(char::is_whitespace)
        || spec.contains('\\')
        || spec.contains(':')
        || spec.contains("..")
        || spec.contains("//")
    {
        return Err("只支持 npm 包名，例如 dsh-recall 或 @anionex/dsh-vision-toolkit。".into());
    }
    let (name, version) = split_name_and_version(spec)?;
    if !is_package_name(name) {
        return Err("只支持 npm 包名，例如 dsh-recall 或 @anionex/dsh-vision-toolkit。".into());
    }
    if let Some(version) = version
        && !is_version_tag(version)
    {
        return Err("版本号无效。".into());
    }
    Ok(spec.to_string())
}

pub fn plugin_add_args(spec: &str) -> Vec<OsString> {
    plugin_args("add", spec)
}

pub fn plugin_remove_args(spec: &str) -> Vec<OsString> {
    plugin_args("remove", spec)
}

pub(crate) fn sync_disable_patch(dsh_home: &Path, app_data: &Path) -> Result<PathBuf, String> {
    let installed = read_user_plugins(app_data);
    let mut state = load_state(app_data);
    let installed_names: Vec<_> = installed.iter().map(|plugin| plugin.name.clone()).collect();
    state
        .disabled
        .retain(|name| installed_names.iter().any(|installed| installed == name));
    save_state(app_data, &state)?;
    let mut entries = Vec::new();
    for name in &state.disabled {
        for id in bundle_entry_ids(app_data, name) {
            if !entries.iter().any(|existing| existing == &id) {
                entries.push(id);
            }
        }
    }
    let path = dsh_home.join(DISABLE_PATCH_FILE);
    fs::create_dir_all(dsh_home).map_err(|error| error.to_string())?;
    fs::write(&path, render_disable_patch(&entries)).map_err(|error| error.to_string())?;
    Ok(path)
}

fn plugin_args(action: &str, spec: &str) -> Vec<OsString> {
    vec![
        OsString::from("plugin"),
        OsString::from("--profile"),
        OsString::from(PLUGIN_PROFILE),
        OsString::from(action),
        OsString::from(spec),
    ]
}

fn registry_name(raw: &str) -> Result<String, String> {
    let spec = parse_registry_spec(raw)?;
    Ok(split_name_and_version(&spec)?.0.to_string())
}

fn split_name_and_version(spec: &str) -> Result<(&str, Option<&str>), String> {
    if spec.starts_with('@') {
        let Some(slash) = spec.find('/') else {
            return Err("scoped 包名需要 @scope/name。".into());
        };
        let rest = &spec[slash + 1..];
        return match rest.split_once('@') {
            Some((name, version)) => Ok((&spec[..slash + 1 + name.len()], Some(version))),
            None => Ok((spec, None)),
        };
    }
    match spec.split_once('@') {
        Some((name, version)) => Ok((name, Some(version))),
        None => Ok((spec, None)),
    }
}

fn is_package_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix('@') {
        let Some((scope, pkg)) = rest.split_once('/') else {
            return false;
        };
        return is_name_part(scope) && is_name_part(pkg);
    }
    is_name_part(name)
}

fn is_name_part(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && first.is_ascii_alphanumeric()
        && chars.all(|item| item.is_ascii_lowercase() || item.is_ascii_digit() || matches!(item, '.' | '_' | '-' | '~'))
}

fn is_version_tag(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && chars.all(|item| item.is_ascii_alphanumeric() || matches!(item, '.' | '_' | '-' | '+' | '~'))
}

fn normalize_hub_sort(value: &str) -> Result<&'static str, String> {
    match value.trim() {
        "" | "trending" => Ok("trending"),
        "stars" => Ok("stars"),
        _ => Err("排序无效。".into()),
    }
}

fn is_hub_segment(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    value.len() <= 100
        && !value.contains("..")
        && !value.contains('/')
        && first.is_ascii_alphanumeric()
        && chars.all(|item| item.is_ascii_alphanumeric() || matches!(item, '.' | '_' | '-'))
}

fn confirm_and_add(app: &AppHandle, spec: String) {
    dismiss_manager(app);
    run_plugin_job(
        app,
        UpdatePhase::PluginInstalling,
        format!("正在添加插件 {spec}。"),
        &["准备 pnpm", "安装插件", "重启"],
        |ctx| {
            let extra_path = prepare_pnpm(ctx, UpdatePhase::PluginInstalling, &spec)?;
            set_step(
                &mut ctx.steps,
                1,
                StepStatus::Active,
                format!("dsh plugin add {spec}"),
            );
            ctx.report(UpdatePhase::PluginInstalling, format!("正在安装 {spec}。"));
            ctx.controller.pause()?;
            run_plugin_cli(
                &ctx.resource_dir,
                &ctx.app_data,
                &ctx.npm_cache,
                &extra_path,
                &plugin_add_args(&spec),
            )?;
            clear_disabled(&ctx.app_data, split_name_and_version(&spec)?.0)?;
            set_step(&mut ctx.steps, 1, StepStatus::Done, format!("已安装 {spec}"));
            Ok(format!("已添加 {spec}，正在重启。"))
        },
    );
}

fn confirm_and_remove(app: &AppHandle, name: String) {
    let Ok(app_data) = app_data_dir(app) else {
        return;
    };
    if !is_user_plugin(&app_data, &name) {
        return;
    }
    dismiss_manager(app);
    run_plugin_job(
        app,
        UpdatePhase::PluginRemoving,
        format!("正在删除插件 {name}。"),
        &["准备 pnpm", "卸载插件", "重启"],
        |ctx| {
            let extra_path = prepare_pnpm(ctx, UpdatePhase::PluginRemoving, &name)?;
            set_step(
                &mut ctx.steps,
                1,
                StepStatus::Active,
                format!("dsh plugin remove {name}"),
            );
            ctx.report(UpdatePhase::PluginRemoving, format!("正在卸载 {name}。"));
            ctx.controller.pause()?;
            run_plugin_cli(
                &ctx.resource_dir,
                &ctx.app_data,
                &ctx.npm_cache,
                &extra_path,
                &plugin_remove_args(&name),
            )?;
            clear_disabled(&ctx.app_data, &name)?;
            set_step(&mut ctx.steps, 1, StepStatus::Done, format!("已卸载 {name}"));
            Ok(format!("已删除 {name}，正在重启。"))
        },
    );
}

fn confirm_and_set_enabled(app: &AppHandle, name: String, enabled: bool) {
    let Ok(app_data) = app_data_dir(app) else {
        return;
    };
    if !is_user_plugin(&app_data, &name) {
        return;
    }
    let action = if enabled { "启用" } else { "停用" };
    dismiss_manager(app);
    let labels = [format!("{action}插件"), "重启".to_string()];
    run_plugin_job(
        app,
        UpdatePhase::PluginToggling,
        format!("正在{action}插件 {name}。"),
        &labels,
        |ctx| {
            set_step(
                &mut ctx.steps,
                0,
                StepStatus::Active,
                format!("正在{action} {name}"),
            );
            ctx.report(
                UpdatePhase::PluginToggling,
                format!("正在{action}插件 {name}。"),
            );
            if enabled {
                clear_disabled(&ctx.app_data, &name)?;
            } else {
                disable_plugin(&ctx.app_data, &name)?;
            }
            let ids = bundle_entry_ids(&ctx.app_data, &name);
            if !enabled && ids.is_empty() {
                return Err("无法读取该插件的条目，停用没有生效点。".into());
            }
            sync_disable_patch(&ctx.app_data.join("dsh"), &ctx.app_data)?;
            set_step(
                &mut ctx.steps,
                0,
                StepStatus::Done,
                format!("已{action} {name}"),
            );
            Ok(format!("已{action} {name}，正在重启。"))
        },
    );
}

struct JobContext<'a> {
    app: &'a AppHandle,
    controller: &'a RuntimeController,
    resource_dir: PathBuf,
    app_data: PathBuf,
    npm_cache: PathBuf,
    steps: Vec<crate::update::UpdateStep>,
}

impl JobContext<'_> {
    fn report(&self, phase: UpdatePhase, message: impl Into<String>) {
        report(
            self.app,
            self.controller,
            phase,
            message,
            None,
            &self.steps,
        );
    }
}

fn run_plugin_job(
    app: &AppHandle,
    start_phase: UpdatePhase,
    start_message: String,
    labels: &[impl AsRef<str>],
    work: impl FnOnce(&mut JobContext<'_>) -> Result<String, String>,
) {
    let coordinator = app.state::<UpdateCoordinator>();
    if !coordinator.try_begin() {
        return;
    }
    refresh_menu(app);
    let controller = app.state::<RuntimeController>();
    let labels: Vec<&str> = labels.iter().map(|label| label.as_ref()).collect();
    let mut steps = flow_from(&labels);
    set_step(&mut steps, 0, StepStatus::Active, start_message.clone());
    report(app, &controller, start_phase, start_message, None, &steps);

    let result = (|| {
        let resource_dir = app
            .path()
            .resource_dir()
            .map_err(|error| error.to_string())?;
        let app_data = app_data_dir(app)?;
        let npm_cache = scoped_runtime_root(
            &app.path()
                .app_cache_dir()
                .map_err(|error| error.to_string())?,
        )
        .join("npm");
        fs::create_dir_all(&npm_cache).map_err(|error| error.to_string())?;
        let mut ctx = JobContext {
            app,
            controller: &controller,
            resource_dir,
            app_data,
            npm_cache,
            steps,
        };
        let done = work(&mut ctx)?;
        Ok::<_, String>((ctx.steps, done))
    })();

    match result {
        Ok((mut steps, done)) => {
            let restart_index = steps.len().saturating_sub(1);
            set_step(
                &mut steps,
                restart_index,
                StepStatus::Active,
                "正在重启以加载插件",
            );
            report(app, &controller, start_phase, done, None, &steps);
            coordinator.finish();
            refresh_menu(app);
            if let Err(error) = controller.restart() {
                set_step(&mut steps, restart_index, StepStatus::Failed, error.clone());
                report(app, &controller, UpdatePhase::Failed, error, None, &steps);
                refresh_menu(app);
            }
        }
        Err(error) => {
            let paused = matches!(
                controller.status().phase,
                crate::runtime::RuntimePhase::Stopping
            );
            let mut steps = flow_from(&labels);
            fail_remaining(&mut steps);
            report(app, &controller, UpdatePhase::Failed, error, None, &steps);
            coordinator.finish();
            refresh_menu(app);
            if paused {
                let _ = controller.restart();
            }
        }
    }
}

fn prepare_pnpm(
    ctx: &mut JobContext<'_>,
    phase: UpdatePhase,
    spec: &str,
) -> Result<Vec<PathBuf>, String> {
    set_step(&mut ctx.steps, 0, StepStatus::Active, "正在准备 pnpm");
    ctx.report(phase, format!("正在处理插件 {spec}。"));
    let resource_dir = ctx.resource_dir.clone();
    let app_data = ctx.app_data.clone();
    let pnpm_dir = ensure_pnpm(&resource_dir, &app_data, |note| {
        set_step(&mut ctx.steps, 0, StepStatus::Active, note);
        report(
            ctx.app,
            ctx.controller,
            phase,
            format!("正在处理插件 {spec}。"),
            None,
            &ctx.steps,
        );
    })?;
    set_step(&mut ctx.steps, 0, StepStatus::Done, "pnpm 已就绪");
    Ok(pnpm_dir.into_iter().collect())
}

fn run_plugin_cli(
    resource_dir: &Path,
    app_data: &Path,
    npm_cache: &Path,
    extra_path_dirs: &[PathBuf],
    args: &[OsString],
) -> Result<(), String> {
    let launcher = resolve_current_launcher(resource_dir, app_data);
    let dsh_home = app_data.join("dsh");
    fs::create_dir_all(&dsh_home).map_err(|error| error.to_string())?;
    let mut command = dsh_cli_command(&launcher, &dsh_home, npm_cache, extra_path_dirs, args);
    if let Ok(registry) = env::var("DSH_DESKTOP_NPM_REGISTRY") {
        command.env("npm_config_registry", registry);
    }
    let output = command
        .output()
        .map_err(|error| format!("无法启动 dsh plugin：{error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .find(|text| !text.is_empty())
        .unwrap_or("pnpm 安装失败");
    let last = detail
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(detail);
    Err(truncate(last, 240))
}

fn ensure_pnpm(
    resource_dir: &Path,
    app_data: &Path,
    mut note: impl FnMut(&str),
) -> Result<Option<PathBuf>, String> {
    if pnpm_available(&[]) {
        note("使用系统 pnpm");
        return Ok(None);
    }
    let Some(node) = bundled_node_executable(resource_dir) else {
        return Err("未找到 pnpm，且没有随包 Node 可启用 corepack。".into());
    };
    let shim_dir = app_data.join("bin");
    fs::create_dir_all(&shim_dir).map_err(|error| error.to_string())?;
    if pnpm_available(&[shim_dir.clone()]) {
        note("使用已启用的 pnpm");
        return Ok(Some(shim_dir));
    }
    note("正在通过 corepack 启用 pnpm");
    let corepack = corepack_executable(&node)
        .ok_or_else(|| "随包 Node 没有 corepack，无法启用 pnpm。".to_string())?;
    let status = Command::new(&corepack)
        .args(["enable", "--install-directory"])
        .arg(&shim_dir)
        .env(
            "PATH",
            crate_path_with(&[node.parent().unwrap_or(node.as_path())]),
        )
        .stdin(Stdio::null())
        .status()
        .map_err(|error| format!("无法运行 corepack：{error}"))?;
    if !status.success() {
        return Err("corepack 启用 pnpm 失败。".into());
    }
    if !pnpm_available(&[shim_dir.clone()]) {
        return Err("已启用 corepack，但仍找不到 pnpm。".into());
    }
    Ok(Some(shim_dir))
}

fn pnpm_available(extra_dirs: &[PathBuf]) -> bool {
    Command::new(pnpm_program())
        .arg("--version")
        .env("PATH", crate_path_with(extra_dirs))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn pnpm_program() -> &'static str {
    if cfg!(windows) { "pnpm.cmd" } else { "pnpm" }
}

fn corepack_executable(node: &Path) -> Option<PathBuf> {
    let parent = node.parent()?;
    let candidate = if cfg!(windows) {
        parent.join("corepack.cmd")
    } else {
        parent.join("corepack")
    };
    candidate.is_file().then_some(candidate)
}

fn crate_path_with(extra_dirs: &[impl AsRef<Path>]) -> OsString {
    let mut dirs: Vec<PathBuf> = extra_dirs.iter().map(|path| path.as_ref().to_path_buf()).collect();
    if let Some(current) = env::var_os("PATH") {
        dirs.extend(env::split_paths(&current));
    }
    env::join_paths(dirs).unwrap_or_else(|_| env::var_os("PATH").unwrap_or_default())
}

fn position_manager(app: &AppHandle, dialog: &WebviewWindow, width: f64, height: f64) {
    let Some(main) = app.get_webview_window("main") else {
        return;
    };
    let Ok(origin) = main.inner_position() else {
        return;
    };
    let Ok(inner) = main.inner_size() else {
        return;
    };
    let _ = dialog.set_size(tauri::LogicalSize::new(width, height));
    let scale = main.scale_factor().unwrap_or(1.0);
    let size = dialog.outer_size().unwrap_or(tauri::PhysicalSize::new(
        (width * scale) as u32,
        (height * scale) as u32,
    ));
    let x = origin.x + (inner.width as i32 - size.width as i32) / 2;
    let y = origin.y + (inner.height as i32 - size.height as i32) / 3;
    let _ = dialog.set_position(PhysicalPosition::new(x, y));
}

fn profile_manifest(app_data: &Path) -> Option<Value> {
    let path = app_data.join("dsh/profiles/web/package.json");
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn read_user_plugins(app_data: &Path) -> Vec<UserPlugin> {
    let Some(manifest) = profile_manifest(app_data) else {
        return Vec::new();
    };
    let bundles = manifest
        .pointer("/dsh/profile/bundles")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let dependencies = manifest.get("dependencies").cloned().unwrap_or(Value::Object(Default::default()));
    let disabled = load_state(app_data).disabled;
    bundles
        .iter()
        .filter_map(Value::as_str)
        .filter(|name| !BUILTIN_BUNDLES.contains(name))
        .map(|name| UserPlugin {
            version: dependency_version(&dependencies, name),
            enabled: !disabled.iter().any(|item| item == name),
            name: name.to_string(),
        })
        .collect()
}

fn dependency_version(dependencies: &Value, name: &str) -> String {
    dependencies
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_start_matches(|item| item == '^' || item == '~' || item == '=')
        .to_string()
}

fn is_user_plugin(app_data: &Path, name: &str) -> bool {
    read_user_plugins(app_data)
        .iter()
        .any(|plugin| plugin.name == name)
}

fn state_path(app_data: &Path) -> PathBuf {
    app_data.join(USER_PLUGINS_FILE)
}

fn load_state(app_data: &Path) -> UserPluginState {
    fs::read_to_string(state_path(app_data))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_state(app_data: &Path, state: &UserPluginState) -> Result<(), String> {
    fs::create_dir_all(app_data).map_err(|error| error.to_string())?;
    fs::write(
        state_path(app_data),
        serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn disable_plugin(app_data: &Path, name: &str) -> Result<(), String> {
    let mut state = load_state(app_data);
    if !state.disabled.iter().any(|item| item == name) {
        state.disabled.push(name.to_string());
    }
    save_state(app_data, &state)
}

fn clear_disabled(app_data: &Path, name: &str) -> Result<(), String> {
    let mut state = load_state(app_data);
    state.disabled.retain(|item| item != name);
    save_state(app_data, &state)
}

fn bundle_entry_ids(app_data: &Path, package: &str) -> Vec<String> {
    let pkg_dir = app_data
        .join("dsh/profiles/web/node_modules")
        .join(package);
    let Ok(text) = fs::read_to_string(pkg_dir.join("package.json")) else {
        return Vec::new();
    };
    let Ok(manifest) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let Some(rel) = manifest.pointer("/dsh/bundle/patch").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Ok(yaml) = fs::read_to_string(pkg_dir.join(rel)) else {
        return Vec::new();
    };
    collect_entry_ids(&yaml)
}

fn collect_entry_ids(yaml: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let rest = if let Some(rest) = trimmed.strip_prefix("- id:") {
            rest
        } else if let Some(rest) = trimmed.strip_prefix("id:") {
            rest
        } else {
            continue;
        };
        let value = unquote_yaml(rest.trim());
        if !value.is_empty() && !ids.iter().any(|item| item == &value) {
            ids.push(value);
        }
    }
    ids
}

fn unquote_yaml(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        return value[1..value.len() - 1].to_string();
    }
    value
        .split('#')
        .next()
        .unwrap_or(value)
        .trim()
        .trim_end_matches(',')
        .to_string()
}

fn render_disable_patch(ids: &[String]) -> String {
    if ids.is_empty() {
        return "# Generated by DeepSeek Harness Desktop.\n[]\n".into();
    }
    let mut out = String::from("# Generated by DeepSeek Harness Desktop.\n");
    for id in ids {
        out.push_str("- id: ");
        out.push_str(&yaml_plain(id));
        out.push_str("\n  disabled: true\n");
    }
    out
}

fn yaml_plain(value: &str) -> String {
    if value.chars().all(|item| item.is_ascii_alphanumeric() || matches!(item, '-' | '_' | '.')) {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('"', "\\\""))
    }
}

fn hub_registry_spec(owner: &str, name: &str) -> Result<String, String> {
    let spec = fetch_hub_detail(owner, name)?
        .manifest
        .and_then(|manifest| manifest.name)
        .ok_or_else(|| "这个插件没有可安装的 npm 包名。".to_string())?;
    parse_registry_spec(&spec)
}

fn fetch_hub_detail(owner: &str, name: &str) -> Result<HubDetail, String> {
    let url = format!(
        "{HUB_API}/plugins/{}/{}",
        query_encode(owner),
        query_encode(name)
    );
    hub_get(&url)
}

fn hub_get<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, String> {
    let response = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .user_agent(&format!(
            "deepseek-harness-desktop/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .get(url)
        .call()
        .map_err(|error| format!("无法连接插件市场：{error}"))?;
    if !(200..300).contains(&response.status()) {
        return Err(format!("插件市场返回 HTTP {}", response.status()));
    }
    let body = response
        .into_string()
        .map_err(|error| format!("插件市场读取失败：{error}"))?;
    serde_json::from_str(&body).map_err(|_| "插件市场数据无效。".into())
}

fn query_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut clipped: String = trimmed.chars().take(max.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_specs_from_official_add_commands_are_accepted() {
        assert_eq!(
            parse_registry_spec(" @anionex/dsh-vision-toolkit ").unwrap(),
            "@anionex/dsh-vision-toolkit"
        );
        assert_eq!(parse_registry_spec("dsh-recall").unwrap(), "dsh-recall");
        assert_eq!(
            parse_registry_spec("dsh-recall@1.2.3").unwrap(),
            "dsh-recall@1.2.3"
        );
        assert_eq!(
            parse_registry_spec("@anionex/dsh-vision-toolkit@0.1.0").unwrap(),
            "@anionex/dsh-vision-toolkit@0.1.0"
        );
    }

    #[test]
    fn non_registry_specs_are_rejected() {
        for spec in [
            "",
            ".",
            "../plugin",
            "file:./plugin",
            "git+https://github.com/a/b.git",
            "github:a/b",
            "@scope",
            "UpperCase",
            "dsh recall",
            "dsh-recall@1.0.0@beta",
        ] {
            assert!(parse_registry_spec(spec).is_err(), "{spec}");
        }
    }

    #[test]
    fn plugin_add_args_match_official_cli() {
        let args = plugin_add_args("@anionex/dsh-vision-toolkit");
        assert_eq!(
            args,
            [
                OsString::from("plugin"),
                OsString::from("--profile"),
                OsString::from("web"),
                OsString::from("add"),
                OsString::from("@anionex/dsh-vision-toolkit"),
            ]
        );
    }

    #[test]
    fn plugin_remove_args_match_official_cli() {
        let args = plugin_remove_args("dsh-recall");
        assert_eq!(
            args,
            [
                OsString::from("plugin"),
                OsString::from("--profile"),
                OsString::from("web"),
                OsString::from("remove"),
                OsString::from("dsh-recall"),
            ]
        );
    }

    #[test]
    fn bundle_patch_ids_come_from_insert_rows() {
        let yaml = concat!(
            "- insert:\n",
            "    - id: vision-toolkit\n",
            "      name: '@anionex/dsh-vision-toolkit'\n",
            "    - id: vision-toolkit-client\n",
            "      name: '@anionex/dsh-vision-toolkit/client'\n",
        );
        assert_eq!(
            collect_entry_ids(yaml),
            ["vision-toolkit", "vision-toolkit-client"]
        );
    }

    #[test]
    fn disable_patch_is_id_targeted_and_empty_is_a_list() {
        assert_eq!(
            render_disable_patch(&[]),
            "# Generated by DeepSeek Harness Desktop.\n[]\n"
        );
        assert_eq!(
            render_disable_patch(&["vision-toolkit".into()]),
            "# Generated by DeepSeek Harness Desktop.\n- id: vision-toolkit\n  disabled: true\n"
        );
    }

    #[test]
    fn builtin_bundles_are_not_user_plugins() {
        let manifest = serde_json::json!({
            "dependencies": {
                "@anionex/dsh-vision-toolkit": "^0.1.9"
            },
            "dsh": {
                "profile": {
                    "bundles": [
                        "@deepseek-ai/dsh-base",
                        "@deepseek-ai/dsh-web-app",
                        "@anionex/dsh-vision-toolkit"
                    ]
                }
            }
        });
        let bundles = manifest["dsh"]["profile"]["bundles"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .filter(|name| !BUILTIN_BUNDLES.contains(name))
            .collect::<Vec<_>>();
        assert_eq!(bundles, ["@anionex/dsh-vision-toolkit"]);
    }

    #[test]
    fn hub_list_item_accepts_null_optional_fields() {
        let item: HubListItem = serde_json::from_str(
            r#"{"owner":"Anionex","name":"dsh-vision-toolkit","fullName":null,"description":null,"stars":null,"category":null}"#,
        )
        .unwrap();
        assert_eq!(item.owner, "Anionex");
        assert_eq!(item.name, "dsh-vision-toolkit");
        assert!(item.description.unwrap_or_default().is_empty());
        assert_eq!(item.stars.unwrap_or(0), 0);
    }

    #[test]
    fn hub_segments_reject_paths() {
        assert!(is_hub_segment("Anionex"));
        assert!(is_hub_segment("dsh-vision-toolkit"));
        assert!(!is_hub_segment("a/b"));
        assert!(!is_hub_segment(".."));
        assert!(!is_hub_segment(""));
    }

    #[test]
    fn query_encode_keeps_chinese_categories_safe() {
        assert_eq!(query_encode("记忆"), "%E8%AE%B0%E5%BF%86");
        assert_eq!(query_encode("dsh-recall"), "dsh-recall");
    }

    #[test]
    fn hub_sort_accepts_trending_and_stars() {
        assert_eq!(normalize_hub_sort("").unwrap(), "trending");
        assert_eq!(normalize_hub_sort("trending").unwrap(), "trending");
        assert_eq!(normalize_hub_sort("stars").unwrap(), "stars");
        assert!(normalize_hub_sort("forks").is_err());
    }
}
