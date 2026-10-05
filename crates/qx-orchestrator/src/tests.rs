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

/// 两份平台入口脚本里禁止出现的进程入口字面量：它们只能由 `plan_workers` 派生。
const WORKER_ENTRYPOINTS: [&str; 7] = [
    "binance-worker",
    "paper-worker",
    "ccxt-worker",
    "scheduler-worker",
    "strategy-worker",
    "outbox-relay-worker",
    "event-consumer-worker",
];

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

/// 两份平台入口都必须把拓扑交给唯一的实现 `qx-cli supervise`，不许在脚本里再写一份
/// 角色→进程入口映射（V13 R2 #192）。PowerShell 那份副本曾经漂移过四处：CCXT 的 endpoint
/// 只对其中一个角色生效、paper 判定用精确字符串而不是 `VenueFamily` 归一、`plan_workers`
/// 会拒绝的拓扑被静默派给币安那条线、有内建入口的两个角色被当成不可托管。
///
/// 判据按"字面量不得出现"而不是"结构长得对"取数：注释里提到某个进程入口名同样会红，
/// 因为副本一旦被重新抄回来，第一件事就是把这个名字写回脚本。
#[test]
fn launchers_delegate_worker_topology_to_supervise() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let launchers = [
        (
            "start-qianxing.ps1",
            std::fs::read_to_string(root.join("start-qianxing.ps1")).unwrap(),
        ),
        (
            "start-qianxing.sh",
            std::fs::read_to_string(root.join("start-qianxing.sh")).unwrap(),
        ),
    ];
    for (name, text) in &launchers {
        assert!(
            text.contains("supervise"),
            "{name} 没有把 worker 拓扑委派给 supervise"
        );
        assert!(
            text.contains("runtime-check"),
            "{name} 丢了 supervise 之前的 runtime-check 前置闸门"
        );
        for entry in WORKER_ENTRYPOINTS {
            assert!(
                !text.contains(entry),
                "{name} 里又出现一份 {entry} 的进程入口映射；唯一实现是 plan_workers"
            );
        }
    }
    assert!(
        launchers[0].1.contains("--allow-unmanaged-roles"),
        "start-qianxing.ps1 丢了 -AllowUnmanagedRoles 到 supervise 旗标的映射"
    );
}

/// 一份 enabled worker 都没有的配置必须被拒，而不是"规划出空集合、监督器管零个子进程"：
/// 后者在旧 PowerShell 入口里表现为不退出循环（进程活着、什么都没跑，读侧看是健康）。
#[test]
fn worker_plan_rejects_a_runtime_without_any_enabled_worker() {
    let mut config = example_config();
    for worker in config.workers.iter_mut() {
        worker.enabled = false;
    }
    let error = plan_workers(&config, Path::new("deploy/runtime.json"), false)
        .err()
        .unwrap_or_default();
    assert!(
        error.contains("没有可托管"),
        "空拓扑应被拒绝，实际返回 {error}"
    );
}
