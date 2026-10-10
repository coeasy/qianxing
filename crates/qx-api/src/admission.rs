//! 浏览器准入面：请求行与头部的解析、CORS 精确 allowlist 与预检、并发连接预算、
//! 查询串的百分号解码。
//!
//! 与 `transport.rs` 的分工是：那边搬字节，这边做**准入判定**。这四件事此前一件都没有，
//! 而它们都是"前后端贯通"的必要条件——本仓库里没有任何前端（全仓没有一份 `package.json`），
//! 所以贯通的边界就是这份读面能不能被一个浏览器直接消费：
//!
//! - 跨源读要求响应里回一个**精确**的 `Access-Control-Allow-Origin`。回 `*` 等于把 mTLS
//!   之外那道身份判定交给任意站点，所以 allowlist 只认逐字符相等的源，配置里写 `*` 当场拒。
//! - 浏览器在跨源读之前先发一条 `OPTIONS` 预检。读面没有 `OPTIONS` 路由，预检会落 404
//!   兜底，于是每一次跨源调用都在预检阶段就死了，而 404 的正文看起来像"这条入口不存在"。
//! - 每条连接一个线程且没有计数：半开连接（对端不发 FIN）永远不会读到 EOF，会话线程
//!   也就永远不退，几十条就能把进程钉死。预算与 503 是这条通道唯一的出口。
//! - `query_value` 过去把 `?account_id=a%20b` 原样当成 `a%20b` 去查投影，读不到就报
//!   `account_projection_not_found`：客户端看不出是**自己的编码没被解开**，只会以为没有这个账户。
//!
//! 升级判定同样住在这里，因为它是一条准入判定而不是字节搬运：过去按**整份请求文本**做
//! `contains("upgrade: websocket")`，正文里恰好带这串字符的 `POST /control/commands`
//! 会被劫持成握手，命令体连同审计一起消失。

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 头部区（请求行 + 各头部，不含正文）：按第一处空行截断。
///
/// 没有空行时整份文本都算头部区——`read_request` 已经保证读到 `\r\n\r\n` 才返回，
/// 这一支只是让本模块单独被调用时也不至于把正文当头部。
fn header_section(request: &str) -> &str {
    match request.find("\r\n\r\n") {
        Some(at) => &request[..at],
        None => request,
    }
}

/// 请求行的 `(方法, 目标)`。目标含查询串，调用方自己按 `?` 切。
pub(crate) fn parse_request_line(request: &str) -> Option<(&str, &str)> {
    let line = header_section(request).lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    Some((method, target))
}

/// 目标切成 `(路径, 查询串)`，与 `handle_inner` 的 `path.split_once('?')` 同口径。
pub(crate) fn split_target(target: &str) -> (&str, &str) {
    target.split_once('?').unwrap_or((target, ""))
}

