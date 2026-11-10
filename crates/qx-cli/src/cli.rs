//! CLI 入口分派：clap 派生的 [`Command`] 变体 → 具体处理器。参数语法见 `cli_args.rs`；机读路由名册由参数源生成并经架构门禁校验。本模块只留一次 `Command` match、横幅机读判定与错误出口；业务实现复用 crate 根函数。

use super::cli_args::{BacktestCommand, Cli, Command, ConfigCommand, RunCommand, StrategyCommand};
mod backtest_builtin;
mod backtest_command;
mod config_command;
mod dataset_command;
mod execution_command;
mod host_command;
mod imports;
mod market_data_command;
mod project_command;
mod runtime_command;
mod selfcheck_command;
mod strategy_command;
mod worker_command;
use backtest_builtin::dispatch_builtin_backtest;
use imports::*;

pub(crate) fn run() {
    let argv: Vec<String> = std::env::args().collect();
    #[cfg(feature = "bench-prepared-evaluation")]
    if argv
        .get(1)
        .is_some_and(|arg| arg == "bench-prepared-evaluation")
    {
        if let Err(error) = crate::workers::run_prepared_evaluation_bench(&argv) {
            eprintln!("[qx-cli · PreparedEvaluation benchmark] {error}");
            std::process::exit(2);
        }
        return;
    }
    if matches!(
        argv.get(1).map(String::as_str),
        Some("help") | Some("--help") | Some("-h")
    ) {
        print_banner();
        print_cli_help();
        return;
    }
    let alias = argv.get(1).map(String::as_str).unwrap_or_default();
    if alias == "--version" || alias == "-V" {
        build_identity::print_identity();
        return;
    }
    let cli = match Cli::try_parse_from(argv.iter().map(String::as_str)) {
        Ok(cli) => cli,
        Err(error) => usage_errors::fail_usage(error),
    };
    let Some(command) = cli.command else {
        print_banner();
        selfcheck_command::dispatch_default();
        return;
    };
    if !command.machine_output() {
        print_banner();
    }
    command.reject_shadowed_backtest_inputs();
    match command {
        Command::Init {
            output,
            force,
            strategy,
            profile,
        } => project_command::dispatch_init(output, force, strategy, profile),
        Command::Quickstart {
            project,
            profile,
            html,
            force,
        } => project_command::dispatch_quickstart(project, html, force, profile),
        Command::Guide(args) => project_command::dispatch_guide(args),
        Command::Doctor {
            path,
            json,
            capabilities,
            capability_id,
            venue,
            product,
            account_mode,
            account_model,
            margin_mode,
            operation,
        } => runtime_command::dispatch_doctor(
            path,
            json,
            capabilities,
            capability_id,
            venue,
            product,
            account_mode,
            account_model,
            margin_mode,
            operation,
        ),
        Command::Config { action } => config_command::dispatch(action),
        Command::Run { action } => runtime_command::dispatch_run(action),
        Command::Status { path, json } => runtime_command::dispatch_status(path, json),
        Command::Report(arguments) => dispatch_report(arguments),
        Command::LiveCheck { path, json } => runtime_command::dispatch_live_check(path, json),
        Command::RuntimeCheck { path, json } => runtime_command::dispatch_runtime_check(path, json),
        Command::Strategy { action } => strategy_command::dispatch(action),
        Command::Backtest {
            runtime,
            frame,
            spec,
            action,
        } => backtest_command::dispatch(runtime, frame, spec, action),
        Command::BuiltinStrategies => backtest_command::dispatch_builtin_strategies(),
        Command::FastBacktest { manifest } => backtest_command::dispatch_fast_manifest(manifest),
        Command::DatasetIngest {
            frame,
            dataset_id,
            version,
            data_dir,
            snapshot_context,
            timezone,
        } => dataset_command::dispatch_ingest(
            frame,
            dataset_id,
            version,
            data_dir,
            snapshot_context,
            timezone,
        ),
        Command::DatasetBundle {
            bundle,
            data_dir,
            bars,
            snapshot,
        } => dataset_command::dispatch_bundle(bundle, data_dir, bars, snapshot),
        Command::DataValidate(args) => dataset_command::dispatch_validate(args),
        Command::DataValidateDepth(args) => dataset_command::dispatch_validate_depth(args),
        Command::DataSnapshot(args) => dataset_command::dispatch_snapshot(args),
        Command::Plan(args) => dataset_command::dispatch_plan(args),
        Command::CcxtMarketSpec {
            config,
            instrument,
            output,
        } => market_data_command::dispatch_market_spec(config, instrument, output),
        Command::Serve { path } => host_command::dispatch_serve(path),
        Command::Console(args) => host_command::dispatch_console(args),
        Command::Supervise {
            path,
            allow_unmanaged_roles,
        } => host_command::dispatch_supervise(path, allow_unmanaged_roles),
        Command::SchedulerWorker {
            path,
            worker_id,
            once,
        } => worker_command::dispatch_scheduler_worker(path, worker_id, once),
        Command::StrategyWorker {
            path,
            worker_id,
            once,
        } => worker_command::dispatch_strategy_worker(path, worker_id, once),
        Command::PaperWorker {
            path,
            worker_id,
            once,
        } => worker_command::dispatch_paper_worker(path, worker_id, once),
        Command::BinanceWorker {
            path,
            worker_id,
            once,
        } => worker_command::dispatch_binance_worker(path, worker_id, once),
        Command::CcxtWorker {
            path,
            worker_id,
            ccxt_config,
            once,
        } => worker_command::dispatch_ccxt_worker(path, worker_id, ccxt_config, once),
        Command::CcxtFetchOhlcv {
            ccxt_config,
            instrument,
            start_ms,
            end_ms,
            output,
            timeframe,
        } => market_data_command::dispatch_fetch_ohlcv(
            ccxt_config,
            instrument,
            start_ms,
            end_ms,
            output,
            timeframe,
        ),
        Command::OutboxRelay {
            root,
            url,
            subject_prefix,
            limit,
        } => worker_command::dispatch_outbox_relay(root, url, subject_prefix, limit),
        Command::OutboxRelayPostgres {
            runtime,
            url,
            subject_prefix,
            limit,
        } => worker_command::dispatch_outbox_relay_postgres(runtime, url, subject_prefix, limit),
        Command::OutboxRelayWorker {
            path,
            worker_id,
            once,
        } => {
            worker_command::dispatch_outbox_relay_worker(path, worker_id, once);
        }
        Command::EventConsumerWorker {
            path,
            worker_id,
            once,
        } => {
            worker_command::dispatch_event_consumer_worker(path, worker_id, once);
        }
        Command::ConsumerDlqReplay {
            runtime,
            group_id,
            event_id,
        } => worker_command::dispatch_consumer_dlq_replay(runtime, group_id, event_id),
        Command::RecoveryChild { arguments } => host_command::dispatch_recovery_child(arguments),
        Command::BinancePublicProbe {
            network,
            instrument,
        } => execution_command::dispatch_binance_public_probe(network, instrument),
        Command::BinancePrivateProbe { path, worker_id } => {
            execution_command::dispatch_binance_private_probe(path, worker_id)
        }
        Command::BinanceSubmitOrder {
            path,
            worker_id,
            command_path,
        } => execution_command::dispatch_binance_submit_order(path, worker_id, command_path),
        Command::PaperSubmitOrder { path, command_path } => {
            execution_command::dispatch_paper_submit_order(path, command_path)
        }
        Command::CcxtSubmitOrder(args) => execution_command::dispatch_ccxt_submit_order(args),
        Command::PaperE2e {
            path,
            quote_price_raw,
            quote_qty_raw,
        } => execution_command::dispatch_paper_e2e(path, quote_price_raw, quote_qty_raw),
        Command::PaperCheck { path, json } => execution_command::dispatch_paper_check(path, json),
        Command::Reconcile { path, worker_id } => {
            execution_command::dispatch_reconcile(path, worker_id)
        }
        Command::Ecosystem(args) => selfcheck_command::dispatch_ecosystem(args.json),
        Command::Paper => selfcheck_command::dispatch_paper(),
        Command::App(args) => app_commands::dispatch(args.action), // T2-2/G1：业务交给应用层
        Command::All(args) => selfcheck_command::dispatch_all(args.json),
        Command::Verify(args) => selfcheck_command::dispatch_verify(args.json),
        Command::Version => build_identity::print_identity(),
        Command::StorageAdoptPostgres(args) => {
            storage_adopt::dispatch_postgres_schema_adoption(args)
        }
        Command::Evidence(args) => evidence_signature::dispatch(args),
    }
}
