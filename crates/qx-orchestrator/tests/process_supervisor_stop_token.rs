use qx_orchestrator::supervise_workers;
use qx_runtime::{RuntimeConfig, WorkerRole};
use std::path::{Path, PathBuf};

/// 仓库内已验收的 CCXT 运行时拓扑，用例只借用它的 worker 清单。
fn example_config() -> RuntimeConfig {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.ccxt.example.json");
    RuntimeConfig::from_json(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// 托管用例的现场：只保留一台 market-data worker，数据目录指到临时目录，
/// 子进程用当前测试可执行文件冒充（它会以 0 退出，绝不碰真实 worker 语义）。
fn supervisor_fixture(tag: &str) -> (RuntimeConfig, PathBuf) {
    let mut config = example_config();
    let mut kept = false;
    for worker in config.workers.iter_mut() {
        worker.enabled = !kept && worker.role == WorkerRole::MarketData;
        kept |= worker.enabled;
    }
    assert!(kept, "夹具必须恰好托管一台 worker");
    let work_dir = std::env::temp_dir().join(format!("qx-orchestrator-supervisor-{tag}"));
    std::fs::create_dir_all(&work_dir).unwrap();
    config.storage.data_dir = work_dir.join("data").to_string_lossy().into_owned();
    (config, work_dir)
}

/// 停机令牌命中时必须按"有序收工"返回 Ok：子进程会自己退出，若循环不读令牌，
/// 用户主动停机就会被报成 `managed worker exited` 的故障。
#[test]
fn supervisor_returns_ok_when_the_stop_token_is_requested() {
    let (config, work_dir) = supervisor_fixture("stop-requested");
    let outcome = supervise_workers(
        &config,
        Path::new("deploy/runtime.json"),
        &std::env::current_exe().unwrap(),
        &work_dir,
        false,
        || true,
    );
    let _ = std::fs::remove_dir_all(&work_dir);
    assert!(
        outcome.is_ok(),
        "停机令牌命中应有序收工（Ok），实际: {outcome:?}"
    );
}

/// 反向对照：不请求停机时，子进程退出仍必须 fail-fast 报 Err，
/// 否则上一条的 Ok 可能来自"循环什么都不检查"。
#[test]
fn supervisor_fails_fast_when_a_child_exits_on_its_own() {
    let (config, work_dir) = supervisor_fixture("child-exited");
    let outcome = supervise_workers(
        &config,
        Path::new("deploy/runtime.json"),
        &std::env::current_exe().unwrap(),
        &work_dir,
        false,
        || false,
    );
    let _ = std::fs::remove_dir_all(&work_dir);
    let message = outcome.expect_err("子进程自行退出必须被报成失败");
    assert!(message.contains("exited"), "应点名退出的 worker: {message}");
}
