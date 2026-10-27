"""报告与复核层：产物、复核与跨 Run 对比（方案 §6.2 接口分层）。

这一层回答一个问题：**跑出来的东西能不能信**。

- [`verify_run`][qianxing.reports.verify_run] / [`verify_depth_run`][qianxing.reports.verify_depth_run]
  独立复核一次回测的四份产物：被篡改的读数判**失败**（`verified: false` + `mismatches`），
  不是抛错——"这份产物对不上"是结论，不是异常。
- [`compare_runs`][qianxing.reports.compare_runs] 把两次已完成的运行逐格对比。

这一层与 `qianxing.core` 共用同一批结果类型（`BacktestOutcome` 一族）：core 负责**产出**，
reports 负责**复核**，两者看的是同一份数据而不是两份各自序列化的副本。

方案 §6.2 说的 "reports 与 Evidence"：产物清单与复核结论就是本仓的 Evidence 面；
L3/L4 实盘轨的 `EvidenceBundle` 是另一份（仓内无生产者），不要与这里的回测/Paper 轨混。
"""

from qianxing.app import (
    BacktestOutcome,
    CompareRunsResult,
    CompareRunsSpec,
    ComparedRun,
    ComparedRunResult,
    DepthBacktestOutcome,
    MultiLegBacktestOutcome,
    VerificationResult,
    compare_runs,
    read_depth_run_report_v1,
    read_multi_account_joint_report_v1,
    read_multi_account_report_v1,
    read_multi_leg_report_facts_v1,
    read_multi_leg_research_report_v1,
    read_multi_leg_event_report_page_v1,
    read_multi_market_event_report_page_v1,
    read_multi_market_depth_event_report_page_v1,
    read_multi_market_depth_research_report_v1,
    read_multi_market_research_report_v1,
    read_run_report_v1,
    verify_depth_run,
    verify_run,
)
from qianxing.reports.joint_account_html import (
    render_multi_account_joint_report_html,
    write_multi_account_joint_report_html,
)

__all__ = [
    "BacktestOutcome",
    "CompareRunsResult",
    "CompareRunsSpec",
    "ComparedRun",
    "ComparedRunResult",
    "DepthBacktestOutcome",
    "MultiLegBacktestOutcome",
    "VerificationResult",
    "compare_runs",
    "read_depth_run_report_v1",
    "read_multi_account_joint_report_v1",
    "read_multi_account_report_v1",
    "read_multi_leg_report_facts_v1",
    "read_multi_leg_research_report_v1",
    "read_multi_leg_event_report_page_v1",
    "read_multi_market_event_report_page_v1",
    "read_multi_market_depth_event_report_page_v1",
    "read_multi_market_depth_research_report_v1",
    "read_multi_market_research_report_v1",
    "read_run_report_v1",
    "render_multi_account_joint_report_html",
    "verify_depth_run",
    "verify_run",
    "write_multi_account_joint_report_html",
]
