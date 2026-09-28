//! 快速回测清单（脱敏录制数据 → 并行多任务）的端到端链路测试。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn deploy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .join("deploy")
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
    let root = temp_dir("crypto");
    copy_examples_into(&root);
    let manifest = root
        .join("qianxing.fast-backtest.example.json")
        .to_string_lossy()
        .to_string();
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
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ashare_manifest_carries_recorded_dataset_provenance() {
    let root = temp_dir("ashare");
    copy_examples_into(&root);
    let manifest = root
        .join("qianxing.fast-backtest.ashare.example.json")
        .to_string_lossy()
        .to_string();
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
    let _ = std::fs::remove_dir_all(&root);
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

/// 把 deploy 目录的示例输入整体搬进临时目录，并把每份运行时示例的产物根改写到那里，
/// 避免用例把 `runs/` 落在仓库里（同一份配置在仓库内是会被反复 bless 的状态，V12 #82）。
/// 快速回测清单同样要搬：manifest 里的 job 路径按 manifest 所在目录解析，所以搬完的清单
/// 指向的就是搬完的示例。
fn copy_examples_into(root: &Path) {
    let mut redirected = 0usize;
    for entry in std::fs::read_dir(deploy_dir()).expect("读取 deploy 目录失败") {
        let path = entry.expect("读取 deploy 条目失败").path();
        let Some(name) = path.file_name() else {
            continue;
        };
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let target = root.join(name);
        std::fs::copy(&path, &target).expect("复制示例输入失败");
        let text = std::fs::read_to_string(&target).expect("读取示例输入失败");
        // 没有 data_dir 的示例（如纯 spec/夹具）原样留着即可。
        if !text.contains("\"data_dir\"") {
            continue;
        }
        let redirected_text = regex_replace_data_dir(
            &text,
            &root.join("data").to_string_lossy().replace('\\', "/"),
        );
        assert_ne!(
            redirected_text,
            text,
            "{} 的 data_dir 写法已变，用例需同步",
            name.to_string_lossy()
        );
        std::fs::write(&target, redirected_text).expect("写入临时运行时配置失败");
        redirected += 1;
    }
    assert!(
        redirected > 0,
        "deploy 目录里没有任何带 data_dir 的运行时示例，用例的隔离前提已变"
    );
}

/// 把 deploy 目录的示例输入整体搬进临时目录，并把回测产物根改写到那里，
/// 避免用例把 `runs/` 落在仓库里（同一份配置在仓库内是会被反复 bless 的状态）。
fn isolated_example(root: &Path, runtime_name: &str) -> PathBuf {
    copy_examples_into(root);
    root.join(runtime_name)
}

/// 只替换 `"data_dir": "<value>"` 这一处，并把示例的 `data/<子目录>` 尾部接到临时目录下：
/// 并行 job 因此仍各写各的产物目录，不会在同一瞬间争着建同一个目录。
fn regex_replace_data_dir(text: &str, data_root: &str) -> String {
    let marker = "\"data_dir\": \"";
    let Some(start) = text.find(marker) else {
        return text.to_string();
    };
    let value_start = start + marker.len();
    let Some(end) = text[value_start..].find('"') else {
        return text.to_string();
    };
    let old = &text[value_start..value_start + end];
    let suffix = old.rsplit_once("data/").map_or("", |(_, tail)| tail);
    let value = if suffix.is_empty() {
        data_root.to_string()
    } else {
        format!("{data_root}/{suffix}")
    };
    text.replacen(
        &text[start..value_start + end + 1],
        &format!("{marker}{value}\""),
        1,
    )
}

/// 取输出里的成交诚实性行；它和摘要文件必须给同一份拒单事实。
fn integrity_line(stdout: &str) -> &str {
    stdout
        .lines()
        .find(|line| line.starts_with("[Strategy · Integrity]"))
        .unwrap_or_else(|| panic!("没有打印成交诚实性行: {stdout}"))
}

/// 从 `[Strategy · Backtest]` 摘要行取一个 `key=value` 字段。
fn field(line: &str, key: &str) -> String {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|token| token.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("摘要行缺少 {key}: {line}"))
        .to_string()
}

/// 读取临时产物目录里唯一的回测摘要。产物目录按运行时示例分档（`data/<示例名>/runs/`），
/// 所以要递归找：并行 job 各写各的档，把它们压进同一个 `runs/` 只会让用例互相踩。
fn first_summary(data_root: &Path) -> serde_json::Value {
    let path = summary_files(data_root)
        .into_iter()
        .min()
        .unwrap_or_else(|| panic!("回测产物里没有摘要文件: {}", data_root.display()));
    let text = std::fs::read_to_string(&path).expect("读取回测摘要失败");
    serde_json::from_str(&text).expect("回测摘要不是合法 JSON")
}

fn summary_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries.flatten().fold(Vec::new(), |mut found, entry| {
        let path = entry.path();
        if path.is_dir() {
            found.extend(summary_files(&path));
        } else if path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.ends_with(".summary.json"))
        {
            found.push(path);
        }
        found
    })
}

