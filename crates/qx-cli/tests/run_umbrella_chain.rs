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
    let base = std::env::temp_dir().join(format!(
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

    let (report_code, report_out, report_err) = run_in(&base, &["run", "report"]);
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

    let _ = std::fs::remove_dir_all(&base);
}
