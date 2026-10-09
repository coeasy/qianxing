//! `qx-cli console`：把静态控制台与 `qx-api` 挂在**同一个源**上（M4' 同源 BFF）。
//!
//! 本文件只做四件事：读配置、按回环地址绑监听、从环境变量取引导令牌、把启动 URL 印给运维。
//! 会话、CSRF、Origin 与身份注入的判定全部在 `qx_api::ConsoleFront` 里，这里不复制第二份。
//!
//! 令牌的来源是刻意的：只从 `api.console.bootstrap_token_env` 点名的环境变量读，**不**接受
//! 命令行传入、**不**写进运行时 JSON、**不**回显进日志。命令行会进进程列表与 shell 历史，
//! 配置文件会进版本库，两者都不是放秘密的地方。

use super::*;

pub(crate) fn serve_console(args: &ConsoleArgs) -> Result<(), String> {
    let config = read_runtime_config(&args.path)?;
    let console = config.api.console.as_ref().ok_or_else(|| {
        format!(
            "运行时配置 {} 没有 api.console 段：控制台面不跟着 serve 一起开，要开就显式配上",
            args.path.display()
        )
    })?;
    // 地址先过 `qx-api` 的判据再绑：等绑上才发现不是回环，控制面已经挂在那张网卡上了。
    qx_api::console_bind_is_loopback(&console.bind)?;
    let token = std::env::var(&console.bootstrap_token_env).map_err(|_| {
        format!(
            "控制台引导令牌未设置：把令牌放进环境变量 {}（不走命令行、不写进配置文件）",
            console.bootstrap_token_env
        )
    })?;
    // 缺省值只有 `qx-api` 那一处定义；配置侧写 None 就是"用它的默认"。
    let ttl_seconds = console
        .session_ttl_seconds
        .unwrap_or(qx_api::DEFAULT_CONSOLE_SESSION_TTL_SECONDS);
    let static_dir = resolve_console_static_dir(&console.static_dir, &args.path)?;
    let supervisor = RuntimeSupervisor::new(config.clone())?;
    let api_worker_id = configured_api_worker_id(supervisor.config())?;
    let service = build_configured_api_service(supervisor.config(), &args.path)?;
    let front = qx_api::ConsoleFront::new(
        service.clone(),
        qx_api::ConsoleConfig::new(
            static_dir.clone(),
            console.operator.clone(),
            token,
            ttl_seconds,
        )?,
    )?;
    let listener = TcpListener::bind(&console.bind)
        .map_err(|error| format!("绑定控制台地址失败 {}: {error}", console.bind))?;
    println!(
        "[控制台 · Console] bind={} operator={} 静态目录={} 会话 TTL={ttl_seconds}s，按 Ctrl+C 停止",
        console.bind,
        console.operator,
        static_dir.display()
    );
    // 只印一次入口 URL：令牌在 URL 里，运维点一次就换到了 HttpOnly 会话 cookie，
    // 之后地址栏里不再有凭据。令牌原文不重复打印，免得它落在终端的滚动缓冲里。
    println!(
        "[控制台 · Console] 浏览器打开 http://{}/?{}={}",
        console.bind,
        qx_api::CONSOLE_TOKEN_QUERY,
        std::env::var(&console.bootstrap_token_env).unwrap_or_default()
    );
    let projection_stop = Arc::new(AtomicBool::new(false));
    let mut projection_thread =
        spawn_api_projection_bridge(&config, service, Arc::clone(&projection_stop));
    let worker = match supervisor.spawn_worker(&api_worker_id, move |context| {
        context.heartbeat(runtime_timestamp_ms())?;
        front
            .serve(listener, runtime_timestamp_ms, || context.should_stop())
            .map_err(|error| format!("控制台服务停止: {error}"))
    }) {
        Ok(worker) => worker,
        Err(error) => {
            stop_api_projection_bridge(&projection_stop, &mut projection_thread);
            return Err(error);
        }
    };
    let result = join_worker_handle(&supervisor, worker, "Console", &api_worker_id);
    stop_api_projection_bridge(&projection_stop, &mut projection_thread);
    result
}

/// 静态控制台资源目录的解析面。
///
/// 配置里写的是仓库形状的相对路径（`web/console`），而进程的工作目录不一定是仓库根：
/// 安装包形态下 exe 与仓库分离。按「配置所在目录 → 可执行文件同级/上一级 → 构建期源码树」
/// 三格找（与 `deploy/` 查找面同序），一格都不中就报错点名找过哪里——静默回落一个空目录
/// 会让控制台在浏览器里 404，而服务端看起来一切正常。
fn resolve_console_static_dir(configured: &str, runtime_config: &Path) -> Result<PathBuf, String> {
    let candidate = Path::new(configured);
    if candidate.is_dir() {
        return Ok(candidate.to_path_buf());
    }
    if candidate.is_absolute() {
        return Err(format!("控制台静态目录不存在: {}", candidate.display()));
    }
    let mut roots = Vec::new();
    if let Some(dir) = runtime_config.parent() {
        roots.push(dir.to_path_buf());
    }
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        roots.push(exe_dir.clone());
        if let Some(parent) = exe_dir.parent() {
            roots.push(parent.to_path_buf());
        }
    }
    roots.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."));
    for root in roots {
        let probe = root.join(candidate);
        if probe.is_dir() {
            return Ok(probe);
        }
    }
    Err(format!(
        "控制台静态目录不存在: {configured}（按配置所在目录、可执行文件同级与构建期源码树找过）"
    ))
}
