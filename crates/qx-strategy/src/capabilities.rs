//! Host capabilities describe delivered behavior, not optional no-op strategy methods.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyCapability {
    Bar,
    Tick,
    Orderbook,
    BarHistory,
    MultiInstrument,
    FillUpdates,
    OrderStatusUpdates,
    AccountUpdates,
    PositionUpdates,
    Timer,
    DurableCheckpoint,
}

impl StrategyCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            StrategyCapability::Bar => "bar",
            StrategyCapability::Tick => "tick",
            StrategyCapability::Orderbook => "orderbook",
            StrategyCapability::BarHistory => "bar_history",
            StrategyCapability::MultiInstrument => "multi_instrument",
            StrategyCapability::FillUpdates => "fill_updates",
            StrategyCapability::OrderStatusUpdates => "order_status_updates",
            StrategyCapability::AccountUpdates => "account_updates",
            StrategyCapability::PositionUpdates => "position_updates",
            StrategyCapability::Timer => "timer",
            StrategyCapability::DurableCheckpoint => "durable_checkpoint",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyHostCapabilitiesV1 {
    pub schema_version: u32,
    pub host_id: String,
    pub profile: String,
    pub supported: Vec<StrategyCapability>,
}

pub const STRATEGY_HOST_CAPABILITY_ACK_SCHEMA_VERSION: u32 = 1;

/// Optional strategy-owned capability requirements exposed by C ABI plugins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyRequiredCapabilitiesV1 {
    pub schema_version: u32,
    pub required_capabilities: Vec<StrategyCapability>,
}

impl StrategyRequiredCapabilitiesV1 {
    pub fn validate_required_capabilities_against_host(
        &self,
        host: &StrategyHostCapabilitiesV1,
    ) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "StrategyRequiredCapabilitiesV1 schema_version 不受支持: {}",
                self.schema_version
            ));
        }
        host.validate_required(&self.required_capabilities)
    }
}

/// Startup acknowledgment from a process host that read and accepted the exact catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyHostCapabilityAckV1 {
    pub schema_version: u32,
    pub host_id: String,
    pub profile: String,
    pub strategy_contract_schema_version: u32,
    pub supported: Vec<StrategyCapability>,
    pub required_capabilities: Vec<StrategyCapability>,
    pub ready: bool,
}

impl StrategyHostCapabilitiesV1 {
    pub fn validate_acknowledgment(
        &self,
        acknowledgment: &StrategyHostCapabilityAckV1,
        required: &[StrategyCapability],
        strategy_contract_schema_version: u32,
    ) -> Result<(), String> {
        self.validate_required(required)?;
        if acknowledgment.schema_version != STRATEGY_HOST_CAPABILITY_ACK_SCHEMA_VERSION
            || acknowledgment.host_id != self.host_id
            || acknowledgment.profile != self.profile
            || acknowledgment.strategy_contract_schema_version != strategy_contract_schema_version
            || acknowledgment.supported != self.supported
            || acknowledgment.required_capabilities != required
            || !acknowledgment.ready
        {
            return Err(
                "external Strategy capability acknowledgment does not match host contract".into(),
            );
        }
        Ok(())
    }

    /// Persistent class adapter delivery paths in the Rust strategy worker.
    /// This is protocol support, not evidence that a feed or venue is healthy.
    pub fn runtime_python_class(
        data_level: &str,
        scoped_account_updates: bool,
    ) -> Result<Self, String> {
        use StrategyCapability::*;
        let mut supported = match data_level {
            "bar" => vec![Bar, BarHistory],
            "tick" => vec![Tick],
            "orderbook" => vec![Orderbook],
            "timer" => vec![Timer],
            _ => {
                return Err(format!(
                    "unsupported Runtime Python data level: {data_level}"
                ));
            }
        };
        supported.extend([FillUpdates, OrderStatusUpdates, DurableCheckpoint]);
        if scoped_account_updates {
            supported.extend([AccountUpdates, PositionUpdates]);
        }
        Ok(Self {
            schema_version: 1,
            host_id: format!("runtime_python_class_{data_level}"),
            profile: "server".into(),
            supported,
        })
    }

    /// Persistent module-function delivery through the versioned StrategyInput protocol.
    /// This host accepts each market-data variant represented by that protocol, but has
    /// no timer callback; account and position updates still require an explicit scope.
    pub fn runtime_python_module_function(scoped_account_updates: bool) -> Self {
        use StrategyCapability::*;
        let mut supported = vec![
            Bar,
            BarHistory,
            Tick,
            Orderbook,
            FillUpdates,
            OrderStatusUpdates,
            DurableCheckpoint,
        ];
        if scoped_account_updates {
            supported.extend([AccountUpdates, PositionUpdates]);
        }
        Self {
            schema_version: 1,
            host_id: "runtime_python_module_function".into(),
            profile: "server".into(),
            supported,
        }
    }

