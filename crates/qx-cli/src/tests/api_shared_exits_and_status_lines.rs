//! 「非 200 口径」的每一行与 HTTP 状态行的原因短语（V13 R2 第十六遍 #221）。
//!
//! 立案现场有两条，都是"文档与实现各说一半"：
//!
//! 1. `deploy/README.md` 第二张端点表里 `GET /health`、`GET /metrics`、
//!    `GET /schema/account-snapshot-v1` 三行的「非 200 口径」写着 `—`，而 `/health` 的第二格还写着
//!    「恒 200」。实现里限流判定在 `handle_inner` 的**分派之前**，任何一条 HTTP 入口都先过桶：
//!    这三条一样会回 429（超额）或 503（桶自己读不到状态）。运维照着"这条入口恒 200"配探活，
//!    收到的却是一个从文档里查不到出口的码——与 #191「空 200 冒充没有这个账户」是同一类读侧欺骗，
//!    只是方向相反。
//! 2. 状态行的原因短语只点名了六个码，`403` 与 `503` 一起落进 `_ => "Internal Server Error"`。
//!    于是"你没权限"和"这道闸门自己坏了"在 HTTP 层都是 500 的那句话（#172 在风控端口上刚把
//!    "拒绝"与"端口坏了"分成两条通道，这里漏了另一半）。
//!
//! 两条判据都不看散文，只看**表里那一格**与**源码那一处 match**：改任一侧而不改另一侧都会红。

use super::api_endpoint_table_routes::{emitted_statuses, semantics_cells};
use super::*;
use std::collections::BTreeMap;

/// 「非 200 口径」每一行都必须出现的两条共享出口，逐字符核对。
///
/// 写成整串而不是"含 429 且含 503"，是因为码名才是读者真正拿去分支的东西：只数状态码的话，
/// 一格里写 `503 队列不可用`（`control_state_unavailable`）也算过关，而限流后端故障是另一件事。
const SHARED_LIMIT_EXITS: &str =
    "429 `api_rate_limit_exceeded`；503 `api_rate_limit_backend_unavailable`";

/// #221 判据一：全表每一行都得写出限流的两条出口，因为它们在路由分派之前判定。
#[test]
fn every_semantics_row_declares_the_shared_rate_limit_exits() {
    for (route, cell) in semantics_cells() {
        assert!(
            cell.contains(SHARED_LIMIT_EXITS),
            "{route} 的「非 200 口径」这一格没有写出全表共有的两条限流出口（{SHARED_LIMIT_EXITS}）。\
             立案现场是三格写着 `—`、`/health` 那格的语义还写着「恒 200」——限流桶在 `handle_inner`\
             分派给任何路由之前就先判定，所以这三条入口一样会被 429/503 挡下，探活脚本按'恒 200'\
             写就会把一个查不到出口的码读成服务坏了。"
        );
    }
    let readme = workspace_source("deploy/README.md");
    assert!(
        !readme.contains("恒 200"),
        "接口文档里又出现了「恒 200」：这条 API 没有任何一条 HTTP 入口是恒 200 的，\
         限流在分派之前判定（本判据的立案理由）"
    );
    // 双向点名：删掉判据或把文档里那句核对声明改掉，都要有一边红。
    assert!(
        readme.contains("every_semantics_row_declares_the_shared_rate_limit_exits"),
        "接口文档不再点名本判据：读者看不出「非 200 口径」每一行的两条限流出口是有常驻核对的"
    );
    let own_source =
        workspace_source("crates/qx-cli/src/tests/api_shared_exits_and_status_lines.rs");
    let definition = format!(
        "fn {}(",
        "every_semantics_row_declares_the_shared_rate_limit_exits"
    );
    assert_eq!(
        own_source.matches(definition.as_str()).count(),
        1,
        "本判据的定义点数不为 1 处：0 处是被改名或删掉，2 处是用例文件被复制出第二份"
    );
}

/// `write_http_response` 里 `reason` 那块 match：`(状态码, 短语)` 与 `_` 那格的短语。
fn reason_arms() -> (BTreeMap<u16, String>, String) {
    let api = workspace_source("crates/qx-api/src/lib.rs");
    let head = "let reason = match response.status {";
    let at = api.find(head).expect(
        "状态行短语必须由 write_http_response 里的一处 match 决定，改成别的形态要连本判据一起改",
    ) + head.len();
    let rest = &api[at..];
    let body = &rest[..rest.find("};").expect("reason match 必须闭合")];
    let mut arms = BTreeMap::new();
    let mut fallback = String::new();
    for line in body.lines() {
        let Some((code, phrase)) = line.trim().split_once("=>") else {
            continue;
        };
        let phrase = phrase
            .trim()
            .trim_end_matches(',')
            .trim_matches('"')
            .to_string();
        match code.trim() {
            "_" => fallback = phrase,
            literal => {
                arms.insert(
                    literal.parse::<u16>().unwrap_or_else(|error| {
                        panic!("reason match 里有一臂不是状态码 {line:?}: {error}")
                    }),
                    phrase,
                );
            }
        };
    }
    (arms, fallback)
}

/// #221 判据二：读面写出的每一个状态码，状态行都要有自己的短语，而且彼此不共用。
#[test]
fn status_line_reason_names_cover_every_code_the_read_face_emits() {
    let (arms, fallback) = reason_arms();
    let emitted = emitted_statuses();
    assert!(
        emitted.len() >= 7 && arms.len() >= 9,
        "取数口径先失灵了：读面数出 {} 个非 200 状态码、状态行点名了 {} 个（立案时是 7 与 9）{:?}。\
         扫描空转时这条判据会一路绿下去，所以宁让它先红。",
        emitted.len(),
        arms.len(),
        arms.keys().copied().collect::<Vec<_>>()
    );
    for code in &emitted {
        assert!(
            arms.contains_key(code),
            "读面会写出 {code}，但状态行的原因短语没有点它的名，于是它落进 `_ => {fallback:?}`\
             那一格——立案时 403 与 503 正是这样：客户端按状态行读会把「没权限」与「闸门自己坏了」\
             听成同一句 500 的话。"
        );
    }
    assert!(
        arms.contains_key(&200),
        "状态行连 200 的短语都没有了，那这条 API 的每个成功响应都在说不成立的话"
    );
    let mut first_user: BTreeMap<String, u16> = BTreeMap::new();
    for (code, phrase) in &arms {
        if let Some(other) = first_user.get(phrase) {
            panic!(
                "{other} 与 {code} 共用状态行短语 {phrase:?}：两个码一句话就是本判据要挡的形态，\
                 把 503 的短语改成 500 那句同样会落在这里"
            );
        }
        first_user.insert(phrase.clone(), *code);
    }
    assert!(
        !first_user.contains_key(&fallback),
        "兜底那格用的是已点名短语 {fallback:?}：新增一个没登记的状态码时，状态行会替它说一句\
         听起来正确的话，读者永远看不出少了一格"
    );
    let readme = workspace_source("deploy/README.md");
    assert!(
        readme.contains("status_line_reason_names_cover_every_code_the_read_face_emits"),
        "接口文档不再点名本判据：读者看不出状态行的短语是有常驻核对的"
    );
}
