//! `init` / `strategy init` 的引导面：生成的 README 与首屏那条回测命令（易用性 U4 成功侧）。
//!
//! 这一层产出的不是配置而是**给使用者抄进终端的命令行**，它的判据是「这条命令在这份项目里
//! 真跑得动」，与 `init_project.rs` 挑模板、复制资产的职责不同源；它也是全仓唯一需要反复写
//! 程序名的地方，所以单独成模块，名字只从 `cli_args::PROGRAM_NAME` 取一份。
//!
//! 两条立案在这一格收口：
//! - #260 程序名：这里曾逐处硬编码 `qianxing`，而装好的机器上可执行文件叫 `qx-cli`，于是首跑
//!   的第二句必然是 command not found。
//! - #261 收尾命令：这里曾对 7 个 profile 一律印 `paper-check <runtime>`，但只有 `paper` 模板
//!   带着启用的 Scheduler worker，其余 profile 照抄必然撞上「Paper 主链路缺少启用的 Scheduler
//!   worker」并以 2 退出。现在分三支：paper 印 `paper-check`，带行情夹具的印 `report --json`
//!   （读上一条回测刚写出的摘要），连 BarFrame 都没复制的不印收尾行——宁缺不错。

use std::path::Path;

use qx_runtime::RuntimeConfig;
use qx_strategy::builtin_signal::{BuiltinSignalKnob, MACD_WINDOWS};
use qx_strategy::{BuiltinStrategy, BuiltinStrategyKind};

use crate::cli_args;

/// Render a generated command path so it works from any current directory and
/// remains a single shell argument when the project directory contains spaces.
pub(crate) fn command_path(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let display = absolute.display().to_string();
    if display.chars().any(char::is_whitespace) {
        format!("\"{display}\"")
    } else {
        display
    }
}

/// init 生成的项目里那条回测命令：只承认"文件真的被复制进项目、入口真的读得动这份绑定"的组合。
///
/// 以前 7 个 profile 打印同一行 `backtest <runtime> qianxing.bar-frame.example.json
/// qianxing.binance.spot.spec.json`，而 `base`/`paper` 没绑策略（必报"跨语言回测必须配置
/// strategy.python_module…"），`ccxt`/`multi-venue` 连 BarFrame 都没复制（必报"找不到文件"）。
/// 首屏命令自己先失败等于没有文档，所以宁可不印也不能印一条注定报错的命令。
/// 印出来的行情与规格路径一律带上项目目录：裸文件名等于把"先 cd 到项目目录"这条从未写在
/// 屏幕上的前提，变成一次必然的"找不到文件"。
pub(crate) fn init_backtest_step(
    project_root: &Path,
    output: &Path,
    config: &RuntimeConfig,
) -> Option<String> {
    let program = cli_args::PROGRAM_NAME;
    let copied = |name: &str| project_root.join(name).is_file().then(|| name.to_owned());
    let declared = config
        .strategy
        .bars_snapshot_path
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .and_then(copied);
    let frame = declared
        .or_else(|| copied("qianxing.bar-frame.example.json"))
        .or_else(|| copied("qianxing.ashare.bar-frame.example.json"))?;
    let spec = if frame.starts_with("qianxing.ashare.") {
        copied("qianxing.ashare.spot.spec.json")
    } else {
        copied("qianxing.binance.spot.spec.json")
    };
    let spec_arg = spec
        .map(|name| format!(" {}", command_path(&project_root.join(name))))
        .unwrap_or_default();
    let frame = command_path(&project_root.join(&frame));
    // 绑定了策略就让它自己的配置跑；否则退回不读运行时的内置策略入口。
    Some(if bound_strategy(config) {
        format!(
            "{program} backtest {} {frame}{spec_arg}",
            command_path(output)
        )
    } else {
        format!(
            "{program} backtest builtin sma_cross {frame}{spec_arg} --config {}",
            command_path(output)
        )
    })
}

