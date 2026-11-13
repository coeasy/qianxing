use super::*;
use std::io::Read as _;

#[test]
fn joint_exchange_backtest_command_runs_okx_and_bitget_accounts() {
    let root = scratch("multi-account-joint-cli");
    let template = std::fs::read_to_string(sample_bars()).expect("read sample BarFrame");
    let members = [("okx", "BTC-USDT.OKX"), ("bitget", "PEPEUSDT.BITGET")]
        .into_iter()
        .enumerate()
        .map(|(index, (exchange_id, instrument))| {
            let bars_path = root.join(format!("joint-{index}.json"));
            std::fs::write(&bars_path, template.replace("BTCUSDT.BINANCE", instrument))
                .expect("write exchange BarFrame");
            json!({
                "exchange_id": exchange_id,
                "account_id": "primary",
                "initial_cash_raw": 100_000_000_000_000_i128,
                "markets": [{
                    "instrument": instrument,
                    "bars_path": bars_path,
                    "priority": 0,
                    "market_spec": null,
                    "fee_schedule": null,
                    "strategies": [{
                        "priority": 0,
                        "reserved_capital_raw": 20_000_000_000_000_i128,
                        "strategy": {
                            "kind": "sma_cross",
                            "strategy_id": format!("joint-cli-{index}"),
                            "quantity_raw": 1_000_000_000_i128,
                            "fast_window": 2,
                            "slow_window": 3
                        }
                    }]
                }]
            })
        })
        .collect::<Vec<_>>();
    let spec_path = root.join("joint-spec.json");
    std::fs::write(
        &spec_path,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "run_id": "cli-joint-exchange-run",
            "settlement_currency": "USDT",
            "seed": 7,
            "output_dir": root.join("runs"),
            "accounts": members
        }))
        .unwrap(),
    )
    .expect("write joint account spec");

    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "multi-account-joint-backtest"])
        .arg(&spec_path)
        .output()
        .expect("start joint account CLI");
    assert_eq!(
        output.status.code(),
        Some(0),
        "joint CLI error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outcome: Value = serde_json::from_slice(&output.stdout).expect("joint CLI result JSON");
    assert_eq!(outcome["accounts_total"], 2);
    assert_eq!(outcome["markets_total"], 2);
    assert_eq!(
        outcome["initial_cash_raw"].as_i64(),
        Some(200_000_000_000_000_i64)
    );
    assert_eq!(outcome["accounts"][0]["account_id"], "primary");
    assert_eq!(outcome["accounts"][1]["account_id"], "primary");
    assert_ne!(
        outcome["accounts"][0]["exchange_id"],
        outcome["accounts"][1]["exchange_id"]
    );

    let outcome_path = root.join("joint-outcome.json");
    let outcome_json = serde_json::to_string_pretty(&outcome).expect("serialize joint outcome");
    std::fs::write(&outcome_path, &outcome_json).expect("save joint outcome");
    let report_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "read-multi-account-joint-report"])
        .arg(&outcome_path)
        .output()
        .expect("read joint report through CLI");
    assert_eq!(
        report_output.status.code(),
        Some(0),
        "joint report CLI error: {}",
        String::from_utf8_lossy(&report_output.stderr)
    );
    let cli_report: Value =
        serde_json::from_slice(&report_output.stdout).expect("joint report JSON");
    assert_eq!(cli_report["schema_version"], 1);
    assert_eq!(cli_report["verification"]["verified"], true);
    assert_eq!(cli_report["summary"]["accounts_total"], 2);
    assert_eq!(cli_report["accounts"].as_array().unwrap().len(), 2);
    assert!(cli_report["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|account| {
            account["exchange_id"] == "okx" && account["event_rows_total"].as_u64().unwrap() > 0
        }));

    let repo = repo_root();
    match python_native::via_python_json_value(
        &repo,
        "read_multi_account_joint_report_v1",
        &outcome_json,
    ) {
        None => eprintln!("[skip] Python native joint report reader: PyO3 extension not built"),
        Some(Ok(python_report)) => assert_eq!(python_report, cli_report),
        Some(Err(payload)) => panic!("Python joint report reader failed: {payload}"),
    }

    let page_request = json!({
        "outcome": outcome,
        "joint_account_event_pages": {
            "okx": {"primary": {"offset": 0, "limit": 1}}
        }
    });
    let page_request_path = root.join("joint-report-page-request.json");
    let page_request_json = serde_json::to_string(&page_request).expect("encode page request");
    std::fs::write(&page_request_path, &page_request_json).expect("save joint report page request");
    let page_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "read-multi-account-joint-report"])
        .arg(&page_request_path)
        .output()
        .expect("read a single venue/account report page through CLI");
    assert_eq!(
        page_output.status.code(),
        Some(0),
        "joint report page CLI error: {}",
        String::from_utf8_lossy(&page_output.stderr)
    );
    let cli_page: Value = serde_json::from_slice(&page_output.stdout).expect("joint page JSON");
    let okx_page = cli_page["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|account| account["exchange_id"] == "okx")
        .unwrap();
    assert_eq!(okx_page["event_rows_offset"], 0);
    assert_eq!(okx_page["event_rows_limit"], 1);
    assert_eq!(okx_page["event_rows"].as_array().unwrap().len(), 1);
    assert_eq!(okx_page["event_rows_next_offset"], 1);
    match python_native::via_python_json_value(
        &repo,
        "read_multi_account_joint_report_v1",
        &page_request_json,
    ) {
        None => eprintln!("[skip] Python native joint report pagination: PyO3 extension not built"),
        Some(Ok(python_page)) => assert_eq!(python_page, cli_page),
        Some(Err(payload)) => panic!("Python joint report pagination failed: {payload}"),
    }

    let equity_path = std::path::PathBuf::from(
        outcome["equity_path"]
            .as_str()
            .expect("joint outcome equity path"),
    );
    let equity_original = std::fs::read_to_string(&equity_path).expect("read joint equity");
    let mut equity: Value = serde_json::from_str(&equity_original).expect("joint equity JSON");
    equity[0][1] = json!(0);
    std::fs::write(
        &equity_path,
        serde_json::to_vec(&equity).expect("encode tampered equity"),
    )
    .expect("tamper joint equity");
    let rejected = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "read-multi-account-joint-report"])
        .arg(&outcome_path)
        .output()
        .expect("read tampered joint report");
    std::fs::write(&equity_path, equity_original).expect("restore joint equity");
    assert_ne!(
        rejected.status.code(),
        Some(0),
        "joint report must reject a modified aggregate equity curve"
    );
}

