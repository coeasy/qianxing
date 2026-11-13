//! `quickstart` —— 把《工业化易用性收口指南》§1 的首跑链条收成一条命令（易用性 P2 第一格）。
//!
//! `init --strategy macd` → `doctor` → `backtest` → `report` → `status` 这五条此前只写在 README
//! 里靠人逐条敲：单条命令的报错一直有出口，但"整条链条走到第几步、失败那步的原文是什么、
//! 跑完接着敲什么"没有（U4 的成功侧）。本模块直调 crate 根上那五个入口的同一批函数，所以
//! 产物、退出码与 `result_hash` 都与手工逐条敲同源 —— 另起子进程只会多付五次启动，
//! 不会多证明任何东西。
//!
//! 落点口径：`init` 把 `storage.data_dir` 钉成项目目录内的绝对路径，因此整条链条只写使用者
//! 指定的目录，仓库的 `deploy/data/` 一份都不碰。

use std::path::{Path, PathBuf};

use crate::{
    cli_args, run_ccxt_builtin_backtest, run_doctor, run_init_with_ccxt_selection,
    run_init_with_profile, run_report, run_status, run_unified_backtest,
    write_ccxt_quickstart_project_manifest, CcxtBuiltinBacktestInput,
};

/// 不带参数时的项目目录名。默认值不取 `.`：那会把整份产物摊进使用者的当前目录，
/// 在仓库根执行就成了一次"首跑命令弄脏工作树"。
const DEFAULT_PROJECT_DIR: &str = "qianxing-quickstart";
/// 首跑绑定的内置策略与 README「安装 A」那六条里的 `--strategy macd` 一致。
const QUICKSTART_STRATEGY: &str = "macd";
/// One whole instrument unit. `CcxtBuiltinBacktestInput.quantity` is in display units,
/// and the snapshot builder converts it to 1e-9 raw quantity; passing SCALE here would
/// accidentally request one billion contracts/coins.
const CCXT_QUICKSTART_DEFAULT_QUANTITY: i64 = 1;
/// 项目内运行时配置的文件名，同样取自 README 那六条的写法。
const RUNTIME_FILE_NAME: &str = "qianxing.runtime.json";
/// 收尾计数最多打开多少个目录。正常项目是一层 data 目录，个位数够用；这一格是符号链接环
/// 与误指大目录树的止损线，不是产品口径，所以取一个正常首跑永远碰不到的数。
pub(crate) const PROJECT_FILE_COUNT_BUDGET: usize = 2000;

/// 把一步的参数拼成可以原样粘贴回终端的一行；含空白的参数加引号。
/// 程序名不另抄一份：用的就是 `cli_args.rs` 里命令表声明的那一个常量。
fn command_line(arguments: &[&str]) -> String {
    let mut line = cli_args::PROGRAM_NAME.to_string();
    for argument in arguments {
        line.push(' ');
        if argument.contains(' ') {
            line.push('"');
            line.push_str(argument);
            line.push('"');
        } else {
            line.push_str(argument);
        }
    }
    line
}

/// 一步的落点：成功印一行「[完成]」；失败印「[止步]」+ 那一步的命令原文 + 重跑整条的写法，
/// 再以退出码 2 fail closed。止步的那一步必须能单独重跑，这是本入口唯一的错误面。
fn require(label: &str, command: &str, retry: &str, outcome: Result<(), String>) {
    if let Err(error) = outcome {
        eprintln!("[止步] {label} 失败：{error}");
        eprintln!("  这一步的原文命令：{command}");
        let from_scratch = if retry.ends_with(" --force") {
            retry.to_owned()
        } else {
            format!("{retry} --force")
        };
        eprintln!("  可直接重试上方单步命令；确认覆盖项目文件后从头重跑：{from_scratch}");
        std::process::exit(2);
    }
    println!("[完成] {label}：{command}");
}

/// 数项目目录里的文件份数，最多打开 `budget` 个目录：返回（份数，是否在预算处截断）。
///
/// 预算是为了让收尾陈述能在符号链接目录环上终止：`is_dir()` 跟随符号链接，一个指回祖先的
/// 链接就能让这里永不返回——五步产品全部跑完之后卡在「共 N 份文件」这一行上。同一份预算
/// 也给一次误指向大目录树的首跑兜底。
/// 文档口径是 11 份（`init`，含 Grafana 面板）+ 6 份（首跑回测与数据集清单）= 17 份。读不动的目录只少计一份，不让一次
/// 已经成功的首跑改口——这格计数是收尾陈述，不是判据，所以截断如实印出来而不是报错。
pub(crate) fn project_file_count(dir: &Path, budget: usize) -> (usize, bool) {
    let mut stack = vec![dir.to_path_buf()];
    let mut files = 0_usize;
    let mut opened = 0_usize;
    while let Some(current) = stack.pop() {
        opened += 1;
        if opened > budget {
            return (files, true);
        }
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files += 1;
            }
        }
    }
    (files, false)
}

