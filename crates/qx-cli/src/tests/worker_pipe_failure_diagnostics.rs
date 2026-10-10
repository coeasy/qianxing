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
/// 第四种结局是"一行长得读不完"（V13 R5）：它由 `read_capped_worker_line` 交出，本用例
/// 同时验它的四种返回形状，因为这条读链的失败必须一路走到这里被记成一行事实。
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
    // 读链本身（V13 R5）：界内的行按 `Lines` 的口径剥掉行尾，尾行没有换行也要交出，
    // 管道 EOF 说成"没有行"，只有越界那臂中止整条泵并点名上限。
    let mut pipe = std::io::BufReader::new(std::io::Cursor::new(b"first\r\nsecond".to_vec()));
    assert_eq!(
        read_capped_worker_line(&mut pipe, 16).unwrap().as_deref(),
        Some("first"),
        "CRLF 行尾必须剥掉，与改前的 lines() 等价"
    );
    assert_eq!(
        read_capped_worker_line(&mut pipe, 6).unwrap().as_deref(),
        Some("second"),
        "恰好等界的尾行不能被误伤"
    );
    assert_eq!(
        read_capped_worker_line(&mut pipe, 16).unwrap(),
        None,
        "管道 EOF 要说成没有行，而不是读取失败"
    );
    let mut over = std::io::BufReader::new(std::io::Cursor::new(vec![b'x'; 40]));
    let cap_error = read_capped_worker_line(&mut over, 8).unwrap_err();
    assert!(cap_error.contains("单行超过 8 字节上限"), "{cap_error}");
    // 越界必须能走到尾部窗口这一臂：宿主把它包成一次读取失败，诊断就要说清是行长越界。
    let note = stderr_tail_note(Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        cap_error,
    )))
    .expect("一行读不完也是一条事实，不能替 worker 宣称没输出");
    assert!(note.contains("字节上限"), "{note}");
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
/// V13 R5 把同一对接线判据扩到行长：两条读链都必须走 `read_capped_worker_line`，
/// 因为 `lines()` 只在 EOF 或 io 错误处停，一字节不换行的管道能把本进程吃到内存耗尽。
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
    assert_eq!(
        source
            .matches("read_capped_worker_line(&mut reader, DEFAULT_MAX_FRAME_BYTES)")
            .count(),
        2,
        "两条读链（stderr 诊断与 stdout 应答）的行长闸不是都在位，掉的那条把缓冲一路涨下去"
    );
    assert!(
        !source.contains("for line in reader.lines()"),
        "读链回到 `lines()`：无界的一行会连同 #203 的记账形状一起失效"
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

/// #281 行为判据：子进程"活着但从不读 stdin"时，写侧必须受 timeout_ms 收口。
///
/// 与本文件另一条 `dead_worker_write_failure_*` 用例互补：那条造的是"worker 先死"，
/// 写侧只能拿到断管道并快速失败；这一条造的是"worker 存活却不接收输入"，超管道缓冲的
/// write_all 会永久阻塞——修复前 `timeout_ms` 只守读侧 recv，本调用永不返回。ping 继承 stdin
/// 读端却一字节都不取，且是被直接跟踪的子进程，`kill()` 即关闭读端、放行写线程。
///
/// 文案取自 `qx-adapter::io_budget::write_all_within`：三处子进程 stdin 写入统一走它之后，
/// 写侧超时只报「未在 N ms 预算内写完」，由调用方（本 crate 的 worker 客户端）再补上
/// 「程序=…/进程状态」两格。断言按**这个**口径写——钉的是"走预算通道 + 交代进程状态"，
/// 不是某一句具体措辞。
#[test]
fn live_but_non_draining_worker_write_is_bounded_not_hanging() {
    #[cfg(windows)]
    let (program, args) = {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        (
            format!("{system_root}\\System32\\PING.EXE"),
            vec!["-n".into(), "300".into(), "127.0.0.1".into()],
        )
    };
    #[cfg(unix)]
    let (program, args) = ("/bin/sleep".to_string(), vec!["300".into()]);
    let mut client = PythonStrategyClient::start_process_with_transport_config(
        &program,
        &args,
        &BTreeMap::new(),
        PYTHON_STRATEGY_TIMEOUT_MS,
        "Python Strategy",
        StrategyTransport::Jsonl,
        SharedRingConfig::default(),
        Some("来自 QX_PYTHON"),
    )
    .expect("启动存活但不读 stdin 的假 worker 失败");
    let started = std::time::Instant::now();
    let error = client
        .request(&oversized_input(40_000))
        .expect_err("worker 存活但不接收输入：写入必须在预算内失败，而不是永久阻塞");
    let elapsed = started.elapsed();
    assert!(
        error.contains("输入失败") && error.contains("预算内写完"),
        "写侧失败没走到预算通道（说明仍在无限阻塞或误落断管道通道）: {error}",
    );
    assert!(
        error.contains("程序=") && error.contains("来自 QX_PYTHON"),
        "预算通道也要说明是哪个解释器: {error}",
    );
    assert!(
        error.contains("退出码") || error.contains("进程未退出"),
        "预算通道也要交代子进程状态: {error}",
    );
    assert!(
        elapsed < std::time::Duration::from_millis(PYTHON_STRATEGY_TIMEOUT_MS * 5),
        "写入没有被 timeout_ms 收口：耗时 {elapsed:?} 说明调用仍在无限阻塞",
    );
}