/// 三份 §4.18 示例必须真的成交并真的付手续费：帧长够得上策略窗口、A 股时间戳够
/// 得上交易日历、数量给到可用的定点裸值。修复前这三份夹具的回测产物一律
/// `fills=0 / turnover_raw=0 / fees_raw=0`，"示例可直接复现"当时只是空话。
#[test]
fn shipped_examples_fill_positions_and_pay_nonzero_fees() {
    for (label, runtime_name, bars_name) in [
        (
            "binance",
            "qianxing.runtime.builtin-strategy.example.json",
            "qianxing.bar-frame.example.json",
        ),
        (
            "okx",
            "qianxing.runtime.builtin-strategy.okx.example.json",
            "qianxing.bar-frame.okx.example.json",
        ),
        (
            "ashare",
            "qianxing.runtime.ashare.example.json",
            "qianxing.ashare.bar-frame.example.json",
        ),
    ] {
        let root = temp_dir(label);
        let runtime = isolated_example(&root, runtime_name);
        let bars = root.join(bars_name);
        let (code, stdout, stderr) = run(&[
            "strategy",
            "backtest",
            &runtime.to_string_lossy(),
            &bars.to_string_lossy(),
        ]);
        assert_eq!(code, 0, "{label} 示例回测失败: {stderr}");
        let line = stdout
            .lines()
            .find(|line| line.starts_with("[Strategy · Backtest]"))
            .unwrap_or_else(|| panic!("{label} 示例没有打印回测摘要: {stdout}"));
        let fills: u64 = field(line, "fills").parse().expect("fills 不是非负整数");
        assert!(fills > 0, "{label} 示例零成交: {line}");
        let summary = first_summary(&root.join("data"));
        let metrics = summary
            .get("metrics")
            .and_then(|value| value.as_object())
            .unwrap_or_else(|| panic!("{label} 示例摘要缺少 metrics: {summary}"));
        for key in ["turnover_raw", "fees_raw"] {
            let value = metrics
                .get(key)
                .and_then(|value| value.as_i64())
                .unwrap_or_else(|| panic!("{label} 示例摘要缺少 {key}"));
            assert!(
                value > 0,
                "{label} 示例的 {key} 为 {value}，成交没走到费用口径"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// `fills=0` 有两种成因完全不同的来源：策略压根没发信号，或信号全被现金/风控挡下。
/// 只报成交数分不出这两件事，所以产物必须把引擎写进事件日志的拒单事实单独说出来。
#[test]
fn blocked_signals_are_distinguishable_from_silent_strategies() {
    let runtime_name = "qianxing.runtime.builtin-strategy.example.json";
    let bars_name = "qianxing.bar-frame.example.json";

    // 对照侧：夹具原样运行时没有任何拒单，诚实性行必须是 none。
    let healthy = temp_dir("healthy");
    let healthy_runtime = isolated_example(&healthy, runtime_name);
    let (code, healthy_stdout, stderr) = run(&[
        "strategy",
        "backtest",
        &healthy_runtime.to_string_lossy(),
        &healthy.join(bars_name).to_string_lossy(),
    ]);
    assert_eq!(code, 0, "对照回测失败: {stderr}");
    let healthy_integrity = integrity_line(&healthy_stdout);
    assert_eq!(field(healthy_integrity, "rejected_orders"), "0");
    assert_eq!(field(healthy_integrity, "rejection_reasons"), "none");
    assert_eq!(
        first_summary(&healthy.join("data"))
            .get("rejected_orders")
            .and_then(|value| value.as_u64())
            .expect("摘要缺少 rejected_orders"),
        0,
        "对照侧摘要不该记到拒单"
    );
    let _ = std::fs::remove_dir_all(&healthy);

    // 实验侧：同一份夹具只把数量放到名义额远超初始现金，信号必然发得出但成交为 0。
    let blocked = temp_dir("blocked");
    let runtime = isolated_example(&blocked, runtime_name);
    let text = std::fs::read_to_string(&runtime).expect("读取示例运行时配置失败");
    let inflated = text.replacen(
        "\"builtin_quantity\": 1000000000,",
        "\"builtin_quantity\": 100000000000000,",
        1,
    );
    assert_ne!(
        inflated, text,
        "{runtime_name} 的 builtin_quantity 写法已变，用例需同步"
    );
    std::fs::write(&runtime, &inflated).expect("改写数量失败");
    let (code, stdout, stderr) = run(&[
        "strategy",
        "backtest",
        &runtime.to_string_lossy(),
        &blocked.join(bars_name).to_string_lossy(),
    ]);
    assert_eq!(code, 0, "放量回测失败: {stderr}");
    let line = stdout
        .lines()
        .find(|line| line.starts_with("[Strategy · Backtest]"))
        .unwrap_or_else(|| panic!("没有打印回测摘要: {stdout}"));
    assert_eq!(
        field(line, "fills"),
        "0",
        "放量后仍然成交，现金门禁没有咬住"
    );
    let integrity = integrity_line(&stdout);
    assert_ne!(
        field(integrity, "rejected_orders"),
        "0",
        "零成交却没有拒单事实，fills=0 与'信号全被挡下'仍然无法区分"
    );
    assert_ne!(field(integrity, "rejection_reasons"), "none");

    let summary = first_summary(&blocked.join("data"));
    assert_eq!(
        summary
            .get("rejected_orders")
            .and_then(|value| value.as_u64())
            .expect("摘要缺少 rejected_orders"),
        1,
        "摘要里的拒单数与事件日志不一致: {summary}"
    );
    let reasons = summary
        .get("rejection_reasons")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("摘要缺少 rejection_reasons: {summary}"));
    assert_eq!(reasons.len(), 1, "拒单原因未归并: {reasons:?}");
    assert_eq!(
        reasons[0]
            .get("count")
            .and_then(|value| value.as_u64())
            .expect("拒单原因缺少次数"),
        1
    );
    assert!(
        reasons[0]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("现金"),
        "拒单原因不是现金门禁: {:?}",
        reasons[0]
    );
    let _ = std::fs::remove_dir_all(&blocked);
}
