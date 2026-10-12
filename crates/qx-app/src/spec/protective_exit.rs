use crate::{AppError, AppErrorCategory};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectiveExitSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_loss_bps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take_profit_bps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailing_stop_bps: Option<u32>,
}

impl ProtectiveExitSpec {
    pub fn validate(self) -> Result<(), AppError> {
        let values = [
            self.stop_loss_bps,
            self.take_profit_bps,
            self.trailing_stop_bps,
        ];
        if values.iter().all(Option::is_none)
            || values
                .into_iter()
                .flatten()
                .any(|bps| bps == 0 || bps >= 10_000)
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "ProtectiveExitSpec 至少配置一项，且基点必须在 1..=9999",
            ));
        }
        Ok(())
    }
}
