use super::*;

pub(super) fn apply_ashare_virtual_events(
    config: &VirtualTradingConfig,
    ashare_action_schedule: Option<&AshareCorporateActionSchedule>,
    bar: &Bar,
    state: &mut VirtualExecution<'_>,
) -> Result<(), qx_core::QxError> {
    if let Some(rules) = config.ashare_rules.as_ref().filter(|rules| rules.enabled) {
        let schedule = ashare_action_schedule
            .ok_or_else(|| qx_core::QxError::Invariant("A 股回测缺公司行为时间索引".into()))?;
        // 现金分红按登记日（或没有登记日时的除权日）生成待支付权益，
        // 不在此处直接增加现金，避免支付日按错误的当前持仓重新计算。
        for event in schedule
            .events_at(&rules.corporate_actions, bar.ts)
            .filter(|event| {
                is_cash_dividend_action(event.action_type)
                    && event.cash_dividend_raw > 0
                    && (schedule.trigger_ts(event.record_ts) == Some(bar.ts)
                        || (event.record_ts.is_none()
                            && event.payment_ts.is_some()
                            && schedule.trigger_ts(Some(event.ts)) == Some(bar.ts)))
            })
        {
            if let Some(entry_id) = state.ledger.grant_cash_dividend_entitlement(
                state.account_id,
                state.instrument,
                state.currency,
                event.cash_dividend_raw,
                bar.ts,
            )? {
                append_ashare_action_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }

        // 可转债利息在登记日锁定债券持仓，支付日只结算已登记的权益。
        for event in schedule
            .events_at(&rules.corporate_actions, bar.ts)
            .filter(|event| {
                is_convertible_bond_interest_action(event.action_type)
                    && event.interest_per_bond_raw > 0
                    && (schedule.trigger_ts(event.record_ts) == Some(bar.ts)
                        || (event.record_ts.is_none()
                            && event.payment_ts.is_some()
                            && schedule.trigger_ts(Some(event.ts)) == Some(bar.ts)))
            })
        {
            let bond_instrument = event
                .convertible_bond_instrument
                .as_deref()
                .ok_or_else(|| {
                    qx_core::QxError::BusinessViolation(
                        "可转债利息事件缺少 convertible_bond_instrument".into(),
                    )
                })
                .and_then(|value| {
                    InstrumentId::parse(value).ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(
                            "可转债利息 convertible_bond_instrument 格式非法".into(),
                        )
                    })
                })?;
            if let Some(entry_id) = state.ledger.grant_convertible_bond_interest_entitlement(
                state.account_id,
                &bond_instrument,
                state.currency,
                event.interest_per_bond_raw,
                bar.ts,
            )? {
                append_ashare_action_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }

        // 登记日只授予独立权利，不能在除权日重新读取持仓推导配额。
        for event in schedule
            .events_at(&rules.corporate_actions, bar.ts)
            .filter(|event| {
                event.action_type == AshareCorporateActionType::RightsIssue
                    && schedule.trigger_ts(event.record_ts) == Some(bar.ts)
            })
        {
            let rights_instrument = event
                .rights_instrument
                .as_deref()
                .ok_or_else(|| {
                    qx_core::QxError::BusinessViolation("配股登记事件缺少 rights_instrument".into())
                })
                .and_then(|value| {
                    InstrumentId::parse(value).ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(
                            "配股登记 rights_instrument 格式非法".into(),
                        )
                    })
                })?;
            let current_position = state
                .ledger
                .position_for(state.account_id, state.instrument)
                .quantity
                .raw();
            let entitled_qty = current_position
                .checked_mul(event.rights_issue_ratio_num)
                .and_then(|value| value.checked_div(event.rights_issue_ratio_den))
                .ok_or_else(|| qx_core::QxError::Invariant("配股配额计算溢出".into()))?;
            let entry_id = state.ledger.grant_rights_entitlement(
                state.account_id,
                state.instrument,
                &rights_instrument,
                state.currency,
                entitled_qty,
                bar.ts,
            )?;
            append_ashare_action_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
        }

        for event in schedule
            .events_at(&rules.corporate_actions, bar.ts)
            .filter(|event| {
                (schedule.trigger_ts(Some(event.ts)) == Some(bar.ts)
                    && (event.record_ts.is_none()
                        || event.subscription_start_ts.is_none()
                        || schedule.trigger_ts(event.subscription_start_ts) == Some(bar.ts))
                    && !(event.action_type == AshareCorporateActionType::ConvertibleBondIssue
                        && event.subscription_start_ts.is_some())
                    && !is_convertible_bond_interest_action(event.action_type)
                    && !(is_convertible_bond_settlement_action(event.action_type)
                        && event.payment_ts.is_some()))
                    || (event.action_type == AshareCorporateActionType::RightsIssue
                        && schedule.trigger_ts(event.subscription_start_ts) == Some(bar.ts))
                    || (event.action_type == AshareCorporateActionType::ConvertibleBondIssue
                        && schedule.trigger_ts(event.subscription_start_ts) == Some(bar.ts))
                    || (is_convertible_bond_settlement_action(event.action_type)
                        && schedule.trigger_ts(event.payment_ts) == Some(bar.ts))
            })
        {
            let entry_ids = match event.action_type {
                AshareCorporateActionType::CashDividend
                | AshareCorporateActionType::BonusShare
                | AshareCorporateActionType::CapitalTransfer
                | AshareCorporateActionType::Unknown => state.ledger.apply_corporate_action(
                    state.account_id,
                    state.instrument,
                    state.currency,
                    qx_core::CorporateAction {
                        cash_dividend_raw: if event.record_ts.is_some()
                            || event.payment_ts.is_some()
                        {
                            0
                        } else {
                            event.cash_dividend_raw
                        },
                        split_num: event.split_num,
                        split_den: event.split_den,
                    },
                    bar.ts,
                )?,
                AshareCorporateActionType::RightsIssue => {
                    let rights_instrument = event
                        .rights_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "配股事件缺少 rights_instrument，不能隐式使用普通股标的".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "配股 rights_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    if event.record_ts.is_some() {
                        if event.subscription_qty_raw == 0 {
                            Vec::new()
                        } else {
                            state
                                .ledger
                                .apply_rights_issue_subscription_from_entitlement(
                                    state.account_id,
                                    &rights_instrument,
                                    state.currency,
                                    event.subscription_qty_raw,
                                    event.rights_issue_price_raw,
                                    bar.ts,
                                )?
                        }
                    } else {
                        let current_position = state
                            .ledger
                            .position_for(state.account_id, state.instrument)
                            .quantity
                            .raw();
                        let entitled_qty = current_position
                            .checked_mul(event.rights_issue_ratio_num)
                            .and_then(|value| value.checked_div(event.rights_issue_ratio_den))
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant("配股配额计算溢出".into())
                            })?;
                        state.ledger.apply_rights_issue_event(
                            state.account_id,
                            qx_core::RightsIssueEvent {
                                source_instrument: state.instrument.clone(),
                                rights_instrument,
                                currency: state.currency.to_owned(),
                                entitled_qty_raw: entitled_qty,
                                subscription_qty_raw: event.subscription_qty_raw,
                                subscription_price_raw: event.rights_issue_price_raw,
                            },
                            bar.ts,
                        )?
                    }
                }
                AshareCorporateActionType::RightsIssueExpiry => {
                    let rights_instrument = event
                        .rights_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "配股权利失效事件缺少 rights_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "配股权利失效 rights_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    vec![state.ledger.expire_rights_entitlement(
                        state.account_id,
                        &rights_instrument,
                        event.rights_expiry_qty_raw,
                        state.currency,
                        bar.ts,
                    )?]
                }
                AshareCorporateActionType::NewShareIssue => {
                    state.ledger.apply_new_share_subscription(
                        state.account_id,
                        state.instrument,
                        state.currency,
                        event.subscription_qty_raw,
                        event.issue_price_raw,
                        bar.ts,
                    )?
                }
                AshareCorporateActionType::ConvertibleBondIssue => {
                    let bond_instrument = event
                        .convertible_bond_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "可转债发行事件缺少 convertible_bond_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "可转债发行 convertible_bond_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    state.ledger.apply_convertible_bond_issue_subscription(
                        state.account_id,
                        &bond_instrument,
                        state.currency,
                        event.subscription_qty_raw,
                        event.issue_price_raw,
                        bar.ts,
                    )?
                }
                AshareCorporateActionType::ConvertibleBondRedemption
                | AshareCorporateActionType::ConvertibleBondPut => {
                    let bond_instrument = event
                        .convertible_bond_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "可转债回售/赎回事件缺少 convertible_bond_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "可转债回售/赎回 convertible_bond_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    state.ledger.apply_convertible_bond_tender(
                        state.account_id,
                        &bond_instrument,
                        state.currency,
                        event.settlement_qty_raw,
                        event.settlement_price_raw,
                        bar.ts,
                    )?
                }
                AshareCorporateActionType::ConvertibleBondCall => {
                    let bond_instrument = event
                        .convertible_bond_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "可转债强赎事件缺少 convertible_bond_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "可转债强赎 convertible_bond_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    let quantity_raw = if event.settlement_qty_raw > 0 {
                        event.settlement_qty_raw
                    } else {
                        state
                            .ledger
                            .position_for(state.account_id, &bond_instrument)
                            .quantity
                            .raw()
                    };
                    if quantity_raw <= 0 {
                        Vec::new()
                    } else {
                        state.ledger.apply_convertible_bond_tender(
                            state.account_id,
                            &bond_instrument,
                            state.currency,
                            quantity_raw,
                            event.settlement_price_raw,
                            bar.ts,
                        )?
                    }
                }
                AshareCorporateActionType::Repurchase => state.ledger.apply_repurchase_tender(
                    state.account_id,
                    state.instrument,
                    state.currency,
                    event.repurchase_qty_raw,
                    event.repurchase_price_raw,
                    bar.ts,
                )?,
                AshareCorporateActionType::ConvertibleBondConversion => {
                    let bond_instrument = event
                        .convertible_bond_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "可转债转股事件缺少 convertible_bond_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "可转债 convertible_bond_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    let target_instrument = event
                        .conversion_target_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "可转债转股事件缺少 conversion_target_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "可转债 conversion_target_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    state.ledger.apply_convertible_bond_conversion(
                        state.account_id,
                        state.currency,
                        qx_core::ConvertibleBondConversion {
                            bond_instrument,
                            target_instrument,
                            bond_qty_raw: event.conversion_qty_raw,
                            target_qty_raw: event.conversion_target_qty_raw,
                            conversion_price_raw: event.conversion_price_raw,
                        },
                        bar.ts,
                    )?
                }
                AshareCorporateActionType::CapitalChange => {
                    // 发行人股本是显式市场事实，已在 BacktestReport 中保存；
                    // 这里不能生成账户 Ledger entry，也不能按持仓比例猜测。
                    Vec::new()
                }
                unsupported => {
                    return Err(qx_core::QxError::BusinessViolation(format!(
                        "A 股公司行为 {:?} 需要发行人/登记结算事实，当前禁止静默回测",
                        unsupported
                    )));
                }
            };
            for entry_id in entry_ids {
                append_ashare_action_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }

        // 认购截止日自动使剩余权利失效；若数据源另有显式失效事实，
        // 以显式事件为准，避免同一权利被重复扣减。
        for event in schedule
            .events_at(&rules.corporate_actions, bar.ts)
            .filter(|event| {
                event.action_type == AshareCorporateActionType::RightsIssue
                    && schedule.trigger_ts(event.subscription_end_ts) == Some(bar.ts)
                    && !schedule
                        .has_explicit_rights_expiry(bar.ts, event.rights_instrument.as_deref())
            })
        {
            let rights_instrument = event
                .rights_instrument
                .as_deref()
                .ok_or_else(|| {
                    qx_core::QxError::BusinessViolation("配股截止事件缺少 rights_instrument".into())
                })
                .and_then(|value| {
                    InstrumentId::parse(value).ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(
                            "配股截止 rights_instrument 格式非法".into(),
                        )
                    })
                })?;
            let remaining = state
                .ledger
                .rights_entitlement_for(state.account_id, &rights_instrument)
                .raw();
            if remaining > 0 {
                let entry_id = state.ledger.expire_rights_entitlement(
                    state.account_id,
                    &rights_instrument,
                    remaining,
                    state.currency,
                    bar.ts,
                )?;
                append_ashare_action_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }

        // 支付日只结算登记日已经锁定的权益；没有 payment_date 但存在
        // record_date 时，除权日作为兼容的支付时点。
        for _event in schedule
            .events_at(&rules.corporate_actions, bar.ts)
            .filter(|event| {
                is_cash_dividend_action(event.action_type)
                    && (event.record_ts.is_some() || event.payment_ts.is_some())
                    && schedule.trigger_ts(Some(event.payment_ts.unwrap_or(event.ts)))
                        == Some(bar.ts)
            })
        {
            if let Some(entry_id) = state.ledger.settle_cash_dividend_entitlement(
                state.account_id,
                state.instrument,
                state.currency,
                bar.ts,
            )? {
                append_ashare_action_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }
        for event in schedule
            .events_at(&rules.corporate_actions, bar.ts)
            .filter(|event| {
                is_convertible_bond_interest_action(event.action_type)
                    && (event.record_ts.is_some() || event.payment_ts.is_some())
                    && schedule.trigger_ts(Some(event.payment_ts.unwrap_or(event.ts)))
                        == Some(bar.ts)
            })
        {
            let bond_instrument = event
                .convertible_bond_instrument
                .as_deref()
                .ok_or_else(|| {
                    qx_core::QxError::BusinessViolation(
                        "可转债利息支付事件缺少 convertible_bond_instrument".into(),
                    )
                })
                .and_then(|value| {
                    InstrumentId::parse(value).ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(
                            "可转债利息支付 convertible_bond_instrument 格式非法".into(),
                        )
                    })
                })?;
            if let Some(entry_id) = state.ledger.settle_convertible_bond_interest_entitlement(
                state.account_id,
                &bond_instrument,
                state.currency,
                bar.ts,
            )? {
                append_ashare_action_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }
    }
    Ok(())
}
