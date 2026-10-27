mod common;

use common::{scratch, write_frame};
use qx_app::{
    run_multi_account_joint_backtest, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    JointBacktestAccount, MultiAccountJointBacktestSpec, MultiMarketBacktestMarket,
    PortfolioStrategySpec, RunContext,
};
use qx_strategy::BuiltinStrategyKind;
use serde_json::Value;

const SCALE: i128 = 1_000_000_000;

fn spec(root: &std::path::Path, run_id: &str) -> MultiAccountJointBacktestSpec {
    let markets = [
        ("okx", "BTC-USDT.OKX"),
        ("binance", "ETHUSDT.BINANCE"),
        ("bitget", "PEPEUSDT.BITGET"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (exchange_id, instrument))| {
        let bars_path = write_frame(root, &format!("market-{index}.json"), instrument, 80);
        let mut strategy = BuiltinStrategySpec::new(
            BuiltinStrategyKind::SmaCross,
            format!("{}-sma", exchange_id),
        );
        strategy.fast_window = 2;
        strategy.slow_window = 3;
        JointBacktestAccount {
            exchange_id: exchange_id.into(),
            account_id: "primary".into(),
            initial_cash_raw: 200_000 * SCALE,
            markets: vec![MultiMarketBacktestMarket {
                instrument: instrument.into(),
                bars_path,
                dataset_manifest_path: None,
                priority: 0,
                market_spec: None,
                asset_class: None,
                fee_schedule: None,
                order_policy: None,
                margin_model: None,
                funding_bps: 0,
                funding_events: Vec::new(),
                strategies: vec![PortfolioStrategySpec {
                    priority: 0,
                    reserved_capital_raw: 50_000 * SCALE,
                    reserved_capital_exact: None,
                    strategy,
                }],
            }],
        }
    })
    .collect();
    MultiAccountJointBacktestSpec {
        schema_version: 1,
        run_id: run_id.into(),
        settlement_currency: "USDT".into(),
        seed: 7,
        output_dir: root.join("out").to_string_lossy().into_owned(),
        accounts: markets,
    }
}

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn joint_run_combines_three_exchanges_and_keeps_accounts_isolated() {
    let root = scratch("joint-multi-exchange");
    let joint_spec = spec(&root, "joint-multi-exchange");
    joint_spec
        .validate()
        .expect("three venue account groups validate");
    let outcome = run_multi_account_joint_backtest(&joint_spec, &research("joint-multi-exchange"))
        .expect("joint exchange backtest");

    assert_eq!(outcome.accounts_total, 3);
    assert_eq!(outcome.markets_total, 3);
    assert_eq!(outcome.initial_cash_raw, 600_000 * SCALE);
    assert_eq!(outcome.accounts.len(), 3);
    assert!(outcome
        .accounts
        .iter()
        .all(|account| account.account_id == "primary"));
    assert_eq!(
        outcome
            .accounts
            .iter()
            .map(|account| account.exchange_id.as_str())
            .collect::<Vec<_>>(),
        vec!["binance", "bitget", "okx"]
    );
    assert!(outcome.successful_fills > 0);

    let equity: Vec<(u64, i128)> = serde_json::from_str(
        &std::fs::read_to_string(&outcome.equity_path).expect("joint equity file exists"),
    )
    .expect("joint equity JSON");
    assert_eq!(equity.len(), 80);
    assert_eq!(equity.last().unwrap().1, outcome.final_equity_raw);

    let artifact: Value = serde_json::from_str(
        &std::fs::read_to_string(&outcome.artifact_path).expect("joint artifact exists"),
    )
    .expect("joint artifact JSON");
    let envelope_schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/multi-account-joint-backtest-envelope-v2.json"
    ))
    .expect("joint envelope schema JSON");
    let artifact_fields = artifact
        .as_object()
        .expect("joint artifact envelope is an object")
        .keys()
        .collect::<std::collections::BTreeSet<_>>();
    let schema_fields = envelope_schema["properties"]
        .as_object()
        .expect("joint envelope schema properties")
        .keys()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(artifact_fields, schema_fields);
    assert_eq!(artifact["schema_version"], 2);
    assert_eq!(
        artifact["aggregation"],
        "sum_of_independent_exchange_account_equity_no_cross_account_margin_netting"
    );
    assert_eq!(
        artifact.pointer("/outcome/accounts_total"),
        Some(&Value::from(3))
    );
    assert_eq!(
        outcome.result_hash.as_deref(),
        artifact["result_hash"].as_str()
    );
    assert!(outcome
        .run_record
        .as_deref()
        .is_some_and(|path| std::path::Path::new(path).is_file()));
    let run_record: qx_spec::RunRecordV2 = serde_json::from_str(
        &std::fs::read_to_string(outcome.run_record.as_deref().unwrap())
            .expect("joint RunRecord bytes"),
    )
    .expect("joint RunRecord V2");
    let portfolio_ref = run_record
        .artifact_refs
        .iter()
        .find(|reference| reference.name == "joint_portfolio")
        .expect("joint portfolio ArtifactRef");
    assert_eq!(
        portfolio_ref.kind,
        qx_app::artifact_store::MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_KIND
    );
    assert_eq!(
        portfolio_ref.schema_version,
        qx_app::artifact_store::MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION
    );
    let outcome_json = serde_json::to_value(&outcome).expect("joint outcome JSON");
    qx_app::artifact_store::verify_declared_run_record(&outcome_json)
        .expect("joint RunRecord V2 binds all parent artifacts");
    let report = qx_app::read_multi_account_joint_report_v1(
        &outcome,
        &research("joint-multi-exchange-report"),
    )
    .expect("joint research report reads through the RunRecord-backed chain");
    assert_eq!(report.summary.accounts_total, 3);

    let fills_path =
        std::path::PathBuf::from(&outcome.artifact_locations.as_ref().unwrap()["joint_fills"]);
    let fills_bytes = std::fs::read(&fills_path).expect("joint fills artifact exists");
    let mut tampered_fills = fills_bytes.clone();
    tampered_fills.push(b' ');
    std::fs::write(&fills_path, &tampered_fills).expect("tamper joint fills artifact");
    assert!(qx_app::artifact_store::verify_declared_run_record(&outcome_json).is_err());
    assert!(qx_app::read_multi_account_joint_report_v1(
        &outcome,
        &research("joint-multi-exchange-report-tampered"),
    )
    .is_err());
    std::fs::write(&fills_path, fills_bytes).expect("restore joint fills artifact");

    let manifest_path = artifact["run_manifest_path"].as_str().unwrap();
    assert_eq!(outcome.run_manifest_path.as_deref(), Some(manifest_path));
    let manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(manifest_path).expect("joint RunManifest exists"),
    )
    .expect("joint RunManifest JSON");
    let canonical =
        qx_spec::CanonicalRunSpec::from_json(manifest.canonical_run_spec_json.as_deref().unwrap())
            .expect("joint CanonicalRunSpec");
    assert_eq!(canonical.run_type, "multi_account_joint_backtest");
    assert_eq!(canonical.datasets.len(), 3);
    assert!(canonical
        .canonical_json()
        .unwrap()
        .contains("multi_account_joint_backtest"));
    assert!(!canonical
        .canonical_json()
        .unwrap()
        .contains(&root.to_string_lossy().to_string()));
    qx_app::artifact_store::verify_declared_run_manifest(&artifact)
        .expect("parent and child RunManifest identities verify");

    let mut tampered = artifact.clone();
    tampered["account_run_verifications"][0]["identity"]["market_data"][0]["end_ts_ms"] =
        Value::from(1_u64);
    assert!(
        qx_app::artifact_store::verify_canonical_run_spec_for_manifest(&manifest, &tampered)
            .is_err()
    );

    let repeated = run_multi_account_joint_backtest(&joint_spec, &research("joint-multi-exchange"))
        .expect("same joint run is idempotent");
    assert_eq!(outcome.spec_fingerprint, repeated.spec_fingerprint);
    assert_eq!(outcome.final_equity_raw, repeated.final_equity_raw);

    let relocated_root = scratch("joint-multi-exchange-relocated");
    let relocated_spec = spec(&relocated_root, "joint-multi-exchange-relocated");
    let relocated = run_multi_account_joint_backtest(
        &relocated_spec,
        &research("joint-multi-exchange-relocated"),
    )
    .expect("same content at a different path has a stable identity");
    let relocated_artifact: Value = serde_json::from_str(
        &std::fs::read_to_string(&relocated.artifact_path).expect("relocated joint artifact"),
    )
    .unwrap();
    let relocated_manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(relocated_artifact["run_manifest_path"].as_str().unwrap())
            .expect("relocated joint manifest"),
    )
    .unwrap();
    assert_eq!(
        manifest.canonical_run_spec_fingerprint,
        relocated_manifest.canonical_run_spec_fingerprint
    );
    assert_eq!(
        manifest.data_fingerprint,
        relocated_manifest.data_fingerprint
    );
    assert_eq!(manifest.result_hash, relocated_manifest.result_hash);
}