    /// Capabilities available from the loaded C ABI plugin and its worker delivery paths.
    /// Market-data level is omitted because this worker does not negotiate one for C ABI.
    pub fn c_abi(
        order_status_updates: bool,
        fill_updates: bool,
        account_updates: bool,
        durable_checkpoint: bool,
        scoped_account_updates: bool,
    ) -> Self {
        use StrategyCapability::*;
        let mut supported = Vec::new();
        if order_status_updates {
            supported.push(OrderStatusUpdates);
        }
        if fill_updates {
            supported.push(FillUpdates);
        }
        if account_updates && scoped_account_updates {
            supported.extend([AccountUpdates, PositionUpdates]);
        }
        if durable_checkpoint {
            supported.push(DurableCheckpoint);
        }
        Self {
            schema_version: 1,
            host_id: "c_abi".into(),
            profile: "server".into(),
            supported,
        }
    }

    /// Built-in server strategies consume the configured BarFrame as a chronological
    /// sequence of Bar events. They do not receive a streaming timer or execution callback.
    pub fn builtin_rust() -> Self {
        use StrategyCapability::*;
        Self {
            schema_version: 1,
            host_id: "builtin_rust_strategy_runtime".into(),
            profile: "server".into(),
            supported: vec![Bar, BarHistory],
        }
    }

    pub fn local_builtin_rust() -> Self {
        let mut capabilities = Self::builtin_rust();
        capabilities.profile = "local".into();
        capabilities
    }

    /// Local dual-leg built-in backtests deliver independent BarFrames for both legs.
    pub fn local_multi_builtin_rust() -> Self {
        use StrategyCapability::*;
        Self {
            schema_version: 1,
            host_id: "local_multi_builtin_rust".into(),
            profile: "local".into(),
            supported: vec![Bar, BarHistory, MultiInstrument],
        }
    }

    /// Persistent external-process contract delivered by the server Strategy worker.
    /// The versioned input can carry every market-data shape and the worker requires
    /// checkpoint responses; account updates still require explicit account + venue scope.
    pub fn runtime_external_process(scoped_account_updates: bool) -> Self {
        use StrategyCapability::*;
        let mut supported = vec![
            Bar,
            BarHistory,
            Tick,
            Orderbook,
            FillUpdates,
            OrderStatusUpdates,
            DurableCheckpoint,
        ];
        if scoped_account_updates {
            supported.extend([AccountUpdates, PositionUpdates]);
        }
        Self {
            schema_version: 1,
            host_id: "runtime_external_process".into(),
            profile: "server".into(),
            supported,
        }
    }

    /// File/backtest external-process calls deliver the current BarFrame and its visible history.
    pub fn local_external_process() -> Self {
        use StrategyCapability::*;
        Self {
            schema_version: 1,
            host_id: "local_external_process".into(),
            profile: "local".into(),
            supported: vec![Bar, BarHistory],
        }
    }

    /// Delivery capabilities of the Python file callback paths through the shared local engines.
    pub fn local_python(data_level: Option<&str>) -> Self {
        use StrategyCapability::*;
        let (host_id, supported) = match data_level {
            Some("bar") => (
                "local_python_bar",
                vec![
                    Bar,
                    BarHistory,
                    MultiInstrument,
                    FillUpdates,
                    OrderStatusUpdates,
                ],
            ),
            Some("tick") => (
                "local_python_tick",
                vec![Tick, MultiInstrument, FillUpdates],
            ),
            Some("orderbook") => (
                "local_python_orderbook",
                vec![Orderbook, MultiInstrument, FillUpdates],
            ),
            _ => ("local_python_unresolved", vec![]),
        };
        Self {
            schema_version: 1,
            host_id: host_id.into(),
            profile: "local".into(),
            supported,
        }
    }