/// 一个头部名的值（大小写不敏感，取第一处，已 trim）。
pub(crate) fn header_value(request: &str, name: &str) -> Option<String> {
    header_section(request).lines().skip(1).find_map(|line| {
        let (head, value) = line.split_once(':')?;
        head.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

/// 这份请求是不是 WebSocket 升级：只看 `Upgrade` 头部，不看正文。
pub(crate) fn is_websocket_upgrade(request: &str) -> bool {
    header_value(request, "upgrade").is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

/// 跨源准入策略：一份**精确**源 allowlist。
///
/// 只认 `http://host[:port]` 与 `https://host[:port]` 这一种形状，逐字符相等才算命中：
/// 通配（`*`、`https://*.example.com`）、带路径、带尾斜杠、带查询或片段一律在配置装载时
/// 就拒，不留到运行时靠"看起来像"去猜。IPv6 字面量（`http://[::1]:8080`）同样拒——
/// 主机名里出现第二个冒号就不是这条口径能判定的形状，本机部署写 `http://localhost:8080`
/// 或 `http://127.0.0.1:8080`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CorsPolicy {
    allowed: BTreeSet<String>,
}

/// 预检可以带的方法：与 `handle_inner` 真分派的那两个方法一致，`OPTIONS` 是预检自己。
const PREFLIGHT_ALLOW_METHODS: &str = "GET, POST, OPTIONS";
/// 预检允许客户端带的头部。
///
/// 写成定值而不是回显 `Access-Control-Request-Headers`：回显等于把客户端给的任意字符串
/// 写进响应头，而本读面不从头部取任何身份（operator 身份只来自 mTLS 证书），需要放行的
/// 就只有正文类型这一格。
const PREFLIGHT_ALLOW_HEADERS: &str = "Content-Type";
/// 预检结果的浏览器侧缓存秒数。
const PREFLIGHT_MAX_AGE: &str = "600";

impl CorsPolicy {
    /// 从配置装载。任一条源不合口径就整份拒绝，不做"跳过坏的留好的"：
    /// 一份被静默削减的 allowlist 会让某个前端在生产上跨源失败，而配置侧看不出原因。
    pub(crate) fn parse(origins: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut allowed = BTreeSet::new();
        for origin in origins {
            Self::validate(&origin)?;
            allowed.insert(origin);
        }
        if allowed.is_empty() {
            return Err("cors_allowed_origins 不能为空：要开浏览器准入就至少给一个源".to_string());
        }
        Ok(Self { allowed })
    }

    fn validate(origin: &str) -> Result<(), String> {
        let reject = |why: &str| Err(format!("cors_allowed_origins 里的 {origin:?} {why}"));
        if origin != origin.trim() {
            return reject("首尾带空白");
        }
        if origin == "*" {
            return reject("是通配：跨源准入只认精确源，`*` 等于取消这道判定");
        }
        let rest = if let Some(rest) = origin.strip_prefix("https://") {
            rest
        } else if let Some(rest) = origin.strip_prefix("http://") {
            rest
        } else {
            return reject("不以 http:// 或 https:// 开头");
        };
        if rest.is_empty() {
            return reject("缺少主机名");
        }
        if rest.contains(['/', '?', '#', '@', '\\']) || rest.chars().any(|c| c.is_whitespace()) {
            return reject("带了路径、查询、片段、凭据或空白：源只到端口为止");
        }
        if rest.contains('*') {
            return reject("带通配符");
        }
        let (host, port) = match rest.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (rest, None),
        };
        if host.is_empty()
            || !host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            return reject("主机名不是 DNS 名或 IPv4 字面量（IPv6 字面量不在这一口径内）");
        }
        if let Some(port) = port {
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return reject("端口不是十进制数");
            }
            match port.parse::<u16>() {
                Ok(0) => return reject("端口是 0"),
                Ok(_) => {}
                Err(_) => return reject("端口超出 16 位"),
            }
        }
        Ok(())
    }

    /// 这份请求的源在不在 allowlist 里。
    fn allows(&self, origin: Option<&str>) -> bool {
        origin.is_some_and(|origin| self.allowed.contains(origin))
    }

    /// WebSocket 升级的跨源判定：浏览器**不**把 CORS 用在 WS 握手上，它照样发出带任意
    /// `Origin` 的 `Upgrade` 请求，只在响应侧拦。所以配了 allowlist 的部署若不在写下 `101`
    /// 之前自己问一次名单，HTTP 侧那份名单就挡不住任何跨源页面读事件流（CSWSH）。
    ///
    /// 不报 `Origin` 的照常放行：那是不带源语义的原生客户端（CLI、探测脚本、代理），本来
    /// 就不在 CORS 的威胁模型内——同一份数据它们走 HTTP 也照样读得到。把这一支改成"没源就
    /// 拒"会先杀掉仓内自己的 WS 客户端与用例，而拦不住任何浏览器。
    pub(crate) fn websocket_denied(&self, request: &str) -> bool {
        header_value(request, "origin").is_some_and(|origin| !self.allows(Some(&origin)))
    }

    /// 普通（非预检）响应要追加的头。
    ///
    /// `Vary: Origin` 在**配了策略时无条件下发**，包括源没命中的那一支：响应随 `Origin`
    /// 而变，中间缓存不按源分开存就会把 A 站的 `Allow-Origin` 发给 B 站。
    pub(crate) fn response_headers(&self, request: &str) -> Vec<(String, String)> {
        let mut headers = vec![("Vary".to_string(), "Origin".to_string())];
        if let Some(origin) = header_value(request, "origin") {
            if self.allows(Some(&origin)) {
                headers.push(("Access-Control-Allow-Origin".to_string(), origin));
            }
        }
        headers
    }

    /// 预检判定：命中 allowlist 才给放行头，否则调用方回 403。
    pub(crate) fn preflight_headers(&self, request: &str) -> Option<Vec<(String, String)>> {
        let origin = header_value(request, "origin")?;
        if !self.allows(Some(&origin)) {
            return None;
        }
        Some(vec![
            ("Vary".to_string(), "Origin".to_string()),
            ("Access-Control-Allow-Origin".to_string(), origin),
            (
                "Access-Control-Allow-Methods".to_string(),
                PREFLIGHT_ALLOW_METHODS.to_string(),
            ),
            (
                "Access-Control-Allow-Headers".to_string(),
                PREFLIGHT_ALLOW_HEADERS.to_string(),
            ),
            (
                "Access-Control-Max-Age".to_string(),
                PREFLIGHT_MAX_AGE.to_string(),
            ),
        ])
    }
}

