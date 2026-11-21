//! 外部适配器：把 REST/HTTP 事实转换为 Provider/Venue 统一契约。本模块只提供**共享传输层**
//! （HTTP/TLS/WebSocket 客户端、对账工具）与对外契约 `pub use`，供应商协议各自住在 `binance`/
//! `ccxt` 子模块。适配器不能直接改 Ledger：网络错误按“结果未知”处理，成交只能由用户流回报进入事实事件。
//! HTTP 请求使用 `Connection: close`，读取受大小与时间上限约束；支持 Content-Length、单独的
//! chunked 和连接关闭定界。未知 transfer coding、帧头冲突、截断及非 UTF-8 正文会返回错误。
//! 关闭定界没有长度信息时，无法仅凭连接关闭区分完整响应与对端提前中断。

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use qx_core::{Order, OrderStatus};
use qx_zhenlu::VenueOrderSnapshot;
use ring::rand::{SecureRandom, SystemRandom};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, ServerName};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod binance;
mod ccxt;
mod ccxt_provider;
mod io_budget;
mod order_lifecycle;
mod reconcile;
mod venue_cache;
pub use binance::{
    run_binance_user_stream, run_binance_user_stream_observing,
    run_binance_user_stream_with_config_loader_observing, BinanceQuotePoll, BinanceSpotAuth,
    BinanceSpotCredentials, BinanceSpotMarketData, BinanceSpotMarketStream, BinanceSpotUserStream,
    BinanceSpotVenue, BinanceStreamPoll, BinanceStreamRetryPolicy, BinanceStreamRunReport,
    BinanceUserStreamRunConfig, BinanceUserStreamSession,
};
pub use ccxt::{
    ccxt_idle_window_ms, CcxtBarDataProvider, CcxtCallOutcome, CcxtProcessClient, CcxtProcessVenue,
    CcxtRpc,
};
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
/// 一次 `recv_message` 窗口的结局。`Idle` 与 `Closed` 分开是必需的：把"这一窗没有帧"
/// 当成链路故障，会让一个连着但长时间没有事件的账户按重连预算被判死。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WebSocketPoll {
    /// 拿到一条数据帧。
    Message(WebSocketMessage),
    /// 读窗口到期，链路仍在原样等待。
    Idle,
    /// 对端发来关闭帧。
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
        let address = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|error| error.to_string())?
            .next()
            .ok_or_else(|| "无法解析 WSS 地址".to_string())?;
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

    /// 读一帧。三种结局必须分开：拿到帧、这一窗没帧但链路仍在（`Idle`）、对端正常关闭。
    /// 旧口径把超时算成 `Err`，于是空闲会话会去消费重连预算。
    pub fn recv_message(&mut self) -> Result<WebSocketPoll, String> {
        poll_websocket_message(&mut self.stream)
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

/// 基于 rustls 的 TLS HTTP/1.1 传输：默认用编译时 Mozilla 根证书并校验主机名，
/// 不提供跳过证书校验的开关；测试或私有 CA 走 `with_config` 显式提供验证配置。
#[derive(Clone)]
pub struct TlsHttpTransport {
    timeout: Duration,
    config: Arc<ClientConfig>,
}

impl Default for TlsHttpTransport {
    fn default() -> Self {
        Self::with_config(Duration::from_secs(10), default_client_config())
    }
}

impl TlsHttpTransport {
    pub fn new(timeout: Duration) -> Result<Self, String> {
        Ok(Self::with_config(timeout, default_client_config()))
    }

    pub fn with_config(timeout: Duration, config: ClientConfig) -> Self {
        Self {
            timeout,
            config: Arc::new(config),
        }
    }

    /// Load a verifying mTLS client from PEM files. The CA file is the only server trust root;
    /// client identity is always presented and certificate/hostname verification stays enabled.
    pub fn from_mtls_pem(
        timeout: Duration,
        server_ca_path: impl AsRef<std::path::Path>,
        client_certificate_chain_path: impl AsRef<std::path::Path>,
        client_private_key_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, String> {
        let server_roots = CertificateDer::pem_file_iter(server_ca_path.as_ref())
            .map_err(|error| format!("读取 mTLS 服务端 CA 失败: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("解析 mTLS 服务端 CA 失败: {error}"))?;
        if server_roots.is_empty() {
            return Err("mTLS 服务端 CA 不能为空".into());
        }
        let mut root_store = RootCertStore::empty();
        for certificate in server_roots {
            root_store
                .add(certificate)
                .map_err(|error| format!("加入 mTLS 服务端 CA 失败: {error}"))?;
        }

        let client_certificate_chain =
            CertificateDer::pem_file_iter(client_certificate_chain_path.as_ref())
                .map_err(|error| format!("读取 mTLS 客户端证书链失败: {error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("解析 mTLS 客户端证书链失败: {error}"))?;
        if client_certificate_chain.is_empty() {
            return Err("mTLS 客户端证书链不能为空".into());
        }
        let client_private_key = PrivateKeyDer::from_pem_file(client_private_key_path.as_ref())
            .map_err(|error| format!("解析 mTLS 客户端私钥失败: {error}"))?;
        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_client_auth_cert(client_certificate_chain, client_private_key)
            .map_err(|error| format!("构造 mTLS HTTP 客户端失败: {error}"))?;
        Ok(Self::with_config(timeout, config))
    }
}

impl HttpTransport for TlsHttpTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        let server_name = ServerName::try_from(request.host.clone())
            .map_err(|_| format!("TLS 主机名非法: {}", request.host))?;
        let address = (request.host.as_str(), request.port)
            .to_socket_addrs()
            .map_err(|error| error.to_string())?
            .next()
            .ok_or_else(|| "无法解析 TLS HTTP 地址".to_string())?;
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
        let bytes = read_capped_response(
            &mut stream,
            MAX_HTTP_RESPONSE_BYTES,
            self.timeout,
            &request.method,
        )?;
        parse_http_response(&bytes, &request.method)
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

fn read_header_block<R: Read>(reader: &mut R, budget: Duration) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    let started = Instant::now();
    loop {
        if started.elapsed() >= budget {
            return Err("WebSocket 握手响应整体超时".into());
        }
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

/// 一次读帧窗口的结局：等到帧，或在**帧边界上**超时（链路仍然可用）。
#[derive(Debug)]
enum FramePoll {
    Frame((bool, u8, Vec<u8>)),
    Idle,
}

/// 单条服务端帧的长度上限，与一条拼好消息的总长上限：帧长各自有闸门，但分片可以一直续，
/// 只卡帧长的话一条永不置 `fin` 的分片序列就能把拼帧缓冲吃到耗尽。
const MAX_WEBSOCKET_FRAME_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// 一次 `poll_websocket_message` 里最多消费多少条帧：调用方的停机令牌只在 poll 返回之后
/// 才读得到（`binance.rs` 的 `while !should_stop()`），对端持续塞 ping 或续帧时这条是唯一的出口。
const MAX_WEBSOCKET_FRAMES_PER_POLL: usize = 64;

/// 套接字读超时不是断链：`set_read_timeout` 之后 `read_exact` 以 `WouldBlock`/`TimedOut` 返回，把它当成会话故障会让"连着但没数据"的账户在几个窗口内被判死。
fn is_read_window_expired(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// 从字节流里拼出一条完整消息，并把"读窗到期"的两种位置分开：还没有半截消息时是空闲（链路仍在），已经开始
/// 收消息时是故障（跨帧串流会把下一条帧读成这条的尾部）。独立成泛型函数是因为 `TlsWebSocketUserStream` 的流字段
/// 是具体 TLS 类型，挂在方法上的这条分支构造不出来——变异把中途超时降级成空闲时全树仍绿。两个上限是唯一退出保证（V13 R2 #213）。
fn poll_websocket_message<R: Read + Write>(reader: &mut R) -> Result<WebSocketPoll, String> {
    let mut message = None;
    let mut consumed_frames = 0_usize;
    loop {
        let (fin, opcode, payload) = match poll_server_frame(reader)? {
            FramePoll::Frame(frame) => frame,
            FramePoll::Idle if message.is_none() => return Ok(WebSocketPoll::Idle),
            // 半截帧之后才超时：帧边界已经破了，不能报空闲。
            FramePoll::Idle => return Err("WebSocket 分帧读取中途超时".into()),
        };
        consumed_frames = consumed_frames.saturating_add(1);
        if consumed_frames > MAX_WEBSOCKET_FRAMES_PER_POLL {
            return Err(format!(
                "WebSocket 单次读取窗口内帧数超过 {MAX_WEBSOCKET_FRAMES_PER_POLL} 上限"
            ));
        }
        match opcode {
            0x8 => return Ok(WebSocketPoll::Closed),
            0x9 => {
                write_client_frame(reader, 0xA, &payload)?;
                continue;
            }
            0xA => continue,
            0x1 | 0x2 if message.is_none() => {
                message = Some(WebSocketMessage { opcode, payload });
                if fin {
                    return take_websocket_message(&mut message);
                }
            }
            0x0 if message.is_some() => {
                let current = message
                    .as_mut()
                    .ok_or_else(|| "WebSocket 延续帧缺少消息状态".to_string())?;
                if current.payload.len().saturating_add(payload.len()) > MAX_WEBSOCKET_MESSAGE_BYTES
                {
                    return Err(format!(
                        "WebSocket 分片消息超过 {} MiB 上限",
                        MAX_WEBSOCKET_MESSAGE_BYTES / (1024 * 1024)
                    ));
                }
                current.payload.extend_from_slice(&payload);
                if fin {
                    return take_websocket_message(&mut message);
                }
            }
            _ => return Err("WebSocket 分片 opcode 或控制帧非法".into()),
        }
    }
}

fn take_websocket_message(message: &mut Option<WebSocketMessage>) -> Result<WebSocketPoll, String> {
    Ok(WebSocketPoll::Message(
        message.take().ok_or_else(|| "消息状态缺失".to_string())?,
    ))
}

fn poll_server_frame<R: Read>(reader: &mut R) -> Result<FramePoll, String> {
    let mut head = [0_u8; 2];
    // 只在帧头之前判定空闲：半截帧超时仍然算故障，否则会跨帧串流。
    match reader.read_exact(&mut head) {
        Err(error) if is_read_window_expired(&error) => return Ok(FramePoll::Idle),
        result => result.map_err(|error| error.to_string())?,
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
            .map_err(|error| error.to_string())?;
        length = u64::from(u16::from_be_bytes(extended));
    } else if length == 127 {
        let mut extended = [0_u8; 8];
        reader
            .read_exact(&mut extended)
            .map_err(|error| error.to_string())?;
        length = u64::from_be_bytes(extended);
    }
    if length > MAX_WEBSOCKET_FRAME_BYTES {
        return Err(format!(
            "WebSocket 帧超过 {} MiB 限制",
            MAX_WEBSOCKET_FRAME_BYTES / (1024 * 1024)
        ));
    }
    if opcode >= 0x8 && (!fin || length > 125) {
        return Err("WebSocket 控制帧格式非法".into());
    }
    let mut payload = vec![0_u8; length as usize];
    reader
        .read_exact(&mut payload)
        .map_err(|error| error.to_string())?;
    Ok(FramePoll::Frame((fin, opcode, payload)))
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
        let address = (request.host.as_str(), request.port)
            .to_socket_addrs()
            .map_err(|error| error.to_string())?
            .next()
            .ok_or_else(|| "无法解析 HTTP 地址".to_string())?;
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
        let bytes = read_capped_response(
            &mut stream,
            MAX_HTTP_RESPONSE_BYTES,
            self.timeout,
            &request.method,
        )?;
        parse_http_response(&bytes, &request.method)
    }
}

/// 一次 HTTP 响应的两道闸：`MAX_HTTP_RESPONSE_BYTES` 挡内存、`budget` 挡时间。`set_read_timeout`
/// 每读到一块就复位，滴水的上游能借它把连接无限续期，字节界单独存在时这条读链没有出口（V13 R17-c）。
const MAX_HTTP_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// 两臂都提前中止：越界与超时都不等于无界增长。上限与截止做成参数，只为让判据能在内存 `Cursor` 上测到。
fn read_capped_response<R: Read>(
    reader: &mut R,
    limit: usize,
    budget: Duration,
    method: &str,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    let started = Instant::now();
    let mut frame = None;
    let mut chunks = ChunkScanner::default();
    let mut header_scan_start = 0;
    loop {
        if started.elapsed() >= budget {
            return Err("HTTP 响应整体截止已到，连接已中止".into());
        }
        let size = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if size == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..size]);
        if bytes.len() > limit {
            return Err(format!("HTTP 响应超过 {limit} 字节上限，连接已中止"));
        }
        if frame.is_none() {
            if bytes[header_scan_start..]
                .windows(4)
                .any(|window| window == b"\r\n\r\n")
            {
                frame = parse_http_head(&bytes, method)?;
            } else {
                if bytes.len() > 64 * 1024 + 3 {
                    return Err("HTTP 响应头超过 64 KiB".into());
                }
                header_scan_start = bytes.len().saturating_sub(3);
            }
        }
        if let Some((start, _, framing)) = frame {
            let body = &bytes[start..];
            let complete = match framing {
                HttpFraming::NoBody => true,
                HttpFraming::Length(length) => body.len() >= length,
                HttpFraming::Chunked => chunks.advance(body)?,
                HttpFraming::Close => false,
            };
            if complete {
                break;
            }
        }
    }
    Ok(bytes)
}

/// CCXT worker 单行的字节上限：泵线程每次只从子进程 stdout 取**一行**，而 `read_line` 会一直往
/// 同一个 `String` 里长，子进程少写一个换行符就能把本进程吃到内存耗尽。取与响应界同一格数量级（V13 R5）。
const MAX_WORKER_LINE_BYTES: usize = MAX_HTTP_RESPONSE_BYTES;

/// 读到换行为止，但把这一行卡在 `limit`：`take` 多放行一字节，读到第 `limit + 1` 字节即越界。
/// `Ok(None)` 是管道 EOF（子进程已退出），`Err` 是这一行长到必须中止整条泵——两者都得让调用侧
/// 看见，静默丢一行等于把"应答没读到"说成"worker 什么都没发生"。上限做成参数只为能在 `Cursor` 上测。
fn read_capped_line<R: Read>(
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
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| format!("worker 输出不是 UTF-8: {error}"))
}

#[derive(Clone, Copy)]
enum HttpFraming {
    NoBody,
    Length(usize),
    Chunked,
    Close,
}

fn parse_http_head(
    response: &[u8],
    method: &str,
) -> Result<Option<(usize, u16, HttpFraming)>, String> {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        // Bound repeated header scans even when the peer trickles bytes.
        if response.len() > 64 * 1024 {
            return Err("HTTP 响应头超过 64 KiB".into());
        }
        return Ok(None);
    };
    if header_end > 64 * 1024 {
        return Err("HTTP 响应头超过 64 KiB".into());
    }
    let headers = std::str::from_utf8(&response[..header_end])
        .map_err(|_| "HTTP 响应头不是 UTF-8".to_string())?;
    let mut lines = headers.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| "响应缺少 HTTP status".to_string())?;
    let mut status_parts = status_line.split_whitespace();
    let version = status_parts.next().unwrap_or_default();
    let status_text = status_parts.next().unwrap_or_default();
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || status_text.len() != 3
        || !status_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("HTTP status 行非法".into());
    }
    let status: u16 = status_text
        .parse()
        .map_err(|_| "HTTP status 非法".to_string())?;
    if !(200..=599).contains(&status) {
        return Err("HTTP 响应不是最终状态".into());
    }
    let mut content_length = None;
    let mut transfer_encoding = None;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| "HTTP 响应头格式非法".to_string())?;
        if name.eq_ignore_ascii_case("content-length") {
            for value in value.split(',') {
                let value = value.trim();
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err("HTTP Content-Length 非法".into());
                }
                let length: usize = value
                    .parse()
                    .map_err(|_| "HTTP Content-Length 超出范围".to_string())?;
                if content_length.is_some_and(|previous| previous != length) {
                    return Err("HTTP Content-Length 冲突".into());
                }
                content_length = Some(length);
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && transfer_encoding.replace(value.trim()).is_some()
        {
            return Err("HTTP Transfer-Encoding 重复".into());
        }
    }
    if transfer_encoding.is_some() && content_length.is_some() {
        return Err("HTTP 响应同时包含 Transfer-Encoding 和 Content-Length".into());
    }
    let framing = if method.eq_ignore_ascii_case("HEAD") || matches!(status, 204 | 304) {
        HttpFraming::NoBody
    } else if let Some(encoding) = transfer_encoding {
        if version != "HTTP/1.1" || !encoding.eq_ignore_ascii_case("chunked") {
            return Err("不支持的 HTTP Transfer-Encoding".into());
        }
        HttpFraming::Chunked
    } else if let Some(length) = content_length {
        HttpFraming::Length(length)
    } else {
        HttpFraming::Close
    };
    Ok(Some((header_end + 4, status, framing)))
}

