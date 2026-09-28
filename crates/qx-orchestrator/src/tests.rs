//! 编排与监督器的用例现场：角色→进程入口的分派表、非受管 Venue 的拒绝口径。
use super::*;
use qx_runtime::WorkerConfig;

fn example_config() -> RuntimeConfig {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.ccxt.example.json");
    let payload = std::fs::read_to_string(path).unwrap();
    RuntimeConfig::from_json(&payload).unwrap()
}

#[test]
fn worker_plan_is_deterministic_and_routes_ccxt_endpoints() {
    let config = example_config();
    let path = Path::new("deploy/runtime.json");
    let launches = plan_workers(&config, path, false).unwrap();
    assert_eq!(launches.len(), 6);
    assert!(launches.iter().any(|launch| {
        launch.worker_id == "ccxt-market-main"
            && launch.args.first().map(String::as_str) == Some("ccxt-worker")
            && launch
                .args
                .last()
                .map(|path| Path::new(path).ends_with("qianxing.ccxt.exchange.example.json"))
                == Some(true)
    }));
}

#[test]
fn worker_plan_rejects_unknown_venue_without_explicit_external_management() {
    let mut config = example_config();
    let worker = config
        .workers
        .iter_mut()
        .find(|worker| worker.role == WorkerRole::Execution)
        .unwrap();
    worker.venue_id = Some("unknown-venue".into());
    worker.endpoint = None;
    assert!(plan_workers(&config, Path::new("runtime.json"), false).is_err());
    assert!(plan_workers(&config, Path::new("runtime.json"), true).is_ok());
}

#[test]
fn worker_plan_routes_ccxt_user_stream_to_public_worker() {
    let mut config = example_config();
    config.workers.push(WorkerConfig {
        id: "ccxt-user-main".into(),
        role: WorkerRole::UserStream,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("okx".into()),
        endpoint: Some("qianxing.ccxt.exchange.example.json".into()),
        symbols: Vec::new(),
        settlement_currency: Some("USDT".into()),
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    let launches = plan_workers(&config, Path::new("deploy/runtime.json"), false).unwrap();
    let launch = launches
        .iter()
        .find(|launch| launch.worker_id == "ccxt-user-main")
        .unwrap();
    assert_eq!(launch.args.first().map(String::as_str), Some("ccxt-worker"));
}

/// Binance 家族按子串认：仓内 `venue_id` 实测有 `binance` / `BINANCE` /
/// `binance-testnet` 三种写法，整名相等会让 testnet 的行情 worker 静默落到
/// "非受管"分支，规划结果看起来正常但没有进程被拉起。
#[test]
fn worker_plan_routes_a_testnet_binance_venue_to_the_private_worker() {
    let mut config = example_config();
    config.workers.push(WorkerConfig {
        id: "binance-market-testnet".into(),
        role: WorkerRole::MarketData,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("binance-testnet".into()),
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: Some("USDT".into()),
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    let launches = plan_workers(&config, Path::new("deploy/runtime.json"), false).unwrap();
    let launch = launches
        .iter()
        .find(|launch| launch.worker_id == "binance-market-testnet")
        .unwrap();
    assert_eq!(
        launch.args.first().map(String::as_str),
        Some("binance-worker"),
        "binance-testnet 必须走私有 Binance worker，而不是被当成非受管角色"
    );
}

/// Paper 分支是家族判定的另一个消费点：大小写与首尾空白的不同写法仍要落进 Paper 域，
/// 而 `paper-proxy` 这种"以 paper 开头"的名字不是 Paper，不能被前缀匹配顺手收进来。
#[test]
fn worker_plan_routes_only_the_exact_paper_venue_to_the_local_worker() {
    let mut config = example_config();
    config.workers.push(WorkerConfig {
        id: "paper-execution-local".into(),
        role: WorkerRole::Execution,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some(" Paper ".into()),
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: Some("USDT".into()),
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    let launches = plan_workers(&config, Path::new("deploy/runtime.json"), false).unwrap();
    let launch = launches
        .iter()
        .find(|launch| launch.worker_id == "paper-execution-local")
        .unwrap();
    assert_eq!(
        launch.args.first().map(String::as_str),
        Some("paper-worker"),
        "「 Paper 」仍是 Paper 域：本地 Paper 执行 worker 不该被要求配 CCXT endpoint"
    );

    config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-execution-local")
        .unwrap()
        .venue_id = Some("paper-proxy".into());
    assert!(
        plan_workers(&config, Path::new("deploy/runtime.json"), false).is_err(),
        "paper-proxy 不是 Paper 域：把它当 Paper 起本地 worker 会让真实 Venue 静默走虚拟撮合"
    );
}

#[test]
fn worker_plan_routes_spread_recovery_to_the_matching_venue_worker() {
    let mut config = example_config();
    config.workers.push(WorkerConfig {
        id: "ccxt-recovery-main".into(),
        role: WorkerRole::SpreadRecovery,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some("okx".into()),
        endpoint: Some("qianxing.ccxt.exchange.example.json".into()),
        symbols: Vec::new(),
        settlement_currency: Some("USDT".into()),
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });
    let launches = plan_workers(&config, Path::new("deploy/runtime.json"), false).unwrap();
    let launch = launches
        .iter()
        .find(|launch| launch.worker_id == "ccxt-recovery-main")
        .unwrap();
    assert_eq!(launch.args.first().map(String::as_str), Some("ccxt-worker"));
    assert!(launch
        .args
        .last()
        .is_some_and(|path| { Path::new(path).ends_with("qianxing.ccxt.exchange.example.json") }));
}

#[test]
fn worker_plan_routes_outbox_relay_to_builtin_worker() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.messaging.example.json");
    let config = RuntimeConfig::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    let launches = plan_workers(
        &config,
        Path::new("deploy/qianxing.runtime.messaging.example.json"),
        false,
    )
    .unwrap();
    assert!(launches.iter().any(|launch| {
        launch.worker_id == "outbox-relay"
            && launch.args
                == vec![
                    "outbox-relay-worker",
                    "deploy/qianxing.runtime.messaging.example.json",
                    "outbox-relay",
                ]
    }));
}

#[test]
fn worker_plan_routes_event_consumer_to_builtin_worker() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.consumer.example.json");
    let config = RuntimeConfig::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    let launches = plan_workers(
        &config,
        Path::new("deploy/qianxing.runtime.consumer.example.json"),
        false,
    )
    .unwrap();
    assert!(launches.iter().any(|launch| {
        launch.worker_id == "ledger-reducer"
            && launch.args.first().map(String::as_str) == Some("event-consumer-worker")
    }));
}
