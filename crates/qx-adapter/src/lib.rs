//! 外部适配器：把 REST/HTTP 事实转换为 Provider/Venue 统一契约。
//!
//! 适配器不能直接改 Ledger。网络错误按“结果未知”处理；成功响应才生成 Accepted，
//! 成交必须由外部用户流或显式 `ingest_fill` 进入事实事件。

use qx_core::{Fill, Order, OrderStatus, QxError, QxResult, Side};
use qx_guanxing::{DataSourceId, RawRecord};
use qx_provider::{
    DataProvider, DataQuery, ProviderCapability, ProviderError, ProviderErrorClass, ProviderResult,
};
use qx_zhenlu::{
    AdapterHealth, ConnectorCapabilities, ConnectorState, RateLimiter, Venue, VenueAdapter,
    VenueEvent, VenueOrderSnapshot,
};
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::ServerName;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

mod binance;
mod ccxt;
mod reconcile;
pub use binance::{
    run_binance_user_stream, run_binance_user_stream_live, run_binance_user_stream_testnet,
    run_binance_user_stream_with_config, run_binance_user_stream_with_config_loader,
    BinanceSpotAuth, BinanceSpotCredentials, BinanceSpotMarketData, BinanceSpotMarketStream,
    BinanceSpotUserStream, BinanceSpotVenue, BinanceStreamRetryPolicy, BinanceStreamRunReport,
    BinanceUserStreamRunConfig, BinanceUserStreamSession,
};
pub use ccxt::{CcxtProcessClient, CcxtProcessVenue, CcxtRpc};

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
        let response = read_header_block(&mut session.stream)?;
        validate_websocket_handshake(&response, &key)?;
        Ok(session)
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn send_text(&mut self, payload: &str) -> Result<(), String> {
        write_client_frame(&mut self.stream, 0x1, payload.as_bytes())
    }

    pub fn recv_message(&mut self) -> Result<Option<WebSocketMessage>, String> {
        let mut message = None;
        loop {
            let (fin, opcode, payload) = read_server_frame(&mut self.stream)?;
            match opcode {
                0x8 => return Ok(None),
                0x9 => {
                    write_client_frame(&mut self.stream, 0xA, &payload)?;
                    continue;
                }
                0xA => continue,
                0x1 | 0x2 if message.is_none() => {
                    message = Some(WebSocketMessage { opcode, payload });
                    if fin {
                        return Ok(message);
                    }
                }
                0x0 if message.is_some() => {
                    let current = message.as_mut().expect("message checked");
                    current.payload.extend_from_slice(&payload);
                    if fin {
                        return Ok(message);
                    }
                }
                _ => return Err("WebSocket 分片 opcode 或控制帧非法".into()),
            }
        }
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

/// 供应商签名只存在适配器边界，不进入 Kernel、事件日志或账户事实。
pub trait RequestSigner: Send + Sync {
    fn sign(&self, request: &mut HttpRequest) -> Result<(), String>;
}

/// HMAC-SHA256 请求签名器。
///
/// 签名内容包含方法、主机、端口、路径、时间戳和正文；凭据只存在适配器边界，
/// 不会进入 Kernel、事件日志或账户事实。供应商若有不同的 canonical 规则，应实现
/// `RequestSigner` 替换本组件，而不是把供应商协议泄漏到核心层。
pub struct HmacSha256Signer {
    key_id: String,
    key: hmac::Key,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl HmacSha256Signer {
    pub fn new(key_id: impl Into<String>, secret: &[u8]) -> Result<Self, String> {
        Self::with_clock(key_id, secret, || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0)
        })
    }

    pub fn with_clock<F>(key_id: impl Into<String>, secret: &[u8], clock: F) -> Result<Self, String>
    where
        F: Fn() -> u64 + Send + Sync + 'static,
    {
        if secret.is_empty() {
            return Err("HMAC secret 不能为空".into());
        }
        let key_id = key_id.into();
        if key_id.trim().is_empty() {
            return Err("HMAC key_id 不能为空".into());
        }
        Ok(Self {
            key_id,
            key: hmac::Key::new(hmac::HMAC_SHA256, secret),
            clock: Arc::new(clock),
        })
    }

    pub fn verify(secret: &[u8], request: &HttpRequest) -> Result<(), String> {
        let timestamp = request
            .headers
            .get("X-QX-Timestamp")
            .ok_or_else(|| "缺少 X-QX-Timestamp".to_string())?;
        let signature = request
            .headers
            .get("X-QX-Signature")
            .ok_or_else(|| "缺少 X-QX-Signature".to_string())?;
        let tag = decode_hex(signature)?;
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
        hmac::verify(&key, canonical_request(request, timestamp).as_bytes(), &tag)
            .map_err(|_| "HMAC 签名校验失败".into())
    }

    /// 在密码学校验之外强制检查请求时间窗，防止有效请求被长期重放。
    pub fn verify_at(
        secret: &[u8],
        request: &HttpRequest,
        now: u64,
        max_skew_seconds: u64,
    ) -> Result<(), String> {
        let timestamp = request
            .headers
            .get("X-QX-Timestamp")
            .ok_or_else(|| "缺少 X-QX-Timestamp".to_string())?
            .parse::<u64>()
            .map_err(|_| "X-QX-Timestamp 非法".to_string())?;
        if now.abs_diff(timestamp) > max_skew_seconds {
            return Err("HMAC 请求已超出允许时间窗".into());
        }
        Self::verify(secret, request)
    }
}

