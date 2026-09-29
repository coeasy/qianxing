//! 响应泵的三颗界限（V13 C8）：队列有界、单行有上限、迟到的答复不串进下一次请求。
//!
//! 这三格的危害方式都是"worker 不守规矩时由父进程付账"，正常路径的用例永远碰不到，
//! 所以逐颗直接打在泵的本体上，而不是等一颗话痨 Python worker 把 CI 拖成 OOM。

use super::*;
use crate::strategy_host::{
    drain_stale_responses, jsonl_line_cap_bytes, read_jsonl_line_within, StrategyWireResponse,
    STRATEGY_PUMP_BACKLOG,
};
use std::io::Cursor;
use std::sync::mpsc;

const LF: u8 = 0x0A;
const CR: u8 = 0x0D;

/// 按行拼一份 worker 输出：行与行之间用 `terminator`，末行不带它——那就是"半行就关流"的写法。
fn worker_output_with(terminator: &[u8], lines: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        bytes.extend_from_slice(line);
        if index + 1 < lines.len() {
            bytes.extend_from_slice(terminator);
        }
    }
    bytes
}

fn worker_output(lines: &[&[u8]]) -> Vec<u8> {
    worker_output_with(&[LF], lines)
}

fn read_first(bytes: &[u8], cap: usize) -> Result<Option<String>, String> {
    read_jsonl_line_within(&mut Cursor::new(bytes.to_vec()), cap)
}

/// 队列写满后必须当场拒绝——这正是"有界"唯一可观测的形状（生产泵用的是会堵住的
/// `send`，堵住与拒收的界限同为一颗：`STRATEGY_PUMP_BACKLOG`）。
#[test]
fn response_backlog_is_a_bounded_channel() {
    let (sender, receiver) = mpsc::sync_channel::<u8>(STRATEGY_PUMP_BACKLOG);
    for index in 0..STRATEGY_PUMP_BACKLOG {
        let slot = u8::try_from(index).expect("backlog 预算内可表示");
        assert!(
            sender.try_send(slot).is_ok(),
            "第 {index} 颗就写不进去：队列比声明的 {STRATEGY_PUMP_BACKLOG} 颗浅"
        );
    }
    assert!(
        matches!(sender.try_send(0), Err(mpsc::TrySendError::Full(_))),
        "队列写满后仍然收得下：泵会继续替话痨 worker 囤货，STRATEGY_PUMP_BACKLOG 形同虚设"
    );
    assert_eq!(
        receiver.iter().take(STRATEGY_PUMP_BACKLOG).count(),
        STRATEGY_PUMP_BACKLOG
    );
}

/// 单行上限只准复用分帧支那颗字节数，不留第二个数。
#[test]
fn jsonl_line_cap_reuses_the_frame_budget() {
    assert_eq!(
        jsonl_line_cap_bytes(),
        DEFAULT_MAX_FRAME_BYTES,
        "JSONL 支另起了一份单行上限，分帧支的预算不再是唯一真相"
    );
}

#[test]
fn oversized_line_is_rejected_instead_of_buffered() {
    let error = read_first(&[b'x'; 64], 8).expect_err("超过上限的单行必须报错，不能静默交出");
    assert!(
        error.contains("超过 8 字节上限"),
        "报错没点名踩到的上限: {error}"
    );
}

#[test]
fn trailing_line_without_newline_is_still_delivered() {
    let mut reader = Cursor::new(worker_output(&[br#"{"a":1}"#, br#"{"b":2}"#]));
    assert_eq!(
        read_jsonl_line_within(&mut reader, 64).unwrap().as_deref(),
        Some(r#"{"a":1}"#)
    );
    assert_eq!(
        read_jsonl_line_within(&mut reader, 64).unwrap().as_deref(),
        Some(r#"{"b":2}"#)
    );
    assert!(
        read_jsonl_line_within(&mut reader, 64).unwrap().is_none(),
        "EOF 之后必须交 None，泵才有机会送出那句「已关闭输出」"
    );
}

#[test]
fn crlf_terminates_a_line_without_leaking_the_carriage_return() {
    let mut reader = Cursor::new(worker_output_with(&[CR, LF], &[b"one", b"two"]));
    for expected in ["one", "two"] {
        assert_eq!(
            read_jsonl_line_within(&mut reader, 64).unwrap().as_deref(),
            Some(expected),
            "CRLF 的行尾处理漏了字符"
        );
    }
    assert!(read_jsonl_line_within(&mut reader, 64).unwrap().is_none());
}

/// 一行跨多次 `fill_buf` 时才走得完累积路径：缓冲只给 3 颗，行有 4 颗。
#[test]
fn line_spanning_several_fill_windows_is_reassembled() {
    let mut reader = BufReader::with_capacity(3, Cursor::new(worker_output(&[b"abcd", b"ef"])));
    assert_eq!(
        read_jsonl_line_within(&mut reader, 64).unwrap().as_deref(),
        Some("abcd")
    );
    assert_eq!(
        read_jsonl_line_within(&mut reader, 64).unwrap().as_deref(),
        Some("ef")
    );
}

/// 超时后躺在队列里的残粒：数量要数得出来，清完要为空，发送端关了也不能转圈。
#[test]
fn drain_clears_queued_responses_and_reports_the_count() {
    let (sender, receiver) = mpsc::sync_channel::<Result<StrategyWireResponse, String>>(4);
    sender
        .send(Ok(StrategyWireResponse::JsonLine("迟到".into())))
        .unwrap();
    sender
        .send(Err("读取 Strategy worker 响应失败".into()))
        .unwrap();
    assert_eq!(
        drain_stale_responses(&receiver),
        2,
        "两颗残粒没数对，下一次请求会读到上一颗"
    );
    assert_eq!(
        drain_stale_responses(&receiver),
        0,
        "队列没清空就报清干净了"
    );
    drop(sender);
    assert_eq!(
        drain_stale_responses(&receiver),
        0,
        "发送端已断开时也必须收口，而不是永远排不完"
    );
}
