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

use super::*;

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
        .map(|name| format!(" {}", project_root.join(name).display()))
        .unwrap_or_default();
    let frame = project_root.join(&frame).display().to_string();
    // 绑定了策略就让它自己的配置跑；否则退回不读运行时的内置策略入口。
    Some(if bound_strategy(config) {
        format!("{program} backtest {} {frame}{spec_arg}", output.display())
    } else {
        format!("{program} backtest builtin sma_cross {frame}{spec_arg}")
    })
}

/// 这份运行时有没有绑定策略入口。`init_backtest_step` 与推荐流程的收尾行共用这一个谓词：
/// 只有绑定运行时的回测会写 summary，`backtest builtin` 那一支只把结果印在屏幕上。
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
    let runtime = output.display().to_string();
    if profile == "paper" {
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
) -> String {
    let program = cli_args::PROGRAM_NAME;
    let runtime = output.display().to_string();
    let strategy_line = strategy
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
    format!(
        "# Qianxing 本地项目\n\n运行时配置：`{runtime}`。profile=`{profile}`。{strategy_line}\n\n## 推荐流程\n\n```text\n{}\n```\n\n带有可读本地数据集的项目会额外生成 `qianxing.project.json`，其中引用运行时、数据集版本和可识别的策略，结构遵循仓库的 `project-manifest-v1` schema。回测产物在 `data/qianxing/runs/`，包含摘要、权益曲线、成交明细、RunManifest 和带文件摘要的 RunRecord。\n\n初始化生成的样例文件只用于本地回测和 Paper 验收，不包含交易密钥，也不会自动发送真实订单。CCXT/多交易所 profile 只生成公共配置和凭据引用，必须自行配置环境变量后再做 sandbox 验收。\n",
        flow.join("\n")
    )
}