impl RequestSigner for HmacSha256Signer {
    fn sign(&self, request: &mut HttpRequest) -> Result<(), String> {
        let timestamp = (self.clock)().to_string();
        request
            .headers
            .insert("X-QX-Key-Id".into(), self.key_id.clone());
        request
            .headers
            .insert("X-QX-Timestamp".into(), timestamp.clone());
        let tag = hmac::sign(&self.key, canonical_request(request, &timestamp).as_bytes());
        request
            .headers
            .insert("X-QX-Signature".into(), encode_hex(tag.as_ref()));
        Ok(())
    }
}

fn canonical_request(request: &HttpRequest, timestamp: &str) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method.to_ascii_uppercase(),
        request.host.to_ascii_lowercase(),
        request.port,
        request.path,
        timestamp,
        request.body
    )
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) {
        return Err("HMAC 签名长度非法".into());
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| "HMAC 签名不是合法十六进制".to_string())
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopSigner;

impl RequestSigner for NoopSigner {
    fn sign(&self, _request: &mut HttpRequest) -> Result<(), String> {
        Ok(())
    }
}

pub struct SignedTransport {
    inner: Arc<dyn HttpTransport>,
    signer: Arc<dyn RequestSigner>,
}

impl SignedTransport {
    pub fn new(inner: Arc<dyn HttpTransport>, signer: Arc<dyn RequestSigner>) -> Self {
        Self { inner, signer }
    }
}

