//! `qx-cli console`：把静态控制台与 `qx-api` 挂在**同一个源**上（M4' 同源 BFF）。
//!
//! 本文件做四件事：读配置、按回环地址绑监听、解析引导令牌、把启动 URL 印给运维；外加三条
//! **不启动服务**的易用性入口（`--init` 脚手架 / `--generate-token` 令牌生成 / 显式公共行情预置）。会话、CSRF、
//! Origin 与身份注入的判定全部在 `qx_api::ConsoleFront` 里，这里不复制第二份。
//!
//! 令牌的来源：**优先**从 `api.console.bootstrap_token_env` 点名的环境变量读，**不**接受命令行
//! 传入、**不**写进运行时 JSON、**不**回显进日志——命令行会进进程列表与 shell 历史，配置文件会进
//! 版本库，两者都不是放秘密的地方。环境变量缺失时（本机开发与首次试用的常见情形），本层**临时生成**
//! 一枚一次性令牌并明确打印，让"忘了 export"不再是一道启动失败的门槛；这枚令牌随进程生灭、不落盘，
//! 因此它只服务本机回环控制台，不能替代运维显式配置的长期秘密。

use crate::cli_args::QuickstartMarketType;
use crate::console_args::ConsolePublicMarketProvider;
use crate::{
    build_configured_api_service, catch_panic, read_runtime_config, runtime_timestamp_ms,
    ConsoleArgs,
};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

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
    catch_panic("console-serve", || serve_console_inner(args))
}