#[test]
fn isolated_multi_account_backtest_is_shared_by_cli_http_and_python() {
    let root = scratch("multi-account");
    let repo = repo_root();
    let btc_path = sample_bars();
    let eth_path = repo
        .join(".task-tmp")
        .join(format!("eth-bars-{}.json", std::process::id()));
    std::fs::create_dir_all(eth_path.parent().unwrap()).expect("create fixture directory");
    let btc = std::fs::read_to_string(&btc_path).expect("read BTC fixture");
    let eth = btc.replace("BTCUSDT.BINANCE", "ETHUSDT.BINANCE");
    std::fs::write(&eth_path, eth).expect("write ETH fixture");
    let member = |account: &str, run: &str, instrument: &str, bars: &Path, output: &Path| {
        json!({
            "schema_version": 1,
            "run_id": run,
            "account_id": account,
            "instrument": instrument,
            "bars_path": bars,
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 20261013,
            "output_dir": output,
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": format!("{account}-sma"),
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 2,
                "slow_window": 3,
            },
        })
    };
    let spec = json!({
        "schema_version": 1,
        "batch_id": "g1-multi-account",
        "runs": [
            member("acct-eth", "g1-eth", "ETHUSDT.BINANCE", &eth_path, &root.join("eth-out")),
            member("acct-btc", "g1-btc", "BTCUSDT.BINANCE", Path::new(&btc_path), &root.join("btc-out")),
        ],
    })
    .to_string();
    let spec_path = root.join("multi-account.json");
    std::fs::write(&spec_path, &spec).expect("write batch spec");
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "multi-account-backtest"])
        .arg(&spec_path)
        .output()
        .expect("start multi-account CLI");
    assert_eq!(
        output.status.code(),
        Some(0),
        "CLI error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: Value = serde_json::from_slice(&output.stdout).expect("CLI batch JSON");
    let result_path = root.join("multi-account-result.json");
    std::fs::write(
        &result_path,
        serde_json::to_vec_pretty(&cli).expect("serialize saved batch result"),
    )
    .expect("save multi-account result for reporting");
    let core_report_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "read-multi-account-report"])
        .arg(&result_path)
        .output()
        .expect("read shared multi-account report through CLI");
    assert_eq!(
        core_report_output.status.code(),
        Some(0),
        "shared report CLI error: {}",
        String::from_utf8_lossy(&core_report_output.stderr)
    );
    let cli_core_report: Value =
        serde_json::from_slice(&core_report_output.stdout).expect("shared CLI report JSON");
    assert_eq!(cli_core_report["schema_version"], 1);
    assert_eq!(cli_core_report["verification"]["verified"], true);
    assert_eq!(cli_core_report["accounts"].as_array().unwrap().len(), 2);
    match python_native::via_python_json_value(
        &repo,
        "read_multi_account_report_v1",
        &serde_json::to_string(&cli).expect("encode batch result for Python report reader"),
    ) {
        None => eprintln!("[skip] Python native report reader: PyO3 extension not built"),
        Some(Ok(python_core_report)) => {
            assert_eq!(python_core_report, cli_core_report);
        }
        Some(Err(payload)) => panic!("Python multi-account report reader failed: {payload}"),
    }
    let (status, http) = via_http("/app/multi-account-backtest", &spec);
    assert_eq!(status, 200, "HTTP batch failed: {http}");
    for value in [&cli, &http] {
        assert!(value["summary"]["complete"].as_bool().unwrap());
        assert_eq!(value["summary"]["accounts_succeeded"], 2);
        assert_eq!(value["summary"]["accounts_failed"], 0);
        assert!(value["summary"]["successful_fills"].as_u64().unwrap() > 0);
        assert_eq!(value["runs"][0]["account_id"], "acct-btc");
        assert_eq!(value["runs"][1]["account_id"], "acct-eth");
    }
    assert_eq!(cli["spec_fingerprint"], http["spec_fingerprint"]);
    for index in 0..2 {
        assert_eq!(
            cli["runs"][index]["outcome"]["result_hash"],
            http["runs"][index]["outcome"]["result_hash"]
        );
    }
    let report_html = root.join("multi-account-report.html");
    let report_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["report"])
        .arg(&result_path)
        .args(["--json", "--html", "--output"])
        .arg(&report_html)
        .output()
        .expect("start multi-account report");
    assert_eq!(
        report_output.status.code(),
        Some(0),
        "multi-account report failed: {}",
        String::from_utf8_lossy(&report_output.stderr)
    );
    let report: Value = serde_json::from_slice(&report_output.stdout).expect("report JSON");
    assert_eq!(report["report"]["report_kind"], "multi_account_backtest");
    assert_eq!(report["report"]["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(report["report"]["summary"], cli_core_report["summary"]);
    assert_eq!(
        report["report"]["accounts"][0]["run_data_fingerprint"],
        cli_core_report["accounts"][0]["report"]["verification"]["data_fingerprint"]
    );
    assert_eq!(
        report["report"]["accounts"][0]["fill_rows_total"],
        report["report"]["accounts"][0]["fills"]
    );
    assert!(
        report["report"]["accounts"][0]["event_rows_total"]
            .as_u64()
            .unwrap()
            >= report["report"]["accounts"][0]["event_rows"]
                .as_array()
                .unwrap()
                .len() as u64
    );
    assert!(!report["report"]["accounts"][0]["fill_rows"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        report["report"]["accounts"][0]["event_rows"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "每个账户的事件流必须通过 replay digest 校验后进入报告"
    );
    assert!(report["report"]["accounts"][0]["fill_rows"]
        .as_array()
        .unwrap()
        .iter()
        .any(|fill| fill.get("event_id").is_some()));
    assert!(report_html.is_file());
    let html = std::fs::read_to_string(&report_html).expect("read multi-account HTML");
    assert!(html.contains("账户之间的现金、持仓、风险和收益彼此独立"));
    assert!(html.contains("BTCUSDT.BINANCE") && html.contains("ETHUSDT.BINANCE"));
    assert!(
        html.contains("id=\"search\"")
            && html.contains("id=\"status\"")
            && html.contains("id=\"minimum\"")
    );
    assert!(
        html.contains("方向字段缺席") && html.contains("数量 raw") && html.contains("price_raw")
    );
    assert!(html.contains("撮合与账簿事件追溯"));
    assert!(html.contains("最近 250 笔成交和 250 条事件"));
    assert!(html.contains("minT=times.reduce((a,b)=>Math.min(a,b),Infinity)"));
    assert!(!html.contains("Math.min(...times)"));
    assert!(html.contains("id=\"event-filter\"") && html.contains("account-acct-btc-event-"));
    let diagnostic_accounts = report["report"]["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|account| {
            json!({
                "account_id": account["account_id"],
                "report": {
                    "schema_version": 1,
                    "kind": "qianxing-strategy-bias-diagnostics",
                    "strategy_id": account["strategy_id"],
                    "instrument": account["instrument"],
                    "data_fingerprint": account["data_fingerprint"],
                    "input_kind": "bar",
                    "input_bars": 15,
                    "as_of": 1_700_000_000_000_u64,
                    "lookahead": {"status": "no_change_detected", "checks": []},
                    "startup": {"status": "stable_across_requested_windows", "windows": []},
                    "data_visibility": {"status": "visibility_unknown", "findings": []}
                }
            })
        })
        .collect::<Vec<_>>();
    let diagnostics_path = root.join("multi-account-diagnostics.json");
    std::fs::write(
        &diagnostics_path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "kind": "qianxing-multi-account-strategy-bias-diagnostics",
            "accounts": diagnostic_accounts.clone()
        }))
        .unwrap(),
    )
    .expect("write multi-account diagnostics");
    let diagnostics_xlsx = root.join("multi-account-diagnostics.xlsx");
    let diagnostic_report = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["report"])
        .arg(&result_path)
        .args(["--json", "--html", "--output"])
        .arg(&report_html)
        .arg("--xlsx")
        .arg(&diagnostics_xlsx)
        .arg("--strategy-diagnostics")
        .arg(&diagnostics_path)
        .arg("--evidence")
        .output()
        .expect("generate diagnostics-linked multi-account report");
    assert_eq!(
        diagnostic_report.status.code(),
        Some(0),
        "multi-account diagnostics report failed: {}",
        String::from_utf8_lossy(&diagnostic_report.stderr)
    );
    let diagnostic_report: Value =
        serde_json::from_slice(&diagnostic_report.stdout).expect("diagnostic report JSON");
    assert_eq!(
        diagnostic_report["report"]["strategy_diagnostics_attachment"]["account_count"],
        2
    );
    assert_eq!(
        diagnostic_report["generated_artifacts"]["xlsx"].as_str(),
        Some(diagnostics_xlsx.to_string_lossy().as_ref())
    );
    let mut workbook = zip::ZipArchive::new(
        std::fs::File::open(&diagnostics_xlsx).expect("open multi-account XLSX"),
    )
    .expect("multi-account report is a valid XLSX package");
    let mut workbook_index = String::new();
    workbook
        .by_name("xl/workbook.xml")
        .expect("workbook index")
        .read_to_string(&mut workbook_index)
        .expect("read workbook index");
    for sheet in [
        "Overview",
        "Accounts",
        "Equity",
        "Fills",
        "Events",
        "StrategyDiagnostics",
        "DiagnosticEvidence",
    ] {
        assert!(workbook_index.contains(sheet), "missing {sheet} sheet");
    }
    let mut diagnostic_summary = String::new();
    workbook
        .by_name("xl/worksheets/sheet6.xml")
        .expect("strategy diagnostic summary sheet")
        .read_to_string(&mut diagnostic_summary)
        .expect("read diagnostic summary");
    assert!(diagnostic_summary.contains("acct-btc"));
    assert!(diagnostic_summary.contains("no_change_detected"));
    let mut diagnostic_evidence = String::new();
    workbook
        .by_name("xl/worksheets/sheet7.xml")
        .expect("strategy diagnostic evidence sheet")
        .read_to_string(&mut diagnostic_evidence)
        .expect("read diagnostic evidence");
    assert!(diagnostic_evidence.contains("acct-btc"));
    assert!(
        diagnostic_report["report"]["strategy_diagnostics_attachment"]["input_sha256"]
            .as_str()
            .is_some_and(|digest| digest.len() == 64)
    );
    assert!(diagnostic_report["report"]["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|account| account["strategy_bias_diagnostics"]["identity_bound"] == true));
    let evidence_accounts = diagnostic_report["report"]["accounts"].as_array().unwrap();
    assert_eq!(evidence_accounts.len(), 2);
    for account in evidence_accounts {
        let evidence_path = PathBuf::from(
            account["run_evidence_path"]
                .as_str()
                .expect("account evidence path"),
        );
        assert!(
            evidence_path.is_file(),
            "missing {}",
            evidence_path.display()
        );
        let verification = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
            .args(["evidence", "verify-run"])
            .arg(&evidence_path)
            .output()
            .expect("verify account run evidence");
        assert_eq!(
            verification.status.code(),
            Some(0),
            "account evidence verification failed: {}",
            String::from_utf8_lossy(&verification.stderr)
        );
    }
    let portable_dir = root.join("portable-acct-btc");
    let btc_evidence = PathBuf::from(
        evidence_accounts
            .iter()
            .find(|account| account["account_id"] == "acct-btc")
            .expect("BTC account report")["run_evidence_path"]
            .as_str()
            .expect("BTC account evidence path"),
    );
    let exported = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "export-run"])
        .arg(&btc_evidence)
        .arg("--output-dir")
        .arg(&portable_dir)
        .output()
        .expect("export account run evidence");
    assert_eq!(
        exported.status.code(),
        Some(0),
        "account evidence export failed: {}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let portable_evidence = portable_dir.join("evidence.json");
    let portable_verification = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "verify-run"])
        .arg(&portable_evidence)
        .output()
        .expect("verify portable account run evidence");
    assert_eq!(
        portable_verification.status.code(),
        Some(0),
        "portable account evidence verification failed: {}",
        String::from_utf8_lossy(&portable_verification.stderr)
    );
    let portable_verification_json: Value =
        serde_json::from_slice(&portable_verification.stdout).expect("portable verification JSON");
    assert_eq!(
        portable_verification_json["strategy_diagnostics_verified"], true,
        "portable verification must semantically bind the diagnostic attachment"
    );
    let portable_bundle: Value = serde_json::from_slice(
        &std::fs::read(&portable_evidence).expect("read portable evidence bundle"),
    )
    .expect("parse portable evidence bundle");
    let diagnostic_relative = portable_bundle
        .pointer("/generation/run/strategy_diagnostics/input_path")
        .and_then(Value::as_str)
        .expect("portable diagnostic attachment path");
    let portable_diagnostic = portable_dir.join(diagnostic_relative);
    let mut tampered = std::fs::read(&portable_diagnostic).expect("read portable diagnostic");
    tampered.push(b' ');
    std::fs::write(&portable_diagnostic, tampered).expect("tamper portable diagnostic");
    let rejected_tamper = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "verify-run"])
        .arg(&portable_evidence)
        .output()
        .expect("reject tampered portable diagnostic");
    assert_ne!(rejected_tamper.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rejected_tamper.stderr).contains("策略诊断 SHA-256 不匹配"));

    let batch_portable_dir = root.join("portable-multi-account");
    let packed_batch = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "pack-multi-account-run"])
        .arg(&result_path)
        .arg("--spec")
        .arg(&spec_path)
        .arg("--output-dir")
        .arg(&batch_portable_dir)
        .output()
        .expect("pack multi-account run evidence");
    assert_eq!(
        packed_batch.status.code(),
        Some(0),
        "multi-account evidence packaging failed: {}",
        String::from_utf8_lossy(&packed_batch.stderr)
    );
    let packed_json: Value =
        serde_json::from_slice(&packed_batch.stdout).expect("batch evidence pack JSON");
    assert_eq!(packed_json["accounts_verified"], 2);
    let packed_manifest: Value = serde_json::from_slice(
        &std::fs::read(batch_portable_dir.join("multi-account-evidence.json"))
            .expect("read batch evidence manifest"),
    )
    .expect("parse batch evidence manifest");
    assert_eq!(packed_manifest["accounts"].as_array().unwrap().len(), 2);
    let packaged_spec: Value = serde_json::from_slice(
        &std::fs::read(batch_portable_dir.join("multi-account-spec.json"))
            .expect("read portable batch spec"),
    )
    .expect("parse portable batch spec");
    assert!(packaged_spec["runs"].as_array().unwrap().iter().all(|run| {
        run["bars_path"]
            .as_str()
            .is_some_and(|path| path.starts_with("accounts/"))
    }));
    let moved_batch_dir = root.join("relocated").join("multi-account");
    std::fs::create_dir_all(moved_batch_dir.parent().unwrap()).expect("create relocation parent");
    std::fs::rename(&batch_portable_dir, &moved_batch_dir).expect("relocate batch package");
    let verified_batch = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "verify-multi-account-run"])
        .arg(&moved_batch_dir)
        .output()
        .expect("verify relocated multi-account evidence");
    assert_eq!(
        verified_batch.status.code(),
        Some(0),
        "relocated multi-account evidence failed verification: {}",
        String::from_utf8_lossy(&verified_batch.stderr)
    );
    let verified_batch_json: Value =
        serde_json::from_slice(&verified_batch.stdout).expect("batch verification JSON");
    assert_eq!(verified_batch_json["verified_account_count"], 2);
    let packaged_result_path = moved_batch_dir.join("multi-account-result.json");
    let original_packaged_result =
        std::fs::read(&packaged_result_path).expect("read portable result");
    let mut changed_result = original_packaged_result.clone();
    changed_result.push(b' ');
    std::fs::write(&packaged_result_path, changed_result).expect("tamper portable result");
    let rejected_batch_tamper = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "verify-multi-account-run"])
        .arg(&moved_batch_dir)
        .output()
        .expect("reject tampered multi-account evidence");
    assert_ne!(rejected_batch_tamper.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rejected_batch_tamper.stderr).contains("SHA-256 不匹配"));
    std::fs::write(&packaged_result_path, original_packaged_result)
        .expect("restore portable batch result after tamper check");

    let diagnostic_html = std::fs::read_to_string(&report_html).expect("read diagnostic HTML");
    assert!(diagnostic_html.contains("策略未来数据与预热诊断"));
    assert!(diagnostic_html.contains("id=\"strategy-diagnostic-status\""));
    assert!(diagnostic_html.contains("JSON.stringify(d,null,2)"));
    let generated_diagnostics_path = root.join("generated-multi-account-diagnostics.json");
    let generated_diagnostic = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["strategy", "diagnose-multi-account"])
        .arg(&result_path)
        .arg("--spec")
        .arg(&spec_path)
        .arg("--output")
        .arg(&generated_diagnostics_path)
        .arg("--html")
        .output()
        .expect("generate multi-account strategy diagnostics");
    assert_eq!(
        generated_diagnostic.status.code(),
        Some(0),
        "multi-account strategy diagnostics failed: {}",
        String::from_utf8_lossy(&generated_diagnostic.stderr)
    );
    let generated_diagnostics: Value = serde_json::from_slice(
        &std::fs::read(&generated_diagnostics_path).expect("read generated diagnostics"),
    )
    .expect("parse generated multi-account diagnostics");
    assert_eq!(
        generated_diagnostics["kind"],
        "qianxing-multi-account-strategy-bias-diagnostics"
    );
    assert_eq!(
        generated_diagnostics["accounts"].as_array().unwrap().len(),
        2
    );
    assert!(generated_diagnostics["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|account| account["report"]["lookahead"]["status"] == "no_change_detected"));
    let generated_html = root.join("multi-account-result.report.html");
    assert!(generated_html.is_file());
    let generated_html = std::fs::read_to_string(generated_html).expect("read generated HTML");
    assert!(generated_html.contains("策略未来数据与预热诊断"));
    assert!(generated_html.contains("acct-btc-sma"));
    let mut incomplete: Value = serde_json::from_slice(
        &std::fs::read(&diagnostics_path).expect("read multi-account diagnostics"),
    )
    .expect("parse diagnostics fixture");
    incomplete["accounts"].as_array_mut().unwrap().pop();
    std::fs::write(&diagnostics_path, serde_json::to_vec(&incomplete).unwrap())
        .expect("write incomplete diagnostics");
    let rejected_incomplete = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["report"])
        .arg(&result_path)
        .args(["--json", "--strategy-diagnostics"])
        .arg(&diagnostics_path)
        .output()
        .expect("reject incomplete multi-account diagnostics");
    assert_ne!(rejected_incomplete.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rejected_incomplete.stderr)
        .contains("必须为每个成功账户各提供且仅提供一份"));
    let mut duplicated: Value = serde_json::from_slice(
        &serde_json::to_vec(&json!({
            "schema_version": 1,
            "kind": "qianxing-multi-account-strategy-bias-diagnostics",
            "accounts": diagnostic_accounts.clone()
        }))
        .unwrap(),
    )
    .unwrap();
    let first = duplicated["accounts"][0].clone();
    duplicated["accounts"][1] = first;
    std::fs::write(&diagnostics_path, serde_json::to_vec(&duplicated).unwrap())
        .expect("write duplicate diagnostics");
    let rejected_duplicate = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["report"])
        .arg(&result_path)
        .args(["--json", "--strategy-diagnostics"])
        .arg(&diagnostics_path)
        .output()
        .expect("reject duplicate multi-account diagnostics");
    assert_ne!(rejected_duplicate.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rejected_duplicate.stderr)
        .contains("多账户策略诊断含重复 account_id"));
    let mut mismatched: Value = serde_json::from_slice(
        &serde_json::to_vec(&json!({
            "schema_version": 1,
            "kind": "qianxing-multi-account-strategy-bias-diagnostics",
            "accounts": diagnostic_accounts
        }))
        .unwrap(),
    )
    .expect("parse diagnostics fixture");
    mismatched["accounts"][0]["report"]["data_fingerprint"] = json!("wrong-fingerprint");
    std::fs::write(&diagnostics_path, serde_json::to_vec(&mismatched).unwrap())
        .expect("write mismatched diagnostics");
    let rejected_diagnostics = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["report"])
        .arg(&result_path)
        .args(["--json", "--strategy-diagnostics"])
        .arg(&diagnostics_path)
        .output()
        .expect("reject mismatched multi-account diagnostics");
    assert_ne!(rejected_diagnostics.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rejected_diagnostics.stderr)
        .contains("data_fingerprint 与复核后的回测输入不一致"));
    match python_native::via_python(&repo, "run_multi_account_backtest", &spec) {
        None => eprintln!("[skip] Python leg: PyO3 extension not built"),
        Some(Ok(python)) => {
            assert_eq!(python["spec_fingerprint"], cli["spec_fingerprint"]);
            assert_eq!(
                python["run_manifest"]["canonical_run_spec_fingerprint"],
                cli["run_manifest"]["canonical_run_spec_fingerprint"]
            );
            for index in 0..2 {
                assert_eq!(
                    python["runs"][index]["outcome"]["result_hash"],
                    cli["runs"][index]["outcome"]["result_hash"]
                );
            }
        }
        Some(Err(payload)) => panic!("Python multi-account use case failed: {payload}"),
    }
    let equity_path = PathBuf::from(
        cli["runs"][0]["outcome"]["artifacts"]["equity"]
            .as_str()
            .unwrap(),
    );
    let equity_before = std::fs::read(&equity_path).expect("read account equity artifact");
    let mut corrupted = equity_before.clone();
    corrupted.extend_from_slice(b"tampered\n");
    std::fs::write(&equity_path, corrupted).expect("tamper account equity fixture");
    let rejected = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["report"])
        .arg(&result_path)
        .args(["--json", "--html", "--output"])
        .arg(&report_html)
        .output()
        .expect("run report against tampered account artifacts");
    std::fs::write(&equity_path, equity_before).expect("restore account equity fixture");
    assert_ne!(rejected.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("运行产物复核未通过"));

    let trace_path = PathBuf::from(
        cli["runs"][0]["outcome"]["artifacts"]["event_trace"]
            .as_str()
            .unwrap(),
    );
    let trace_before = std::fs::read(&trace_path).expect("read account event trace");
    let mut corrupted_trace = trace_before.clone();
    corrupted_trace.extend_from_slice(b"{}\n");
    std::fs::write(&trace_path, corrupted_trace).expect("tamper account event trace");
    let rejected_trace = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["report"])
        .arg(&result_path)
        .args(["--json", "--html", "--output"])
        .arg(&report_html)
        .output()
        .expect("run report against tampered event trace");
    std::fs::write(&trace_path, trace_before).expect("restore account event trace");
    assert_ne!(rejected_trace.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rejected_trace.stderr).contains("事件追溯"));

    let recompute_temp = root.with_file_name(format!(
        "multi-account-recompute-temp-{}",
        std::process::id()
    ));
    let relocated_package = root.with_file_name(format!(
        "multi-account-recompute-package-{}",
        std::process::id()
    ));
    assert!(!recompute_temp.starts_with(&root));
    assert!(!relocated_package.starts_with(&root));
    assert!(!recompute_temp.exists());
    assert!(!relocated_package.exists());
    std::fs::create_dir_all(&recompute_temp).expect("create isolated multi-account recompute temp");
    std::fs::rename(&moved_batch_dir, &relocated_package)
        .expect("move portable batch evidence outside source workspace");
    std::fs::remove_dir_all(&root).expect("remove source runs, specifications and fixtures");
    assert!(
        !root.exists(),
        "source workspace must be absent before recompute"
    );
    let relocated_package_text = relocated_package.to_string_lossy().into_owned();
    let recomputed = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args([
            "evidence",
            "verify-multi-account-run",
            relocated_package_text.as_str(),
            "--recompute",
        ])
        .env("TEMP", &recompute_temp)
        .env("TMP", &recompute_temp)
        .output()
        .expect("recompute relocated multi-account package without source workspace");
    assert_eq!(
        recomputed.status.code(),
        Some(0),
        "portable multi-account recompute failed: {}",
        String::from_utf8_lossy(&recomputed.stderr)
    );
    let recomputed_json: Value =
        serde_json::from_slice(&recomputed.stdout).expect("multi-account recompute JSON");
    assert_eq!(recomputed_json["result_recomputed"], true);
    std::fs::remove_dir_all(&recompute_temp).expect("remove isolated recompute temp directory");
}

