//! `run` 统一入口的缺省运行时必须**只有一份**：`run backtest` 写、`run report` 读。
//!
//! 立案的是 V13 R28。`run` 自称"统一执行常用安全入口"（`cli_help.rs`），而它最常用的一对就是
//! `run backtest` 写产物、`run report` 读产物；可它的数据面子命令各自解析默认运行时——
//! `run backtest` 走内置策略模板（`storage.data_dir = data/qianxing-builtin-strategy`），
//! `run doctor` / `run runtime-check` / `run report` 走顶层那份
//! （`qianxing.runtime.example.json`，`data/qianxing`）。于是刚写出的摘要读侧一个都找不到：
//! 实测 `未找到回测摘要: data/qianxing\runs` 并以 2 退出。两份模板只差默认运行时不同，
//! 所以"在某台机器上碰巧能跑通"只是因为那台机器的 `data/qianxing/runs` 里正好有更早的产物。
//!
//! 用例把"写→读"这条链在两份默认之间真跑一遍，并**逐字比对 `result_hash`**：只断言两个入口
//! 都退 0 是不够的——各自退 0 而读的是别处的旧产物同样会绿，那正是这个缺陷在真实机器上的形态。

use std::path::Path;
use std::process::Command;

fn run_in(cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(cwd)
        // Offline replay creates a private work directory under the process temp root.
        // Keep that root inside this test's writable isolated directory on Windows.
        .env("TEMP", cwd)
        .env("TMP", cwd)
        .args(args)
        .output()
        .expect("启动 qx-cli 失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 从 `[Strategy · Backtest] … result_hash=<16 位十六进制>` 那一行取结果哈希。
fn result_hash(output: &str) -> String {
    output
        .split("result_hash=")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .filter(|hash| hash.len() == 16)
        .unwrap_or_else(|| panic!("回测输出里没有 result_hash=<16 位>：\n{output}"))
        .to_string()
}

#[test]
fn the_run_umbrella_reads_back_what_its_backtest_step_wrote() {
    // 落在**启动目录**（临时目录）而不是仓库根：这条链的口径就是"产物按进程当前目录取数"，
    // 在仓库根跑会往被 git 跟踪的 `data/` 里写东西。
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!(
            "qianxing-run-umbrella-chain-{}",
            std::process::id()
        ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("创建临时启动目录失败");

    let (backtest_code, backtest_out, backtest_err) = run_in(&base, &["run", "backtest"]);
    assert_eq!(
        backtest_code, 0,
        "`run backtest` 必须退 0（它用的是内置策略模板，不需要 Python 解释器）\n\
         stdout={backtest_out}\nstderr={backtest_err}"
    );
    let written = result_hash(&backtest_out);

    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../deploy/qianxing.runtime.builtin-strategy.example.json")
        .to_string_lossy()
        .into_owned();
    let (report_code, report_out, report_err) = run_in(&base, &["run", "report", runtime.as_str()]);
    assert_eq!(
        report_code, 0,
        "`run report` 必须读到 `run backtest` 刚写出的摘要——两份缺省运行时落到不同的 \
         storage.data_dir 时这里会退 2 并报「未找到回测摘要」\nstderr={report_err}"
    );
    assert!(
        report_out.contains(&written),
        "`run report` 读到的必须是 `run backtest` 刚写的那一轮（result_hash={written}）——\
         只断言两个入口都退 0 会放过「读的是别处旧产物」那一类\nstdout={report_out}"
    );

    let (direct_report_code, direct_report_out, direct_report_err) =
        run_in(&base, &["report", runtime.as_str()]);
    assert_eq!(
        (report_code, &report_out, &report_err),
        (direct_report_code, &direct_report_out, &direct_report_err),
        "`run report` 与顶层 `report` 对同一目录下的同一回测产物必须逐字段一致"
    );

    let runs_dir = base.join("data/qianxing-builtin-strategy/runs");
    let summary = std::fs::read_dir(&runs_dir)
        .expect("backtest must create its run summary directory")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.extension().and_then(|value| value.to_str()) == Some("json")
                && path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|name| name.ends_with(".summary.json"))
        })
        .expect("backtest must create a summary artifact");
    let summary_arg = summary.to_string_lossy().into_owned();
    let (summary_report_code, summary_report_out, summary_report_err) =
        run_in(&base, &["report", summary_arg.as_str()]);
    assert_eq!(
        summary_report_code, 0,
        "`report <summary.json>` 必须读取显式摘要: {summary_report_err}"
    );
    assert_eq!(
        result_hash(&summary_report_out),
        written,
        "显式 summary 路径必须复核 `run backtest` 刚生成的结果"
    );
    assert_eq!(
        result_hash(&summary_report_out),
        result_hash(&direct_report_out),
        "`report <runtime.json>` 与 `report <summary.json>` 必须指向同一回测结果"
    );

    // 报告命令生成的证据包必须能通过真实进程入口独立复核并重放。
    let (evidence_code, evidence_out, evidence_err) =
        run_in(&base, &["report", runtime.as_str(), "--evidence", "--json"]);
    assert_eq!(
        evidence_code, 0,
        "`report --evidence --json` 必须生成可复核证据: {evidence_err}"
    );
    let evidence_json: serde_json::Value =
        serde_json::from_str(&evidence_out).expect("report --evidence --json emits JSON");
    let evidence_path = evidence_json["run_evidence"]["path"]
        .as_str()
        .expect("report JSON returns the generated run EvidenceBundle path");
    let (verify_code, verify_out, verify_err) = run_in(
        &base,
        &["evidence", "verify-run", evidence_path, "--recompute"],
    );
    assert_eq!(
        verify_code, 0,
        "`evidence verify-run --recompute` must independently replay the same run: {verify_err}"
    );
    let verified: serde_json::Value =
        serde_json::from_str(&verify_out).expect("verify-run emits machine-readable JSON");
    assert_eq!(verified["ok"], true);
    assert_eq!(verified["result_recomputed"], true);
    assert_eq!(
        verified["result_hash"].as_str(),
        Some(written.as_str()),
        "offline replay must match the original backtest hash"
    );

    let portable_dir = base.join("portable-evidence");
    let portable_dir_arg = portable_dir.to_string_lossy().into_owned();
    let (export_code, export_out, export_err) = run_in(
        &base,
        &[
            "evidence",
            "export-run",
            evidence_path,
            "--output-dir",
            portable_dir_arg.as_str(),
            "--runtime-config",
            runtime.as_str(),
        ],
    );
    assert_eq!(
        export_code, 0,
        "`evidence export-run` must produce a portable, self-contained bundle: {export_err}"
    );
    let export_json: serde_json::Value =
        serde_json::from_str(&export_out).expect("export-run emits machine-readable JSON");
    assert_eq!(export_json["self_contained_files"], true);
    let portable_bundle = portable_dir.join("evidence.json");
    let portable_bundle_arg = portable_bundle.to_string_lossy().into_owned();
    let (portable_verify_code, portable_verify_out, portable_verify_err) = run_in(
        &base,
        &[
            "evidence",
            "verify-run",
            portable_bundle_arg.as_str(),
            "--recompute",
        ],
    );
    assert_eq!(
        portable_verify_code, 0,
        "portable export must verify and replay after relocation: {portable_verify_err}"
    );
    let portable_verified: serde_json::Value = serde_json::from_str(&portable_verify_out)
        .expect("portable verify-run emits machine-readable JSON");
    assert_eq!(portable_verified["result_recomputed"], true);
    assert_eq!(
        portable_verified["result_hash"].as_str(),
        Some(written.as_str())
    );

    let _ = std::fs::remove_dir_all(&base);
}
