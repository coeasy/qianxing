from pathlib import Path
import re


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    return text.replace(old, new, 1)


# qx-zhenlu: represent one-way and hedge legs explicitly.
path = Path("crates/qx-zhenlu/src/lib.rs")
source = path.read_text(encoding="utf-8")
source = replace_once(
    source,
    """pub struct PositionSnapshot {
    pub net_qty: i128,
    pub gross_notional: i128,
    pub multiplier: i128,
}""",
    """pub struct PositionSnapshot {
    /// One-way/net position only. Hedge-mode long/short legs are stored separately.
    pub net_qty: i128,
    /// Gross notional across one-way and both hedge legs at the snapshot mark.
    pub gross_notional: i128,
    pub multiplier: i128,
    pub long_qty: i128,
    pub short_qty: i128,
}""",
    "PositionSnapshot struct",
)
source = replace_once(
    source,
    """impl PositionSnapshot {
    pub fn new(net_qty: i128, gross_notional: i128) -> Self {
        Self {
            net_qty,
            gross_notional,
            multiplier: 1,
        }
    }

    pub fn new_with_multiplier(net_qty: i128, gross_notional: i128, multiplier: i128) -> Self {
        Self {
            net_qty,
            gross_notional,
            multiplier: multiplier.max(1),
        }
    }
}""",
    """impl PositionSnapshot {
    pub fn new(net_qty: i128, gross_notional: i128) -> Self {
        Self {
            net_qty,
            gross_notional,
            multiplier: 1,
            long_qty: 0,
            short_qty: 0,
        }
    }

    pub fn new_with_multiplier(net_qty: i128, gross_notional: i128, multiplier: i128) -> Self {
        Self {
            net_qty,
            gross_notional,
            multiplier: multiplier.max(1),
            long_qty: 0,
            short_qty: 0,
        }
    }

    pub fn with_hedge_legs(mut self, long_qty: i128, short_qty: i128) -> Self {
        self.long_qty = long_qty;
        self.short_qty = short_qty;
        self
    }

    pub fn active_qty_for(&self, order: &Order) -> i128 {
        let policy = order.policy.unwrap_or_default();
        if policy.position_mode == qx_core::PositionMode::Hedge {
            match policy.position_side {
                qx_core::PositionSide::Long => self.long_qty,
                qx_core::PositionSide::Short => self.short_qty,
                qx_core::PositionSide::Net => self.net_qty,
            }
        } else {
            self.net_qty
        }
    }
}

fn validate_reduce_only(order: &Order, position: &PositionSnapshot) -> QxResult<()> {
    let policy = order.policy.unwrap_or_default();
    if !policy.reduce_only {
        return Ok(());
    }
    let current = position.active_qty_for(order);
    let current_abs = current
        .checked_abs()
        .ok_or_else(|| QxError::Invariant("当前持仓绝对值溢出".into()))?;
    let reduces_direction = (current > 0 && order.side == Side::Sell)
        || (current < 0 && order.side == Side::Buy);
    if current_abs == 0 || !reduces_direction || order.qty.raw() > current_abs {
        return Err(QxError::BusinessViolation(
            "reduce_only 订单必须只减少目标持仓腿且不得反向穿仓".into(),
        ));
    }
    Ok(())
}""",
    "PositionSnapshot impl",
)
context_anchor = source.index("impl RiskContext")
start = source.index("        let signed_delta = match order.side {", context_anchor)
end = source.index("        if let Some(available) = self.available_margin_raw {", start)
source = source[:start] + """        validate_reduce_only(order, position)?;
        let current_qty = position.active_qty_for(order);
        let signed_delta = match order.side {
            Side::Buy => order.qty.raw(),
            Side::Sell => order
                .qty
                .raw()
                .checked_neg()
                .ok_or_else(|| QxError::Invariant("订单数量取反溢出".into()))?,
        };
        let projected_qty = current_qty
            .checked_add(signed_delta)
            .ok_or_else(|| QxError::Invariant("投影持仓数量溢出".into()))?;
        if let Some(limit) = self.max_position_notional_raw {
            let current_abs = current_qty
                .checked_abs()
                .ok_or_else(|| QxError::Invariant("当前持仓绝对值溢出".into()))?;
            let projected_abs = projected_qty
                .checked_abs()
                .ok_or_else(|| QxError::Invariant("投影持仓绝对值溢出".into()))?;
            let current_leg_notional = spec.notional(current_abs, price.raw())?;
            let projected_leg_notional = spec.notional(projected_abs, price.raw())?;
            let other_notional = position
                .gross_notional
                .saturating_sub(current_leg_notional);
            let projected_gross_notional = other_notional
                .checked_add(projected_leg_notional)
                .ok_or_else(|| QxError::Invariant("投影持仓名义额溢出".into()))?;
            if projected_gross_notional > limit {
                return Err(QxError::BusinessViolation(
                    "投影持仓名义额超过账户限额".into(),
                ));
            }
        }
""" + source[end:]
source = replace_once(
    source,
    """        let signed_delta = match o.side {
            Side::Buy => o.qty.raw(),
            Side::Sell => o.qty.raw().saturating_neg(),
        };
        let projected_qty = pos.net_qty.saturating_add(signed_delta);
        let current_abs = pos.net_qty.saturating_abs();""",
    """        let current_qty = pos.active_qty_for(o);
        let signed_delta = match o.side {
            Side::Buy => o.qty.raw(),
            Side::Sell => o.qty.raw().saturating_neg(),
        };
        let projected_qty = current_qty.saturating_add(signed_delta);
        let current_abs = current_qty.saturating_abs();""",
    "MaxNotionalRule",
)
source = replace_once(
    source,
    """        let mut errs = Vec::new();
        for r in &self.rules {
            if let Err(e) = r.check_with_price(o, pos, reference_price) {
                errs.push(format!("{}: {}", r.name(), e));
            }
        }""",
    """        let mut errs = Vec::new();
        if let Err(error) = validate_reduce_only(o, pos) {
            errs.push(format!("ReduceOnly: {error}"));
        }
        for r in &self.rules {
            if let Err(e) = r.check_with_price(o, pos, reference_price) {
                errs.push(format!("{}: {}", r.name(), e));
            }
        }""",
    "RiskGate invariant",
)
path.write_text(source, encoding="utf-8")

