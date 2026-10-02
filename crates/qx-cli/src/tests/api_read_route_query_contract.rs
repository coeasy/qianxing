//! 读面的查询串契约：文档写的收窄键必须与实现真读的键一一对应（V13 R2 #205）。
//!
//! 立案时两侧各有一处空头承诺：
//!
//! 1. `deploy/README.md` 第二张端点表把 `/account/ledger` 写成 `GET /account/ledger[?…]`，
//!    而 `handle_inner` 那条臂从头到尾没碰过 `query`。调用方递来 `?account_id=shadow`
//!    拿到的仍是"默认账户"那份流水——它不是报错、不是空数组，而是**另一个账户的真实数据**，
//!    读侧看不出任何痕迹。同一张表里 `/scheduler/runs`、`/reconcile/reports`、`/control/audit`
//!    是同一条形状，只是没写 `[?…]`。
//! 2. `/account/snapshot/diff` 里有一条 `404 snapshot_not_found` 分支，两张端点表都没写它——
//!    不是因为漏写：`publish_snapshot`（全局与按账户两处）都把 `snapshot_history` 与 `snapshot`
//!    同批写入，基准查得到就一定有当前快照，那个 404 取不到。留着它等于让文档去解释一条
//!    永不返回的分支，也让"这张表列全了非 200 口径"这句话打折。

use super::api_endpoint_table_routes::backticked_routes;
use super::*;

const KEYLESS_ROUTES: [&str; 4] = [
    "/account/ledger",
    "/scheduler/runs",
    "/reconcile/reports",
    "/control/audit",
];

/// 第二张端点表里那条路由那一整行（按 `backticked_routes` 认行，与 #180 同一取数口径：
/// 一格并列多条入口时只有第一条带 `GET`，按 `"`+路由`"` 找会漏读）。
fn semantics_row(route: &str) -> String {
    let readme = workspace_source("deploy/README.md");
    let header = "| 端点 | 语义 | 非 200 口径 |";
    let start = readme
        .find(header)
        .expect("deploy/README.md 必须有「非 200 口径」那张端点表");
    for line in readme[start..].lines().skip(1) {
        let Some(rest) = line.strip_prefix("| ") else {
            break;
        };
        let first_cell = rest.split('|').next().unwrap_or_default().trim();
        if backticked_routes(first_cell)
            .iter()
            .any(|listed| listed == route)
        {
            return line.to_string();
        }
    }
    panic!("「非 200 口径」那张表里没有 {route} 这一行：这张表的口径需要重新核对")
}

/// #205 行为判据：四条整体现读端点带查询串必须 400 并点名是哪条入口，不带才 200。
#[test]
fn keyless_read_routes_refuse_a_query_they_cannot_honor() {
    let service = ApiService::new(ApiState::default());
    for route in KEYLESS_ROUTES {
        for query in [
            "?account_id=shadow&venue_id=paper",
            "?account_id=shadow",
            "?after=1",
        ] {
            let response = service.handle("GET", &format!("{route}{query}"), "", 1);
            assert_eq!(
                response.status, 400,
                "{route}{query} 读的是整份现读模型，收窄键没有生效却回了 {}",
                response.status
            );
            assert!(
                response.body.contains(route) && response.body.contains("不接受查询参数"),
                "{route}{query} 的 400 必须点名是哪条入口: {}",
                response.body
            );
        }
        assert_eq!(
            service.handle("GET", route, "", 2).status,
            200,
            "{route} 不带查询串时仍要照常可读"
        );
    }
}

/// #205 不能被读成"读面开始拒绝查询串"：带键的七条投影读面依旧按 `account_id`/`venue_id`
/// 收窄，形状非法回 400、投影缺失回 404，走的不是上面那条通道（V13 R2 #191）。
#[test]
fn projection_scoped_routes_keep_their_own_key_contract() {
    let service = ApiService::new(ApiState::default());
    let missing = service.handle("GET", "/account/orders?account_id=a&venue_id=paper", "", 3);
    assert_eq!(missing.status, 404, "带键但仓内没有这份投影必须 404");
    assert!(
        missing.body.contains("account_projection_not_found"),
        "投影缺失的码名不能漂到 400 那一格: {}",
        missing.body
    );
    let pairing = service.handle("GET", "/account/positions?account_id=a", "", 4);
    assert_eq!(pairing.status, 400, "只给一半键仍是形状非法");
    assert!(
        pairing.body.contains("同时提供"),
        "收窄键的配对口径要说清楚: {}",
        pairing.body
    );
    assert_eq!(
        service.handle("GET", "/account/balances", "", 5).status,
        200,
        "不带键的投影读面照旧读全局"
    );
}

