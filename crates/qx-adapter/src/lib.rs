//! 外部适配器：把 REST/HTTP 事实转换为 Provider/Venue 统一契约。
//!
//! 本模块只提供**共享的传输层**（HTTP/TLS/WebSocket 客户端、对账工具）与对外
//! 契约 `pub use`；供应商协议实现各自住在 `binance`/`ccxt` 子模块里。适配器不能
//! 直接改 Ledger。网络错误按“结果未知”处理；成功响应才生成 Accepted，成交只能
//! 由对应供应商的用户流回报进入事实事件（V12 §16：原先这里还有一套没有任何
//! 装配读者的通用 `RestVenue`/`RestProvider`/`RequestSigner` 脚手架，已随其
//! 平行去重、平行限流与平行 reconcile 纪律一并删除，避免第二条入账入口）。

use qx_core::{Order, OrderStatus};
use qx_zhenlu::VenueOrderSnapshot;
use ring::rand::{SecureRandom, SystemRandom};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::ServerName;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

mod binance;
mod ccxt;
mod io_budget;
mod reconcile;
mod venue_cache;
pub use binance::{
    run_binance_stream, run_binance_user_stream, run_binance_user_stream_with_config_loader,
    BinanceSpotAuth, BinanceSpotCredentials, BinanceSpotMarketData, BinanceSpotMarketStream,
    BinanceSpotUserStream, BinanceSpotVenue, BinanceStreamRead, BinanceStreamRetryPolicy,
    BinanceStreamRunReport, BinanceStreamSession, BinanceUserStreamRunConfig,
};
pub use ccxt::{CcxtProcessClient, CcxtProcessVenue, CcxtRpc};
pub use io_budget::write_all_within;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HttpRequest {
    pub method: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub body: String,
    pub headers: BTreeMap<String, String>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub trait HttpTransport: Send + Sync {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, String>;
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WebSocketMessage {
    pub opcode: u8,
    pub payload: Vec<u8>,
}

/// 一次 WebSocket 读的三态结果：拿到消息、这一轮连接上没数据、对端正常关闭。
///
/// `Idle` 与"失败"必须分开（V11 N10）：会话 socket 带读超时，超时只是"此刻没有帧"，
/// 连接依然可用。把它和真正的传输故障混成一类，一条长时间无人成交的薄行情就会
/// 杀掉订阅它的整个进程。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WebSocketRead {
    Message(WebSocketMessage),
    Idle,
    Closed,
}

/// 通用 WSS 用户流会话。供应商认证、订阅 payload 和业务事件映射由上层适配器提供。
pub struct TlsWebSocketUserStream {
    stream: StreamOwned<ClientConnection, TcpStream>,
    host: String,
}

impl TlsWebSocketUserStream {
    pub fn connect(
        host: impl Into<String>,
        port: u16,
        path: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, String> {
        Self::connect_with_config(host, port, path, timeout, Arc::new(default_client_config()))
    }

    pub fn connect_with_headers(
        host: impl Into<String>,
        port: u16,
        path: impl Into<String>,
        timeout: Duration,
        headers: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        Self::connect_with_config_and_headers(
            host,
            port,
            path,
            timeout,
            Arc::new(default_client_config()),
            headers,
        )
    }

    pub fn connect_with_config(
        host: impl Into<String>,
        port: u16,
        path: impl Into<String>,
        timeout: Duration,
        config: Arc<ClientConfig>,
    ) -> Result<Self, String> {
        Self::connect_with_config_and_headers(host, port, path, timeout, config, BTreeMap::new())
    }

    pub fn connect_with_config_and_headers(
        host: impl Into<String>,
        port: u16,
        path: impl Into<String>,
        timeout: Duration,
        config: Arc<ClientConfig>,
        headers: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let host = host.into();
        let path = path.into();
        if !path.starts_with('/') {
            return Err("WebSocket path 必须以 / 开头".into());
        }
        if headers.iter().any(|(name, value)| {
            name.is_empty()
                || name
                    .bytes()
                    .any(|byte| byte == b'\r' || byte == b'\n' || byte == b':')
                || value.bytes().any(|byte| byte == b'\r' || byte == b'\n')
        }) {
            return Err("WebSocket 握手头包含非法换行或分隔符".into());
        }
        let server_name =
            ServerName::try_from(host.clone()).map_err(|_| format!("TLS 主机名非法: {host}"))?;
        let address = resolve_socket_address(&host, port, timeout, "WSS")?;
        let stream =
            TcpStream::connect_timeout(&address, timeout).map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|error| error.to_string())?;
        let connection = ClientConnection::new(config, server_name.to_owned())
            .map_err(|error| format!("TLS WSS 连接初始化失败: {error}"))?;
        let mut session = Self {
            stream: StreamOwned::new(connection, stream),
            host,
        };
        let mut nonce = [0_u8; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "无法生成 WebSocket 握手随机数".to_string())?;
        let key = encode_base64(&nonce);
        let mut request = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n",
            session.host
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        session
            .stream
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        let response = read_header_block(&mut session.stream, timeout)?;
        validate_websocket_handshake(&response, &key)?;
        Ok(session)
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn send_text(&mut self, payload: &str) -> Result<(), String> {
        write_client_frame(&mut self.stream, 0x1, payload.as_bytes())
    }

    pub fn recv_message(&mut self) -> Result<WebSocketRead, String> {
        read_websocket_message(&mut self.stream)
    }

    pub fn close(&mut self) -> Result<(), String> {
        write_client_frame(&mut self.stream, 0x8, &[])
    }
}

fn default_client_config() -> ClientConfig {
    let root_store = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth()
}
#[derive(Clone, Debug)]
pub struct TcpHttpTransport {
    timeout: Duration,
}

impl Default for TcpHttpTransport {
    fn default() -> Self {
        Self::new(Duration::from_secs(10))
    }
}

impl TcpHttpTransport {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

/// 基于 rustls 的 TLS HTTP/1.1 传输。
///
/// 默认使用编译时 Mozilla 根证书和主机名校验；不提供跳过证书校验的开关，
/// 测试或私有 CA 应通过 `with_config` 显式提供验证配置。
#[derive(Clone)]
pub struct TlsHttpTransport {
    timeout: Duration,
    config: Arc<ClientConfig>,
}

impl Default for TlsHttpTransport {
    fn default() -> Self {
        Self::new(Duration::from_secs(10)).expect("默认 TLS 配置必须可构造")
    }
}

impl TlsHttpTransport {
    pub fn new(timeout: Duration) -> Result<Self, String> {
        let root_store = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Ok(Self::with_config(timeout, config))
    }

    pub fn with_config(timeout: Duration, config: ClientConfig) -> Self {
        Self {
            timeout,
            config: Arc::new(config),
        }
    }
}

impl HttpTransport for TlsHttpTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        let server_name = ServerName::try_from(request.host.clone())
            .map_err(|_| format!("TLS 主机名非法: {}", request.host))?;
        let address =
            resolve_socket_address(&request.host, request.port, self.timeout, "TLS HTTP")?;
        let stream = TcpStream::connect_timeout(&address, self.timeout)
            .map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|error| error.to_string())?;
        let connection = ClientConnection::new(Arc::clone(&self.config), server_name.to_owned())
            .map_err(|error| format!("TLS 连接初始化失败: {error}"))?;
        let mut stream = StreamOwned::new(connection, stream);
        let request_text = format_http_request(&request);
        stream
            .write_all(request_text.as_bytes())
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        stream
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        parse_http_response(&String::from_utf8_lossy(&bytes))
    }
}

