use serde::Serialize;
use std::{
    env,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, Url, WebviewWindow};

const OFFICIAL_DSH_PACKAGE: &str = "@deepseek-ai/dsh@0.1.0-rc.6";
const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 300;
const MAX_LOG_LINES: usize = 180;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePhase {
    Starting,
    Ready,
    Failed,
    Stopping,
    Stopped,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSnapshot {
    pub phase: RuntimePhase,
    pub message: String,
    pub package_spec: String,
    pub url: Option<String>,
    pub pid: Option<u32>,
    pub recent_logs: Vec<String>,
}

impl RuntimeSnapshot {
    fn initial(package_spec: String) -> Self {
        Self {
            phase: RuntimePhase::Starting,
            message: "正在准备官方 DSH 运行时。".into(),
            package_spec,
            url: None,
            pid: None,
            recent_logs: Vec::new(),
        }
    }
}

pub struct RuntimeController {
    commands: Sender<SupervisorCommand>,
    snapshot: SharedSnapshot,
    shutdown_started: AtomicBool,
}

impl RuntimeController {
    pub fn start(
        app: &AppHandle,
        window: WebviewWindow,
        landing_url: Url,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let config_started = Instant::now();
        let config = RuntimeConfig::resolve(app)?;
        let config_elapsed = config_started.elapsed();
        let snapshot = Arc::new(Mutex::new(RuntimeSnapshot::initial(
            config.package_spec.clone(),
        )));
        let (commands, receiver) = mpsc::channel();
        install_termination_handler(commands.clone())?;

        let thread_snapshot = snapshot.clone();
        thread::Builder::new()
            .name("dsh-runtime-supervisor".into())
            .spawn(move || {
                supervisor_loop(
                    config,
                    receiver,
                    thread_snapshot,
                    window,
                    landing_url,
                    config_elapsed,
                )
            })?;

        Ok(Self {
            commands,
            snapshot,
            shutdown_started: AtomicBool::new(false),
        })
    }

    pub fn status(&self) -> RuntimeSnapshot {
        lock_snapshot(&self.snapshot).clone()
    }

    pub fn owns_ready_url(&self, url: &Url) -> bool {
        let snapshot = lock_snapshot(&self.snapshot);
        let Some(runtime_url) = snapshot
            .url
            .as_deref()
            .and_then(|value| Url::parse(value).ok())
        else {
            return false;
        };
        same_http_origin(&runtime_url, url)
    }

    pub fn restart(&self) -> Result<(), String> {
        self.commands
            .send(SupervisorCommand::Restart)
            .map_err(|_| "DSH 监督器已经停止".to_string())
    }

    pub fn shutdown(&self) {
        self.request_shutdown();
    }

    fn request_shutdown(&self) {
        if self.shutdown_started.swap(true, Ordering::SeqCst) {
            return;
        }

        let (acknowledge, completed) = mpsc::channel();
        if self
            .commands
            .send(SupervisorCommand::Shutdown(acknowledge))
            .is_ok()
        {
            let _ = completed.recv_timeout(Duration::from_secs(7));
        }
    }
}

impl Drop for RuntimeController {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

fn install_termination_handler(commands: Sender<SupervisorCommand>) -> Result<(), ctrlc::Error> {
    ctrlc::set_handler(move || {
        let (acknowledge, completed) = mpsc::channel();
        if commands
            .send(SupervisorCommand::Shutdown(acknowledge))
            .is_ok()
        {
            let _ = completed.recv_timeout(Duration::from_secs(7));
        }
        std::process::exit(130);
    })
}

type SharedSnapshot = Arc<Mutex<RuntimeSnapshot>>;

enum SupervisorCommand {
    Restart,
    Shutdown(Sender<()>),
}

#[derive(Clone, Debug)]
struct Launcher {
    program: PathBuf,
    prefix_args: Vec<OsString>,
    description: String,
    uses_npx: bool,
}

#[derive(Clone, Debug)]
struct RuntimeConfig {
    package_spec: String,
    launcher: Launcher,
    dsh_home: PathBuf,
    npm_cache: PathBuf,
    workspace: PathBuf,
    patch: Option<PathBuf>,
    startup_timeout: Duration,
}

impl RuntimeConfig {
    fn resolve(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let app_data = scoped_runtime_root(&app.path().app_data_dir()?);
        let app_cache = scoped_runtime_root(&app.path().app_cache_dir()?);
        let resource_dir = app.path().resource_dir()?;
        let dsh_home = app_data.join("dsh");
        let npm_cache = app_cache.join("npm");
        let workspace = env::var_os("DSH_DESKTOP_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|| app_data.join("workspace"));

        fs::create_dir_all(&dsh_home)?;
        fs::create_dir_all(&npm_cache)?;
        fs::create_dir_all(&workspace)?;

        let package_spec =
            env::var("DSH_DESKTOP_PACKAGE").unwrap_or_else(|_| OFFICIAL_DSH_PACKAGE.to_string());
        let launcher = resolve_launcher(&resource_dir, &package_spec);
        let patch = if env::var_os("DSH_DESKTOP_DISABLE_PLUGIN").is_some() {
            None
        } else {
            let plugin = resolve_desktop_plugin(&resource_dir)?;
            Some(write_generated_patch(&dsh_home, &plugin, &package_spec)?)
        };
        let startup_timeout = env::var("DSH_DESKTOP_STARTUP_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| *seconds > 0)
            .unwrap_or(DEFAULT_STARTUP_TIMEOUT_SECS);

        Ok(Self {
            package_spec,
            launcher,
            dsh_home,
            npm_cache,
            workspace,
            patch,
            startup_timeout: Duration::from_secs(startup_timeout),
        })
    }

    fn command(&self, port: u16) -> Command {
        let mut command = Command::new(&self.launcher.program);
        if self.launcher.uses_npx {
            command.arg("--cache").arg(&self.npm_cache);
        }
        command.args(&self.launcher.prefix_args);
        command.args(dsh_arguments(self.patch.as_deref(), port));
        if !self.launcher.uses_npx
            && let Some(path) = bundled_path_env(&self.launcher.program)
        {
            command.env("PATH", path);
        }
        command
            .current_dir(&self.workspace)
            .env("DSH_HOME", &self.dsh_home)
            .env("NPM_CONFIG_CACHE", &self.npm_cache)
            .env("NPM_CONFIG_AUDIT", "false")
            .env("NPM_CONFIG_FUND", "false")
            .env("NPM_CONFIG_UPDATE_NOTIFIER", "false")
            .env("NO_UPDATE_NOTIFIER", "1")
            .env("DSH_DESKTOP_PARENT_PID", std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }

        command
    }
}

fn scoped_runtime_root(base: &Path) -> PathBuf {
    if cfg!(debug_assertions) {
        base.join("development")
    } else {
        base.to_path_buf()
    }
}

fn bundled_path_env(program: &Path) -> Option<OsString> {
    let node_directory = program.parent()?;
    let mut paths = vec![node_directory.to_path_buf()];
    if let Some(current) = env::var_os("PATH") {
        paths.extend(env::split_paths(&current));
    }
    env::join_paths(paths).ok()
}

fn resolve_launcher(resource_dir: &Path, package_spec: &str) -> Launcher {
    let packaged_root = resource_dir.join("resources/dsh-runtime");
    let development_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/dsh-runtime");
    let packaged_bootstrap = resource_dir.join("resources/runtime-bootstrap.mjs");
    let development_bootstrap =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/runtime-bootstrap.mjs");
    let mut bundled_candidates = Vec::with_capacity(2);
    if cfg!(debug_assertions) {
        bundled_candidates.push((
            development_root,
            development_bootstrap,
            "开发目录中的官方 DSH npm 运行时",
        ));
    }
    bundled_candidates.push((
        packaged_root,
        packaged_bootstrap,
        "随安装包交付的官方 DSH npm 运行时",
    ));

    for (bundled_root, bootstrap, description) in bundled_candidates {
        let bundled_node = if cfg!(windows) {
            bundled_root.join("node/node.exe")
        } else {
            bundled_root.join("node/bin/node")
        };
        let bundled_entry = bundled_root.join("app/node_modules/@deepseek-ai/dsh/lib/bin.js");

        if bundled_node.is_file() && bundled_entry.is_file() && bootstrap.is_file() {
            return Launcher {
                program: bundled_node,
                prefix_args: vec![bootstrap.into_os_string(), bundled_entry.into_os_string()],
                description: description.into(),
                uses_npx: false,
            };
        }
    }

    let npx = env::var_os("DSH_DESKTOP_NPX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "npx.cmd" } else { "npx" }));
    Launcher {
        program: npx,
        prefix_args: vec![OsString::from("--yes"), OsString::from(package_spec)],
        description: "系统 npx + 固定官方 DSH 版本".into(),
        uses_npx: true,
    }
}

fn resolve_desktop_plugin(resource_dir: &Path) -> Result<PathBuf, std::io::Error> {
    let candidates = [
        resource_dir.join("resources/desktop-plugin/index.mjs"),
        resource_dir.join("desktop-plugin/index.mjs"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/desktop-plugin/index.mjs"),
    ];

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "desktop DSH plugin resource is missing",
            )
        })
}

fn write_generated_patch(
    dsh_home: &Path,
    plugin_path: &Path,
    package_spec: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let patch_path = dsh_home.join("desktop.generated.patch.yml");
    let plugin_path = serde_json::to_string(&plugin_path.to_string_lossy())?;
    let package_spec = serde_json::to_string(package_spec)?;
    let desktop_version = serde_json::to_string(env!("CARGO_PKG_VERSION"))?;
    let contents = format!(
        concat!(
            "# Generated by DeepSeek Harness Desktop.\n",
            "- insert:\n",
            "    - id: desktop-runtime\n",
            "      name: {plugin_path}\n",
            "      config:\n",
            "        desktopVersion: {desktop_version}\n",
            "        dshPackage: {package_spec}\n",
            "        desktopPid: {desktop_pid}\n",
        ),
        plugin_path = plugin_path,
        desktop_version = desktop_version,
        package_spec = package_spec,
        desktop_pid = std::process::id(),
    );
    fs::write(&patch_path, contents)?;
    Ok(patch_path)
}

fn dsh_arguments(patch: Option<&Path>, port: u16) -> Vec<OsString> {
    let mut arguments = vec![OsString::from("web")];
    if let Some(patch) = patch {
        arguments.push(OsString::from("--patch"));
        arguments.push(patch.as_os_str().to_owned());
    }
    arguments.extend([
        OsString::from("--host"),
        OsString::from("127.0.0.1"),
        OsString::from("--port"),
        OsString::from(port.to_string()),
    ]);
    arguments
}

fn supervisor_loop(
    config: RuntimeConfig,
    commands: Receiver<SupervisorCommand>,
    snapshot: SharedSnapshot,
    window: WebviewWindow,
    landing_url: Url,
    config_elapsed: Duration,
) {
    let mut child: Option<Child> = None;
    let mut launched_at: Option<Instant> = None;
    let mut port = 0;
    let ready_announced = Arc::new(AtomicBool::new(false));
    let mut ready = false;
    let mut http_ready_streak = 0_u8;
    let mut http_response_announced = false;
    let mut launch_requested = true;
    let mut launch_started = Instant::now();

    loop {
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(SupervisorCommand::Restart) => {
                if let Some(mut running) = child.take() {
                    terminate_process(&mut running);
                }
                ready_announced.store(false, Ordering::SeqCst);
                ready = false;
                http_ready_streak = 0;
                http_response_announced = false;
                launch_requested = true;
                set_window_visible(&window, false);
                let _ = navigate(&window, landing_url.clone());
            }
            Ok(SupervisorCommand::Shutdown(acknowledge)) => {
                update_snapshot(&snapshot, |state| {
                    state.phase = RuntimePhase::Stopping;
                    state.message = "正在关闭 DSH。".into();
                });
                if let Some(mut running) = child.take() {
                    terminate_process(&mut running);
                }
                update_snapshot(&snapshot, |state| {
                    state.phase = RuntimePhase::Stopped;
                    state.message = "DSH 已关闭。".into();
                    state.pid = None;
                });
                let _ = acknowledge.send(());
                return;
            }
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(mut running) = child.take() {
                    terminate_process(&mut running);
                }
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if launch_requested {
            launch_requested = false;
            ready_announced.store(false, Ordering::SeqCst);
            ready = false;
            http_ready_streak = 0;
            http_response_announced = false;
            launch_started = Instant::now();
            port = match available_port() {
                Ok(port) => port,
                Err(error) => {
                    fail(
                        &snapshot,
                        &window,
                        &landing_url,
                        format!("无法选择本地端口：{error}"),
                    );
                    continue;
                }
            };

            update_snapshot(&snapshot, |state| {
                state.phase = RuntimePhase::Starting;
                state.message = format!("正在通过{}启动。", config.launcher.description);
                state.url = None;
                state.pid = None;
                state.recent_logs.clear();
            });
            push_startup_log(
                &snapshot,
                launch_started,
                format!(
                    "启动配置解析完成（{} ms），运行方式：{}",
                    config_elapsed.as_millis(),
                    config.launcher.description
                ),
            );

            let mut command = config.command(port);
            match command.spawn() {
                Ok(mut spawned) => {
                    let pid = spawned.id();
                    attach_logs(
                        spawned.stdout.take(),
                        spawned.stderr.take(),
                        snapshot.clone(),
                        ready_announced.clone(),
                    );
                    update_snapshot(&snapshot, |state| state.pid = Some(pid));
                    push_startup_log(
                        &snapshot,
                        launch_started,
                        format!("DSH 进程已创建，PID {pid}"),
                    );
                    launched_at = Some(Instant::now());
                    child = Some(spawned);
                }
                Err(error) => {
                    fail(
                        &snapshot,
                        &window,
                        &landing_url,
                        format!("无法执行 {}：{error}", config.launcher.program.display()),
                    );
                }
            }
        }

        let Some(running) = child.as_mut() else {
            continue;
        };

        match running.try_wait() {
            Ok(Some(status)) => {
                child = None;
                let message = if ready {
                    format!("DSH 运行时意外退出：{status}")
                } else {
                    format!("DSH 未完成启动：{status}")
                };
                fail(&snapshot, &window, &landing_url, message);
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                child = None;
                fail(
                    &snapshot,
                    &window,
                    &landing_url,
                    format!("无法读取 DSH 进程状态：{error}"),
                );
                continue;
            }
        }

        if ready {
            continue;
        }

        let elapsed = launched_at.map(|time| time.elapsed()).unwrap_or_default();
        let http_ready = probe_http(port);
        if http_ready && !http_response_announced {
            http_response_announced = true;
            push_startup_log(&snapshot, launch_started, "DSH HTTP 服务开始响应".into());
        }
        http_ready_streak = if http_ready {
            http_ready_streak.saturating_add(1)
        } else {
            0
        };
        if readiness_gate(
            http_ready_streak,
            ready_announced.load(Ordering::SeqCst),
            elapsed,
        ) {
            ready = true;
            let url = format!("http://127.0.0.1:{port}");
            update_snapshot(&snapshot, |state| {
                state.phase = RuntimePhase::Ready;
                state.message = "官方 DSH Web UI 已就绪。".into();
                state.url = Some(url.clone());
            });
            if let Ok(parsed) = Url::parse(&url) {
                push_startup_log(
                    &snapshot,
                    launch_started,
                    "DSH 已就绪，正在加载官方界面".into(),
                );
                if let Err(error) = navigate(&window, parsed) {
                    fail(
                        &snapshot,
                        &window,
                        &landing_url,
                        format!("无法打开 DSH Web UI：{error}"),
                    );
                }
            }
        } else if elapsed >= config.startup_timeout {
            if let Some(mut timed_out) = child.take() {
                terminate_process(&mut timed_out);
            }
            fail(
                &snapshot,
                &window,
                &landing_url,
                format!(
                    "DSH 在 {} 秒内没有完成启动。",
                    config.startup_timeout.as_secs()
                ),
            );
        }
    }
}

fn readiness_gate(http_ready_streak: u8, announced: bool, elapsed: Duration) -> bool {
    (announced && http_ready_streak >= 3)
        || (elapsed >= Duration::from_secs(3) && http_ready_streak >= 5)
}

fn attach_logs(
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    snapshot: SharedSnapshot,
    ready_announced: Arc<AtomicBool>,
) {
    if let Some(stdout) = stdout {
        let stdout_snapshot = snapshot.clone();
        thread::spawn(move || pump_lines(stdout, "stdout", stdout_snapshot, Some(ready_announced)));
    }
    if let Some(stderr) = stderr {
        thread::spawn(move || pump_lines(stderr, "stderr", snapshot, None));
    }
}

fn pump_lines<R: Read>(
    reader: R,
    stream: &str,
    snapshot: SharedSnapshot,
    ready_announced: Option<Arc<AtomicBool>>,
) {
    for line in BufReader::new(reader).lines().map_while(Result::ok) {
        if line.contains("dsh web:")
            && let Some(ready_announced) = &ready_announced
        {
            ready_announced.store(true, Ordering::SeqCst);
        }
        push_log(&snapshot, format!("[{stream}] {line}"));
    }
}

fn available_port() -> Result<u16, std::io::Error> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn probe_http(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(180)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let request = format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut response = [0_u8; 64];
    let Ok(length) = stream.read(&mut response) else {
        return false;
    };
    http_status_is_ready(&response[..length])
}

fn http_status_is_ready(response: &[u8]) -> bool {
    let response = String::from_utf8_lossy(response);
    response.starts_with("HTTP/1.1 2")
        || response.starts_with("HTTP/1.1 3")
        || response.starts_with("HTTP/1.0 2")
        || response.starts_with("HTTP/1.0 3")
}

fn same_http_origin(expected: &Url, actual: &Url) -> bool {
    matches!(expected.scheme(), "http" | "https")
        && expected.scheme() == actual.scheme()
        && expected.host_str() == actual.host_str()
        && expected.port_or_known_default() == actual.port_or_known_default()
}

fn terminate_process(child: &mut Child) {
    let pid = child.id();

    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }

    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    let deadline = Instant::now() + Duration::from_millis(5_500);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }

    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn navigate(window: &WebviewWindow, url: Url) -> Result<(), String> {
    let window_for_navigation = window.clone();
    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let result = window_for_navigation
                .navigate(url)
                .map_err(|error| error.to_string());
            let _ = result_sender.send(result);
        })
        .map_err(|error| error.to_string())?;
    result_receiver
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| "窗口导航请求没有完成".to_string())?
}

