//! Strategy worker 的 stderr 尾部窗口：把"读到的每一行"和"读管道这件事本身失败"记成同一本账。
//!
//! `strategy_host.rs` 的两条诊断通道（`diagnostics()` 与 `death_note()`）只看这个窗口，
//! 窗口空着就被说成「worker 无 stderr 输出」。所以把读取错误静默丢掉的写法是在撒谎：
//! "我们读不下去了"（管道被切断、行编码坏了）与"它一个字都没写"在失败归因上是两回事，
//! 前者该指向宿主与子进程之间的管道，后者才指向解释器占位桩（V13 R2 #203）。
//! 同一条读链上的响应通道（stdout）本来就把读取错误发回调用方，这里补齐不对称的另一半。
//!
//! 这条读链的行长闸（`read_capped_worker_line`）也住在这里：它与尾部窗口管的是同一件事的两种
//! 失败形状——"读不下去"要留下事实，"一行长得读不完"要提前中止，两者都不能变成安静的一行。

use std::collections::VecDeque;
use std::io;
use std::io::{BufRead, BufReader, Read};

/// 单行截断长度：与窗口容量一起是 stderr 诊断的唯一口径来源。
pub(crate) const STDERR_TAIL_MAX_CHARS: usize = 512;

/// 窗口容量：只留最后若干行，失败时看到的就是死亡前的最后几句话。
pub(crate) const STDERR_TAIL_LINES: usize = 16;

/// 这一次读取该往窗口里记什么。`None` 表示没有值得记的事实（空白行）；
/// 读取失败必须记，而且记的是"读中断"，不是替 worker 宣称它没写。
pub(crate) fn stderr_tail_note(line: Result<String, io::Error>) -> Option<String> {
    match line {
        Ok(line) => {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            Some(line.chars().take(STDERR_TAIL_MAX_CHARS).collect())
        }
        Err(error) => Some(format!("<stderr 读取中断: {error}>")),
    }
}

/// 记进窗口并按容量淘汰最旧的行。
pub(crate) fn record_stderr_tail(tail: &mut VecDeque<String>, note: String) {
    tail.push_back(note);
    while tail.len() > STDERR_TAIL_LINES {
        tail.pop_front();
    }
}

/// 读 worker 管道的一行，但把这一行卡在 `limit`：`take` 多放行一字节，读到第 `limit + 1`
/// 字节就是越界——宁可中止整条泵也不让一行的缓冲一路攒下去。子进程少写一个换行符就能把
/// 本进程吃到内存耗尽，而 `BufReader::lines()` 对此毫无办法：它只在 EOF 或 io 错误处停。
/// `Ok(None)` 才是管道 EOF（子进程退出），`Err` 是这一行必须中止整条泵——两者都要让调用侧
/// 看见，静默丢掉一行等于把"应答没读到"说成"worker 什么都没发生"（同 #203 的记账口径）。
/// 行尾的 `\n`/`\r` 按 `Lines` 的口径剥掉，调用侧拿到的仍是纯内容，与改前的 `lines()` 等价。
pub(crate) fn read_capped_worker_line<R: Read>(
    reader: &mut BufReader<R>,
    limit: usize,
) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > limit {
        return Err(format!("worker 输出单行超过 {limit} 字节上限，泵已中止"));
    }
    if bytes.ends_with(b"\n") {
        bytes.pop();
    }
    if bytes.ends_with(b"\r") {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| format!("worker 输出不是 UTF-8: {error}"))
}