/// #205 第二半：`snapshot_diff` 的非 200 口径只剩文档写的那两个码。
/// 用例按源码区间取数，因为那条 404 是"分支到得了但路由取不到"的形态——
/// 只看两张端点表会一致地漏掉它，只有实现侧的分支会说话。
#[test]
fn snapshot_diff_returns_only_the_documented_non_200_codes() {
    let api = workspace_source("crates/qx-api/src/lib.rs");
    let start = api
        .find("fn snapshot_diff(")
        .expect("qx-api 必须有 snapshot_diff");
    let end = api[start..]
        .find("\n    fn ")
        .expect("snapshot_diff 之后必须还有下一个方法，取数区间才闭合")
        + start;
    let body = &api[start..end];
    assert!(
        !body.contains("ApiResponse::json(404"),
        "snapshot_diff 又产出了两张端点表都没写的 404"
    );
    assert!(
        body.contains("ApiResponse::json(400") && body.contains("ApiResponse::json(409"),
        "snapshot_diff 的两个已文档化口径（400 参数非法 / 409 基准缺失）不能一起消失"
    );
    for projection in ["Some(key) =>", "None =>"] {
        assert!(
            body.contains(projection),
            "按账户与全局两条分支都得留在同一个 409 口径下: 缺 {projection}"
        );
    }

    let service = ApiService::new(ApiState::default());
    let missing_base = service.handle("GET", "/account/snapshot/diff?base_hash=7", "", 6);
    assert_eq!(
        missing_base.status, 409,
        "基准不在历史里要说 409，不能漂成文档里没有的码"
    );
    assert!(
        missing_base.body.contains("snapshot_base_not_found"),
        "{}",
        missing_base.body
    );
    assert_eq!(
        service
            .handle("GET", "/account/snapshot/diff", "", 7)
            .status,
        400,
        "缺 base_hash 仍是 400"
    );
    assert_eq!(
        service
            .handle("GET", "/account/snapshot/diff?base_hash=x", "", 8)
            .status,
        400,
        "base_hash 非无符号整数仍是 400"
    );
}

/// 那一格里有没有承诺"404 + 错误码名"。
///
/// 表格的写法是 `404 \`码名\``；negation（"无 \`404\` 分支"）后面跟的是反引号加空格，不算承诺。
/// 直接 `contains("404")` 会把那句否定读成承诺，判据就成了自己跟自己打架。
fn promises_404(cell: &str) -> bool {
    let mut rest = cell;
    while let Some(found) = rest.find("404") {
        let after = rest[found + 3..].trim_start();
        let name = after.strip_prefix('`').unwrap_or(after).trim_start();
        if name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
            return true;
        }
        rest = &rest[found + 3..];
    }
    false
}

/// #205 文档侧：那张表不再给整体现读端点写 `[?…]`，而带键的那几条的收窄口径原样留着。
#[test]
fn endpoint_table_only_promise_query_keys_where_the_route_reads_them() {
    for route in KEYLESS_ROUTES {
        let row = semantics_row(route);
        assert!(
            !row.contains(&format!("`GET {route}[?")) && !row.contains(&format!("`{route}[?")),
            "{route} 那一行又写回 `[?…]`：这条入口没有收窄键，写出来就是让调用方以为能按账户读\n{row}"
        );
        // 四条都要写出 400：`/control/audit` 起初那一格是「—」，但 #205 的通道一样盖着它，
        // 留一个豁免等于让这条入口的 400 没有文档落点。
        assert!(
            row.contains("400"),
            "{route} 现在会因查询串回 400，「非 200 口径」那一格必须写它: {row}"
        );
    }
    let diff_row = semantics_row("/account/snapshot/diff");
    assert!(
        !promises_404(&diff_row),
        "diff 那一行承诺了 404 码，而实现里已经没有这条分支: {diff_row}"
    );
    let snapshot_row = semantics_row("/account/snapshot");
    assert!(
        snapshot_row.contains("[?account_id=&venue_id=]") && promises_404(&snapshot_row),
        "带键读面的口径与这条新通道是同一次改动的两侧，不能被一起抹掉: {snapshot_row}"
    );
}

/// 接线判据：两份名单必须互不相交，且都从源码取数——把一条路由从带键名单挪到无键名单
/// 会让它拒绝自己文档承诺的键，反过来则让 400 通道空转。
#[test]
fn keyed_and_keyless_route_lists_are_disjoint_and_both_live() {
    let api = workspace_source("crates/qx-api/src/lib.rs");
    let extract = |name: &str| {
        let start = api
            .find(&format!("const {name}: [&str;"))
            .unwrap_or_else(|| panic!("qx-api 里找不到 {name}"));
        let end = api[start..].find("];").expect("路由名单必须闭合") + start;
        api[start..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let scoped = extract("PROJECTION_SCOPED_ROUTES");
    let keyless = extract("KEYLESS_READ_ROUTES");
    let set = |routes: &[String]| -> std::collections::BTreeSet<String> {
        routes.iter().cloned().collect()
    };
    assert_eq!(scoped.len(), 7, "带键读面是七条，见接口文档那段");
    assert_eq!(
        set(&keyless),
        set(&KEYLESS_ROUTES
            .iter()
            .map(|r| (*r).to_string())
            .collect::<Vec<_>>()),
        "无键读面名单与用例里的四条不一致：这条契约两侧各自改了口"
    );
    for route in &keyless {
        assert!(
            !scoped.contains(route),
            "{route} 同时出现在带键与无键两份名单里"
        );
        assert!(
            api.contains(&format!("(\"GET\", \"{route}\")")),
            "{route} 在名单里却没有分派臂：400 通道会为一条不存在的入口说话"
        );
    }
}
