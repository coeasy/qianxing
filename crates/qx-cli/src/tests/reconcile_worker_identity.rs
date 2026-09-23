//! `reconcile` 省略 worker-id 时按 **role + Venue 绑定**解析，不再回落到 `reconciler-main`
//! 这个字面量（V11 S12，与 S1 同一判据）。字面量的两处谎言都在这里被钉住：名字合法改动
//! 的拓扑上报"找不到 worker"（把"没解析"说成"不存在"），而候选不唯一时也没有任何依据
//! 替运维挑一个。

use super::*;

/// 模板里的 reconciler 全部摘掉，只留进来之时点名给用例的那批候选。
fn topology(
    label: &str,
    candidates: &[(&str, WorkerRole, &str, bool)],
) -> (PathBuf, RuntimeConfig) {
    let root = temp_cli_case_dir(label);
    let mut config = paper_runtime_config(&root);
    // 这份配置是为用例改过的内存拓扑，不是锁定过的发布配置。
    config.config_fingerprint = None;
    config
        .workers
        .retain(|worker| worker.role != WorkerRole::Reconciler);
    for (id, role, venue, enabled) in candidates {
        let mut worker = mk_worker(id, *role, venue, None);
        worker.enabled = *enabled;
        // 写回磁盘的那份要过 `read_runtime_config` 的凭据来源检查：这里放的是环境变量名，
        // 不是密钥值，配置校验只要求"点名了来源"。
        worker.credential_env = Some(qx_runtime::CredentialEnv {
            api_key: "QX_TEST_BINANCE_KEY".into(),
            secret: "QX_TEST_BINANCE_SECRET".into(),
        });
        config.workers.push(worker);
    }
    (root, config)
}

fn write(path: &Path, config: &RuntimeConfig) -> PathBuf {
    let runtime_path = path.join("runtime.json");
    std::fs::write(&runtime_path, config.to_json().unwrap()).unwrap();
    runtime_path
}

/// 合法拓扑只是没沿用那个名字：解析要按 role + Venue 命中它，旧实现则在这里"启动即死"。
#[test]
fn renamed_binance_reconciler_is_the_one_reconcile_resolves() {
    let (root, config) = topology(
        "s12-renamed",
        &[(
            "reconcile-btc",
            WorkerRole::Reconciler,
            "binance-testnet",
            true,
        )],
    );
    assert_eq!(
        configured_reconcile_worker_id(&config, &VenueEntry::BINANCE).as_deref(),
        Ok("reconcile-btc"),
        "必须按 role + Venue 解析，而不是回落到字面量"
    );
    // 缺陷本体：字面量在这份合法配置上是"找不到 worker"，而不是任何口径信息。
    let path = write(&root, &config);
    let error = run_binance_reconcile_once(&path, Some("reconciler-main")).unwrap_err();
    assert!(error.contains("找不到 worker: reconciler-main"), "{error}");
    let _ = std::fs::remove_dir_all(root);
}

/// 关掉了就没有"那一个"：必须报清缺的是哪个角色的 worker，而不是报一个不存在的名。
#[test]
fn reconcile_lookup_fails_closed_without_an_enabled_reconciler() {
    let (root, config) = topology(
        "s12-disabled",
        &[(
            "reconcile-btc",
            WorkerRole::Reconciler,
            "binance-testnet",
            false,
        )],
    );
    let error = configured_reconcile_worker_id(&config, &VenueEntry::BINANCE).unwrap_err();
    assert!(
        error.contains("reconciler worker") && error.contains("启用"),
        "报错要说清缺的是启用的 reconciler：{error}"
    );
    assert!(
        !error.contains("reconciler-main"),
        "不能再把口径问题说成一个不存在的名字: {error}"
    );
    let path = write(&root, &config);
    let error = run_binance_reconcile_once(&path, None).unwrap_err();
    assert!(error.contains("reconciler worker"), "{error}");
    let _ = std::fs::remove_dir_all(root);
}