fn serve_console_inner(args: &ConsoleArgs) -> Result<(), String> {
    // 两条易用性入口放在读配置之前：它们各自不需要一份"能跑"的配置，也不该因为配置缺失而失败。
    if let Some(target) = &args.init {
        return scaffold_console_runtime(target, args.public_market_provider, args.market_type);
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
    let static_dir = resolve_console_static_dir(&console.static_dir, &args.path)?;
    let service = build_configured_api_service(&config, &args.path)?;
    qx_service::http_host::serve_console(
        config,
        service,
        static_dir,
        token,
        runtime_timestamp_ms,
        |bind, operator, static_dir, ttl_seconds, token| {
            println!(
                "[控制台 · Console] bind={bind} operator={operator} 静态目录={} 会话 TTL={ttl_seconds}s，按 Ctrl+C 停止",
                static_dir.display()
            );
            // 入口只打印一次；令牌验证成功后会由 HttpOnly 会话替代地址栏凭据。
            println!(
                "[控制台 · Console] 浏览器打开 http://{bind}/?{}={token}",
                qx_api::CONSOLE_TOKEN_QUERY
            );
        },
    )
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

/// `--generate-token`：只打印一枚令牌与当前 shell 可粘贴的环境变量赋值（不启动服务）。
///
/// 不写成"给命令替换用"的裸输出：启动横幅走的是 stdout，`$(qx-cli console --generate-token)`
/// 会把横幅一起吞进去。这里给一行适配当前 shell 的赋值语句，人在 shell 里粘一次即可。
fn print_generated_token() {
    let token = generate_bootstrap_token();
    println!("[控制台 · Console] 新引导令牌（≥16 字符；复制下面这一行到你的 shell）：");
    println!(
        "{}",
        format_bootstrap_token_assignment("QX_CONSOLE_BOOTSTRAP_TOKEN", &token)
    );
}

#[cfg(windows)]
fn format_bootstrap_token_assignment(env_name: &str, value: &str) -> String {
    format!("$env:{env_name}=\"{value}\"")
}

#[cfg(not(windows))]
fn format_bootstrap_token_assignment(env_name: &str, value: &str) -> String {
    format!("export {env_name}=\"{value}\"")
}

/// `--init`：写出一份就绪的运行时模板到指定路径后退出（不启动服务）。
///
/// **拒绝覆盖**已存在的文件：控制台模板指向数据目录与端口，静默改写会让使用者对一份没人点过名的
/// 配置签字。写好后把下一步（当前 shell 的环境变量赋值 + 启动命令）印成人可以直接复制的一行。
fn scaffold_console_runtime(
    target: &Path,
    provider: Option<ConsolePublicMarketProvider>,
    market_type: Option<QuickstartMarketType>,
) -> Result<(), String> {
    let (runtime_contents, companion) = match provider {
        Some(provider) => {
            let market_type = market_type.unwrap_or(QuickstartMarketType::Swap);
            if market_type == QuickstartMarketType::Spot
                && provider != ConsolePublicMarketProvider::Binance
            {
                return Err("当前 spot 公共行情初始化仅支持 Binance；OKX 和 Bitget 请使用 --market-type swap".into());
            }
            let (runtime_contents, companion) =
                public_market_console_scaffold(target, provider, market_type)?;
            (runtime_contents, Some(companion))
        }
        None => (CONSOLE_RUNTIME_TEMPLATE.to_owned(), None),
    };
    let public_companion_path = companion.as_ref().map(|(path, _)| path.clone());
    let mut outputs = vec![(target.to_path_buf(), runtime_contents.into_bytes())];
    if let Some((path, contents)) = companion {
        outputs.push((path, contents.into_bytes()));
    }
    write_new_files_with_rollback(&outputs)?;
    let env_name = "QX_CONSOLE_BOOTSTRAP_TOKEN";
    println!("[控制台 · Console] 已写出运行时模板 {}", target.display());
    if let Some(path) = public_companion_path {
        println!(
            "[控制台 · Console] 已写出无凭据公共行情配置 {}",
            path.display()
        );
        println!("[控制台 · Console] 公共行情只读，不会提交订单；使用前需安装 Python CCXT extra：python -m pip install \"qianxing[ccxt]\"");
    }
    println!("[控制台 · Console] 下一步（引导令牌只走环境变量，不进命令行 / 配置文件）：");
    println!(
        "  {}   # 或用 `qx-cli console --generate-token` 生成一行",
        format_bootstrap_token_assignment(env_name, "<至少16字符的秘密>")
    );
    println!("  qx-cli console {}", target.display());
    println!(
        "[控制台 · Console] 若不设置引导环境变量，启动时会自动临时生成一次性令牌并打印入口 URL。"
    );
    Ok(())
}

/// Build an opt-in public-market runtime plus its companion CCXT config. Keep both paths adjacent
/// so the normal runtime-relative resolver can find the companion from any working directory.
fn public_market_console_scaffold(
    target: &Path,
    provider: ConsolePublicMarketProvider,
    market_type: QuickstartMarketType,
) -> Result<(String, (PathBuf, String)), String> {
    let exchange = provider.as_str();
    let market_name = market_type.as_str();
    let instrument = match market_type {
        QuickstartMarketType::Spot => "BTC/USDT.BINANCE".to_owned(),
        QuickstartMarketType::Swap => format!("BTC/USDT:USDT.{}", exchange.to_ascii_uppercase()),
    };
    let companion_path = public_market_companion_path(target)?;
    let companion_name = companion_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("公共 CCXT 配置文件名无效: {}", companion_path.display()))?;

    let mut runtime: serde_json::Value = serde_json::from_str(CONSOLE_RUNTIME_TEMPLATE)
        .map_err(|error| format!("内置 Console 运行时模板无效: {error}"))?;
    let mut exchange_config = serde_json::json!({
        "exchange_id": exchange,
        "sandbox": false,
        "use_environment_proxy": true,
        "enable_rate_limit": true,
        "timeout_ms": 30000,
        "historical_data_timeout_ms": 300000,
        "default_type": market_name,
        "options": {},
        "instruments": [instrument],
    });
    if market_type == QuickstartMarketType::Spot {
        exchange_config["options"] = serde_json::json!({"fetchMarkets": {"types": ["spot"]}});
        exchange_config["public_market_data_endpoint"] =
            serde_json::Value::String("binance_spot_data_api".into());
    }
    runtime["api"]["data_providers"] = serde_json::json!([{
        "provider_id": format!("ccxt-{exchange}-{market_name}-public"),
        "version": "ccxt-public-bars-v1",
        "adapter": {"kind": "ccxt", "config_path": companion_name},
        "data_kinds": ["Bar"],
        "asset_classes": ["crypto"],
        "frequencies": ["1m", "5m", "15m", "1h", "4h", "1d"],
        "auth_scope": "public",
        "rate_limit_per_second": 5,
        "freshness_seconds": 60,
        "historical_start": 0,
        "historical_end": u64::MAX,
        "realtime": false,
        "priority": 0,
        "quality_score": 80,
        "cost_score": 1
    }]);
    let runtime_text = serde_json::to_string_pretty(&runtime)
        .map_err(|error| format!("编码 Console 运行时模板失败: {error}"))?;
    let companion_text = serde_json::to_string_pretty(&exchange_config)
        .map_err(|error| format!("编码公共 CCXT 配置失败: {error}"))?;
    Ok((runtime_text, (companion_path, companion_text)))
}

fn public_market_companion_path(target: &Path) -> Result<PathBuf, String> {
    let file_name = target
        .file_name()
        .ok_or_else(|| format!("运行时配置路径缺少文件名: {}", target.display()))?;
    let mut companion_name = file_name.to_os_string();
    companion_name.push(".ccxt-public.json");
    Ok(target.with_file_name(companion_name))
}

/// Create all requested files with `create_new`, so a concurrent invocation cannot overwrite a
/// file between an existence check and the write. On any partial write failure, remove only the
/// files this invocation successfully created.
fn write_new_files_with_rollback(outputs: &[(PathBuf, Vec<u8>)]) -> Result<(), String> {
    let mut created: Vec<(PathBuf, File, &[u8])> = Vec::with_capacity(outputs.len());
    for (path, contents) in outputs {
        let file = match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => file,
            Err(error) => {
                let message = if error.kind() == std::io::ErrorKind::AlreadyExists {
                    format!(
                        "{} 已存在，拒绝覆盖：换一个路径，或先自行处理这份文件",
                        path.display()
                    )
                } else {
                    format!("创建配置文件失败 {}: {error}", path.display())
                };
                return match rollback_created_files(created) {
                    Ok(()) => Err(message),
                    Err(cleanup_error) => Err(format!(
                        "{message}；清理本次已创建的文件也失败：{cleanup_error}"
                    )),
                };
            }
        };
        created.push((path.clone(), file, contents.as_slice()));
    }
    let mut write_error = None;
    for (path, file, contents) in &mut created {
        if let Err(error) = file.write_all(contents).and_then(|()| file.sync_all()) {
            write_error = Some(format!("写出配置文件失败 {}: {error}", path.display()));
            break;
        }
    }
    if let Some(error) = write_error {
        return match rollback_created_files(created) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(format!(
                "{error}；清理本次已创建的文件也失败：{cleanup_error}"
            )),
        };
    }
    Ok(())
}