/// 预检的三态。`NotConfigured` 与 `Rejected` 必须分开：前者是"这份部署没开浏览器准入"，
/// `OPTIONS` 应当照常走分派落到 404 兜底；后者是"开了、而这个源不在名单里"，要说 403。
pub(crate) enum Preflight {
    NotConfigured,
    Allowed { headers: Vec<(String, String)> },
    Rejected,
}

/// 一条 `OPTIONS` 请求的处置。只有同时具备"配了策略 + 方法是 OPTIONS + 带
/// `Access-Control-Request-Method`（预检的标志）"才算预检；不带最后那一格的 `OPTIONS`
/// 是普通请求，交给分派去落 404。
pub(crate) fn preflight(policy: Option<&CorsPolicy>, request: &str) -> Preflight {
    let Some(policy) = policy else {
        return Preflight::NotConfigured;
    };
    let Some((method, _)) = parse_request_line(request) else {
        return Preflight::NotConfigured;
    };
    if !method.eq_ignore_ascii_case("OPTIONS")
        || header_value(request, "access-control-request-method").is_none()
    {
        return Preflight::NotConfigured;
    }
    match policy.preflight_headers(request) {
        Some(headers) => Preflight::Allowed { headers },
        None => Preflight::Rejected,
    }
}

/// 并发连接预算的默认上限。
///
/// 一条连接一个线程（`spawn_connection`），所以上限同时是线程数上限。256 与
/// `deploy/README.md` 里那条浏览器准入段说的是同一个数。
pub(crate) const DEFAULT_MAX_CONCURRENT_CONNECTIONS: usize = 256;

/// 并发连接预算：飞行中的连接数不得超过 `limit`，超出的当场 503 而不是排队。
///
/// 排队等于把"拒绝"变成"更慢的接受"，而这条通道上排队的连接照样各占一个已 accept 的
/// 套接字与一份读缓冲，压力不会因为排队而消失。
pub(crate) struct ConnectionBudget {
    limit: usize,
    live: AtomicUsize,
}

/// 一个占位。`Drop` 时归还，所以会话线程无论怎么退出（正常关闭、写失败、停机令牌、
/// 空闲上界）都不会把额度留在账上。
pub(crate) struct ConnectionGuard {
    budget: Arc<ConnectionBudget>,
}

impl ConnectionBudget {
    pub(crate) fn new(limit: usize) -> Result<Arc<Self>, String> {
        if limit == 0 {
            return Err("max_concurrent_connections 不能为 0：那等于不接任何连接".to_string());
        }
        Ok(Arc::new(Self {
            limit,
            live: AtomicUsize::new(0),
        }))
    }