#[test]
fn partial_multi_account_package_recomputes_after_source_removal() {
    let root = scratch("multi-account-partial-portable");
    let missing_bars = root.join("missing-account-bars.json");
    let make_member = |account: &str, run: &str, bars: &Path| {
        json!({
            "schema_version": 1,
            "run_id": run,
            "account_id": account,
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": bars,
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 20261026,
            "output_dir": root.join(format!("{account}-output")),
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": format!("{account}-strategy"),
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 2,
                "slow_window": 3,
            },
        })
    };
    let spec_path = root.join("multi-account-spec.json");
    let spec = json!({
        "schema_version": 1,
        "batch_id": "partial-portable-batch",
        "runs": [
            make_member("acct-good", "good-run", Path::new(&sample_bars())),
            make_member("acct-missing", "missing-run", &missing_bars),
        ],
    });
    std::fs::write(&spec_path, serde_json::to_vec_pretty(&spec).unwrap())
        .expect("write partial batch spec");
    let result_path = root.join("multi-account-result.json");
    let batch = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "multi-account-backtest"])
        .arg(&spec_path)
        .output()
        .expect("run partial multi-account batch");
    assert_eq!(
        batch.status.code(),
        Some(0),
        "partial batch CLI failed: {}",
        String::from_utf8_lossy(&batch.stderr)
    );
    let result: Value = serde_json::from_slice(&batch.stdout).expect("partial batch result JSON");
    assert_eq!(result["summary"]["accounts_succeeded"], 1);
    assert_eq!(result["summary"]["accounts_failed"], 1);
    std::fs::write(&result_path, serde_json::to_vec_pretty(&result).unwrap())
        .expect("save partial result");

    let report = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["report"])
        .arg(&result_path)
        .args(["--json", "--evidence"])
        .output()
        .expect("write successful account evidence");
    assert_eq!(
        report.status.code(),
        Some(0),
        "partial batch evidence report failed: {}",
        String::from_utf8_lossy(&report.stderr)
    );
    let package_dir = root.join("portable-batch");
    let packed = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "pack-multi-account-run"])
        .arg(&result_path)
        .arg("--spec")
        .arg(&spec_path)
        .arg("--output-dir")
        .arg(&package_dir)
        .output()
        .expect("pack partial multi-account evidence");
    assert_eq!(
        packed.status.code(),
        Some(0),
        "partial batch packaging failed: {}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let package_manifest: Value = serde_json::from_slice(
        &std::fs::read(package_dir.join("multi-account-evidence.json")).unwrap(),
    )
    .expect("partial package manifest JSON");
    assert_eq!(package_manifest["schema_version"], 2);
    assert_eq!(package_manifest["inputs"].as_array().unwrap().len(), 2);
    assert!(package_manifest["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|input| input["account_id"] == "acct-missing" && input["missing"] == true));
    let good_input_relative = package_manifest["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|input| input["account_id"] == "acct-good")
        .unwrap()["path"]
        .as_str()
        .unwrap();
    let good_input = package_dir.join(good_input_relative);
    let good_input_original = std::fs::read(&good_input).expect("read packaged successful input");
    let mut tampered_good_input = good_input_original.clone();
    tampered_good_input.push(b' ');
    std::fs::write(&good_input, tampered_good_input).expect("tamper packaged successful input");
    let rejected_input_tamper = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "verify-multi-account-run"])
        .arg(&package_dir)
        .output()
        .expect("reject tampered successful account input");
    assert_ne!(rejected_input_tamper.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rejected_input_tamper.stderr).contains("SHA-256 不匹配"));
    std::fs::write(&good_input, good_input_original).expect("restore packaged successful input");

    let missing_input_relative = package_manifest["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|input| input["account_id"] == "acct-missing")
        .unwrap()["path"]
        .as_str()
        .unwrap();
    let unexpected_missing_input = package_dir.join(missing_input_relative);
    std::fs::write(&unexpected_missing_input, b"unexpected")
        .expect("create a file for an input declared missing");
    let rejected_missing_marker = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "verify-multi-account-run"])
        .arg(&package_dir)
        .output()
        .expect("reject changed missing-input state");
    assert_ne!(rejected_missing_marker.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&rejected_missing_marker.stderr)
        .contains("清单声明输入缺失，但包内文件仍存在"));
    std::fs::remove_file(unexpected_missing_input).expect("restore missing-input marker");

    let relocated_package = root.with_file_name(format!(
        "multi-account-partial-package-{}",
        std::process::id()
    ));
    let recompute_temp =
        root.with_file_name(format!("multi-account-partial-temp-{}", std::process::id()));
    assert!(!relocated_package.exists());
    assert!(!recompute_temp.exists());
    std::fs::create_dir_all(&recompute_temp).expect("create partial recompute temp directory");
    std::fs::rename(&package_dir, &relocated_package).expect("relocate partial evidence package");
    std::fs::remove_dir_all(&root).expect("remove partial batch source workspace");
    let relocated_text = relocated_package.to_string_lossy().into_owned();
    let temp_text = recompute_temp.to_string_lossy().into_owned();
    let recomputed = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args([
            "evidence",
            "verify-multi-account-run",
            relocated_text.as_str(),
            "--recompute",
        ])
        .env("TEMP", &temp_text)
        .env("TMP", &temp_text)
        .output()
        .expect("recompute relocated partial evidence package");
    assert_eq!(
        recomputed.status.code(),
        Some(0),
        "partial portable batch recompute failed: {}",
        String::from_utf8_lossy(&recomputed.stderr)
    );
    let recomputed_json: Value =
        serde_json::from_slice(&recomputed.stdout).expect("partial recompute JSON");
    assert_eq!(recomputed_json["result_recomputed"], true);
    assert_eq!(recomputed_json["verified_account_count"], 1);
    assert_eq!(recomputed_json["failed_account_count"], 1);
    std::fs::remove_dir_all(&recompute_temp).expect("remove partial recompute temp directory");
}

