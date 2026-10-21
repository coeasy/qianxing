use super::{OrderBookExecutionModel, QueuePositionModel};
use qx_core::Fnv1a;
use serde::{Deserialize, Serialize};

impl QueuePositionModel {
    pub fn fingerprint(self) -> i128 {
        match self {
            Self::Disabled => 0,
            Self::FifoVolumeEstimate { volume_advance_bps } => i128::from(volume_advance_bps) + 1,
        }
    }

    pub fn audit_descriptor(self, initial_queue_bps: i128) -> QueuePositionDescriptor {
        match self {
            Self::Disabled => QueuePositionDescriptor {
                name: "disabled".into(),
                version: "v1".into(),
                initial_queue_bps,
                volume_advance_bps: None,
                assumption: "queue advancement is disabled".into(),
                limitations: Vec::new(),
            },
            Self::FifoVolumeEstimate { volume_advance_bps } => QueuePositionDescriptor {
                name: "fifo_volume_estimate".into(),
                version: "v1".into(),
                initial_queue_bps,
                volume_advance_bps: Some(volume_advance_bps),
                assumption: "displayed depth reduction estimates traded volume".into(),
                limitations: vec![
                    "hidden liquidity is unobserved".into(),
                    "cancellations are unobserved".into(),
                ],
            },
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QueuePositionDescriptor {
    pub name: String,
    pub version: String,
    pub initial_queue_bps: i128,
    pub volume_advance_bps: Option<u32>,
    pub assumption: String,
    pub limitations: Vec<String>,
}

impl OrderBookExecutionModel {
    pub(crate) fn write_identity(&self, hash: &mut Fnv1a) {
        hash.write_i128(self.fee_bps);
        hash.write_u64(self.latency_snapshots);
        hash.write_i128(self.queue_position_bps);
        hash.write_i128(self.queue_position_model.fingerprint());
        hash.write_i128(self.market_impact_bps);
    }

    pub fn audit_descriptor(&self) -> OrderBookExecutionDescriptor {
        OrderBookExecutionDescriptor {
            schema_version: 1,
            kernel: "orderbook-matching".into(),
            version: "v2".into(),
            fee_bps: self.fee_bps,
            latency_snapshots: self.latency_snapshots,
            queue_position: self
                .queue_position_model
                .audit_descriptor(self.queue_position_bps),
            market_impact_bps: self.market_impact_bps,
            assumption: "orders consume displayed depth across later snapshots".into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OrderBookExecutionDescriptor {
    pub schema_version: u32,
    pub kernel: String,
    pub version: String,
    pub fee_bps: i128,
    pub latency_snapshots: u64,
    pub queue_position: QueuePositionDescriptor,
    pub market_impact_bps: i128,
    pub assumption: String,
}
