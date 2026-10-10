//! 读面的收窄口径：一条入口认哪几把查询键、键怎么解析、解析结果怎么套在结果集上。
//!
//! 从 crate 根拆出（lib.rs 的行数预算不允许读面分类继续堆在那儿）。三张路由名单是这条契约的
//! 单点：`admission::refused_query_param` 按它决定「名单外的键当场 400 并点名」，
//! `ApiService::handle_inner` 按 `PROJECTION_SCOPED_ROUTES` 决定「要不要先过投影缺失那道 404」。

use super::ApiProjectionKey;
use crate::admission::query_param;

/// 接受 `?account_id=&venue_id=` 的读面，即"键指向某一份账户投影"的那些入口。
/// 它们在分派前共用 `ApiService::missing_projection_response` 那道 404（V13 R2 #191）；
/// `/account/snapshot/diff` 不在列——它的定位符是 `base_hash`，投影缺失时
/// `409 snapshot_base_not_found` 说的就是"这份基线不在这条链上"，不是账户不存在。
pub(crate) const PROJECTION_SCOPED_ROUTES: [&str; 7] = [
    "/account/snapshot",
    "/account/snapshot/envelope",
    "/account/orders",
    "/account/positions",
    "/account/balances",
    "/events",
    "/events/live",
];

/// 现读模型的按账户收窄读面（V13 R31）：一条一格，写清这条入口认哪几把收窄键。
///
/// 读的还是整份现读模型，`?account_id=`（对账报告还认 `?venue_id=`）只在结果集上过滤，
/// 不落到"某一份账户投影"。所以键形状合法但没有匹配条目时回 `200 []`，而不是投影那族的
/// `404 account_projection_not_found`——两族的差别是刻意的：投影是 `(account_id, venue_id)`
/// 键控的一份独立副本，没有这份副本就是"这个账户不在这份部署里"；账簿与对账报告是整本台账
/// 加一轮对账结果，过滤不出条目只说明这一轮没有事实。
///
/// 账簿那一格只认一把键不是取巧：`LedgerEntry` 没有 venue 字段，给它配第二把收窄键只会让
/// 调用方以为按 venue 读过一遍。
pub(crate) const MODEL_FILTER_ROUTES: [(&str, &[&str]); 2] = [
    ("/account/ledger", &LEDGER_FILTER_PARAMS),
    ("/reconcile/reports", &REPORT_FILTER_PARAMS),
];

/// 反过来：这两条读的是整份现读模型，且数据本身没有账户归属列（`JobRun` 是作业级、
/// `AuditRecord` 是操作员级），没有任何收窄键。带查询串进来必须回 400 而不是照常 200——
/// 按账户读请走 `/account/snapshot` 一族或 `MODEL_FILTER_ROUTES` 那两条（V13 R2 #205 / R31）。
pub(crate) const KEYLESS_READ_ROUTES: [&str; 2] = ["/scheduler/runs", "/control/audit"];

/// 带键读面认的两把收窄键。
const ACCOUNT_SCOPED_PARAMS: [&str; 2] = ["account_id", "venue_id"];
/// `/events` 与 `/events/live` 在收窄键之外还认游标。
const EVENT_SCOPED_PARAMS: [&str; 3] = ["account_id", "venue_id", "after"];
/// `/account/snapshot/diff` 的定位符是基线哈希，不是收窄键。
const SNAPSHOT_DIFF_PARAMS: [&str; 3] = ["base_hash", "account_id", "venue_id"];
/// 账簿流水只有 `account_id` 一列。
const LEDGER_FILTER_PARAMS: [&str; 1] = ["account_id"];
/// 对账报告两条都有，可以单独按 venue 收窄。
const REPORT_FILTER_PARAMS: [&str; 2] = ["account_id", "venue_id"];

