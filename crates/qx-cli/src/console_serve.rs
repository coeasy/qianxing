//! `qx-cli console`：把静态控制台与 `qx-api` 挂在**同一个源**上（M4' 同源 BFF）。
//!
//! 本文件做四件事：读配置、按回环地址绑监听、解析引导令牌、把启动 URL 印给运维；外加两条
//! **不启动服务**的易用性入口（`--init` 脚手架 / `--generate-token` 令牌生成）。会话、CSRF、
//! Origin 与身份注入的判定全部在 `qx_api::ConsoleFront` 里，这里不复制第二份。
//!
//! 令牌的来源：**优先**从 `api.console.bootstrap_token_env` 点名的环境变量读，**不**接受命令行
//! 传入、**不**写进运行时 JSON、**不**回显进日志——命令行会进进程列表与 shell 历史，配置文件会进
//! 版本库，两者都不是放秘密的地方。环境变量缺失时（本机开发与首次试用的常见情形），本层**临时生成**
//! 一枚一次性令牌并明确打印，让"忘了 export"不再是一道启动失败的门槛；这枚令牌随进程生灭、不落盘，
//! 因此它只服务本机回环控制台，不能替代运维显式配置的长期秘密。

use super::*;

/// `--init` 写出的运行时模板：与 `deploy/qianxing.runtime.console.example.json` **同形**
/// （回环 bind、令牌只给环境变量名、正文无令牌字面量字段、写明 static_dir 与 operator）。
/// 门禁 `console_usability_check` 会把它当 JSON 解析后逐格核对，改了形状当场红。
const CONSOLE_RUNTIME_TEMPLATE: &str = r#"{
  "schema_version": 1,
  "environment": "paper",
  "api": {
    "bind": "127.0.0.1:18090",
    "transport": "plaintext",
    "tls": null,
    "operators": {},
    "console": {
      "bind": "127.0.0.1:18091",
      "static_dir": "web/console",
      "operator": "console-operator",
      "bootstrap_token_env": "QX_CONSOLE_BOOTSTRAP_TOKEN",
      "session_ttl_seconds": 3600
    }
  },
  "storage": {
    "backend": "files",
    "data_dir": "data/qianxing-console",
    "sqlite_path": null
  },
  "workers": [
    {
      "id": "api",
      "role": "api",
      "enabled": true,
      "account_id": null,
      "venue_id": null,
      "endpoint": null,
      "symbols": [],
      "credential_env": null
    }
  ],
  "shutdown_timeout_ms": 30000
}
"#;

pub(crate) fn serve_console(args: &ConsoleArgs) -> Result<(), String> {
    // 两条易用性入口放在读配置之前：它们各自不需要一份"能跑"的配置，也不该因为配置缺失而失败。
    if let Some(target) = &args.init {
        return scaffold_console_runtime(target);
    }
    if args.generate_token {
        print_generated_token();
        return Ok(());
    }
    let config = read_runtime_config(&args.path)?;
    let console = config.api.console.as_ref().ok_or_else(|| {
        format!(
            "运行时配置 {} 没有 api.console 段：控制台面不跟着 serve 一起开，要开就显式配上",
            args.path.display()
        )
    })?;
    // 地址先过 `qx-api` 的判据再绑：等绑上才发现不是回环，控制面已经挂在那张网卡上了。
    qx_api::console_bind_is_loopback(&console.bind)?;
    // 引导令牌：运维显式配的照用；缺失时临时生成一枚并当场说清它是临时的、随进程生灭。
    let (token, generated) = match std::env::var(&console.bootstrap_token_env) {
        Ok(value) if !value.trim().is_empty() => (value, false),
        _ => (generate_bootstrap_token(), true),
    };
    if generated {
        println!(
            "[控制台 · Console] 未检测到环境变量 {}：已临时生成一次性引导令牌（进程退出即失效，重启换新）。",
            console.bootstrap_token_env
        );
    }
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
            token.clone(),
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
        token
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

/// 生成一枚一次性引导令牌（96 个十六进制字符，远超 16 字符的下限）。
///
/// 熵来源是标准库的 `RandomState`：它的种子在进程内首次使用时由操作系统熵源
/// （`getrandom` / `RtlGenRandom`）随机化，因此对未持有本进程内存的对手不可预测。本仓刻意
/// **不引入 RNG 依赖**（与 `console.rs` 用引导令牌经 SHA-1 派生会话 id 是同一取舍）；这枚令牌
/// 只服务本机回环控制台的首次引导，且随进程生灭，够用且可解释。混合时间与 pid 让同一进程内
/// 连续两次生成也不同。
fn generate_bootstrap_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let state = RandomState::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let pid = u64::from(std::process::id());
    let mut token = String::with_capacity(96);
    for round in 0..6u64 {
        let mut hasher = state.build_hasher();
        hasher.write_u64(now);
        hasher.write_u64(pid);
        hasher.write_u64(round);
        token.push_str(&format!("{:016x}", hasher.finish()));
    }
    token
}