impl HttpTransport for SignedTransport {
    fn send(&self, mut request: HttpRequest) -> Result<HttpResponse, String> {
        self.signer.sign(&mut request)?;
        self.inner.send(request)
    }
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

fn read_header_block<R: Read>(reader: &mut R) -> Result<String, String> {
    let mut bytes = Vec::new();
    loop {
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

fn read_server_frame<R: Read>(reader: &mut R) -> Result<(bool, u8, Vec<u8>), String> {
    let mut head = [0_u8; 2];
    reader
        .read_exact(&mut head)
        .map_err(|error| error.to_string())?;
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
    if length > 16 * 1024 * 1024 {
        return Err("WebSocket 帧超过 16 MiB 限制".into());
    }
    if opcode >= 0x8 && (!fin || length > 125) {
        return Err("WebSocket 控制帧格式非法".into());
    }
    let mut payload = vec![0_u8; length as usize];
    reader
        .read_exact(&mut payload)
        .map_err(|error| error.to_string())?;
    Ok((fin, opcode, payload))
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

pub struct RestProvider {
    capability: ProviderCapability,
    transport: Arc<dyn HttpTransport>,
    host: String,
    port: u16,
    path: String,
}

impl RestProvider {
    pub fn new(
        capability: ProviderCapability,
        transport: Arc<dyn HttpTransport>,
        host: impl Into<String>,
        port: u16,
        path: impl Into<String>,
    ) -> Self {
        Self {
            capability,
            transport,
            host: host.into(),
            port,
            path: path.into(),
        }
    }
}

#[derive(Deserialize)]
struct RawRecordWire {
    event_time: u64,
    receive_time: u64,
    payload_hash: u64,
    schema_version: u32,
}

impl DataProvider for RestProvider {
    fn capability(&self) -> &ProviderCapability {
        &self.capability
    }

    fn fetch(&self, query: &DataQuery) -> Result<ProviderResult, ProviderError> {
        query.validate()?;
        let path = format!(
            "{}?kind={:?}&asset_class={}&instruments={}&fields={}&frequency={}&adjustment={}&quality_policy={}&start={}&end={}&as_of={}",
            self.path,
            query.kind,
            query.asset_class,
            query.instrument_set.iter().cloned().collect::<Vec<_>>().join(","),
            query.field_set.iter().cloned().collect::<Vec<_>>().join(","),
            query.frequency,
            query.adjustment,
            query.quality_policy,
            query.start,
            query.end,
            query.as_of.unwrap_or(u64::MAX)
        );
        let response = self
            .transport
            .send(HttpRequest {
                method: "GET".into(),
                host: self.host.clone(),
                port: self.port,
                path,
                body: String::new(),
                headers: BTreeMap::new(),
            })
            .map_err(|error| ProviderError::new(ProviderErrorClass::SwitchProvider, error))?;
        if !(200..300).contains(&response.status) {
            return Err(ProviderError::new(
                if response.status == 401 || response.status == 403 {
                    ProviderErrorClass::ManualIntervention
                } else if response.status == 429 || response.status >= 500 {
                    ProviderErrorClass::SwitchProvider
                } else {
                    ProviderErrorClass::Permanent
                },
                format!("Provider HTTP status {}", response.status),
            ));
        }
        let wires: Vec<RawRecordWire> = serde_json::from_str(&response.body).map_err(|error| {
            ProviderError::new(
                ProviderErrorClass::Permanent,
                format!("Provider JSON 非法: {error}"),
            )
        })?;
        let mut records = wires
            .into_iter()
            .filter(|record| {
                record.event_time >= query.start
                    && record.event_time <= query.end
                    && query.as_of.is_none_or(|as_of| record.receive_time <= as_of)
            })
            .map(|record| RawRecord {
                source: DataSourceId::new(self.capability.provider_id.clone()),
                event_time: record.event_time,
                receive_time: record.receive_time,
                payload_hash: record.payload_hash,
                schema_version: record.schema_version,
            })
            .collect::<Vec<_>>();
        records.sort_by_key(|record| (record.event_time, record.receive_time, record.payload_hash));
        let mut result = ProviderResult {
            records,
            provider_id: self.capability.provider_id.clone(),
            provider_version: self.capability.version.clone(),
            request_id: format!("{}-{:016x}", self.capability.provider_id, query.digest()),
            retry_chain: Vec::new(),
            received_at: query.end,
            source_hash: 0,
        };
        result.source_hash = result.compute_source_hash();
        Ok(result)
    }
}

pub struct RestVenue {
    id: String,
    transport: Arc<dyn HttpTransport>,
    host: String,
    port: u16,
    submit_path: String,
    cancel_path: String,
    snapshot_path: String,
    connected: bool,
    state: ConnectorState,
    orders: BTreeMap<u64, Order>,
    venue_order_ids: BTreeMap<u64, String>,
    seen_fill_keys: BTreeSet<(u64, u64, i128, i128, i128, String)>,
    rate_limiter: RateLimiter,
    last_event_ts: u64,
    reconnects: u64,
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

impl RestVenue {
    pub fn new(
        id: impl Into<String>,
        transport: Arc<dyn HttpTransport>,
        host: impl Into<String>,
        port: u16,
        submit_path: impl Into<String>,
        cancel_path: impl Into<String>,
        snapshot_path: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            transport,
            host: host.into(),
            port,
            submit_path: submit_path.into(),
            cancel_path: cancel_path.into(),
            snapshot_path: snapshot_path.into(),
            connected: true,
            state: ConnectorState::Live,
            orders: BTreeMap::new(),
            venue_order_ids: BTreeMap::new(),
            seen_fill_keys: BTreeSet::new(),
            rate_limiter: RateLimiter::new(100, 100, 0),
            last_event_ts: 0,
            reconnects: 0,
        }
    }

    pub fn disconnect(&mut self) {
        self.connected = false;
        self.state = ConnectorState::ReconcileRequired;
    }

    pub fn reconnect(&mut self) {
        self.connected = true;
        self.state = ConnectorState::Snapshotting;
        self.reconnects += 1;
    }

    pub fn with_rate_limit(mut self, capacity: u64, refill_per_second: u64) -> Self {
        self.rate_limiter = RateLimiter::new(capacity, refill_per_second, 0);
        self
    }

    pub fn ingest_fill(&mut self, mut fill: Fill) -> QxResult<VenueEvent> {
        let order = self
            .orders
            .get_mut(&fill.order_id)
            .ok_or_else(|| QxError::Invariant("外部成交对应的本地订单不存在".into()))?;
        if fill.qty.raw() <= 0 || fill.qty.raw() > order.remaining().raw() {
            return Err(QxError::Invariant("外部成交超过本地订单剩余量".into()));
        }
        let next_filled = qx_core::Quantity::from_raw(order.filled.raw() + fill.qty.raw());
        let next_status = if next_filled == order.qty {
            OrderStatus::Filled
        } else {
            OrderStatus::PartiallyFilled
        };
        if matches!(order.status, OrderStatus::Accepted | OrderStatus::Working) {
            order
                .status
                .transition(OrderStatus::Working)
                .map_err(QxError::Invariant)?;
        }
        order
            .status
            .transition(next_status)
            .map_err(QxError::Invariant)?;
        order.filled = next_filled;
        let venue_order_id = fill.venue_order_id.clone();
        order.trace_fill(&mut fill, Some(&self.id), venue_order_id.as_deref());
        self.last_event_ts = fill.ts;
        Ok(VenueEvent::Fill(fill))
    }

    /// 用户流的幂等成交入口；重复回报返回 None，绝不再次修改本地订单。
    pub fn ingest_fill_once(&mut self, fill: Fill) -> QxResult<Option<VenueEvent>> {
        let key = (
            fill.order_id,
            fill.ts,
            fill.qty.raw(),
            fill.price.raw(),
            fill.fee.raw(),
            fill.venue_order_id.clone().unwrap_or_default(),
        );
        if self.seen_fill_keys.contains(&key) {
            return Ok(None);
        }
        match self.ingest_fill(fill) {
            Ok(event) => {
                self.seen_fill_keys.insert(key);
                Ok(Some(event))
            }
            Err(error) => Err(error),
        }
    }

    pub fn fetch_remote_snapshot(&mut self) -> QxResult<Vec<VenueOrderSnapshot>> {
        let response = self.request(
            "GET",
            &self.snapshot_path.clone(),
            String::new(),
            self.last_event_ts,
        )?;
        if !(200..300).contains(&response.status) {
            return Err(if response.status == 429 || response.status >= 500 {
                QxError::Transient(format!("REST snapshot status {}", response.status))
            } else {
                QxError::Permanent(format!("REST snapshot status {}", response.status))
            });
        }
        let remote: Vec<VenueOrderWire> = serde_json::from_str(&response.body)
            .map_err(|error| QxError::Permanent(format!("snapshot JSON 非法: {error}")))?;
        remote
            .into_iter()
            .map(|order| {
                if order.filled_raw < 0 {
                    return Err(QxError::Permanent("snapshot 包含负成交数量".into()));
                }
                Ok(VenueOrderSnapshot {
                    client_order_id: order.client_order_id,
                    status: order.status,
                    filled: qx_core::Quantity::from_raw(order.filled_raw),
                })
            })
            .collect()
    }

    pub fn reconcile_remote(
        &mut self,
        remote: &[VenueOrderSnapshot],
    ) -> QxResult<Vec<AdapterReconcileIssue>> {
        if !self.connected || !matches!(self.state, ConnectorState::Snapshotting) {
            return Err(QxError::VenueState("REST Venue 不在重连对账阶段".into()));
        }
        let remote = remote
            .iter()
            .try_fold(BTreeMap::new(), |mut orders, order| {
                if orders.insert(order.client_order_id, order).is_some() {
                    return Err(QxError::ReconcileRequired(
                        "远端快照包含重复 client_order_id".into(),
                    ));
                }
                Ok(orders)
            })?;
        // venue 专属前置硬校验：远端成交数量必须落在本地订单量范围内，否则快照本身
        // 不可信，直接判为对账失败（不作为普通差异上报）。
        for (id, local) in &self.orders {
            if let Some(remote) = remote.get(id) {
                if remote.filled.raw() < 0 || remote.filled.raw() > local.qty.raw() {
                    return Err(QxError::ReconcileRequired(format!(
                        "远端订单 {} 成交数量超出本地订单范围",
                        id
                    )));
                }
            }
        }
        // 判定委托 qx-genglu（对账归一，V10 §4.8）：装配中性事实并翻译回本类型。
        let local_facts: Vec<_> = self
            .orders
            .iter()
            .map(|(id, order)| reconcile::local_order_fact(id, order))
            .collect();
        let issues =
            reconcile::reconcile_issues(&local_facts, &reconcile::remote_order_facts(&remote));
        self.state = if issues.is_empty() {
            ConnectorState::Live
        } else {
            ConnectorState::ReconcileRequired
        };
        Ok(issues)
    }

    pub fn fetch_and_reconcile(&mut self) -> QxResult<Vec<AdapterReconcileIssue>> {
        let remote = match self.fetch_remote_snapshot() {
            Ok(remote) => remote,
            Err(error) => {
                self.state = ConnectorState::ReconcileRequired;
                return Err(error);
            }
        };
        self.reconcile_remote(&remote)
    }

    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: String,
        now: u64,
    ) -> QxResult<HttpResponse> {
        if !self.connected {
            return Err(QxError::Ambiguous("REST Venue 连接中断".into()));
        }
        if !self.rate_limiter.try_acquire(1, now) {
            return Err(QxError::ResourceExhausted("Venue REST 请求限频".into()));
        }
        match self.transport.send(HttpRequest {
            method: method.into(),
            host: self.host.clone(),
            port: self.port,
            path: path.into(),
            body,
            headers: BTreeMap::new(),
        }) {
            Ok(response) => Ok(response),
            Err(error) => {
                self.state = ConnectorState::ReconcileRequired;
                Err(QxError::Ambiguous(error))
            }
        }
    }
}

#[derive(Deserialize)]
struct AcceptedWire {
    venue_order_id: String,
}

#[derive(Deserialize)]
struct VenueOrderWire {
    client_order_id: u64,
    status: OrderStatus,
    filled_raw: i128,
}

impl Venue for RestVenue {
    fn id(&self) -> &str {
        &self.id
    }

    fn submit(&mut self, order: Order, ts: u64) -> QxResult<Vec<VenueEvent>> {
        order.validate().map_err(QxError::BusinessViolation)?;
        if !matches!(
            order.status,
            OrderStatus::PendingSubmit | OrderStatus::Submitted
        ) {
            return Err(QxError::BusinessViolation(
                "REST Venue 只接受待提交或已提交订单".into(),
            ));
        }
        if !self.connected {
            return Err(QxError::Ambiguous("REST Venue 连接中断".into()));
        }
        if !matches!(self.state, ConnectorState::Live) {
            return Err(QxError::VenueState("REST Venue 尚未完成对账".into()));
        }
        if self.orders.contains_key(&order.client_id) {
            if let Some(venue_order_id) = self.venue_order_ids.get(&order.client_id) {
                return Ok(vec![VenueEvent::Accepted {
                    client_order_id: order.client_id,
                    venue_order_id: venue_order_id.clone(),
                    ts,
                }]);
            }
            return Err(QxError::ReconcileRequired("本地订单缺少远端订单号".into()));
        }
        let payload = serde_json::json!({
            "client_order_id": order.client_id,
            "instrument": order.instrument.to_string(),
            "side": match order.side { Side::Buy => "BUY", Side::Sell => "SELL" },
            "quantity_raw": order.qty.raw(),
            "limit_raw": order.limit.map(|price| price.raw()),
            "account_id": order.account_id,
        })
        .to_string();
        let client_order_id = order.client_id;
        let response = self.request("POST", &self.submit_path.clone(), payload, ts)?;
        if !(200..300).contains(&response.status) {
            return Err(if response.status == 429 || response.status >= 500 {
                QxError::Transient(format!("REST submit status {}", response.status))
            } else {
                QxError::Permanent(format!("REST submit status {}", response.status))
            });
        }
        let accepted: AcceptedWire = serde_json::from_str(&response.body)
            .map_err(|error| QxError::Permanent(format!("Accepted JSON 非法: {error}")))?;
        if accepted.venue_order_id.trim().is_empty() {
            return Err(QxError::Permanent("Accepted JSON 缺少远端订单号".into()));
        }
        let mut accepted_order = order;
        accepted_order.status = OrderStatus::Accepted;
        self.orders.insert(client_order_id, accepted_order);
        self.venue_order_ids
            .insert(client_order_id, accepted.venue_order_id.clone());
        Ok(vec![VenueEvent::Accepted {
            client_order_id,
            venue_order_id: accepted.venue_order_id,
            ts,
        }])
    }

    fn cancel(&mut self, client_order_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        if !self.connected {
            return Err(QxError::Ambiguous("REST 取消结果未知".into()));
        }
        if !matches!(self.state, ConnectorState::Live) {
            return Err(QxError::VenueState("REST Venue 尚未完成对账".into()));
        }
        let existing = self
            .orders
            .get(&client_order_id)
            .ok_or_else(|| QxError::Permanent("本地订单不存在".into()))?;
        if existing.status.is_terminal() {
            return Ok(Vec::new());
        }
        if matches!(existing.status, OrderStatus::Unknown) {
            return Err(QxError::ReconcileRequired(
                "未知订单状态不能直接撤单".into(),
            ));
        }
        if !self.venue_order_ids.contains_key(&client_order_id) {
            return Err(QxError::Permanent("本地订单缺少远端订单号".into()));
        }
        let body = serde_json::json!({ "client_order_id": client_order_id }).to_string();
        let response = self.request("POST", &self.cancel_path.clone(), body, ts)?;
        if !(200..300).contains(&response.status) {
            return Err(if response.status == 429 || response.status >= 500 {
                QxError::Transient(format!("REST cancel status {}", response.status))
            } else {
                QxError::Permanent(format!("REST cancel status {}", response.status))
            });
        }
        let order = self
            .orders
            .get_mut(&client_order_id)
            .ok_or_else(|| QxError::Permanent("本地订单不存在".into()))?;
        order
            .status
            .transition(OrderStatus::CancelPending)
            .map_err(QxError::Invariant)?;
        order
            .status
            .transition(OrderStatus::Cancelled)
            .map_err(QxError::Invariant)?;
        Ok(vec![VenueEvent::Cancelled {
            client_order_id,
            ts,
        }])
    }

    fn snapshot(&self) -> Vec<VenueOrderSnapshot> {
        self.orders
            .values()
            .map(|order| VenueOrderSnapshot {
                client_order_id: order.client_id,
                status: order.status,
                filled: order.filled,
            })
            .collect()
    }

    fn connected(&self) -> bool {
        self.connected
    }
}

impl VenueAdapter for RestVenue {
    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            market_data: false,
            user_stream: false,
            submit: true,
            cancel: true,
            replace: false,
        }
    }

    fn health(&self) -> AdapterHealth {
        AdapterHealth {
            state: self.state,
            last_event_ts: self.last_event_ts,
            reconnects: self.reconnects,
        }
    }
}

pub struct RestSnapshotSource {
    venue: String,
    transport: Arc<dyn HttpTransport>,
    host: String,
    port: u16,
    path: String,
}

impl RestSnapshotSource {
    pub fn new(
        venue: impl Into<String>,
        transport: Arc<dyn HttpTransport>,
        host: impl Into<String>,
        port: u16,
        path: impl Into<String>,
    ) -> Self {
        Self {
            venue: venue.into(),
            transport,
            host: host.into(),
            port,
            path: path.into(),
        }
    }

