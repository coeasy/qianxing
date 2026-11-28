//! `init` 将可识别的本地数据/策略身份落到严格 ProjectManifest。

use super::*;

#[test]
fn init_writes_valid_project_manifest_with_real_dataset_and_strategy_refs() {
    let root = temp_cli_case_dir("project-manifest-init");
    let runtime = root.join("qianxing.runtime.json");
    run_init_with_profile(&runtime, false, Some("macd"), None, true).expect("本地初始化应通过");

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
    run_strategy_init("sma_cross", &runtime, None, &[], false).expect("strategy init 应当成功");

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

#[test]
fn strategy_init_applies_only_signal_knobs_consumed_by_the_selected_strategy() {
    let root = temp_cli_case_dir("project-manifest-strategy-init-knobs");
    let runtime = root.join("strategy.json");
    let knobs = vec!["period=12".to_string(), "threshold_bps=250".to_string()];
    run_strategy_init("mean_reversion", &runtime, None, &knobs, false)
        .expect("mean-reversion signal knobs should be written into the runtime config");

    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&runtime).expect("runtime config exists"))
            .expect("runtime config is valid JSON");
    assert_eq!(document["strategy"]["builtin_strategy"], "mean_reversion");
    assert_eq!(document["strategy"]["builtin_period"], 12);
    assert_eq!(document["strategy"]["builtin_threshold_bps"], 250);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_init_rejects_knobs_that_the_selected_strategy_does_not_read() {
    let root = temp_cli_case_dir("project-manifest-strategy-init-unused-knob");
    let runtime = root.join("strategy.json");
    let knobs = vec!["period=9".to_string()];
    let error = run_strategy_init("macd", &runtime, None, &knobs, false)
        .expect_err("MACD uses fixed windows and must reject generic signal knobs");
    assert!(error.contains("策略 macd 不读取信号参数 period"), "{error}");
    assert!(
        !runtime.exists(),
        "invalid overrides must not write a partial config"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_init_rejects_malformed_duplicate_and_invalid_knob_values_before_writing() {
    let invalid_overrides = [
        (
            "malformed",
            vec!["fast_window=8=9".to_string()],
            "信号参数格式应为 NAME=INTEGER",
        ),
        (
            "duplicate",
            vec!["fast_window=8".to_string(), "fast_window=9".to_string()],
            "信号参数 fast_window 被重复指定",
        ),
        (
            "invalid-range",
            vec!["fast_window=21".to_string()],
            "builtin_fast_window 必须小于 builtin_slow_window",
        ),
    ];

    for (case, knobs, expected_error) in invalid_overrides {
        let root = temp_cli_case_dir(&format!("project-manifest-strategy-init-{case}"));
        let runtime = root.join("strategy.json");
        let error = run_strategy_init("sma_cross", &runtime, None, &knobs, false)
            .expect_err("invalid signal knob overrides must fail");
        assert!(error.contains(expected_error), "{error}");
        assert!(
            !runtime.exists(),
            "{case} overrides must fail before the runtime config is written"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
