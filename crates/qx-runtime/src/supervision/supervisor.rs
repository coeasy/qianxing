//! 停机令牌、worker 上下文与线程监督器。

use super::*;

#[derive(Clone, Default)]
pub struct ShutdownToken(Arc<AtomicBool>);

impl ShutdownToken {
    pub fn request(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub struct RuntimeSupervisor {
    config: RuntimeConfig,
    health: Arc<Mutex<HealthRegistry>>,
    shutdown: ShutdownToken,
}

#[derive(Clone)]
pub struct WorkerContext {
    id: String,
    shutdown: ShutdownToken,
    health: Arc<Mutex<HealthRegistry>>,
}

impl WorkerContext {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn should_stop(&self) -> bool {
        self.shutdown.is_requested()
    }

    pub fn heartbeat(&self, now_ms: u64) -> Result<(), String> {
        self.health
            .lock()
            .map_err(|_| "运行时健康锁已中毒".to_string())?
            .heartbeat(&self.id, now_ms)
    }

    pub fn mark(
        &self,
        status: ServiceStatus,
        detail: impl Into<String>,
        now_ms: Option<u64>,
    ) -> Result<(), String> {
        self.health
            .lock()
            .map_err(|_| "运行时健康锁已中毒".to_string())?
            .mark(&self.id, status, detail, now_ms)
    }
}

impl RuntimeSupervisor {
    pub fn new(config: RuntimeConfig) -> Result<Self, String> {
        config.validate()?;
        let mut registry = HealthRegistry::default();
        for worker in config.workers.iter().filter(|worker| worker.enabled) {
            registry.register(worker.id.clone(), worker.role)?;
        }
        Ok(Self {
            config,
            health: Arc::new(Mutex::new(registry)),
            shutdown: ShutdownToken::default(),
        })
    }

    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    pub fn health(&self) -> Arc<Mutex<HealthRegistry>> {
        Arc::clone(&self.health)
    }

    pub fn shutdown_token(&self) -> ShutdownToken {
        self.shutdown.clone()
    }

    pub fn request_shutdown(&self) {
        self.shutdown.request();
        if let Ok(mut health) = self.health.lock() {
            let ids = health.services.keys().cloned().collect::<Vec<_>>();
            for id in ids {
                if let Some(service) = health.services.get(&id) {
                    if matches!(
                        service.status,
                        ServiceStatus::Ready | ServiceStatus::Running
                    ) {
                        let _ =
                            health.mark(&id, ServiceStatus::Stopping, "shutdown requested", None);
                    }
                }
            }
        }
    }

    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown.is_requested()
    }

    /// 启动一个由调用方注入的 worker；worker 不允许直接改 Kernel 状态，必须通过
    /// 已有的 Adapter/Control/Storage 契约提交事实。线程异常被转换为 Failed 状态，
    /// 返回 `Err` 的正常退出也会留下可查询的失败原因。
    pub fn spawn_worker<F>(
        &self,
        id: &str,
        run: F,
    ) -> Result<JoinHandle<Result<(), String>>, String>
    where
        F: FnOnce(WorkerContext) -> Result<(), String> + Send + 'static,
    {
        {
            let health = self
                .health
                .lock()
                .map_err(|_| "运行时健康锁已中毒".to_string())?;
            if !health.services.contains_key(id) {
                return Err(format!("未知 worker: {id}"));
            }
        }
        let context = WorkerContext {
            id: id.into(),
            shutdown: self.shutdown.clone(),
            health: Arc::clone(&self.health),
        };
        let thread_id = id.to_string();
        Ok(std::thread::spawn(move || {
            let _ = context.mark(ServiceStatus::Ready, "running", None);
            let _ = context.mark(ServiceStatus::Running, "worker loop running", None);
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(context.clone())));
            match result {
                Ok(Ok(())) => {
                    let _ = context.mark(ServiceStatus::Stopping, "worker stopping", None);
                    let _ = context.mark(ServiceStatus::Stopped, "stopped", None);
                    Ok(())
                }
                Ok(Err(error)) => {
                    let _ = context.mark(ServiceStatus::Failed, error.clone(), None);
                    Err(format!("worker {thread_id} failed"))
                }
                Err(_) => {
                    let _ = context.mark(ServiceStatus::Failed, "worker panicked", None);
                    Err(format!("worker {thread_id} panicked"))
                }
            }
        }))
    }
}
