//! Test-tree dependencies kept out of the production CLI root.
pub(crate) use qx_api::{ApiPolicy, ApiService, ApiState};
pub(crate) use qx_app::artifact_store::recompute_declared_backtest_input;
pub(crate) use qx_app::{
    market_spec_source_label, multi_leg_cost_bps, CCXT_MARKET_SPEC_VERSION,
    PRODUCT_MARKET_SPEC_VERSION,
};
pub(crate) use qx_control::CommandStatus;
pub(crate) use qx_control::ControlPlane;
pub(crate) use qx_core::{MarginMode, OrderStatus, PositionMode, PositionSide, Side, SCALE};
pub(crate) use qx_factor::{
    CandidateRequest, FactorCatalog, FeatureArtifact, FeatureDefinition, StrategyResearchSnapshot,
};
pub(crate) use qx_guanxing::DataSourceId;
pub(crate) use qx_protocol::AccountSnapshot;
pub(crate) use qx_runtime::{
    load_control_state, ApiTransport, StorageBackend, StrategyContractBars, StrategyContractInput,
    StrategyRuntimeConfig, StrategyTargetSnapshot, StrategyTransport,
};
pub(crate) use qx_scheduler::{
    unsupported_dispatch_shape, JobSpec, JobStatus, JobWindow, RetryPolicy, Scheduler, Trigger,
};
pub(crate) use qx_storage::{FileJobQueue, QueuedJob};
pub(crate) use qx_strategy::SharedRingConfig;
pub(crate) use qx_xingban::{AshareRuleConfig, DepthFrame, ExecutionCostRules};
pub(crate) use qx_zhenlu::{SpreadOrderGroup, SpreadOrderGroupStatus, SpreadOrderLeg};
pub(crate) use std::process::Command;
pub(crate) use std::sync::atomic::{AtomicBool, Ordering};
pub(crate) use std::time::{SystemTime, UNIX_EPOCH};
