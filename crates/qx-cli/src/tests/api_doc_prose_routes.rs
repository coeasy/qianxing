//! 接口文档**两张端点表之外**的散文路由字面量，也必须是真的分派得到的入口（V13 R2 第二十二遍 #240）。
//!
//! 立案时的实测（`logs/s523_pass22_probe_doc_routes.txt`）：`deploy/README.md` 全文有 44 条
//! `` `METHOD /path` `` 形态的路由字面量，其中 14 条写在两张表之外的正文句子里。而按张核对那两条
//! 判据（`api_endpoint_table_routes.rs`）与门禁 `api_surface_doc_check` 都只吃以 `| ` 开头的表格行，
//! 于是「事件日志的读面与写面」那一节里 `GET /accounts/` 复数形态、带路径参数的那条入口
//! ——**从来没有被分派过**——跨过三轮文档改动没人报错。读者照那句话写客户端只会拿到 404 兜底，
//! 而这份文档是这套框架对外唯一的读面说明。
//!
//! 本轮把那一处改成真形状（`GET /account/snapshot?account_id=&venue_id=`），并把散文面纳入核对：
//! 表外每一处反引号路由字面量都拿去与 `handle_inner` 的 `(方法, 路由)` 分派集合比对，另加一条
//! 全文禁词（那种复数形态不许再出现）。改后同口径复测：44→45 条字面量、表外 14→15 条、幽灵 1→0
//! （`logs/s531_pass22_doc_routes_after.txt`），本判据的取数地板就按 15 条钉。
//!
//! 为什么单独住一个文件：`api_endpoint_table_routes.rs` 加上本判据会越过 500 行的棘轮门槛，而
//! 门禁 `line_budget_check` 只允许下降，所以这里按判据边界拆文件而不是把超限行登记进去。
//! 分派集合那把尺子（`dispatch_method_routes`）继续住在原文件，两侧共用同一口径。

use super::api_endpoint_table_routes::dispatch_method_routes;
use super::*;

/// 一行里反引号包住、且带方法前缀的 `(方法, 路径)`。
///
/// 路径的裁剪口径与 [`backticked_routes`](super::api_endpoint_table_routes::backticked_routes)
/// 相同（`[`、`?` 与空白之后不算路径），所以 `GET /account/snapshot[?account_id=&venue_id=]` 与
/// `GET /account/snapshot?after=0` 都落到同一条入口上；带 `{id}` 的形状会整段留在路径里，
/// 正是这种写法人工读得懂、机器不认账。
fn method_route_literals(line: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('`') else {
            break;
        };
        let token = after_open[..close].trim();
        rest = &after_open[close + 1..];
        for method in ["GET", "POST"] {
            let Some(suffix) = token.strip_prefix(method).map(str::trim) else {
                continue;
            };
            if !suffix.starts_with('/') {
                continue;
            }
            let path = suffix
                .chars()
                .take_while(|c| *c != '[' && *c != '?' && !c.is_whitespace())
                .collect::<String>();
            found.push((method.to_string(), path));
        }
    }
    found
}

/// #240 判据：散文句子里反引号包住的 `METHOD 路径` 也必须是分派得到的入口。
#[test]
fn prose_route_literals_outside_the_tables_are_served_too() {
    let raw = workspace_source("deploy/README.md");
    // 与 #183 同一口径：这份文档是纯 CRLF，按行取锚点前先折行尾。
    let readme = raw.replace("\r\n", "\n");
    let pairs = dispatch_method_routes();
    let mut prose_hits = 0_usize;
    let mut ghosts: Vec<String> = Vec::new();
    for line in readme.lines() {
        if line.starts_with("| ") {
            continue; // 表格行由 each_endpoint_table_lists_exactly_the_dispatch_routes 逐张核对
        }
        for (method, path) in method_route_literals(line) {
            prose_hits += 1;
            if !pairs.contains(&(method.clone(), path.clone())) {
                ghosts.push(format!("{method} {path}"));
            }
        }
    }
    assert!(
        prose_hits >= 15,
        "散文行里只数出 {prose_hits} 条路由字面量，比本轮实测的 15 条少：取数口径先修，\
         否则这条判据是空转（立案时表外有 14 条，本轮给本节补的点名句加了 1 条）"
    );
    assert!(
        ghosts.is_empty(),
        "接口文档的正文句子里承诺了这些入口，而 handle_inner 分派不到：{ghosts:?}。#240 立案时\
         那条 `GET /accounts/{{id}}/snapshot` 就是这个形状 —— 两张端点表都有一一对应的实现，只有\
         散文里多出来的这条没有。按仓库口径写成 `GET /account/snapshot?account_id=&venue_id=`。"
    );
    assert!(
        !readme.contains("/accounts/"),
        "接口文档里又出现了复数形态的账户路径：这条路由从来没有被分派过（V13 R2 第二十二遍 #240）"
    );
    assert!(
        readme.contains("prose_route_literals_outside_the_tables_are_served_too"),
        "接口文档不再点名本判据：读者看不出散文里的路由字面量也是有常驻核对的"
    );
    // 双向点名的另一半：文档要点出判据住的那个文件，否则读者按文件名找不到它。
    assert!(
        readme.contains("api_doc_prose_routes.rs"),
        "接口文档点名了本判据却不再点名它住的文件：散文路由核对的入口在文档里断了一半"
    );
}
