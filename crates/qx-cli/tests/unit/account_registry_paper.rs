use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn registered_account_cash_is_seeded_once_across_workers() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-account-registry-cash-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut config = paper_runtime_config(&root);
    config.accounts.push(qx_runtime::AccountRuntimeConfig {
        id: "main".into(),
        settlement_currency: Some("USDT".into()),
        initial_cash_raw: Some(2_500),
        portfolio_id: None,
    });
    let mut worker = config
        .workers
        .iter()
        .find(|worker| worker.id == "paper-execution")
        .unwrap()
        .clone();
    worker.paper_initial_cash_raw = None;
    let mut pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    seed_paper_initial_cash_from_config(&mut pipeline, &config, &worker, 10).unwrap();
    worker.id.push_str("-second");
    seed_paper_initial_cash_from_config(&mut pipeline, &config, &worker, 11).unwrap();
    assert_eq!(pipeline.ledger().cash_for("main", "USDT"), 2_500);
    assert_eq!(pipeline.log().events().len(), 2);
    let _ = std::fs::remove_dir_all(root);
}
