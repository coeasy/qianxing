use super::*;

pub(super) struct AttributionInput<'a> {
    pub spec: &'a MultiBuiltinBacktestSpec,
    pub primary_frame: &'a BarFrame,
    pub reference_frame: &'a BarFrame,
    pub primary_bars: &'a [Bar],
    pub reference_bars: &'a [Bar],
    pub primary_targets: &'a BTreeMap<u64, i128>,
    pub reference_targets: &'a BTreeMap<u64, i128>,
    pub primary_report: &'a qx_xingban::BacktestReport,
    pub reference_report: &'a qx_xingban::BacktestReport,
}

pub(super) fn build(input: AttributionInput<'_>) -> Result<MultiBuiltinAttribution, AppError> {
    let AttributionInput {
        spec,
        primary_frame,
        reference_frame,
        primary_bars,
        reference_bars,
        primary_targets,
        reference_targets,
        primary_report,
        reference_report,
    } = input;
    let primary_leg = MultiLegLegFacts {
        label: "primary",
        account_id: spec.primary.account_id.as_deref().unwrap_or_default(),
        instrument: &primary_frame.instrument,
        targets: primary_targets,
        bars: primary_bars,
        spec: spec.primary.market_spec.as_ref(),
        fills: &primary_report.fills,
        event_log: &primary_report.event_log,
        clock_end: primary_report.clock_end,
    };
    let reference_leg = MultiLegLegFacts {
        label: "reference",
        account_id: spec.reference.account_id.as_deref().unwrap_or_default(),
        instrument: &reference_frame.instrument,
        targets: reference_targets,
        bars: reference_bars,
        spec: spec.reference.market_spec.as_ref(),
        fills: &reference_report.fills,
        event_log: &reference_report.event_log,
        clock_end: reference_report.clock_end,
    };
    let primary_buckets = multi_leg_leg_buckets(&primary_leg, spec.funding_bps).map_err(invalid)?;
    let reference_buckets =
        multi_leg_leg_buckets(&reference_leg, spec.funding_bps).map_err(invalid)?;
    let strategy_id = &spec.strategy.strategy_id;
    let attribution = multi_leg_group_attributions(
        strategy_id,
        &primary_leg,
        &reference_leg,
        &primary_buckets,
        &reference_buckets,
    )
    .map_err(invalid)?;
    let totals = multi_leg_group_totals(&attribution.groups).map_err(invalid)?;
    let expected_fees_raw = primary_report
        .fees_raw
        .checked_add(reference_report.fees_raw)
        .and_then(|fees| fees.checked_sub(attribution.residual_fees_raw))
        .ok_or_else(|| invalid("多腿归因预期费用溢出"))?;
    if totals.0 != expected_fees_raw {
        return Err(invalid(format!(
            "多腿归因费用不闭合: groups={} expected={expected_fees_raw}",
            totals.0
        )));
    }
    let net_cost_raw = totals
        .0
        .checked_add(totals.2)
        .ok_or_else(|| invalid("多腿归因净成本溢出"))?;
    let leg_integrity = [
        multi_leg_leg_integrity(&primary_leg, &primary_buckets).map_err(invalid)?,
        multi_leg_leg_integrity(&reference_leg, &reference_buckets).map_err(invalid)?,
    ];
    let naked_filled_qty_raw =
        attribution
            .pending_reconcile
            .iter()
            .try_fold(0_i128, |sum, item| {
                let qty = item
                    .filled_qty_raw
                    .parse::<i128>()
                    .map_err(|error| invalid(format!("裸腿成交量无法解析: {error}")))?;
                sum.checked_add(qty)
                    .ok_or_else(|| invalid("裸腿成交量合计溢出"))
            })?;
    if naked_filled_qty_raw != attribution.residual_filled_qty_raw {
        return Err(invalid(format!(
            "多腿裸腿事实与残余成交不闭合: pending={naked_filled_qty_raw} residual={}",
            attribution.residual_filled_qty_raw
        )));
    }
    for (leg, integrity, report) in [
        (&primary_leg, &leg_integrity[0], primary_report),
        (&reference_leg, &leg_integrity[1], reference_report),
    ] {
        let filled_from_fills_raw = report.fills.iter().try_fold(0_i128, |sum, fill| {
            sum.checked_add(
                fill.qty
                    .raw()
                    .checked_abs()
                    .ok_or_else(|| invalid("腿成交数量绝对值溢出"))?,
            )
            .ok_or_else(|| invalid("腿成交数量合计溢出"))
        })?;
        if integrity.filled_qty_raw.parse::<i128>().ok() != Some(filled_from_fills_raw) {
            return Err(invalid(format!(
                "{} 腿归因成交量与撮合成交不闭合: attribution={} fills={filled_from_fills_raw}",
                leg.label, integrity.filled_qty_raw
            )));
        }
    }
    let combined_return_bps = multi_leg_combined_return_bps([
        (
            "primary",
            spec.primary.initial_cash_raw,
            primary_report.final_equity(),
        ),
        (
            "reference",
            spec.reference.initial_cash_raw,
            reference_report.final_equity(),
        ),
    ])
    .map_err(invalid)?;
    let cost_bps = multi_leg_cost_bps(net_cost_raw, totals.1).map_err(invalid)?;
    let margin_model = if [
        spec.primary.market_spec.as_ref(),
        spec.reference.market_spec.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|market| market.product.is_derivative())
    {
        "realized-initial-margin-leverage-1"
    } else {
        "none-no-derivative-leg-spec"
    };
    Ok(MultiBuiltinAttribution {
        combined_return_bps,
        groups: attribution.groups,
        totals,
        residual_fees_raw: attribution.residual_fees_raw,
        residual_filled_qty_raw: attribution.residual_filled_qty_raw,
        pending_reconcile: attribution.pending_reconcile,
        naked_filled_qty_raw,
        leg_integrity,
        net_cost_raw,
        cost_bps,
        margin_model: margin_model.into(),
    })
}
