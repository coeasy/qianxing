//! HTTP/WebSocket 传输层：套接字选项、TLS 包装、请求读取、响应写出与握手摘要。
//!
//! 这些函数不含任何领域判定——权限、限流、路由与投影都在 `lib.rs` 的 `ApiService` 上。
//! 外置的理由是行数棘轮：`lib.rs` 已顶到登记值，浏览器准入面（CORS、连接预算、升级
//! 路径闸、查询串解码）还要在同一个 crate 里落地，与判定无关的字节搬运必须先挪出去。

use crate::ApiResponse;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn configure_connection(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))
}

pub(crate) fn tls_stream(
    stream: TcpStream,
    config: Arc<ServerConfig>,
) -> std::io::Result<StreamOwned<ServerConnection, TcpStream>> {
    let connection = ServerConnection::new(config).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("TLS server config invalid: {error}"),
        )
    })?;
    Ok(StreamOwned::new(connection, stream))
}

pub(crate) fn write_ws_text<S: Write>(stream: &mut S, text: &str) -> std::io::Result<()> {
    let payload = text.as_bytes();
    let mut frame = vec![0x81_u8];
    match payload.len() {
        0..=125 => frame.push(payload.len() as u8),
        126..=65_535 => {
            frame.push(126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        _ => {
            frame.push(127);
            frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)
}

pub(crate) fn read_request<S: Read>(stream: &mut S) -> std::io::Result<Vec<u8>> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..count]);
        if request.len() > 1_048_576 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "http request too large",
            ));
        }
        if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            let header_end = header_end + 4;
            let header = String::from_utf8_lossy(&request[..header_end]);
            let mut content_length = 0_usize;
            for line in header.lines() {
                let Some((name, value)) = line.split_once(':') else {
                    continue;
                };
                if name.eq_ignore_ascii_case("Content-Length") {
                    content_length = value.trim().parse::<usize>().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid Content-Length",
                        )
                    })?;
                    break;
                }
            }
            let expected = header_end.checked_add(content_length).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "request length overflow")
            })?;
            if expected > 1_048_576 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "http request too large",
                ));
            }
            if request.len() >= expected {
                break;
            }
        }
    }
    Ok(request)
}

pub(crate) fn parse_http_request(request: &str) -> Result<(String, String, String), String> {
    let (header, body) = request
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed http request".to_string())?;
    let mut first = header.lines().next().unwrap_or_default().split_whitespace();
    let method = first.next().ok_or_else(|| "missing method".to_string())?;
    let path = first.next().ok_or_else(|| "missing path".to_string())?;
    Ok((method.into(), path.into(), body.into()))
}

/// 响应写出。`extra` 是这一层连接要追加的头部（跨源准入那几格），必须是调用方已经
/// 判定过的**定值**：这里不做任何转义，把客户端给的字符串直接塞进来就是一条响应头注入。
///
/// 追加而不改 `ApiResponse` 的形状，是因为那份结构体是读面的公开返回类型，
/// `crates/qx-cli/src/tests/api_response_field_doc.rs` 按它的字段逐条与文档比相等；
/// 跨源头是**这一条连接**的属性（随 `Origin` 而变），不是响应的属性。
///
/// 状态行的短语必须与状态码本身说的是同一件事：`403` 与 `503` 过去一起落进 `_ => "Internal
/// Server Error"`，于是"认证没过"和"后端暂时不接"在 HTTP 层都被读成"服务坏了"（与 #172 把
/// 风控端口的"拒绝"与"端口坏了"分成两条通道同族，V13 R2 第十六遍）。
/// `_` 那格给的是 `Unknown` 而不是复用 500 的短语：本构建写出的每个状态码都在上面的名单里，
/// 落到这里就是新增了没登记的状态码，不该由一个听起来正确的词把差异盖住。
/// 取数判据 `status_line_reason_names_cover_every_code_the_read_face_emits` 读的就是本文件
/// 这一处 match（传输层外置之后它不再住在 `lib.rs`）。
pub(crate) fn write_http_response<S: Write>(
    stream: &mut S,
    response: &ApiResponse,
    extra: &[(String, String)],
) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\n",
        response.status, reason, response.content_type
    );
    // 204 不许带 Content-Length（RFC 9110 §15.3.5：1xx 与 204 一律不得出现这一格）。
    // 预检那一条正是 204，写上去就会被严格实现读成"正文还有 0 字节要等"，
    // 于是浏览器侧的预检以协议错误收场，而不是以"允许"收场。
    if response.status != 204 {
        head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    }
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(response.body.as_bytes())
}

/// 把一条**还没读过任何字节**的连接体面地拒掉：写出拒绝响应，半关写方向，再把对端
/// 已经发来的请求字节读掉丢弃。
///
/// 不排空就走的话，套接字接收缓冲里还躺着未读数据，`close` 会以 RST 收场，客户端读到的
/// 是"连接被重置"而不是那条 503——一个有名字的拒绝码就这样变成查不到出口的故障
/// （`browser_admission.rs` 的预算用例立案时实测到的正是 10054）。
///
/// 排空必须在半关**之后**：先关写方向，客户端才读得到 FIN 并结束它自己的读，随后关闭；
/// 反过来先排空就会两边互相等。读侧沿用 `configure_connection` 装好的 100ms 超时，所以
/// 一个只连不发的对端最多把 accept 循环按住一轮，不会变成新的无出口循环。
pub(crate) fn refuse_connection(stream: &TcpStream, response: &ApiResponse) {
    let mut writer = stream;
    if let Err(error) = write_http_response(&mut writer, response, &[]) {
        eprintln!("[qx-api] 写出拒绝响应失败: {error}");
        return;
    }
    if let Err(error) = stream.shutdown(std::net::Shutdown::Write) {
        eprintln!("[qx-api] 半关拒绝连接失败: {error}");
    }
    let mut discard = [0_u8; 4096];
    let mut drained = 0_usize;
    // 上界与 `read_request` 的请求体上界同量级：排空是为了让 RST 变成 FIN，
    // 不是为了替对端把一份超大请求收完。
    while drained < 64 * 1024 {
        match writer.read(&mut discard) {
            Ok(0) => break,
            Ok(size) => drained += size,
            Err(_) => break,
        }
    }
}

pub fn websocket_accept(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64_encode(&sha1(&input))
}

pub(crate) fn sha1(input: &[u8]) -> [u8; 20] {
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
        let mut w = [0_u32; 80];
        for (i, slot) in w.iter_mut().take(16).enumerate() {
            let offset = i * 4;
            *slot = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, value) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*value);
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
    let mut out = [0_u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (a << 16) | (b << 8) | c;
        out.push(TABLE[((triple >> 18) & 63) as usize] as char);
        out.push(TABLE[((triple >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
