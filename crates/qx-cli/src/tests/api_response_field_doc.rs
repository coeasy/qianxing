//! 接口文档「返回」那一格承诺的字段名，必须就是 `qx-api` 真的序列化出来的那一组（V13 R2 第六遍）。
//!
//! 这条用例补的是 `api_surface_doc_check` 的另一半。那颗门禁只比**路由集合**，一个字段都不看，
//! 而且它按整篇 `deploy/README.md` 的并集取数——文档里有两张端点表（一张写 200 响应体形状，
//! 一张写语义/非 200 口径），所以从「返回」这张表里删掉任意一行，路由并集仍然相等，它照印绿。
//! 本轮实测到的三类腐坏全落在这一盲区里：
//! - `/account/snapshot/diff` 那格只写「差异」，而差分的八个汇总钱标量**只**由 `replacement`
//!   搬运；漏读它的客户端会拿基线的权益与费用去核对目标状态哈希。
//! - `/events` 与 `/events/live` 都写「事件数组」，实际一条是裸 `Event`（9 键）、另一条是
//!   `ProjectionEnvelope`（14 键，事件在它的 `data` 里）。
//! - `/account/balances` 把 `cash_raw` 与三个标量并列，而它其实是按币种的 map。
//!
//! 键集两侧都从事实取，不写第二份手工清单：文档侧解析表格那一格，实现侧解析 `handle` 返回的正文。

use super::api_endpoint_table_routes::{backticked_routes, RETURN_TABLE_HEADER};
use super::*;
use std::collections::BTreeSet;

/// 「### HTTP 读面与控制面路由」里那张表的行：路由 → 「返回」那一格的原文。
///
/// 取数范围收到**表本身**：从表头行往下扫，遇到不再以 `| ` 开头的行就停。原先按"到下一个二级标题
/// 为止"截，是因为第二张端点表住在别的章节；第九遍（#183）把它移进同一章之后，那种截法会把两张表
/// 一起算进来（`/health` 立刻被数成列了两遍）。按表截与文档里那张表的邻居是谁无关，也仍然避免了
/// 门禁那个并集口径的错——第二张表根本没有「返回」这一列。
fn endpoint_return_cells() -> Vec<(String, String)> {
    let readme = workspace_source("deploy/README.md").replace("\r\n", "\n");
    let start = readme.find(RETURN_TABLE_HEADER).unwrap_or_else(|| {
        panic!(
            "deploy/README.md 缺少表头为 {RETURN_TABLE_HEADER:?} 的端点表（「### HTTP 读面与控制面路由」那一节）"
        )
    });
    let mut rows: Vec<(String, String)> = Vec::new();
    for line in readme[start..].lines().skip(1) {
        if !line.starts_with("| ") {
            break;
        }
        let cells = line.split('|').map(str::trim).collect::<Vec<_>>();
        assert!(cells.len() >= 3, "「返回」形状表这一行不足三格：{line}");
        for route in backticked_routes(cells[1]) {
            rows.push((route, cells[2].to_string()));
        }
    }
    let mut seen = BTreeSet::new();
    for (route, _) in &rows {
        assert!(
            seen.insert(route.clone()),
            "端点表把 {route} 列了两遍，「返回」那一格就有了两种说法"
        );
    }
    assert!(
        rows.len() >= 14,
        "端点表只解析出 {} 行，明显不是那张 17 条路由的表——解析口径需要先修",
        rows.len()
    );
    rows
}

/// 「返回」那一格里声明的键集：第一个花括号形态的反引号片段，按顶层逗号切分，每段取首个标识符。
///
/// 该格没有花括号就是"这条入口不做字段级承诺"，返回 `None`；有花括号但不是键清单则当场炸——
/// 否则"文档写了一个 `{}`"会安静地退化成没有承诺。
fn documented_keys(cell: &str) -> Option<BTreeSet<String>> {
    let mut rest = cell;
    while let Some(open) = rest.find('`') {
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('`') else {
            break;
        };
        let token = after_open[..close].trim();
        rest = &after_open[close + 1..];
        if !(token.starts_with('{') && token.ends_with('}') && token.len() >= 2) {
            continue;
        }
        let inner = token[1..token.len() - 1].trim();
        assert!(
            !inner.is_empty(),
            "端点表里出现空的键清单 {{}}（整格 {cell:?}）：它不承诺任何字段，判据无从核对"
        );
        let mut keys = BTreeSet::new();
        for part in inner.split(',') {
            let key = part
                .trim()
                .trim_start_matches('"')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>();
            assert!(
                !key.is_empty() && !key.starts_with(|c: char| c.is_ascii_digit()),
                "端点表这一格不是键清单: {part:?}（整格 {cell:?}）"
            );
            assert!(keys.insert(key.clone()), "端点表里重复声明了 {key}");
        }
        return Some(keys);
    }
    None
}