/// 这份运行时有没有绑定策略入口。`init_backtest_step` 与推荐流程的收尾行共用这一个谓词：
/// 有绑定策略的项目走运行时策略入口；基础配置显式传给 builtin 入口，以保留项目数据目录与配置来源。
fn bound_strategy(config: &RuntimeConfig) -> bool {
    let strategy = &config.strategy;
    strategy.builtin_strategy.is_some()
        || strategy.python_module.is_some()
        || strategy.external_executable.is_some()
        || strategy.c_abi_library.is_some()
}

/// 推荐流程的收尾行（#261）。`paper-check` 只在 paper 模板那份带启用 Scheduler worker 的运行时
/// 上跑得动，`report --json` 要有上一条回测写出的 summary，两支都不满足就不印收尾行。
pub(crate) fn init_flow_tail(
    output: &Path,
    profile: &str,
    config: &RuntimeConfig,
) -> Option<String> {
    let program = cli_args::PROGRAM_NAME;
    let runtime = command_path(output);
    if matches!(profile, "paper" | "crypto-paper") {
        return Some(format!("{program} paper-check {runtime}"));
    }
    bound_strategy(config).then(|| format!("{program} report {runtime} --json"))
}

/// 项目里那份 `README.qianxing.md` 的正文：推荐流程逐条都是真命令，跑得动的才印。
pub(crate) fn init_readme(
    output: &Path,
    strategy: Option<&str>,
    profile: &str,
    backtest_step: Option<&str>,
    final_step: Option<&str>,
    config: &RuntimeConfig,
) -> String {
    let program = cli_args::PROGRAM_NAME;
    let runtime = command_path(output);
    let selected_strategy = strategy.or(config.strategy.builtin_strategy.as_deref());
    let strategy_line = selected_strategy
        .map(|name| format!("已绑定内置策略：`{name}`。"))
        .unwrap_or_else(|| {
            format!(
                "当前配置为基础运行时，可通过 `{program} init --strategy macd --force` 绑定内置策略。"
            )
        });
    let without_frame = format!(
        "本 profile 未附带 BarFrame，暂无本地回测命令；自备行情文件后走 `{program} strategy init`。"
    );
    let mut flow = vec![
        format!("{program} doctor {runtime}"),
        format!("{program} config explain {runtime}"),
        format!("{program} config validate {runtime}"),
        backtest_step.unwrap_or(without_frame.as_str()).to_string(),
    ];
    if let Some(step) = final_step {
        flow.push(step.to_string());
    }
    let execution_note = if matches!(
        profile,
        "ccxt" | "multi-venue" | "crypto-sandbox-acceptance"
    ) {
        "\n\n交易所执行 worker 默认关闭。安装每个市场对应的受信任数据供应方签名标的角色目录、核验市场规格并配置 sandbox 凭据后，才能手动启用；初始化不会伪造这些授权材料。"
    } else {
        ""
    };
    let strategy_assumptions = selected_strategy
        .and_then(|name| builtin_strategy_assumptions(name, config))
        .map(|details| format!("\n\n## 回测策略与假设\n\n{details}"))
        .unwrap_or_default();
    format!(
        "# Qianxing 本地项目\n\n运行时配置：`{runtime}`。profile=`{profile}`。{strategy_line}\n\n## 推荐流程\n\n```text\n{}\n```\n\n带有可读本地数据集的项目会额外生成 `qianxing.project.json`，其中引用运行时、数据集版本和可识别的策略，结构遵循仓库的 `project-manifest-v1` schema。回测产物在 `data/qianxing/runs/`，包含摘要、权益曲线、成交明细、RunManifest 和带文件摘要的 RunRecord。\n\n初始化生成的样例文件只用于本地回测和 Paper 验收，不包含交易密钥，也不会自动发送真实订单。CCXT/多交易所 profile 只生成公共配置和凭据引用，必须自行配置环境变量后再做 sandbox 验收。{execution_note}{strategy_assumptions}\n",
        flow.join("\n")
    )
}