fn set_window_visible(window: &WebviewWindow, visible: bool) {
    let window_for_visibility = window.clone();
    let _ = window.run_on_main_thread(move || {
        if visible {
            let _ = window_for_visibility.show();
            let _ = window_for_visibility.set_focus();
        } else {
            let _ = window_for_visibility.hide();
        }
    });
}

fn fail(snapshot: &SharedSnapshot, window: &WebviewWindow, landing_url: &Url, message: String) {
    push_log(snapshot, format!("[desktop] {message}"));
    update_snapshot(snapshot, |state| {
        state.phase = RuntimePhase::Failed;
        state.message = message;
        state.url = None;
        state.pid = None;
    });
    let _ = navigate(window, landing_url.clone());
    set_window_visible(window, true);
}

fn push_startup_log(snapshot: &SharedSnapshot, started: Instant, message: String) {
    push_log(
        snapshot,
        format!("[desktop +{}ms] {message}", started.elapsed().as_millis()),
    );
}

fn push_log(snapshot: &SharedSnapshot, line: String) {
    #[cfg(debug_assertions)]
    eprintln!("{line}");
    update_snapshot(snapshot, |state| {
        state.recent_logs.push(line);
        let excess = state.recent_logs.len().saturating_sub(MAX_LOG_LINES);
        if excess > 0 {
            state.recent_logs.drain(0..excess);
        }
    });
}

