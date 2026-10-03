//! `qx-adapter` 连接层的用例：HTTP/WS 传输、帧轮询与重连退避。
//!
//! 从 `lib.rs` 的行内 `mod tests` 拆出（V13 R2 第十四遍）：本轮给拼帧上限加判据，
//! 用例继续长在宿主文件里就会顶破行数棘轮，而拆成 `ccxt/` 目录模块会让门禁按路径
//! 取数的那条判据读不到文件。
//!
use super::*;
use std::io::Cursor;
use std::net::TcpListener;
use std::thread;

#[test]
fn http_transport_preserves_vendor_form_content_type() {
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".into(),
        "application/x-www-form-urlencoded".into(),
    );
    let wire = format_http_request(&HttpRequest {
        method: "POST".into(),
        host: "api.example.test".into(),
        port: 443,
        path: "/api/v3/order".into(),
        body: "symbol=BTCUSDT".into(),
        headers,
    });
    assert!(wire.contains("Content-Type: application/x-www-form-urlencoded\r\n"));
    assert!(!wire.contains("Content-Type: application/json\r\n"));
    assert_eq!(wire.matches("Content-Type:").count(), 1);
}

#[test]
fn tls_transport_rejects_invalid_server_name_before_connecting() {
    let error = TlsHttpTransport::default().send(HttpRequest {
        method: "GET".into(),
        host: "not a valid host name".into(),
        port: 443,
        path: "/health".into(),
        body: String::new(),
        headers: BTreeMap::new(),
    });
    assert!(error.unwrap_err().contains("TLS 主机名非法"));
}

#[test]
fn websocket_user_stream_validates_rfc_handshake_and_server_frames() {
    assert_eq!(
        websocket_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
    let response = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
    validate_websocket_handshake(response, "dGhlIHNhbXBsZSBub25jZQ==").unwrap();
    let mut frames = Cursor::new(vec![0x81, 0x02, b'o', b'k']);
    let frame = match poll_server_frame(&mut frames).unwrap() {
        FramePoll::Frame(frame) => frame,
        FramePoll::Idle => panic!("游标里就躺着一帧，不能判成空闲"),
    };
    assert_eq!(frame, (true, 0x1, b"ok".to_vec()));
}

/// 读窗到期的两种结局必须可区分：帧边界上的超时是"这一窗没数据"（链路还开着），
/// 半截帧上的超时是故障（跳过就会把一条流读到下一帧去）。
struct AlwaysExpired;

impl Read for AlwaysExpired {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let _ = buf;
        Err(std::io::Error::from(std::io::ErrorKind::TimedOut))
    }
}

struct PartialFrame {
    head: Option<[u8; 2]>,
}

impl Read for PartialFrame {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.head.take() {
            Some(head) => {
                let len = buf.len().min(head.len());
                buf[..len].copy_from_slice(&head[..len]);
                Ok(len)
            }
            None => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
        }
    }
}

#[test]
fn websocket_frame_poll_separates_idle_window_from_broken_link() {
    assert!(matches!(
        poll_server_frame(&mut AlwaysExpired),
        Ok(FramePoll::Idle)
    ));
    let error = poll_server_frame(&mut PartialFrame {
        head: Some([0x81, 0x02]),
    })
    .unwrap_err();
    assert!(
        !error.contains("没有帧"),
        "半截帧超时被降级成了空闲: {error}"
    );
}

/// 一条帧后接读窗到期：消息已经开了头，这一窗的静默必须是故障而不是空闲，
/// 否则下一条帧的字节会被当成这条的尾部（跨帧串流）。控制帧不算开了头。
#[test]
fn websocket_message_poll_keeps_mid_message_timeout_fatal() {
    // 0x01 = 非终止的文本分片，负载 2 字节；之后读窗到期。
    let mut fragmented = BufferThenExpired(vec![0x01, 0x02, b'a', b'b']);
    let error = poll_websocket_message(&mut fragmented).unwrap_err();
    assert!(
        error.contains("中途超时"),
        "半截消息后的静默被降级成空闲: {error}"
    );
    // 0x89 = 终止的 ping：回 pong 之后到期，帧边界仍在 → 空闲。
    let mut after_ping = BufferThenExpired(vec![0x89, 0x00]);
    assert!(matches!(
        poll_websocket_message(&mut after_ping),
        Ok(WebSocketPoll::Idle)
    ));
}