#[test]
fn legacy_joint_artifact_upgrades_after_full_report_verification() {
    let root = scratch("joint-multi-exchange-legacy-upgrade");
    let joint_spec = spec(&root, "joint-legacy-upgrade");
    let legacy = run_multi_account_joint_backtest(&joint_spec, &research("joint-legacy-upgrade"))
        .expect("initial joint run creates a fully verified report");
    let mut artifact: Value =
        serde_json::from_slice(&std::fs::read(&legacy.artifact_path).expect("read joint artifact"))
            .expect("joint artifact JSON");
    let outcome = artifact["outcome"].as_object_mut().unwrap();
    outcome.remove("result_hash");
    outcome.remove("run_record");
    outcome.remove("artifact_locations");
    std::fs::write(
        &legacy.artifact_path,
        serde_json::to_vec(&artifact).expect("serialize legacy joint artifact"),
    )
    .expect("replace outcome with a legacy shape");
    std::fs::remove_file(legacy.run_record.as_ref().unwrap())
        .expect("remove the not-yet-published legacy RunRecord");

    let upgraded = run_multi_account_joint_backtest(&joint_spec, &research("joint-legacy-upgrade"))
        .expect("verified legacy report upgrades without changing its identity");
    assert!(upgraded.result_hash.is_some());
    assert!(upgraded.run_record.is_some());
    assert!(upgraded.artifact_locations.is_some());
    let value = serde_json::to_value(&upgraded).unwrap();
    qx_app::artifact_store::verify_declared_run_record(&value)
        .expect("upgraded RunRecord V2 verifies");
    qx_app::read_multi_account_joint_report_v1(&upgraded, &research("joint-legacy-upgrade-read"))
        .expect("upgraded report remains readable");
}

