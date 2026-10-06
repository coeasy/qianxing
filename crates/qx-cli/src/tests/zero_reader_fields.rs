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

/// #178 判据：`qx_pipeline_*` 有没有生产出口，源码说明、capabilities、接口文档三处必须一起改口。
///
/// 这六个计数在 `ingest`/`refresh` 里每笔都加。第八遍立案时全仓对 `LiveEventPipeline::metrics()`
/// 的调用只有 `pipeline.rs` 自己的用例 —— 数据在生产里被算出来、被丢掉，当时按"留而不删 + 登记
/// 缺口"处理，并把三处说法双向钉住（名字里的 `unpublished` 就是那一段）。第二十遍按 worker 进程
/// 定好量纲、接上 `.prom` 出口之后，这条判据的作用不变，只是钉的三处换成了"现在说出口在哪"的
/// 三句话：摘掉旧登记、`qx-runtime` 的公开面指认真实读者、接口文档给出带标签的样本形态。
/// 反向同样咬：把 `pipeline_metrics_report.rs` 的调用拆掉而三处说法留着不动，就判红。
#[test]
fn pipeline_metrics_publication_and_docs_move_together() {
    let readers = all_crate_production_sources()
        .into_iter()
        .filter(|path| {
            !(path_under_crate(path, "qx-runtime")
                && path.file_name().is_some_and(|name| name == "pipeline.rs"))
        })
        .filter(|path| {
            let source = std::fs::read_to_string(path).unwrap();
            // 接上出口必然在持有 `LiveEventPipeline` 的文件里调 `.metrics()`，两处都提到才算：
            // 只按 `.metrics()` 取会把 qx-api 自己那份 `ApiMetricsSnapshot` 的读数数成读者。
            source.contains("LiveEventPipeline") && source.contains(".metrics()")
        })
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    let published = !readers.is_empty();
    assert_eq!(
        limitation_registered(
            &workspace_source("maturity/capabilities.yaml"),
            "paper_execution",
            "pipeline_metrics_have_no_production_surface"
        ),
        !published,
        "`qx_pipeline_*` 的生产读者清单与 capabilities limitation 必须同进同退：\
         现在读者={readers:?}。有读者就不许再登记「没有生产出口」，没读者就得把它加回来"
    );
    assert_eq!(
        workspace_source("crates/qx-runtime/src/lib.rs").contains("pipeline_metrics_report.rs"),
        published,
        "`PipelineMetricsSnapshot` 走的是 qx-runtime 的公开 re-export，说明就得写在那一处：\
         出口在 `pipeline_metrics_report.rs`，读者清单里没有它时这句就是假话"
    );
    assert_eq!(
        workspace_source("deploy/README.md").contains("qx_pipeline_ingested_events_total{worker="),
        published,
        "接口文档「指标出口」那一节是给运维看 `/metrics` 该不该出现 `qx_pipeline_*` 的地方，\
         它必须与读者清单一致：现在读者={readers:?}"
    );
}

/// 判据（发布审计 第2轮）：字段级零读者必须与 capabilities limitation 同进同退。
///
/// #174 缺口的已知形状：静态门禁的 `dead_type_surface_check` 只扫类型与 `pub fn` / `pub const`，
/// 结构体字段既不是类型也不是函数，于是「这一格每笔都写、全仓没人读」在门禁侧恒绿。
/// 按 #118/#171 先例：保留不删（删面等于把缺口藏起来），但要在能力矩阵里逐条登记，
/// 并在这里双向钉住——接上真读者后必须摘掉登记，偷偷删登记也判红。
///
/// 读者的口径刻意收紧到「字段访问/解构」：`field:` 形态是字段声明或结构体字面量构造
/// （`qx-cli` 装配 `JobSpec` 正是这种形态），它把值写进去而不读出来，所以不算读者；
/// 纯注释行也不算。
#[test]
fn zero_reader_struct_fields_stay_registered_in_capabilities() {
    // (字段名, 归属 crate, 能力键, limitation 键)
    const CASES: [(&str, &str, &str, &str); 4] = [
        (
            "volatility_bps",
            "qx-risk",
            "canonical_order_risk_decision",
            "risk_snapshot_carries_fields_no_decision_reads",
        ),
        (
            "net_exposure",
            "qx-risk",
            "canonical_order_risk_decision",
            "risk_snapshot_carries_fields_no_decision_reads",
        ),
        (
            "max_drawdown_raw",
            "qx-xingban",
            "local_backtest",
            "backtest_report_max_drawdown_raw_has_no_reader",
        ),
        (
            "permission_scope",
            "qx-scheduler",
            "paper_execution",
            "job_spec_declaration_fields_have_no_production_reader",
        ),
    ];
    let capabilities = workspace_source("maturity/capabilities.yaml");
    for (field, owner, capability, limitation) in CASES {
        let declaration = format!("{field}:");
        let readers = all_crate_production_sources()
            .into_iter()
            .filter(|path| !path_under_crate(path, owner))
            .filter(|path| {
                let source = std::fs::read_to_string(path).unwrap();
                source.lines().any(|line| {
                    let trimmed = line.trim_start();
                    !trimmed.starts_with("//")
                        && line.contains(field)
                        && !line.contains(&declaration)
                })
            })
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        let registered = limitation_registered(&capabilities, capability, limitation);
        assert_eq!(
            registered,
            readers.is_empty(),
            "字段 `{field}`（{owner}）的生产读者清单与 capabilities limitation `{limitation}` \
             必须同进同退：现在读者={readers:?}，登记={registered}"
        );
    }
}
