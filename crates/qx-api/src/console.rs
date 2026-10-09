//! 同源 BFF（M4'）：把静态控制台与 `qx-api` 放在**同一个源**上，会话与 CSRF 留在服务端。
//!
//! ## 为什么要有这一层
//!
//! `web/console/` 是一份静态页面。它单独部署时与 API 不同源：浏览器必须先过 CORS，
//! 而页面手里没有任何凭据——控制写面就只能靠"只允许本机回环"这种网络位置来挡。
//! 同源 BFF 把这件事换一种做法：**浏览器只跟一个源说话**，那个源既发静态资源、
//! 又代理 API，于是
//!
//! * 浏览器侧不再需要 CORS allowlist（同源请求根本不发 `Origin` 预检）；
//! * 会话 cookie 与 CSRF token 都落在**服务端**，页面拿不到也不需要拿任何凭据；
//! * 身份由这一层注入——`handle_inner` 收到的是**服务端认过的** operator，
//!   而不是命令体里那个可以随便填的 `operator_id`（那是审计字段，不是认证）。
//!
//! ## 边界（诚实登记）
//!
//! 本实现是**单进程形态**：BFF 与 `qx-api` 在同一个进程里，代理走的是进程内调用而不是
//! 网络回环。多机部署要的是"独立反代持有客户端证书"那种形态，本仓**没有**实现它
//! （那属于部署件，不是代码）。因此本层只应监听**回环地址**——`console_bind_is_loopback`
//! 就是这个约束的判据，装配处必须先过它。
//!
//! ## 会话与 CSRF
//!
//! * 引导：运维带着 `?token=<bootstrap>` 打开一次页面，服务端校验后签发会话 cookie
//!   （`HttpOnly` + `SameSite=Strict`）与一枚**非 HttpOnly** 的 CSRF cookie。
//! * 读写：后续请求按会话 cookie 认人；**非 GET 请求**还必须带 `X-QX-CSRF` 头，
//!   且与服务器记住的那一枚逐字符相等，否则 403。
//! * 令牌不是随机数发生器：会话 id 与 CSRF 都由**引导令牌**（秘密）经 SHA-1 派生的
//!   伪随机函数给出。秘密在运维手里，派生值不可预测；这样本 crate 不必引入 RNG 依赖，
//!   而"秘密只有一个来源"也让轮换变得可解释。

use crate::admission::{ConnectionBudget, ConnectionGuard, DEFAULT_MAX_CONCURRENT_CONNECTIONS};
use crate::transport::{configure_connection, read_request, sha1, write_http_response};
use crate::{ApiResponse, ApiService};
use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 会话 cookie 名。`HttpOnly`：页面脚本读不到它，XSS 也偷不走会话。
pub const CONSOLE_SESSION_COOKIE: &str = "qx_console_session";
/// CSRF cookie 名。**刻意不是 HttpOnly**：页面要读出来放进请求头，这正是双提交模式。
pub const CONSOLE_CSRF_COOKIE: &str = "qx_console_csrf";
/// 非 GET 请求必须带的头名（小写比较）。
pub const CONSOLE_CSRF_HEADER: &str = "x-qx-csrf";
/// 引导令牌的查询参数名。
pub const CONSOLE_TOKEN_QUERY: &str = "token";
/// 会话有效期缺省值（秒）。配置可覆盖。
pub const DEFAULT_CONSOLE_SESSION_TTL_SECONDS: u64 = 3600;
/// 这一层只发这三份静态资源；不在册的名字一律 404，路径穿越因此不可表达。
pub const CONSOLE_ASSETS: [&str; 3] = ["index.html", "app.js", "styles.css"];