fn update_snapshot(snapshot: &SharedSnapshot, update: impl FnOnce(&mut RuntimeSnapshot)) {
    let mut state = lock_snapshot(snapshot);
    update(&mut state);
}

fn lock_snapshot(snapshot: &SharedSnapshot) -> std::sync::MutexGuard<'_, RuntimeSnapshot> {
    snapshot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsh_arguments_keep_launcher_flags_before_web_flags() {
        let arguments = dsh_arguments(Some(Path::new("/tmp/desktop patch.yml")), 43123);
        let actual = arguments
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            [
                "web",
                "--patch",
                "/tmp/desktop patch.yml",
                "--host",
                "127.0.0.1",
                "--port",
                "43123",
            ]
        );
    }

    #[test]
    fn http_probe_accepts_success_and_redirect_statuses() {
        assert!(http_status_is_ready(b"HTTP/1.1 200 OK\r\n"));
        assert!(http_status_is_ready(b"HTTP/1.1 304 Not Modified\r\n"));
        assert!(!http_status_is_ready(b"HTTP/1.1 404 Not Found\r\n"));
    }

    #[test]
    fn readiness_waits_for_a_stable_http_server() {
        assert!(!readiness_gate(1, true, Duration::from_secs(1)));
        assert!(readiness_gate(3, true, Duration::from_secs(1)));
        assert!(!readiness_gate(4, false, Duration::from_secs(3)));
        assert!(readiness_gate(5, false, Duration::from_secs(3)));
    }

    #[test]
    fn runtime_url_match_requires_the_exact_dsh_port() {
        let runtime = Url::parse("http://127.0.0.1:43123").unwrap();
        let runtime_page = Url::parse("http://127.0.0.1:43123/session/abc").unwrap();
        let tauri_dev_page = Url::parse("http://127.0.0.1:1430").unwrap();
        assert!(same_http_origin(&runtime, &runtime_page));
        assert!(!same_http_origin(&runtime, &tauri_dev_page));
    }

    #[test]
    fn official_package_is_pinned() {
        assert_eq!(OFFICIAL_DSH_PACKAGE, "@deepseek-ai/dsh@0.1.0-rc.6");
    }

    #[test]
    fn generated_patch_preserves_cordis_indentation() {
        let temporary =
            std::env::temp_dir().join(format!("dsh-desktop-runtime-test-{}", std::process::id()));
        std::fs::create_dir_all(&temporary).unwrap();
        let patch = write_generated_patch(
            &temporary,
            Path::new("/tmp/desktop-plugin/index.mjs"),
            OFFICIAL_DSH_PACKAGE,
        )
        .unwrap();
        let contents = std::fs::read_to_string(patch).unwrap();
        assert!(contents.contains("- insert:\n    - id: desktop-runtime\n"));
        assert!(contents.contains("      config:\n        desktopVersion:"));
        assert!(contents.contains("        desktopPid:"));
    }

    #[test]
    fn development_runtime_data_is_isolated_from_release_data() {
        let base = Path::new("/app-data");
        let scoped = scoped_runtime_root(base);
        if cfg!(debug_assertions) {
            assert_eq!(scoped, base.join("development"));
        } else {
            assert_eq!(scoped, base);
        }
    }

    #[test]
    fn os_string_arguments_preserve_non_utf8_compatible_shape() {
        let patch = Path::new(std::ffi::OsStr::new("desktop.cordis.patch.yml"));
        let arguments = dsh_arguments(Some(patch), 3080);
        assert_eq!(arguments[2], patch.as_os_str());
    }

    #[test]
    fn bundled_node_directory_is_first_on_path() {
        let program = if cfg!(windows) {
            Path::new(r"C:\runtime\node\node.exe")
        } else {
            Path::new("/runtime/node/bin/node")
        };
        let value = bundled_path_env(program).unwrap();
        let first = env::split_paths(&value).next().unwrap();
        assert_eq!(first, program.parent().unwrap());
    }
}
