use super::{DataProvider, DataQuery, ProviderError, ProviderErrorClass, ProviderResult};
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq)]
pub enum RegistryError {
    Duplicate(String),
    NoProvider,
    Provider(ProviderError),
}

pub struct ProviderRegistry {
    providers: BTreeMap<String, Box<dyn DataProvider>>,
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
        self.providers.insert(id, provider);
        Ok(())
    }

    pub fn candidates(&self, query: &DataQuery) -> Result<Vec<String>, RegistryError> {
        query.validate().map_err(RegistryError::Provider)?;
        let mut candidates: Vec<_> = self
            .providers
            .values()
            .filter(|provider| provider.capability().matches(query))
            .map(|provider| provider.capability().clone())
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
        let candidates = self.candidates(query)?;
        if candidates.is_empty() {
            return Err(RegistryError::NoProvider);
        }
        let mut retry_chain = Vec::new();
        let mut last_error = None;
        for id in candidates {
            retry_chain.push(id.clone());
            let provider = self.providers.get(&id).expect("candidate is registered");
            match provider.fetch(query) {
                Ok(mut result) => {
                    let capability = provider.capability();
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
                    match result.validate_for(query) {
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
}
