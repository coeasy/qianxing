//! Bounded I/O primitives shared by subprocess owners and transport adapters.

use std::io::Write;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Outcome of a bounded pipe write that can also be cancelled by its owner.
pub enum CancellableWrite<W> {
    Written(W),
    Cancelled,
}

/// Write and flush a subprocess pipe within `timeout`, returning the reusable writer on success.
/// The caller's timeout callback interrupts the child so a blocked pipe write can unwind.
pub fn write_all_within<W: Write + Send + 'static>(
    writer: W,
    payload: Vec<u8>,
    timeout: Duration,
    on_timeout: impl FnOnce(),
) -> Result<W, String> {
    match write_all_within_cancellable(writer, payload, timeout, || false, on_timeout)? {
        CancellableWrite::Written(writer) => Ok(writer),
        CancellableWrite::Cancelled => Err("写入已取消".into()),
    }
}

/// Write and flush a subprocess pipe within `timeout`, polling cancellation while the pipe is
/// blocked. `on_abort` must interrupt the peer so the writer thread can release its pipe.
pub fn write_all_within_cancellable<W: Write + Send + 'static>(
    writer: W,
    payload: Vec<u8>,
    timeout: Duration,
    mut should_cancel: impl FnMut() -> bool,
    on_abort: impl FnOnce(),
) -> Result<CancellableWrite<W>, String> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("qx-pipe-writer".into())
        .spawn(move || {
            let mut writer = writer;
            let result = writer.write_all(&payload).and_then(|_| writer.flush());
            let _ = sender.send((result, writer));
        })
        .map_err(|error| format!("启动管道写线程失败: {error}"))?;
    let started = Instant::now();
    let mut on_abort = Some(on_abort);
    loop {
        if should_cancel() {
            if let Some(abort) = on_abort.take() {
                abort();
            }
            return Ok(CancellableWrite::Cancelled);
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            if let Some(abort) = on_abort.take() {
                abort();
            }
            return Err(format!("未在 {}ms 预算内写完", timeout.as_millis()));
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(20))) {
            Ok((Ok(()), writer)) => return Ok(CancellableWrite::Written(writer)),
            Ok((Err(error), _)) => return Err(format!("写入失败: {error}")),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("写入线程退出且没有返回结果".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{write_all_within_cancellable, CancellableWrite};
    use std::io::{self, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    struct BlockedWriter {
        released: Arc<AtomicBool>,
    }

    impl Write for BlockedWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            while !self.released.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(2));
            }
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn blocked_pipe_write_returns_cancelled_and_interrupts_the_writer() {
        let released = Arc::new(AtomicBool::new(false));
        let abort_released = Arc::clone(&released);
        let checks = AtomicUsize::new(0);
        let outcome = write_all_within_cancellable(
            BlockedWriter {
                released: Arc::clone(&released),
            },
            b"payload".to_vec(),
            Duration::from_secs(5),
            || checks.fetch_add(1, Ordering::SeqCst) >= 2,
            move || abort_released.store(true, Ordering::SeqCst),
        )
        .expect("cancel is not a pipe error");
        assert!(matches!(outcome, CancellableWrite::Cancelled));
        assert!(released.load(Ordering::SeqCst));
    }
}
