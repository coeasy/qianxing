//! `runtime-check` 的出口形状：机器可读报告的安全边界，与健康那半边的真实语义。

use super::*;

#[test]
fn runtime_check_report_is_machine_readable_and_safe() {
    let path = example_runtime_path();
    let report = collect_runtime_check_report(&path).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["ok"], true);
    assert_eq!(report["network_accessed"], false);
    assert_eq!(report["orders_sent"], false);
    assert!(!report["health"]["services"].as_array().unwrap().is_empty());
    // V11 L4：体检跑在任何 worker 启动之前，这份 health 只是配置派生的花名册。它必须自己
    // 说清"没有心跳事实"，否则采集方会把 overall=starting 读成"健康检查通过"。
    assert_eq!(report["health_observed"], false);
    assert!(report["config_fingerprint"].as_str().unwrap().len() >= 32);
}

/// 健康那半边能主张什么、不能主张什么：能主张的是"这批 worker 会被托管"，不能主张的是任何
/// 活体结论——`ok` 里那半句 Failed 判定在这一刻投不出反对票。
#[test]
fn runtime_check_health_block_is_a_roster_and_not_a_live_probe() {
    let path = example_runtime_path();
    let config = read_runtime_config(&path).unwrap();
    let mut expected = config
        .workers
        .iter()
        .filter(|worker| worker.enabled)
        .map(|worker| worker.id.as_str())
        .collect::<Vec<_>>();
    expected.sort_unstable();
    let report = collect_runtime_check_report(&path).unwrap();
    let services = report["health"]["services"].as_array().unwrap();
    let roster = services
        .iter()
        .map(|service| service["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        roster, expected,
        "花名册必须正好是 enabled worker，不多不少不重名"
    );
    assert_eq!(report["health"]["overall"], "starting");
    // 逐行都是 Starting + 无心跳：这就是"这一项结构上到不了 Failed"的现场证据。
    for service in services {
        assert_eq!(service["status"], "starting", "{service}");
        assert!(service["last_heartbeat_ms"].is_null(), "{service}");
    }
    assert_eq!(report["health_observed"], false);
}

fn example_runtime_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json")
}