fn format_http_request(request: &HttpRequest) -> String {
    let content_type = request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("Content-Type"))
        .map(|(_, value)| value.as_str())
        .unwrap_or("application/json");
    let custom_headers = request
        .headers
        .iter()
        .filter(|(key, _)| !key.eq_ignore_ascii_case("Content-Type"))
        .map(|(key, value)| format!("{key}: {value}\r\n"))
        .collect::<String>();
    format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{}",
        request.method,
        request.path,
        request.host,
        content_type,
        request.body.len(),
        custom_headers,
        request.body
    )
}

/// 把一段可能不返回的阻塞工作放到一条短命线程上，只等 `budget`。
///
/// 界到的是**调用方的等待**，不是那条线程本身：预算用尽后它仍在原地跑，跑完才退。
/// 这三处调用点每次构造一条、随解析结束而结束，不会攒出第二条（V11 O9）。
fn within_budget<T, F>(thread_name: &str, budget: Duration, work: F) -> Result<T, RecvTimeoutError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            let _ = sender.send(work());
        })
        .map_err(|_| RecvTimeoutError::Disconnected)?;
    receiver.recv_timeout(budget)
}

/// 带预算的地址解析。`to_socket_addrs` 走系统解析器，代码侧原本一个界都没有：
/// 一台黑洞掉的 DNS 服务器（或一份配错的 resolv.conf）能让它在几十秒到几分钟里不返回，
/// 而这段时间调用方既读不到停机令牌也回不到循环头——`connect_timeout` 只界住解析
/// **之后**那一跳（V11 O9）。
fn resolve_socket_address(
    host: &str,
    port: u16,
    budget: Duration,
    label: &str,
) -> Result<SocketAddr, String> {
    let host = host.to_string();
    match within_budget(&format!("qianxing-dns-{label}"), budget, move || {
        (host.as_str(), port)
            .to_socket_addrs()
            .map(|mut addresses| addresses.next())
            .map_err(|error| error.to_string())
    }) {
        Ok(Ok(Some(address))) => Ok(address),
        Ok(Ok(None)) => Err(format!("{label} 地址解析不到任何 socket 地址")),
        Ok(Err(error)) => Err(format!("{label} 地址解析失败: {error}")),
        Err(RecvTimeoutError::Timeout) => Err(format!("{label} 地址解析超过 {budget:?} 预算")),
        Err(RecvTimeoutError::Disconnected) => Err(format!("{label} 地址解析线程异常退出")),
    }
}