# qx-cli live execution snapshot: do not net hedge legs away.
path = Path("crates/qx-cli/src/main.rs")
source = path.read_text(encoding="utf-8")
source = replace_once(
    source,
    """    let state = pipeline
        .ledger()
        .position_for(account_id, &order.instrument);
    let position_qty = state
        .quantity
        .raw()
        .checked_abs()
        .ok_or_else(|| "当前持仓数量绝对值溢出".to_string())?;
    let gross_notional = reference_price
        .map(|price| spec.notional(position_qty, price.raw()))
        .transpose()
        .map_err(|error| format!("计算当前持仓名义额失败: {error:?}"))?
        .unwrap_or(0);
    let position = PositionSnapshot::new(state.quantity.raw(), gross_notional);""",
    """    let aggregate = pipeline
        .ledger()
        .position_for(account_id, &order.instrument);
    let long_qty = pipeline
        .ledger()
        .position_for_side(account_id, &order.instrument, qx_core::PositionSide::Long)
        .quantity
        .raw();
    let short_qty = pipeline
        .ledger()
        .position_for_side(account_id, &order.instrument, qx_core::PositionSide::Short)
        .quantity
        .raw();
    let one_way_qty = aggregate
        .quantity
        .raw()
        .checked_sub(long_qty)
        .and_then(|value| value.checked_sub(short_qty))
        .ok_or_else(|| "拆分 one-way/hedge 持仓数量溢出".to_string())?;
    let gross_notional = reference_price
        .map(|price| {
            let one_way = spec.notional(one_way_qty.saturating_abs(), price.raw())?;
            let long = spec.notional(long_qty.saturating_abs(), price.raw())?;
            let short = spec.notional(short_qty.saturating_abs(), price.raw())?;
            one_way
                .checked_add(long)
                .and_then(|value| value.checked_add(short))
                .ok_or_else(|| qx_core::QxError::Invariant("当前 gross notional 溢出".into()))
        })
        .transpose()
        .map_err(|error| format!("计算当前持仓名义额失败: {error:?}"))?
        .unwrap_or(0);
    let position = PositionSnapshot::new_with_multiplier(
        one_way_qty,
        gross_notional,
        spec.contract_size,
    )
    .with_hedge_legs(long_qty, short_qty);""",
    "live risk snapshot",
)
path.write_text(source, encoding="utf-8")

