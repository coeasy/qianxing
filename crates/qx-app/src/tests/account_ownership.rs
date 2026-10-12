use super::fixtures::{backtest_spec, scratch, write_frame};
use crate::cases::{run_backtest, verify_run};
use crate::context::{CallerCapability, RunContext};
use crate::error::AppErrorCategory;

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn explicit_account_ownership_reaches_engine_result_summary_and_verification() {
    let root = scratch("bt-account");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let output_dir = root.join("out");
    let mut spec = backtest_spec("run-account", &bars, &output_dir);
    spec.account_id = Some("acct.paper-01".into());
    let outcome = run_backtest(&spec, &research("run-account")).expect("账户回测跑通");

    assert_eq!(outcome.account_id.as_deref(), Some("acct.paper-01"));
    let summary: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&outcome.artifacts.summary).expect("summary 存在"),
    )
    .expect("summary JSON");
    assert_eq!(summary["account_id"], "acct.paper-01");
    assert!(
        verify_run(&outcome, &research("run-account"))
            .expect("结果复核")
            .verified
    );

    let mut misattributed = outcome.clone();
    misattributed.account_id = Some("acct.paper-02".into());
    let verification =
        verify_run(&misattributed, &research("run-account")).expect("身份不一致应作为复核结论返回");
    assert!(!verification.verified);
    assert!(verification
        .mismatches
        .iter()
        .any(|entry| entry.contains("account_id")));

    let mut conflicting = spec;
    conflicting.account_id = Some("acct.paper-02".into());
    let error = run_backtest(&conflicting, &research("run-account"))
        .expect_err("账户不同是不同运行身份，不允许覆盖同一 run_id");
    assert_eq!(error.category(), AppErrorCategory::Conflict);
}
