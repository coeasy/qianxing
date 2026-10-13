//! Caller-authored strategy support for the shared two-leg Bar workflow.

use crate::{AppError, AppErrorCategory, MultiLegBacktestSpec, StrategyArtifactIdentity};
use qx_strategy::Strategy;

pub(in crate::cases) struct InjectedMultiLegStrategy {
    pub(in crate::cases) identity: StrategyArtifactIdentity,
    pub(in crate::cases) strategy: Box<dyn Strategy>,
}

pub(in crate::cases) fn validate_identity(
    spec: &MultiLegBacktestSpec,
    identity: &StrategyArtifactIdentity,
) -> Result<(), AppError> {
    let valid_token = |value: &str| {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    if !valid_token(&identity.strategy_id)
        || identity.strategy_id != spec.strategy.strategy_id
        || !valid_token(&identity.strategy_version)
        || identity.source_sha256.len() != 64
        || !identity
            .source_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "StrategyArtifactIdentity requires a matching strategy_id, a valid strategy_version, and a lowercase SHA-256 digest",
        ));
    }
    let defaults =
        crate::BuiltinStrategySpec::new(spec.strategy.kind, spec.strategy.strategy_id.clone());
    if spec.strategy != defaults {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "caller-supplied strategies must use default BuiltinStrategySpec parameters; built-in signal and sizing knobs do not apply to external callbacks",
        ));
    }
    Ok(())
}
