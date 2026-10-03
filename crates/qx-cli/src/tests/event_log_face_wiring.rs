use super::*;

/// #169a/#169b 的接线判据：读面与写面必须各自声明自己要哪一种 Outbox 恢复。
///
/// 改前只有一句注释说"读模型打开 EventLog 只为算查询"，而 `open_with_store` 只要日志
/// 非空就整段补投影（文件后端 = 全局锁 + 每条事实一个 Outbox 文件）。于是每个
/// `GET /account/snapshot?account_id=&venue_id=`、每次策略上下文读取、每个 API 轮询 tick 都在重写
/// Outbox：本轮实测 4000 条日志的一次读打开写 4000 个文件、花 5.49s。
/// 现在两面由 `OutboxRecovery` 分开，且 qx-cli 的每个调用点都必须显式说出自己是哪一面。
///
/// 只测行为用例不会发现"某个读调用点被改回写面分支"（读面仍然读得到正确结果），
/// 所以这里按文件逐处数分派。
#[test]
fn event_log_read_and_write_faces_are_declared_at_every_open_site() {
    // 读面：三处账户级读模型 + 一处策略绑定 + 一处策略契约，全部显式 ReadOnly，
    // 且一处都不许出现写面分支。
    for (path, want) in [
        ("crates/qx-cli/src/api_service.rs", 3_usize),
        ("crates/qx-cli/src/strategy_binding.rs", 1),
        ("crates/qx-cli/src/strategy_contract.rs", 1),
    ] {
        let source = workspace_source(path);
        assert_eq!(
            source.matches("OutboxRecovery::ReadOnly").count(),
            want,
            "{path} 的读面声明份数不对（期望 {want}）：少一处说明有读调用点回到了会写 Outbox 的入口"
        );
        assert_eq!(
            source.matches("OutboxRecovery::ReprojectOnOpen").count(),
            0,
            "{path} 里出现了写面声明：这条链路是读模型，补投影会把读请求变成写请求"
        );
    }
    // API 轮询桥（读面）与 Paper 行情桥（写面）在同一个文件里，各恰好一处。
    let bridges = workspace_source("crates/qx-cli/src/market_bridges.rs");
    assert_eq!(
        bridges.matches("storage.open_read_only(").count(),
        1,
        "API 轮询桥的读面入口丢了：它每 tick 投影整本日志，正是本轮量出来的那笔开销"
    );
    assert_eq!(
        bridges.matches("storage.open(").count(),
        1,
        "Paper 行情桥要走写面入口（它确实往账户日志追加行情），份数变了要按新名册改判据"
    );

    // 写面：提交与 worker 仍然显式要求补投影 —— 本轮不是把恢复能力删了换性能。
    for (path, want) in [
        ("crates/qx-cli/src/venue_runtime/paper_submit.rs", 2_usize),
        ("crates/qx-cli/src/venue_runtime/paper_worker.rs", 6),
    ] {
        let source = workspace_source(path);
        assert_eq!(
            source.matches("OutboxRecovery::ReprojectOnOpen").count(),
            want,
            "{path} 的写面声明份数不对（期望 {want}）：少一处就说明某条会追加事实的链路\
             被改成了只读打开，崩溃留下的 Outbox 缺口没人补"
        );
        assert_eq!(
            source.matches("OutboxRecovery::ReadOnly").count(),
            0,
            "{path} 是写面，出现 ReadOnly 等于放弃崩溃缺口恢复"
        );
    }

    // 分派层：三个后端 × 两个面都要有分支，且两个公开入口各绑定一个面。
    // 这一层住在 `runtime_wiring/pipeline_storage.rs`（第十七遍从 runtime_wiring.rs 拆出，
    // 父文件当时越过 500 行门槛），所以按路径取数的判据要跟着搬到子文件。
    let wiring = workspace_source("crates/qx-cli/src/runtime_wiring/pipeline_storage.rs");
    for (needle, want) in [
        ("OutboxRecovery::ReprojectOnOpen =>", 3),
        ("OutboxRecovery::ReadOnly =>", 3),
        (
            "self.open_with(log_name, currency, OutboxRecovery::ReadOnly)",
            1,
        ),
        (
            "self.open_with(log_name, currency, OutboxRecovery::ReprojectOnOpen)",
            1,
        ),
        ("LiveEventPipeline::open_read_only(", 1),
        ("LiveEventPipeline::open_configured(", 1),
    ] {
        assert_eq!(
            wiring.matches(needle).count(),
            want,
            "分派层形状变了：`{needle}` 期望 {want} 处。少一个分支就是某一条后端的两个面被静默合并\
             （postgres/sqlite/文件三条后端各自两支）"
        );
    }
    assert_eq!(
        wiring.matches("recovery: OutboxRecovery").count(),
        1,
        "分派层只应该有一处 `recovery` 形参（`open_with`），多一处说明面又被搬到别处各定一份"
    );
    // 拆出去的分派层必须仍挂在父模块上，否则它是编译不可见的死源码：判据照样绿，
    // 生产走的还是旧的那一份。
    let parent = workspace_source("crates/qx-cli/src/runtime_wiring.rs");
    for needle in [
        "mod pipeline_storage;",
        "pub(crate) use pipeline_storage::PipelineStorage;",
    ] {
        assert_eq!(
            parent.matches(needle).count(),
            1,
            "分派层的挂载行 `{needle}` 份数不对：缺一半就是拆出去的模块没人用"
        );
    }
    assert_eq!(
        parent.matches("recovery: OutboxRecovery").count(),
        2,
        "两个统一入口（`open_runtime_pipeline` 与 `open_account_pipeline`）必须各自把面收成形参，\
         少一处就是某条链又靠注释决定自己是读还是写"
    );

    // 内核：补投影只在写面分支里发生；投影游标必须在序列化之前过滤。
    let pipeline = workspace_source("crates/qx-runtime/src/pipeline.rs");
    assert_eq!(
        pipeline
            .matches("if recovery == OutboxRecovery::ReprojectOnOpen && !log.is_empty()")
            .count(),
        1,
        "#169a 的核心判据不在了：补投影若回到无条件执行，每个读打开又会整段重写 Outbox"
    );
    assert_eq!(
        pipeline
            .matches("project_event_log_to_outbox(name, log, projection_cursor)")
            .count(),
        1,
        "#169b 的游标没有传进投影：写面回到全量序列化"
    );
    assert!(
        !pipeline.contains(".retain("),
        "游标过滤又退回\"先全量序列化再丢弃\"（`.retain(`）：每笔追加都为已投递的事实付 serde"
    );

    let storage = workspace_source("crates/qx-storage/src/lib.rs");
    assert_eq!(
        storage.matches("projection_cursor: u64").count(),
        1,
        "`project_event_log_to_outbox` 的游标参数没了"
    );
    assert_eq!(
        storage
            .matches("filter(|event| event.seq >= projection_cursor)")
            .count(),
        1,
        "游标过滤必须在 `serde_json::to_string` 之前，否则过滤只是装饰"
    );
    // 顺序本身要核对：切片从投影函数开头起，第一次出现的 `to_string` 就是本函数里的那次。
    let projection_body = &storage[storage
        .find("pub fn project_event_log_to_outbox")
        .expect("投影函数不在读到的文件里")..];
    let filter_at = projection_body
        .find("filter(|event| event.seq >= projection_cursor)")
        .expect("游标过滤不在投影函数里");
    let serde_at = projection_body
        .find("serde_json::to_string(event)")
        .expect("投影函数里没有序列化调用");
    assert!(
        filter_at < serde_at,
        "游标过滤（偏移 {filter_at}）排在序列化（偏移 {serde_at}）之后：那还是全量序列化再丢弃，\
         本轮量到的那笔白算没被拿掉"
    );
    // 日志名合法性由那份共用规则给出，不允许在本函数里再抄一份字面量。
    assert_eq!(
        storage.matches("validate_segment_name(log_name)?;").count(),
        1,
        "投影函数改回自带字面量校验：合法字符集就有了第二份口径（V13 R1-A3 同一类复发）"
    );

    // #169c：API 前缀核对必须按序号二分。行为用例对两种实现都绿（语义一致），
    // 所以这一格只能按形状数——线性 `find` 回来时行为不变、代价回到 O(n²)。
    let api = workspace_source("crates/qx-api/src/lib.rs");
    assert_eq!(
        api.matches("partition_point(|current| current.seq < seq)")
            .count(),
        1,
        "#169c 的二分定位没了：API 轮询桥每 tick 投影整本日志，前缀核对回到线性扫\
         （本轮实测 16000 条时二次投影 0.6038s，倍率 3.87/4.18/4.32）"
    );
    assert_eq!(
        api.matches("find(|current| current.seq == event.seq)")
            .count(),
        0,
        "两处按序号的线性 `find` 又回来了：行为等价，但长跑是 O(n²)"
    );
    assert_eq!(
        api.matches("projected_event(self.events.events(), event.seq)")
            .count(),
        2,
        "两条投影链（账户级与全局兼容）必须共用同一份按序号定位，份数不对说明有一条被改回去了"
    );
}