fn parse_http_response(response: &[u8], method: &str) -> Result<HttpResponse, String> {
    let (start, status, framing) =
        parse_http_head(response, method)?.ok_or_else(|| "响应缺少 header 分隔符".to_string())?;
    let body = &response[start..];
    let decoded = match framing {
        HttpFraming::NoBody => {
            if !body.is_empty() {
                return Err("HTTP 无正文响应包含额外数据".into());
            }
            Vec::new()
        }
        HttpFraming::Chunked => decode_chunked_body(body)?,
        HttpFraming::Length(length) => {
            if length != body.len() {
                return Err("HTTP Content-Length 与实际正文长度不符".into());
            }
            body.to_vec()
        }
        HttpFraming::Close => body.to_vec(),
    };
    let body = String::from_utf8(decoded).map_err(|_| "HTTP 响应正文不是 UTF-8".to_string())?;
    Ok(HttpResponse { status, body })
}

#[derive(Default)]
struct ChunkScanner {
    cursor: usize,
    data_size: Option<usize>,
    trailers: bool,
}

impl ChunkScanner {
    fn advance(&mut self, bytes: &[u8]) -> Result<bool, String> {
        loop {
            if let Some(size) = self.data_size {
                let end = self
                    .cursor
                    .checked_add(size)
                    .and_then(|value| value.checked_add(2))
                    .ok_or_else(|| "HTTP chunk 长度超出范围".to_string())?;
                if bytes.len() < end {
                    return Ok(false);
                }
                if &bytes[end - 2..end] != b"\r\n" {
                    return Err("HTTP chunk 数据格式非法".into());
                }
                self.cursor = end;
                self.data_size = None;
                continue;
            }
            let rest = &bytes[self.cursor..];
            let Some(end) = rest.windows(2).position(|window| window == b"\r\n") else {
                if rest.len() > 8192 {
                    return Err("HTTP chunk 行超过 8 KiB".into());
                }
                return Ok(false);
            };
            if end > 8192 {
                return Err("HTTP chunk 行超过 8 KiB".into());
            }
            let line = &rest[..end];
            self.cursor += end + 2;
            if self.trailers {
                if line.is_empty() {
                    return Ok(true);
                }
                if !line.contains(&b':') {
                    return Err("HTTP chunk trailer 格式非法".into());
                }
            } else {
                let size = line.split(|byte| *byte == b';').next().unwrap_or_default();
                if size.is_empty() || !size.iter().all(u8::is_ascii_hexdigit) {
                    return Err("HTTP chunk 长度非法".into());
                }
                let text =
                    std::str::from_utf8(size).map_err(|_| "HTTP chunk 长度非法".to_string())?;
                let size = usize::from_str_radix(text, 16)
                    .map_err(|_| "HTTP chunk 长度超出范围".to_string())?;
                if size == 0 {
                    self.trailers = true;
                } else {
                    self.data_size = Some(size);
                }
            }
        }
    }
}

