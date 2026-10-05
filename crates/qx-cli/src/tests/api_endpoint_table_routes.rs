//! 两张端点表各自的路由集合，都必须与 `handle_inner` 真的分派的那一组逐一相等（V13 R2 第八遍）。
//!
//! 立案的是门禁 `api_surface_doc_check` 的取数口径：它把 `deploy/README.md` 全文里所有
//! `` | `GET /x` `` 形态的行收成一个**并集**，再与实现侧的路由集合比相等。文档里有两张端点表
//! （一张写 200 响应体形状，一张写语义/非 200 口径），两条都进并集，所以从任一张里整行删掉
//! 一条路由，并集不变、门禁照印绿——第六遍把它登记为 #176（只钉住了第一张），本轮补第二张。
//!
//! 这一盲区不是推测：第六遍实测删掉 `/events/live` 那一行时门禁仍然 `[PASS]`（`logs/s128_*.txt`）。
//! 读面少一行不等于少一条能力，但运维照着一张表配抓取与告警、另一张表说这条路存在，两边就会
//! 各自缺一块。所以这里逐表比，不比并集。
//!
//! 本文件后半还有一条 #182：同一张表第三格（「非 200 口径」）里点名的**错误码名与状态码**，
//! 与读面实现之间两向都要平——路由名对上而错误体说错话，是同一类腐坏的下一层。
//!
//! 最后一条 #183 钉的是这三样东西**住在文档的哪一章**：两张表与「### 端点表按张核对」必须一起
//! 住在 `## Paper API` 与下一个一级标题之间。上面那两条判据都按整篇文档搜表头取数，所以第八遍
//! 立案时那一段其实挂在 `## Outbox 与 NATS JetStream` 下，搬家也不会让它们红 —— 位置需要专门一条。

use super::*;
use std::collections::BTreeSet;

