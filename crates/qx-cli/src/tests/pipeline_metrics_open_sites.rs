//! #178 出口的第二条判据：每一处「打开账户 pipeline」的生产站点，要么把计数并进出口，
//! 要么在豁免名单里逐名点名。
//!
//! 第十九遍「在册不改」第 ③ 条说的是这件事：`absorb` 的调用点位置只有那条真链路夹具在钉，
//! 把它从「对象用完那一刻」挪到另一处同样只用一次的位置仍是少计，而没人管。第二十遍收口时
//! 排除清单还只是出口模块文档里手工抄写的一段，本文件把它变成机器判据 —— 站点集合、并账次数、
//! 豁免名单、出口模块文档四者必须同进同退，少一项就红。
//!
//! 站点认两个入口：`open_account_pipeline(` 与它下面那层 `open_runtime_pipeline(`。只数前者会留
//! 一条绕过路线（`build_configured_api_service` 就是直接调后层的），这正是"名单看起来完整"的成因。

use super::*;

/// 会并进 worker 累计量的函数：函数名 -> 其中的 pipeline 打开站点数。
///
/// 并账次数必须与站点数**逐函数相等**：多一次是双计，少一次是漏计，两边都不会让正文报错。
const COUNTED_OPEN_SITES: [(&str, usize); 2] = [
    ("run_paper_spread_recovery_worker", 1),
    ("run_paper_execution_worker", 3),
];

/// 不并账的函数：函数名 -> 站点数与"为什么不该出现在按 worker 进程累计的量纲里"。
///
/// 这里的每一项都必须在 `pipeline_metrics_report.rs` 的模块文档里点名，反过来也一样。
const EXEMPT_OPEN_SITES: [(&str, usize, &str); 9] = [
    (
        "open_account_pipeline",
        1,
        "统一入口向 open_runtime_pipeline 的委托",
    ),
    ("build_configured_api_service", 1, "只读"),
    ("load_api_query_models", 1, "只读"),
    ("load_api_account_snapshot_for_worker", 1, "只读"),
    ("strategy_current_qty_for", 1, "只读"),
    ("strategy_account_context", 1, "只读"),
    ("run_paper_pipeline_once", 2, "一次性验收入口"),
    ("run_paper_submit_order", 1, "一次性验收入口"),
    ("paper_submit_action", 1, "一次性验收入口拆出的提交尝试"),
];

/// 生产源码里每个函数拥有的 `(打开站点数, absorb 次数)`。
fn production_open_sites() -> BTreeMap<String, (usize, usize)> {
    let mut ledger: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for path in all_crate_production_sources() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut current: Option<String> = None;
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if let Some(name) = function_declared_on(trimmed) {
                current = Some(name);
                // 入口自己的定义行不算站点；它向下一层的委托才算。
                continue;
            }
            let Some(name) = current.clone() else {
                continue;
            };
            let entry = ledger.entry(name).or_default();
            if line.contains("open_account_pipeline(") || line.contains("open_runtime_pipeline(") {
                entry.0 += 1;
            }
            if line.contains(".absorb(") {
                entry.1 += 1;
            }
        }
    }
    ledger.retain(|_, counts| counts.0 > 0);
    ledger
}

/// 这一行声明的函数名（只取第一个 `fn`，注释行已由调用方滤掉）。
fn function_declared_on(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

#[test]
fn account_pipeline_open_sites_are_either_counted_or_exempted_by_name() {
    let ledger = production_open_sites();
    let mut known: Vec<&str> = COUNTED_OPEN_SITES
        .iter()
        .map(|(name, _)| *name)
        .chain(EXEMPT_OPEN_SITES.iter().map(|(name, _, _)| *name))
        .collect();
    known.sort_unstable();
    let mut unique = known.clone();
    unique.dedup();
    assert!(
        unique.len() == known.len(),
        "两份名单里同一个函数登记了两次，判据会同时放过双计与漏登: {known:?}"
    );

    for (name, expected_sites) in COUNTED_OPEN_SITES {
        let (sites, absorbs) = site_counts(name, &ledger);
        assert_eq!(
            sites, expected_sites,
            "{name} 的 pipeline 打开站点数与名单不符（新增/删掉的调用点都要先登记再改代码）"
        );
        assert_eq!(
            absorbs, expected_sites,
            "{name} 打开 {sites} 个 pipeline 却并账 {absorbs} 次：少一次是漏计、多一次是双计，\
             而这族样本一旦上线，抓取端看到的是一条不认账的累计计数"
        );
    }
    for (name, expected_sites, reason) in EXEMPT_OPEN_SITES {
        let (sites, absorbs) = site_counts(name, &ledger);
        assert_eq!(
            sites, expected_sites,
            "{name} 的 pipeline 打开站点数与名单不符（原因：{reason}）"
        );
        assert_eq!(
            absorbs, 0,
            "{name} 在豁免名单里（原因：{reason}）却出现了 absorb：接了出口就要把它移进计数名单，\
             否则计数名单的逐名核对会漏掉它"
        );
    }

    let unregistered: Vec<String> = ledger
        .keys()
        .filter(|name| !known.contains(&name.as_str()))
        .cloned()
        .collect();
    assert!(
        unregistered.is_empty(),
        "生产里出现没有登记的 pipeline 打开站点: {unregistered:?}。要么并进 worker 出口（计数名单），\
         要么说明它为什么不属于「按 worker 进程累计」这个量纲（豁免名单 + 出口模块文档）"
    );

    let outlet_doc = workspace_source("crates/qx-cli/src/pipeline_metrics_report.rs");
    for (name, _, reason) in EXEMPT_OPEN_SITES {
        assert!(
            outlet_doc.contains(name),
            "出口模块文档没有点名豁免函数 {name}（原因：{reason}）：清单是给人读的，\
             只有文档与判据同进同退才不会腐烂"
        );
    }
}

fn site_counts(name: &str, ledger: &BTreeMap<String, (usize, usize)>) -> (usize, usize) {
    *ledger
        .get(name)
        .unwrap_or_else(|| panic!("生产源码里找不到函数 {name} 的 pipeline 打开站点：名单已经腐烂"))
}

/// 反向自我证明：这条判据扫到的站点总数必须与它自己读到的源码一致。
///
/// 单列一条是因为上面那条一旦因为名单腐烂而 panic，就没人说得清"扫描本身有没有在工作"。
#[test]
fn open_site_ledger_actually_sees_the_production_sources() {
    let ledger = production_open_sites();
    let sites: usize = ledger.values().map(|counts| counts.0).sum();
    let absorbs: usize = ledger.values().map(|counts| counts.1).sum();
    assert_eq!(
        sites,
        COUNTED_OPEN_SITES
            .iter()
            .map(|(_, count)| count)
            .sum::<usize>()
            + EXEMPT_OPEN_SITES
                .iter()
                .map(|(_, count, _)| count)
                .sum::<usize>(),
        "扫描到的站点总数与两份名单之和不等: {ledger:?}"
    );
    let expected_absorbs = workspace_source("crates/qx-cli/src/venue_runtime/paper_worker.rs")
        .lines()
        .filter(|line| line.contains(".absorb("))
        .count();
    assert_eq!(
        absorbs, expected_absorbs,
        "并账次数只应来自 paper 的两条 worker 循环，实际台账: {ledger:?}"
    );
}