fn read_header_block<R: Read>(reader: &mut R, budget: Duration) -> Result<String, String> {
    // 逐字节读意味着"单次读有界"不等于"整块有界"：socket 的 10 秒读超时只管一次
    // `read_exact`，一个每 9 秒吐一个字节的对端能把一次握手占住 65536 × 10 秒，而 64 KiB
    // 那道上限只在累计之后才问。这里在每一字节的读之前问一次墙钟，整块的成本因此收在
    // `budget` 之内（最坏再加一次已经被 socket 超时界住的读）（V11 O7）。
    let deadline = Instant::now() + budget;
    let mut bytes = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(format!(
                "WebSocket 握手响应超过整体截止 {budget:?}，已读 {} 字节",
                bytes.len()
            ));
        }
        let mut byte = [0_u8; 1];
        reader
            .read_exact(&mut byte)
            .map_err(|error| error.to_string())?;
        bytes.push(byte[0]);
        if bytes.len() > 64 * 1024 {
            return Err("WebSocket 握手响应过大".into());
        }
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8(bytes).map_err(|error| error.to_string());
        }
    }
}

fn validate_websocket_handshake(response: &str, key: &str) -> Result<(), String> {
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| "WebSocket 握手缺少 status".to_string())?;
    if status != "101" {
        return Err(format!("WebSocket 握手 status 非法: {status}"));
    }
    let has_upgrade = response.lines().any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        name.eq_ignore_ascii_case("Upgrade") && value.trim().eq_ignore_ascii_case("websocket")
    });
    let has_connection_upgrade = response.lines().any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        name.eq_ignore_ascii_case("Connection")
            && value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
    });
    if !has_upgrade || !has_connection_upgrade {
        return Err("WebSocket 握手缺少 Upgrade/Connection 响应头".into());
    }
    let accept = response.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("Sec-WebSocket-Accept")
            .then_some(value.trim())
    });
    if accept != Some(websocket_accept_key(key).as_str()) {
        return Err("WebSocket 握手 accept 校验失败".into());
    }
    Ok(())
}

fn websocket_accept_key(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    encode_base64(&sha1_digest(&input))
}

fn write_client_frame<W: Write>(writer: &mut W, opcode: u8, payload: &[u8]) -> Result<(), String> {
    if opcode >= 0x8 && payload.len() > 125 {
        return Err("WebSocket 控制帧超过 125 字节".into());
    }
    let mut frame = vec![0x80 | (opcode & 0x0F)];
    match payload.len() {
        0..=125 => frame.push(0x80 | payload.len() as u8),
        126..=65_535 => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        _ => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
    }
    let mut mask = [0_u8; 4];
    SystemRandom::new()
        .fill(&mut mask)
        .map_err(|_| "无法生成 WebSocket 帧掩码".to_string())?;
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    writer.write_all(&frame).map_err(|error| error.to_string())
}

