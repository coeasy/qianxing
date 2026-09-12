//! # qx-genglu — 更路
//!
//! 审计与对账。更路簿是明清航海者记录航线、针位、更数的手抄本——
//! 用它命名"事件日志与审计"是精准的：都是**不可篡改的航行事实记录**。

use qx_core::{Fill, InstrumentId, Order};
use std::collections::BTreeMap;

/// 最大回撤（定点比例，0..1e9 表示 0..100%）。
pub fn max_drawdown(equity: &[i128]) -> i128 {
    let mut peak = 0i128;
    let mut mdd = 0i128;
    for &e in equity {
        if e > peak {
            peak = e;
        }
        if peak > 0 {
            let dd = ((peak - e) * 1_000_000_000) / peak;
            if dd > mdd {
                mdd = dd;
            }
        }
    }
    mdd
}

/// 总收益（定点比例）。
pub fn total_return(equity: &[i128]) -> i128 {
    match (equity.first(), equity.last()) {
        (Some(&f), Some(&l)) if f != 0 => ((l - f) * 1_000_000_000) / f,
        _ => 0,
    }
}

/// 夏普比率。**仅用于报告展示，允许 f64**——不进入撮合与记账路径。
pub fn sharpe_ratio(returns: &[f64], periods_per_year: f64, risk_free: f64) -> Option<f64> {
    if returns.len() < 2 {
        return None;
    }
    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let var = returns.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / (n - 1.0);
    let sd = var.sqrt();
    if sd == 0.0 {
        return None;
    }
    Some((mean - risk_free / periods_per_year) / sd * periods_per_year.sqrt())
}

/// 绩效汇总。
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub n_fills: usize,
    pub total_fee: i128,
    pub total_return: i128,
    pub max_drawdown: i128,
    pub final_equity: i128,
}

/// 分析器：把成交流归约为绩效指标。
#[derive(Default)]
pub struct BasicAnalyser {
    pub total_fee: i128,
    pub n_fills: usize,
}

impl BasicAnalyser {
    pub fn on_fill(&mut self, f: &Fill) {
        self.n_fills += 1;
        self.total_fee += f.fee.raw();
    }

    pub fn report(&self, equity: &[i128]) -> Metrics {
        Metrics {
            n_fills: self.n_fills,
            total_fee: self.total_fee,
            total_return: total_return(equity),
            max_drawdown: max_drawdown(equity),
            final_equity: *equity.last().unwrap_or(&0),
        }
    }
}

/// 对账差异。**"本地账簿 = 柜台账簿"不能作为默认假设。**
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Discrepancy {
    /// 同一对账快照出现重复键，禁止用 BTreeMap 的 last-write-wins 静默覆盖。
    DuplicateSnapshot {
        domain: String,
        side: String,
        key: String,
    },
    /// 本地有、柜台无。
    MissingAtVenue {
        client_id: u64,
    },
    /// 柜台有、本地无。
    MissingLocally {
        client_id: u64,
    },
    QtyMismatch {
        client_id: u64,
        local: i128,
        venue: i128,
    },
    CashMismatch {
        currency: String,
        local: i128,
        venue: i128,
    },
    PositionMismatch {
        instrument: InstrumentId,
        local: i128,
        venue: i128,
    },
    FeeMismatch {
        currency: String,
        local: i128,
        venue: i128,
    },
    FundingMismatch {
        currency: String,
        local: i128,
        venue: i128,
    },
    FillMissingAtVenue {
        order_id: u64,
        ts: u64,
    },
    FillMissingLocally {
        order_id: u64,
        ts: u64,
    },
    FillMismatch {
        order_id: u64,
        ts: u64,
        local_qty: i128,
        venue_qty: i128,
        local_price: i128,
        venue_price: i128,
    },
}

