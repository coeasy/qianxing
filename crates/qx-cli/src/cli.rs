//! CLI 入口分派：clap 派生的 [`Command`] 变体 → 具体处理器。参数语法见 `cli_args.rs`；机读路由名册由参数源生成并经架构门禁校验。本模块只留一次 `Command` match、横幅机读判定与错误出口；业务实现复用 crate 根函数。

use super::cli_args::{BacktestCommand, Cli, Command, ConfigCommand, RunCommand, StrategyCommand};
mod backtest_builtin;
mod imports;
use backtest_builtin::dispatch_builtin_backtest;
use imports::*;

fn dispatch_output_result(result: Result<(), String>, command: &str) {
    if let Err(error) = result {
        eprintln!("[qx-cli · CLI] {command} 输出失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(feature = "nats")]
fn dispatch_outbox_relay(root: PathBuf, url: String, subject_prefix: String, limit: Option<usize>) {
    if let Err(error) = run_file_outbox_relay(&root, &url, &subject_prefix, limit.unwrap_or(100)) {
        eprintln!("[qx-cli · CLI] Outbox relay 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
fn dispatch_outbox_relay(
    _root: PathBuf,
    _url: String,
    _subject_prefix: String,
    _limit: Option<usize>,
) {
    eprintln!("[qx-cli · CLI] outbox-relay 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

#[cfg(all(feature = "nats", feature = "postgres"))]
fn dispatch_outbox_relay_postgres(
    runtime: PathBuf,
    url: String,
    subject_prefix: String,
    limit: Option<usize>,
) {
    if let Err(error) =
        run_postgres_outbox_relay(&runtime, &url, &subject_prefix, limit.unwrap_or(100))
    {
        eprintln!("[qx-cli · CLI] PostgreSQL Outbox relay 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(all(feature = "nats", feature = "postgres")))]
fn dispatch_outbox_relay_postgres(
    _runtime: PathBuf,
    _url: String,
    _subject_prefix: String,
    _limit: Option<usize>,
) {
    eprintln!(
        "[qx-cli · CLI] outbox-relay-postgres 需要使用 --features 'nats postgres' 构建 qx-cli"
    );
    std::process::exit(2);
}

#[cfg(feature = "nats")]
fn dispatch_outbox_relay_worker(path: PathBuf, worker_id: String, once: bool) {
    if let Err(error) = run_outbox_relay_worker(&path, &worker_id, once) {
        eprintln!("[qx-cli · CLI] Outbox relay worker 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
fn dispatch_outbox_relay_worker(_path: PathBuf, _worker_id: String, _once: bool) {
    eprintln!("[qx-cli · CLI] outbox-relay-worker 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

#[cfg(feature = "nats")]
fn dispatch_event_consumer_worker(path: PathBuf, worker_id: String, once: bool) {
    if let Err(error) = run_event_consumer_worker(&path, &worker_id, once) {
        eprintln!("[qx-cli · CLI] Event consumer worker 失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
fn dispatch_event_consumer_worker(_path: PathBuf, _worker_id: String, _once: bool) {
    eprintln!("[qx-cli · CLI] event-consumer-worker 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

#[cfg(feature = "nats")]
fn dispatch_consumer_dlq_replay(runtime: PathBuf, group_id: String, event_id: String) {
    if let Err(error) = run_dead_letter_replay(&runtime, &group_id, &event_id) {
        eprintln!("[qx-cli · CLI] DLQ 重放失败: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(feature = "nats"))]
fn dispatch_consumer_dlq_replay(_runtime: PathBuf, _group_id: String, _event_id: String) {
    eprintln!("[qx-cli · CLI] consumer-dlq-replay 需要使用 --features nats 构建 qx-cli");
    std::process::exit(2);
}

pub(crate) fn run() {
    let argv: Vec<String> = std::env::args().collect();
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
        dispatch_output_result(selfcheck::run(selfcheck::Scope::Full, false), "自校验");
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
        } => {
            if strategy.as_deref() == Some("") {
                eprintln!("[qx-cli · CLI] init --strategy= 缺少策略名称");
                std::process::exit(2);
            }
            if profile.as_deref() == Some("") {
                eprintln!("[qx-cli · CLI] init --profile= 缺少场景名称");
                std::process::exit(2);
            }
            let output = output.unwrap_or_else(|| PathBuf::from("qianxing.runtime.json"));
            if let Err(error) =
                run_init_with_profile(&output, force, strategy.as_deref(), profile.as_deref())
            {
                eprintln!("[qx-cli · CLI] 初始化失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Quickstart {
            project,
            html,
            force,
        } => quickstart::run(project, html, force),
        Command::Guide(args) => guide::run(args),
        Command::Doctor { path, json } => {
            let path = path.unwrap_or_else(default_runtime_path);
            if let Err(error) = run_doctor(&path, json) {
                eprintln!("[qx-cli · CLI] Doctor 失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Config { action } => {
            let validate_path = match &action {
                Some(ConfigCommand::Validate { path }) => path.clone(),
                _ => None,
            };
            let result = match action {
                Some(ConfigCommand::Explain { path, json }) => {
                    run_config_explain(&path.unwrap_or_else(default_runtime_path), json)
                }
                Some(ConfigCommand::Validate { .. }) | None => match read_runtime_config(
                    &validate_path.clone().unwrap_or_else(default_runtime_path),
                ) {
                    Ok(config) => {
                        let path = validate_path.unwrap_or_else(default_runtime_path);
                        let (failures, warnings) = validate_runtime_references(&path, &config);
                        for warning in warnings {
                            println!("[WARN] {warning}");
                        }
                        if failures.is_empty() {
                            println!("[PASS] config validate 通过: {}", path.display());
                            Ok(())
                        } else {
                            for failure in &failures {
                                eprintln!("[FAIL] {failure}");
                            }
                            Err(format!("配置引用校验失败，共 {} 项", failures.len()))
                        }
                    }
                    Err(error) => Err(error),
                },
                Some(ConfigCommand::Fingerprint { path }) => {
                    run_config_fingerprint(&path.unwrap_or_else(default_runtime_path))
                }
                Some(ConfigCommand::Lock {
                    path,
                    output,
                    force,
                }) => {
                    let path = path.unwrap_or_else(default_runtime_path);
                    let output = output.unwrap_or_else(|| {
                        let stem = path
                            .file_stem()
                            .and_then(|value| value.to_str())
                            .unwrap_or("qianxing.runtime");
                        path.with_file_name(format!("{stem}.locked.json"))
                    });
                    run_config_lock(&path, &output, force)
                }
            };
            if let Err(error) = result {
                eprintln!("[qx-cli · CLI] 配置命令失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Run { action } => {
            let arguments = match action {
                Some(RunCommand::Backtest { arguments }) => run_arguments("backtest", arguments),
                Some(RunCommand::Paper { arguments }) => run_arguments("paper", arguments),
                Some(RunCommand::PaperCheck { arguments }) => {
                    run_arguments("paper-check", arguments)
                }
                Some(RunCommand::Doctor { arguments }) => run_arguments("doctor", arguments),
                Some(RunCommand::LiveCheck { arguments }) => run_arguments("live-check", arguments),
                Some(RunCommand::RuntimeCheck { arguments }) => {
                    run_arguments("runtime-check", arguments)
                }
                Some(RunCommand::Report { arguments }) => run_arguments("report", arguments),
                None => Vec::new(),
            };
            if let Err(error) = run_unified_command(&arguments) {
                eprintln!("[qx-cli · CLI] 统一运行入口失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Status { path, json } => {
            let path = path.unwrap_or_else(default_runtime_path);
            if let Err(error) = run_status(&path, json) {
                eprintln!("[qx-cli · CLI] 状态查看失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Report(arguments) => dispatch_report(arguments),
        Command::LiveCheck { path, json } => {
            if let Err(error) = run_live_check(&path, json) {
                eprintln!("[qx-cli · CLI] 实盘前置检查失败: {error}");
                std::process::exit(2);
            }
        }
        Command::RuntimeCheck { path, json } => {
            if let Err(error) = run_runtime_check(&path, json) {
                eprintln!("[qx-cli · CLI] 运行时配置校验失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Strategy { action } => match action {
            None | Some(StrategyCommand::List) => print_builtin_strategies(),
            Some(StrategyCommand::Init {
                name,
                output,
                bars,
                force,
            }) => {
                let output = output
                    .unwrap_or_else(|| PathBuf::from(format!("qianxing.strategy.{name}.json")));
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
                if let Err(error) =
                    crate::report_multi_market_depth::generate_builtin_depth_diagnostics(
                        &artifact, &output,
                    )
                {
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
        },
        Command::Backtest {
            runtime,
            frame,
            spec,
            action,
        } => match action {
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
            }) => {
                dispatch_builtin_backtest(strategy, frame, spec, quantity, config, dataset_manifest)
            }
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
        },
        Command::BuiltinStrategies => print_builtin_strategies(),
        Command::FastBacktest { manifest } => {
            if let Err(error) = run_fast_backtest_manifest(&manifest) {
                eprintln!("[qx-cli · CLI] 快速批量回测失败: {error}");
                std::process::exit(2);
            }
        }
        Command::DatasetIngest {
            frame,
            dataset_id,
            version,
            data_dir,
        } => usage_errors::exit_on_failure(
            run_dataset_ingest(&frame, &dataset_id, &version, &data_dir),
            "数据集摄取失败",
        ),
        Command::DatasetBundle {
            bundle,
            data_dir,
            bars,
        } => usage_errors::exit_on_failure(
            run_dataset_bundle(&bundle, &data_dir, bars.as_deref()),
            "数据集 Bundle 保存失败",
        ),
        Command::DataValidate(args) => {
            usage_errors::exit_on_failure(validate_dataset(&args), "数据集校验失败");
        }
        Command::DataValidateDepth(args) => {
            usage_errors::exit_on_failure(validate_depth_dataset(&args), "深度数据集校验失败");
        }
        Command::Plan(args) => usage_errors::exit_on_failure(plan_readout(&args), "规格读入失败"),
        Command::CcxtMarketSpec {
            config,
            instrument,
            output,
        } => {
            if let Err(error) = run_ccxt_market_spec(&config, &instrument, &output) {
                eprintln!("[qx-cli · CLI] CCXT market spec 下载失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Serve { path } => {
            if let Err(error) = run_runtime_api(&path) {
                eprintln!("[qx-cli · CLI] 运行时 API 启动失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Console(args) => {
            usage_errors::exit_on_failure(serve_console(&args), "控制台启动失败")
        }
        Command::Supervise {
            path,
            allow_unmanaged_roles,
        } => {
            if let Err(failure) = run_process_supervisor(&path, allow_unmanaged_roles) {
                eprintln!("[qx-cli · CLI] 进程监督器停止: {failure}");
                match failure {
                    qx_orchestrator::SuperviseFailure::Usage(_) => std::process::exit(2),
                    qx_orchestrator::SuperviseFailure::WorkerExit { raw_code, .. } => {
                        std::process::exit(match raw_code {
                            Some(code) if code != 0 => code,
                            _ => 128,
                        })
                    }
                }
            }
        }
        Command::SchedulerWorker {
            path,
            worker_id,
            once,
        } => {
            if let Err(error) = run_scheduler_worker(&path, &worker_id, once, false) {
                eprintln!("[qx-cli · CLI] scheduler-worker 启动/运行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::StrategyWorker {
            path,
            worker_id,
            once,
        } => {
            if let Err(error) = run_strategy_worker(&path, &worker_id, once, false) {
                eprintln!("[qx-cli · CLI] strategy-worker 启动/运行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::PaperWorker {
            path,
            worker_id,
            once,
        } => {
            if let Err(error) = run_paper_execution_worker(&path, &worker_id, once) {
                eprintln!("[qx-cli · CLI] Paper Execution worker 启动/运行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::BinanceWorker {
            path,
            worker_id,
            once,
        } => {
            if let Err(error) = run_binance_worker(&path, &worker_id, once) {
                eprintln!("[qx-cli · CLI] Binance worker 启动/运行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::CcxtWorker {
            path,
            worker_id,
            ccxt_config,
            once,
        } => {
            if let Err(error) = run_ccxt_worker(&path, &worker_id, &ccxt_config, once) {
                eprintln!("[qx-cli · CLI] CCXT worker 启动/运行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::CcxtFetchOhlcv {
            ccxt_config,
            instrument,
            start_ms,
            end_ms,
            output,
            timeframe,
        } => {
            if let Err(error) = run_ccxt_fetch_ohlcv(
                &ccxt_config,
                &instrument,
                &timeframe,
                start_ms,
                end_ms,
                &output,
            ) {
                eprintln!("[qx-cli · CLI] CCXT OHLCV 下载失败: {error}");
                std::process::exit(2);
            }
        }
        Command::OutboxRelay {
            root,
            url,
            subject_prefix,
            limit,
        } => dispatch_outbox_relay(root, url, subject_prefix, limit),
        Command::OutboxRelayPostgres {
            runtime,
            url,
            subject_prefix,
            limit,
        } => dispatch_outbox_relay_postgres(runtime, url, subject_prefix, limit),
        Command::OutboxRelayWorker {
            path,
            worker_id,
            once,
        } => {
            dispatch_outbox_relay_worker(path, worker_id, once);
        }
        Command::EventConsumerWorker {
            path,
            worker_id,
            once,
        } => {
            dispatch_event_consumer_worker(path, worker_id, once);
        }
        Command::ConsumerDlqReplay {
            runtime,
            group_id,
            event_id,
        } => dispatch_consumer_dlq_replay(runtime, group_id, event_id),
        Command::RecoveryChild { arguments } => {
            let mut argv = vec![String::new(), "recovery-child".to_string()];
            argv.extend(arguments);
            if let Err(error) = run_recovery_child(&argv) {
                eprintln!("[qx-cli · CLI] 跨进程恢复子进程失败: {error}");
                std::process::exit(2);
            }
        }
        Command::BinancePublicProbe {
            network,
            instrument,
        } => {
            let testnet = match network.as_str() {
                "testnet" => true,
                "mainnet" => false,
                _ => {
                    eprintln!(
                        "[qx-cli · CLI] binance-public-probe network 必须是 testnet 或 mainnet"
                    );
                    std::process::exit(2);
                }
            };
            if let Err(error) = run_binance_public_probe(testnet, &instrument) {
                eprintln!("[qx-cli · CLI] Binance public probe 失败: {error}");
                std::process::exit(2);
            }
        }
        Command::BinancePrivateProbe { path, worker_id } => {
            if let Err(error) = run_binance_private_probe(&path, &worker_id) {
                eprintln!("[qx-cli · CLI] Binance private probe 失败: {error}");
                std::process::exit(2);
            }
        }
        Command::BinanceSubmitOrder {
            path,
            worker_id,
            command_path,
        } => {
            if let Err(error) = run_binance_submit_order(&path, &worker_id, &command_path) {
                eprintln!("[qx-cli · CLI] Binance SubmitOrder 执行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::PaperSubmitOrder { path, command_path } => {
            if let Err(error) = run_paper_submit_order(&path, &command_path) {
                eprintln!("[qx-cli · CLI] Paper SubmitOrder 执行失败: {error}");
                std::process::exit(2);
            }
        }
        Command::CcxtSubmitOrder(args) => usage_errors::exit_on_failure(
            run_ccxt_submit_order(&args),
            "[qx-cli · CLI] CCXT SubmitOrder 执行失败",
        ),
        Command::PaperE2e { path } => {
            if let Err(error) = run_paper_pipeline_once(&path) {
                eprintln!("[qx-cli · CLI] Paper 主链路验收失败: {error}");
                std::process::exit(2);
            }
        }
        Command::PaperCheck { path, json } => {
            if let Err(error) = run_paper_check(&path, json) {
                eprintln!("[qx-cli · CLI] Paper 主链路验收失败: {error}");
                std::process::exit(2);
            }
        }
        Command::Reconcile { path, worker_id } => {
            if let Some(path) = path {
                if let Err(error) = run_binance_reconcile_once(&path, worker_id.as_deref()) {
                    eprintln!("[qx-cli · CLI] Binance 对账失败: {error}");
                    std::process::exit(2);
                }
            } else {
                eprintln!(
                    "reconcile 需要本地与远端两个来源：reconcile <runtime.json> [worker-id]\n  \
                     本地来源 = 运行时配置指向的 EventLog 账本\n  \
                     远端来源 = 该配置中 Binance worker 的交易所账户（凭据由配置引用，不在命令行传）"
                );
                std::process::exit(2);
            }
        }
        Command::Ecosystem(args) => {
            dispatch_output_result(run_ecosystem_smoke(args.json), "生态自检")
        }
        Command::Paper => dispatch_output_result(run_paper_smoke(true), "Paper 冒烟"),
        Command::App(args) => app_commands::dispatch(args.action), // T2-2/G1：业务交给应用层
        Command::All(args) => dispatch_output_result(
            selfcheck::run(selfcheck::Scope::Full, args.json),
            "全量自检",
        ),
        Command::Verify(args) => dispatch_output_result(
            selfcheck::run(selfcheck::Scope::KernelOnly, args.json),
            "内核自检",
        ),
        Command::Version => build_identity::print_identity(),
        Command::StorageAdoptPostgres(args) => {
            storage_adopt::dispatch_postgres_schema_adoption(args)
        }
        Command::Evidence(args) => evidence_signature::dispatch(args),
    }
}
