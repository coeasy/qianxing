//! Backtest command family routing for unified, builtin, multi-builtin, CCXT, and depth runs.

use super::*;

pub(super) fn dispatch_builtin_strategies() {
    print_builtin_strategies();
}

pub(super) fn dispatch_fast_manifest(manifest: PathBuf) {
    if let Err(error) = run_fast_backtest_manifest(&manifest) {
        eprintln!("[qx-cli · CLI] 快速批量回测失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch(
    runtime: Option<PathBuf>,
    frame: Option<PathBuf>,
    spec: Option<PathBuf>,
    action: Option<BacktestCommand>,
) {
    match action {
        None => {
            if let Err(error) =
                run_unified_backtest(runtime.as_deref(), frame.as_deref(), spec.as_deref())
            {
                eprintln!("[qx-cli · CLI] 统一策略回测失败: {error}");
                std::process::exit(2);
            }
        }
        Some(BacktestCommand::Builtin {
            strategy,
            frame,
            spec,
            quantity,
            config,
            dataset_manifest,
        }) => dispatch_builtin_backtest(strategy, frame, spec, quantity, config, dataset_manifest),
        Some(BacktestCommand::MultiBuiltin {
            strategy,
            primary_bar,
            reference_bar,
            primary_spec,
            reference_spec,
            positional_quantity,
            quantity,
            funding_bps,
            root,
            config,
            primary_dataset_manifest,
            reference_dataset_manifest,
        }) => {
            let quantity = quantity.or(positional_quantity).unwrap_or(1);
            if let Err(error) = run_multi_builtin_backtest(
                &strategy,
                &primary_bar,
                &reference_bar,
                primary_spec.as_deref(),
                reference_spec.as_deref(),
                primary_dataset_manifest.as_deref(),
                reference_dataset_manifest.as_deref(),
                quantity,
                funding_bps.unwrap_or(0),
                root.as_deref(),
                config.as_deref(),
            ) {
                eprintln!("[qx-cli · CLI] 多腿内置策略回测失败: {error}");
                std::process::exit(2);
            }
        }
        Some(BacktestCommand::CcxtBuiltin {
            ccxt_config,
            strategy,
            instrument,
            start_ms,
            end_ms,
            timeframe,
            spec,
            quantity,
            config,
            use_public_default_fees,
        }) => {
            if let Err(error) = run_ccxt_builtin_backtest(CcxtBuiltinBacktestInput {
                ccxt_config_path: &ccxt_config,
                strategy_name: &strategy,
                instrument: &instrument,
                timeframe: &timeframe,
                start_ms,
                end_ms,
                spec_path: spec.as_deref(),
                quantity: quantity.unwrap_or(1),
                runtime_config_path: config.as_deref(),
                use_public_default_fees,
            }) {
                eprintln!("[qx-cli · CLI] CCXT 内置策略回测失败: {error}");
                std::process::exit(2);
            }
        }
        Some(BacktestCommand::Book {
            fill_tier,
            root,
            strategy,
            frame,
            spec,
            quantity,
            fee_bps,
            market_impact_bps,
            latency_snapshots,
            config,
            dataset_manifest,
        }) => {
            if let Err(error) = run_depth_backtest_with_quality_manifest(DepthBacktestCommand {
                tier: &fill_tier,
                strategy_name: &strategy,
                frame_path: &frame,
                spec_path: spec.as_deref(),
                quantity: quantity.unwrap_or(1),
                fee_bps,
                execution: DepthExecutionModel {
                    latency_snapshots: latency_snapshots.unwrap_or(0),
                    market_impact_bps: market_impact_bps.unwrap_or(0),
                },
                root: &root,
                runtime_config_path: config.as_deref(),
                quality_manifest_path: dataset_manifest.as_deref(),
            }) {
                eprintln!("[qx-cli · CLI] 深度档位回测失败: {error}");
                std::process::exit(2);
            }
        }
    }
}
