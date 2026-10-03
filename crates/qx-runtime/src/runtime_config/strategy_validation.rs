//! 策略实例配置的 fail-closed 校验。

use super::*;

impl RuntimeConfig {
    pub(super) fn validate_strategy_config(
        &self,
        strategy: &StrategyRuntimeConfig,
        label: &str,
        require_binding: bool,
    ) -> Result<(), String> {
        if strategy.version.trim().is_empty() || strategy.max_orders == 0 {
            return Err(format!("{label} version 不能为空且 max_orders 必须大于 0"));
        }
        let product = strategy.product.unwrap_or(TradingProduct::Spot);
        let allow_short = strategy.allow_short.unwrap_or(product.is_derivative());
        if strategy.target_qty < 0 && !allow_short {
            return Err(format!(
                "{label} target_qty 不能为负；当前产品/策略未开启 allow_short"
            ));
        }
        if strategy.live_timeframe.trim().is_empty() {
            return Err(format!("{label} live_timeframe 不能为空"));
        }
        if strategy.live_history_limit < 2 || strategy.live_history_limit > 100_000 {
            return Err(format!("{label} live_history_limit 必须在 2..=100000 内"));
        }
        if strategy
            .live_max_staleness_ms
            .is_some_and(|staleness| staleness == 0 || staleness > 7 * 86_400_000)
        {
            return Err(format!(
                "{label} live_max_staleness_ms 必须在 1..=604800000 内"
            ));
        }
        if strategy.live_enabled && strategy.bars_snapshot_path.is_none() {
            return Err(format!(
                "{label} live_enabled=true 时必须配置 bars_snapshot_path"
            ));
        }
        if strategy
            .target_snapshot_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} target_snapshot_path 不能为空字符串"));
        }
        if strategy
            .research_snapshot_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} research_snapshot_path 不能为空字符串"));
        }
        if strategy.research_snapshot_required && strategy.research_snapshot_path.is_none() {
            return Err(format!(
                "{label} research_snapshot_required=true 时必须配置 research_snapshot_path"
            ));
        }
        if strategy
            .research_data_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint.trim().is_empty())
        {
            return Err(format!("{label} research_data_fingerprint 不能为空字符串"));
        }
        if strategy
            .ashare_actions_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} ashare_actions_path 不能为空字符串"));
        }
        if strategy
            .ashare_calendar_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} ashare_calendar_path 不能为空字符串"));
        }
        if strategy.research_snapshot_required && strategy.research_data_fingerprint.is_none() {
            return Err(format!(
                "{label} research_snapshot_required=true 时必须配置 research_data_fingerprint"
            ));
        }
        if strategy
            .dataset_bundle_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} dataset_bundle_path 不能为空字符串"));
        }
        if strategy
            .dataset_component_paths
            .iter()
            .any(|(kind, path)| kind.trim().is_empty() || path.trim().is_empty() || kind == "bars")
        {
            return Err(format!(
                "{label} dataset_component_paths 的 kind/path 不能为空，且 bars 必须使用 bars_snapshot_path"
            ));
        }
        if strategy
            .python_module
            .as_deref()
            .is_some_and(|module| module.trim().is_empty())
        {
            return Err(format!("{label} python_module 不能为空字符串"));
        }
        if let Some(name) = strategy.builtin_strategy.as_deref() {
            if name.trim().is_empty() {
                return Err(format!("{label} builtin_strategy 不能为空字符串"));
            }
            qx_strategy::BuiltinStrategyKind::parse(name)
                .map_err(|error| format!("{label} builtin_strategy 非法: {error}"))?;
        }
        if strategy
            .bars_snapshot_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} bars_snapshot_path 不能为空字符串"));
        }
        if strategy.builtin_strategy.is_some() && strategy.bars_snapshot_path.is_none() {
            return Err(format!(
                "{label} builtin_strategy 运行时必须配置 bars_snapshot_path"
            ));
        }
        if strategy
            .builtin_quantity
            .is_some_and(|quantity| quantity <= 0)
        {
            return Err(format!("{label} builtin_quantity 必须为正整数"));
        }
        // 四个信号旋钮按 kind 清单体检（V12 #102）：一份运行时配置会喂给多种 kind
        // （`backtest builtin <kind>` 的 kind 来自命令行），而清单外的一项进不了那一轮的结果，
        // 就不该有拒那一轮的权力。清单与内核读法同源，见 `qx_strategy::BuiltinStrategyKind`。
        let builtin_kind = strategy
            .builtin_strategy
            .as_deref()
            .map(qx_strategy::BuiltinStrategyKind::parse)
            .transpose()
            .map_err(|error| format!("{label} builtin_strategy 非法: {error}"))?;
        let uses = |knob: qx_strategy::builtin_signal::BuiltinSignalKnob| {
            builtin_kind.is_some_and(|kind| kind.uses_signal_knob(knob))
        };
        if uses(qx_strategy::builtin_signal::BuiltinSignalKnob::FastWindow)
            || uses(qx_strategy::builtin_signal::BuiltinSignalKnob::SlowWindow)
        {
            if strategy
                .builtin_fast_window
                .is_some_and(|window| window == 0)
                || strategy
                    .builtin_slow_window
                    .is_some_and(|window| window == 0)
            {
                return Err(format!("{label} builtin fast/slow window 必须大于 0"));
            }
            if let (Some(fast), Some(slow)) =
                (strategy.builtin_fast_window, strategy.builtin_slow_window)
            {
                if fast >= slow {
                    return Err(format!(
                        "{label} builtin_fast_window 必须小于 builtin_slow_window"
                    ));
                }
            }
        }
        if uses(qx_strategy::builtin_signal::BuiltinSignalKnob::Period)
            && strategy.builtin_period.is_some_and(|period| period < 2)
        {
            return Err(format!("{label} builtin_period 必须大于等于 2"));
        }
        if uses(qx_strategy::builtin_signal::BuiltinSignalKnob::ThresholdBps)
            && strategy
                .builtin_threshold_bps
                .is_some_and(|threshold| threshold < 0)
        {
            return Err(format!("{label} builtin_threshold_bps 不能为负"));
        }
        let needs_reference = builtin_kind.is_some_and(|kind| kind.needs_reference_leg());
        if needs_reference
            && (strategy.builtin_reference_instrument.is_none()
                || strategy.builtin_reference_bars_snapshot_path.is_none())
        {
            return Err(format!(
                "{label} 双腿套利必须配置 builtin_reference_instrument 和 builtin_reference_bars_snapshot_path"
            ));
        }
        if let Some(reference) = strategy.builtin_reference_instrument.as_deref() {
            let reference = InstrumentId::parse(reference)
                .ok_or_else(|| format!("{label} builtin_reference_instrument 非法: {reference}"))?;
            if strategy
                .instrument
                .as_deref()
                .and_then(InstrumentId::parse)
                .is_some_and(|instrument| instrument == reference)
            {
                return Err(format!(
                    "{label} builtin_reference_instrument 不能与 instrument 相同"
                ));
            }
        }
        if strategy
            .builtin_reference_bars_snapshot_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!(
                "{label} builtin_reference_bars_snapshot_path 不能为空字符串"
            ));
        }
        if strategy
            .builtin_reference_leverage
            .is_some_and(|leverage| leverage == 0)
        {
            return Err(format!("{label} builtin_reference_leverage 必须大于 0"));
        }
        if strategy.python_timeout_ms == 0 || strategy.python_timeout_ms > 60_000 {
            return Err(format!("{label} python_timeout_ms 必须在 1..=60000 内"));
        }
        if matches!(
            strategy.transport,
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar
        ) {
            qx_strategy::SharedRingConfig {
                capacity: strategy.shared_memory_capacity,
                slot_bytes: strategy.shared_memory_slot_bytes,
            }
            .validate()
            .map_err(|error| format!("{label} shared memory ring 配置非法: {error}"))?;
        }
        if strategy
            .external_executable
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} external_executable 不能为空字符串"));
        }
        if strategy
            .c_abi_library
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(format!("{label} c_abi_library 不能为空字符串"));
        }
        if strategy.c_abi_library.is_some() && strategy.c_abi_sha256.is_none() {
            return Err(format!("{label} c_abi_library 必须同时配置 c_abi_sha256"));
        }
        if strategy.c_abi_sha256.is_some() && strategy.c_abi_library.is_none() {
            return Err(format!(
                "{label} c_abi_sha256 只能与 c_abi_library 一起配置"
            ));
        }
        if strategy.c_abi_library.is_some() && strategy.c_abi_max_library_bytes == 0 {
            return Err(format!("{label} c_abi_max_library_bytes 必须大于 0"));
        }
        if strategy
            .strategy_artifact_sha256
            .as_deref()
            .is_some_and(|digest| {
                digest.len() != 64 || !digest.chars().all(|value| value.is_ascii_hexdigit())
            })
        {
            return Err(format!(
                "{label} strategy_artifact_sha256 必须是64位十六进制摘要"
            ));
        }
        if strategy.strategy_artifact_sha256.is_some()
            && strategy.python_module.is_none()
            && strategy.external_executable.is_none()
        {
            return Err(format!(
                "{label} strategy_artifact_sha256 必须与 python_module 或 external_executable 一起配置"
            ));
        }
        if strategy.c_abi_ed25519_public_key.is_some() != strategy.c_abi_ed25519_signature.is_some()
        {
            return Err(format!("{label} C ABI Ed25519 公钥和签名必须成对配置"));
        }
        if self.environment.eq_ignore_ascii_case("production")
            && strategy.c_abi_library.is_some()
            && strategy.c_abi_ed25519_public_key.is_none()
        {
            return Err(format!(
                "{label} production C ABI 策略必须配置 Ed25519 公钥和签名"
            ));
        }
        if self.environment.eq_ignore_ascii_case("production")
            && (strategy.external_executable.is_some() || strategy.python_module.is_some())
            && strategy.strategy_artifact_sha256.is_none()
        {
            return Err(format!(
                "{label} production 外部策略必须配置 strategy_artifact_sha256"
            ));
        }
        let strategy_sources = [
            strategy.builtin_strategy.is_some(),
            strategy.python_module.is_some(),
            strategy.external_executable.is_some(),
            strategy.c_abi_library.is_some(),
        ]
        .into_iter()
        .filter(|configured| *configured)
        .count();
        if strategy_sources > 1 {
            return Err(format!(
                "{label} builtin_strategy、python_module、external_executable、c_abi_library 只能配置一个"
            ));
        }
        if strategy
            .external_args
            .iter()
            .any(|arg| arg.contains('\n') || arg.contains('\r'))
        {
            return Err(format!("{label} external_args 不能包含换行"));
        }
        if strategy.external_env.iter().any(|(key, value)| {
            key.trim().is_empty()
                || key.contains('=')
                || value.contains('\n')
                || value.contains('\r')
                || {
                    let upper = key.to_ascii_uppercase();
                    [
                        "SECRET",
                        "TOKEN",
                        "PASSWORD",
                        "API_KEY",
                        "PRIVATE_KEY",
                        "CREDENTIAL",
                    ]
                    .iter()
                    .any(|marker| upper.contains(marker))
                }
        }) {
            return Err(format!(
                "{label} external_env 含敏感凭证或非法键值；策略进程不得接收交易凭证"
            ));
        }
        if strategy.target_snapshot_path.is_some() && strategy.research_snapshot_path.is_some() {
            return Err(format!(
                "{label} 不能同时配置 target_snapshot_path 和 research_snapshot_path"
            ));
        }
        let leverage = strategy.leverage.unwrap_or(1);
        if leverage == 0 {
            return Err(format!("{label} leverage 必须大于 0"));
        }
        if product == TradingProduct::Spot
            && (leverage != 1
                || strategy
                    .margin_mode
                    .is_some_and(|mode| mode != MarginMode::Cash)
                || strategy.position_mode == Some(PositionMode::Hedge))
        {
            return Err(format!("{label} 现货只能使用 Cash/1x/OneWay"));
        }
        if strategy.allow_short == Some(true) && product == TradingProduct::Spot {
            return Err(format!("{label} 现货不能开启 allow_short"));
        }
        let strategy_binding_configured = strategy.account_id.is_some()
            || strategy.venue_id.is_some()
            || strategy.instrument.is_some();
        if require_binding && !strategy_binding_configured {
            return Err(format!("{label} 必须配置 account_id、venue_id、instrument"));
        }
        if !strategy_binding_configured {
            return Ok(());
        }
        if self.environment.eq_ignore_ascii_case("production")
            && (!strategy.research_snapshot_required || strategy.research_snapshot_path.is_none())
        {
            return Err(format!(
                "{label} production 已绑定交易对象，必须启用并配置 research_snapshot_path"
            ));
        }
        if self.environment.eq_ignore_ascii_case("production") && strategy.target_qty != 0 {
            return Err(format!(
                "{label} production 禁止使用裸 target_qty；必须通过 ResearchSnapshot/CandidateBinding 产生目标"
            ));
        }
        if self.environment.eq_ignore_ascii_case("production")
            && strategy.research_snapshot_required
            && strategy.dataset_bundle_path.is_none()
        {
            return Err(format!(
                "{label} production research_snapshot_required=true 时必须配置 dataset_bundle_path"
            ));
        }
        let account_id = strategy
            .account_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{label} account_id、venue_id、instrument 必须成组配置"))?;
        let venue_id = strategy
            .venue_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{label} account_id、venue_id、instrument 必须成组配置"))?;
        let instrument = strategy
            .instrument
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .and_then(InstrumentId::parse)
            .ok_or_else(|| format!("{label} instrument 不是合法 InstrumentId"))?;
        let has_matching_worker = self.workers.iter().any(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Strategy
                && worker.account_id.as_deref() == Some(account_id)
                && worker.venue_id.as_deref() == Some(venue_id)
                && (worker.symbols.is_empty()
                    || worker
                        .symbols
                        .iter()
                        .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(&instrument)))
        });
        if !has_matching_worker {
            return Err(format!(
                "{label} 绑定 {}@{} / {} 没有匹配的启用 strategy worker",
                account_id, venue_id, instrument
            ));
        }
        if strategy.live_enabled {
            let has_primary_market_data = self.workers.iter().any(|worker| {
                worker.enabled
                    && worker.role == WorkerRole::MarketData
                    && worker
                        .symbols
                        .iter()
                        .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(&instrument))
            });
            if !has_primary_market_data {
                return Err(format!(
                    "{label} live_enabled=true 但没有包含 {} 的启用 MarketData worker",
                    instrument
                ));
            }
            if let Some(reference_text) = strategy.builtin_reference_instrument.as_deref() {
                let reference = InstrumentId::parse(reference_text)
                    .ok_or_else(|| format!("{label} 对冲腿 instrument 非法: {reference_text}"))?;
                let has_reference_market_data = self.workers.iter().any(|worker| {
                    worker.enabled
                        && worker.role == WorkerRole::MarketData
                        && worker
                            .symbols
                            .iter()
                            .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(&reference))
                });
                if !has_reference_market_data {
                    return Err(format!(
                        "{label} live_enabled=true 但没有包含对冲腿 {} 的启用 MarketData worker",
                        reference
                    ));
                }
            }
            let has_matching_execution = self.workers.iter().any(|worker| {
                worker.enabled
                    && worker.role == WorkerRole::Execution
                    && worker.account_id.as_deref() == Some(account_id)
                    && worker
                        .venue_id
                        .as_deref()
                        .is_some_and(|configured| configured.eq_ignore_ascii_case(venue_id))
                    && (worker.symbols.is_empty()
                        || worker.symbols.iter().any(|symbol| {
                            InstrumentId::parse(symbol).as_ref() == Some(&instrument)
                        }))
            });
            if !has_matching_execution {
                return Err(format!(
                    "{label} live_enabled=true 但没有匹配 {}@{} / {} 的启用 Execution worker",
                    account_id, venue_id, instrument
                ));
            }
        }
        Ok(())
    }
}