/// 一份最小可用的 API 状态：两条账户快照（差一格现金与一格权益）、三条投影事件、两条总线事件。
///
/// 夹具必须非空：空数组取不出元素键集，那时这条判据会在文档写错时也印绿。
struct Seeded {
    service: ApiService,
    base_hash: u64,
}

fn seeded_service() -> Seeded {
    let mut state = ApiState::default();
    let mut base = AccountSnapshot::new(1, "main", "default", "paper", 20);
    base.cash_raw.insert("USDT".into(), 100_000);
    let base_hash = state.publish_snapshot(base).expect("基线快照必须能发布");
    let mut target = AccountSnapshot::new(2, "main", "default", "paper", 21);
    target.cash_raw.insert("USDT".into(), 99_000);
    target.equity_raw = Some(101_000);
    state.publish_snapshot(target).expect("目标快照必须能发布");
    for step in 0..3u64 {
        let seq = state.events.alloc_seq();
        state.events.append(qx_core::Event::new(
            seq,
            30 + step,
            qx_core::Priority::POST,
            qx_core::EventKind::Settle,
        ));
    }
    for seq in 0..2u64 {
        state
            .event_bus
            .publish(qx_core::Event::new(
                seq,
                40 + seq,
                qx_core::Priority::POST,
                qx_core::EventKind::Settle,
            ))
            .expect("事件总线必须收下夹具事件");
    }
    Seeded {
        service: ApiService::new(state),
        base_hash,
    }
}

/// 这条入口真的返回什么形状，就按那个形状取键集：对象取自己的键，数组取第一条元素的键。
fn response_keys(seeded: &Seeded, route: &str) -> BTreeSet<String> {
    let query = match route {
        "/account/snapshot/diff" => format!("?base_hash={}", seeded.base_hash),
        "/events" | "/events/live" => "?after=0".to_string(),
        _ => String::new(),
    };
    let response = seeded
        .service
        .handle("GET", &format!("{route}{query}"), "", 3);
    assert_eq!(
        response.status, 200,
        "{route} 没有返回 200，读侧形状无从核对: {}",
        response.body
    );
    let body: serde_json::Value = serde_json::from_str(&response.body)
        .unwrap_or_else(|error| panic!("{route} 的响应不是 JSON: {error} / {}", response.body));
    match body {
        serde_json::Value::Object(fields) => fields.keys().cloned().collect(),
        serde_json::Value::Array(items) => {
            let first = items.first().unwrap_or_else(|| {
                panic!("{route} 返回空数组，元素键集取不出来: {}", response.body)
            });
            first
                .as_object()
                .unwrap_or_else(|| panic!("{route} 的数组元素不是对象: {first}"))
                .keys()
                .cloned()
                .collect()
        }
        other => panic!("{route} 返回的是标量 {other}，端点表的键集没有可比对象"),
    }
}

/// 这一轮起被字段级承诺的入口。文档若给别的入口补了键集，用例必须同时驱动它（下面的双向断言）。
const FIELD_PINNED: [&str; 6] = [
    "/health",
    "/ready",
    "/account/balances",
    "/account/snapshot/diff",
    "/events",
    "/events/live",
];

#[test]
fn the_endpoint_table_promises_exactly_the_fields_qx_api_serializes() {
    let rows = endpoint_return_cells();
    let seeded = seeded_service();

    let declared = rows
        .iter()
        .filter_map(|(route, cell)| documented_keys(cell).map(|keys| (route.clone(), keys)))
        .collect::<Vec<_>>();
    for route in FIELD_PINNED {
        assert!(
            declared.iter().any(|(path, _)| path.as_str() == route),
            "{route} 的「返回」那一格不再声明键集：字段级承诺被删掉了，而这条用例只会静默少比一条"
        );
    }
    for (route, _) in &declared {
        assert!(
            FIELD_PINNED.contains(&route.as_str()),
            "端点表给 {route} 声明了键集，用例却没驱动它（补进 FIELD_PINNED，否则这条承诺没人核对）"
        );
    }
    assert_eq!(
        declared.len(),
        FIELD_PINNED.len(),
        "字段级承诺的入口数与用例驱动的入口数不平"
    );

    for (route, expected) in declared {
        let actual = response_keys(&seeded, &route);
        assert_eq!(
            expected,
            actual,
            "{route} 的响应键集与端点表承诺的不一致——只在文档 {only_in_doc:?} / 只在响应 \
             {only_in_body:?}（改任一侧都要连读侧客户端一起改，所以两侧都得点名）",
            only_in_doc = expected.difference(&actual).cloned().collect::<Vec<_>>(),
            only_in_body = actual.difference(&expected).cloned().collect::<Vec<_>>(),
        );
    }
}