fn rollback_created_files(created: Vec<(PathBuf, File, &[u8])>) -> Result<(), String> {
    let paths: Vec<PathBuf> = created.into_iter().map(|(path, _, _)| path).collect();
    let mut failures = Vec::new();
    for path in paths {
        if let Err(error) = fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                failures.push(format!("{}: {error}", path.display()));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
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
    use crate::cli_args::{Cli, Command};
    use clap::Parser;

    fn temporary_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "qx-console-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

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
    fn bootstrap_token_assignment_uses_current_shell_syntax() {
        let assignment = format_bootstrap_token_assignment("QX_CONSOLE_BOOTSTRAP_TOKEN", "abc123");
        #[cfg(windows)]
        assert_eq!(assignment, "$env:QX_CONSOLE_BOOTSTRAP_TOKEN=\"abc123\"");
        #[cfg(not(windows))]
        assert_eq!(assignment, "export QX_CONSOLE_BOOTSTRAP_TOKEN=\"abc123\"");
    }

    #[test]
    fn scaffold_refuses_to_overwrite_an_existing_file() {
        let dir = temporary_root("scaffold-existing");
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let target = dir.join("runtime.json");
        std::fs::write(&target, "{}").expect("预置文件");
        let error = scaffold_console_runtime(&target, None, None).expect_err("已存在必须拒绝");
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

    #[test]
    fn public_market_scaffold_builds_valid_read_only_configs_for_supported_venues() {
        for provider in [
            ConsolePublicMarketProvider::Okx,
            ConsolePublicMarketProvider::Binance,
            ConsolePublicMarketProvider::Bitget,
        ] {
            let root = temporary_root(provider.as_str());
            let target = root.join("console.json");
            let (runtime_text, (ccxt_path, ccxt_text)) =
                public_market_console_scaffold(&target, provider, QuickstartMarketType::Swap)
                    .expect("构建 swap 公共行情模板");
            let runtime = qx_runtime::RuntimeConfig::from_json(&runtime_text)
                .expect("生成的 RuntimeConfig 必须通过正式校验");
            assert_eq!(runtime.api.data_providers.len(), 1);
            let configured_path = match &runtime.api.data_providers[0].adapter {
                qx_runtime::ApiDataProviderAdapterConfig::Ccxt { config_path } => config_path,
            };
            assert_eq!(configured_path, "console.json.ccxt-public.json");
            assert_eq!(
                ccxt_path.file_name().unwrap(),
                std::ffi::OsStr::new(configured_path)
            );
            let exchange_config: serde_json::Value =
                serde_json::from_str(&ccxt_text).expect("公共 CCXT JSON");
            assert_eq!(exchange_config["exchange_id"], provider.as_str());
            assert_eq!(exchange_config["sandbox"], false);
            assert_eq!(exchange_config["default_type"], "swap");
            assert!(exchange_config.get("credential_env").is_none());
            assert_eq!(
                exchange_config["instruments"][0],
                format!("BTC/USDT:USDT.{}", provider.as_str().to_ascii_uppercase())
            );
        }
    }

    #[test]
    fn public_market_scaffold_supports_binance_spot_endpoint_only() {
        let root = temporary_root("binance-spot");
        let target = root.join("runtime.json");
        let (runtime_text, (_, ccxt_text)) = public_market_console_scaffold(
            &target,
            ConsolePublicMarketProvider::Binance,
            QuickstartMarketType::Spot,
        )
        .expect("构建 Binance spot 模板");
        qx_runtime::RuntimeConfig::from_json(&runtime_text)
            .expect("spot RuntimeConfig 必须通过正式校验");
        let ccxt: serde_json::Value = serde_json::from_str(&ccxt_text).unwrap();
        assert_eq!(ccxt["public_market_data_endpoint"], "binance_spot_data_api");
        assert_eq!(ccxt["instruments"][0], "BTC/USDT.BINANCE");
        assert!(ccxt.get("credential_env").is_none());

        let error = scaffold_console_runtime(
            &root.join("unsupported.json"),
            Some(ConsolePublicMarketProvider::Okx),
            Some(QuickstartMarketType::Spot),
        )
        .expect_err("OKX spot 当前未纳入支持矩阵");
        assert!(error.contains("仅支持 Binance"), "{error}");
    }

    #[test]
    fn paired_scaffold_refuses_existing_companion_without_leaving_runtime_file() {
        let root = temporary_root("companion-existing");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("runtime.json");
        let companion = public_market_companion_path(&target).unwrap();
        std::fs::write(&companion, "existing").unwrap();
        let error =
            scaffold_console_runtime(&target, Some(ConsolePublicMarketProvider::Bitget), None)
                .expect_err("已有 companion 必须拒绝覆盖");
        assert!(error.contains("已存在，拒绝覆盖"), "{error}");
        assert!(!target.exists(), "失败时不留下半套配置");
        assert_eq!(std::fs::read(&companion).unwrap(), b"existing");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scaffold_writes_both_files_and_generated_runtime_resolves_its_provider_config() {
        let root = temporary_root("complete-pair");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("runtime.json");
        scaffold_console_runtime(
            &target,
            Some(ConsolePublicMarketProvider::Okx),
            Some(QuickstartMarketType::Swap),
        )
        .expect("完整写出成对配置");
        let runtime_text = std::fs::read_to_string(&target).unwrap();
        let runtime = qx_runtime::RuntimeConfig::from_json(&runtime_text).unwrap();
        let configured = match &runtime.api.data_providers[0].adapter {
            qx_runtime::ApiDataProviderAdapterConfig::Ccxt { config_path } => config_path,
        };
        let resolved = qx_runtime::resolve_runtime_relative_path(&target, configured);
        let ccxt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(resolved).unwrap()).unwrap();
        assert_eq!(ccxt["exchange_id"], "okx");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn public_market_flags_require_init_and_keep_existing_init_mode_opt_in() {
        let with_provider = Cli::try_parse_from([
            "qx-cli",
            "console",
            "--init",
            "console.json",
            "--public-market-provider",
            "okx",
        ])
        .expect("完整 opt-in 参数");
        match with_provider.command.unwrap() {
            Command::Console(args) => {
                assert_eq!(
                    args.public_market_provider,
                    Some(ConsolePublicMarketProvider::Okx)
                );
                assert_eq!(args.market_type, None);
            }
            _ => panic!("应解析成 Console 命令"),
        }
        assert!(
            Cli::try_parse_from(["qx-cli", "console", "--public-market-provider", "okx"]).is_err()
        );
        assert!(Cli::try_parse_from([
            "qx-cli",
            "console",
            "--init",
            "console.json",
            "--market-type",
            "spot"
        ])
        .is_err());
    }
}