/// 读面的**全部**源码，而不是 crate 根那一个文件。
///
/// 立案现场：下面两条取数原先都写死 `crates/qx-api/src/lib.rs`，而读面已经拆成
/// `lib.rs` + `transport.rs` + `ws.rs` + `admission.rs` + `event_cursor.rs`。把一段
/// `ApiResponse::json(409, error_json("…"))` 从 lib.rs 挪进任何子模块，取数集合就静默
/// 缩小一格：文档那一格点名的码名会从"实现产得出"变成"实现产不出"，而这条判据只红在
/// 文档侧，读者看不出真正搬家的是代码。这与 `api_surface_doc_check` 把两张表收成并集
/// 是同一类盲区，只是方向相反（那边是"多读一份所以看不见少一行"，这边是"少读一份所以
/// 看不见少一个码名"）。
///
/// 所以这里按目录取数而不是按文件名：`crates/qx-api/src/` 下每一个写着 `ApiResponse::`
/// 或 `error_json(` 的 `.rs` 都进扫描集，将来再拆一个模块也会自动被收进来，不需要回来改
/// 这份名单。两条地板断言是空转守卫——扫描集缩到只剩 lib.rs（有人把子模块改成不写
/// `ApiResponse::` 的形态）或一份都取不到时，宁让判据先红。
pub(crate) fn read_face_source() -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("crates")
        .join("qx-api")
        .join("src");
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("读不到 qx-api 源码目录 {}: {error}", dir.display()));
    let mut files: Vec<(String, String)> = Vec::new();
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| panic!("读 qx-api 源码目录项失败: {error}"))
            .path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string();
        let source = workspace_source(&format!("crates/qx-api/src/{name}"));
        if source.contains("ApiResponse::") || source.contains("error_json(") {
            files.push((name, source));
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let names = files
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert!(
        names.contains(&"lib.rs"),
        "读面扫描集里没有 lib.rs，取数口径先坏了：{names:?}"
    );
    assert!(
        files.len() >= 2,
        "读面扫描集只剩 {} 份（{names:?}）：qx-api 的读面至少由 crate 根与 WebSocket 会话层\
         两份源码构成，只剩一份说明子模块改成了不带 `ApiResponse::` 的形态，扫描会静默缩小",
        files.len()
    );
    files
        .into_iter()
        .map(|(_, source)| source)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 入口那一格里反引号包住的路径。
///
/// `GET`/`POST` 前缀是可选的（同一格并列第二条路径时不带，例如
/// `` `GET /account/orders`、`/account/positions` ``），`[?…]` 与 `?after=` 之后的部分不算路径。
pub(crate) fn backticked_routes(cell: &str) -> Vec<String> {
    let mut routes = Vec::new();
    let mut rest = cell;
    while let Some(open) = rest.find('`') {
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('`') else {
            break;
        };
        let token = after_open[..close].trim();
        rest = &after_open[close + 1..];
        let without_method = token
            .strip_prefix("GET ")
            .or_else(|| token.strip_prefix("POST "))
            .unwrap_or(token);
        let path = without_method
            .chars()
            .take_while(|c| *c != '[' && *c != '?' && !c.is_whitespace())
            .collect::<String>();
        if path.starts_with('/') && !routes.contains(&path) {
            routes.push(path);
        }
    }
    routes
}

/// 表头之后那一段表格的第一列里出现的全部路由（按文档顺序，不去重到集合以外）。
///
/// 表格按"下一行不再是 `| ` 开头"截断，而不是按下一处标题截断：第二张表夹在正文中间，
/// 表尾后面紧跟的是散文，两种截法在这里结果相同，但按行截断不需要为每张表另配一个结束标记。
fn table_routes(header: &str, expected_at_least: usize) -> Vec<String> {
    let readme = workspace_source("deploy/README.md");
    let start = readme
        .find(header)
        .unwrap_or_else(|| panic!("deploy/README.md 缺少表头为 {header:?} 的端点表"));
    let mut routes = Vec::new();
    for line in readme[start..].lines().skip(1) {
        let Some(rest) = line.strip_prefix("| ") else {
            break;
        };
        let first_cell = rest.split('|').next().unwrap_or_default().trim();
        for route in backticked_routes(first_cell) {
            assert!(
                !routes.contains(&route),
                "表 {header:?} 把 {route} 列了两遍"
            );
            routes.push(route);
        }
    }
    assert!(
        routes.len() >= expected_at_least,
        "表 {header:?} 只解析出 {} 条路由，少于这张表应有的 {expected_at_least} 条——解析口径需要先修",
        routes.len()
    );
    routes
}

/// `handle_inner` 分派区间里写出的 `(方法, 路由)` 集合。
///
/// 区间与 [`dispatch_routes`] 同源（见 [`dispatch_region`]）：路由名对上而方法对上不算对上，
/// 文档里 `POST /health` 这种写错方法的入口，`dispatch_routes` 那把尺子是看不见的。
pub(crate) fn dispatch_method_routes() -> BTreeSet<(String, String)> {
    let dispatch = dispatch_region();
    let mut pairs = BTreeSet::new();
    // 方法名与搜索前缀成对写出：从前缀串里反推方法名要把 `"GET", "` 那一段切开，
    // 上一版的切片长度差一颗引号，于是 pairs 里全是 `("GET\"", …)`，散文核对那边逐条判成幽灵。
    for (method, prefix) in [("GET", "(\"GET\", \""), ("POST", "(\"POST\", \"")] {
        let mut cursor = 0_usize;
        while let Some(found) = dispatch[cursor..].find(prefix) {
            let quote = cursor + found + prefix.len() - 1;
            let end = dispatch[quote + 1..].find('"').expect("路由字面量必须闭合") + quote + 1;
            pairs.insert((method.to_string(), dispatch[quote + 1..end].to_string()));
            cursor = end;
        }
    }
    assert!(
        pairs.len() >= 17,
        "分派区间里只数出 {} 条 (方法, 路由)，比端点表声称的 17 条还少——取数区间被改窄了",
        pairs.len()
    );
    for (method, _) in &pairs {
        assert!(
            method == "GET" || method == "POST",
            "分派集合里出现方法名 {method:?}：取数口径把方法名切错了，散文路由核对会整批判成幽灵"
        );
    }
    pairs
}

/// `handle_inner` 的分派区间里写出的路由集合（按源码文本逐条数）。
///
/// 取"从 `fn handle_inner(` 到那条 `_ => ApiResponse::text(404, ...)` 兜底"而不是整份文件：
/// 用例里也有 `handle("GET", "/…")` 这样的调用，把它们当路由会让文档被迫去解释一条不存在的入口；
/// 而按第一次 `#[cfg(test)]` 截断是本项目明确禁掉的取数口径（V12 §16 的元判据）。
pub(crate) fn dispatch_routes() -> BTreeSet<String> {
    dispatch_method_routes()
        .into_iter()
        .map(|(_, route)| route)
        .collect()
}

/// [`dispatch_routes`] 与 [`dispatch_method_routes`] 共用的取数区间。
fn dispatch_region() -> String {
    let api = workspace_source("crates/qx-api/src/lib.rs");
    let start = api
        .find("fn handle_inner(")
        .expect("qx-api 必须有一个 handle_inner 分派入口");
    let fallback = api
        .find("_ => ApiResponse::text(404, \"not found\")")
        .expect("qx-api 的路由分派必须有一条 404 兜底");
    assert!(
        fallback > start,
        "404 兜底不在 handle_inner 之后：这条用例的取数区间需要重新界定"
    );
    api[start..fallback].to_string()
}

/// 「### HTTP 读面与控制面路由」那张写 200 响应体形状的表。
pub(crate) const RETURN_TABLE_HEADER: &str = "| 入口 | 返回 | 说明 |";
/// 「### HTTP 读面与控制面路由」里那张写语义与非 200 口径的表（住在第一张表之后，见 #183）。
const SEMANTICS_TABLE_HEADER: &str = "| 端点 | 语义 | 非 200 口径 |";
/// 两张表共用的核对小节，第九遍起也住在同一章里。
const CHECK_SECTION_HEADING: &str = "### 端点表按张核对";

#[test]
fn each_endpoint_table_lists_exactly_the_dispatch_routes() {
    let routes = dispatch_routes();
    for (header, documented) in [
        (RETURN_TABLE_HEADER, table_routes(RETURN_TABLE_HEADER, 17)),
        (
            SEMANTICS_TABLE_HEADER,
            table_routes(SEMANTICS_TABLE_HEADER, 17),
        ),
    ] {
        let documented = documented.into_iter().collect::<BTreeSet<String>>();
        let only_in_table = documented
            .iter()
            .filter(|route| !routes.contains(*route))
            .cloned()
            .collect::<Vec<_>>();
        let only_in_impl = routes
            .iter()
            .filter(|route| !documented.contains(*route))
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            only_in_table.is_empty() && only_in_impl.is_empty(),
            "表 {header:?} 与 handle_inner 的分派集合不平：只在这张表 {only_in_table:?} / 这张表里\
             没有的实现路由 {only_in_impl:?}。门禁按两张表的并集比，所以整行删一条它不会红——\
             这条判据按表逐一比。"
        );
    }
}

