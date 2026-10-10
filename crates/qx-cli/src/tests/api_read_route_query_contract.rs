//! 读面的查询串契约：文档写的收窄键必须与实现真读的键一一对应（V13 R2 #205 / R31）。
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
//!
//! R31 补的是这条契约的第三张名单：`/account/ledger` 与 `/reconcile/reports` 不再是无键整体现
//! 读面，`?account_id=`（对账报告还认 `?venue_id=`）在结果集上真过滤。空结果回 `200 []` 而不是
//! 404——账簿与对账报告是整本台账加一轮对账结果，不是按 `(account_id, venue_id)` 键控的投影
//! 副本，过滤不出条目只说明这一轮没有事实，不等于"这个账户不在这份部署里"。

use super::api_endpoint_table_routes::backticked_routes;
use super::*;

const KEYLESS_ROUTES: [&str; 2] = ["/scheduler/runs", "/control/audit"];

/// 与 `read_scope::MODEL_FILTER_ROUTES` 逐格相等：路由名 + 它真认的收窄键。接线判据那条会用
/// 源码取数把它核回来，这里这一格是文档侧的承诺。
const MODEL_FILTER_ROUTES: [(&str, &[&str]); 2] = [
    ("/account/ledger", &["account_id"]),
    ("/reconcile/reports", &["account_id", "venue_id"]),
];

const READ_SCOPE_FILE: &str = "crates/qx-api/src/read_scope.rs";

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

/// 两份现读模型：账簿三条、对账报告三条，两个账户各带不同 venue。
///
/// 收窄是否真的生效只能靠"跨账户互不可见"证明：单账户用例过滤前后条数一样，看不出有没有
/// 真的按键筛过。
fn scoped_service() -> ApiService {
    let entry = |id: u64, account_id: &str, instrument: &str| -> qx_core::LedgerEntry {
        qx_core::LedgerEntry {
            id,
            account_id: account_id.into(),
            currency: "USDT".into(),
            kind: qx_core::LedgerEntryKind::TradeCash,
            amount: Money::from_i64(-50_000 * id as i64),
            instrument: Some(InstrumentId::parse(instrument).unwrap()),
            quantity: Quantity::from_i64(1),
            price: Some(Price::from_i64(50_000)),
            order_id: Some(id),
            ts: id * 10,
            multiplier: 1,
            position_side: None,
        }
    };
    let report = |worker_id: &str, account_id: &str, venue_id: &str| ReconcileReportSnapshot {
        schema_version: 1,
        worker_id: worker_id.into(),
        account_id: account_id.into(),
        venue_id: venue_id.into(),
        observed_ts: 1_700_000_000_000,
        order_issues: Vec::new(),
        balances_count: 1,
        balance_discrepancies: Vec::new(),
        position_snapshots_count: Some(0),
        funding_rate_snapshots_count: None,
        cashflow_count: None,
    };
    let mut state = ApiState::default();
    state.ledger_entries = vec![
        entry(1, "main", "BTCUSDT.OKX"),
        entry(2, "main", "ETHUSDT.OKX"),
        entry(3, "shadow", "BTCUSDT.BINANCE"),
    ];
    state.reconcile_reports.insert("w1".into(), report("w1", "main", "okx"));
    state.reconcile_reports.insert("w2".into(), report("w2", "main", "binance"));
    state.reconcile_reports.insert("w3".into(), report("w3", "shadow", "okx"));
    ApiService::new(state)
}

fn array_body(response: &qx_api::ApiResponse) -> Vec<serde_json::Value> {
    let value: serde_json::Value =
        serde_json::from_str(&response.body).expect("收窄读面的响应体必须是 JSON 数组");
    value.as_array().expect("收窄读面的响应体必须是数组").clone()
}

/// #205 行为判据：无键整体现读端点带查询串必须 400 并点名是哪条入口，不带才 200。
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

