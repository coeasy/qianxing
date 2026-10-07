//! 子进程管道上的**带预算**写入（V11 N8）。
//!
//! `Write::write_all` 打在管道上是可能永远不返回的：对端不读、缓冲写满，调用线程
//! 就卡在原地。三处生产调用点（CCXT Worker、策略 worker、事件 consumer handler）的
//! "响应超时"判定都排在写之后——写一卡，那圈预算根本没机会开始，配置里的
//! `timeout_ms` 于是只保护了读、没保护写。这里把写交给一颗线程，用带截止时间的
//! channel 收它：预算内没收到结果就调用 `on_timeout`（生产里是杀掉子进程——管道
//! 读端随之关闭，卡住的写线程自己以 `BrokenPipe` 收尾，不留一颗僵尸线程）。

use std::io::Write;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// 在 `timeout` 预算内写完 `payload` 并 flush。
///
/// 成功时把 writer 原样还回来，好让同一条子进程 stdin 继续复用；失败（写报错、
/// 超出预算、写线程没交回结果）即视为这条管道不再归调用方掌控，返回 `Err` 且不
/// 带回 writer——把手里的句柄置空即可，下一轮会走同一条"stdin 不可用"的错误出口。
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
            // 收信方可能已因超时先离开：那时 writer 随闭包一起丢弃，不做二次处理。
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    /// 会一直收的管道：写侧不该被预算绊住。
    #[derive(Debug)]
    struct Drains(Vec<u8>);

    impl Write for Drains {
        fn write(&mut self, payload: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(payload);
            Ok(payload.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// 立刻断掉的管道：子进程已经退出时 `write` 就是这个形状。
    #[derive(Debug)]
    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// 对端永远不读的管道：`write` 停在原地，直到测试放行才以 `BrokenPipe` 收场。
    #[derive(Debug)]
    struct NeverDrains {
        release: Arc<AtomicBool>,
        written: Arc<AtomicUsize>,
    }

    impl Write for NeverDrains {
        fn write(&mut self, payload: &[u8]) -> io::Result<usize> {
            self.written.fetch_add(payload.len(), Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(2));
            }
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// 正向对照：调用方给的预算必须作数，而对端在读时不该误伤。只测"超时会报"的话，
    /// 把预算改成 0ms 也能让那条断言绿——这里要的是"预算管得住写"而不是"写永远失败"。
    /// 1ms 这种量级会把"线程还没起来"念成"管道不读"，所以留够调度余量、只小过生产值。
    #[test]
    fn a_draining_pipe_writes_through_and_gives_the_writer_back() {
        let drained = write_all_within(
            Drains(Vec::new()),
            b"payload".to_vec(),
            Duration::from_millis(500),
            || panic!("对端在读时不该走超时分支"),
        )
        .expect("对端在读就不该判超时");
        assert_eq!(drained.0, b"payload");
    }

    /// 反向：对端不读时预算必须真的兜住写，并且把"打断管道"那一步交给调用方。
    #[test]
    fn a_pipe_that_never_drains_stops_at_the_budget_and_kills() {
        let release = Arc::new(AtomicBool::new(false));
        let written = Arc::new(AtomicUsize::new(0));
        let interrupted = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&interrupted);
        let started = Instant::now();
        let error = write_all_within(
            NeverDrains {
                release: Arc::clone(&release),
                written: Arc::clone(&written),
            },
            vec![b'x'; 4096],
            Duration::from_millis(50),
            move || {
                flag.store(true, Ordering::SeqCst);
            },
        )
        .expect_err("对端不读时不该报成功");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "预算没兜住写: {elapsed:?}"
        );
        assert!(error.contains("未在"), "{error}");
        assert!(written.load(Ordering::SeqCst) > 0, "确实卡在了写入里");
        assert!(
            interrupted.load(Ordering::SeqCst),
            "超时必须让调用方打断管道"
        );
        release.store(true, Ordering::SeqCst);
    }

    /// 普通写错误（典型是子进程先退了）走错误出口，但不算超时：不该顺手杀进程。
    #[test]
    fn a_broken_pipe_reports_the_io_error_without_the_timeout_path() {
        let interrupted = AtomicBool::new(false);
        let error = write_all_within(Broken, b"x".to_vec(), Duration::from_secs(5), || {
            interrupted.store(true, Ordering::SeqCst)
        })
        .expect_err("管道已断时不该报成功");
        assert!(error.contains("写入失败"), "{error}");
        assert!(
            !interrupted.load(Ordering::SeqCst),
            "普通写错误不该走打断管道那条分支"
        );
    }
}