    pub(crate) fn acquire(self: &Arc<Self>) -> Option<ConnectionGuard> {
        // 先读后加会放过并发超额，所以用 CAS 循环：读到的值只有在自己写回时才算数。
        let mut live = self.live.load(Ordering::Acquire);
        loop {
            if live >= self.limit {
                return None;
            }
            match self.live.compare_exchange_weak(
                live,
                live + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ConnectionGuard {
                        budget: Arc::clone(self),
                    })
                }
                Err(actual) => live = actual,
            }
        }
    }

    /// 飞行中的连接数与上限，给 503 的正文用：客户端拿到的是"这份部署的上限是多少"，
    /// 而不是一个没有分母的"忙"。
    ///
    /// 这里**不新增第五条 `/metrics` 样本**，是有意的：那四条样本的名单被
    /// `deploy/README.md` 与读侧用例逐条点名，多一条要连文档与用例一起改口；而连接压力
    /// 在这份构建里已经有出口——超限即 503，正文带上限，stderr 各打一行。
    pub(crate) fn occupancy(&self) -> (usize, usize) {
        (self.live.load(Ordering::Acquire), self.limit)
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.budget.live.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 浏览器准入这两格配置的体检入口：`config validate` 调它，`serve` 装配时走的是同一份实现。
///
/// 单开一个入口是分层逼出来的：口径住在 `qx-api`，而 `qx-runtime` 的配置校验不许反向依赖它，
/// 于是由同样看得见两侧的 `qx-cli` 在 `config validate` 里调这一份。此前那两格谁都不看，
/// 坏源与 0 上限要等到 `serve` 起不来才现身，而 `config validate` 正是部署前唯一能把这类
/// 问题一次列全的命令。这里不复制规则，只把已有的两个构造点各调一次。
pub fn validate_admission_config(
    cors_allowed_origins: &[String],
    max_concurrent_connections: Option<usize>,
) -> Result<(), String> {
    if !cors_allowed_origins.is_empty() {
        CorsPolicy::parse(cors_allowed_origins.iter().cloned())?;
    }
    if let Some(limit) = max_concurrent_connections {
        ConnectionBudget::new(limit)?;
    }
    Ok(())
}

/// 百分号解码失败的口径（`error_json` 的正文）。
///
/// 写成一句英文而不是一颗小写码名，是有意的：`crates/qx-cli/src/tests/api_endpoint_table_routes.rs`
/// 的 `emitted_error_names` 把 `error_json("` 之后连续 6 个以上的小写下划线串当成对外承诺的
/// 稳定码名，逐条要求文档里有名字。这一格说的是"你给的查询串解不开"，与
/// `base_hash is required` 同一族（都是请求形状问题），不该占一颗码名。
pub(crate) const QUERY_DECODE_ERROR: &str = "query string percent-encoding is malformed";

/// `application/x-www-form-urlencoded` 的解码：`%XX` 还原成字节，`+` 还原成空格。
///
/// 解出来的字节序列必须是合法 UTF-8，否则整条查询串判非法——读面的每个键值都要参与
/// 字符串比较（`account_id`、`venue_id`、`base_hash`），拿一段非法 UTF-8 去比只会得到
/// "没有这个账户"这种听起来像业务结论的假话。
pub(crate) fn percent_decode(value: &str) -> Result<String, ()> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let high = hex_digit(*bytes.get(index + 1).ok_or(())?).ok_or(())?;
                let low = hex_digit(*bytes.get(index + 2).ok_or(())?).ok_or(())?;
                out.push(high * 16 + low);
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 查询串里一个键的值（已解码）。
///
/// 与原先那份 `query_value` 的差别有两处，两处都是判据：`+` 与 `%XX` 会被解开，
/// 而解不开时返回 `Err` 让调用方回 400，不再是"当成字面量去查、查不到报 404"。
pub(crate) fn query_param(query: &str, key: &str) -> Result<Option<String>, &'static str> {
    for part in query.split('&') {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        if name != key {
            continue;
        }
        return percent_decode(value)
            .map(Some)
            .map_err(|()| QUERY_DECODE_ERROR);
    }
    Ok(None)
}

/// 这条入口不认的那把查询键，按路由取名单；不在任何名册里的路径（`/health` 那一类）不判。
///
/// 名单本身住在 `read_scope`（读面分类的单点），这里只负责"名单外的键当场点名"：`?acount_id=`
/// 拼错时它会落到"没有收窄键"那一支，默认账户那份被念成调用方点名的账户（V13 R6 / R31）。
/// 名单外的键当场 400 并点名那把键，就是把这条读面的口径从"查不到就算了"改成"你没说清就别读"。
pub(crate) fn refused_query_param(route: &str, query: &str) -> Option<String> {
    let accepted = crate::read_scope::accepted_query_params(route)?;
    refused_param(query, accepted)
}

/// WS 那一支不按路径查表：`Upgrade: websocket` 可以从任何路径进来，认的键与 `/events` 同宽。
pub(crate) fn refused_event_query_param(query: &str) -> Option<String> {
    refused_param(query, crate::read_scope::event_scoped_params())
}

fn refused_param(query: &str, accepted: &[&str]) -> Option<String> {
    for part in query.split('&').filter(|part| !part.is_empty()) {
        let name = part.split('=').next().unwrap_or(part);
        if !accepted.contains(&name) {
            return Some(name.to_string());
        }
    }
    None
}