/// 两个候选分属不同账户域时没有任何依据替运维挑一个：必须全部列出并要求点名。
#[test]
fn ambiguous_reconciler_candidates_are_not_resolved_by_choosing_one() {
    let (root, config) = topology(
        "s12-ambiguous",
        &[
            (
                "reconcile-btc",
                WorkerRole::Reconciler,
                "binance-testnet",
                true,
            ),
            ("reconcile-bnb", WorkerRole::Reconciler, "binance", true),
        ],
    );
    let error = configured_reconcile_worker_id(&config, &VenueEntry::BINANCE).unwrap_err();
    assert!(
        error.contains("2 个候选")
            && error.contains("reconcile-btc")
            && error.contains("reconcile-bnb"),
        "要把候选全部列出来: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 解析用的是入口自己那份 Venue 绑定，不是"任何 reconciler"：paper 账户的对账腿
/// 不会成为 Binance 链的候选，命名规约因此也无法绕开绑定判定。
#[test]
fn paper_reconciler_is_not_a_candidate_for_the_binance_entry() {
    let (paper_root, config) = topology(
        "s12-venue-binding",
        &[("reconcile-paper", WorkerRole::Reconciler, "paper", true)],
    );
    assert!(configured_reconcile_worker_id(&config, &VenueEntry::BINANCE).is_err());
    assert!(configured_reconcile_worker_id(&config, &VenueEntry::CCXT).is_err());
    // 角色也不许蹭：同一条链的 execution worker 不是对账候选。
    let (exec_root, config) = topology(
        "s12-role-only",
        &[("reconcile-btc", WorkerRole::Execution, "binance", true)],
    );
    let error = configured_reconcile_worker_id(&config, &VenueEntry::BINANCE).unwrap_err();
    assert!(error.contains("reconciler worker"), "{error}");
    let _ = std::fs::remove_dir_all(paper_root);
    let _ = std::fs::remove_dir_all(exec_root);
}

fn deploy(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .join("deploy")
        .join(name)
}

/// 发布配置里这条链**真**是什么名字，由解析说出而不是由字面量约定：Binance 两份示例
/// 仍解析到 `reconciler-main`（改名前后等价），CCXT 那份的 reconciler 实际叫
/// `ccxt-reconciler-main`（V11 S12 记的分叉），而关掉了 reconciler 的示例必须报口径
/// 缺失、不能报"找不到 worker"。
#[test]
fn published_runtimes_resolve_their_own_reconciler() {
    for (file, expected) in [
        (
            "qianxing.runtime.production.example.json",
            Ok("reconciler-main"),
        ),
        (
            "qianxing.runtime.binance-testnet.example.json",
            Ok("reconciler-main"),
        ),
    ] {
        let config = read_runtime_config(&deploy(file)).unwrap();
        assert_eq!(
            configured_reconcile_worker_id(&config, &VenueEntry::BINANCE).as_deref(),
            expected,
            "{file} 的对账腿解析错了"
        );
    }

    let config = read_runtime_config(&deploy("qianxing.runtime.ccxt.example.json")).unwrap();
    assert!(
        configured_reconcile_worker_id(&config, &VenueEntry::BINANCE).is_err(),
        "CCXT 配置里没有 Binance 对账腿，不该被名字凑出来"
    );
    assert_eq!(
        configured_reconcile_worker_id(&config, &VenueEntry::CCXT).as_deref(),
        Ok("ccxt-reconciler-main"),
        "CCXT 链的 reconciler 由解析给出，不再要求它改名叫 reconciler-main"
    );

    // 基线示例把 reconciler 关掉了：旧实现报"找不到 worker: reconciler-main"，
    // 把"这条链没启用"说成"没有这么个 worker"。
    let config = read_runtime_config(&deploy("qianxing.runtime.example.json")).unwrap();
    let error = configured_reconcile_worker_id(&config, &VenueEntry::BINANCE).unwrap_err();
    assert!(
        error.contains("启用") && !error.contains("找不到 worker"),
        "要报口径缺失而不是名字缺失: {error}"
    );
}
