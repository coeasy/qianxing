//! `serve` 必须按 role 找到它的 API worker，而不是按字面量名字猜（V11 S1）。

use super::*;

/// 合法拓扑，只是把 API worker 改了个名：worker id 是自由命名，配置校验只数"启用的
/// api role worker 恰好一个"，不看名字。
fn renamed_api_topology(data_dir: &Path) -> RuntimeConfig {
    let mut config = paper_runtime_config(data_dir);
    // 这份配置是被用例改过的内存拓扑，不是锁定过的发布配置。
    config.config_fingerprint = None;
    for worker in config.workers.iter_mut() {
        if worker.role == WorkerRole::Api {
            worker.id = "api-gw".into();
        }
    }
    config
}

/// 字面量 `"api"` 在这份合法配置上就是"启动即死"：监督器的注册表按 `worker.id` 建键，
/// 按名字 spawn 会被判未知 worker。这条用例把两件事一起钉住——按 role 解析出的 id 能
/// 注册，而旧的字面量不能，于是它同时是缺陷本体的证明。
#[test]
fn renamed_api_worker_is_the_one_the_supervisor_accepts() {
    let root = temp_cli_case_dir("runtime-api-worker-identity");
    let config = renamed_api_topology(&root);
    let supervisor =
        RuntimeSupervisor::new(config.clone()).expect("只改 api worker 的 id 不该让拓扑校验失败");
    let resolved =
        configured_api_worker_id(supervisor.config()).expect("启用的 api worker 解析得出 id");
    assert_eq!(resolved, "api-gw", "必须按 role 解析，而不是回落到字面量");

    assert!(
        supervisor.spawn_worker("api", |_| Ok(())).is_err(),
        "字面量 api 在这份拓扑上必须失败，否则这条用例咬不住缺陷本体"
    );
    let by_role = supervisor
        .spawn_worker(&resolved, |_| Ok(()))
        .expect("按 role 解析出的 id 必须能注册");
    let _ = by_role.join();
}

/// 关掉了就没有"那一个"：解析必须报错，不能静默挑别的 worker 当 API。
#[test]
fn api_worker_lookup_fails_closed_without_an_enabled_one() {
    let root = temp_cli_case_dir("runtime-api-worker-disabled");
    let mut config = renamed_api_topology(&root);
    for worker in config.workers.iter_mut() {
        if worker.role == WorkerRole::Api {
            worker.enabled = false;
        }
    }
    let error = configured_api_worker_id(&config).unwrap_err();
    assert!(
        error.contains("api worker"),
        "报错要说清缺的是 api worker：{error}"
    );
}
