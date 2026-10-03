//! 运行时事实的元数据与归因上下文补全。
//!
//! 连接器只给出供应商形态的事实；进入 EventLog 之前，本模块把 correlation 身份、去重键，
//! 以及账户/策略/信号/意图这一组归因字段补成内核的 `EventContext`。补全只在字段为空时发生：
//! 上游已经写明的归因不会被运行时覆盖。

use qx_core::{EventContext, EventMetadata, Fill};

use super::RuntimeExternalEvent;

pub(super) fn runtime_event_metadata(
    correlation_id: &str,
    source_seq: u64,
    source_kind: &str,
) -> EventMetadata {
    let source_id = correlation_id
        .split(':')
        .next()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("runtime")
        .to_string();
    let dedup_key = if correlation_id.trim().is_empty() {
        format!("runtime:source:{source_seq}")
    } else {
        format!("{correlation_id}:source:{source_seq}")
    };
    EventMetadata {
        schema_version: qx_core::EVENT_METADATA_SCHEMA_VERSION,
        source_id,
        source_kind: source_kind.into(),
        dedup_key,
        rule_version: "runtime-v1".into(),
        context: EventContext::default(),
    }
}

pub(super) fn enrich_runtime_event_context(
    metadata: &mut EventMetadata,
    event: &RuntimeExternalEvent,
) {
    let context = &mut metadata.context;
    match event {
        RuntimeExternalEvent::AccountBalanceSnapshot {
            account_id,
            venue_id,
            ..
        }
        | RuntimeExternalEvent::AccountPositionSnapshot {
            account_id,
            venue_id,
            ..
        } => {
            if context.account_id.trim().is_empty() {
                context.account_id = account_id.clone();
            }
            if context.portfolio_id.trim().is_empty() {
                context.portfolio_id = account_id.clone();
            }
            if context.tenant_id.trim().is_empty() {
                context.tenant_id = account_id.clone();
            }
            if context.run_id.trim().is_empty() {
                context.run_id = format!("account:{account_id}");
            }
            if context.strategy_id.trim().is_empty() {
                context.strategy_id = "external-account-state".into();
            }
            if context.signal_id.trim().is_empty() {
                context.signal_id = venue_id.clone();
            }
        }
        RuntimeExternalEvent::AccountCashflow { cashflow } => {
            if context.account_id.trim().is_empty() {
                context.account_id = cashflow.account_id.clone();
            }
            if context.portfolio_id.trim().is_empty() {
                context.portfolio_id = cashflow.account_id.clone();
            }
            if context.tenant_id.trim().is_empty() {
                context.tenant_id = cashflow.account_id.clone();
            }
            if context.run_id.trim().is_empty() {
                context.run_id = format!("account:{}", cashflow.account_id);
            }
            if context.strategy_id.trim().is_empty() {
                context.strategy_id = "external-cashflow".into();
            }
            if context.signal_id.trim().is_empty() {
                context.signal_id = cashflow.venue_id.clone();
            }
        }
        RuntimeExternalEvent::Fill { fill } => enrich_fill_context(context, fill),
        RuntimeExternalEvent::FillWithSpec { fill, .. } => enrich_fill_context(context, fill),
        RuntimeExternalEvent::MarketQuote { .. }
        | RuntimeExternalEvent::FundingRateSnapshot { .. }
        | RuntimeExternalEvent::Accepted { .. }
        | RuntimeExternalEvent::Cancelled { .. }
        | RuntimeExternalEvent::ReconcileRequired { .. } => {}
    }
}

fn enrich_fill_context(context: &mut EventContext, fill: &Fill) {
    if context.account_id.trim().is_empty() {
        context.account_id = fill.account_id.clone();
    }
    if context.strategy_id.trim().is_empty() {
        context.strategy_id = fill
            .strategy_id
            .clone()
            .unwrap_or_else(|| "external-execution".into());
    }
    if context.signal_id.trim().is_empty() {
        context.signal_id = fill
            .signal_id
            .map(|value| value.to_string())
            .unwrap_or_default();
    }
    if context.intent_id.trim().is_empty() {
        context.intent_id = fill
            .intent_id
            .map(|value| value.to_string())
            .unwrap_or_default();
    }
    if context.run_id.trim().is_empty() && !context.account_id.trim().is_empty() {
        context.run_id = format!("account:{}", context.account_id);
    }
    if context.portfolio_id.trim().is_empty() && !context.account_id.trim().is_empty() {
        context.portfolio_id = context.account_id.clone();
    }
    if context.tenant_id.trim().is_empty() && !context.account_id.trim().is_empty() {
        context.tenant_id = context.account_id.clone();
    }
}