#[test]
fn all_failed_multi_account_package_relocates_and_verifies_without_claiming_recompute() {
    let root = scratch("multi-account-all-failed-portable");
    let make_member = |account: &str, run: &str| {
        json!({
            "schema_version": 1,
            "run_id": run,
            "account_id": account,
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": root.join(format!("missing-{account}.json")),
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 20261026,
            "output_dir": root.join(format!("{account}-output")),
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": format!("{account}-strategy"),
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 2,
                "slow_window": 3,
            },
        })
    };
    let spec_path = root.join("multi-account-spec.json");
    let spec = json!({
        "schema_version": 1,
        "batch_id": "all-failed-portable-batch",
        "runs": [
            make_member("acct-missing-a", "missing-run-a"),
            make_member("acct-missing-b", "missing-run-b"),
        ],
    });
    std::fs::write(&spec_path, serde_json::to_vec_pretty(&spec).unwrap())
        .expect("write all-failed batch spec");
    let batch = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["app", "multi-account-backtest"])
        .arg(&spec_path)
        .output()
        .expect("run all-failed multi-account batch");
    assert_eq!(
        batch.status.code(),
        Some(0),
        "all-failed batch CLI failed: {}",
        String::from_utf8_lossy(&batch.stderr)
    );
    let result: Value = serde_json::from_slice(&batch.stdout).expect("all-failed result JSON");
    assert_eq!(result["summary"]["accounts_succeeded"], 0);
    assert_eq!(result["summary"]["accounts_failed"], 2);
    assert!(result["run_manifest"].is_null());
    let result_path = root.join("multi-account-result.json");
    std::fs::write(&result_path, serde_json::to_vec_pretty(&result).unwrap())
        .expect("save all-failed batch result");

    let package_dir = root.join("portable-batch");
    let packed = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "pack-multi-account-run"])
        .arg(&result_path)
        .arg("--spec")
        .arg(&spec_path)
        .arg("--output-dir")
        .arg(&package_dir)
        .output()
        .expect("pack all-failed multi-account evidence");
    assert_eq!(
        packed.status.code(),
        Some(0),
        "all-failed batch packaging failed: {}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(package_dir.join("multi-account-evidence.json")).unwrap(),
    )
    .expect("all-failed package manifest JSON");
    assert_eq!(manifest["accounts"].as_array().unwrap().len(), 0);
    assert_eq!(manifest["inputs"].as_array().unwrap().len(), 2);
    assert!(manifest["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|input| { input["missing"] == true && input["sha256"].is_null() }));

    let relocated = root.with_file_name(format!(
        "multi-account-all-failed-package-{}",
        std::process::id()
    ));
    std::fs::rename(&package_dir, &relocated).expect("relocate all-failed evidence package");
    std::fs::remove_dir_all(&root).expect("remove all-failed source workspace");
    let verified = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "verify-multi-account-run"])
        .arg(&relocated)
        .output()
        .expect("verify relocated all-failed package");
    assert_eq!(
        verified.status.code(),
        Some(0),
        "all-failed package verification failed: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let verified: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified["verified_account_count"], 0);
    assert_eq!(verified["failed_account_count"], 2);
    assert_eq!(verified["result_recomputed"], false);

    let recompute = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["evidence", "verify-multi-account-run"])
        .arg(&relocated)
        .arg("--recompute")
        .output()
        .expect("refuse dataset recompute without a parent RunManifest");
    assert_ne!(recompute.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&recompute.stderr)
        .contains("全失败或历史无父 RunManifest 的多账户批次不能声明独立复算"));
    std::fs::remove_dir_all(&relocated).expect("remove relocated all-failed package");
}