    pub fn validate_required(&self, required: &[StrategyCapability]) -> Result<(), String> {
        if self.schema_version != 1
            || self.host_id.trim().is_empty()
            || self.profile.trim().is_empty()
        {
            return Err("invalid StrategyHostCapabilitiesV1 identity/schema".into());
        }
        let supported: BTreeSet<_> = self.supported.iter().copied().collect();
        let requirements: BTreeSet<_> = required.iter().copied().collect();
        if supported.len() != self.supported.len() || requirements.len() != required.len() {
            return Err("strategy capability lists must not contain duplicates".into());
        }
        let missing: Vec<_> = requirements.difference(&supported).copied().collect();
        if !missing.is_empty() {
            let missing_names = missing
                .iter()
                .map(|capability| capability.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "host {} does not support required capabilities: [{}]",
                self.host_id, missing_names
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_class_catalog_is_bound_to_data_level_and_account_scope() {
        use StrategyCapability::*;
        for (level, capability) in [
            ("bar", Bar),
            ("tick", Tick),
            ("orderbook", Orderbook),
            ("timer", Timer),
        ] {
            let host = StrategyHostCapabilitiesV1::runtime_python_class(level, true).unwrap();
            host.validate_required(&[
                capability,
                FillUpdates,
                OrderStatusUpdates,
                DurableCheckpoint,
                AccountUpdates,
                PositionUpdates,
            ])
            .unwrap();
            if level != "bar" {
                assert!(host.validate_required(&[BarHistory]).is_err());
            }
            assert!(
                StrategyHostCapabilitiesV1::runtime_python_class(level, false)
                    .unwrap()
                    .validate_required(&[AccountUpdates])
                    .is_err()
            );
        }
        assert!(StrategyHostCapabilitiesV1::runtime_python_class("magic", true).is_err());
    }
    #[test]
    fn local_hosts_reject_unimplemented_delivery_and_state_contracts() {
        use StrategyCapability::*;
        for level in ["bar", "tick", "orderbook"] {
            let host = StrategyHostCapabilitiesV1::local_python(Some(level));
            host.validate_required(&host.supported).unwrap();
            for unsupported in [AccountUpdates, PositionUpdates, Timer, DurableCheckpoint] {
                assert!(host.validate_required(&[unsupported]).is_err());
            }
        }
        assert!(StrategyHostCapabilitiesV1::local_python(Some("tick"))
            .validate_required(&[BarHistory])
            .is_err());
    }

    #[test]
    fn wire_rejects_unknown_capabilities_and_duplicate_requirements() {
        assert!(serde_json::from_str::<Vec<StrategyCapability>>(r#"["future_magic"]"#).is_err());
        assert!(StrategyHostCapabilitiesV1::local_python(Some("bar"))
            .validate_required(&[StrategyCapability::Bar, StrategyCapability::Bar])
            .is_err());
    }

    #[test]
    fn strategy_host_profiles_accept_only_their_delivered_capabilities() {
        use StrategyCapability::*;

        let hosts = [
            (
                StrategyHostCapabilitiesV1::local_python(Some("bar")),
                &[Bar, BarHistory][..],
                &[Tick, Timer, DurableCheckpoint][..],
            ),
            (
                StrategyHostCapabilitiesV1::runtime_python_class("bar", false).unwrap(),
                &[
                    Bar,
                    BarHistory,
                    FillUpdates,
                    OrderStatusUpdates,
                    DurableCheckpoint,
                ][..],
                &[MultiInstrument, AccountUpdates, PositionUpdates, Timer][..],
            ),
            (
                StrategyHostCapabilitiesV1::runtime_python_module_function(false),
                &[
                    Bar,
                    BarHistory,
                    Tick,
                    Orderbook,
                    FillUpdates,
                    OrderStatusUpdates,
                    DurableCheckpoint,
                ][..],
                &[MultiInstrument, AccountUpdates, PositionUpdates, Timer][..],
            ),
            (
                StrategyHostCapabilitiesV1::local_builtin_rust(),
                &[Bar, BarHistory][..],
                &[MultiInstrument, Timer, DurableCheckpoint][..],
            ),
            (
                StrategyHostCapabilitiesV1::local_multi_builtin_rust(),
                &[Bar, BarHistory, MultiInstrument][..],
                &[Tick, Timer, DurableCheckpoint][..],
            ),
            (
                StrategyHostCapabilitiesV1::c_abi(true, true, true, true, false),
                &[OrderStatusUpdates, FillUpdates, DurableCheckpoint][..],
                &[AccountUpdates, PositionUpdates, Bar, Timer][..],
            ),
            (
                StrategyHostCapabilitiesV1::runtime_external_process(false),
                &[
                    Bar,
                    BarHistory,
                    Tick,
                    Orderbook,
                    FillUpdates,
                    OrderStatusUpdates,
                    DurableCheckpoint,
                ][..],
                &[MultiInstrument, AccountUpdates, PositionUpdates, Timer][..],
            ),
            (
                StrategyHostCapabilitiesV1::local_external_process(),
                &[Bar, BarHistory][..],
                &[Tick, MultiInstrument, Timer, DurableCheckpoint][..],
            ),
        ];

        for (host, supported, unsupported) in &hosts {
            for capability in supported.iter() {
                host.validate_required(&[*capability])
                    .unwrap_or_else(|error| {
                        panic!("{} rejected {}: {error}", host.host_id, capability.as_str())
                    });
            }
            for capability in unsupported.iter() {
                assert!(
                    host.validate_required(&[*capability]).is_err(),
                    "{} unexpectedly accepted {}",
                    host.host_id,
                    capability.as_str()
                );
            }
        }

        let legacy_c_abi = StrategyHostCapabilitiesV1::c_abi(false, false, false, false, false);
        legacy_c_abi.validate_required(&[]).unwrap();
        assert!(legacy_c_abi.validate_required(&[FillUpdates]).is_err());

        let declaration = StrategyRequiredCapabilitiesV1 {
            schema_version: 1,
            required_capabilities: vec![FillUpdates],
        };
        declaration
            .validate_required_capabilities_against_host(&hosts[5].0)
            .unwrap();
        assert!(declaration
            .validate_required_capabilities_against_host(&legacy_c_abi)
            .is_err());
    }
}
