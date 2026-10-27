//! CLI compatibility exports for service-owned multi-leg execution.

pub(crate) use qx_service::spread_recovery_host::{
    command_is_final, dedicated_spread_recovery_configured,
    pending_spread_recovery_groups_for_account, persist_strategy_spread_group,
    persist_strategy_submit, recover_paper_spread_groups_for_account,
    recover_spread_groups_for_venue, recovery_order_validator, spread_recovery_poll_delay,
    SpreadRecoveryContext,
};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use qx_service::spread_recovery_host::{
    open_spread_group_store, pending_spread_recovery_groups, recover_paper_spread_groups,
    sync_spread_group_after_order,
};
