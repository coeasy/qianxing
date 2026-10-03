//! Strategy worker 的 stderr 尾部窗口：把"读到的每一行"和"读管道这件事本身失败"记成同一本账。
//!
//! `strategy_host.rs` 的两条诊断通道（`diagnostics()` 与 `death_note()`）只看这个窗口，
//! 窗口空着就被说成「worker 无 stderr 输出」。所以把读取错误静默丢掉的写法是在撒谎：
//! "我们读不下去了"（管道被切断、行编码坏了）与"它一个字都没写"在失败归因上是两回事，
//! 前者该指向宿主与子进程之间的管道，后者才指向解释器占位桩（V13 R2 #203）。
//! 同一条读链上的响应通道（stdout）本来就把读取错误发回调用方，这里补齐不对称的另一半。

use std::collections::VecDeque;
use std::io;

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
