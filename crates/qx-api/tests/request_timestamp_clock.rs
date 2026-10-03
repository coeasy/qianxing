//! V13 R2 第十六遍 #221：一条 HTTP 连接上的时间戳必须**按连接**现取。
//!
//! 立案现场：`serve` 与 `serve_tls_mtls_with_stores` 的签名是 `ts: u64`，值在进 accept 循环之前
//! 就取好了（`crates/qx-cli/src/strategy_contract.rs` 两处传的是 `runtime_timestamp_ms()` 的**结果**），
//! 循环里每条连接都复用同一个数。这个数是三条通道共同的唯一时间来源：
//!
//! - 限流桶的补充（`handle_inner` → `try_acquire`），桶见底之后就再也回不了血；
//! - 控制命令的审计时间（`ControlPlane::submit_as(.., ts)`），全部定格在 API worker 启动那一刻；
//! - 命令入队的租约秒（`lease_clock(ts)`），uptime 越久就越"一落库已过期"。
//!
//! 本用例把第一条与第二条钉住：注入一个每连接递增一秒的墙钟，走真 socket 提两条控制命令，
//! 审计里的 `ts` 必须一个是 `BASE_MS`、一个是 `BASE_MS + 1000`。把 `serve` 改回"入口取一次"
//! （变异 M1）时第二条立刻判红；把接受循环里的 `now()` 换成入口那份常数（M2）同样红。
//!
//! 第三条（限流的时钟域）也在本文件里钉：`handle_inner` 把毫秒戳换算成秒才交给桶，
//! 桶按秒补充。换算缺失时"每秒 100 次"实际是"每毫秒 100 次"，任何持续流量都会读成
//! "额度用不完"，而这份额度是 `DEFAULT_RATE_LIMIT_*` 唯一声明的政策。

use qx_api::{ApiService, ApiState};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

/// 一个真实的 epoch 毫秒戳（2023-11-14），与 `runtime_timestamp_ms()` 同域。
const BASE_MS: u64 = 1_700_000_000_000;

fn stop_channel() -> (Arc<AtomicBool>, impl Fn() -> bool) {
    let flag = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&flag);
    (flag, move || seen.load(Ordering::Acquire))
}

/// 提交一条 `PauseStrategy` 控制命令，返回完整响应文本。
///
/// 命令体按线格式手写（本 crate 的集成用例没有 `qx-control` 可用，这正是它要核对的东西：
/// 文档承诺的请求体形状必须能被裸字符串满足）。
fn submit_command(address: SocketAddr, command_id: u64) -> String {
    let body = format!(
        r#"{{"command_id":{command_id},"request_id":"clock-{command_id}","operator_id":"ops","reason":"时间戳判据","kind":"PauseStrategy","target":"s1","payload":{{}},"permission":"Trading","dry_run":true}}"#
    );
    let mut client = TcpStream::connect(address).expect("连接监听端口");
    client
        .write_all(
            format!("POST /control/commands HTTP/1.1\r\nHost: localhost\r\n\r\n{body}").as_bytes(),
        )
        .expect("写入请求");
    let mut response = String::new();
    client.read_to_string(&mut response).expect("读取响应");
    response
}

/// 从 202 响应体里取出审计记录承诺的 `ts`（`AuditRecord` 的序列化形态）。
fn audited_ts(response: &str) -> u64 {
    let marker = "\"ts\":";
    let at = response
        .find(marker)
        .unwrap_or_else(|| panic!("响应体里没有审计时间戳：{response}"));
    let digits = response[at + marker.len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    digits
        .parse()
        .unwrap_or_else(|error| panic!("审计时间戳不是整数 {digits:?}: {error}"))
}

#[test]
fn the_request_timestamp_is_read_per_connection_not_per_server() {
    let service = ApiService::new(ApiState::default());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = stop_channel();
    // 每读一次前进一秒：本用例断言的正是在这条监听循环里"读了几次钟"。
    let clock = Arc::new(AtomicU64::new(BASE_MS));
    let ticking = Arc::clone(&clock);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(service.serve(
            listener,
            move || ticking.fetch_add(1_000, Ordering::SeqCst),
            stopped,
        ))
        .unwrap()
    });

    let first = submit_command(address, 1);
    assert!(first.starts_with("HTTP/1.1 202"), "{first}");
    let second = submit_command(address, 2);
    assert!(second.starts_with("HTTP/1.1 202"), "{second}");
    assert_eq!(
        audited_ts(&first),
        BASE_MS,
        "第一条命令的审计戳应取到这条连接那一刻的钟"
    );
    assert_eq!(
        audited_ts(&second),
        BASE_MS + 1_000,
        "第二条命令的审计戳必须换一条连接就前进：同一个值说明 serve 把时间戳取在了监听入口，\
         之后这条 API 写出的每条审计、每个租约秒都会定格在进程启动那一刻"
    );
    // 钟被读的次数本身就是判据：两次请求各读一次，多读少读都不算这条链通了。
    assert_eq!(
        clock.load(Ordering::SeqCst),
        BASE_MS + 2_000,
        "监听循环读钟的次数不对"
    );

    stop.store(true, Ordering::Release);
    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("停机后 serve 必须返回");
    assert!(result.is_ok(), "{result:?}");
}

/// 限流桶的时钟域：`handle` 收毫秒、桶按秒补充，两者之间只有 `rate_limit_bucket_seconds` 一处换算。
///
/// 换算缺失时（变异 M3：把 `try_acquire(rate_limit_bucket_seconds(ts))` 改成 `try_acquire(ts)`），
/// 毫秒戳让每格补充放大 1000 倍 —— 第二条断言（同秒内 +5 毫秒仍然见底）立刻判红。
/// 反过来把窗口钉死成"永远不回血"（变异 M4：换算后仍减去余数、或 `handle` 里换成常数）也会红，
/// 因为第三条断言要求跨过一格之后额度回来。
#[test]
fn the_limiter_clock_domain_is_bucket_seconds_and_the_bucket_refills_across_one() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-api-clock-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let service = ApiService::new(ApiState::default()).with_shared_file_rate_limit(
        qx_storage::FileTokenBucket::new(&root, "clock-domain", 3, 3).expect("桶要能建"),
    );

    // 容量 3：同一格内三次放行，第四次见底。
    for command_id in 1..=3 {
        assert_eq!(
            service.handle("GET", "/health", "", BASE_MS).status,
            200,
            "第 {command_id} 次请求应在额度内"
        );
    }
    assert_eq!(
        service.handle("GET", "/health", "", BASE_MS).status,
        429,
        "容量用完后同一格内必须拒绝"
    );
    assert_eq!(
        service.handle("GET", "/health", "", BASE_MS + 5).status,
        429,
        "+5 毫秒还在同一格里：桶若按毫秒收戳，这一格会补充 5×3 颗令牌，把'每秒 3 次'读成没这回事"
    );
    assert_eq!(
        service.handle("GET", "/health", "", BASE_MS + 1_000).status,
        200,
        "跨过一格之后额度必须回来：桶按调用方给的秒补充，永远见底说明喂给它的钟定住了"
    );
    let _ = std::fs::remove_dir_all(root);
}