/// 一条 WebSocket 消息的长度预算：单帧与分片累计共用同一个数，分片不额外放宽。
const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// 这次读失败是否只是"此刻没有数据"。
///
/// 两种错误码都得认：`set_read_timeout` 到期时 std 给 `TimedOut`，而 rustls 会把底层
/// 的 would-block 直接透传成 `WouldBlock`。它们都代表连接仍然完好，只是静默。
fn is_read_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// 读出下一帧的头部与载荷；`Ok(None)` 表示**一个字节都没消耗**就超时。
///
/// 静默判定只能落在帧边界上：读走半个头部再按 idle 交还，下一次调用会把剩下的字节
/// 当成新帧的起点，整条读侧从此错位下去——那种情况必须失败，让上层重连。
fn read_server_frame_or_idle<R: Read>(
    reader: &mut R,
) -> Result<Option<(bool, u8, Vec<u8>)>, String> {
    let mut head = [0_u8; 2];
    let mut filled = 0_usize;
    while filled < head.len() {
        match reader.read(&mut head[filled..]) {
            Ok(0) => return Err("WebSocket 连接已被对端关闭".into()),
            Ok(read) => filled += read,
            Err(error) if filled == 0 && is_read_timeout(&error) => return Ok(None),
            Err(error) => return Err(format!("WebSocket 帧头读取失败: {error}")),
        }
    }
    let fin = head[0] & 0x80 != 0;
    let opcode = head[0] & 0x0F;
    let masked = head[1] & 0x80 != 0;
    if masked {
        return Err("服务端 WebSocket 帧不应带掩码".into());
    }
    let mut length = u64::from(head[1] & 0x7F);
    if length == 126 {
        let mut extended = [0_u8; 2];
        reader
            .read_exact(&mut extended)
            .map_err(|error| format!("WebSocket 扩展长度读取失败: {error}"))?;
        length = u64::from(u16::from_be_bytes(extended));
    } else if length == 127 {
        let mut extended = [0_u8; 8];
        reader
            .read_exact(&mut extended)
            .map_err(|error| format!("WebSocket 扩展长度读取失败: {error}"))?;
        length = u64::from_be_bytes(extended);
    }
    if length > MAX_WEBSOCKET_MESSAGE_BYTES as u64 {
        return Err(format!(
            "WebSocket 帧超过 {} MiB 限制",
            MAX_WEBSOCKET_MESSAGE_BYTES / (1024 * 1024)
        ));
    }
    if opcode >= 0x8 && (!fin || length > 125) {
        return Err("WebSocket 控制帧格式非法".into());
    }
    let mut payload = vec![0_u8; length as usize];
    reader
        .read_exact(&mut payload)
        .map_err(|error| format!("WebSocket 帧载荷读取失败: {error}"))?;
    Ok(Some((fin, opcode, payload)))
}

/// 读出一条完整的 WebSocket 消息：答 ping、跳 pong、把分片拼回一条。
///
/// 长度预算按**整条消息**算，不只是按帧（V11 N11）：只卡单帧的话，对端只要把一条
/// 超大消息切成一串各自合法的片段，就能让这里的缓冲无界长大，而这条链路是行情
/// 用户流的共用读侧——它先 OOM，同一颗进程里的所有订阅就一起没了。
fn read_websocket_message<R: Read + Write>(stream: &mut R) -> Result<WebSocketRead, String> {
    let mut message = None;
    loop {
        let Some((fin, opcode, payload)) = read_server_frame_or_idle(stream)? else {
            // 攒着半条消息时静默不能当好消息交出去：续帧随后就到，届时 `0x0` 会被
            // 当成非法首帧。宁可让上层重连一次，也不能把已收下的片段装作没发生。
            if message.is_some() {
                return Err("WebSocket 分片之间读取超时，半条消息不可恢复".into());
            }
            return Ok(WebSocketRead::Idle);
        };
        match opcode {
            0x8 => return Ok(WebSocketRead::Closed),
            0x9 => {
                write_client_frame(stream, 0xA, &payload)?;
                continue;
            }
            0xA => continue,
            0x1 | 0x2 if message.is_none() => {
                let current = WebSocketMessage { opcode, payload };
                if fin {
                    return Ok(WebSocketRead::Message(current));
                }
                message = Some(current);
            }
            0x0 if message.is_some() => {
                let current = message.as_mut().expect("message checked");
                if current.payload.len() + payload.len() > MAX_WEBSOCKET_MESSAGE_BYTES {
                    return Err(format!(
                        "WebSocket 分片累计长度超过 {} MiB 限制",
                        MAX_WEBSOCKET_MESSAGE_BYTES / (1024 * 1024)
                    ));
                }
                current.payload.extend_from_slice(&payload);
                if fin {
                    return Ok(WebSocketRead::Message(message.expect("message checked")));
                }
            }
            _ => return Err("WebSocket 分片 opcode 或控制帧非法".into()),
        }
    }
}