fn builtin_strategy_assumptions(name: &str, runtime: &RuntimeConfig) -> Option<String> {
    let kind = BuiltinStrategyKind::parse(name).ok()?;
    let instrument = runtime
        .strategy
        .instrument
        .as_deref()
        .and_then(qx_core::InstrumentId::parse)
        .or_else(|| qx_core::InstrumentId::parse("BTCUSDT.BINANCE"))?;
    let config =
        qx_runtime::builtin_strategy_config_from_runtime(&runtime.strategy, &instrument).ok()?;
    let warmup = BuiltinStrategy::new(config.clone())
        .ok()?
        .minimum_history_bars();

    let parameters = if kind.signal_knobs().is_empty() {
        format!(
            "信号参数：MACD 使用内核固定窗口 {}/{}/{}；`builtin_fast_window`、`builtin_slow_window`、`builtin_period`、`builtin_threshold_bps` 均不参与该策略。",
            MACD_WINDOWS.0, MACD_WINDOWS.1, MACD_WINDOWS.2
        )
    } else {
        let values = kind
            .signal_knobs()
            .iter()
            .map(|knob| match knob {
                BuiltinSignalKnob::FastWindow => format!("fast_window={}", config.fast_window),
                BuiltinSignalKnob::SlowWindow => format!("slow_window={}", config.slow_window),
                BuiltinSignalKnob::Period => format!("period={}", config.period),
                BuiltinSignalKnob::ThresholdBps => {
                    format!("threshold_bps={}", config.threshold_bps)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("生效信号参数：{values}。未列出的 `builtin_*` 信号项不参与本策略。")
    };
    let data = runtime
        .strategy
        .bars_snapshot_path
        .as_deref()
        .map(|path| format!("K 线输入：`{path}`。初始化自带的行情文件是历史样例，只用于本地回测/Paper 验收；数据身份见 `qianxing.project.json`。"))
        .unwrap_or_else(|| "K 线输入：请在运行时配置 `strategy.bars_snapshot_path`。".into());
    let sizing = if let Some(notional) = runtime.strategy.builtin_target_notional_raw {
        format!("固定名义金额 sizing：`builtin_target_notional_raw={notional}`（1e-9 定点单位）。")
    } else if let Some(quantity) = runtime.strategy.builtin_quantity {
        format!("固定数量 sizing：`builtin_quantity={quantity}`（1e-9 定点裸值）。")
    } else {
        "Sizing：未配置固定名义金额或数量目标，使用策略运行时默认仓位，并继续经过风险门检查。"
            .into()
    };
    let fill_model = runtime
        .strategy
        .fill_model
        .as_deref()
        .unwrap_or("next_bar_open（引擎默认）");
    let costs = runtime
        .strategy
        .cost_rules_path
        .as_deref()
        .map(|path| format!("成本来源：运行时配置的成本规则 `strategy.cost_rules_path={path}`。"))
        .unwrap_or_else(|| {
            "成本来源：未配置 `strategy.cost_rules_path`，使用引擎默认费用规则；请在 RunManifest/摘要中核对实际费用。".into()
        });
    let risk = runtime
        .strategy
        .risk_rules
        .as_ref()
        .map(|rules| {
            format!(
                "风险规则：版本 `{}`，max_qty_raw={:?}，max_notional_raw={:?}，no_short={}。",
                rules.version, rules.max_qty_raw, rules.max_notional_raw, rules.no_short
            )
        })
        .unwrap_or_else(|| {
            "风险规则：未配置账户级自定义数量/名义额上限；运行时仍执行默认不变式与 reduce-only 检查。".into()
        });
    let short_policy = match runtime.strategy.allow_short {
        Some(true) => "允许建立空头（仍受账户风控与产品规则约束）".to_string(),
        Some(false) => "禁止建立空头".to_string(),
        None => {
            "未显式设置 allow_short；按产品默认值决定（现货/杠杆禁空，衍生品允许双向）".to_string()
        }
    };

    Some(format!(
        "- 策略：**{}**（`{}`）。\n- 预热：至少 **{warmup} 根 Bar** 后才可能产生首个信号；不足时不会提前交易。\n- {parameters}\n- {data}\n- {sizing}\n- 撮合模型：`{fill_model}`。OHLCV 回测不会重建盘口队列或深度。\n- {costs}\n- {risk}\n- 空头规则：{short_policy}。\n- 回测结果受数据区间、参数、成本规则、撮合模型和风控配置影响；比较策略时先固定这些输入，并以 RunManifest、RunRecord 与成交明细为准。",
        kind.description(),
        kind.name(),
    ))
}