/// R31 行为判据：两条模型过滤读面的收窄键真的在结果集上生效，而且跨账户互不可见。
#[test]
fn model_filter_routes_honor_the_account_scope() {
    let service = scoped_service();

    let all = array_body(&service.handle("GET", "/account/ledger", "", 1));
    assert_eq!(all.len(), 3, "不带键的账簿读面照旧读整份现读模型");

    let main = array_body(&service.handle("GET", "/account/ledger?account_id=main", "", 2));
    assert_eq!(main.len(), 2, "按账户收窄要真筛掉别的账户: {main:?}");
    for entry in &main {
        assert_eq!(
            entry["account_id"].as_str(),
            Some("main"),
            "按账户收窄却把别的账户的流水带了回来: {entry:?}"
        );
    }
    let shadow = array_body(&service.handle("GET", "/account/ledger?account_id=shadow", "", 3));
    assert_eq!(shadow.len(), 1, "另一个账户要按自己的身份读得到: {shadow:?}");
    assert_eq!(shadow[0]["account_id"].as_str(), Some("shadow"));

    let all_reports = array_body(&service.handle("GET", "/reconcile/reports", "", 4));
    assert_eq!(all_reports.len(), 3, "不带键的对账读面照旧读整份现读模型");
    let main_reports =
        array_body(&service.handle("GET", "/reconcile/reports?account_id=main", "", 5));
    assert_eq!(main_reports.len(), 2, "对账报告也要按账户收窄: {main_reports:?}");
    let shadow_reports =
        array_body(&service.handle("GET", "/reconcile/reports?account_id=shadow", "", 6));
    assert_eq!(shadow_reports.len(), 1);
    for report in &shadow_reports {
        assert_eq!(report["account_id"].as_str(), Some("shadow"));
    }
}

/// 对账报告两把收窄键都能单独收窄；账簿只有 `account_id` 一列，所以这一条不盖 `/account/ledger`。
#[test]
fn reconcile_reports_narrow_by_venue_alone() {
    let service = scoped_service();

    let by_venue = array_body(&service.handle("GET", "/reconcile/reports?venue_id=okx", "", 1));
    assert_eq!(by_venue.len(), 2, "只给 venue 也要能收窄: {by_venue:?}");
    for report in &by_venue {
        assert_eq!(report["venue_id"].as_str(), Some("okx"));
    }
    let single =
        array_body(&service.handle("GET", "/reconcile/reports?venue_id=binance", "", 2));
    assert_eq!(single.len(), 1);
    let both = array_body(
        &service
            .handle("GET", "/reconcile/reports?account_id=main&venue_id=okx", "", 3),
    );
    assert_eq!(both.len(), 1, "两把键同给是叠加收窄，不是替代: {both:?}");
    assert_eq!(both[0]["worker_id"].as_str(), Some("w1"));
}

/// 收窄读面借用不了投影那族的 404：键形状合法但没有匹配条目时是 200 空数组。
///
/// 两族读的不是一份东西——投影是 `(account_id, venue_id)` 键控的独立副本，账簿与对账报告
/// 是整本台账加一轮结果。把 404 借过来等于宣布"这份部署里没有这个账户"，而调用方只是这一
/// 轮没有事实。反过来把账簿也接到 `missing_projection_response` 会静默把它换成另一条账户的
/// 数据，正是 #205 那一格。
#[test]
fn model_filter_routes_return_an_empty_array_not_a_missing_projection() {
    let service = scoped_service();
    for (route, query) in [
        ("/account/ledger", "?account_id=absent"),
        ("/reconcile/reports", "?account_id=absent&venue_id=okx"),
        ("/reconcile/reports", "?account_id=shadow&venue_id=binance"),
    ] {
        let response = service.handle("GET", &format!("{route}{query}"), "", 1);
        assert_eq!(
            response.status,
            200,
            "{route}{query} 没有匹配条目要回 200 空数组，不是投影缺失: {}",
            response.body
        );
        assert!(
            !response.body.contains("account_projection_not_found"),
            "{route}{query} 借用了投影那族的 404 码名: {}",
            response.body
        );
        assert!(array_body(&response).is_empty(), "{route}{query} 正文必须是空数组");
    }
}

