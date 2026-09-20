//! 账户维度的对账：资金、费率流水与成交事实。
//!
//! 这些维度不涉及订单状态机，判定就是"逐键聚合后比较金额/事实"；订单维度的
//! 状态感知裁决在 [`super::order`]，两边不得互相复制规则。

use super::Discrepancy;
use qx_core::Fill;
use std::collections::BTreeMap;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CashSnapshot {
    pub currency: String,
    pub amount: i128,
}

pub fn reconcile_cash(local: &[CashSnapshot], venue: &[CashSnapshot]) -> Vec<Discrepancy> {
    let (l, mut out) = amount_map(
        local,
        "cash",
        "local",
        |value| value.currency.clone(),
        |value| value.amount,
    );
    let (v, venue_duplicates) = amount_map(
        venue,
        "cash",
        "venue",
        |value| value.currency.clone(),
        |value| value.amount,
    );
    out.extend(venue_duplicates);
    out.extend(
        l.keys()
            .chain(v.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter_map(|currency| {
                let a = *l.get(currency).unwrap_or(&0);
                let b = *v.get(currency).unwrap_or(&0);
                (a != b).then(|| Discrepancy::CashMismatch {
                    currency: currency.clone(),
                    local: a,
                    venue: b,
                })
            })
            .collect::<Vec<_>>(),
    );
    out
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FlowSnapshot {
    pub currency: String,
    pub amount: i128,
}

pub fn reconcile_fees(local: &[FlowSnapshot], venue: &[FlowSnapshot]) -> Vec<Discrepancy> {
    reconcile_flows(local, venue, |currency, a, b| Discrepancy::FeeMismatch {
        currency,
        local: a,
        venue: b,
    })
}

pub fn reconcile_funding(local: &[FlowSnapshot], venue: &[FlowSnapshot]) -> Vec<Discrepancy> {
    reconcile_flows(local, venue, |currency, a, b| {
        Discrepancy::FundingMismatch {
            currency,
            local: a,
            venue: b,
        }
    })
}

fn reconcile_flows<F>(
    local: &[FlowSnapshot],
    venue: &[FlowSnapshot],
    make_discrepancy: F,
) -> Vec<Discrepancy>
where
    F: Fn(String, i128, i128) -> Discrepancy,
{
    let (local, mut out) = amount_map(
        local,
        "flow",
        "local",
        |value| value.currency.clone(),
        |value| value.amount,
    );
    let (venue, venue_duplicates) = amount_map(
        venue,
        "flow",
        "venue",
        |value| value.currency.clone(),
        |value| value.amount,
    );
    out.extend(venue_duplicates);
    out.extend(
        local
            .keys()
            .chain(venue.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter_map(|currency| {
                let a = *local.get(currency).unwrap_or(&0);
                let b = *venue.get(currency).unwrap_or(&0);
                (a != b).then(|| make_discrepancy(currency.clone(), a, b))
            })
            .collect::<Vec<_>>(),
    );
    out
}

fn amount_map<T, K, V>(
    values: &[T],
    domain: &str,
    side: &str,
    key_of: K,
    value_of: V,
) -> (BTreeMap<String, i128>, Vec<Discrepancy>)
where
    K: Fn(&T) -> String,
    V: Fn(&T) -> i128,
{
    let mut map = BTreeMap::new();
    let mut issues = Vec::new();
    for value in values {
        let key = key_of(value);
        if map.insert(key.clone(), value_of(value)).is_some() {
            issues.push(Discrepancy::DuplicateSnapshot {
                domain: domain.into(),
                side: side.into(),
                key,
            });
        }
    }
    (map, issues)
}

/// 成交对账必须比较成交事实本身，不能只用订单 filled 数量推断。
pub fn reconcile_fills(local: &[Fill], venue: &[Fill]) -> Vec<Discrepancy> {
    let key = |fill: &Fill| (fill.order_id, fill.ts);
    let mut local_groups = BTreeMap::<(u64, u64), Vec<&Fill>>::new();
    let mut venue_groups = BTreeMap::<(u64, u64), Vec<&Fill>>::new();
    for fill in local {
        local_groups.entry(key(fill)).or_default().push(fill);
    }
    for fill in venue {
        venue_groups.entry(key(fill)).or_default().push(fill);
    }
    let sort_group = |group: &mut Vec<&Fill>| {
        group.sort_by_key(|fill| {
            (
                fill.qty.raw(),
                fill.price.raw(),
                fill.fee.raw(),
                fill.venue_order_id.as_deref().unwrap_or("").to_string(),
            )
        });
    };
    for group in local_groups.values_mut() {
        sort_group(group);
    }
    for group in venue_groups.values_mut() {
        sort_group(group);
    }
    let keys = local_groups
        .keys()
        .chain(venue_groups.keys())
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    keys.into_iter()
        .filter_map(|(order_id, ts)| {
            match (
                local_groups.get(&(order_id, ts)),
                venue_groups.get(&(order_id, ts)),
            ) {
                (None, Some(_)) => Some(Discrepancy::FillMissingLocally { order_id, ts }),
                (Some(_), None) => Some(Discrepancy::FillMissingAtVenue { order_id, ts }),
                (Some(local), Some(venue))
                    if local.len() != venue.len()
                        || local.iter().zip(venue).any(|(a, b)| {
                            a.qty != b.qty
                                || a.price != b.price
                                || a.fee != b.fee
                                || a.venue_order_id != b.venue_order_id
                        }) =>
                {
                    let a = local[0];
                    let b = venue[0];
                    Some(Discrepancy::FillMismatch {
                        order_id,
                        ts,
                        local_qty: a.qty.raw(),
                        venue_qty: b.qty.raw(),
                        local_price: a.price.raw(),
                        venue_price: b.price.raw(),
                    })
                }
                _ => None,
            }
        })
        .collect()
}
