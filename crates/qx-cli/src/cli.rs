//! CLI 入口分派：命令名 → 具体处理器。
//!
//! 本模块只负责参数解析与错误出口（退出码、`--json` 判定），
//! 不承载任何交易/存储/风控语义；所有业务实现仍从 crate 根复用同一批函数。

use super::*;

pub(crate) fn run() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    let run_json = mode == "run"
        && matches!(
            std::env::args().nth(2).as_deref(),
            Some("doctor") | Some("live-check") | Some("runtime-check") | Some("report")
        )
        && std::env::args().any(|argument| argument == "--json");
    let machine_output = matches!(mode.as_str(), "config" | "status" | "doctor" | "report")
        && std::env::args().any(|argument| argument == "--json")
        || matches!(mode.as_str(), "live-check" | "runtime-check")
            && std::env::args().any(|argument| argument == "--json")
        || run_json;
    if !machine_output {
        println!("牵星 Qianxing — 分级校准，量天定位\n");
    }
    if matches!(mode.as_str(), "help" | "--help" | "-h") {
        print_cli_help();
        return;
    }
    if mode == "run" {
        let arguments = std::env::args().skip(2).collect::<Vec<_>>();
        if let Err(error) = run_unified_command(&arguments) {
            eprintln!("统一运行入口失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "init" {
        let arguments = std::env::args().skip(2).collect::<Vec<_>>();
        let force = arguments.iter().any(|argument| argument == "--force");
        let mut positional = Vec::new();
        let mut strategy_name = None;
        let mut profile = None;
        let mut index = 0;
        while index < arguments.len() {
            let argument = &arguments[index];
            if argument == "--strategy" {
                index += 1;
                strategy_name = arguments.get(index).cloned();
                if strategy_name.is_none() {
                    eprintln!("init --strategy 缺少策略名称");
                    std::process::exit(2);
                }
            } else if let Some(value) = argument.strip_prefix("--strategy=") {
                if value.is_empty() {
                    eprintln!("init --strategy= 缺少策略名称");
                    std::process::exit(2);
                }
                strategy_name = Some(value.to_string());
            } else if argument == "--profile" {
                index += 1;
                profile = arguments.get(index).cloned();
                if profile.is_none() {
                    eprintln!("init --profile 缺少场景名称");
                    std::process::exit(2);
                }
            } else if let Some(value) = argument.strip_prefix("--profile=") {
                if value.is_empty() {
                    eprintln!("init --profile= 缺少场景名称");
                    std::process::exit(2);
                }
                profile = Some(value.to_string());
            } else if !argument.starts_with('-') {
                positional.push(argument.clone());
            }
            index += 1;
        }
        let output = positional
            .first()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("qianxing.runtime.json"));
        if let Err(error) =
            run_init_with_profile(&output, force, strategy_name.as_deref(), profile.as_deref())
        {
            eprintln!("初始化失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "doctor" {
        let path = std::env::args()
            .skip(2)
            .find(|argument| !argument.starts_with('-'))
            .map(PathBuf::from)
            .unwrap_or_else(default_runtime_path);
        if let Err(error) = run_doctor(&path, std::env::args().any(|argument| argument == "--json"))
        {
            eprintln!("Doctor 失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "report" {
        let path = std::env::args()
            .skip(2)
            .find(|argument| !argument.starts_with('-'))
            .map(PathBuf::from)
            .unwrap_or_else(default_runtime_path);
        if let Err(error) = run_report(&path, std::env::args().any(|argument| argument == "--json"))
        {
            eprintln!("报告查看失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "config" {
        let action = std::env::args().nth(2).unwrap_or_else(|| "validate".into());
        let path = std::env::args()
            .nth(3)
            .map(PathBuf::from)
            .unwrap_or_else(default_runtime_path);
        let result = match action.as_str() {
            "explain" => {
                run_config_explain(&path, std::env::args().any(|argument| argument == "--json"))
            }
            "validate" => match read_runtime_config(&path) {
                Ok(config) => {
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
            "fingerprint" => run_config_fingerprint(&path),
            "lock" => {
                let output = std::env::args()
                    .nth(4)
                    .filter(|value| !value.starts_with('-'))
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        let stem = path
                            .file_stem()
                            .and_then(|value| value.to_str())
                            .unwrap_or("qianxing.runtime");
                        path.with_file_name(format!("{stem}.locked.json"))
                    });
                run_config_lock(
                    &path,
                    &output,
                    std::env::args().any(|argument| argument == "--force"),
                )
            }
            _ => Err("config 仅支持 explain、validate、fingerprint 或 lock".into()),
        };
        if let Err(error) = result {
            eprintln!("配置命令失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "status" {
        let path = std::env::args()
            .nth(2)
            .filter(|value| !value.starts_with('-'))
            .map(PathBuf::from)
            .unwrap_or_else(default_runtime_path);
        if let Err(error) = run_status(&path, std::env::args().any(|argument| argument == "--json"))
        {
            eprintln!("状态查看失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "strategy" {
        let action = std::env::args().nth(2).unwrap_or_else(|| "list".into());
        match action.as_str() {
            "list" => {
                for kind in BuiltinStrategyKind::ALL {
                    println!("{}\t{}", kind.name(), kind.description());
                }
            }
            "init" => {
                let name = match std::env::args().nth(3) {
                    Some(value) => value,
                    None => {
                        eprintln!("strategy init 需要 strategy 名称");
                        std::process::exit(2);
                    }
                };
                let positional = std::env::args()
                    .skip(4)
                    .filter(|argument| !argument.starts_with('-'))
                    .collect::<Vec<_>>();
                let output = positional
                    .first()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(format!("qianxing.strategy.{name}.json")));
                let bars = positional.get(1).map(PathBuf::from);
                let force = std::env::args().any(|argument| argument == "--force");
                if let Err(error) = run_strategy_init(&name, &output, bars.as_deref(), force) {
                    eprintln!("策略初始化失败: {error}");
                    std::process::exit(2);
                }
            }
            "backtest" => {
                let runtime = match std::env::args().nth(3) {
                    Some(value) => PathBuf::from(value),
                    None => {
                        eprintln!("strategy backtest 需要 runtime.json bar-frame.json");
                        std::process::exit(2);
                    }
                };
                let bars = match std::env::args().nth(4) {
                    Some(value) => PathBuf::from(value),
                    None => {
                        eprintln!("strategy backtest 缺少 bar-frame.json");
                        std::process::exit(2);
                    }
                };
                let spec = std::env::args().nth(5).map(PathBuf::from);
                if let Err(error) = run_strategy_backtest(&runtime, &bars, spec.as_deref()) {
                    eprintln!("策略回测失败: {error}");
                    std::process::exit(2);
                }
            }
            _ => {
                eprintln!("strategy 仅支持 list、init、backtest");
                std::process::exit(2);
            }
        }
        return;
    }
    if mode == "backtest" {
        let mut arguments: Vec<String> = std::env::args().skip(2).collect();
        let entry = arguments.first().cloned().unwrap_or_default();
        // 命令行型回测入口（builtin / multi-builtin / book / ccxt-builtin）历史上把风控规则
        // 写死在代码里；`--config` 让它们与 strategy 回测吃同一份 `strategy.risk_rules`。
        let runtime_config = match take_config_flag(&mut arguments) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("backtest {error}");
                std::process::exit(2);
            }
        };
        match Some(entry.as_str()) {
            Some("builtin") => {
                let (Some(strategy), Some(frame)) = (arguments.get(1), arguments.get(2)) else {
                    eprintln!(
                        "backtest builtin 需要 <strategy> <bar-frame.json> [market-spec.json] [quantity] [--config runtime.json]"
                    );
                    std::process::exit(2);
                };
                let spec = arguments.get(3).map(PathBuf::from);
                let quantity = parse_backtest_quantity(arguments.get(4), "backtest builtin");
                if let Err(error) = run_builtin_backtest(
                    strategy,
                    Path::new(frame),
                    spec.as_deref(),
                    quantity,
                    runtime_config.as_deref(),
                ) {
                    eprintln!("内置策略回测失败: {error}");
                    std::process::exit(2);
                }
                return;
            }
            Some("multi-builtin") => {
                let mut funding_bps = None;
                let mut root = None;
                let mut quantity_flag = None;
                let mut positional: Vec<String> = Vec::new();
                let mut index = 1;
                while index < arguments.len() {
                    match arguments[index].as_str() {
                        "--funding-bps" | "--root" | "--quantity" => {
                            let key = arguments[index].clone();
                            index += 1;
                            let value = match arguments.get(index) {
                                Some(value) => value.clone(),
                                None => {
                                    eprintln!("backtest multi-builtin 的 {key} 缺少取值");
                                    std::process::exit(2);
                                }
                            };
                            match key.as_str() {
                                "--funding-bps" => funding_bps = Some(value),
                                "--quantity" => quantity_flag = Some(value),
                                _ => root = Some(value),
                            }
                        }
                        other if !other.starts_with('-') => positional.push(other.to_string()),
                        other => {
                            eprintln!("backtest multi-builtin 未知参数: {other}");
                            std::process::exit(2);
                        }
                    }
                    index += 1;
                }
                let (Some(strategy), Some(primary), Some(reference)) =
                    (positional.first(), positional.get(1), positional.get(2))
                else {
                    eprintln!("backtest multi-builtin 需要 <strategy> <primary-bar.json> <reference-bar.json> [primary-spec.json] [reference-spec.json] [quantity] [--funding-bps <n>] [--quantity <n>] [--root <产物目录>] [--config runtime.json]");
                    std::process::exit(2);
                };
                let primary_spec = positional.get(3).map(PathBuf::from);
                let reference_spec = positional.get(4).map(PathBuf::from);
                let quantity = match quantity_flag {
                    Some(value) => parse_backtest_quantity(Some(&value), "backtest multi-builtin"),
                    None => parse_backtest_quantity(
                        positional.get(5).cloned().as_ref(),
                        "backtest multi-builtin",
                    ),
                };
                let funding_bps = match funding_bps {
                    Some(value) => match value.parse::<i64>() {
                        Ok(parsed) if (0..=10_000).contains(&parsed) => parsed,
                        _ => {
                            eprintln!("backtest multi-builtin --funding-bps 必须在 0..=10000 内");
                            std::process::exit(2);
                        }
                    },
                    None => 0,
                };
                if let Err(error) = run_multi_builtin_backtest(
                    strategy,
                    Path::new(primary),
                    Path::new(reference),
                    primary_spec.as_deref(),
                    reference_spec.as_deref(),
                    quantity,
                    funding_bps,
                    root.as_deref().map(Path::new),
                    runtime_config.as_deref(),
                ) {
                    eprintln!("多腿内置策略回测失败: {error}");
                    std::process::exit(2);
                }
                return;
            }
            Some("ccxt-builtin") => {
                let (Some(ccxt_config), Some(strategy), Some(instrument), Some(start), Some(end)) = (
                    arguments.get(1),
                    arguments.get(2),
                    arguments.get(3),
                    arguments.get(4),
                    arguments.get(5),
                ) else {
                    eprintln!("backtest ccxt-builtin 需要 <ccxt-config> <strategy> <instrument> <start_ms> <end_ms> [timeframe] [market-spec.json] [quantity] [--config runtime.json]");
                    std::process::exit(2);
                };
                let (Ok(start_ms), Ok(end_ms)) = (start.parse::<u64>(), end.parse::<u64>()) else {
                    eprintln!("backtest ccxt-builtin start_ms/end_ms 非法");
                    std::process::exit(2);
                };
                let timeframe = arguments.get(6).cloned().unwrap_or_else(|| "1h".into());
                let spec = arguments.get(7).map(PathBuf::from);
                let quantity = parse_backtest_quantity(arguments.get(8), "backtest ccxt-builtin");
                if let Err(error) = run_ccxt_builtin_backtest(
                    Path::new(ccxt_config),
                    strategy,
                    instrument,
                    &timeframe,
                    start_ms,
                    end_ms,
                    spec.as_deref(),
                    quantity,
                    runtime_config.as_deref(),
                ) {
                    eprintln!("CCXT 内置策略回测失败: {error}");
                    std::process::exit(2);
                }
                return;
            }
            Some("book") => {
                let mut tier = None;
                let mut root = None;
                let mut fee_bps = None;
                let mut positional: Vec<String> = Vec::new();
                let mut index = 1;
                while index < arguments.len() {
                    match arguments[index].as_str() {
                        "--fill-tier" | "--root" | "--fee-bps" => {
                            let key = arguments[index].clone();
                            index += 1;
                            let value = match arguments.get(index) {
                                Some(value) => value.clone(),
                                None => {
                                    eprintln!("backtest book 的 {key} 缺少取值");
                                    std::process::exit(2);
                                }
                            };
                            match key.as_str() {
                                "--fill-tier" => tier = Some(value),
                                "--root" => root = Some(value),
                                _ => fee_bps = Some(value),
                            }
                        }
                        other if !other.starts_with('-') => positional.push(other.to_string()),
                        other => {
                            eprintln!("backtest book 未知参数: {other}");
                            std::process::exit(2);
                        }
                    }
                    index += 1;
                }
                let (Some(strategy), Some(frame)) = (positional.first(), positional.get(1)) else {
                    eprintln!("backtest book 需要 --fill-tier <l1|l2> --root <产物目录> <strategy> <depth-frame.json> [market-spec.json] [quantity] [--fee-bps <n>] [--config runtime.json]");
                    std::process::exit(2);
                };
                let spec = positional.get(2).map(PathBuf::from);
                let quantity =
                    parse_backtest_quantity(positional.get(3).cloned().as_ref(), "backtest book");
                let fee_bps = match fee_bps {
                    Some(value) => match value.parse::<i128>() {
                        Ok(parsed) => parsed,
                        Err(_) => {
                            eprintln!("backtest book --fee-bps 非法: {value}");
                            std::process::exit(2);
                        }
                    },
                    None => 5,
                };
                let (Some(tier), Some(root)) = (tier, root) else {
                    eprintln!("backtest book 必须显式给出 --fill-tier 与 --root");
                    std::process::exit(2);
                };
                if let Err(error) = run_depth_backtest(
                    &tier,
                    strategy,
                    Path::new(frame),
                    spec.as_deref(),
                    quantity,
                    fee_bps,
                    Path::new(&root),
                    runtime_config.as_deref(),
                ) {
                    eprintln!("深度档位回测失败: {error}");
                    std::process::exit(2);
                }
                return;
            }
            _ => {
                let runtime = arguments.first().map(PathBuf::from);
                let frame = arguments.get(1).map(PathBuf::from);
                let spec = arguments.get(2).map(PathBuf::from);
                if let Err(error) =
                    run_unified_backtest(runtime.as_deref(), frame.as_deref(), spec.as_deref())
                {
                    eprintln!("统一策略回测失败: {error}");
                    std::process::exit(2);
                }
                return;
            }
        }
    }
    if mode == "dataset-ingest" {
        let frame = match std::env::args().nth(2) {
            Some(value) => PathBuf::from(value),
            None => {
                eprintln!("dataset-ingest 需要 bar-frame.json dataset-id version data-dir");
                std::process::exit(2);
            }
        };
        let dataset_id = match std::env::args().nth(3) {
            Some(value) => value,
            None => {
                eprintln!("dataset-ingest 缺少 dataset-id");
                std::process::exit(2);
            }
        };
        let version = match std::env::args().nth(4) {
            Some(value) => value,
            None => {
                eprintln!("dataset-ingest 缺少 version");
                std::process::exit(2);
            }
        };
        let data_root = match std::env::args().nth(5) {
            Some(value) => PathBuf::from(value),
            None => {
                eprintln!("dataset-ingest 缺少 data-dir");
                std::process::exit(2);
            }
        };
        if let Err(error) = run_dataset_ingest(&frame, &dataset_id, &version, &data_root) {
            eprintln!("数据集摄取失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "dataset-bundle" {
        let bundle = match std::env::args().nth(2) {
            Some(value) => PathBuf::from(value),
            None => {
                eprintln!("dataset-bundle 需要 bundle.json data-dir");
                std::process::exit(2);
            }
        };
        let data_root = match std::env::args().nth(3) {
            Some(value) => PathBuf::from(value),
            None => {
                eprintln!("dataset-bundle 缺少 data-dir");
                std::process::exit(2);
            }
        };
        let bars_frame = std::env::args().nth(4).map(PathBuf::from);
        if let Err(error) = run_dataset_bundle(&bundle, &data_root, bars_frame.as_deref()) {
            eprintln!("数据集 Bundle 保存失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "builtin-strategies" {
        for kind in BuiltinStrategyKind::ALL {
            println!("{}\t{}", kind.name(), kind.description());
        }
        return;
    }
    if mode == "live-check" {
        let path = std::env::args()
            .skip(2)
            .find(|argument| !argument.starts_with('-'))
            .unwrap_or_else(|| "deploy/qianxing.runtime.production.example.json".into());
        if let Err(error) = run_live_check(
            Path::new(&path),
            std::env::args().any(|argument| argument == "--json"),
        ) {
            eprintln!("实盘前置检查失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "recovery-child" {
        let args = std::env::args().collect::<Vec<_>>();
        if let Err(error) = run_recovery_child(&args) {
            eprintln!("跨进程恢复子进程失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "outbox-relay" {
        #[cfg(feature = "nats")]
        {
            let root = match std::env::args().nth(2) {
                Some(value) => PathBuf::from(value),
                None => {
                    eprintln!("outbox-relay 需要 data-root nats-url subject-prefix [limit]");
                    std::process::exit(2);
                }
            };
            let url = match std::env::args().nth(3) {
                Some(value) => value,
                None => {
                    eprintln!("outbox-relay 缺少 nats-url");
                    std::process::exit(2);
                }
            };
            let prefix = match std::env::args().nth(4) {
                Some(value) => value,
                None => {
                    eprintln!("outbox-relay 缺少 subject-prefix");
                    std::process::exit(2);
                }
            };
            let limit = std::env::args()
                .nth(5)
                .map(|value| value.parse::<usize>())
                .transpose()
                .unwrap_or_else(|_| {
                    eprintln!("outbox-relay limit 非法");
                    std::process::exit(2);
                })
                .unwrap_or(100);
            if let Err(error) = run_file_outbox_relay(&root, &url, &prefix, limit) {
                eprintln!("Outbox relay 失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(not(feature = "nats"))]
        {
            eprintln!("outbox-relay 需要使用 --features nats 构建 qx-cli");
            std::process::exit(2);
        }
    }
    if mode == "outbox-relay-postgres" {
        #[cfg(all(feature = "nats", feature = "postgres"))]
        {
            let runtime_path = match std::env::args().nth(2) {
                Some(value) => PathBuf::from(value),
                None => {
                    eprintln!(
                        "outbox-relay-postgres 需要 runtime.json nats-url subject-prefix [limit]"
                    );
                    std::process::exit(2);
                }
            };
            let url = match std::env::args().nth(3) {
                Some(value) => value,
                None => {
                    eprintln!("outbox-relay-postgres 缺少 nats-url");
                    std::process::exit(2);
                }
            };
            let prefix = match std::env::args().nth(4) {
                Some(value) => value,
                None => {
                    eprintln!("outbox-relay-postgres 缺少 subject-prefix");
                    std::process::exit(2);
                }
            };
            let limit = std::env::args()
                .nth(5)
                .map(|value| value.parse::<usize>())
                .transpose()
                .unwrap_or_else(|_| {
                    eprintln!("outbox-relay-postgres limit 非法");
                    std::process::exit(2);
                })
                .unwrap_or(100);
            if let Err(error) = run_postgres_outbox_relay(&runtime_path, &url, &prefix, limit) {
                eprintln!("PostgreSQL Outbox relay 失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(not(all(feature = "nats", feature = "postgres")))]
        {
            eprintln!("outbox-relay-postgres 需要使用 --features 'nats postgres' 构建 qx-cli");
            std::process::exit(2);
        }
    }
    if mode == "outbox-relay-worker" {
        #[cfg(feature = "nats")]
        {
            let path = std::env::args()
                .nth(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("deploy/qianxing.runtime.example.json"));
            let worker_id = match std::env::args().nth(3) {
                Some(value) => value,
                None => {
                    eprintln!("outbox-relay-worker 需要 runtime.json worker-id [--once]");
                    std::process::exit(2);
                }
            };
            let once = std::env::args().any(|argument| argument == "--once");
            if let Err(error) = run_outbox_relay_worker(&path, &worker_id, once) {
                eprintln!("Outbox relay worker 失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(not(feature = "nats"))]
        {
            eprintln!("outbox-relay-worker 需要使用 --features nats 构建 qx-cli");
            std::process::exit(2);
        }
    }
    if mode == "event-consumer-worker" {
        #[cfg(feature = "nats")]
        {
            let path = std::env::args()
                .nth(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("deploy/qianxing.runtime.example.json"));
            let worker_id = match std::env::args().nth(3) {
                Some(value) => value,
                None => {
                    eprintln!("event-consumer-worker 需要 runtime.json worker-id [--once]");
                    std::process::exit(2);
                }
            };
            let once = std::env::args().any(|argument| argument == "--once");
            if let Err(error) = run_event_consumer_worker(&path, &worker_id, once) {
                eprintln!("Event consumer worker 失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(not(feature = "nats"))]
        {
            eprintln!("event-consumer-worker 需要使用 --features nats 构建 qx-cli");
            std::process::exit(2);
        }
    }
    if mode == "consumer-dlq-replay" {
        #[cfg(feature = "nats")]
        {
            let path = match std::env::args().nth(2) {
                Some(value) => PathBuf::from(value),
                None => {
                    eprintln!("consumer-dlq-replay 需要 runtime.json group-id event-id");
                    std::process::exit(2);
                }
            };
            let group_id = match std::env::args().nth(3) {
                Some(value) => value,
                None => {
                    eprintln!("consumer-dlq-replay 缺少 group-id");
                    std::process::exit(2);
                }
            };
            let event_id = match std::env::args().nth(4) {
                Some(value) => value,
                None => {
                    eprintln!("consumer-dlq-replay 缺少 event-id");
                    std::process::exit(2);
                }
            };
            if let Err(error) = run_dead_letter_replay(&path, &group_id, &event_id) {
                eprintln!("DLQ 重放失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(not(feature = "nats"))]
        {
            eprintln!("consumer-dlq-replay 需要使用 --features nats 构建 qx-cli");
            std::process::exit(2);
        }
    }
    if mode == "runtime-check" {
        let path = std::env::args()
            .skip(2)
            .find(|argument| !argument.starts_with('-'))
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        if let Err(error) = run_runtime_check(
            Path::new(&path),
            std::env::args().any(|argument| argument == "--json"),
        ) {
            eprintln!("运行时配置校验失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "supervise" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let allow_unmanaged_roles =
            std::env::args().any(|argument| argument == "--allow-unmanaged-roles");
        if let Err(error) = run_process_supervisor(Path::new(&path), allow_unmanaged_roles) {
            eprintln!("进程监督器停止: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "serve" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        if let Err(error) = run_runtime_api(Path::new(&path)) {
            eprintln!("运行时 API 启动失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "scheduler-worker" || mode == "strategy-worker" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let worker_id = match std::env::args().nth(3) {
            Some(worker_id) => worker_id,
            None => {
                eprintln!("{mode} 需要 worker-id");
                std::process::exit(2);
            }
        };
        let once = std::env::args().any(|argument| argument == "--once");
        let result = if mode == "scheduler-worker" {
            run_scheduler_worker(Path::new(&path), &worker_id, once)
        } else {
            run_strategy_worker(Path::new(&path), &worker_id, once)
        };
        if let Err(error) = result {
            eprintln!("{mode} 启动/运行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "binance-worker" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let worker_id = match std::env::args().nth(3) {
            Some(worker_id) => worker_id,
            None => {
                eprintln!("binance-worker 需要 worker-id");
                std::process::exit(2);
            }
        };
        let once = std::env::args().any(|argument| argument == "--once");
        if let Err(error) = run_binance_worker(Path::new(&path), &worker_id, once) {
            eprintln!("Binance worker 启动/运行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "binance-public-probe" {
        let network = std::env::args().nth(2).unwrap_or_else(|| "testnet".into());
        let instrument = std::env::args()
            .nth(3)
            .unwrap_or_else(|| "BTCUSDT.BINANCE".into());
        let testnet = match network.as_str() {
            "testnet" => true,
            "mainnet" => false,
            _ => {
                eprintln!("binance-public-probe network 必须是 testnet 或 mainnet");
                std::process::exit(2);
            }
        };
        if let Err(error) = run_binance_public_probe(testnet, &instrument) {
            eprintln!("Binance public probe 失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "binance-private-probe" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.production.example.json".into());
        let worker_id = std::env::args()
            .nth(3)
            .unwrap_or_else(|| "binance-execution-main".into());
        if let Err(error) = run_binance_private_probe(Path::new(&path), &worker_id) {
            eprintln!("Binance private probe 失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "ccxt-worker" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let worker_id = match std::env::args().nth(3) {
            Some(worker_id) => worker_id,
            None => {
                eprintln!("ccxt-worker 需要 worker-id");
                std::process::exit(2);
            }
        };
        let ccxt_config = match std::env::args().nth(4) {
            Some(config) => config,
            None => {
                eprintln!("ccxt-worker 需要 ccxt-config.json");
                std::process::exit(2);
            }
        };
        let once = std::env::args().any(|argument| argument == "--once");
        if let Err(error) =
            run_ccxt_worker(Path::new(&path), &worker_id, Path::new(&ccxt_config), once)
        {
            eprintln!("CCXT worker 启动/运行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "ccxt-fetch-ohlcv" {
        let ccxt_config = match std::env::args().nth(2) {
            Some(config) => config,
            None => {
                eprintln!("ccxt-fetch-ohlcv 需要 ccxt-config.json instrument start_ms end_ms output.json [timeframe]");
                std::process::exit(2);
            }
        };
        let instrument = match std::env::args().nth(3) {
            Some(instrument) => instrument,
            None => {
                eprintln!("ccxt-fetch-ohlcv 缺少 instrument");
                std::process::exit(2);
            }
        };
        let start_ms = match std::env::args().nth(4).and_then(|value| value.parse().ok()) {
            Some(value) => value,
            None => {
                eprintln!("ccxt-fetch-ohlcv start_ms 非法");
                std::process::exit(2);
            }
        };
        let end_ms = match std::env::args().nth(5).and_then(|value| value.parse().ok()) {
            Some(value) => value,
            None => {
                eprintln!("ccxt-fetch-ohlcv end_ms 非法");
                std::process::exit(2);
            }
        };
        let output = match std::env::args().nth(6) {
            Some(output) => output,
            None => {
                eprintln!("ccxt-fetch-ohlcv 缺少 output.json");
                std::process::exit(2);
            }
        };
        let timeframe = std::env::args().nth(7).unwrap_or_else(|| "1m".into());
        if let Err(error) = run_ccxt_fetch_ohlcv(
            Path::new(&ccxt_config),
            &instrument,
            &timeframe,
            start_ms,
            end_ms,
            Path::new(&output),
        ) {
            eprintln!("CCXT OHLCV 下载失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "fast-backtest" {
        let manifest = match std::env::args().nth(2) {
            Some(manifest) => manifest,
            None => {
                eprintln!("fast-backtest 需要 manifest.json");
                std::process::exit(2);
            }
        };
        if let Err(error) = run_fast_backtest_manifest(Path::new(&manifest)) {
            eprintln!("快速批量回测失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "ccxt-market-spec" {
        let config = match std::env::args().nth(2) {
            Some(config) => config,
            None => {
                eprintln!("ccxt-market-spec 需要 ccxt-config.json instrument output.json");
                std::process::exit(2);
            }
        };
        let instrument = match std::env::args().nth(3) {
            Some(instrument) => instrument,
            None => {
                eprintln!("ccxt-market-spec 缺少 instrument");
                std::process::exit(2);
            }
        };
        let output = match std::env::args().nth(4) {
            Some(output) => output,
            None => {
                eprintln!("ccxt-market-spec 缺少 output.json");
                std::process::exit(2);
            }
        };
        if let Err(error) =
            run_ccxt_market_spec(Path::new(&config), &instrument, Path::new(&output))
        {
            eprintln!("CCXT market spec 下载失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "binance-submit-order" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.production.example.json".into());
        let worker_id = match std::env::args().nth(3) {
            Some(worker_id) => worker_id,
            None => {
                eprintln!("binance-submit-order 需要 worker-id");
                std::process::exit(2);
            }
        };
        let command_path = match std::env::args().nth(4) {
            Some(command_path) => command_path,
            None => {
                eprintln!("binance-submit-order 需要 command.json");
                std::process::exit(2);
            }
        };
        if let Err(error) =
            run_binance_submit_order(Path::new(&path), &worker_id, Path::new(&command_path))
        {
            eprintln!("Binance SubmitOrder 执行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "paper-submit-order" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let command_path = match std::env::args().nth(3) {
            Some(command_path) => command_path,
            None => {
                eprintln!("paper-submit-order 需要 command.json");
                std::process::exit(2);
            }
        };
        if let Err(error) = run_paper_submit_order(Path::new(&path), Path::new(&command_path)) {
            eprintln!("Paper SubmitOrder 执行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "paper-worker" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
        let worker_id = match std::env::args().nth(3) {
            Some(worker_id) => worker_id,
            None => {
                eprintln!("paper-worker 需要 worker-id");
                std::process::exit(2);
            }
        };
        let once = std::env::args().any(|argument| argument == "--once");
        if let Err(error) = run_paper_execution_worker(Path::new(&path), &worker_id, once) {
            eprintln!("Paper Execution worker 启动/运行失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "paper-e2e" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.paper-strategy.example.json".into());
        if let Err(error) = run_paper_pipeline_once(Path::new(&path)) {
            eprintln!("Paper 主链路验收失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "paper-check" {
        let path = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "deploy/qianxing.runtime.paper-strategy.example.json".into());
        if let Err(error) = run_paper_pipeline_once(Path::new(&path)) {
            eprintln!("Paper 主链路验收失败: {error}");
            std::process::exit(2);
        }
        return;
    }
    if mode == "ecosystem" {
        run_ecosystem_smoke();
        return;
    }
    if mode == "paper" {
        run_paper_smoke();
        return;
    }
    if mode == "reconcile" {
        if let Some(path) = std::env::args().nth(2) {
            let worker_id = std::env::args()
                .nth(3)
                .unwrap_or_else(|| "reconciler-main".into());
            if let Err(error) = run_binance_worker(Path::new(&path), &worker_id, true) {
                eprintln!("Binance 对账失败: {error}");
                std::process::exit(2);
            }
            return;
        }
        // V10 §4.3：无参时曾用两份手写向量打印"差异"，看起来像真对账能力。
        // 对账必须同时点名本地与远端来源，缺任一即是用法错误。
        eprintln!(
            "reconcile 需要本地与远端两个来源：reconcile <runtime.json> [worker-id]\n  \
             本地来源 = 运行时配置指向的 EventLog 账本\n  \
             远端来源 = 该配置中 Binance worker 的交易所账户（凭据由配置引用，不在命令行传）"
        );
        std::process::exit(2);
    }

    if matches!(mode.as_str(), "all" | "verify") {
        selfcheck::run(if mode == "verify" {
            selfcheck::Scope::KernelOnly
        } else {
            selfcheck::Scope::Full
        });
        return;
    }
    eprintln!("未知命令: {mode}");
    print_cli_help();
    std::process::exit(2);
}