    pub fn reconcile(&self, local: &[VenueOrderSnapshot]) -> QxResult<Vec<VenueOrderSnapshot>> {
        let response = self
            .transport
            .send(HttpRequest {
                method: "GET".into(),
                host: self.host.clone(),
                port: self.port,
                path: self.path.clone(),
                body: String::new(),
                headers: BTreeMap::new(),
            })
            .map_err(QxError::Ambiguous)?;
        if !(200..300).contains(&response.status) {
            return Err(QxError::Permanent(format!(
                "snapshot status {}",
                response.status
            )));
        }
        let remote: Vec<VenueOrderWire> = serde_json::from_str(&response.body)
            .map_err(|error| QxError::Permanent(format!("snapshot JSON 非法: {error}")))?;
        let _ = (local, &self.venue);
        Ok(remote
            .into_iter()
            .map(|order| VenueOrderSnapshot {
                client_order_id: order.client_order_id,
                status: order.status,
                filled: qx_core::Quantity::from_raw(order.filled_raw),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::InstrumentId;
    use qx_provider::{DataKind, ProviderCapability};
    use std::io::Cursor;
    use std::net::TcpListener;
    use std::thread;

    #[derive(Clone)]
    struct MockTransport {
        responses: BTreeMap<String, HttpResponse>,
    }

    impl HttpTransport for MockTransport {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
            self.responses
                .get(&request.path)
                .cloned()
                .ok_or_else(|| "missing mock route".into())
        }
    }

    struct HeaderSigner;

    impl RequestSigner for HeaderSigner {
        fn sign(&self, request: &mut HttpRequest) -> Result<(), String> {
            request
                .headers
                .insert("X-Test-Signature".into(), "ok".into());
            Ok(())
        }
    }

    struct HeaderEchoTransport;

    impl HttpTransport for HeaderEchoTransport {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
            Ok(HttpResponse {
                status: 200,
                body: request
                    .headers
                    .get("X-Test-Signature")
                    .cloned()
                    .unwrap_or_default(),
            })
        }
    }

    fn capability() -> ProviderCapability {
        ProviderCapability {
            provider_id: "rest".into(),
            version: "v1".into(),
            data_kinds: [DataKind::Bar].into_iter().collect(),
            asset_classes: ["crypto".into()].into_iter().collect(),
            frequencies: ["1d".into()].into_iter().collect(),
            auth_scope: "public".into(),
            rate_limit_per_second: 10,
            freshness_seconds: 60,
            historical_start: 0,
            historical_end: 100,
            realtime: false,
            priority: 1,
            quality_score: 100,
            cost_score: 1,
        }
    }

    #[test]
    fn rest_provider_keeps_raw_source_and_query_boundary() {
        let transport = Arc::new(MockTransport {
            responses: [("/bars?kind=Bar&asset_class=crypto&instruments=&fields=&frequency=1d&adjustment=none&quality_policy=strict&start=1&end=2&as_of=2".into(), HttpResponse {
                status: 200,
                body: "[{\"event_time\":1,\"receive_time\":2,\"payload_hash\":3,\"schema_version\":1},{\"event_time\":2,\"receive_time\":3,\"payload_hash\":4,\"schema_version\":1}]".into(),
            })]
            .into_iter()
            .collect(),
        });
        let provider = RestProvider::new(capability(), transport, "mock", 80, "/bars");
        let result = provider
            .fetch(&DataQuery {
                kind: DataKind::Bar,
                asset_class: "crypto".into(),
                instrument_set: BTreeSet::new(),
                field_set: BTreeSet::new(),
                frequency: "1d".into(),
                adjustment: "none".into(),
                quality_policy: "strict".into(),
                start: 1,
                end: 2,
                as_of: Some(2),
            })
            .unwrap();
        assert_eq!(result.records[0].source.0, "rest");
        assert_eq!(result.records.len(), 1);
    }

    #[test]
    fn signed_transport_keeps_credentials_at_adapter_boundary() {
        let transport = SignedTransport::new(Arc::new(HeaderEchoTransport), Arc::new(HeaderSigner));
        let response = transport
            .send(HttpRequest {
                method: "GET".into(),
                host: "mock".into(),
                port: 80,
                path: "/health".into(),
                body: String::new(),
                headers: BTreeMap::new(),
            })
            .unwrap();
        assert_eq!(response.body, "ok");
    }

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
    fn hmac_signer_is_deterministic_and_rejects_tampering() {
        let signer = HmacSha256Signer::with_clock("demo", b"secret", || 1_700_000_000).unwrap();
        let mut request = HttpRequest {
            method: "get".into(),
            host: "EXAMPLE.COM".into(),
            port: 443,
            path: "/orders?symbol=T.SIM".into(),
            body: "{}".into(),
            headers: BTreeMap::new(),
        };
        signer.sign(&mut request).unwrap();
        assert_eq!(request.headers.get("X-QX-Key-Id"), Some(&"demo".into()));
        HmacSha256Signer::verify(b"secret", &request).unwrap();
        HmacSha256Signer::verify_at(b"secret", &request, 1_700_000_005, 10).unwrap();
        assert!(HmacSha256Signer::verify_at(b"secret", &request, 1_700_000_011, 10).is_err());
        request.body = "{\"tampered\":true}".into();
        assert!(HmacSha256Signer::verify(b"secret", &request).is_err());
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
            read_server_frame(&mut frames).unwrap(),
            (true, 0x1, b"ok".to_vec())
        );
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

    #[test]
    fn rest_venue_maps_submit_and_external_fill_to_facts() {
        let transport = Arc::new(MockTransport {
            responses: [
                (
                    "/submit".into(),
                    HttpResponse {
                        status: 200,
                        body: "{\"venue_order_id\":\"v-1\"}".into(),
                    },
                ),
                (
                    "/snapshot".into(),
                    HttpResponse {
                        status: 200,
                        body: "[{\"client_order_id\":7,\"status\":\"Filled\",\"filled_raw\":1000000000}]"
                            .into(),
                    },
                ),
            ]
            .into_iter()
            .collect(),
        });
        let instrument = InstrumentId::parse("T.SIM").unwrap();
        let mut venue = RestVenue::new(
            "rest",
            transport,
            "mock",
            80,
            "/submit",
            "/cancel",
            "/snapshot",
        );
        let order = Order {
            client_id: 7,
            instrument,
            side: Side::Buy,
            qty: qx_core::Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Submitted,
            filled: qx_core::Quantity::ZERO,
            account_id: "a".into(),
            trace: None,
            policy: None,
        };
        let accepted = venue.submit(order.clone(), 1).unwrap();
        assert!(matches!(
            accepted[0],
            VenueEvent::Accepted {
                client_order_id: 7,
                ..
            }
        ));
        let duplicate = venue.submit(order, 2).unwrap();
        assert!(matches!(
            duplicate[0],
            VenueEvent::Accepted {
                client_order_id: 7,
                ..
            }
        ));
        let event = venue
            .ingest_fill(Fill {
                order_id: 7,
                qty: qx_core::Quantity::from_i64(1),
                price: qx_core::Price::from_i64(10),
                ts: 2,
                ..Fill::default()
            })
            .unwrap();
        match event {
            VenueEvent::Fill(fill) => {
                assert_eq!(fill.account_id, "a");
                assert_eq!(fill.venue_id.as_deref(), Some("rest"));
            }
            _ => panic!("expected fill"),
        }
        assert_eq!(venue.fetch_remote_snapshot().unwrap()[0].client_order_id, 7);
        venue.disconnect();
        venue.reconnect();
        assert!(venue.fetch_and_reconcile().unwrap().is_empty());
        assert_eq!(venue.health().state, ConnectorState::Live);
    }

    struct FailingTransport;

    impl HttpTransport for FailingTransport {
        fn send(&self, _request: HttpRequest) -> Result<HttpResponse, String> {
            Err("connection reset".into())
        }
    }

    #[test]
    fn ambiguous_rest_submission_enters_reconcile_required() {
        let instrument = InstrumentId::parse("T.SIM").unwrap();
        let mut venue = RestVenue::new(
            "rest",
            Arc::new(FailingTransport),
            "mock",
            80,
            "/submit",
            "/cancel",
            "/snapshot",
        );
        let error = venue.submit(
            Order {
                client_id: 9,
                instrument,
                side: Side::Buy,
                qty: qx_core::Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::Submitted,
                filled: qx_core::Quantity::ZERO,
                account_id: "a".into(),
                trace: None,
                policy: None,
            },
            1,
        );
        assert!(matches!(error, Err(QxError::Ambiguous(_))));
        assert_eq!(venue.health().state, ConnectorState::ReconcileRequired);
    }

    #[test]
    fn user_stream_fill_replay_is_ingested_once() {
        let instrument = InstrumentId::parse("T.SIM").unwrap();
        let transport = Arc::new(MockTransport {
            responses: [(
                "/submit".into(),
                HttpResponse {
                    status: 200,
                    body: "{\"venue_order_id\":\"v-1\"}".into(),
                },
            )]
            .into_iter()
            .collect(),
        });
        let mut venue = RestVenue::new(
            "rest",
            transport,
            "mock",
            80,
            "/submit",
            "/cancel",
            "/snapshot",
        );
        venue
            .submit(
                Order {
                    client_id: 11,
                    instrument,
                    side: Side::Buy,
                    qty: qx_core::Quantity::from_i64(2),
                    limit: None,
                    status: OrderStatus::Submitted,
                    filled: qx_core::Quantity::ZERO,
                    account_id: "a".into(),
                    trace: None,
                    policy: None,
                },
                1,
            )
            .unwrap();
        let fill = Fill {
            order_id: 11,
            qty: qx_core::Quantity::from_i64(1),
            price: qx_core::Price::from_i64(10),
            ts: 2,
            ..Fill::default()
        };
        assert!(venue.ingest_fill_once(fill.clone()).unwrap().is_some());
        assert!(venue.ingest_fill_once(fill).unwrap().is_none());
        assert_eq!(venue.snapshot()[0].filled, qx_core::Quantity::from_i64(1));
    }

    #[test]
    fn rest_venue_rate_limit_isolated_from_business_errors() {
        let instrument = InstrumentId::parse("T.SIM").unwrap();
        let transport = Arc::new(MockTransport {
            responses: [(
                "/submit".into(),
                HttpResponse {
                    status: 200,
                    body: "{\"venue_order_id\":\"v-1\"}".into(),
                },
            )]
            .into_iter()
            .collect(),
        });
        let mut venue = RestVenue::new(
            "rest",
            transport,
            "mock",
            80,
            "/submit",
            "/cancel",
            "/snapshot",
        )
        .with_rate_limit(1, 0);
        venue
            .submit(
                Order {
                    client_id: 1,
                    instrument,
                    side: Side::Buy,
                    qty: qx_core::Quantity::from_i64(1),
                    limit: None,
                    status: OrderStatus::Submitted,
                    filled: qx_core::Quantity::ZERO,
                    account_id: "a".into(),
                    trace: None,
                    policy: None,
                },
                1,
            )
            .unwrap();
        assert!(matches!(
            venue.cancel(1, 1),
            Err(QxError::ResourceExhausted(_))
        ));
    }
}