# Bar backtest: hedge-aware snapshot while preserving causal visible_close.
path = Path("crates/qx-xingban/src/backtest.rs")
source = path.read_text(encoding="utf-8")
bar_pattern = re.compile(
    r"\s*let gross_notional = notional_for\(\s*derivative_spec,\s*checked_abs\(position\)\?,\s*checked_abs\(visible_close\)\?,\s*multiplier,\s*\)\?;\s*if matches!\(order\.status, OrderStatus::PendingSubmit\) \{.*?let risk_result = risk\.check_with_price\(\s*&order,\s*&PositionSnapshot::new_with_multiplier\(\s*position,\s*gross_notional,\s*multiplier,\s*\),\s*Some\(Price::from_raw\(reference_price\)\),\s*\);",
    re.S,
)
bar_replacement = """
                    let long_qty = ledger
                        .position_for_side(
                            &account_id,
                            &instrument,
                            qx_core::PositionSide::Long,
                        )
                        .quantity
                        .raw();
                    let short_qty = ledger
                        .position_for_side(
                            &account_id,
                            &instrument,
                            qx_core::PositionSide::Short,
                        )
                        .quantity
                        .raw();
                    let one_way_qty = position
                        .checked_sub(long_qty)
                        .and_then(|value| value.checked_sub(short_qty))
                        .ok_or_else(|| {
                            qx_core::QxError::Invariant(
                                "拆分回测 one-way/hedge 持仓数量溢出".into(),
                            )
                        })?;
                    let mark_abs = checked_abs(visible_close)?;
                    let one_way_notional = notional_for(
                        derivative_spec,
                        checked_abs(one_way_qty)?,
                        mark_abs,
                        multiplier,
                    )?;
                    let long_notional = notional_for(
                        derivative_spec,
                        checked_abs(long_qty)?,
                        mark_abs,
                        multiplier,
                    )?;
                    let short_notional = notional_for(
                        derivative_spec,
                        checked_abs(short_qty)?,
                        mark_abs,
                        multiplier,
                    )?;
                    let gross_notional = one_way_notional
                        .checked_add(long_notional)
                        .and_then(|value| value.checked_add(short_notional))
                        .ok_or_else(|| {
                            qx_core::QxError::Invariant(
                                "回测 hedge gross notional 溢出".into(),
                            )
                        })?;
                    if matches!(order.status, OrderStatus::PendingSubmit) {
                        order.status = OrderStatus::Submitted;
                    }
                    let risk_position = PositionSnapshot::new_with_multiplier(
                        one_way_qty,
                        gross_notional,
                        multiplier,
                    )
                    .with_hedge_legs(long_qty, short_qty);
                    let risk_result = risk.check_with_price(
                        &order,
                        &risk_position,
                        Some(Price::from_raw(reference_price)),
                    );"""
source, count = bar_pattern.subn(bar_replacement, source, count=1)
if count != 1:
    raise SystemExit(f"bar risk snapshot: expected 1 structural match, found {count}")
path.write_text(source, encoding="utf-8")

# OrderBook/Tick backtest: same account snapshot and invariant.
path = Path("crates/qx-xingban/src/orderbook_backtest.rs")
source = path.read_text(encoding="utf-8")
ob_pattern = re.compile(
    r"\s*let position_snapshot = if let Some\(spec\) = instrument_spec\.as_ref\(\) \{\s*PositionSnapshot::new_with_multiplier\(\s*position,\s*reference_price\s*\.map\(\|price\| spec\.notional\(position\.abs\(\), price\.raw\(\)\)\.unwrap_or\(0\)\)\s*\.unwrap_or\(0\),\s*spec\.contract_size,\s*\)\s*\} else \{\s*PositionSnapshot::new\(position, 0\)\s*\};",
    re.S,
)
ob_replacement = """
                let long_qty = ledger
                    .position_for_side(&account_id, &instrument, qx_core::PositionSide::Long)
                    .quantity
                    .raw();
                let short_qty = ledger
                    .position_for_side(&account_id, &instrument, qx_core::PositionSide::Short)
                    .quantity
                    .raw();
                let one_way_qty = position
                    .checked_sub(long_qty)
                    .and_then(|value| value.checked_sub(short_qty))
                    .ok_or_else(|| {
                        qx_core::QxError::Invariant(
                            "拆分订单簿 one-way/hedge 持仓数量溢出".into(),
                        )
                    })?;
                let position_snapshot = if let Some(spec) = instrument_spec.as_ref() {
                    let gross_notional = if let Some(price) = reference_price {
                        let one_way = spec.notional(one_way_qty.saturating_abs(), price.raw())?;
                        let long = spec.notional(long_qty.saturating_abs(), price.raw())?;
                        let short = spec.notional(short_qty.saturating_abs(), price.raw())?;
                        one_way
                            .checked_add(long)
                            .and_then(|value| value.checked_add(short))
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant(
                                    "订单簿 hedge gross notional 溢出".into(),
                                )
                            })?
                    } else {
                        0
                    };
                    PositionSnapshot::new_with_multiplier(
                        one_way_qty,
                        gross_notional,
                        spec.contract_size,
                    )
                    .with_hedge_legs(long_qty, short_qty)
                } else {
                    PositionSnapshot::new(one_way_qty, 0).with_hedge_legs(long_qty, short_qty)
                };"""
source, count = ob_pattern.subn(ob_replacement, source, count=1)
if count != 1:
    raise SystemExit(f"orderbook risk snapshot: expected 1 structural match, found {count}")
path.write_text(source, encoding="utf-8")