/// 第二张表里 `(路由, 「非 200 口径」那一格)` 的全部行。
///
/// 路由取自第一格，口径取自第三格；表仍按"下一行不再是 `| ` 开头"截断，与 [`table_routes`] 同口径。
pub(crate) fn semantics_cells() -> Vec<(String, String)> {
    let readme = workspace_source("deploy/README.md");
    let start = readme
        .find(SEMANTICS_TABLE_HEADER)
        .expect("deploy/README.md 缺少「非 200 口径」那张端点表");
    let mut rows = Vec::new();
    for line in readme[start..].lines().skip(1) {
        let Some(rest) = line.strip_prefix("| ") else {
            break;
        };
        let cells = rest.split('|').collect::<Vec<_>>();
        assert!(cells.len() >= 3, "「非 200 口径」表这一行不足三格：{line}");
        for route in backticked_routes(cells[0]) {
            rows.push((route, cells[2].trim().to_string()));
        }
    }
    assert!(
        rows.len() >= 17,
        "「非 200 口径」表只解析出 {} 行，少于这张表应有的 17 行——解析口径需要先修",
        rows.len()
    );
    rows
}

/// 反引号里的全部字面量（不做路径裁剪，调用方自己筛）。
fn backticked_literals(cell: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut rest = cell;
    while let Some(open) = rest.find('`') {
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('`') else {
            break;
        };
        literals.push(after_open[..close].trim().to_string());
        rest = &after_open[close + 1..];
    }
    literals
}

