use super::{
    DataProvider, DataQuery, DataRequest, DataRequirements, ProviderError, ProviderErrorClass,
    ProviderResult,
};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub enum RegistryError {
    Duplicate(String),
    NoProvider,
    Provider(ProviderError),
}

pub struct ProviderRegistry {
    providers: BTreeMap<String, RegisteredProvider>,
}

struct RegisteredProvider {
    provider: Box<dyn DataProvider>,
    rate_gate: RateGate,
}

/// A provider result together with its validated canonical BarFrame projection.
/// Keeping both values prevents callers from rebuilding the lineage or parsing
/// the same provider payload a second time before freezing the dataset.
pub struct ProviderBarFrame {
    pub query: DataQuery,
    pub result: ProviderResult,
    pub frame: qx_datastruct::BarFrame,
}

impl ProviderBarFrame {
    /// Build the shared v2 dataset identity from this validated frame and its actual source.
    pub fn dataset_manifest(
        &self,
        dataset_id: &str,
        dataset_version: &str,
        interval_ms: u64,
        timezone: &str,
    ) -> Result<qx_data::DatasetManifestV2, String> {
        let instrument = self.frame.instrument.to_string();
        if self.query.kind != super::DataKind::Bar
            || self.query.instrument_set.len() != 1
            || self.query.instrument_set.iter().next() != Some(&instrument)
        {
            return Err("dataset manifest requires one matching Bar instrument".into());
        }
        qx_data::manifest_from_bar_frame(
            &self.frame,
            dataset_id,
            dataset_version,
            interval_ms,
            timezone,
            &self.result.provider_version,
            &self.result.provider_id,
        )
    }
}

/// Spaces calls to one provider port according to its advertised requests-per-second limit.
/// This gate lives at the external-I/O boundary; it does not affect event ordering or replay.
struct RateGate {
    interval: Duration,
    next_allowed: Mutex<Option<Instant>>,
}

impl RateGate {
    fn new(requests_per_second: u32) -> Self {
        let rate = u64::from(requests_per_second);
        let nanos_per_second = 1_000_000_000_u64;
        let interval_nanos = nanos_per_second.div_ceil(rate).max(1);
        Self {
            interval: Duration::from_nanos(interval_nanos),
            next_allowed: Mutex::new(None),
        }
    }

    fn reserve_turn(&self, now: Instant) -> Result<Duration, super::ProviderError> {
        let mut next_allowed = self.next_allowed.lock().map_err(|_| {
            super::ProviderError::new(
                super::ProviderErrorClass::Permanent,
                "Provider rate gate lock poisoned",
            )
        })?;
        let (delay, next) = reserve_at(now, *next_allowed, self.interval);
        *next_allowed = Some(next);
        Ok(delay)
    }

    fn wait_turn(&self) -> Result<(), super::ProviderError> {
        let delay = self.reserve_turn(Instant::now())?;
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        Ok(())
    }
}