#[test]
fn legacy_joint_run_record_artifact_ref_upgrades_after_full_verification() {
    let root = scratch("joint-multi-exchange-record-ref-upgrade");
    let joint_spec = spec(&root, "joint-record-ref-upgrade");
    let initial =
        run_multi_account_joint_backtest(&joint_spec, &research("joint-record-ref-upgrade"))
            .expect("initial run creates v2 envelope and RunRecord");
    let record_path = std::path::PathBuf::from(initial.run_record.as_deref().unwrap());
    let mut legacy_record: Value =
        serde_json::from_slice(&std::fs::read(&record_path).expect("read current RunRecord"))
            .expect("RunRecord JSON");
    let portfolio_ref = legacy_record["artifact_refs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|reference| reference["name"] == "joint_portfolio")
        .expect("joint portfolio ArtifactRef");
    portfolio_ref["kind"] = Value::String("multi-account-joint-backtest-result-json".into());
    portfolio_ref["schema_version"] = Value::from(1);
    std::fs::write(
        &record_path,
        serde_json::to_vec(&legacy_record).expect("serialize legacy RunRecord"),
    )
    .expect("write legacy RunRecord");

    let upgraded =
        run_multi_account_joint_backtest(&joint_spec, &research("joint-record-ref-upgrade"))
            .expect("identical run verifies and upgrades the legacy ArtifactRef");
    let upgraded_record: qx_spec::RunRecordV2 =
        serde_json::from_slice(&std::fs::read(&record_path).expect("read upgraded RunRecord"))
            .expect("upgraded RunRecord JSON");
    let upgraded_portfolio_ref = upgraded_record
        .artifact_refs
        .iter()
        .find(|reference| reference.name == "joint_portfolio")
        .expect("upgraded joint portfolio ArtifactRef");
    assert_eq!(
        upgraded_portfolio_ref.kind,
        qx_app::artifact_store::MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_KIND
    );
    assert_eq!(
        upgraded_portfolio_ref.schema_version,
        qx_app::artifact_store::MULTI_ACCOUNT_JOINT_BACKTEST_ENVELOPE_SCHEMA_VERSION
    );
    qx_app::artifact_store::verify_declared_run_record(
        &serde_json::to_value(&upgraded).expect("upgraded outcome JSON"),
    )
    .expect("upgraded record still binds the report artifacts");
}

#[test]
fn joint_accounts_may_overlap_markets_but_each_account_must_match_its_exchange() {
    let root = scratch("joint-multi-exchange-invalid");
    let mut overlap = spec(&root, "joint-overlapping-instrument");
    overlap.accounts[1].exchange_id = "okx".into();
    overlap.accounts[1].account_id = "secondary".into();
    overlap.accounts[1].markets[0].instrument = overlap.accounts[0].markets[0].instrument.clone();
    overlap.accounts[1].markets[0].bars_path = write_frame(
        &root,
        "overlap-market.json",
        &overlap.accounts[0].markets[0].instrument,
        80,
    );
    overlap
        .validate()
        .expect("isolated accounts may trade the same instrument");
    let outcome = run_multi_account_joint_backtest(&overlap, &research("joint-overlapping-run"))
        .expect("overlapping isolated accounts run independently");
    assert_eq!(outcome.accounts_total, 3);
    assert_eq!(outcome.markets_total, 3);

    let mut duplicate_within_account = overlap.clone();
    let same_market = duplicate_within_account.accounts[0].markets[0].clone();
    duplicate_within_account.accounts[0]
        .markets
        .push(same_market);
    assert_eq!(
        duplicate_within_account.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    let mut mismatch = spec(&root, "joint-venue-mismatch");
    mismatch.accounts[0].exchange_id = "binance".into();
    assert_eq!(
        mismatch.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}
