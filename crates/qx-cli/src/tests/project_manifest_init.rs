//! `init` 将可识别的本地数据/策略身份落到严格 ProjectManifest。

use super::*;

#[test]
fn init_writes_valid_project_manifest_with_real_dataset_and_strategy_refs() {
    let root = temp_cli_case_dir("project-manifest-init");
    let runtime = root.join("qianxing.runtime.json");
    run_init_with_profile(&runtime, false, Some("macd"), None).expect("本地初始化应通过");

    let payload = std::fs::read_to_string(root.join("qianxing.project.json"))
        .expect("存在真实数据集时 init 必须写 ProjectManifest");
    let readout = qx_spec::describe("project", &payload).expect("项目清单必须通过规格校验");
    let project: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(project["profile"], "crypto-paper");
    assert_eq!(project["runtime"], "qianxing.runtime.json");
    assert_eq!(
        project["datasets"][0]["dataset_id"],
        "strategy-bars:BTCUSDT.BINANCE"
    );
    assert_eq!(project["datasets"][0]["version"], BARFRAME_DATASET_VERSION);
    assert_eq!(project["strategies"][0]["language"], "rust");
    assert_eq!(project["strategies"][0]["source"], "macd");
    assert!(!readout.fingerprint.is_empty());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_init_writes_the_same_project_identity_layer() {
    let root = temp_cli_case_dir("project-manifest-strategy-init");
    let runtime = root.join("strategy.json");
    run_strategy_init("sma_cross", &runtime, None, false).expect("strategy init 应当成功");

    let payload = std::fs::read_to_string(root.join("qianxing.project.json"))
        .expect("strategy init 也必须接 ProjectManifest");
    let project: serde_json::Value = serde_json::from_str(&payload).unwrap();
    qx_spec::describe("project", &payload).expect("strategy init 项目清单必须通过 schema");
    assert_eq!(project["profile"], "crypto-paper");
    assert_eq!(
        project["strategies"][0]["strategy_id"],
        "strategy-sma_cross"
    );
    assert_eq!(project["strategies"][0]["language"], "rust");
    assert_eq!(project["strategies"][0]["source"], "sma_cross");

    let _ = std::fs::remove_dir_all(root);
}