fn reserve_at(
    now: Instant,
    next_allowed: Option<Instant>,
    interval: Duration,
) -> (Duration, Instant) {
    let start = next_allowed.filter(|next| *next > now).unwrap_or(now);
    (start.saturating_duration_since(now), start + interval)
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, provider: Box<dyn DataProvider>) -> Result<(), RegistryError> {
        let id = provider.capability().provider_id.clone();
        provider
            .capability()
            .validate()
            .map_err(RegistryError::Provider)?;
        if id.trim().is_empty() {
            return Err(RegistryError::NoProvider);
        }
        if self.providers.contains_key(&id) {
            return Err(RegistryError::Duplicate(id));
        }
        let rate_gate = RateGate::new(provider.capability().rate_limit_per_second);
        self.providers.insert(
            id,
            RegisteredProvider {
                provider,
                rate_gate,
            },
        );
        Ok(())
    }

    pub fn candidates(&self, query: &DataQuery) -> Result<Vec<String>, RegistryError> {
        query.validate().map_err(RegistryError::Provider)?;
        self.candidates_for_request(&DataRequest::new(
            query.clone(),
            DataRequirements::default(),
        ))
    }

    pub fn candidates_for_request(
        &self,
        request: &DataRequest,
    ) -> Result<Vec<String>, RegistryError> {
        request
            .query
            .validate()
            .and_then(|()| request.requirements.validate_for(&request.query))
            .map_err(RegistryError::Provider)?;
        let mut candidates: Vec<_> = self
            .providers
            .values()
            .filter(|registered| registered.provider.capability().matches_request(request))
            .map(|registered| registered.provider.capability().clone())
            .collect();
        candidates.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| b.quality_score.cmp(&a.quality_score))
                .then_with(|| a.cost_score.cmp(&b.cost_score))
                .then_with(|| a.provider_id.cmp(&b.provider_id))
        });
        Ok(candidates.into_iter().map(|c| c.provider_id).collect())
    }

    pub fn fetch_with_failover(&self, query: &DataQuery) -> Result<ProviderResult, RegistryError> {
        self.fetch_request(&DataRequest::new(
            query.clone(),
            DataRequirements::default(),
        ))
    }

    pub fn fetch_request(&self, request: &DataRequest) -> Result<ProviderResult, RegistryError> {
        let candidates = self.candidates_for_request(request)?;
        if candidates.is_empty() {
            return Err(RegistryError::NoProvider);
        }
        let mut retry_chain = Vec::new();
        let mut last_error = None;
        for id in candidates {
            retry_chain.push(id.clone());
            let registered = self.providers.get(&id).expect("candidate is registered");
            if let Err(error) = registered.rate_gate.wait_turn() {
                last_error = Some(error);
                continue;
            }
            match registered.provider.fetch_request(request) {
                Ok(mut result) => {
                    let capability = registered.provider.capability();
                    if result.provider_id != capability.provider_id
                        || result.provider_version != capability.version
                    {
                        // 结果的自述身份必须与这次实际调用的端口一致；否则仅校验记录
                        // 内部一致性会允许 Provider 冒充另一个已注册来源。
                        last_error = Some(ProviderError::new(
                            ProviderErrorClass::SwitchProvider,
                            format!(
                                "Provider {}@{} 返回了不匹配的身份 {}@{}",
                                capability.provider_id,
                                capability.version,
                                result.provider_id,
                                result.provider_version
                            ),
                        ));
                        continue;
                    }
                    match result.validate_for_request(request, capability) {
                        Ok(()) => {
                            // Validate the provider's own digest first, then recompute the
                            // result fingerprint with the registry's deterministic retry chain.
                            result.retry_chain = retry_chain.clone();
                            result.source_hash = result.compute_source_hash();
                            return Ok(result);
                        }
                        Err(error) => {
                            // 非法数据不能被当成成功；按既定 failover 顺序继续尝试。
                            last_error = Some(error);
                        }
                    }
                }
                Err(error) => {
                    let terminal = matches!(
                        error.class,
                        ProviderErrorClass::ManualIntervention | ProviderErrorClass::Permanent
                    );
                    last_error = Some(error.clone());
                    if terminal {
                        return Err(RegistryError::Provider(error));
                    }
                }
            }
        }
        Err(last_error
            .map(RegistryError::Provider)
            .unwrap_or(RegistryError::NoProvider))
    }

    /// Fetch and validate a single-instrument Bar request through this registry,
    /// preserving the selected provider result alongside the canonical frame.
    pub fn fetch_bar_frame(
        &self,
        request: &DataRequest,
    ) -> Result<ProviderBarFrame, RegistryError> {
        let result = self.fetch_request(request)?;
        let frame = result
            .to_bar_frame(&request.query)
            .map_err(RegistryError::Provider)?;
        Ok(ProviderBarFrame {
            query: request.query.clone(),
            result,
            frame,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{reserve_at, RateGate};
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    #[test]
    fn provider_rate_gate_reserves_non_overlapping_slots() {
        let now = Instant::now();
        let interval = Duration::from_millis(100);
        let (first_delay, first_next) = reserve_at(now, None, interval);
        let (second_delay, second_next) = reserve_at(now, Some(first_next), interval);
        let (third_delay, third_next) = reserve_at(first_next, Some(second_next), interval);

        assert_eq!(first_delay, Duration::ZERO);
        assert_eq!(second_delay, interval);
        assert_eq!(third_delay, interval);
        assert_eq!(third_next.duration_since(now), interval * 3);
    }

    #[test]
    fn provider_rate_gate_does_not_wait_after_slot_has_elapsed() {
        let now = Instant::now();
        let previous = now - Duration::from_millis(1);
        let (delay, next) = reserve_at(now, Some(previous), Duration::from_secs(1));

        assert_eq!(delay, Duration::ZERO);
        assert_eq!(next, now + Duration::from_secs(1));
    }

    #[test]
    fn provider_rate_gate_rounds_up_subsecond_intervals() {
        assert_eq!(RateGate::new(1).interval, Duration::from_secs(1));
        assert_eq!(RateGate::new(3).interval, Duration::from_nanos(333_333_334));
        assert_eq!(RateGate::new(u32::MAX).interval, Duration::from_nanos(1));
    }

    #[test]
    fn concurrent_provider_calls_reserve_distinct_rate_slots() {
        let gate = Arc::new(RateGate::new(10));
        let start = Instant::now();
        let barrier = Arc::new(Barrier::new(5));
        let threads = (0..4)
            .map(|_| {
                let gate = Arc::clone(&gate);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    gate.reserve_turn(start).unwrap()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();

        let mut delays = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        delays.sort();

        assert_eq!(
            delays,
            [
                Duration::ZERO,
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(300),
            ]
        );
    }
}