fn decode_chunked_body(mut input: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    loop {
        let line_end = input
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| "HTTP chunk 长度行不完整".to_string())?;
        let size = input[..line_end]
            .split(|byte| *byte == b';')
            .next()
            .unwrap_or_default();
        if size.is_empty() || !size.iter().all(u8::is_ascii_hexdigit) {
            return Err("HTTP chunk 长度非法".into());
        }
        let size_text = std::str::from_utf8(size).map_err(|_| "HTTP chunk 长度非法".to_string())?;
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| "HTTP chunk 长度超出范围".to_string())?;
        input = &input[line_end + 2..];
        if size == 0 {
            loop {
                let end = input
                    .windows(2)
                    .position(|window| window == b"\r\n")
                    .ok_or_else(|| "HTTP chunk trailer 不完整".to_string())?;
                let line = &input[..end];
                input = &input[end + 2..];
                if line.is_empty() {
                    if !input.is_empty() {
                        return Err("HTTP chunk 之后有额外数据".into());
                    }
                    return Ok(output);
                }
                if !line.contains(&b':') {
                    return Err("HTTP chunk trailer 格式非法".into());
                }
            }
        }
        let end = size
            .checked_add(2)
            .ok_or_else(|| "HTTP chunk 长度超出范围".to_string())?;
        if input.len() < end || &input[size..end] != b"\r\n" {
            return Err("HTTP chunk 数据不完整".into());
        }
        output.extend_from_slice(&input[..size]);
        input = &input[end..];
    }
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
    FilledExactMismatch {
        client_order_id: u64,
        local: qx_core::ExactDecimal,
        venue: qx_core::ExactDecimal,
    },
}

#[cfg(test)]
mod tests;