/// 小写下划线形态的"错误码名"：文档里反引号包住的这类字面量都在承诺 `{"error": …}` 的取值。
///
/// 排除大写形态是有原因的：`ControlError` 的 `DuplicateRequest` 那几个变体名经 `{error:?}` 进正文，
/// 是 Rust 类型面而不是稳定契约，不该被这条判据要求实现里出现同名串。`len >= 6` 挡掉 `error`
/// 这类字段名（它是键名，不是取值）。
fn error_code_names(cell: &str) -> Vec<String> {
    backticked_literals(cell)
        .into_iter()
        .filter(|name| {
            name.len() >= 6
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
        .collect()
}

/// 读面真能写进 `{"error": …}` 的错误码名，从 `crates/qx-api/src/lib.rs` 派生。
///
/// 三个来源形态：`error_json("token")`、`error_json(&format!("token: {error}"))`（限流后端与控制队列
/// 那两处把原因拼在后缀里），以及直写的 `"{\"error\":\"token\"}"`。经变量转手的
/// （`error_json(&error)`、`error_json(&format!("{error:?}"))`）不在此列 —— 它们的取值不是字面量，
/// 也就无从按名核对，第二张表那一格对此的写法是"某某的 Debug 形态"而不是承诺一个码名。
fn emitted_error_names() -> BTreeSet<String> {
    let api = read_face_source();
    const PREFIXES: [&str; 3] = [
        "error_json(\"",
        "error_json(&format!(\"",
        // 直写的 JSON 字面量：源码里就是 {\"error\":\"token\"} 这串带反斜杠的字符。
        "{\\\"error\\\":\\\"",
    ];
    let mut names = BTreeSet::new();
    for prefix in PREFIXES {
        let mut cursor = 0_usize;
        while let Some(found) = api[cursor..].find(prefix) {
            let start = cursor + found + prefix.len();
            let run = api[start..]
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
                .collect::<String>();
            if run.len() >= 6 {
                names.insert(run);
            }
            cursor = start;
        }
    }
    names
}

/// 读面直接写出的非 200 状态码（`ApiResponse::json(` / `text(` 后面那个整数字面量）。
pub(crate) fn emitted_statuses() -> BTreeSet<u16> {
    let api = read_face_source();
    let mut statuses = BTreeSet::new();
    for marker in ["ApiResponse::json(", "ApiResponse::text("] {
        let mut cursor = 0_usize;
        while let Some(found) = api[cursor..].find(marker) {
            let start = cursor + found + marker.len();
            let digits = api[start..]
                .chars()
                .skip_while(|c| c.is_whitespace())
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>();
            if let Ok(code) = digits.parse::<u16>() {
                if code != 200 {
                    statuses.insert(code);
                }
            }
            cursor = start;
        }
    }
    statuses
}

/// 那一格里出现的三位状态码。
///
/// 200 被放行：这一列叫「非 200 口径」，但有两格顺手写了"无快照时 200 空数组"这种正向口径，
/// 那不是我在这条判据里要核对的承诺。
fn documented_statuses(cell: &str) -> Vec<u16> {
    let mut codes = Vec::new();
    let digits = cell
        .chars()
        .map(|c| if c.is_ascii_digit() { c } else { ' ' })
        .collect::<String>();
    for run in digits.split(' ').filter(|run| run.len() == 3) {
        if let Ok(code) = run.parse::<u16>() {
            if code != 200 {
                codes.push(code);
            }
        }
    }
    codes
}

/// #182 判据：第二张表「非 200 口径」那一格点名的错误码名与状态码，必须与实现两讫。
///
/// 立案时实测：`crates/qx-api/src/lib.rs` 的错误体字面量共 8 个码名，文档只点名 5 个 ——
/// `forbidden`（已认证但策略给不出权限的 403）与 `control_state_unavailable`（队列不可用的 503）
/// 在全仓文档里没有任何名字，而 `POST /control/commands` 那一格原先写着「403；503 队列不可用」。
/// 同一条路上其实有**两个不同码名的 403**，客户端按 `error` 分支写代码就会把"没登录"与"没权限"
/// 合成一件事。此前的核对口径只到路由名（#176/#180），这一格没人管。
#[test]
fn non_200_column_names_match_the_read_face_implementation() {
    let emitted_names = emitted_error_names();
    assert!(
        emitted_names.len() >= 8,
        "从 qx-api 只数出 {} 个错误码名，比立案时的 8 个还少——取数形态需要先修，否则这条判据是空转：{emitted_names:?}",
        emitted_names.len()
    );
    let rows = semantics_cells();
    let readme = workspace_source("deploy/README.md");
    let statuses = emitted_statuses();

    // 正向：文档那一格承诺的码名与状态码，实现必须真产得出。
    let mut documented_names = BTreeSet::new();
    for (route, cell) in &rows {
        for name in error_code_names(cell) {
            documented_names.insert(name.clone());
            assert!(
                emitted_names.contains(&name),
                "{route} 的「非 200 口径」承诺了错误码 `{name}`，但读面的错误体字面量里没有它：\
                 客户端按这一格分支就会走进一条实现永不返回的路径"
            );
        }
        for code in documented_statuses(cell) {
            assert!(
                statuses.contains(&code),
                "{route} 的「非 200 口径」写着 {code}，但读面没有一处直接写出这个状态码"
            );
        }
    }

    // 反向：实现产出的码名与非 200 状态码，文档必须有名字。
    let missing = emitted_names
        .difference(&documented_names)
        .filter(|name| !readme.contains(&format!("`{name}`")))
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "读面会写进 `{{\"error\": …}}` 的这些码名在接口文档里没有任何名字：{missing:?}。\
         #182 立案时 `forbidden` 与 `control_state_unavailable` 就是这样漏掉的——同一状态码上两个\
         不同码名，客户端无法按 `error` 分支区分。"
    );
    let undocumented = emitted_statuses()
        .into_iter()
        .filter(|code| !readme.contains(&code.to_string()))
        .collect::<Vec<_>>();
    assert!(
        undocumented.is_empty(),
        "读面直接写出的这些非 200 状态码在接口文档里没出现：{undocumented:?}"
    );

    // 双向点名：这条判据自己也要写在文档里，否则删判据不会有任何文档腐坏。
    assert!(
        readme.contains("non_200_column_names_match_the_read_face_implementation"),
        "接口文档不再点名本判据：读者看不出「非 200 口径」那一格是有常驻核对的"
    );
}

/// #183 判据：两张表与「按张核对」小节一起住在 `## Paper API` 章节内，且各自只有一份。
///
/// 立案时的现状（第八遍收口后清点文档结构时看到）：`### 端点表按张核对` 连同第二张表整段挂在
/// `## Outbox 与 NATS JetStream` 下。这不是排版洁癖——那张表的第三格是运维配告警与错误分支的
/// 依据，而本文件前两条判据都按整篇文档搜表头取数，所以"搬错章节"这件事在既有核对口径下
/// 永远不会红，也没有任何文档承诺过它住在哪儿。
#[test]
fn endpoint_tables_and_their_check_section_live_under_paper_api() {
    // `deploy/README.md` 是纯 CRLF 文件：这条判据要按行首取锚点，先把行尾折成 LF，
    // 否则 "\n## Paper API\n" 这种带换行的 needle 在真树上恒为 0 命中（第九遍 dry-run 实测）。
    let raw = workspace_source("deploy/README.md");
    let readme = if raw.contains("\r\n") {
        raw.replace("\r\n", "\n")
    } else {
        raw.clone()
    };

    let chapter_heading = "\n## Paper API\n";
    let chapter_start = unique_index(&readme, chapter_heading, "一级标题「## Paper API」");
    let body_start = chapter_start + chapter_heading.len();
    let body = &readme[body_start..];
    let chapter_end = body_start
        + body.find("\n## ").unwrap_or_else(|| {
            panic!("Paper API 章节之后必须还有别的一级标题，否则这条判据取不出右边界")
        });
    assert!(
        chapter_end > body_start,
        "Paper API 章节按这条判据的口径是空的：右边界 {chapter_end} 不在正文起点 {body_start} 之后"
    );

    // 锚点一律带换行钉成"整行"：正文里会用「」引用这些标题与表头（本文件的点名段就是这么写的），
    // 按裸子串数就会把引用当成第二份实体。
    let anchors = [
        ("写「返回」形状的表", format!("\n{RETURN_TABLE_HEADER}\n")),
        (
            "写「非 200 口径」的表",
            format!("\n{SEMANTICS_TABLE_HEADER}\n"),
        ),
        ("按张核对小节", format!("\n{CHECK_SECTION_HEADING}\n")),
        (
            "404 兜底那句声明",
            "\n`serve` 暴露的端点就是下表这些，未列出的路径一律 404。".to_string(),
        ),
    ];
    for (label, anchor) in &anchors {
        let hits = readme.matches(anchor.as_str()).count();
        assert_eq!(
            hits, 1,
            "{label}的锚点「{anchor}」按整行数出 {hits} 次，应为 1 次：\
             两张端点表与核对小节各只该有一份，在别的章节再抄一份就会让读者面对两套口径"
        );
        let at = unique_index(&readme, anchor, label);
        assert!(
            at > chapter_start && at < chapter_end,
            "{label}不在 `## Paper API` 章节内（锚点在第 {at} 字节，章节范围 {chapter_start}..{chapter_end}）。\
             #183 立案时它挂在 `## Outbox 与 NATS JetStream` 下——按整篇文档取数的判据看不见这件事。"
        );
    }

    let return_at = unique_index(&readme, &anchors[0].1, "表一");
    let semantics_at = unique_index(&readme, &anchors[1].1, "表二");
    let section_at = unique_index(&readme, &anchors[2].1, "按张核对小节");
    assert!(
        return_at < semantics_at && semantics_at < section_at,
        "章节内三样东西的相对次序变了：表一 {return_at} / 表二 {semantics_at} / 核对小节 {section_at}。\
         「按张核对」那段以「上面那张」与「本节这张表」指代两张表，次序一翻指代就落到别的表上。"
    );

    // 双向点名：文档要说出这条判据的存在，判据也要认自己的定义点。
    assert!(
        readme.contains("endpoint_tables_and_their_check_section_live_under_paper_api"),
        "接口文档不再点名本判据：读者看不出这两张表的位置是有常驻核对的"
    );
    let own_source = workspace_source("crates/qx-cli/src/tests/api_endpoint_table_routes.rs");
    // needle 由 format 拼出来：直接写字面量的话这一行本身就含 `fn …(`，定义点会被数成 2（自指）。
    let definition = format!(
        "fn {}(",
        "endpoint_tables_and_their_check_section_live_under_paper_api"
    );
    let definitions = own_source.matches(definition.as_str()).count();
    assert_eq!(
        definitions, 1,
        "本判据的定义点数出 {definitions} 处，应为 1 处：0 处是判据被改名或搬走却没更新文档，\
         2 处是用例文件被复制出第二份"
    );
}

/// 锚点在全文里唯一时返回它的字节偏移；不唯一直接把用例判红，避免调用方拿到"第一处"却以为在别处。
fn unique_index(haystack: &str, needle: &str, label: &str) -> usize {
    let hits = haystack.matches(needle).count();
    assert_eq!(
        hits, 1,
        "{label}的锚点「{needle}」出现 {hits} 次，应为 1 次"
    );
    haystack.find(needle).expect("上面已经断言命中一次")
}