/// `qx-cli quickstart [目录] [--profile builtin|ashare] [--strategy KIND] [--html] [--force]` 的实现体。
/// 失败自行以 2 退出，成功印收尾指引后返回。
pub(crate) fn run(
    project: Option<PathBuf>,
    html: bool,
    force: bool,
    profile: cli_args::QuickstartProfile,
    strategy: Option<String>,
    days: Option<u32>,
    timeframe: Option<String>,
    as_of_ms: Option<u64>,
    instrument: Option<String>,
    exchange: Option<String>,
) {
    let ccxt_options_used = days.is_some()
        || timeframe.is_some()
        || as_of_ms.is_some()
        || instrument.is_some()
        || exchange.is_some();
    if profile != cli_args::QuickstartProfile::Ccxt && ccxt_options_used {
        eprintln!("[止步] --days、--timeframe、--as-of-ms、--instrument、--exchange 仅适用于 --profile ccxt");
        std::process::exit(2);
    }
    let days = days.unwrap_or(30);
    let timeframe = timeframe.unwrap_or_else(|| "1h".into());
    let instrument = instrument.unwrap_or_else(|| "BTC/USDT:USDT.OKX".into());
    let exchange = exchange.unwrap_or_else(|| "okx".into());
    let root = project.unwrap_or_else(|| PathBuf::from(DEFAULT_PROJECT_DIR));
    let absolute = std::path::absolute(&root).unwrap_or_else(|_| root.clone());
    let runtime = absolute.join(RUNTIME_FILE_NAME);
    let runtime_text = runtime.to_string_lossy().into_owned();
    let root_text = absolute.to_string_lossy().into_owned();
    let profile_name = profile.as_str();
    let (strategy_name, init_profile, ccxt_strategy) = match profile {
        cli_args::QuickstartProfile::Builtin => {
            let selected = strategy.as_deref().unwrap_or(QUICKSTART_STRATEGY);
            let kind = match qx_strategy::BuiltinStrategyKind::parse(selected) {
                Ok(kind) => kind,
                Err(error) => {
                    eprintln!("[止步] quickstart 策略无效：{error}");
                    std::process::exit(2);
                }
            };
            if kind.needs_reference_leg() {
                eprintln!(
                    "[止步] quickstart 暂不接受双腿策略 {}；请用 builtin-strategies 查看可用回测入口并运行 multi-builtin。",
                    kind.name()
                );
                std::process::exit(2);
            }
            (Some(kind.name()), None, None)
        }
        cli_args::QuickstartProfile::Ashare => {
            if strategy.is_some() {
                eprintln!("[止步] --strategy 仅适用于 builtin profile");
                std::process::exit(2);
            }
            (None, Some("ashare"), None)
        }
        cli_args::QuickstartProfile::Ccxt => {
            let selected = strategy.as_deref().unwrap_or("ema_cross");
            let kind = match qx_strategy::BuiltinStrategyKind::parse(selected) {
                Ok(kind) => kind,
                Err(error) => {
                    eprintln!("[止步] quickstart 策略无效：{error}");
                    std::process::exit(2);
                }
            };
            if kind.needs_reference_leg() {
                eprintln!(
                    "[止步] CCXT quickstart 暂不接受双腿策略 {}；请用 builtin-strategies 查看可用入口。",
                    kind.name()
                );
                std::process::exit(2);
            }
            (None, Some("ccxt"), Some(kind.name().to_owned()))
        }
    };
    let ccxt_preflight = if profile == cli_args::QuickstartProfile::Ccxt {
        let range = match quickstart_ccxt_window(days, &timeframe, as_of_ms) {
            Ok(range) => range,
            Err(error) => {
                eprintln!("[止步] CCXT quickstart 参数无效：{error}");
                std::process::exit(2);
            }
        };
        let exchange_id = exchange.to_ascii_lowercase();
        if !matches!(exchange_id.as_str(), "okx" | "binance" | "bitget") {
            eprintln!("[止步] 公共行情交易所仅支持 okx、binance、bitget");
            std::process::exit(2);
        }
        let expected_suffix = exchange_id.to_ascii_uppercase();
        if instrument
            .rsplit_once('.')
            .is_none_or(|(_, suffix)| suffix != expected_suffix)
            || !instrument.contains(':')
        {
            eprintln!(
                "[止步] 标的必须是带结算币的永续合约并以 .{expected_suffix} 结尾，例如 BTC/USDT:USDT.{expected_suffix}"
            );
            std::process::exit(2);
        }
        let public_config = absolute.join("qianxing.ccxt.public.json");
        if public_config.exists() && !force {
            eprintln!(
                "[止步] 公共 CCXT 配置已存在: {}；如确认覆盖，请显式添加 --force",
                public_config.display()
            );
            std::process::exit(2);
        }
        Some((range, exchange_id))
    } else {
        None
    };
    let days_text = days.to_string();
    let mut retry_arguments = vec!["quickstart", root_text.as_str()];
    if profile != cli_args::QuickstartProfile::Builtin {
        retry_arguments.extend(["--profile", profile_name]);
    }
    if let Some(strategy) = strategy.as_deref() {
        retry_arguments.extend(["--strategy", strategy]);
    }
    if profile == cli_args::QuickstartProfile::Ccxt {
        retry_arguments.extend([
            "--days",
            days_text.as_str(),
            "--timeframe",
            timeframe.as_str(),
            "--instrument",
            instrument.as_str(),
            "--exchange",
            exchange.as_str(),
        ]);
    }
    if html {
        retry_arguments.push("--html");
    }
    if force {
        retry_arguments.push("--force");
    }
    let retry = command_line(&retry_arguments);
    println!(
        "quickstart 使用 {profile_name} profile，依次执行 5 步（建项目 → 静态检查 → 回测 → 读回摘要 → 安全状态），只写 {}：",
        absolute.display()
    );
    if let Some(strategy) = strategy_name.or(ccxt_strategy.as_deref()) {
        println!("内置策略：{strategy}");
    }
    let mut init_arguments = vec!["init", runtime_text.as_str()];
    if let Some(strategy) = strategy_name {
        init_arguments.extend(["--strategy", strategy]);
    }
    if let Some(profile) = init_profile {
        init_arguments.extend(["--profile", profile]);
    }
    if force {
        init_arguments.push("--force");
    }
    let init_outcome = if let Some((_, exchange_id)) = ccxt_preflight.as_ref() {
        run_init_with_ccxt_selection(&runtime, force, exchange_id, &instrument)
    } else {
        run_init_with_profile(&runtime, force, strategy_name, init_profile, false)
    };
    require(
        "建项目",
        &command_line(&init_arguments),
        &retry,
        // quickstart owns the full guided flow; do not interleave `init`'s
        // standalone follow-up command with this profile-specific sequence.
        init_outcome,
    );
    require(
        "静态检查",
        &command_line(&["doctor", &runtime_text]),
        &retry,
        run_doctor(&runtime, false),
    );
    if let Some(strategy_name) = ccxt_strategy.as_deref() {
        let Some((range, exchange_id)) = ccxt_preflight.as_ref() else {
            eprintln!("[止步] CCXT quickstart 缺少已校验的公共行情参数");
            std::process::exit(2);
        };
        let public_config = absolute.join("qianxing.ccxt.public.json");
        if let Err(error) =
            write_public_ccxt_config(&public_config, exchange_id, &instrument, force)
        {
            eprintln!("[止步] 创建只读公共行情配置失败：{error}");
            std::process::exit(2);
        }
        let ccxt_command = command_line(&[
            "backtest",
            "ccxt-builtin",
            public_config.to_string_lossy().as_ref(),
            strategy_name,
            &instrument,
            &range.start_ms.to_string(),
            &range.end_ms.to_string(),
            &timeframe,
            "--config",
            &runtime_text,
            "--use-public-default-fees",
        ]);
        let as_of_label = as_of_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "system_utc".into());
        println!(
            "[真实公开行情 · 只读] exchange={exchange_id} instrument={instrument} timeframe={timeframe} days={days} as_of={as_of_label} start_ms={} end_ms={}；不加载密钥、不提交订单",
            range.start_ms, range.end_ms
        );
        let backtest_outcome = run_ccxt_builtin_backtest(CcxtBuiltinBacktestInput {
            ccxt_config_path: &public_config,
            strategy_name,
            instrument: &instrument,
            timeframe: &timeframe,
            start_ms: range.start_ms,
            end_ms: range.end_ms,
            spec_path: None,
            quantity: CCXT_QUICKSTART_DEFAULT_QUANTITY,
            runtime_config_path: Some(&runtime),
            use_public_default_fees: true,
        })
        .and_then(|snapshot| {
            let (manifest_path, fingerprint) = write_ccxt_quickstart_project_manifest(
                &runtime,
                &absolute,
                &snapshot.manifest_path,
                strategy_name,
            )?;
            println!(
                "[ProjectManifest] 已绑定真实行情与策略 strategy={} fingerprint={} path={}",
                strategy_name,
                fingerprint,
                manifest_path.display()
            );
            Ok(())
        });
        require(
            "获取真实公开行情并回测",
            &ccxt_command,
            &retry,
            backtest_outcome,
        );
    } else {
        require(
            "跑一轮回测",
            &command_line(&["backtest", &runtime_text]),
            &retry,
            run_unified_backtest(Some(&runtime), None, None),
        );
    }
    let report_command = if html {
        command_line(&["report", &runtime_text, "--html"])
    } else {
        command_line(&["report", &runtime_text])
    };
    require(
        if html {
            "复核摘要并生成 HTML 报告"
        } else {
            "读回摘要"
        },
        &report_command,
        &retry,
        run_report(&runtime, false, html),
    );
    require(
        "看安全状态",
        &command_line(&["status", &runtime_text]),
        &retry,
        run_status(&runtime, false),
    );
    let (files, truncated) = project_file_count(&absolute, PROJECT_FILE_COUNT_BUDGET);
    println!();
    let counted = if truncated {
        format!(
            "项目里共 {files} 份文件（已在 {PROJECT_FILE_COUNT_BUDGET} 个目录的计数预算处截断，实际可能更多）"
        )
    } else {
        format!("项目里共 {files} 份文件")
    };
    println!("你刚做完了 5 步：建项目 → 静态检查 → 回测 → 读回摘要 → 安全状态，{counted}。");
    println!(
        "回测产物在 {} 下（summary / equity.csv / fills.csv / run.json / record.json 与数据集清单）。",
        absolute.join("data").display()
    );
    println!("下一步三条命令：");
    let next_profile = match profile {
        cli_args::QuickstartProfile::Builtin => "ashare",
        cli_args::QuickstartProfile::Ashare => "ccxt",
        cli_args::QuickstartProfile::Ccxt => "crypto-sandbox-acceptance",
    };
    for step in [
        command_line(&["report", &runtime_text, "--json"]),
        command_line(&["strategy", "list"]),
        command_line(&[
            "init",
            absolute
                .join(next_profile)
                .join("qianxing.runtime.json")
                .to_string_lossy()
                .as_ref(),
            "--profile",
            next_profile,
        ]),
    ] {
        println!("  {step}");
    }
    // #261：这里同样不印注定报错的命令。paper-check 要 paper profile 那份带启用 Scheduler
    // worker 的运行时，对 macd 项目直接敲它以 2 退出，所以只在收尾句点名它的前置条件。
    println!(
        "本地 Paper 验收（{}）要先把运行时换成 paper profile 那份模板，否则会如实报「Paper 主链路缺少启用的 Scheduler worker」。",
        command_line(&["paper-check", &runtime_text])
    );
    println!(
        "全部入口：{}；这一行是哪个构建：{}",
        command_line(&["help"]),
        command_line(&["version"])
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CcxtQuickstartWindow {
    start_ms: u64,
    end_ms: u64,
}

fn quickstart_ccxt_window(
    days: u32,
    timeframe: &str,
    as_of_ms: Option<u64>,
) -> Result<CcxtQuickstartWindow, String> {
    if !(1..=365).contains(&days) {
        return Err("--days 必须在 1 到 365 之间".into());
    }
    if timeframe.ends_with('M') {
        return Err("当前真实行情 quickstart 只接受固定长度周期，例如 15m 或 1h".into());
    }
    let interval_ms = crate::timeframe_to_ms(timeframe)?;
    let window_ms = u64::from(days)
        .checked_mul(86_400_000)
        .ok_or_else(|| "回测窗口超出范围".to_string())?;
    // fetch_ohlcv's end_ms is inclusive. Count the candles that cover the requested
    // duration, then derive the first candle open from that count; subtracting the
    // whole window directly adds one extra candle whenever the window divides evenly.
    let requested_bars = window_ms / interval_ms + u64::from(window_ms % interval_ms != 0);
    if requested_bars == 0 || requested_bars > 100_000 {
        return Err(
            "此 quickstart 最多获取 100000 根 Bar；请缩短天数或使用更大的 timeframe".into(),
        );
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("读取系统 UTC 时间失败: {error}"))?
        .as_millis();
    let now_ms = u64::try_from(now_ms).map_err(|_| "系统 UTC 毫秒时间戳溢出".to_string())?;
    let cutoff_ms = as_of_ms.unwrap_or(now_ms);
    if cutoff_ms > now_ms {
        return Err("--as-of-ms 不能晚于当前 UTC 时间".into());
    }
    let end_ms = (cutoff_ms / interval_ms)
        .checked_mul(interval_ms)
        .and_then(|aligned| aligned.checked_sub(interval_ms))
        .ok_or_else(|| "系统 UTC 时间早于有效回测窗口".to_string())?;
    let bar_span_ms = requested_bars
        .checked_sub(1)
        .and_then(|count| count.checked_mul(interval_ms))
        .ok_or_else(|| "回测 Bar 数量超出范围".to_string())?;
    let start_ms = end_ms
        .checked_sub(bar_span_ms)
        .ok_or_else(|| "回测开始时间超出可表示范围".to_string())?;
    Ok(CcxtQuickstartWindow { start_ms, end_ms })
}

fn write_public_ccxt_config(
    path: &Path,
    exchange: &str,
    instrument: &str,
    force: bool,
) -> Result<(), String> {
    if path.exists() && !force {
        return Err(format!(
            "公共 CCXT 配置已存在: {}；如确认覆盖，请显式添加 --force",
            path.display()
        ));
    }
    let payload = serde_json::json!({
        "exchange_id": exchange,
        "sandbox": false,
        "enable_rate_limit": true,
        "timeout_ms": 30000,
        "default_type": "swap",
        "options": {},
        "instruments": [instrument],
    });
    let serialized = serde_json::to_vec_pretty(&payload)
        .map_err(|error| format!("编码公共 CCXT 配置失败: {error}"))?;
    std::fs::write(path, serialized)
        .map_err(|error| format!("写入 {} 失败: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{quickstart_ccxt_window, CCXT_QUICKSTART_DEFAULT_QUANTITY};

    #[test]
    fn ccxt_quickstart_default_is_one_display_unit() {
        assert_eq!(
            qx_core::Quantity::from_i64(CCXT_QUICKSTART_DEFAULT_QUANTITY).raw(),
            qx_core::SCALE
        );
    }

    #[test]
    fn ccxt_window_uses_closed_bars_without_an_inclusive_endpoint_extra_bar() {
        let window = quickstart_ccxt_window(3, "1h", None).unwrap();
        assert_eq!(window.end_ms % 3_600_000, 0);
        assert_eq!((window.end_ms - window.start_ms) / 3_600_000 + 1, 72);
        assert_eq!(window.end_ms - window.start_ms + 3_600_000, 3 * 86_400_000);
    }

    #[test]
    fn ccxt_long_window_bar_counts_match_six_month_and_bitget_api_cadence() {
        let six_months = quickstart_ccxt_window(180, "15m", None).unwrap();
        assert_eq!(
            (six_months.end_ms - six_months.start_ms) / 900_000 + 1,
            17_280
        );
        let bitget_classic_limit = quickstart_ccxt_window(52, "15m", None).unwrap();
        assert_eq!(
            (bitget_classic_limit.end_ms - bitget_classic_limit.start_ms) / 900_000 + 1,
            4_992
        );
    }

    #[test]
    fn ccxt_as_of_timestamp_makes_the_window_end_reproducible() {
        let as_of_ms = 1_700_000_100_000;
        let first = quickstart_ccxt_window(30, "15m", Some(as_of_ms)).unwrap();
        let second = quickstart_ccxt_window(30, "15m", Some(as_of_ms)).unwrap();
        let interval_ms = 900_000;
        let expected_end = (as_of_ms / interval_ms) * interval_ms - interval_ms;
        assert_eq!(first, second);
        assert_eq!(first.end_ms, expected_end);
        assert_eq!(
            (first.end_ms - first.start_ms) / interval_ms + 1,
            30 * 24 * 4
        );
        assert!(quickstart_ccxt_window(30, "15m", Some(u64::MAX)).is_err());
    }

    #[test]
    fn ccxt_window_rounds_up_non_dividing_timeframes_to_cover_requested_days() {
        let window = quickstart_ccxt_window(1, "7m", None).unwrap();
        assert_eq!((window.end_ms - window.start_ms) / 420_000 + 1, 206);
        assert!(window.end_ms - window.start_ms + 420_000 >= 86_400_000);
    }

    #[test]
    fn ccxt_window_rejects_large_or_unbounded_periods() {
        assert!(quickstart_ccxt_window(0, "1h", None).is_err());
        assert!(quickstart_ccxt_window(366, "1h", None).is_err());
        assert!(quickstart_ccxt_window(100, "1m", None).is_err());
        assert!(quickstart_ccxt_window(30, "1M", None).is_err());
    }
}