/// 每条入口认哪几把键，由数据里有没有那一列决定：账簿没有 venue 列，第二把键当场 400。
#[test]
fn model_filter_routes_refuse_the_keys_their_data_cannot_honor() {
    let service = scoped_service();
    for (route, query, refused) in [
        ("/account/ledger", "?venue_id=okx", "venue_id"),
        ("/account/ledger", "?after=1", "after"),
        ("/account/ledger", "?limit=1", "limit"),
        ("/reconcile/reports", "?limit=1", "limit"),
    ] {
        let response = service.handle("GET", &format!("{route}{query}"), "", 1);
        assert_eq!(
            response.status,
            400,
            "{route}{query} 里的 {refused} 不在这条入口的名单里: {}",
            response.body
        );
        assert!(
            response.body.contains(route) && response.body.contains(refused),
            "{route}{query} 的 400 要点名是哪条入口、哪把键: {}",
            response.body
        );
    }
    // 空串与带空白的键不是"没有点名"，是形状非法——否则会被当成一个真实账户去查。
    for query in ["?account_id=", "?account_id=%20", "?account_id=%20&venue_id=okx"] {
        let response = service.handle("GET", &format!("/reconcile/reports{query}"), "", 2);
        assert_eq!(
            response.status,
            400,
            "/reconcile/reports{query} 的收窄键是空串，要回 400: {}",
            response.body
        );
    }
    // 归一化口径与 `ApiProjectionKey::new` 同一支：两侧都 trim，配置里写 `" main "` 不会读出空表。
    let trimmed = array_body(&service.handle("GET", "/account/ledger?account_id=%20main", "", 3));
    assert_eq!(
        trimmed.len(),
        2,
        "带空白的收窄键要按 trim 后的值去比，而不是读出一份空账簿: {trimmed:?}"
    );
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
    // R6 的另一半：无键整体现读端点与带键读面各自只认自己的键。`?acount_id=` 拼错时它落到
    // "没有收窄键"那一支，默认账户那份就被念成调用方点名的账户——正是 #191 那句"拼错的
    // 账户 id 读成干净的空账户"剩下的下半格。名单外的键当场 400 并点名那把键。
    // 路由名单从源码取，不在这里抄第二份：把一条入口挪出名单，它就拒不起自己文档承诺的键。
    let scope = workspace_source(READ_SCOPE_FILE);
    let list = scope
        .find("const PROJECTION_SCOPED_ROUTES: [&str;")
        .expect("qx-api 里找不到 PROJECTION_SCOPED_ROUTES");
    let list_end = scope[list..].find("];").expect("带键读面名单必须闭合") + list;
    let refusal_scope = scope[list..list_end]
        .split('"')
        .skip(1)
        .step_by(2)
        .chain(std::iter::once("/account/snapshot/diff"))
        .collect::<Vec<_>>();
    // 取数区间一旦挪偏，这个循环会一颗判据都不发就过去：七条带键读面加 `diff` 共八条，缺一即红。
    assert_eq!(
        refusal_scope.len(),
        8,
        "这一格的覆盖面挪了：{refusal_scope:?}"
    );
    for route in refusal_scope {
        for (query, name) in [
            ("?acount_id=ghost", "acount_id"),
            ("?account_id=a&venue_id=paper&limit=10", "limit"),
        ] {
            let response = service.handle("GET", &format!("{route}{query}"), "", 6);
            assert_eq!(
                response.status, 400,
                "{route}{query} 里的 {name} 不在这条入口的名单里，不能默默读成全局那一份: {}",
                response.body
            );
            assert!(
                response.body.contains("不接受查询参数") && response.body.contains(name),
                "{route} 的 400 要点名被拒的那把键 {name}: {}",
                response.body
            );
        }
    }
    // 正向臂：`after` 在事件读面上是合法键，这条通道不是"带查询串就拒"。
    let cursor = service.handle("GET", "/events?after=0", "", 7);
    assert!(
        !cursor.body.contains("不接受查询参数"),
        "事件读面认 `after`，它不能被上面那张表一起拒掉: {}",
        cursor.body
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
        missing_base.status,
        409,
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

/// #205 / R31 文档侧：无键读面不再写 `[?…]`，而模型过滤读面那一格必须写出它真认的收窄键。
#[test]
fn endpoint_table_only_promise_query_keys_where_the_route_reads_them() {
    for route in KEYLESS_ROUTES {
        let row = semantics_row(route);
        assert!(
            !row.contains(&format!("`GET {route}[?")) && !row.contains(&format!("`{route}[?")),
            "{route} 那一行又写回 `[?…]`：这条入口没有收窄键，写出来就是让调用方以为能按账户读\n{row}"
        );
        // 两条都要写出 400：`/control/audit` 起初那一格是「—」，但 #205 的通道一样盖着它，
        // 留一个豁免等于让这条入口的 400 没有文档落点。
        assert!(
            row.contains("400"),
            "{route} 现在会因查询串回 400，「非 200 口径」那一格必须写它: {row}"
        );
    }
    for (route, params) in MODEL_FILTER_ROUTES {
        let row = semantics_row(route);
        for param in params {
            assert!(
                row.contains(&format!("[?")) && row.contains(&format!("{param}=")),
                "{route} 现在真按 `?{param}=` 收窄，「入口」那一格必须写出这把键: {row}"
            );
        }
        assert!(
            !promises_404(&row) && !row.contains("account_projection_not_found"),
            "{route} 借用了投影那族的 404 口径，而它过滤不出条目时回的是 200 空数组: {row}"
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

/// 接线判据：三张名单必须两两不相交，且都从源码取数——把一条路由从带键名单挪到无键名单
/// 会让它拒绝自己文档承诺的键，反过来则让 400 通道空转。
#[test]
fn keyed_and_keyless_route_lists_are_disjoint_and_both_live() {
    let scope = workspace_source(READ_SCOPE_FILE);
    let api = workspace_source("crates/qx-api/src/lib.rs");
    let extract = |text: &str, name: &str| {
        let start = text
            .find(&format!("const {name}: [&str;"))
            .unwrap_or_else(|| panic!("qx-api 里找不到 {name}"));
        let end = text[start..].find("];").expect("路由名单必须闭合") + start;
        text[start..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    // 元组表：只取每格的第一个字面量（路由名），`step_by(2)` 会把收窄键名一起抽出来。
    let filter_start = scope
        .find("const MODEL_FILTER_ROUTES: [(&str, &[&str]);")
        .unwrap_or_else(|| panic!("qx-api 里找不到 MODEL_FILTER_ROUTES"));
    let filter_end = scope[filter_start..]
        .find("];")
        .expect("模型过滤读面名单必须闭合")
        + filter_start;
    let mut filter = Vec::new();
    let mut cursor = &scope[filter_start..filter_end];
    while let Some(open) = cursor.find("(") {
        let rest = &cursor[open + 1..];
        let Some(first) = rest.find('"') else {
            break;
        };
        let after = &rest[first + 1..];
        let Some(close) = after.find('"') else {
            break;
        };
        filter.push(after[..close].to_string());
        cursor = &after[close + 1..];
    }
    let scoped = extract(&scope, "PROJECTION_SCOPED_ROUTES");
    let keyless = extract(&scope, "KEYLESS_READ_ROUTES");
    let set = |routes: &[String]| -> std::collections::BTreeSet<String> {
        routes.iter().cloned().collect()
    };
    let filter_set = set(&filter);
    assert_eq!(scoped.len(), 7, "带键读面是七条，见接口文档那段");
    assert_eq!(filter.len(), 2, "模型过滤读面是两条：账簿流水与对账报告");
    assert_eq!(
        set(&keyless),
        set(&KEYLESS_ROUTES
            .iter()
            .map(|r| (*r).to_string())
            .collect::<Vec<_>>()),
        "无键读面名单与用例里的两条不一致：这条契约两侧各自改了口"
    );
    assert_eq!(
        filter_set,
        set(&MODEL_FILTER_ROUTES
            .iter()
            .map(|(route, _)| route.to_string())
            .collect::<Vec<_>>()),
        "模型过滤读面名单与用例里的两格不一致：这条契约两侧各自改了口"
    );
    for (label, routes) in [("带键", &scoped), ("无键", &keyless), ("模型过滤", &filter)] {
        for route in routes {
            assert!(
                api.contains(&format!("(\"GET\", \"{route}\")")),
                "{route} 在{label}名单里却没有分派臂：收窄通道会为一条不存在的入口说话"
            );
        }
    }
    let scoped_set = set(&scoped);
    for (label, routes) in [("无键", &keyless), ("模型过滤", &filter)] {
        for route in routes {
            assert!(
                !scoped_set.contains(route),
                "{route} 同时出现在带键名单与{label}名单里"
            );
        }
    }
    assert!(
        keyless.iter().all(|route| !filter.contains(route)),
        "无键名单与模型过滤名单相交"
    );
}