/// 文档与告警模板点名的每个指标，必须真有生产者印出同名样本；带标签的样本不许写成 `名字=值`。
///
/// `qx_worker_up` 的线上形态是 `qx_worker_up{worker="<worker_id>"} 0|1`，读它的那一侧
/// （`runtime_wiring.rs` 按 `starts_with("qx_worker_up{")` 认）也以此为前提。接口文档此前写
/// `qx_worker_up=1`：照着它写抓取规则或解析脚本的人会拿到一条永远为空的查询。
#[test]
fn documented_prometheus_metrics_are_the_names_the_runtime_emits() {
    let readme = workspace_source("deploy/README.md");
    let alerts = workspace_source("deploy/prometheus/qianxing-alerts.yml");
    let mut doc_named = BTreeSet::new();
    for token in inline_backtick_tokens(&readme) {
        if let Some(name) = metric_name(&token) {
            doc_named.insert(name);
        }
    }
    let mut alert_named = BTreeSet::new();
    for line in alerts.lines() {
        let Some(expr) = line.trim().strip_prefix("expr:") else {
            continue;
        };
        for word in expr.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if let Some(name) = metric_name(word) {
                alert_named.insert(name);
            }
        }
    }
    assert!(
        doc_named.len() >= 2 && alert_named.len() >= 5,
        "文档侧数出 {} 个指标名、告警模板侧数出 {} 个，明显少于实际承诺的面：解析口径需要先修",
        doc_named.len(),
        alert_named.len()
    );

    let sources = all_crate_production_sources()
        .into_iter()
        .map(|path| {
            std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("读取 {} 失败: {error}", path.display()))
        })
        .collect::<Vec<_>>();
    // `name{{` 是带标签样本在 `format!` 里的写法，`name ` 是无标签样本的写法；只出现在注释里的名字
    // 两种都不满足，所以这条取数不会把"文档提到过一个没人印的名字"读成绿。
    let emitted = |name: &str| {
        sources
            .iter()
            .any(|text| text.contains(&format!("{name}{{")) || text.contains(&format!("{name} ")))
    };
    let labelled = |name: &str| {
        sources
            .iter()
            .any(|text| text.contains(&format!("{name}{{")))
    };

    for name in doc_named.union(&alert_named) {
        assert!(
            emitted(name),
            "{name} 被 deploy/README.md 或告警模板点名，但生产源码里没有任何地方印出这个样本"
        );
    }
    for name in &doc_named {
        if !labelled(name) {
            continue;
        }
        assert!(
            !readme.contains(&format!("{name}=")),
            "{name} 在生产里带 worker 标签，文档却写成 `{name}=…` 的标量形态：\
             照文档写抓取或解析会拿到空样本"
        );
        assert!(
            readme.contains(&format!("{name}{{worker=")),
            "文档点名了带标签的 {name}，就必须给出它的真实形态 `{name}{{worker=\"<worker_id>\"}} …`"
        );
    }
}

/// 反引号片段，但先剥掉 ``` 围栏代码块：围栏里的 `qx_user` 是数据库用户名，不是指标。
fn inline_backtick_tokens(text: &str) -> Vec<String> {
    let mut outside = String::new();
    let mut inside_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            inside_fence = !inside_fence;
            continue;
        }
        if !inside_fence {
            outside.push_str(line);
            outside.push('\n');
        }
    }
    let mut tokens = Vec::new();
    let mut rest = outside.as_str();
    while let Some(open) = rest.find('`') {
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('`') else {
            break;
        };
        tokens.push(after_open[..close].to_string());
        rest = &after_open[close + 1..];
    }
    tokens
}

/// 一段文本是不是指标名：剥掉尾部标签/取值后以 `qx_` 开头，且落在 worker 族或计数/时间/失鲜后缀上。
///
/// 后缀白名单不是为了省事，是为了把 `qx_core`、`qx-cli`、数据库用户名 `qx_user` 这类同前缀的
/// 东西挡在判据外面——它们不是样本名，没有"生产者"可查。
fn metric_name(token: &str) -> Option<String> {
    let name = token
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || *c == '_')
        .collect::<String>();
    if !name.starts_with("qx_") || name.len() <= "qx_worker".len() {
        return None;
    }
    (name.starts_with("qx_worker_")
        || name.ends_with("_total")
        || name.ends_with("_seconds")
        || name.ends_with("_stale"))
    .then_some(name)
}

