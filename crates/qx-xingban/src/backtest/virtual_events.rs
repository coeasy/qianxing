use super::*;

mod ashare;

pub(super) fn apply_virtual_events(
    config: &VirtualTradingConfig,
    ashare_action_schedule: Option<&AshareCorporateActionSchedule>,
    bar: &Bar,
    state: &mut VirtualExecution<'_>,
) -> Result<(), qx_core::QxError> {
    ashare::apply_ashare_virtual_events(config, ashare_action_schedule, bar, state)?;
    // 公司行为可能改变数量；后续资金费按归约后的最新持仓计算，避免
    // 在同一根 bar 上继续使用事件前的旧快照。
    let position = state
        .ledger
        .position_for(state.account_id, state.instrument)
        .quantity
        .raw();
    for event in events_at(&config.funding, bar.ts, |event| event.ts) {
        if position == 0 {
            continue;
        }
        let amount = if let Some(spec) = state.spec {
            spec.funding_payment(position, bar.close, event.rate_bp)?
                .saturating_neg()
        } else {
            let notional = notional_for(
                state.spec,
                checked_abs(position)?,
                checked_abs(bar.close)?,
                state.multiplier,
            )?;
            let signed = if position > 0 { -1 } else { 1 };
            qx_core::bp_amount(notional, event.rate_bp).saturating_mul(signed)
        };
        if amount != 0 {
            let entry_id = state.ledger.apply_funding(
                state.account_id,
                state.currency,
                Money::from_raw(amount),
                bar.ts,
            )?;
            append_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
        }
    }
    for event in events_at(&config.interest, bar.ts, |event| event.ts) {
        let cash = state.ledger.cash_for(state.account_id, state.currency);
        let amount = qx_core::bp_amount(cash, event.rate_bp);
        if amount != 0 {
            let entry_id = state.ledger.apply_interest(
                state.account_id,
                state.currency,
                Money::from_raw(amount),
                bar.ts,
            )?;
            append_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
        }
    }
    for event in events_at(&config.delivery, bar.ts, |event| event.ts) {
        let settle_seq = state.log.alloc_seq();
        state.log.append(Event::new(
            settle_seq,
            bar.ts,
            Priority::APPLY,
            EventKind::Settle,
        ));
        let current = state
            .ledger
            .position_for(state.account_id, state.instrument)
            .quantity
            .raw();
        let long_leg = state
            .ledger
            .position_for_side(
                state.account_id,
                state.instrument,
                qx_core::PositionSide::Long,
            )
            .quantity
            .raw();
        let short_leg = state
            .ledger
            .position_for_side(
                state.account_id,
                state.instrument,
                qx_core::PositionSide::Short,
            )
            .quantity
            .raw();
        // 单向（Net）持仓在双向账本里两条腿都是 0，"按腿结算"等于什么都不平；
        // 与强平路径共用同一分支规则。
        if state.spec.is_some() && (long_leg != 0 || short_leg != 0) {
            for (position_side, leg) in [
                (qx_core::PositionSide::Long, long_leg),
                (qx_core::PositionSide::Short, short_leg),
            ] {
                close_virtual_position(
                    state,
                    leg,
                    event.settlement_price,
                    bar.ts,
                    None,
                    Some(position_side),
                )?;
            }
        } else {
            close_virtual_position(state, current, event.settlement_price, bar.ts, None, None)?;
        }
    }
    Ok(())
}