/// 同时存在的会话数上限。超出按**签发顺序**退掉最老的一条（不是按字典序挑一个）。
const MAX_CONSOLE_SESSIONS: usize = 64;
/// 引导令牌最短长度：短到能被猜出来的秘密等于没有秘密。
const MIN_CONSOLE_TOKEN_LEN: usize = 16;
const CONSOLE_ACCEPT_POLL: Duration = Duration::from_millis(2);
const JSON_CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// 控制台的装配参数。`new` 是唯一的构造入口，把"配错了也要起得来"这条路堵死。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleConfig {
    pub static_dir: PathBuf,
    pub operator: String,
    pub bootstrap_token: String,
    pub session_ttl_ms: u64,
}

impl ConsoleConfig {
    pub fn new(
        static_dir: PathBuf,
        operator: String,
        bootstrap_token: String,
        session_ttl_seconds: u64,
    ) -> Result<Self, String> {
        if operator.trim().is_empty() {
            return Err("console.operator 不能为空：会话要把身份注入给谁必须写清楚".to_string());
        }
        if bootstrap_token.len() < MIN_CONSOLE_TOKEN_LEN {
            return Err(format!(
                "console 引导令牌太短（至少 {MIN_CONSOLE_TOKEN_LEN} 字符）：能被猜到的秘密等于没有秘密"
            ));
        }
        if session_ttl_seconds == 0 {
            return Err(
                "console.session_ttl_seconds 不能为 0：那等于签一个立刻过期的会话".to_string(),
            );
        }
        if !static_dir.is_dir() {
            return Err(format!(
                "console.static_dir 不是目录: {}",
                static_dir.display()
            ));
        }
        Ok(Self {
            static_dir,
            operator,
            bootstrap_token,
            session_ttl_ms: session_ttl_seconds.saturating_mul(1000),
        })
    }
}

