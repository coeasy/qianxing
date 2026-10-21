//! `environment` 的四种合法写法各自决定实时策略作业走哪条提交臂（V13 R2 第二十三遍 #245）。
//!
//! `scheduler.rs` 里 `dry_run` 只按 `paper` 判定，而它的上游是运行时配置的闭合名单
//! （`qx_runtime::ENVIRONMENT_VOCAB`）。名单每加一个写法，就必须在这里为它显式决定
//! "模拟还是真实提交" —— 否则新写法会顺着 `== "paper"` 的默认分支静默落进真实提交臂。

use super::*;
use std::collections::BTreeSet;

/// 每一种合法写法期望的 `dry_run`：`paper` 模拟，其余三种按名字提交到各自的 venue
/// （`sandbox` 的隔离性由 CCXT 端点 JSON 自己的 `sandbox: true` 承载，`testnet` 由
/// `venue_id=binance-testnet` 承载，都不靠 `environment` 的措辞兜底）。
const SUBMIT_ARM_TABLE: [(&str, bool); 4] = [
    ("paper", true),
    ("sandbox", false),
    ("testnet", false),
    ("production", false),
];

fn live_strategy_fixture() -> StrategyRuntimeConfig {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let template = workspace_root
        .join("deploy")
        .join("qianxing.runtime.example.json");
    read_runtime_config(&template)
        .expect("读取运行时配置模板失败")
        .strategy
}

#[test]
fn every_admitted_environment_spelling_declares_its_submit_arm() {
    let admitted: BTreeSet<&str> = qx_runtime::ENVIRONMENT_VOCAB.into_iter().collect();
    let declared: BTreeSet<&str> = SUBMIT_ARM_TABLE.iter().map(|(s, _)| *s).collect();
    assert_eq!(
        admitted, declared,
        "environment 名单与 dry_run 表必须同轮扩写：名单新增写法却没有声明提交臂，\
         它就会顺着 `== \"paper\"` 的默认分支静默真实提交"
    );

    let strategy = live_strategy_fixture();
    for (spelling, expected_dry_run) in SUBMIT_ARM_TABLE {
        let (job, _) = live_strategy_job(
            &strategy,
            "strategy-1",
            0x1234_5678,
            1_700_000_000_000,
            spelling,
        );
        assert_eq!(
            job.dry_run, expected_dry_run,
            "environment={spelling:?} 的提交臂与声明不一致"
        );
    }

    // 大小写不敏感：写法换个大小写不得换提交臂。
    let (mixed, _) = live_strategy_job(&strategy, "strategy-1", 0x1, 1_700_000_000_000, "Paper");
    assert!(mixed.dry_run, "PAPER 的混排写法必须仍是模拟");
}