fn sha1_digest(input: &[u8]) -> [u8; 20] {
    let mut h = [
        0x67452301_u32,
        0xEFCDAB89,
        0x98BADCFE,
        0x10325476,
        0xC3D2E1F0,
    ];
    let bit_len = (input.len() as u64) * 8;
    let mut data = input.to_vec();
    data.push(0x80);
    while !(data.len() + 8).is_multiple_of(64) {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in data.chunks(64) {
        let mut words = [0_u32; 80];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (index, word) in words.iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut result = [0_u8; 20];
    for (index, word) in h.iter().enumerate() {
        result[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    result
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (a << 16) | (b << 8) | c;
        output.push(TABLE[((triple >> 18) & 63) as usize] as char);
        output.push(TABLE[((triple >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(triple & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

impl HttpTransport for TcpHttpTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        let address = resolve_socket_address(&request.host, request.port, self.timeout, "HTTP")?;
        let mut stream = TcpStream::connect_timeout(&address, self.timeout)
            .map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|error| error.to_string())?;
        let request_text = format_http_request(&request);
        stream
            .write_all(request_text.as_bytes())
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        stream
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        parse_http_response(&String::from_utf8_lossy(&bytes))
    }
}

fn parse_http_response(response: &str) -> Result<HttpResponse, String> {
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "响应缺少 header 分隔符".to_string())?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| "响应缺少 HTTP status".to_string())?
        .parse()
        .map_err(|error| format!("HTTP status 非法: {error}"))?;
    Ok(HttpResponse {
        status,
        body: body.into(),
    })
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AdapterReconcileIssue {
    MissingLocally {
        client_order_id: u64,
    },
    MissingAtVenue {
        client_order_id: u64,
    },
    StatusMismatch {
        client_order_id: u64,
        local: OrderStatus,
        venue: OrderStatus,
    },
    FilledMismatch {
        client_order_id: u64,
        local: qx_core::Quantity,
        venue: qx_core::Quantity,
    },
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            read_server_frame_or_idle(&mut frames).unwrap(),
            Some((true, 0x1, b"ok".to_vec()))
        );
    }

    /// 按脚本交付字节的假 socket：`Ok(bytes)` 表示这次读给出这些字节（空 = EOF），
    /// `Err(kind)` 表示这次读以该错误码失败。脚本走完即失败，避免用例悄悄多读。
    struct ScriptedSocket {
        steps: Vec<Result<Vec<u8>, std::io::ErrorKind>>,
        index: usize,
        written: Vec<u8>,
    }

    impl ScriptedSocket {
        fn new(steps: Vec<Result<Vec<u8>, std::io::ErrorKind>>) -> Self {
            Self {
                steps,
                index: 0,
                written: Vec::new(),
            }
        }
    }

    impl Read for ScriptedSocket {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let step = self
                .steps
                .get(self.index)
                .ok_or_else(|| std::io::Error::other("脚本已用尽"))?;
            self.index += 1;
            match step {
                Ok(bytes) => {
                    let length = bytes.len().min(buffer.len());
                    buffer[..length].copy_from_slice(&bytes[..length]);
                    Ok(length)
                }
                Err(kind) => Err(std::io::Error::from(*kind)),
            }
        }
    }

    impl Write for ScriptedSocket {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// V11 N10：读超时是"这一轮没数据"，不是故障。把它当故障会让一条无人成交的
    /// 薄行情杀掉订阅进程，而这里返回 idle 的前提是读位置还停在帧边界上。
    #[test]
    fn websocket_silence_at_a_frame_boundary_keeps_the_connection_usable() {
        let mut socket = ScriptedSocket::new(vec![
            Err(std::io::ErrorKind::WouldBlock),
            Ok(vec![0x81, 0x02]),
            Ok(b"ok".to_vec()),
            Err(std::io::ErrorKind::TimedOut),
            Ok(server_frame(true, 0x8, &[])),
        ]);
        assert_eq!(
            read_websocket_message(&mut socket).unwrap(),
            WebSocketRead::Idle
        );
        // 同一颗 socket 接着给出真帧：idle 没有把读位置弄乱，两种超时错误码也都认。
        let message = read_websocket_message(&mut socket).unwrap();
        assert_eq!(
            message,
            WebSocketRead::Message(WebSocketMessage {
                opcode: 0x1,
                payload: b"ok".to_vec()
            })
        );
        assert_eq!(
            read_websocket_message(&mut socket).unwrap(),
            WebSocketRead::Idle
        );
        assert_eq!(
            read_websocket_message(&mut socket).unwrap(),
            WebSocketRead::Closed
        );
    }

    /// 反面：消耗了半个头部之后的超时不能报静默，否则下一次调用会把剩下的字节当成
    /// 新帧起点；EOF 与真正的传输故障同样必须失败。
    #[test]
    fn websocket_timeout_after_the_first_header_byte_fails_instead_of_claiming_silence() {
        let mut half_header =
            ScriptedSocket::new(vec![Ok(vec![0x81]), Err(std::io::ErrorKind::WouldBlock)]);
        let error = read_websocket_message(&mut half_header).unwrap_err();
        assert!(error.contains("帧头"), "半截头部要说清是帧头: {error}");

        let mut reset = ScriptedSocket::new(vec![Err(std::io::ErrorKind::ConnectionReset)]);
        let error = read_websocket_message(&mut reset).unwrap_err();
        assert!(error.contains("帧头"), "非超时故障不该被当成静默: {error}");

        let mut eof = ScriptedSocket::new(vec![Ok(Vec::new())]);
        let error = read_websocket_message(&mut eof).unwrap_err();
        assert!(
            error.contains("对端关闭"),
            "EOF 必须失败而不是无限空转: {error}"
        );
    }

    /// 半条分片消息上的超时同样不是静默：续帧随后就到，届时 `0x0` 会被当成非法首帧。
    #[test]
    fn websocket_timeout_between_fragments_discards_the_partial_message() {
        let mut socket = ScriptedSocket::new(vec![
            Ok(vec![0x01, 0x01]),
            Ok(b"a".to_vec()),
            Err(std::io::ErrorKind::WouldBlock),
        ]);
        let error = read_websocket_message(&mut socket).unwrap_err();
        assert!(
            error.contains("半条消息"),
            "分片间超时要说清丢了一半: {error}"
        );
    }

    /// 按 RFC 6455 编一个服务端帧（不带掩码），长度取 7/16/64 位三档里最短的那档。
    fn server_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![if fin { 0x80 } else { 0 } | opcode];
        let length = payload.len();
        if length < 126 {
            frame.push(length as u8);
        } else if length <= u16::MAX as usize {
            frame.push(126);
            frame.extend_from_slice(&(length as u16).to_be_bytes());
        } else {
            frame.push(127);
            frame.extend_from_slice(&(length as u64).to_be_bytes());
        }
        frame.extend_from_slice(payload);
        frame
    }

    /// V11 N11：单帧上限管不住分片，预算必须按整条消息算。
    #[test]
    fn websocket_fragment_accumulation_is_bounded_by_the_message_budget() {
        // 首片留两片字节的余量，好让"正好到预算"和"越一格"两条用例共用同一份前缀。
        let mut prefix = Vec::new();
        prefix.extend_from_slice(&server_frame(
            false,
            0x1,
            &vec![b'a'; MAX_WEBSOCKET_MESSAGE_BYTES - 2],
        ));
        prefix.extend_from_slice(&server_frame(false, 0x0, b"b"));

        // 正向对照：整条消息恰好等于预算必须放行。只测"越界会报"的话，把预算
        // 改成任意小的数也能让那条断言绿——这里要的是"限制正确"而非"限制存在"。
        let mut legal = prefix.clone();
        legal.extend_from_slice(&server_frame(true, 0x0, b"c"));
        let reassembled = match read_websocket_message(&mut Cursor::new(legal)).unwrap() {
            WebSocketRead::Message(message) => message,
            other => panic!("合法的分片消息该拼成一条，实际 {other:?}"),
        };
        assert_eq!(reassembled.opcode, 0x1);
        assert_eq!(reassembled.payload.len(), MAX_WEBSOCKET_MESSAGE_BYTES);
        assert_eq!(reassembled.payload[0], b'a');
        assert_eq!(*reassembled.payload.last().unwrap(), b'c');

        // 反向：每一片单独都合法，只有整条消息的预算拦得住。
        prefix.extend_from_slice(&server_frame(false, 0x0, b"c"));
        prefix.extend_from_slice(&server_frame(true, 0x0, b"d"));
        let error = match read_websocket_message(&mut Cursor::new(prefix)) {
            Err(error) => error,
            Ok(WebSocketRead::Message(message)) => panic!(
                "越出一格预算的分片消息不该被收下: 长度 {:?}",
                message.payload.len()
            ),
            Ok(read) => panic!("越出一格预算的分片消息不该被收下: {read:?}"),
        };
        assert!(error.contains("累计"), "越界要说清是分片累计: {error}");
    }

    /// 单帧那一格也要有用例：它管的是"对端一开口就声明一条超限消息"，这时一个字节
    /// 的载荷都不该去读，更不该为此先分配缓冲。
    #[test]
    fn websocket_frame_announcing_more_than_the_budget_fails_before_reading_payload() {
        let mut frame = vec![0x82, 0x7F];
        frame.extend_from_slice(&((MAX_WEBSOCKET_MESSAGE_BYTES as u64) + 1).to_be_bytes());
        let error = read_websocket_message(&mut Cursor::new(frame))
            .expect_err("超限帧不该被收下，也不该先去读那 16 MiB + 1");
        assert!(
            error.contains("MiB 限制"),
            "要说是预算挡下的，不是读取失败: {error}"
        );
    }

    /// 逐字节读的握手块要有一个**整体**截止：socket 的读超时只界住单次 `read_exact`，
    /// 一个每 9 秒吐一个字节的对端过去能把一次握手占住 65536 × 超时，而 64 KiB 那道上限
    /// 只在累计之后才问（V11 O7）。
    #[test]
    fn websocket_handshake_header_block_stops_at_its_own_deadline() {
        struct Dribble;
        impl Read for Dribble {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                buf[0] = b'A';
                Ok(1)
            }
        }
        let error = read_header_block(&mut Dribble, Duration::ZERO)
            .expect_err("永不结束的响应块必须在整体截止处收掉");
        assert!(
            error.contains("整体截止"),
            "要说是截止挡下的，不是读取失败: {error}"
        );
        // 正向对照：合法而完整的响应块，只要在截止之内就必须照常读完。
        let block = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n".to_vec();
        let read = read_header_block(&mut Cursor::new(block), Duration::from_secs(10))
            .expect("截止之内的完整响应块不该被拒");
        assert!(read.contains("101"), "读回来的应是整块响应: {read}");
    }

    /// 地址解析的预算管的是"调用方最多等多久"：一条不返回的解析过去能把整个 worker 带走
    /// ——它既读不到停机令牌也回不到循环头（V11 O9）。
    #[test]
    fn address_resolution_returns_to_its_caller_at_the_budget() {
        let started = Instant::now();
        let error = within_budget("qianxing-test-slow", Duration::from_millis(20), || {
            std::thread::sleep(Duration::from_millis(400));
            "never arrives in time"
        })
        .expect_err("超过预算的阻塞工作必须把等待交还给调用方");
        assert!(
            matches!(error, RecvTimeoutError::Timeout),
            "要说是预算到点，不是线程没了: {error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "预算 20ms 却等了 {:?}，等于没界",
            started.elapsed()
        );
        // 正向对照：预算之内的解析照常把地址交回来。
        let address = resolve_socket_address("127.0.0.1", 9, Duration::from_secs(5), "test")
            .expect("字面量 IP 的解析不该失败");
        assert_eq!(address.ip().to_string(), "127.0.0.1");
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
}