struct BufferThenExpired(Vec<u8>);

impl Read for BufferThenExpired {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.0.is_empty() {
            return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
        }
        let len = buf.len().min(self.0.len());
        buf[..len].copy_from_slice(&self.0[..len]);
        self.0.drain(..len);
        Ok(len)
    }
}

impl Write for BufferThenExpired {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 服务端下行帧不带掩码；长度按 RFC 6455 的三种编码铺开。
fn server_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![(if fin { 0x80 } else { 0 }) | opcode];
    let len = payload.len();
    if len <= 125 {
        out.push(len as u8);
    } else if len <= u16::MAX as usize {
        out.push(126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

/// #213：一条永不置 `fin` 的分片序列 + 持续的控制帧，把拼帧循环打成没有出口的循环。
/// 单帧长度闸门管不到它（每帧只有 1 字节），只有"单次 poll 的帧数上限"能出口。
#[test]
fn websocket_message_poll_caps_frames_consumed_per_read_window() {
    let mut bytes = server_frame(false, 0x1, b"a");
    for _ in 0..(MAX_WEBSOCKET_FRAMES_PER_POLL + 8) {
        bytes.extend_from_slice(&server_frame(false, 0x0, b"a"));
    }
    let error = poll_websocket_message(&mut BufferThenExpired(bytes)).unwrap_err();
    assert!(
        error.contains("帧数"),
        "帧数上限没咬住，循环靠读窗到期才脱身: {error}"
    );
}

/// #213：分片总长的闸门与帧数闸门是两条独立的出口 —— 这条夹具只有 3 帧，
/// 帧数上限咬不到，必须由拼接总长先拒。
#[test]
fn websocket_message_poll_caps_reassembled_message_size() {
    let chunk = vec![b'x'; 9 * 1024 * 1024];
    let mut bytes = server_frame(false, 0x1, b"a");
    bytes.extend_from_slice(&server_frame(false, 0x0, &chunk));
    bytes.extend_from_slice(&server_frame(false, 0x0, &chunk));
    let error = poll_websocket_message(&mut BufferThenExpired(bytes)).unwrap_err();
    assert!(
        error.contains("分片消息超过"),
        "拼接总长没有上限，16 MiB 帧闸门管得住单帧却管不住分片序列: {error}"
    );
}

/// 两条上限都不能把合法的分片消息一起拒了：3 段、每段远低于任何闸门。
#[test]
fn websocket_message_poll_still_reassembles_legal_fragmented_message() {
    let mut bytes = server_frame(false, 0x1, b"ab");
    bytes.extend_from_slice(&server_frame(false, 0x0, b"cd"));
    bytes.extend_from_slice(&server_frame(true, 0x0, b"ef"));
    match poll_websocket_message(&mut BufferThenExpired(bytes)).unwrap() {
        WebSocketPoll::Message(message) => {
            assert_eq!(message.opcode, 0x1);
            assert_eq!(message.payload, b"abcdef".to_vec());
        }
        other => panic!("分片消息没被拼回来: {other:?}"),
    }
}

#[test]
fn tcp_transport_executes_a_real_http_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .unwrap();
    });
    let response = TcpHttpTransport::default()
        .send(HttpRequest {
            method: "GET".into(),
            host: address.ip().to_string(),
            port: address.port(),
            path: "/health".into(),
            body: String::new(),
            headers: BTreeMap::new(),
        })
        .unwrap();
    worker.join().unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "ok");
}
