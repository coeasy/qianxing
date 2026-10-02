//! 写侧（stdin）失败通道的诊断必须与读侧超时通道说同样的话。
//!
//! `crates/qx-cli/tests/worker_launch_diagnostics.rs` 只能覆盖"子进程恰好还活着时写成功"
//! 的那一半：那里 payload 只有一千多字节，匿名管道缓冲吃得下，写失败纯属调度运气，
//! 所以断管道这条通道（os error 232 / 109）长期只在整跑里随机红、从不常驻红。
//! 本用例把 payload 撑到远大于管道缓冲，于是"worker 先死 → 写侧必败"成为必然路径。
//!
//! 同一份诊断链的读侧（#203）也住在这里：stderr 尾部窗口一旦把"读取中断"静默丢掉，
//! `death_note()` 就会以"worker 无 stderr 输出"的口吻替子进程说话。

use super::*;

/// #203 行为判据：一次读取的三种结果要分得开——有内容、没内容、读不下去。
/// 前两种共用"窗口里多一行/不多"的形状没问题，第三种必须留下事实，否则空窗口就会
/// 被 `death_note()` 解释成"它一个字都没写"。
#[test]
fn stderr_tail_records_a_broken_pipe_as_a_fact_instead_of_silence() {
    assert_eq!(
        stderr_tail_note(Ok("   ".to_string())),
        None,
        "空白行不值得记"
    );
    assert_eq!(
        stderr_tail_note(Ok(" Traceback ".to_string())).as_deref(),
        Some("Traceback"),
        "真实输出要按行留下"
    );
    let note = stderr_tail_note(Err(std::io::Error::other("管道正在被关闭")))
        .expect("读取中断必须是一条事实，不能被当成没输出");
    assert!(
        note.contains("stderr 读取中断") && note.contains("管道正在被关闭"),
        "读取中断没留下原因: {note}"
    );
    let long = "x".repeat(STDERR_TAIL_MAX_CHARS + 200);
    assert_eq!(
        stderr_tail_note(Ok(long)).unwrap().chars().count(),
        STDERR_TAIL_MAX_CHARS,
        "单行必须截到登记过的长度，一行 traceback 不能挤掉整条诊断链"
    );
}

/// #203 窗口容量：只留最后若干行，且淘汰的是最旧的那条。
#[test]
fn stderr_tail_window_keeps_only_the_last_lines() {
    let mut tail = std::collections::VecDeque::new();
    for index in 0..(STDERR_TAIL_LINES * 2 + 1) {
        record_stderr_tail(&mut tail, format!("line {index}"));
    }
    assert_eq!(tail.len(), STDERR_TAIL_LINES, "窗口容量没生效");
    assert_eq!(
        (
            tail.front().map(String::as_str),
            tail.back().map(String::as_str)
        ),
        (
            Some(format!("line {}", STDERR_TAIL_LINES + 1).as_str()),
            Some(format!("line {}", STDERR_TAIL_LINES * 2).as_str())
        ),
        "淘汰的不是最旧那条"
    );
}

/// #203 接线判据：`strategy_host.rs` 的 stderr 尾部不再只收 `Ok` 行。
/// 同文件的响应（stdout）通道一直会把读取错误外传（`读取 Strategy worker 响应失败`），
/// 两条读链一条说实话、一条替 worker 宣称"没输出"，在日志里看不出区别。
#[test]
fn strategy_host_stderr_tailer_reports_broken_reads_too() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let source = std::fs::read_to_string(
        root.join("crates")
            .join("qx-cli")
            .join("src")
            .join("strategy_host.rs"),
    )
    .unwrap();
    assert!(
        !source.contains("reader.lines().map_while(Result::ok)"),
        "stderr 尾部又只收成功行：读中断会被报成 worker 无 stderr 输出"
    );
    assert!(
        source.contains("let broken = line.is_err();")
            && source.contains("if let Some(note) = stderr_tail_note(line)")
            && source.contains("record_stderr_tail(&mut tail, note)"),
        "stderr 尾部不再走 #203 那份记账，或读中断后没有停下（`Lines` 不 fuse，坏管道会空转）"
    );
    assert!(
        source.contains("读取 Strategy worker 响应失败"),
        "响应通道的读取错误外传被摘掉：两条读链的口径差异就没人管了"
    );
}

/// 造一份写不完的输入：Bar 列足够长，序列化后的 JSON 远大于 OS 匿名管道缓冲。
/// 子进程从不读 stdin，所以超出缓冲的那部分只能以断管道失败收场。
fn oversized_input(rows: usize) -> StrategyContractInput {
    StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-broken-pipe".into(),
        strategy_id: "strategy-broken-pipe".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-sha256".into(),
        as_of: 1_700_000_000,
        instrument: "BTC/USDT.OKX".into(),
        positions: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
        available_margin_raw: Some(100_000_000),
        risk_state: "ready".into(),
        research_targets: BTreeMap::new(),
        bars: Some(StrategyContractBars {
            source: "synthetic".into(),
            ts: (0..rows as u64)
                .map(|index| 1_700_000_000 + index)
                .collect(),
            open_raw: vec![1_i128; rows],
            high_raw: vec![1_i128; rows],
            low_raw: vec![1_i128; rows],
            close_raw: vec![1_i128; rows],
            volume_raw: vec![1_i128; rows],
        }),
    }
}

#[test]
fn dead_worker_write_failure_names_program_origin_and_exit_state() {
    // 用测试二进制自己当解释器：它不认 `-m`，打印一行错误就退出，且从不读 stdin。
    let executable = std::env::current_exe().expect("测试二进制路径不可用");
    let mut client = PythonStrategyClient::start_process_with_transport_config(
        &executable.to_string_lossy(),
        &[
            "-m".into(),
            "qianxing_strategy.worker".into(),
            "--module".into(),
            "unused.py".into(),
        ],
        &BTreeMap::new(),
        PYTHON_STRATEGY_TIMEOUT_MS,
        "Python Strategy",
        StrategyTransport::Jsonl,
        SharedRingConfig::default(),
        Some("来自 QX_PYTHON"),
    )
    .expect("启动假 worker 失败");
    let error = client
        .request(&oversized_input(20_000))
        .expect_err("worker 不读 stdin 且必然退出，写入必须失败");
    assert!(
        error.contains("输入失败"),
        "失败必须走写侧通道（而不是碰巧落到超时/断链通道）: {error}"
    );
    assert!(
        error.contains("程序=") && error.contains("来自 QX_PYTHON"),
        "写侧失败必须说明是哪个解释器: {error}"
    );
    assert!(
        error.contains("退出码") || error.contains("进程未退出"),
        "写侧失败必须说明子进程状态: {error}"
    );
    assert!(
        error.contains("stderr="),
        "子进程已退出时，写侧失败必须带上它留下的 stderr 尾部: {error}"
    );
}
