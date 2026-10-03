//! L1/L2 深度档位回测的端到端链路测试：CLI 入口、统一产物和确定性。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .join("deploy")
        .join(name)
}

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "qianxing-depth-backtest-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("创建回测产物目录失败");
    root
}

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(args)
        .output()
        .expect("启动 qx-cli 深度回测失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn result_hash(stdout: &str) -> String {
    stdout
        .lines()
        .find(|line| line.starts_with("[Depth · Backtest]"))
        .and_then(|line| line.split("result_hash=").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_string)
        .expect("输出缺少 [Depth · Backtest] result_hash")
}

fn artifacts(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let runs = root.join("runs");
    let mut manifest = None;
    for entry in std::fs::read_dir(&runs).expect("读取 runs 目录失败") {
        let path = entry.expect("读取 runs 条目失败").path();
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if name.ends_with(".run.json") {
            manifest = Some(path);
        }
    }
    let manifest = manifest.expect("缺少 RunManifest");
    let stem = manifest
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .strip_suffix(".run.json")
        .expect("RunManifest 文件名非法")
        .to_string();
    (
        manifest,
        runs.join(format!("{stem}.summary.json")),
        runs.join(format!("{stem}.equity.csv")),
    )
}

#[test]
fn depth_backtest_writes_unified_artifacts_and_stays_deterministic() {
    let root = temp_root("l2");
    let frame = fixture("qianxing.depth-frame.example.json");
    let frame = frame.to_string_lossy().to_string();
    let first_root = root.join("first");
    let (code, stdout, stderr) = run(&[
        "backtest",
        "book",
        "--fill-tier",
        "l2",
        "--root",
        &first_root.to_string_lossy(),
        "sma-cross",
        &frame,
    ]);
    assert_eq!(code, 0, "L2 深度回测失败: {stderr}");
    assert!(stdout.contains("snapshots=80"), "输出缺少快照数: {stdout}");
    assert!(stdout.contains("fills=1"), "输出缺少成交笔数: {stdout}");
    // 深度示例的 SMA 下穿会发卖空意图，保守风控把它挡下：这正是"零成交"之外
    // 必须能看见的事实，否则产物里只剩一个看不出原因的 fills=0。
    let integrity = stdout
        .lines()
        .find(|line| line.starts_with("[Depth · Integrity]"))
        .unwrap_or_else(|| panic!("深度链没有打印成交诚实性行: {stdout}"));
    assert!(
        integrity.contains("rejected_orders=1"),
        "被风控挡下的卖空没有记进诚实性行: {integrity}"
    );
    assert!(
        integrity.contains("1x") && integrity.contains("NoShort"),
        "诚实性行缺少拒单原因: {integrity}"
    );
    let first_hash = result_hash(&stdout);

    let second_root = root.join("second");
    let (code, stdout, _) = run(&[
        "backtest",
        "book",
        "--fill-tier",
        "l2",
        "--root",
        &second_root.to_string_lossy(),
        "sma-cross",
        &frame,
    ]);
    assert_eq!(code, 0);
    assert_eq!(
        result_hash(&stdout),
        first_hash,
        "同参数二次回测结果指纹漂移"
    );

    let (manifest, summary, equity) = artifacts(&first_root);
    let manifest_payload = std::fs::read_to_string(&manifest).expect("读取 RunManifest 失败");
    assert!(manifest_payload.contains("\"depth-backtest:l2:sma_cross"));
    assert!(manifest_payload.contains("example-depth-l2-v1"));
    let summary_payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&summary).expect("读取摘要失败")).unwrap();
    assert_eq!(summary_payload["sample_unit"], "depth_snapshot");
    assert_eq!(summary_payload["bars"], 80);
    assert_eq!(summary_payload["fills"], 1);
    assert_eq!(summary_payload["rejected_orders"], 1);
    let rejections = summary_payload["rejection_reasons"]
        .as_array()
        .expect("rejection_reasons 必须是数组");
    assert_eq!(
        rejections
            .iter()
            .filter(|entry| entry["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("NoShort"))
            .count(),
        1,
        "摘要里的拒单原因与诚实性行不一致: {rejections:?}"
    );
    assert_eq!(
        summary_payload["metrics"]["final_equity_raw"],
        100_265_589_000_000_i64
    );
    assert_eq!(
        summary_payload["risk_rules"]["rule_set_version"],
        "conservative-default-v1+implicit-no-short"
    );
    assert!(summary_payload["assumptions"]
        .as_array()
        .expect("assumptions 必须是数组")
        .iter()
        .any(|value| value == "data_tier=L2L3"));
    let equity_lines = std::fs::read_to_string(&equity)
        .expect("读取权益曲线失败")
        .lines()
        .count();
    assert_eq!(equity_lines, 81, "权益曲线必须逐快照一点");

    let changed_root = root.join("changed");
    let (code, stdout, _) = run(&[
        "backtest",
        "book",
        "--fill-tier",
        "l2",
        "--root",
        &changed_root.to_string_lossy(),
        "--fee-bps",
        "50",
        "sma-cross",
        &frame,
    ]);
    assert_eq!(code, 0);
    assert_ne!(result_hash(&stdout), first_hash, "费率变化必须改变结果指纹");
    let (_, changed_summary, _) = artifacts(&changed_root);
    let changed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&changed_summary).expect("读取摘要失败"))
            .unwrap();
    assert!(changed["metrics"]["fees_raw"].as_i64().unwrap() > 32_411_000_000_i64);

    let _ = std::fs::remove_dir_all(&root);
}

