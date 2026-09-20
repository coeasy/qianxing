//! 服务健康状态注册表与确定性快照。

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Starting,
    Ready,
    Running,
    Degraded,
    Failed,
    Stopping,
    Stopped,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ServiceHealth {
    pub id: String,
    pub role: WorkerRole,
    pub status: ServiceStatus,
    pub last_heartbeat_ms: Option<u64>,
    pub detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverallHealth {
    Starting,
    Ready,
    Degraded,
    Failed,
    Stopped,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub overall: OverallHealth,
    pub services: Vec<ServiceHealth>,
}

#[derive(Clone, Default)]
pub struct HealthRegistry {
    pub(crate) services: BTreeMap<String, ServiceHealth>,
}

impl HealthRegistry {
    pub fn register(&mut self, id: impl Into<String>, role: WorkerRole) -> Result<(), String> {
        let id = id.into();
        if id.trim().is_empty() || self.services.contains_key(&id) {
            return Err(format!("服务 id 为空或重复: {id}"));
        }
        self.services.insert(
            id.clone(),
            ServiceHealth {
                id,
                role,
                status: ServiceStatus::Starting,
                last_heartbeat_ms: None,
                detail: "registered".into(),
            },
        );
        Ok(())
    }

    pub fn mark(
        &mut self,
        id: &str,
        status: ServiceStatus,
        detail: impl Into<String>,
        now_ms: Option<u64>,
    ) -> Result<(), String> {
        let service = self
            .services
            .get_mut(id)
            .ok_or_else(|| format!("未知服务: {id}"))?;
        service.status = status;
        service.detail = detail.into();
        if now_ms.is_some() {
            service.last_heartbeat_ms = now_ms;
        }
        Ok(())
    }

    pub fn heartbeat(&mut self, id: &str, now_ms: u64) -> Result<(), String> {
        let service = self
            .services
            .get_mut(id)
            .ok_or_else(|| format!("未知服务: {id}"))?;
        service.last_heartbeat_ms = Some(now_ms);
        if service.status == ServiceStatus::Starting {
            service.status = ServiceStatus::Ready;
        }
        Ok(())
    }

    pub fn snapshot(&self, now_ms: u64, stale_after_ms: u64) -> HealthSnapshot {
        let mut services: Vec<_> = self.services.values().cloned().collect();
        services.sort_by(|left, right| left.id.cmp(&right.id));
        let awaiting_first_heartbeat = services.iter().any(|service| {
            matches!(
                service.status,
                ServiceStatus::Ready | ServiceStatus::Running
            ) && service.last_heartbeat_ms.is_none()
        });
        let stale = services.iter().any(|service| {
            matches!(
                service.status,
                ServiceStatus::Ready | ServiceStatus::Running
            ) && service
                .last_heartbeat_ms
                .map(|heartbeat| now_ms.saturating_sub(heartbeat) > stale_after_ms)
                .unwrap_or(false)
        });
        let overall = if services.is_empty() {
            OverallHealth::Stopped
        } else if services
            .iter()
            .any(|service| service.status == ServiceStatus::Failed)
        {
            OverallHealth::Failed
        } else if services
            .iter()
            .all(|service| service.status == ServiceStatus::Stopped)
        {
            OverallHealth::Stopped
        } else if stale
            || services.iter().any(|service| {
                matches!(
                    service.status,
                    ServiceStatus::Degraded | ServiceStatus::Stopping
                )
            })
        {
            OverallHealth::Degraded
        } else if awaiting_first_heartbeat
            || services
                .iter()
                .any(|service| service.status == ServiceStatus::Starting)
        {
            OverallHealth::Starting
        } else {
            OverallHealth::Ready
        };
        HealthSnapshot { overall, services }
    }
}
