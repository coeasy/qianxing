//! 快速回测清单（脱敏录制数据 → 并行多任务）的端到端链路测试。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn deploy(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .join("deploy")
        .join(name)
        .to_string_lossy()
        .to_string()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(args)
        .output()
        .expect("启动 qx-cli 快速回测失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn temp_dir(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "qianxing-fast-backtest-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("创建快速回测临时目录失败");
    root
}

/// 并行调度下任务输出顺序不稳定，因此按行前缀收集 `result_hash=` 后排序比较。
fn hashes(stdout: &str, prefix: &str) -> Vec<String> {
    let mut values = stdout
        .lines()
        .filter(|line| line.starts_with(prefix))
        .filter_map(|line| line.split("result_hash=").nth(1))
        .map(|rest| {
            rest.split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .collect::<Vec<_>>();
    values.sort();
    values
}

#[test]
fn crypto_manifest_runs_every_job_in_parallel_and_is_reproducible() {
    let manifest = deploy("qianxing.fast-backtest.example.json");
    let (code, stdout, stderr) = run(&["fast-backtest", &manifest]);
    assert_eq!(code, 0, "加密现货快速回测失败: {stderr}");
    assert!(
        stdout.contains("[Fast Backtest] manifest=") && stdout.contains("jobs=2 completed=2"),
        "输出缺少任务完成摘要: {stdout}"
    );
    let strategy = hashes(&stdout, "[Strategy · Backtest]");
    assert_eq!(strategy.len(), 2, "每个 job 都应打印一条结果指纹");
    assert_eq!(
        hashes(&stdout, "[RunManifest] run_id="),
        strategy,
        "运行清单的结果指纹必须与策略摘要一致"
    );
    assert!(
        stdout.contains("instrument=BTCUSDT.BINANCE")
            && stdout.contains("instrument=BTC/USDT:USDT.OKX"),
        "多 venue 任务必须各自绑定标的: {stdout}"
    );

    let (second_code, second_stdout, _) = run(&["fast-backtest", &manifest]);
    assert_eq!(second_code, 0);
    assert_eq!(
        hashes(&second_stdout, "[Strategy · Backtest]"),
        strategy,
        "同清单二次运行的结果指纹漂移（并行调度不得影响确定性）"
    );
}

#[test]
fn ashare_manifest_carries_recorded_dataset_provenance() {
    let manifest = deploy("qianxing.fast-backtest.ashare.example.json");
    let (code, stdout, stderr) = run(&["fast-backtest", &manifest]);
    assert_eq!(code, 0, "A 股快速回测失败: {stderr}");
    assert!(
        stdout.contains("jobs=1 completed=1"),
        "任务未完成: {stdout}"
    );
    // 录制数据的来源标识必须透传到数据集指纹与运行清单，否则脱敏 fixture 无法审计。
    assert!(
        stdout.contains("source=ashare-example-qfq-v1"),
        "数据集缺少录制来源标记: {stdout}"
    );
    assert!(
        stdout.contains("[RunManifest] run_id=strategy-backtest:strategy-ashare:000001.SZSE"),
        "缺少运行清单: {stdout}"
    );
    let strategy = hashes(&stdout, "[Strategy · Backtest]");
    assert_eq!(strategy.len(), 1);
    assert_eq!(hashes(&stdout, "[RunManifest] run_id="), strategy);
}

#[test]
fn manifest_validation_rejects_empty_jobs_and_missing_fields() {
    let root = temp_dir("invalid");
    let empty = root.join("empty.json");
    std::fs::write(&empty, r#"{"schema_version":1,"jobs":[]}"#).expect("写入空清单失败");
    let (code, _, stderr) = run(&["fast-backtest", &empty.to_string_lossy()]);
    assert_ne!(code, 0, "空 jobs 清单没有被拒绝");
    assert!(
        stderr.contains("1..=256"),
        "报错未说明 jobs 数量约束: {stderr}"
    );

    let missing = root.join("missing.json");
    std::fs::write(
        &missing,
        r#"{"schema_version":1,"jobs":[{"runtime":"qianxing.runtime.ashare.example.json"}]}"#,
    )
    .expect("写入缺字段清单失败");
    let (code, _, stderr) = run(&["fast-backtest", &missing.to_string_lossy()]);
    assert_ne!(code, 0, "缺少 bars 的清单没有被拒绝");
    assert!(
        stderr.contains("jobs[0] 缺少 bars"),
        "报错未定位到具体任务: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