/// `--generate-token`：只打印一枚令牌与可直接粘贴的 `export` 行（不启动服务）。
///
/// 不写成"给命令替换用"的裸输出：启动横幅走的是 stdout，`$(qx-cli console --generate-token)`
/// 会把横幅一起吞进去。这里给一行现成可复制的 `export`，人在 shell 里粘一次即可。
fn print_generated_token() {
    let token = generate_bootstrap_token();
    println!("[控制台 · Console] 新引导令牌（≥16 字符；复制下面这一行到你的 shell）：");
    println!("export QX_CONSOLE_BOOTSTRAP_TOKEN=\"{token}\"");
}

/// `--init`：写出一份就绪的运行时模板到指定路径后退出（不启动服务）。
///
/// **拒绝覆盖**已存在的文件：控制台模板指向数据目录与端口，静默改写会让使用者对一份没人点过名的
/// 配置签字。写好后把下一步（`export` + 启动命令）印成人可以直接复制的一行。
fn scaffold_console_runtime(target: &Path) -> Result<(), String> {
    if target.exists() {
        return Err(format!(
            "{} 已存在，拒绝覆盖：换一个路径，或先自行处理这份文件",
            target.display()
        ));
    }
    std::fs::write(target, CONSOLE_RUNTIME_TEMPLATE)
        .map_err(|error| format!("写出运行时模板失败 {}: {error}", target.display()))?;
    let env_name = "QX_CONSOLE_BOOTSTRAP_TOKEN";
    println!("[控制台 · Console] 已写出运行时模板 {}", target.display());
    println!("[控制台 · Console] 下一步（引导令牌只走环境变量，不进命令行 / 配置文件）：");
    println!("  export {env_name}=\"<至少16字符的秘密>\"   # 或用 `qx-cli console --generate-token` 生成一行");
    println!("  qx-cli console {}", target.display());
    println!("[控制台 · Console] 若跳过 export，启动时会自动临时生成一次性令牌并打印入口 URL。");
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_bootstrap_token_is_long_hex_and_varies() {
        let first = generate_bootstrap_token();
        let second = generate_bootstrap_token();
        assert!(first.len() >= 16, "生成的令牌长度 {} 短于下限", first.len());
        assert!(
            first.chars().all(|c| c.is_ascii_hexdigit()),
            "生成的令牌含非十六进制字符: {first:?}"
        );
        assert_ne!(first, second, "同一进程内两次生成不得相同");
    }

    #[test]
    fn scaffold_refuses_to_overwrite_an_existing_file() {
        let dir = std::env::temp_dir().join(format!("qx-console-scaffold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let target = dir.join("runtime.json");
        std::fs::write(&target, "{}").expect("预置文件");
        let error = scaffold_console_runtime(&target).expect_err("已存在必须拒绝");
        assert!(error.contains("已存在"), "{error}");
        std::fs::remove_file(&target).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scaffold_template_is_a_loopback_console_runtime() {
        let parsed: serde_json::Value =
            serde_json::from_str(CONSOLE_RUNTIME_TEMPLATE).expect("模板必须是合法 JSON");
        let console = &parsed["api"]["console"];
        assert_eq!(console["bind"], "127.0.0.1:18091");
        assert!(console["static_dir"].as_str().is_some());
        assert!(console["operator"].as_str().is_some());
        let env_name = console["bootstrap_token_env"].as_str().expect("环境变量名");
        assert!(
            env_name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "环境变量名形状不对: {env_name:?}"
        );
        assert!(!CONSOLE_RUNTIME_TEMPLATE.contains("\"bootstrap_token\""));
    }
}
