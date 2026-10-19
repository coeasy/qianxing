//! Bounded I/O primitives shared by subprocess owners and transport adapters.

use std::io::Write;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Write and flush a subprocess pipe within `timeout`, returning the reusable writer on success.
/// The caller's timeout callback interrupts the child so a blocked pipe write can unwind.
pub fn write_all_within<W: Write + Send + 'static>(
    writer: W,
    payload: Vec<u8>,
    timeout: Duration,
    on_timeout: impl FnOnce(),
) -> Result<W, String> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("qx-pipe-writer".into())
        .spawn(move || {
            let mut writer = writer;
            let result = writer.write_all(&payload).and_then(|_| writer.flush());
            let _ = sender.send((result, writer));
        })
        .map_err(|error| format!("启动管道写线程失败: {error}"))?;
    match receiver.recv_timeout(timeout) {
        Ok((Ok(()), writer)) => Ok(writer),
        Ok((Err(error), _)) => Err(format!("写入失败: {error}")),
        Err(_) => {
            on_timeout();
            Err(format!("未在 {}ms 预算内写完", timeout.as_millis()))
        }
    }
}