/// 控制台监听地址必须是回环。单进程 BFF 没有独立反代那一层，一旦绑到非回环地址，
/// 会话就变成"谁能连上谁能拿"，而这一层**没有** TLS、没有 mTLS、没有运维审批。
pub fn console_bind_is_loopback(bind: &str) -> Result<(), String> {
    let address: SocketAddr = bind
        .parse()
        .map_err(|error| format!("console.bind 不是 host:port 形状: {bind}（{error}）"))?;
    if !address.ip().is_loopback() {
        return Err(format!(
            "console.bind 必须是回环地址（127.0.0.1 / ::1），实得 {}：\
             本实现没有独立反代与 TLS，绑到外部接口等于把控制面交给任何能连上的人",
            address.ip()
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConsoleSession {
    operator: String,
    csrf: String,
    expires_at_ms: u64,
    issued_at: u64,
}

#[derive(Default)]
struct ConsoleSessions {
    entries: BTreeMap<String, ConsoleSession>,
    issued: u64,
}

impl ConsoleSessions {
    fn issue(&mut self, config: &ConsoleConfig, now_ms: u64) -> (String, ConsoleSession) {
        self.issued += 1;
        let id = derive_secret(&config.bootstrap_token, "session", self.issued);
        let session = ConsoleSession {
            operator: config.operator.clone(),
            csrf: derive_secret(&config.bootstrap_token, "csrf", self.issued),
            expires_at_ms: now_ms.saturating_add(config.session_ttl_ms),
            issued_at: self.issued,
        };
        self.evict(now_ms);
        self.entries.insert(id.clone(), session.clone());
        (id, session)
    }

    fn lookup(&mut self, id: &str, now_ms: u64) -> Option<ConsoleSession> {
        self.evict(now_ms);
        self.entries.get(id).cloned()
    }

    fn evict(&mut self, now_ms: u64) {
        self.entries
            .retain(|_, session| session.expires_at_ms > now_ms);
        while self.entries.len() >= MAX_CONSOLE_SESSIONS {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, session)| session.issued_at)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.entries.remove(&victim);
        }
    }
}

/// 同源 BFF 本体：静态资源 + 会话 + CSRF + 进程内代理。
#[derive(Clone)]
pub struct ConsoleFront {
    service: ApiService,
    config: Arc<ConsoleConfig>,
    sessions: Arc<Mutex<ConsoleSessions>>,
    budget: Arc<ConnectionBudget>,
}

impl ConsoleFront {
    pub fn new(service: ApiService, config: ConsoleConfig) -> Result<Self, String> {
        Ok(Self {
            service,
            config: Arc::new(config),
            sessions: Arc::new(Mutex::new(ConsoleSessions::default())),
            budget: ConnectionBudget::new(DEFAULT_MAX_CONCURRENT_CONNECTIONS)?,
        })
    }

    /// 一条请求的完整判定，返回响应与要追加的头部（登录那一次是两条 `Set-Cookie`）。
    ///
    /// 纯函数式入口：不起连接、不读套接字，用例可以直接喂字符串进来钉住每一条拒绝。
    pub fn handle(&self, request: &str, now_ms: u64) -> (ApiResponse, Vec<(String, String)>) {
        let Ok(parsed) = parse_console_request(request) else {
            return (json_response(400, "console_bad_request"), Vec::new());
        };
        let (route, query) = parsed
            .target
            .split_once('?')
            .unwrap_or((parsed.target.as_str(), ""));
        let cookies = parse_cookies(parsed.headers.get("cookie").map_or("", String::as_str));
        let session = cookies
            .get(CONSOLE_SESSION_COOKIE)
            .and_then(|id| self.sessions.lock().unwrap().lookup(id, now_ms));

        if let Some(asset) = static_asset(route) {
            return match session {
                Some(_) => (self.read_asset(asset), Vec::new()),
                None => self.bootstrap(asset, query, now_ms),
            };
        }

        let Some(session) = session else {
            return (json_response(401, "console_session_required"), Vec::new());
        };
        if parsed.method != "GET" {
            if !self.csrf_matches(&parsed, &session) {
                return (json_response(403, "console_csrf_token_invalid"), Vec::new());
            }
            if !origin_matches_host(&parsed) {
                return (json_response(403, "console_origin_not_allowed"), Vec::new());
            }
        }
        let response = self.service.handle_inner(
            &parsed.method,
            parsed.target.as_str(),
            parsed.body,
            now_ms,
            Some(&session.operator),
        );
        (response, Vec::new())
    }

    fn csrf_matches(&self, parsed: &ConsoleRequest<'_>, session: &ConsoleSession) -> bool {
        let presented = parsed
            .headers
            .get(CONSOLE_CSRF_HEADER)
            .map_or("", String::as_str);
        !presented.is_empty() && presented == session.csrf
    }

    /// 没有会话时唯一能走通的一条路：带对引导令牌换一份会话。
    fn bootstrap(
        &self,
        asset: &str,
        query: &str,
        now_ms: u64,
    ) -> (ApiResponse, Vec<(String, String)>) {
        let Some(presented) = query_param(query, CONSOLE_TOKEN_QUERY) else {
            return (
                json_response(401, "console_bootstrap_token_required"),
                Vec::new(),
            );
        };
        if presented != self.config.bootstrap_token {
            return (
                json_response(403, "console_bootstrap_token_invalid"),
                Vec::new(),
            );
        }
        let (id, session) = self.sessions.lock().unwrap().issue(&self.config, now_ms);
        let max_age = self.config.session_ttl_ms / 1000;
        let cookies = vec![
            (
                "Set-Cookie".to_string(),
                format!(
                    "{CONSOLE_SESSION_COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}"
                ),
            ),
            (
                "Set-Cookie".to_string(),
                format!(
                    "{CONSOLE_CSRF_COOKIE}={}; Path=/; SameSite=Strict; Max-Age={max_age}",
                    session.csrf
                ),
            ),
        ];
        (self.read_asset(asset), cookies)
    }

    fn read_asset(&self, name: &str) -> ApiResponse {
        match std::fs::read_to_string(self.config.static_dir.join(name)) {
            Ok(body) => ApiResponse {
                status: 200,
                content_type: asset_content_type(name).to_string(),
                body,
            },
            Err(_) => json_response(503, "console_asset_unavailable"),
        }
    }

    /// 持续接受连接，直到 `stopped()` 为真（与 `ApiService::serve` 同一条收摊口径）。
    pub fn serve(
        &self,
        listener: TcpListener,
        mut now: impl FnMut() -> u64,
        stopped: impl Fn() -> bool,
    ) -> std::io::Result<()> {
        listener.set_nonblocking(true)?;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    configure_connection(&stream)?;
                    match self.budget.acquire() {
                        Some(guard) => self.spawn_connection(stream, now(), guard),
                        None => refuse_connection(&stream, "console_connection_budget_exhausted"),
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if stopped() {
                        return Ok(());
                    }
                    std::thread::sleep(CONSOLE_ACCEPT_POLL);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn spawn_connection(&self, stream: TcpStream, now_ms: u64, guard: ConnectionGuard) {
        let front = self.clone();
        std::thread::spawn(move || {
            // 额度随这条会话线程一起活，线程怎么退出都归还（与 API 那条 loop 同口径）。
            let _guard = guard;
            if let Err(error) = front.serve_stream(stream, now_ms) {
                eprintln!("[qx-api] 控制台会话结束: {error}");
            }
        });
    }

    fn serve_stream(&self, mut stream: TcpStream, now_ms: u64) -> std::io::Result<()> {
        let request = read_request(&mut stream)?;
        let request = String::from_utf8_lossy(&request);
        let (response, extra) = self.handle(&request, now_ms);
        write_http_response(&mut stream, &response, &extra)
    }

    /// 同步处理单条连接（不起线程），供本 crate 用例钉住确定性的请求/响应顺序。
    #[cfg(test)]
    pub(crate) fn serve_once(&self, listener: &TcpListener, now_ms: u64) -> std::io::Result<()> {
        let (stream, _) = listener.accept()?;
        configure_connection(&stream)?;
        self.serve_stream(stream, now_ms)
    }
}

fn refuse_connection(stream: &TcpStream, code: &'static str) {
    let mut writer = stream;
    if let Err(error) = write_http_response(&mut writer, &json_response(503, code), &[]) {
        eprintln!("[qx-api] 控制台写出拒绝响应失败: {error}");
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

struct ConsoleRequest<'a> {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: &'a str,
}

fn parse_console_request(request: &str) -> Result<ConsoleRequest<'_>, String> {
    let (head, body) = request
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed http request".to_string())?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().ok_or_else(|| "missing method".to_string())?;
    let target = first.next().ok_or_else(|| "missing path".to_string())?;
    let mut headers = BTreeMap::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    Ok(ConsoleRequest {
        method: method.to_string(),
        target: target.to_string(),
        headers,
        body,
    })
}

/// 静态路由只认这三份资源（`/` 等价于 `/index.html`）。名字逐个相等，不做路径拼接，
/// 所以 `../` 这类穿越在这张表上不可表达。
fn static_asset(path: &str) -> Option<&'static str> {
    let name = path.strip_prefix('/')?;
    let name = if name.is_empty() { "index.html" } else { name };
    CONSOLE_ASSETS.iter().copied().find(|asset| *asset == name)
}

fn asset_content_type(name: &str) -> &'static str {
    match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn parse_cookies(header: &str) -> BTreeMap<String, String> {
    let mut cookies = BTreeMap::new();
    for pair in header.split(';') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        cookies.insert(name.trim().to_string(), value.trim().to_string());
    }
    cookies
}

/// 查询串取一个键。**不做百分号解码**：引导令牌是十六进制串，编码后与原文相同；
/// 需要解码的调用方是路由分派那一条（`admission::percent_decode`），不是这里。
fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

/// 带 `Origin` 的请求必须与 `Host` 同源。同源页面发的 POST 会带 `Origin`，
/// 而**另一个站点**诱导发起的请求带的是它自己的源——这一格把后者挡在写面之外，
/// 与 CSRF 头是两道独立的锁（双提交 + 同源校验）。
fn origin_matches_host(parsed: &ConsoleRequest<'_>) -> bool {
    let Some(origin) = parsed.headers.get("origin") else {
        return true;
    };
    let Some(host) = parsed.headers.get("host") else {
        return false;
    };
    origin
        .split_once("://")
        .is_some_and(|(_, authority)| authority.eq_ignore_ascii_case(host))
}

fn json_response(status: u16, code: &'static str) -> ApiResponse {
    ApiResponse {
        status,
        content_type: JSON_CONTENT_TYPE.to_string(),
        body: format!("{{\"error\":\"{code}\"}}"),
    }
}

/// 由引导令牌派生的伪随机值：秘密 + 标签 + 计数器 → SHA-1 十六进制。
/// 秘密不在进程外暴露，派生值因此不可预测；`label` 让会话 id 与 CSRF 走两条互不相干的流。
fn derive_secret(secret: &str, label: &str, counter: u64) -> String {
    let mut input = Vec::with_capacity(secret.len() + label.len() + 16);
    input.extend_from_slice(secret.as_bytes());
    input.extend_from_slice(b"|");
    input.extend_from_slice(label.as_bytes());
    input.extend_from_slice(b"|");
    input.extend_from_slice(&counter.to_be_bytes());
    sha1(&input)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApiPolicy, ApiState};
    use qx_control::Permission;

    const TOKEN: &str = "0123456789abcdef";

    /// 静态资源目录按 crate 位置回推：`crates/qx-api` → `crates` → 仓库根 → `web/console`。
    fn console_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("web")
            .join("console")
    }

    fn front() -> ConsoleFront {
        let service = ApiService::with_policy(
            ApiState::default(),
            ApiPolicy::new().grant("console-operator", Permission::Trading),
        );
        let config = ConsoleConfig::new(
            console_dir(),
            "console-operator".to_string(),
            TOKEN.to_string(),
            DEFAULT_CONSOLE_SESSION_TTL_SECONDS,
        )
        .expect("控制台配置");
        ConsoleFront::new(service, config).expect("控制台")
    }

    fn request(method: &str, target: &str, extra: &str) -> String {
        format!("{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:18082\r\n{extra}Content-Length: 0\r\n\r\n")
    }

    fn set_cookies(extra: &[(String, String)]) -> BTreeMap<String, String> {
        let joined = extra
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, value)| value.split(';').next().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("; ");
        parse_cookies(&joined)
    }

    #[test]
    fn a_page_without_a_session_asks_for_the_bootstrap_token() {
        let (response, extra) = front().handle(&request("GET", "/", ""), 1_000);
        assert_eq!(response.status, 401);
        assert!(response.body.contains("console_bootstrap_token_required"));
        assert!(extra.is_empty(), "没有会话时不许发任何 cookie");
    }

    #[test]
    fn a_wrong_bootstrap_token_is_refused_and_issues_nothing() {
        let (response, extra) =
            front().handle(&request("GET", "/?token=deadbeefdeadbeef", ""), 1_000);
        assert_eq!(response.status, 403);
        assert!(response.body.contains("console_bootstrap_token_invalid"));
        assert!(extra.is_empty());
    }

    #[test]
    fn the_right_token_issues_an_httponly_strict_session_and_a_readable_csrf_cookie() {
        let (response, extra) =
            front().handle(&request("GET", &format!("/?token={TOKEN}"), ""), 1_000);
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "text/html; charset=utf-8");
        assert!(
            response.body.contains("<html"),
            "登录成功必须真的把页面发出去，而不是只发 cookie"
        );
        let cookies = set_cookies(&extra);
        assert_eq!(cookies.len(), 2, "会话与 CSRF 各一枚: {extra:?}");
        let session_cookie = extra
            .iter()
            .find(|(_, value)| value.starts_with(CONSOLE_SESSION_COOKIE))
            .expect("会话 cookie");
        assert!(session_cookie.1.contains("HttpOnly"));
        assert!(session_cookie.1.contains("SameSite=Strict"));
        let csrf_cookie = extra
            .iter()
            .find(|(_, value)| value.starts_with(CONSOLE_CSRF_COOKIE))
            .expect("CSRF cookie");
        assert!(
            !csrf_cookie.1.contains("HttpOnly"),
            "CSRF cookie 必须能被页面读到，否则双提交无从发起"
        );
    }

    #[test]
    fn a_write_without_the_csrf_header_is_refused() {
        let front = front();
        let (_, extra) = front.handle(&request("GET", &format!("/?token={TOKEN}"), ""), 1_000);
        let cookies = set_cookies(&extra);
        let cookie = format!(
            "Cookie: {CONSOLE_SESSION_COOKIE}={}; {CONSOLE_CSRF_COOKIE}={}\r\n",
            cookies[CONSOLE_SESSION_COOKIE], cookies[CONSOLE_CSRF_COOKIE]
        );
        let (response, _) = front.handle(&request("POST", "/control/commands", &cookie), 1_000);
        assert_eq!(response.status, 403);
        assert!(response.body.contains("console_csrf_token_invalid"));
    }

    #[test]
    fn a_write_with_the_wrong_csrf_header_is_refused() {
        let front = front();
        let (_, extra) = front.handle(&request("GET", &format!("/?token={TOKEN}"), ""), 1_000);
        let cookies = set_cookies(&extra);
        let cookie = format!(
            "Cookie: {CONSOLE_SESSION_COOKIE}={}\r\n",
            cookies[CONSOLE_SESSION_COOKIE]
        );
        let (response, _) = front.handle(
            &request(
                "POST",
                "/control/commands",
                &format!("{cookie}{CONSOLE_CSRF_HEADER}: 0000\r\n"),
            ),
            1_000,
        );
        assert_eq!(response.status, 403);
        assert!(response.body.contains("console_csrf_token_invalid"));
    }

    #[test]
    fn a_cross_site_origin_is_refused_even_with_the_right_csrf_header() {
        let front = front();
        let (_, extra) = front.handle(&request("GET", &format!("/?token={TOKEN}"), ""), 1_000);
        let cookies = set_cookies(&extra);
        let csrf = cookies[CONSOLE_CSRF_COOKIE].clone();
        let cookie = format!(
            "Cookie: {CONSOLE_SESSION_COOKIE}={}\r\n",
            cookies[CONSOLE_SESSION_COOKIE]
        );
        let (response, _) = front.handle(
            &request(
                "POST",
                "/control/commands",
                &format!(
                    "{cookie}Origin: http://evil.example\r\n{CONSOLE_CSRF_HEADER}: {csrf}\r\n"
                ),
            ),
            1_000,
        );
        assert_eq!(response.status, 403);
        assert!(response.body.contains("console_origin_not_allowed"));
    }

    #[test]
    fn a_session_request_reaches_the_api_with_the_server_side_operator() {
        let front = front();
        let (_, extra) = front.handle(&request("GET", &format!("/?token={TOKEN}"), ""), 1_000);
        let cookies = set_cookies(&extra);
        let cookie = format!(
            "Cookie: {CONSOLE_SESSION_COOKIE}={}\r\n",
            cookies[CONSOLE_SESSION_COOKIE]
        );
        // `/health` 在策略外，换成受策略保护的那一条才验得出身份真的注入了。
        let (open, _) = front.handle(&request("GET", "/health", &cookie), 1_000);
        assert_eq!(open.status, 200);
        let (guarded, _) = front.handle(&request("GET", "/control/audit", &cookie), 1_000);
        assert_eq!(
            guarded.status, 200,
            "服务端注入的 operator 必须带着权限，否则受策略保护的读面会 403: {}",
            guarded.body
        );
    }

    #[test]
    fn an_expired_session_is_refused_without_touching_the_api() {
        let front = front();
        let (_, extra) = front.handle(&request("GET", &format!("/?token={TOKEN}"), ""), 1_000);
        let cookies = set_cookies(&extra);
        let cookie = format!(
            "Cookie: {CONSOLE_SESSION_COOKIE}={}\r\n",
            cookies[CONSOLE_SESSION_COOKIE]
        );
        let later = 1_000 + DEFAULT_CONSOLE_SESSION_TTL_SECONDS * 1000 + 1;
        let (response, _) = front.handle(&request("GET", "/health", &cookie), later);
        assert_eq!(response.status, 401);
    }

    #[test]
    fn a_bogus_session_cookie_is_refused() {
        let (response, _) = front().handle(
            &request(
                "GET",
                "/health",
                &format!("Cookie: {CONSOLE_SESSION_COOKIE}=not-a-real-session\r\n"),
            ),
            1_000,
        );
        assert_eq!(response.status, 401);
        assert!(response.body.contains("console_session_required"));
    }

    #[test]
    fn only_the_three_registered_assets_are_reachable() {
        let front = front();
        let (_, extra) = front.handle(&request("GET", &format!("/?token={TOKEN}"), ""), 1_000);
        let cookies = set_cookies(&extra);
        let cookie = format!(
            "Cookie: {CONSOLE_SESSION_COOKIE}={}\r\n",
            cookies[CONSOLE_SESSION_COOKIE]
        );
        for asset in CONSOLE_ASSETS {
            let (response, _) = front.handle(&request("GET", &format!("/{asset}"), &cookie), 1_000);
            assert_eq!(response.status, 200, "{asset} 必须发得出去");
        }
        for probe in ["/../Cargo.toml", "/console.rs", "/deploy/README.md"] {
            let (response, _) = front.handle(&request("GET", probe, &cookie), 1_000);
            assert_eq!(response.status, 404, "{probe} 不该被这一层发出去");
        }
    }

    #[test]
    fn console_config_refuses_configurations_that_would_ship_a_weak_boundary() {
        let dir = console_dir();
        assert!(ConsoleConfig::new(dir.clone(), String::new(), TOKEN.into(), 60).is_err());
        assert!(ConsoleConfig::new(dir.clone(), "op".into(), "short".into(), 60).is_err());
        assert!(ConsoleConfig::new(dir.clone(), "op".into(), TOKEN.into(), 0).is_err());
        assert!(ConsoleConfig::new(dir.join("nope"), "op".into(), TOKEN.into(), 60).is_err());
        assert!(ConsoleConfig::new(dir, "op".into(), TOKEN.into(), 60).is_ok());
    }

    #[test]
    fn only_loopback_bindings_are_accepted() {
        assert!(console_bind_is_loopback("127.0.0.1:18082").is_ok());
        assert!(console_bind_is_loopback("[::1]:18082").is_ok());
        assert!(console_bind_is_loopback("0.0.0.0:18082").is_err());
        assert!(console_bind_is_loopback("10.0.0.7:18082").is_err());
        assert!(console_bind_is_loopback("not-an-address").is_err());
    }

    /// 真实套接字上的一条往返：`handle` 的判定与传输层（请求读取、状态行、连接选项）接起来之后
    /// 仍然是同一个答案。只喂字符串进来测 `handle` 的话，"读不到请求 / 写不出响应"这两段永远
    /// 不被走到——而它们正是控制台面唯一与网络打交道的部分。
    #[test]
    fn one_accepted_connection_is_answered_end_to_end_over_a_real_socket() {
        use std::io::{Read, Write};
        let front = front();
        let listener = TcpListener::bind("127.0.0.1:0").expect("监听回环");
        let address = listener.local_addr().expect("监听地址");
        let worker = std::thread::spawn(move || front.serve_once(&listener, 1_000));
        let mut client = TcpStream::connect(address).expect("连接控制台");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .expect("写请求");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("读响应");
        worker.join().expect("会话线程").expect("处理一条连接");
        assert!(
            response.starts_with("HTTP/1.1 401 Unauthorized"),
            "没有会话的首页必须在真实套接字上也是 401，且状态行有名字: {response}"
        );
    }
}
