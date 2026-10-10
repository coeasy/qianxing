//! Multi-leg attribution is implemented once in `qx-app`; this module keeps the CLI facade thin.

pub(crate) use qx_app::{
    multi_leg_cost_bps, multi_leg_group_attributions, multi_leg_group_totals,
    multi_leg_integrity_json, multi_leg_integrity_summary, multi_leg_leg_buckets,
    multi_leg_leg_integrity, multi_leg_pending_reconcile_json, rejection_count, rejection_facts,
    rejection_facts_line, spread_group_id, MultiLegAttribution, MultiLegLegFacts,
};
