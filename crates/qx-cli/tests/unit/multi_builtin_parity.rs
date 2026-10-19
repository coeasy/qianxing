use super::*;

/// `backtest multi-builtin` 入口：多腿归因产物必须由腿级共用装配的真实成交计提。
#[test]
fn multi_builtin_backtest_entry_writes_spread_attribution_from_leg_fills() {
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let root = temp_cli_case_dir("multi-builtin-attribution");
    run_multi_builtin_backtest(
        "pairs_arbitrage",
        &deploy.join("qianxing.bar-frame.pairs-primary.example.json"),
        &deploy.join("qianxing.bar-frame.pairs-reference.example.json"),
        None,
        None,
        None,
        None,
        1,
        0,
        Some(&root),
        None,
    )
    .unwrap();
    let artifacts = std::fs::read_dir(root.join("runs"))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            path.file_name()?
                .to_string_lossy()
                .ends_with(".spread-attribution.json")
                .then_some(path)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        artifacts.len(),
        1,
        "多腿回测应产出一份归因产物: {}",
        root.display()
    );
    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&artifacts[0]).unwrap()).unwrap();
    assert_eq!(payload["strategy_id"], "builtin-pairs_arbitrage-multi");
    assert_eq!(payload["primary_instrument"], "BTCUSDT.BINANCE");
    assert_eq!(payload["reference_instrument"], "ETHUSDT.BINANCE");
    assert_eq!(payload["funding_bps"], 0);
    // V11 Q58：缺规格时按现货乘数 1 记账，产物必须把"哪条腿带了规格、这一轮到底有没有
    // 会被计提保证金的腿"写出来，否则 `margin_peak_raw=0` 分不清现货与漏配。
    assert_eq!(payload["market_specs"]["primary"], serde_json::Value::Null);
    assert_eq!(
        payload["market_specs"]["reference"],
        serde_json::Value::Null
    );
    assert_eq!(payload["margin_model"], "none-no-derivative-leg-spec");
    assert_eq!(payload["totals"]["margin_peak_raw"], "0");
    assert_eq!(payload["totals"]["funding_raw"], "0");
    let groups = payload["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 3, "三组配对信号应全部归因");
    let turnover: i128 = payload["totals"]["turnover_raw"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let fees: i128 = payload["totals"]["fees_raw"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let filled: i128 = payload["totals"]["filled_qty_raw"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(turnover > 0 && filled > 0, "腿级成交必须进入归因");
    assert_eq!(
        fees * 10_000,
        turnover * 5,
        "无 market spec 的现货腿应全部按 taker 5 bp 计提，且与单标的回测同一费用口径"
    );
    let runs = std::fs::read_dir(root.join("runs"))
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    let summaries = runs
        .iter()
        .filter(|path| path.to_string_lossy().ends_with(".summary.json"))
        .collect::<Vec<_>>();
    assert_eq!(
        summaries.len(),
        3,
        "多腿链应为两条腿和组合各写一份 v5 可复核摘要"
    );
    let mut result_hashes = summaries
        .iter()
        .map(|summary_path| {
            let summary: serde_json::Value = serde_json::from_slice(
                &std::fs::read(summary_path).expect("读取多腿等价基线 summary"),
            )
            .expect("多腿等价基线 summary JSON 有效");
            summary["result_hash"]
                .as_str()
                .expect("多腿 summary result_hash")
                .to_string()
        })
        .collect::<Vec<_>>();
    result_hashes.sort();
    assert_eq!(
        result_hashes,
        [
            "4674f7667f4ead02",
            "4e40f0d5e8e79687",
            "5b408f7f29ecfd26bbe6b3ea6af8869f3b544acfb52be3a6536a5cf801978146",
        ],
        "默认双腿输入的旧 CLI 结果哈希多重集是迁移对拍基线"
    );
    assert_eq!(
        runs.len(),
        17,
        "默认双腿输入应包含三组五件套、一份组合 EvidenceBundle 和一份 spread attribution"
    );
    assert_eq!(
        runs.iter()
            .filter(|path| path.to_string_lossy().ends_with(".run.json"))
            .count(),
        3,
        "两条腿和组合各有 RunManifest"
    );
    assert_eq!(
        runs.iter()
            .filter(|path| path.to_string_lossy().ends_with(".record.json"))
            .count(),
        3,
        "两条腿和组合各有 RunRecord"
    );
    assert_eq!(
        runs.iter()
            .filter(|path| path.to_string_lossy().ends_with(".evidence.json"))
            .count(),
        1,
        "组合回测必须额外写一份可复核 EvidenceBundle"
    );
    assert_eq!(
        runs.iter()
            .filter(|path| path.to_string_lossy().ends_with(".spread-attribution.json"))
            .count(),
        1,
        "组合成交必须写出一份 spread attribution"
    );
    for summary_path in summaries {
        let summary: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(summary_path).expect("读取多腿 v7 summary"),
        )
        .expect("多腿 v7 summary JSON 有效");
        assert_eq!(summary["schema_version"], 7);
        if summary["input"]["kind"] == "multi-barframe" {
            assert!(summary["run_manifest"].as_str().is_some());
        } else {
            assert_eq!(summary["replay"]["log_digest"], summary["result_hash"]);
        }
        recompute_declared_backtest_input(&summary)
            .expect("report 复核单腿重放或组合输入与四份工件")
            .expect("单腿与组合 summary 均声明真实输入");
        run_report(summary_path, true, false).expect("report 命令可复核并输出多腿 v7 summary");
    }
    let _ = std::fs::remove_dir_all(&root);
}