/// Prometheus 文本必须是**逐行**的 exposition，两处都要守：线上正文与生产源码里的模板。
///
/// 立案的是 V13 R2 第七遍的发布面实测：`qx-cli.exe serve` 的 `/metrics` 返回 200，正文 462 字节
/// 里却一条样本都解析不出来（实测 `logs/s143_*.txt`），因为 `ApiMetricsSnapshot::to_prometheus`
/// 把行分隔写成字面反斜杠 + n（源码里的 `\\n`）而不是真正的换行。整份文本是一行，
/// Prometheus 抓取端解析失败后**不报错**，只是所有基于 `qx_api_*`/`qx_worker_*` 的告警永不触发；
/// 同仓库另一处 `PipelineMetricsSnapshot::to_prometheus` 是同一个写法，它的用例当时用 `contains`
/// 断言，所以两处都不出声。这条判据因此从两头各取一次：真驱动一次 `/metrics` 按行解析，
/// 再把"看起来是样本模板"的源码行扫一遍，不许出现转义写法 `\\n`。
///
/// 第二十遍（#178 接线）之后源码侧只剩一处自己渲染样本的出口：`to_prometheus` 那份第二实现被
/// 删了，pipeline 计数改由 `pipeline_metrics_report.rs` 按 worker 进程写进 `.prom`。所以这里扫的
/// 是"今天所有还在渲染样本的模板"，而不是当初那两处。
#[test]
fn prometheus_exposition_is_line_separated_at_both_ends() {
    let seeded = seeded_service();
    let metrics = seeded.service.handle("GET", "/metrics", "", 3);
    assert_eq!(metrics.status, 200, "/metrics 必须可读: {}", metrics.body);
    assert!(
        metrics
            .content_type
            .starts_with("text/plain; version=0.0.4"),
        "/metrics 的口径是 Prometheus 0.0.4 文本，实际 {:?}",
        metrics.content_type
    );
    let mut samples = Vec::new();
    for line in metrics.body.trim_end_matches('\n').split('\n') {
        assert!(
            !line.contains(r"\n"),
            "正文里出现字面反斜杠 + n：行分隔被写成转义文本，抓取端会把整份正文读成一行:\n{}",
            metrics.body
        );
        if line.is_empty() {
            continue;
        }
        if line.starts_with("# ") {
            continue;
        }
        let (name, value) = line
            .rsplit_once(' ')
            .unwrap_or_else(|| panic!("样本行必须是 `<名> <值>`，实际 {line:?}"));
        assert!(!name.contains(' '), "指标名与值之间只许一个空格: {line:?}");
        value.parse::<f64>().unwrap_or_else(|error| {
            panic!("样本值必须是数字，实际 {line:?} / {error}");
        });
        samples.push(name);
    }
    assert_eq!(
        samples,
        vec![
            "qx_api_requests_total",
            "qx_api_rate_limit_rejected_total",
            "qx_api_authentication_rejected_total",
            "qx_api_command_enqueue_failures_total",
            "qx_control_retired_commands_total",
            "qx_control_retired_audit_records_total",
        ],
        "`/metrics` 的自身样本必须是六条可解析的样本行：四条 API 计数 + 两条控制面退场计数。\
         退场计数一旦没人读，「内存工作集有界」就只是写法，运维面看不出它有没有真发生过"
    );

    // 源码侧看的是三个字符的 `\\n`：那是"渲染出字面反斜杠 + n"的写法。两个字符的 `\n` 在源码里
    // 本来就是行分隔本身，逐行模板里每一行都有——把它当违规会当场把修好的两处出口全判红。
    for path in all_crate_production_sources() {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("读取 {} 失败: {error}", path.display()));
        for line in text.lines() {
            if !looks_like_prometheus_sample_template(line) {
                continue;
            }
            assert!(
                !line.contains(r"\\n"),
                "{} 里这一行把行分隔写成了转义文本（字面反斜杠 + n），抓取端读不出样本:\n  {}",
                path.display(),
                line.trim()
            );
        }
    }
}

/// 一行源码是不是"在渲染一条 Prometheus 样本"：出现 `# HELP`/`# TYPE`，或同时有 `qx_` 指标名与值占位。
fn looks_like_prometheus_sample_template(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("# HELP qx") || trimmed.starts_with("# TYPE qx") {
        return true;
    }
    trimmed
        .strip_prefix('"')
        .unwrap_or(trimmed)
        .starts_with("qx_")
        && trimmed.contains('{')
}
