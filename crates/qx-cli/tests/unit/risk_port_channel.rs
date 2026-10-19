use super::*;

/// #172 接线判据：风控端口的"裁决"与"端口故障"必须各走各的通道，两条播报都得可达。
///
/// 改前：`RiskDecision { accepted, reason_code }` 只在通过时被写成 `accepted: true`，
/// 两个生产实现把拒绝改成 `Err` 抛出，于是 `submit_with_risk` 里「账户级 RiskPort 拒绝
/// 订单」这条播报只有测试假件能触发 —— 一次正常的风控拒单被上层报成「RiskPort 执行
/// 失败」，运维据此去查链路，而链路是好的。只测行为用例（`qx-execution` 的
/// `gateway_port.rs`）不会发现某个端口被改回 `Err(..)`，所以按调用点逐个数。
#[test]
fn risk_port_verdicts_and_port_failures_keep_separate_broadcasts() {
    let application = workspace_source("crates/qx-execution/src/application.rs");
    assert!(
        application
            .contains("fn evaluate_order(&self, order: &Order) -> Result<RiskVerdict, String>;"),
        "#172 的端口签名回退了：拒绝若再走 Err，端口裁决与链路故障就分不开"
    );
    assert!(
        !application.contains("pub struct RiskDecision"),
        "`RiskDecision` 结构回来了，与 `qx_risk::RiskDecision` 的同名异物（V13 §5 R2-1）也跟着回来"
    );

    let production = workspace_source("crates/qx-execution/src/lib.rs");
    assert_eq!(
        production.matches("impl RiskPort for").count(),
        2,
        "生产 RiskPort 实现份数变了：本判据的逐处计数要按新名册改，别只改这里就算通过"
    );
    for (needle, want, why) in [
        ("Ok(RiskVerdict::Allow)", 2, "两个生产端口各一处通过裁决"),
        (
            "Ok(RiskVerdict::Reject {",
            2,
            "拒绝必须以 Ok(Reject) 给出；改回 Err 就是 #172 的复发点",
        ),
        (
            "decision.violations.join",
            2,
            "拒绝理由要带上违规清单，否则播报只说「被拒」而说不出为什么",
        ),
        (
            "账户级 RiskPort 拒绝订单",
            1,
            "拒绝通道的播报必须恰好一处，多一处就是第二条链路绕过了端口口径",
        ),
        (
            "账户级 RiskPort 执行失败",
            1,
            "「执行失败」只能留给端口给不出裁决的那种情况，恰好一处",
        ),
    ] {
        assert_eq!(
            production.matches(needle).count(),
            want,
            "{why}：`{needle}` 在生产实现里的份数不对"
        );
    }
    for banned in ["accepted: bool", "reason_code"] {
        assert!(
            !production.contains(banned),
            "#172 删掉的 `{banned}` 形状回来了：那是只写通过值、把拒绝挤进 Err 的老口径"
        );
    }
}
