//! 同源 BFF 写面的身份：审计里的 operator 必须是**服务端会话**里那一个，不是命令体里自称的那一个。
//!
//! V13 R25 用 `qx-cli console` + `qx-cli console --init` 的出厂模板做端到端实测时抓到这条：身份覆盖
//! 原先住在 `submit_command` 里 `match &self.policy` 的 `Some(policy)` 臂中，于是
//! `api.operators: {}`（模板与 `--init` 的默认形状，也是本机试用的常见形状）这一条路上**永远走不到**
//! 那句赋值——页面表单里的 `operator_id` 原样进审计，客户端可以自选把命令记在谁名下。而控制台那一层
//! 的文档与门禁判据都写着「身份由服务端按会话注入，不读命令体里那个可以随便填的字段」。
//!
//! 这条用例走**公开面**（`ConsoleFront::handle`），不经 `handle_as`，所以它钉的是运维真正启动的那条
//! 链：引导换会话 → 带 CSRF 双提交写一条命令 → 从 `/control/audit` 回读身份。它刻意用
//! `ApiService::new`（**没有** operator 名册），因为 `src/console.rs` 里既有的用例全都配了名册，
//! 恰好是缺陷藏得最深的那一侧。住在 `tests/` 而不是内联 `mod tests`：`crates/qx-api/src/lib.rs`
//! 正压在自己的行数预算上。

use std::path::PathBuf;

use qx_api::{
    ConsoleConfig, ConsoleFront, CONSOLE_CSRF_COOKIE, CONSOLE_CSRF_HEADER, CONSOLE_TOKEN_QUERY,
};

const TOKEN: &str = "console-identity-boundary-bootstrap-token";
const OPERATOR: &str = "console-operator";
const FORGED: &str = "someone-else";

fn console_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("web")
        .join("console")
}

/// 没有 operator 名册的一份控制台——`--init` 模板就是这个形状。
fn front() -> ConsoleFront {
    let service = qx_api::ApiService::new(qx_api::ApiState::default());
    let config = ConsoleConfig::new(
        console_dir(),
        OPERATOR.to_string(),
        TOKEN.to_string(),
        qx_api::DEFAULT_CONSOLE_SESSION_TTL_SECONDS,
    )
    .expect("控制台配置");
    ConsoleFront::new(service, config).expect("控制台")
}

fn request(method: &str, target: &str, extra: &str, body: &str) -> String {
    format!(
        "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:18091\r\n{extra}\
         Content-Type: application/json\r\n\r\n{body}"
    )
}

/// 引导一次，返回要带回的 `Cookie:` 头与 CSRF 头值。
fn session(front: &ConsoleFront) -> (String, String) {
    let (page, issued) = front.handle(
        &request("GET", &format!("/?{CONSOLE_TOKEN_QUERY}={TOKEN}"), "", ""),
        1_000,
    );
    assert_eq!(page.status, 200, "引导必须换来页面: {}", page.body);
    let mut cookies = Vec::new();
    let mut csrf = String::new();
    for (name, value) in &issued {
        if !name.eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        let pair = value.split(';').next().unwrap_or("").to_string();
        if let Some((key, tail)) = pair.split_once('=') {
            if key == CONSOLE_CSRF_COOKIE {
                csrf = tail.to_string();
            }
            cookies.push(format!("{key}={tail}"));
        }
    }
    assert!(!csrf.is_empty(), "引导必须签出 CSRF cookie");
    assert_eq!(
        cookies.len(),
        2,
        "应当同时签出会话与 CSRF 两枚 cookie，读到 {cookies:?}"
    );
    (
        format!("Cookie: {}\r\n", cookies.join("; ")),
        format!("{CONSOLE_CSRF_HEADER}: {csrf}\r\n"),
    )
}

fn command_body(id: u64, request_id: &str) -> String {
    // PauseStrategy 在本构建里确有派发者，所以这条路能走到 202 而不是被受理面拒掉。
    format!(
        "{{\"command_id\":{id},\"request_id\":\"{request_id}\",\"operator_id\":\"{FORGED}\",\
         \"reason\":\"identity boundary\",\"kind\":\"PauseStrategy\",\"target\":\"strategy-1\",\
         \"payload\":{{}},\"permission\":\"Trading\",\"dry_run\":true}}"
    )
}

#[test]
fn the_bff_writes_the_session_operator_into_audit_not_the_claimed_one() {
    let front = front();
    let (cookie, csrf) = session(&front);
    let accepted = front.handle(
        &request(
            "POST",
            "/control/commands",
            &format!("{cookie}{csrf}Origin: http://127.0.0.1:18091\r\n"),
            &command_body(1, "bff-identity-1"),
        ),
        2_000,
    );
    assert_eq!(
        accepted.0.status, 202,
        "同源 BFF 的写面必须真的受理（CSRF 双提交与同源那道锁都过了才对）: {}",
        accepted.0.body
    );

    let audit = front.handle(
        &request(
            "GET",
            "/control/audit",
            &format!("{cookie}Origin: http://127.0.0.1:18091\r\n"),
            "",
        ),
        3_000,
    );
    assert_eq!(audit.0.status, 200, "审计回读: {}", audit.0.body);
    let records: Vec<serde_json::Value> =
        serde_json::from_str(&audit.0.body).expect("audit 是 JSON 数组");
    assert_eq!(records.len(), 1, "审计应当只有那一条: {}", audit.0.body);
    assert_eq!(
        records[0]["operator_id"].as_str(),
        Some(OPERATOR),
        "审计身份必须由服务端按会话注入，命令体里自称的 {FORGED} 不许进账"
    );
    assert!(
        !audit.0.body.contains(FORGED),
        "自称的身份不该以任何形式出现在审计里: {}",
        audit.0.body
    );
    // 会话那一格仍然只能由服务端给出：换一枚别的会话 id 就碰不到这条链。
    let no_session = front.handle(
        &request(
            "POST",
            "/control/commands",
            "",
            &command_body(2, "bff-identity-2"),
        ),
        4_000,
    );
    assert_eq!(no_session.0.status, 401, "无会话的写面必须被拒");
}