fn summary(root: &Path) -> serde_json::Value {
    let (_, path, _) = artifacts(root);
    let payload = std::fs::read_to_string(&path).expect("读取摘要失败");
    serde_json::from_str(&payload).expect("摘要不是合法 JSON")
}

fn execution_line(stdout: &str) -> String {
    stdout
        .lines()
        .find(|line| line.starts_with("[Depth · Execution]"))
        .unwrap_or_else(|| panic!("深度链没有打印撮合模型参数行: {stdout}"))
        .to_string()
}

fn has_descriptor(summary: &serde_json::Value, needle: &str) -> bool {
    summary["model_descriptors"]
        .as_array()
        .expect("model_descriptors 必须是数组")
        .iter()
        .any(|value| value.as_str().unwrap_or_default().contains(needle))
}

/// V11 Q1a：接到命令面的撮合参数必须"换了就真的换结果"，并且产物能读出实际口径。
/// 内核的第三项 `queue_position_bps` 故意不做成旗标——它只作用于限价单，而内置策略
/// 一律发市价单（`qx-strategy/src/builtin.rs` 的 intent 恒为 `limit: None`），
/// 声明一个换不动任何成交的旗标就是 Q0b 判掉的假风控形状。
#[test]
fn depth_execution_model_flags_change_results_and_are_recorded() {
    let root = temp_root("execution-model");
    let frame = fixture("qianxing.depth-frame.example.json");
    let frame = frame.to_string_lossy().to_string();
    let run_model = |label: &str, extra: &[&str]| -> (String, String, serde_json::Value) {
        let out = root.join(label);
        let out = out.to_string_lossy().to_string();
        let mut args = vec!["backtest", "book", "--fill-tier", "l2", "--root", &out];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["sma-cross", &frame]);
        let (code, stdout, stderr) = run(&args);
        assert_eq!(code, 0, "{label} 深度回测失败: {stderr}");
        (
            execution_line(&stdout),
            result_hash(&stdout),
            summary(Path::new(&out)),
        )
    };

    let (baseline_line, baseline_hash, baseline) = run_model("default", &[]);
    assert_eq!(
        baseline_line, "[Depth · Execution] latency_snapshots=0 market_impact_bps=0",
        "缺省撮合口径必须显式写成全 0，而不是不告诉使用者用了什么"
    );
    assert!(
        has_descriptor(
            &baseline,
            "latency_snapshots=0 queue_position_bps=0 market_impact_bps=0"
        ),
        "摘要没有写出实际撮合模型参数: {:?}",
        baseline["model_descriptors"]
    );
    let baseline_fees = baseline["metrics"]["fees_raw"].as_i64().unwrap();
    let baseline_equity = baseline["metrics"]["final_equity_raw"].as_i64().unwrap();

    let (impact_line, impact_hash, impact) = run_model("impact", &["--market-impact-bps", "50"]);
    assert_eq!(
        impact_line,
        "[Depth · Execution] latency_snapshots=0 market_impact_bps=50"
    );
    assert!(
        has_descriptor(&impact, "market_impact_bps=50"),
        "冲击参数没有写进摘要: {:?}",
        impact["model_descriptors"]
    );
    assert_ne!(impact_hash, baseline_hash, "冲击参数没有改变结果指纹");
    assert!(
        impact["metrics"]["fees_raw"].as_i64().unwrap() > baseline_fees,
        "买入被推高后手续费没有变化: {:?} vs {baseline_fees}",
        impact["metrics"]["fees_raw"]
    );
    assert!(
        impact["metrics"]["final_equity_raw"].as_i64().unwrap() < baseline_equity,
        "单边冲击没有让终值变差，参数没有真的进撮合"
    );

    let (latency_line, latency_hash, latency) = run_model("latency", &["--latency-snapshots", "2"]);
    assert_eq!(
        latency_line,
        "[Depth · Execution] latency_snapshots=2 market_impact_bps=0"
    );
    assert!(
        has_descriptor(&latency, "latency_snapshots=2"),
        "延迟参数没有写进摘要: {:?}",
        latency["model_descriptors"]
    );
    assert_ne!(latency_hash, baseline_hash, "延迟参数没有改变结果指纹");
    assert_ne!(
        latency_hash, impact_hash,
        "两种延迟/冲击口径共用同一结果指纹"
    );
    assert_ne!(
        latency["metrics"]["final_equity_raw"].as_i64().unwrap(),
        baseline_equity,
        "延后成交换到的价格与逐档吃单一致，延迟没有进撮合"
    );

    let (code, _, stderr) = run(&[
        "backtest",
        "book",
        "--fill-tier",
        "l2",
        "--root",
        &root.join("queue").to_string_lossy(),
        "--queue-position-bps",
        "5000",
        "sma-cross",
        &frame,
    ]);
    assert_ne!(code, 0, "队列前置旗标没有落点却仍被接受");
    assert!(
        stderr.contains("--queue-position-bps"),
        "报错没有点名被拒的旗标: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn l1_depth_backtest_runs_and_rejects_multi_level_books() {
    let root = temp_root("l1");
    let l1_frame = fixture("qianxing.depth-frame.l1.example.json");
    let l1_frame = l1_frame.to_string_lossy().to_string();
    let (code, stdout, stderr) = run(&[
        "backtest",
        "book",
        "--fill-tier",
        "l1",
        "--root",
        &root.to_string_lossy(),
        "sma-cross",
        &l1_frame,
    ]);
    assert_eq!(code, 0, "L1 深度回测失败: {stderr}");
    assert!(stdout.contains("tier=l1"), "输出缺少档位标记: {stdout}");
    let (_, summary, _) = artifacts(&root);
    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&summary).expect("读取摘要失败")).unwrap();
    assert!(payload["assumptions"]
        .as_array()
        .expect("assumptions 必须是数组")
        .iter()
        .any(|value| value == "data_tier=L1"));

    let l2_frame = fixture("qianxing.depth-frame.example.json");
    let (code, _, stderr) = run(&[
        "backtest",
        "book",
        "--fill-tier",
        "l1",
        "--root",
        &root.join("mismatch").to_string_lossy(),
        "sma-cross",
        &l2_frame.to_string_lossy(),
    ]);
    assert_ne!(code, 0, "L1 档位接受多档盘口却没有报错");
    assert!(
        stderr.contains("只接受一档盘口"),
        "报错未说明档位约束: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
