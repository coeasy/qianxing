//! 回测轨验收的行为面（P0-1 / §46）。
//!
//! `tools/backtest_acceptance.py` 会生成 `maturity/backtest_acceptance.yaml` 这份可核对的记录，
//! 但记录是**产物**，不是判据：它可能过时，也可能被手改。这里把同一件事钉进 `cargo test`——
//! 两轮 `backtest` 的产物必须逐字节相等，且全程不需要任何交易所凭据。
//!
//! 为什么这条轨单独存在：`maturity/evidence/testnet/` 那份 Binance 验收是 `skipped`（缺凭据），
//! 于是能力矩阵的 `sandbox_tested` / `production_approved` 只能全 false。那两档只对**需要外部
//! venue** 的能力有意义；本仓的主用法是回测与 Paper，它一条凭据都不用。这条用例就是
//! 「不需要凭据也能完整验收」这句话的行为证据。

use std::path::{Path, PathBuf};
use std::process::Command;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_qx-cli"))
}

/// 每个用例一个独立目录；集成用例并行跑，共用目录会互相踩产物。
fn temp_base(label: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "qianxing-backtest-acceptance-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("创建临时目录失败");
    base
}

/// 从子进程环境里摘掉一切凭据变量：这条轨的意义就是"没有凭据也能跑完"。
fn run(cwd: &Path, args: &[&str]) -> (i32, String) {
    let mut command = Command::new(binary());
    command.current_dir(cwd).args(args);
    for (name, _) in std::env::vars() {
        if name.starts_with("QX_BINANCE_")
            || name.starts_with("QX_CCXT_")
            || name.starts_with("QX_OKX_")
        {
            command.env_remove(&name);
        }
    }
    let output = command.output().expect("启动被测 binary 失败");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), text)
}

/// 建一个项目并回读它的运行时路径。
fn quickstart(root: &Path, project: &Path) -> PathBuf {
    let (code, output) = run(root, &["quickstart", &project.to_string_lossy()]);
    assert_eq!(code, 0, "quickstart 失败：\n{output}");
    let runtime = project.join("qianxing.runtime.json");
    assert!(runtime.is_file(), "quickstart 没有交出运行时配置");
    runtime
}

/// 跑一轮 `backtest`，回读这一轮点名的四份产物路径。
///
/// 按 stdout 取路径而不是扫 `runs/` 目录：`quickstart` 自带的那一轮与受控的这两轮落在同一个
/// 目录里，扫目录会把它们混在一起。
fn backtest(project: &Path, root: &Path) -> Vec<PathBuf> {
    let (code, output) = run(
        root,
        &[
            "backtest",
            &project.join("qianxing.runtime.json").to_string_lossy(),
            &project
                .join("qianxing.bar-frame.example.json")
                .to_string_lossy(),
            &project
                .join("qianxing.binance.spot.spec.json")
                .to_string_lossy(),
        ],
    );
    assert_eq!(code, 0, "backtest 失败：\n{output}");

    let manifest = output
        .lines()
        .find_map(|line| line.strip_prefix("[RunManifest] path="))
        .expect("输出里没有 [RunManifest] path=")
        .trim()
        .to_string();
    let artifacts = output
        .lines()
        .find(|line| line.starts_with("[Artifacts]"))
        .expect("输出里没有 [Artifacts] 行")
        .to_string();

    let mut paths = vec![PathBuf::from(&manifest)];
    for key in ["summary=", "equity=", "fills="] {
        let start = artifacts.find(key).expect("产物行缺少字段") + key.len();
        let rest = &artifacts[start..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        paths.push(PathBuf::from(rest[..end].trim()));
    }
    for path in &paths {
        assert!(path.is_file(), "输出点名的产物不在盘：{}", path.display());
    }
    paths.sort();
    paths
}

/// 从摘要里取 `result_hash`（不引入 JSON 依赖，按键值对切）。
fn result_hash(project: &Path) -> String {
    let summaries: Vec<PathBuf> = std::fs::read_dir(project.join("data/qianxing/runs"))
        .expect("读取 runs 目录失败")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.to_string_lossy().ends_with(".summary.json"))
        .collect();
    assert!(!summaries.is_empty(), "没有 summary 产物");
    let text = std::fs::read_to_string(&summaries[0]).expect("读取摘要失败");
    let key = "\"result_hash\"";
    let start = text.find(key).expect("摘要里没有 result_hash") + key.len();
    let rest = &text[start..];
    let start = rest.find('"').expect("result_hash 不是字符串") + 1;
    let end = rest[start..].find('"').expect("result_hash 没有收尾引号") + start;
    rest[start..end].to_string()
}

/// 同目录重跑：四份产物**逐字节**相等。这是 §46 确定性的最直接形状。
#[test]
fn same_directory_reruns_are_byte_identical() {
    let root = temp_base("rerun");
    let project = root.join("project");
    quickstart(&root, &project);

    let first = backtest(&project, &root);
    // 先把第一轮的字节读进内存：第二轮会原地覆盖同名产物，读晚了就只剩第二轮。
    let before: Vec<Vec<u8>> = first
        .iter()
        .map(|path| std::fs::read(path).expect("读取第一轮产物失败"))
        .collect();

    let second = backtest(&project, &root);
    assert_eq!(
        first, second,
        "同目录重跑点名的产物路径必须一致（配置指纹决定文件名后缀）"
    );
    for (path, expected) in first.iter().zip(before) {
        assert_eq!(
            std::fs::read(path).expect("读取第二轮产物失败"),
            expected,
            "{} 重跑后内容变了",
            path.display()
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// 两个互相独立的项目：`result_hash` 必须相等，且全程没有任何凭据。
#[test]
fn independent_projects_agree_on_result_hash_without_credentials() {
    let root = temp_base("independent");
    let mut hashes = Vec::new();
    for label in ["a", "b"] {
        let project = root.join(label);
        quickstart(&root, &project);
        backtest(&project, &root);
        hashes.push(result_hash(&project));

        let (code, output) = run(
            &root,
            &[
                "status",
                &project.join("qianxing.runtime.json").to_string_lossy(),
                "--json",
            ],
        );
        assert_eq!(code, 0, "status 失败：\n{output}");
        assert!(
            output.contains("\"network_accessed\": false"),
            "回测轨不得访问网络：\n{output}"
        );
        assert!(
            output.contains("\"orders_sent\": false"),
            "回测轨不得发出订单：\n{output}"
        );
    }
    assert_eq!(
        hashes[0], hashes[1],
        "两个独立目录的 result_hash 必须相等：结果与路径无关"
    );
    assert!(!hashes[0].is_empty(), "result_hash 不能为空");
    let _ = std::fs::remove_dir_all(&root);
}
