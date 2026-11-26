//! Stable shared execution facts used by the application and its facades.

use std::collections::BTreeMap;

/// Stable id for a spread group; the live and research facades share this identity rule.
pub fn spread_group_id(strategy_id: &str, run_id: u64, signal_id: u64) -> String {
    format!("spread-{strategy_id}-{run_id}-{signal_id}")
}

/// Collapse rejected events into one deterministic, severity-neutral summary.
pub fn rejection_facts(event_log: &qx_core::EventLog) -> Vec<(String, usize)> {
    let mut grouped = BTreeMap::<String, usize>::new();
    for reason in event_log
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            qx_core::EventKind::Rejected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
    {
        *grouped.entry(reason).or_default() += 1;
    }
    let mut pairs = grouped.into_iter().collect::<Vec<_>>();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs
}

pub fn rejection_facts_line(rejections: &[(String, usize)]) -> String {
    let line = rejections
        .iter()
        .map(|(reason, count)| format!("{count}x{}", reason.replace(' ', "_")))
        .collect::<Vec<_>>()
        .join(";");
    if line.is_empty() {
        "none".to_string()
    } else {
        line
    }
}

pub fn rejection_count(rejections: &[(String, usize)]) -> usize {
    rejections.iter().map(|(_, count)| count).sum()
}

/// Preserve a bounded, per-order readout for report consumers. Older venue events can contain
/// only an order ID and reason, so the payload also states how many events lack a request snapshot.
pub fn rejection_detail_readout(event_log: &qx_core::EventLog) -> serde_json::Value {
    const MAX_DETAILS: usize = 100;

    let mut total = 0usize;
    let mut available = 0usize;
    let mut details = Vec::new();
    for event in event_log.events() {
        let qx_core::EventKind::Rejected {
            client_order_id,
            reason,
            reference_price_exact,
            order_snapshot,
        } = &event.kind
        else {
            continue;
        };
        total += 1;
        let Some(order) = order_snapshot.as_deref() else {
            continue;
        };
        available += 1;
        if details.len() >= MAX_DETAILS {
            continue;
        }
        let quantity_exact = order.exact_amounts.map_or_else(
            || qx_core::ExactDecimal::from_legacy_raw(order.qty.raw()).to_string(),
            |amounts| amounts.quantity.to_string(),
        );
        let limit_price_exact = order
            .limit
            .map(|price| qx_core::ExactDecimal::from_legacy_raw(price.raw()).to_string());
        let (strategy_id, signal_id, intent_id) =
            order.trace.as_ref().map_or((None, None, None), |trace| {
                (trace.strategy_id.clone(), trace.signal_id, trace.intent_id)
            });
        details.push(serde_json::json!({
            "ts": event.ts,
            "client_order_id": client_order_id,
            "snapshot_client_order_id": order.client_id,
            "reason": reason,
            "instrument": order.instrument.to_string(),
            "side": match order.side {
                qx_core::Side::Buy => "buy",
                qx_core::Side::Sell => "sell",
            },
            "quantity_raw": order.qty.raw().to_string(),
            "quantity_scale": 9,
            "quantity_exact": quantity_exact,
            "limit_price_raw": order.limit.map(|price| price.raw().to_string()),
            "limit_price_exact": limit_price_exact,
            "reference_price_exact": reference_price_exact.map(|price| price.to_string()),
            "quote_budget_exact": order.exact_amounts.and_then(|amounts| amounts.quote_budget).map(|value| value.to_string()),
            "account_id": order.account_id,
            "strategy_id": strategy_id,
            "signal_id": signal_id,
            "intent_id": intent_id,
            "policy": order.policy,
        }));
    }

    serde_json::json!({
        "details": details,
        "recorded_count": available.min(MAX_DETAILS),
        "available_count": available,
        "omitted_count": available.saturating_sub(MAX_DETAILS),
        "unavailable_count": total.saturating_sub(available),
    })
}

pub(crate) fn rejection_facts_u64(event_log: &qx_core::EventLog) -> Vec<(String, u64)> {
    rejection_facts(event_log)
        .into_iter()
        .map(|(reason, count)| (reason, count as u64))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::rejection_detail_readout;
    use qx_core::{
        Event, EventKind, EventLog, ExactDecimal, InstrumentId, Order, OrderStatus, Priority,
        Quantity, Side,
    };

    #[test]
    fn rejection_readout_exposes_order_facts_and_counts_legacy_events() {
        let order = Order {
            client_id: 7,
            instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_raw(10_000_000),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        let mut events = EventLog::new();
        let seq = events.alloc_seq();
        events.append(Event::new(
            seq,
            1_000,
            Priority::COMMAND,
            EventKind::Rejected {
                client_order_id: 7,
                reason: "minimum notional".into(),
                reference_price_exact: Some(ExactDecimal::parse("100").unwrap()),
                order_snapshot: Some(Box::new(order)),
            },
        ));
        let seq = events.alloc_seq();
        events.append(Event::new(
            seq,
            2_000,
            Priority::COMMAND,
            EventKind::Rejected {
                client_order_id: 8,
                reason: "venue rejection".into(),
                reference_price_exact: None,
                order_snapshot: None,
            },
        ));

        let readout = rejection_detail_readout(&events);
        assert_eq!(readout["recorded_count"], 1);
        assert_eq!(readout["available_count"], 1);
        assert_eq!(readout["unavailable_count"], 1);
        assert_eq!(readout["details"][0]["client_order_id"], 7);
        assert_eq!(readout["details"][0]["quantity_exact"], "0.01");
        assert_eq!(readout["details"][0]["reference_price_exact"], "100");
        assert_eq!(readout["details"][0]["reason"], "minimum notional");
    }

    #[test]
    fn rejection_readout_caps_details_and_reports_omissions() {
        let mut events = EventLog::new();
        for client_order_id in 1..=102 {
            let order = Order {
                client_id: client_order_id,
                instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
                side: Side::Buy,
                qty: Quantity::from_raw(10_000_000),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
                exact_amounts: None,
            };
            let seq = events.alloc_seq();
            events.append(Event::new(
                seq,
                client_order_id,
                Priority::COMMAND,
                EventKind::Rejected {
                    client_order_id,
                    reason: "minimum notional".into(),
                    reference_price_exact: None,
                    order_snapshot: (client_order_id <= 101).then(|| Box::new(order)),
                },
            ));
        }

        let readout = rejection_detail_readout(&events);
        assert_eq!(readout["recorded_count"], 100);
        assert_eq!(readout["available_count"], 101);
        assert_eq!(readout["omitted_count"], 1);
        assert_eq!(readout["unavailable_count"], 1);
        assert_eq!(readout["details"].as_array().map(Vec::len), Some(100));
    }
}
