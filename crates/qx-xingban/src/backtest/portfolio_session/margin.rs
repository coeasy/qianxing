use super::runtime::RuntimeMarkets;
use qx_core::{InstrumentId, Ledger, Price, TradingInstrumentSpec};
use std::collections::BTreeMap;

/// Immutable margin totals for one ledger and one visible-mark snapshot.
///
/// The portfolio admission loop can evaluate several instruments against the
/// same account state. Cache each instrument's contribution once, then answer
/// the usual "all instruments except target" query by market index.
/// Returning `None` asks the caller to use the original checked calculation;
/// this preserves its per-target behavior for incomplete marks and overflow.
pub(super) struct PortfolioMarginSnapshot {
    total: i128,
    by_market_index: Vec<i128>,
}

pub(super) type CachedPortfolioMarginSnapshot = (u64, Option<PortfolioMarginSnapshot>);

impl PortfolioMarginSnapshot {
    pub(super) fn try_new(
        ledger: &Ledger,
        account_id: &str,
        specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
        marks: &BTreeMap<InstrumentId, Price>,
        markets: &RuntimeMarkets<'_>,
    ) -> Option<Self> {
        // The measured snapshot setup is worthwhile for portfolios with at
        // least four configured markets; smaller portfolios keep the simpler
        // target-excluding scan and avoid cache construction overhead.
        if specs.len() < 4 {
            return None;
        }
        let mut total = 0_i128;
        let mut by_market_index = vec![0_i128; markets.by_index.len()];
        for (instrument, spec) in specs {
            if !spec.product.supports_leverage() {
                continue;
            }
            let mark = marks.get(instrument)?;
            let net_qty = ledger.position_for(account_id, instrument).quantity.raw();
            let long_qty = ledger
                .position_for_side(account_id, instrument, qx_core::PositionSide::Long)
                .quantity
                .raw();
            let short_qty = ledger
                .position_for_side(account_id, instrument, qx_core::PositionSide::Short)
                .quantity
                .raw();
            let qty = if long_qty != 0 || short_qty != 0 {
                long_qty
                    .checked_abs()?
                    .checked_add(short_qty.checked_abs()?)?
            } else {
                net_qty.checked_abs()?
            };
            let market_index = *markets.index_by_instrument.get(instrument)?;
            if qty == 0 {
                continue;
            }
            let margin = markets
                .by_index
                .get(market_index)?
                .margin
                .instrument_initial_margin(spec, qty, mark.raw().abs(), 1)
                .ok()?;
            // Margin contributions are non-negative. Reject any unexpected
            // negative value so subtraction cannot alter legacy semantics.
            if margin < 0 {
                return None;
            }
            total = total.checked_add(margin)?;
            *by_market_index.get_mut(market_index)? = margin;
        }
        Some(Self {
            total,
            by_market_index,
        })
    }

    pub(super) fn other_market_margin(
        &self,
        target: &InstrumentId,
        markets: &RuntimeMarkets<'_>,
    ) -> Option<i128> {
        let target_margin = markets
            .index_by_instrument
            .get(target)
            .and_then(|index| self.by_market_index.get(*index))
            .copied()
            .unwrap_or(0);
        self.total.checked_sub(target_margin)
    }
}

pub(super) fn other_market_margin(
    ledger: &Ledger,
    account_id: &str,
    target: &InstrumentId,
    specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    marks: &BTreeMap<InstrumentId, Price>,
    markets: &RuntimeMarkets<'_>,
) -> Result<i128, qx_core::QxError> {
    let mut total = 0_i128;
    for (instrument, spec) in specs {
        if instrument == target || !spec.product.supports_leverage() {
            continue;
        }
        let mark = marks.get(instrument).ok_or_else(|| {
            qx_core::QxError::ReconcileRequired(format!(
                "共享账户保证金估值缺少标记价 {instrument}"
            ))
        })?;
        let net_qty = ledger.position_for(account_id, instrument).quantity.raw();
        let long_qty = ledger
            .position_for_side(account_id, instrument, qx_core::PositionSide::Long)
            .quantity
            .raw();
        let short_qty = ledger
            .position_for_side(account_id, instrument, qx_core::PositionSide::Short)
            .quantity
            .raw();
        let qty = if long_qty != 0 || short_qty != 0 {
            long_qty
                .checked_abs()
                .and_then(|long| {
                    short_qty
                        .checked_abs()
                        .and_then(|short| long.checked_add(short))
                })
                .ok_or_else(|| {
                    qx_core::QxError::Invariant("共享账户 hedge 保证金数量溢出".into())
                })?
        } else {
            net_qty
                .checked_abs()
                .ok_or_else(|| qx_core::QxError::Invariant("共享账户保证金持仓数量溢出".into()))?
        };
        if qty == 0 {
            continue;
        }
        let market_index = markets
            .index_by_instrument
            .get(instrument)
            .copied()
            .ok_or_else(|| {
                qx_core::QxError::Invariant("保证金规格不属于共享 Bar session 市场".into())
            })?;
        let margin = markets
            .by_index
            .get(market_index)
            .ok_or_else(|| qx_core::QxError::Invariant("保证金市场索引不属于运行时配置".into()))?
            .margin
            .instrument_initial_margin(spec, qty, mark.raw().abs(), 1)?;
        total = total
            .checked_add(margin)
            .ok_or_else(|| qx_core::QxError::Invariant("共享账户保证金合计溢出".into()))?;
    }
    Ok(total)
}

pub(super) struct OtherMarketMarginQuery<'a> {
    pub(super) visible_ts: u64,
    pub(super) ledger: &'a Ledger,
    pub(super) account_id: &'a str,
    pub(super) target: &'a InstrumentId,
    pub(super) specs: &'a BTreeMap<InstrumentId, TradingInstrumentSpec>,
    pub(super) marks: &'a BTreeMap<InstrumentId, Price>,
    pub(super) markets: &'a RuntimeMarkets<'a>,
}

pub(super) fn cached_or_other_market_margin(
    cache: &mut Option<CachedPortfolioMarginSnapshot>,
    query: OtherMarketMarginQuery<'_>,
) -> Result<i128, qx_core::QxError> {
    let OtherMarketMarginQuery {
        visible_ts,
        ledger,
        account_id,
        target,
        specs,
        marks,
        markets,
    } = query;
    if cache.as_ref().map(|(cached_ts, _)| *cached_ts) != Some(visible_ts) {
        *cache = Some((
            visible_ts,
            PortfolioMarginSnapshot::try_new(ledger, account_id, specs, marks, markets),
        ));
    }
    if let Some(Some(snapshot)) = cache.as_ref().map(|(_, snapshot)| snapshot.as_ref()) {
        if let Some(margin) = snapshot.other_market_margin(target, markets) {
            return Ok(margin);
        }
    }
    other_market_margin(ledger, account_id, target, specs, marks, markets)
}
