use super::*;

impl AppRunEntry {
    pub(super) fn status(&mut self) -> RunStatus {
        self.observe()
    }

    pub(super) fn observe(&mut self) -> RunStatus {
        let Some(run) = &mut self.run else {
            return RunStatus::Running;
        };
        let observation = match run {
            AppRun::Bar(handle) => handle.observe(),
            AppRun::Depth(handle) => handle.observe(),
            AppRun::DepthExperiment(handle) => handle.observe(),
            AppRun::Experiment(handle) => handle.observe(),
            AppRun::MultiLeg(handle) => handle.observe(),
            AppRun::MultiLegExperiment(handle) => handle.observe(),
            AppRun::MultiMarketExperiment(handle) => handle.observe(),
            AppRun::MultiAccount(handle) => handle.observe(),
            AppRun::MultiAccountDepth(handle) => handle.observe(),
            AppRun::MultiAccountJoint(handle) => handle.observe(),
            AppRun::MultiMarket(handle) => handle.observe(),
            AppRun::MultiMarketDepth(handle) => handle.observe(),
            AppRun::MultiMarketDepthExperiment(handle) => handle.observe(),
        };
        if observation.events.len() > self.events.len() {
            self.events = observation.events;
        }
        observation.status
    }

    pub(super) fn cancel(&mut self) {
        if self.restored_status.is_some() {
            return;
        }
        self.cancel_requested = true;
        match &mut self.run {
            Some(AppRun::Bar(handle)) => handle.cancel(),
            Some(AppRun::Depth(handle)) => handle.cancel(),
            Some(AppRun::DepthExperiment(handle)) => handle.cancel(),
            Some(AppRun::Experiment(handle)) => handle.cancel(),
            Some(AppRun::MultiLeg(handle)) => handle.cancel(),
            Some(AppRun::MultiLegExperiment(handle)) => handle.cancel(),
            Some(AppRun::MultiMarketExperiment(handle)) => handle.cancel(),
            Some(AppRun::MultiAccount(handle)) => handle.cancel(),
            Some(AppRun::MultiAccountDepth(handle)) => handle.cancel(),
            Some(AppRun::MultiAccountJoint(handle)) => handle.cancel(),
            Some(AppRun::MultiMarket(handle)) => handle.cancel(),
            Some(AppRun::MultiMarketDepth(handle)) => handle.cancel(),
            Some(AppRun::MultiMarketDepthExperiment(handle)) => handle.cancel(),
            None => {}
        }
    }

    pub(super) fn collect_result(&mut self) -> RunStatus {
        let status = self.observe();
        if !is_terminal(status) || self.result.is_some() || self.error.is_some() {
            return status;
        }
        let outcome = match &mut self.run {
            None => return status,
            Some(AppRun::Bar(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::Depth(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::DepthExperiment(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::Experiment(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiLeg(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiLegExperiment(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiMarketExperiment(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiAccount(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiAccountDepth(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiAccountJoint(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiMarket(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiMarketDepth(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiMarketDepthExperiment(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
        };
        if let Some(outcome) = outcome {
            match outcome {
                Ok(document) => self.result = Some(document),
                Err(error) => self.error = Some(error.to_json()),
            }
        }
        status
    }

    pub(super) fn document(&mut self) -> String {
        if let Some(status) = &self.restored_status {
            return serde_json::json!({
                "run_id": self.run_id,
                "status": status,
                "result": self.result.as_ref().and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
                "error": self.error.as_ref().and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
            }).to_string();
        }
        let observed_status = self.collect_result();
        let status = match observed_status {
            RunStatus::Running => "running",
            RunStatus::Cancelling => "cancelling",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Failed => "failed",
        };
        serde_json::json!({
            "run_id": self.run_id,
            "status": if self.run.is_none() { "starting" } else { status },
            "result": self.result.as_ref().and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
            "error": self.error.as_ref().and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
        })
        .to_string()
    }

    pub(super) fn refresh_events(&mut self) {
        self.observe();
    }

    pub(super) fn events_document(&mut self, cursor: u64) -> Result<String, AppError> {
        self.refresh_events();
        events::document(&self.events, cursor)
    }
}

pub(crate) fn is_terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Succeeded | RunStatus::Cancelled | RunStatus::Failed
    )
}
pub(crate) fn status_route_id(route: &str) -> Option<&str> {
    let prefix = RUN_STATUS_ROUTE_TEMPLATE.strip_suffix("{run_id}")?;
    route
        .strip_prefix(prefix)
        .filter(|id| valid_run_route_id(id))
}

pub(crate) fn cancel_route_id(route: &str) -> Option<&str> {
    let (prefix, suffix) = RUN_CANCEL_ROUTE_TEMPLATE.split_once("{run_id}")?;
    route
        .strip_prefix(prefix)
        .and_then(|run_id| run_id.strip_suffix(suffix))
        .filter(|id| valid_run_route_id(id))
}

pub(crate) fn valid_run_route_id(run_id: &str) -> bool {
    !run_id.is_empty()
        && !run_id.contains('/')
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
