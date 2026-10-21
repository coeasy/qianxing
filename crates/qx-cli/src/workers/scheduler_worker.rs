use crate::{
    configured_job_queue, dispatch_scheduled_jobs, load_scheduler_state, read_runtime_config,
    runtime_timestamp_ms, scheduler_manifest, utc_schedule_tick,
};
use qx_runtime::WorkerRole;
use std::path::Path;

macro_rules! worker_log {
    ($quiet:expr, $($arg:tt)*) => {
        if !$quiet {
            println!($($arg)*);
        }
    };
}

pub(crate) fn run_scheduler_worker(
    path: &Path,
    worker_id: &str,
    once: bool,
    quiet: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let worker =
        qx_service::worker_host::configured_worker(&config, worker_id, WorkerRole::Scheduler)?
            .clone();
    let root = Path::new(&config.storage.data_dir).to_path_buf();
    let queue = configured_job_queue(&config, &root)?;
    let (state_store, scheduler, state_path) = load_scheduler_state(&config, &root, path)?;
    let interval_ms = config.scheduler.tick_interval_ms;
    let registered_id = worker.id.clone();
    qx_service::worker_host::run_worker(
        config,
        &registered_id,
        WorkerRole::Scheduler,
        "Scheduler",
        move |context| {
            let scheduler = scheduler;
            let mut last_minute = None;
            context.mark(
                qx_runtime::ServiceStatus::Ready,
                format!("scheduler jobs={}", scheduler.len()),
                Some(runtime_timestamp_ms()),
            )?;
            qx_service::worker_host::run_polling_worker(&context, once, || {
                let now = runtime_timestamp_ms();
                let (trading_day, tick) = utc_schedule_tick(now);
                let minute_key = now / 60_000;
                if last_minute != Some(minute_key) {
                    let manifest = scheduler_manifest(context.id(), &trading_day, now);
                    let dispatch = dispatch_scheduled_jobs(
                        &state_store,
                        &state_path,
                        &queue,
                        &tick,
                        &trading_day,
                        &manifest,
                        now,
                    )?;
                    if dispatch.queued > 0 || dispatch.timed_out > 0 || dispatch.skipped > 0 {
                        worker_log!(
                            quiet,
                            "[调度 · Scheduler] worker={} day={} queued={} timed_out={} skipped={}",
                            context.id(),
                            trading_day,
                            dispatch.queued,
                            dispatch.timed_out,
                            dispatch.skipped
                        );
                    }
                    last_minute = Some(minute_key);
                }
                context.heartbeat(now)?;
                if once {
                    worker_log!(
                        quiet,
                        "[调度 · Scheduler] worker={} READY jobs={}",
                        context.id(),
                        scheduler.len()
                    );
                }
                Ok(interval_ms)
            })?;
            Ok(())
        },
    )
}