/// 每条入口认的收窄键名单，按路由点名。返回 `None` 的那类路径（`/health` 那一类）不判查询串。
///
/// 名单外的键当场 400 并点名那把键，就是把这条读面的口径从"查不到就算了"改成"你没说清就别读"：
/// `?acount_id=` 拼错时它会落到"没有收窄键"那一支，把默认账户念成调用方点名的账户（V13 R6）。
pub(crate) fn accepted_query_params(route: &str) -> Option<&'static [&'static str]> {
    if let Some((_, params)) = MODEL_FILTER_ROUTES.iter().find(|(name, _)| *name == route) {
        return Some(params);
    }
    match route {
        "/events" | "/events/live" => Some(&EVENT_SCOPED_PARAMS),
        "/account/snapshot/diff" => Some(&SNAPSHOT_DIFF_PARAMS),
        route if KEYLESS_READ_ROUTES.contains(&route) => Some(&[]),
        route if PROJECTION_SCOPED_ROUTES.contains(&route) => Some(&ACCOUNT_SCOPED_PARAMS),
        _ => None,
    }
}

/// WS 那一支不按路径查表：`Upgrade: websocket` 可以从任何路径进来，认的键与 `/events` 同宽。
pub(crate) fn event_scoped_params() -> &'static [&'static str] {
    &EVENT_SCOPED_PARAMS
}

pub(crate) fn projection_key_from_query(query: &str) -> Result<Option<ApiProjectionKey>, String> {
    let account_id = query_param(query, "account_id")?;
    let venue_id = query_param(query, "venue_id")?;
    match (account_id, venue_id) {
        (None, None) => Ok(None),
        (Some(account_id), Some(venue_id)) => {
            let key = ApiProjectionKey::new(account_id, venue_id);
            key.validate()?;
            Ok(Some(key))
        }
        _ => Err("account_id 和 venue_id 必须同时提供".into()),
    }
}

/// 现读模型读面的收窄结果。`None` 表示调用方没点名任何账户，读整份现读模型；
/// 两格都是 `Option` 而不是强配对——账簿流水只有 `account_id` 一列，对账报告两把都认，
/// 哪一把能进哪条入口由 `accepted_query_params` 在分派前拒绝，这里不必知道路由。
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct ScopeFilter {
    pub account_id: Option<String>,
    pub venue_id: Option<String>,
}

impl ScopeFilter {
    /// 空串与带空白的键不是"没有点名"，是形状非法：回 400，而不是被当成一个真实账户去查。
    /// `trim` 与 `ApiProjectionKey::new` 同一口径，查询侧与投影侧因此不会分叉。
    pub fn from_query(query: &str) -> Result<Option<ScopeFilter>, String> {
        let account_id = query_param(query, "account_id")?.map(|value| value.trim().to_owned());
        let venue_id = query_param(query, "venue_id")?.map(|value| value.trim().to_owned());
        if account_id.as_deref().is_some_and(str::is_empty)
            || venue_id.as_deref().is_some_and(str::is_empty)
        {
            return Err("account_id 与 venue_id 都不能是空串".into());
        }
        Ok(match (account_id, venue_id) {
            (None, None) => None,
            (account_id, venue_id) => Some(Self {
                account_id,
                venue_id,
            }),
        })
    }
}

/// 把收窄套在 `account_id` 那一列上：没点名就全收。
///
/// 比较两侧都 `trim`：配置里写 `" main "` 这种带空白的账户已经由 `ApiProjectionKey::new`
/// 归一化成 `main`，若这里只 trim 查询侧，存了带空白值的账簿条目会过滤不到，运维看到的是
/// "这个账户存在、但没有流水"的空表——正是 #205 想堵的那一类静默误读。
pub(crate) fn scope_keeps_account(scope: Option<&ScopeFilter>, account_id: &str) -> bool {
    scope
        .and_then(|filter| filter.account_id.as_deref())
        .is_none_or(|wanted| wanted == account_id.trim())
}

/// 把收窄套在 `venue_id` 那一列上：没点名就全收。比较口径同 `scope_keeps_account`。
pub(crate) fn scope_keeps_venue(scope: Option<&ScopeFilter>, venue_id: &str) -> bool {
    scope
        .and_then(|filter| filter.venue_id.as_deref())
        .is_none_or(|wanted| wanted == venue_id.trim())
}
