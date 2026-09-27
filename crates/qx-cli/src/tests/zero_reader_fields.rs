use super::*;

/// 从源码里切出 `pub struct NAME { … }` 整块（含收尾花括号）。
fn struct_block(source: &str, name: &str) -> String {
    let header = format!("pub struct {name} {{");
    let start = source
        .find(&header)
        .unwrap_or_else(|| panic!("源码里找不到 `{header}`"));
    let rest = &source[start..];
    let end = rest
        .find("\n}")
        .unwrap_or_else(|| panic!("{name} 的结构体块没有列首收尾花括号"));
    rest[..end + 2].to_string()
}

/// #170 判据：`ingest` 的回执只留有人读的那三格，且不再整档算摘要。
///
/// 改前：`RuntimeIngestReceipt` 有五格，其中 `primary_seq` 与 `log_digest` 由三个构造点
/// 每笔都写，却在全仓（含用例）零读者 —— 而 `log_digest` 要为一次没人读的播报把整条
/// 事实流重哈希一遍，正是 #169 那条 O(n²) 账上的一笔。
#[test]
fn ingest_receipt_carries_only_the_fields_someone_reads() {
    let pipeline = workspace_source("crates/qx-runtime/src/pipeline.rs");
    let receipt = struct_block(&pipeline, "RuntimeIngestReceipt");
    let mut fields = receipt
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("pub ") && line.contains(':'))
        .collect::<Vec<_>>();
    fields.sort();
    assert_eq!(
        fields,
        [
            "pub deduplicated: bool,",
            "pub derived_seqs: Vec<u64>,",
            "pub engine_ts: u64,"
        ],
        "#170 删的是回执里两格零读者字段，名册要钉住：{fields:?}"
    );
    assert!(
        !pipeline.contains("primary_seq"),
        "#170 的 `primary_seq` 回来了：它在生产与用例里都没有读者"
    );
    assert_eq!(
        pipeline.matches(".digest()").count(),
        1,
        "整档摘要只该在 `snapshot()` 里算一次；每笔 ingest 再算一遍就是给零读者字段付全量重放的价钱"
    );
}

/// 该 `limitations` 名单里是否真有一条以 `limitation` 命名的登记项。
///
/// 不按"整份文件里有没有这个子串"判：把键名抄进注释、或写进别的能力的说明里，
/// 子串判据就照样绿（先例：#136 的解释器外传判据被报错提示语骗绿）。这里只认
/// `capabilities.yaml` 中该能力自己 `limitations` 名单下的列表项。
fn limitation_registered(source: &str, capability: &str, limitation: &str) -> bool {
    let lines = source.lines().collect::<Vec<_>>();
    let header = format!("  {capability}:");
    let at = lines
        .iter()
        .position(|line| *line == header)
        .unwrap_or_else(|| panic!("capabilities.yaml 里找不到能力 `{capability}`"));
    let prefix = format!("- {limitation}");
    lines[at + 1..]
        .iter()
        .take_while(|line| line.trim().is_empty() || line.starts_with("    "))
        .skip_while(|line| line.trim() != "limitations:")
        .any(|line| line.trim_start().starts_with(&prefix))
}

/// #171 判据：风控投影三格的"零生产读者"要与文档、capabilities 同时成立或同时不成立。
///
/// 这一格每笔订单都算，但 `qx-execution` 的两个 `RiskPort` 只读 `allowed` / `violations`
/// / `rule_set_version`，所以"这一单成交后仓位与保证金会变成多少"今天读不到产物。
/// 按 #118/#119 先例保留不删（删面等于把缺口藏起来），但要双向钉住：接上真读者后必须
/// 摘掉 limitation 与那句"没有生产读者"，反过来偷偷删登记也判红。
#[test]
fn order_risk_projection_fields_stay_documented_as_unread_in_production() {
    const PROJECTION_FIELDS: [&str; 3] = [
        "projected_position_raw",
        "projected_margin_raw",
        "reference_price_raw",
    ];
    let readers = all_crate_production_sources()
        .into_iter()
        .filter(|path| !path_under_crate(path, "qx-risk"))
        .filter(|path| {
            let source = std::fs::read_to_string(path).unwrap();
            // 刻意按"提及即算读者"的宽松口径数：宁可让下一次改名/挪位惊动人，
            // 也不要靠 `.` 前缀之类形状把解构读法漏掉。
            PROJECTION_FIELDS.iter().any(|field| source.contains(field))
        })
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let documented = limitation_registered(
        &workspace_source("maturity/capabilities.yaml"),
        "canonical_order_risk_decision",
        "order_risk_projection_fields_have_no_production_reader",
    );
    assert_eq!(
        documented,
        readers.is_empty(),
        "投影三格的生产读者清单与 capabilities limitation 必须同进同退：\
         现在读者={readers:?}，登记={documented}。接上读者就摘掉登记并写进产物，\
         别把已登记的缺口改掉"
    );
    assert_eq!(
        workspace_source("crates/qx-risk/src/lib.rs").contains("没有任何生产读者"),
        readers.is_empty(),
        "`OrderRiskDecision` 的文档得跟着读者走：有读者时这句就是假话，没读者时删掉它就是漏登记"
    );
}
