//! Strategy subcommand adapters for the CLI facade.

use super::*;

pub(super) fn dispatch(action: Option<StrategyCommand>) {
    match action {
        None | Some(StrategyCommand::List) => print_builtin_strategies(),
        Some(StrategyCommand::Init {
            name,
            output,
            bars,
            force,
        }) => {
            let output =
                output.unwrap_or_else(|| PathBuf::from(format!("qianxing.strategy.{name}.json")));
            if let Err(error) = run_strategy_init(&name, &output, bars.as_deref(), force) {
                eprintln!("[qx-cli · CLI] 策略初始化失败: {error}");
                std::process::exit(2);
            }
        }
        Some(StrategyCommand::Backtest {
            runtime,
            bars,
            spec,
            dataset_manifest,
        }) => {
            if let Err(error) = run_strategy_backtest_with_quality_manifest(
                &runtime,
                &bars,
                spec.as_deref(),
                dataset_manifest.as_deref(),
            ) {
                eprintln!("[qx-cli · CLI] 策略回测失败: {error}");
                std::process::exit(2);
            }
        }
        Some(StrategyCommand::DiagnoseDepth {
            artifact,
            output,
            html,
        }) => {
            if let Err(error) = crate::report_multi_market_depth::generate_builtin_depth_diagnostics(
                &artifact, &output,
            ) {
                eprintln!("[qx-cli · CLI] 深度策略诊断失败: {error}");
                std::process::exit(2);
            }
            if html {
                let options = crate::report_command::ReportOptions {
                    as_json: false,
                    html: true,
                    evidence: false,
                    output: None,
                    against: &[],
                    benchmark_bars: None,
                    benchmark_calendar: None,
                    benchmark_csv: None,
                    strategy_diagnostics: Some(&output),
                    xlsx: None,
                };
                if let Err(error) =
                    crate::report_command::run_report_with_benchmark(&artifact, options)
                {
                    eprintln!("[qx-cli · CLI] 深度策略交互报告生成失败: {error}");
                    std::process::exit(2);
                }
            }
        }
        Some(StrategyCommand::DiagnoseMultiBar {
            artifact,
            output,
            html,
        }) => {
            if let Err(error) = crate::report_multi_market::generate_builtin_bar_diagnostics(
                &artifact, &output, html,
            ) {
                eprintln!("[qx-cli · CLI] 多市场 Bar 策略诊断失败: {error}");
                std::process::exit(2);
            }
        }
        Some(StrategyCommand::DiagnoseMultiAccount {
            artifact,
            spec,
            output,
            html,
        }) => {
            if let Err(error) = crate::strategy_multi_account_diagnostics::generate_multi_account_builtin_bar_diagnostics(
                    &artifact,
                    &spec,
                    &output,
                ) {
                    eprintln!("[qx-cli · CLI] 多账户 Bar 策略诊断失败: {error}");
                    std::process::exit(2);
                }
            if html {
                let options = crate::report_command::ReportOptions {
                    as_json: false,
                    html: true,
                    evidence: false,
                    output: None,
                    against: &[],
                    benchmark_bars: None,
                    benchmark_calendar: None,
                    benchmark_csv: None,
                    strategy_diagnostics: Some(&output),
                    xlsx: None,
                };
                if let Err(error) =
                    crate::report_command::run_report_with_benchmark(&artifact, options)
                {
                    eprintln!("[qx-cli · CLI] 多账户策略交互报告生成失败: {error}");
                    std::process::exit(2);
                }
            }
        }
        Some(StrategyCommand::DiagnoseBar {
            summary,
            spec,
            output,
            html,
        }) => {
            if let Err(error) = crate::strategy_diagnostics::generate_builtin_bar_diagnostics(
                &summary, &spec, &output, html,
            ) {
                eprintln!("[qx-cli · CLI] Bar 策略诊断失败: {error}");
                std::process::exit(2);
            }
        }
    }
}