/// 订单对账：`venue` 参数为 (client_id, filled_qty)。
pub fn reconcile_orders(local: &[Order], venue: &[(u64, i128)]) -> Vec<Discrepancy> {
    let mut out = Vec::new();
    let mut local_map = BTreeMap::new();
    for order in local {
        if local_map.insert(order.client_id, order).is_some() {
            out.push(Discrepancy::DuplicateSnapshot {
                domain: "order".into(),
                side: "local".into(),
                key: order.client_id.to_string(),
            });
        }
    }
    let mut venue_map = BTreeMap::new();
    for (client_id, filled) in venue {
        if venue_map.insert(*client_id, *filled).is_some() {
            out.push(Discrepancy::DuplicateSnapshot {
                domain: "order".into(),
                side: "venue".into(),
                key: client_id.to_string(),
            });
        }
    }

    for o in local_map.values() {
        match venue_map.get(&o.client_id) {
            None => out.push(Discrepancy::MissingAtVenue {
                client_id: o.client_id,
            }),
            Some(&vq) if vq != o.filled.raw() => out.push(Discrepancy::QtyMismatch {
                client_id: o.client_id,
                local: o.filled.raw(),
                venue: vq,
            }),
            Some(_) => {}
        }
    }

    let local_ids: std::collections::BTreeSet<u64> = local_map.keys().copied().collect();
    for id in venue_map.keys() {
        if !local_ids.contains(id) {
            out.push(Discrepancy::MissingLocally { client_id: *id });
        }
    }

    out
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CashSnapshot {
    pub currency: String,
    pub amount: i128,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PositionSnapshot {
    pub instrument: InstrumentId,
    pub quantity: i128,
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

pub fn reconcile_positions(
    local: &[PositionSnapshot],
    venue: &[PositionSnapshot],
) -> Vec<Discrepancy> {
    let (l, mut out) = position_map(local, "local");
    let (v, venue_duplicates) = position_map(venue, "venue");
    out.extend(venue_duplicates);
    out.extend(
        l.keys()
            .chain(v.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter_map(|instrument| {
                let a = *l.get(instrument).unwrap_or(&0);
                let b = *v.get(instrument).unwrap_or(&0);
                (a != b).then(|| Discrepancy::PositionMismatch {
                    instrument: instrument.clone(),
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

fn position_map(
    values: &[PositionSnapshot],
    side: &str,
) -> (BTreeMap<InstrumentId, i128>, Vec<Discrepancy>) {
    let mut map = BTreeMap::new();
    let mut issues = Vec::new();
    for value in values {
        let key = value.instrument.clone();
        if map.insert(key.clone(), value.quantity).is_some() {
            issues.push(Discrepancy::DuplicateSnapshot {
                domain: "position".into(),
                side: side.into(),
                key: key.to_string(),
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

/// 成交归因链：不改变 Fill 事实，只在运营层保存来源。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FillAttribution {
    pub fill: Fill,
    pub strategy_id: String,
    pub signal_id: Option<u64>,
    pub intent_id: u64,
    pub account_id: String,
    pub rule_version: String,
}

#[derive(Default)]
pub struct AttributionBook {
    fills: Vec<FillAttribution>,
}

impl AttributionBook {
    pub fn record(&mut self, item: FillAttribution) {
        self.fills.push(item);
    }
    pub fn all(&self) -> &[FillAttribution] {
        &self.fills
    }
    pub fn total_by_strategy(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for f in &self.fills {
            *out.entry(f.strategy_id.clone()).or_insert(0) += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawdown_computed() {
        let eq = vec![100, 120, 90, 130];
        let dd = max_drawdown(&eq);
        // peak 120 -> 90，回撤 25%
        assert_eq!(dd, 250_000_000);
    }

    #[test]
    fn total_return_signed() {
        assert_eq!(total_return(&[100, 110]), 100_000_000); // +10%
        assert_eq!(total_return(&[100, 90]), -100_000_000); // -10%
    }

    #[test]
    fn detects_missing_at_venue() {
        let local = vec![Order {
            client_id: 1,
            instrument: qx_core::InstrumentId::parse("T.V").unwrap(),
            side: qx_core::Side::Buy,
            qty: qx_core::Quantity::from_i64(1),
            limit: None,
            status: qx_core::OrderStatus::Filled,
            filled: qx_core::Quantity::from_i64(1),
            account_id: "a".into(),
            trace: None,
            policy: None,
        }];
        let diffs = reconcile_orders(&local, &[]);
        assert_eq!(diffs, vec![Discrepancy::MissingAtVenue { client_id: 1 }]);
    }

    #[test]
    fn detects_qty_mismatch() {
        let local = vec![Order {
            client_id: 1,
            instrument: qx_core::InstrumentId::parse("T.V").unwrap(),
            side: qx_core::Side::Buy,
            qty: qx_core::Quantity::from_i64(10),
            limit: None,
            status: qx_core::OrderStatus::PartiallyFilled,
            filled: qx_core::Quantity::from_i64(4),
            account_id: "a".into(),
            trace: None,
            policy: None,
        }];
        let diffs = reconcile_orders(&local, &[(1, 6_000_000_000)]);
        assert!(matches!(diffs[0], Discrepancy::QtyMismatch { .. }));
    }

    #[test]
    fn reconciles_cash_and_positions() {
        let cash = reconcile_cash(
            &[CashSnapshot {
                currency: "USD".into(),
                amount: 10,
            }],
            &[CashSnapshot {
                currency: "USD".into(),
                amount: 9,
            }],
        );
        assert!(matches!(cash[0], Discrepancy::CashMismatch { .. }));
        let instrument = qx_core::InstrumentId::parse("T.V").unwrap();
        let pos = reconcile_positions(
            &[PositionSnapshot {
                instrument: instrument.clone(),
                quantity: 2,
            }],
            &[PositionSnapshot {
                instrument,
                quantity: 1,
            }],
        );
        assert!(matches!(pos[0], Discrepancy::PositionMismatch { .. }));
        let fees = reconcile_fees(
            &[FlowSnapshot {
                currency: "USD".into(),
                amount: 3,
            }],
            &[FlowSnapshot {
                currency: "USD".into(),
                amount: 2,
            }],
        );
        assert!(matches!(fees[0], Discrepancy::FeeMismatch { .. }));
    }

    #[test]
    fn attribution_keeps_strategy_chain() {
        let mut book = AttributionBook::default();
        book.record(FillAttribution {
            fill: Fill {
                order_id: 1,
                qty: qx_core::Quantity::from_i64(1),
                price: qx_core::Price::from_i64(1),
                fee: qx_core::Money::ZERO,
                ts: 1,
                ..Fill::default()
            },
            strategy_id: "s1".into(),
            signal_id: Some(9),
            intent_id: 1,
            account_id: "a".into(),
            rule_version: "r1".into(),
        });
        assert_eq!(book.total_by_strategy().get("s1"), Some(&1));
    }

    #[test]
    fn reconciles_fill_facts_without_using_order_status_as_a_proxy() {
        let local = Fill {
            order_id: 1,
            qty: qx_core::Quantity::from_i64(2),
            price: qx_core::Price::from_i64(10),
            ts: 5,
            ..Fill::default()
        };
        let venue = Fill {
            order_id: 1,
            qty: qx_core::Quantity::from_i64(1),
            price: qx_core::Price::from_i64(11),
            ts: 5,
            ..Fill::default()
        };
        assert!(matches!(
            reconcile_fills(&[local], &[venue]).as_slice(),
            [Discrepancy::FillMismatch { .. }]
        ));
    }

    #[test]
    fn duplicate_snapshot_keys_are_reported_instead_of_overwritten() {
        let duplicates = reconcile_cash(
            &[
                CashSnapshot {
                    currency: "USD".into(),
                    amount: 10,
                },
                CashSnapshot {
                    currency: "USD".into(),
                    amount: 11,
                },
            ],
            &[],
        );
        assert!(duplicates.iter().any(|item| matches!(
            item,
            Discrepancy::DuplicateSnapshot { domain, side, key }
                if domain == "cash" && side == "local" && key == "USD"
        )));
    }
}
