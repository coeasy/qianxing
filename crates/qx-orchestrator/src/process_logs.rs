//! Bounded stdout/stderr sinks for supervised workers.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};

pub(crate) const MAX_PROCESS_LOG_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FAILURE_LOG_BYTES: usize = 64 * 1024;
const FAILURE_LOG_HEADER: &[u8] =
    b"failed strategy replacement; retained process output tail follows\n";

pub(crate) fn prepare(path: &Path) -> io::Result<File> {
    let rotated = rotated_path(path);
    match fs::remove_file(rotated) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    File::create(path)
}

/// Preserve the most recent bounded tail before another worker generation truncates `source`.
/// Empty and missing logs are omitted; each destination is a single replaceable snapshot per worker.
pub(crate) fn preserve_failure_tail(source: &Path, destination: &Path) -> io::Result<bool> {
    let mut input = match File::open(source) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            remove_failure_snapshot(destination)?;
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let length = input.metadata()?.len();
    if length == 0 {
        remove_failure_snapshot(destination)?;
        return Ok(false);
    }

    let payload_limit = MAX_FAILURE_LOG_BYTES.saturating_sub(FAILURE_LOG_HEADER.len());
    let retained = length.min(payload_limit as u64);
    input.seek(SeekFrom::End(-(retained as i64)))?;
    let mut tail = Vec::with_capacity(retained as usize);
    input.take(retained).read_to_end(&mut tail)?;

    let mut output = File::create(destination)?;
    output.write_all(FAILURE_LOG_HEADER)?;
    output.write_all(&tail)?;
    output.flush()?;
    Ok(true)
}

fn remove_failure_snapshot(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn pump<R>(
    mut reader: R,
    path: PathBuf,
    file: File,
    worker_id: String,
    stream: &'static str,
) -> JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        if let Err(error) = copy_rotating(&mut reader, path, file, MAX_PROCESS_LOG_BYTES) {
            eprintln!(
                "[监督器 · 日志] worker={} stream={} 后续输出已丢弃: {error}",
                worker_id, stream
            );
            // If the disk sink fails, continue draining the pipe so the worker itself does not
            // fail on a full pipe. The copy helper reports sink failures before returning.
            let mut discard = io::sink();
            let _ = io::copy(&mut reader, &mut discard);
        }
    })
}

fn copy_rotating<R: Read>(
    reader: &mut R,
    path: PathBuf,
    mut file: File,
    max_bytes: u64,
) -> io::Result<()> {
    assert!(max_bytes > 0);
    let mut written = file.metadata()?.len();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        let mut remaining = &buffer[..count];
        while !remaining.is_empty() {
            if written == max_bytes {
                file.flush()?;
                drop(file);
                let rotated = rotated_path(&path);
                match fs::remove_file(&rotated) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                fs::rename(&path, &rotated)?;
                file = File::create(&path)?;
                written = 0;
            }
            let length = remaining.len().min((max_bytes - written) as usize);
            file.write_all(&remaining[..length])?;
            written += length as u64;
            remaining = &remaining[length..];
        }
    }
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut rotated = path.as_os_str().to_os_string();
    rotated.push(".1");
    PathBuf::from(rotated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_path(label: &str) -> PathBuf {
        let target = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("qx-orchestrator has a repository root")
            .join("target");
        fs::create_dir_all(&target).expect("create workspace test output directory");
        target.join(format!(
            "process-log-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after the Unix epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn output_rotation_keeps_only_the_latest_two_bounded_chunks() {
        let path = test_path("rotation");
        let file = File::create(&path).unwrap();
        copy_rotating(&mut Cursor::new(b"abcdefghijkl"), path.clone(), file, 4).unwrap();

        let rotated = rotated_path(&path);
        assert_eq!(fs::read(&rotated).unwrap(), b"efgh");
        assert_eq!(fs::read(&path).unwrap(), b"ijkl");
        assert!(fs::metadata(&path).unwrap().len() <= 4);
        assert!(fs::metadata(&rotated).unwrap().len() <= 4);
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(rotated);
    }

    #[test]
    fn pipe_pump_rotates_at_the_worker_log_limit() {
        let path = test_path("pump");
        let file = File::create(&path).unwrap();
        let payload = vec![b'x'; MAX_PROCESS_LOG_BYTES as usize + 1];
        pump(
            Cursor::new(payload),
            path.clone(),
            file,
            "pump-test".into(),
            "stdout",
        )
        .join()
        .expect("the log pump thread should finish");

        let rotated = rotated_path(&path);
        assert_eq!(fs::metadata(&rotated).unwrap().len(), MAX_PROCESS_LOG_BYTES);
        assert_eq!(fs::read(&path).unwrap(), b"x");
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(rotated);
    }

    #[test]
    fn preparing_a_worker_log_discards_the_previous_rotation() {
        let path = test_path("prepare");
        fs::write(rotated_path(&path), b"old").unwrap();
        fs::write(&path, b"current").unwrap();
        drop(prepare(&path).unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"");
        assert!(!rotated_path(&path).exists());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn failed_worker_tail_is_bounded_and_keeps_the_latest_bytes() {
        let source = test_path("failed-source");
        let destination = test_path("failed-destination");
        let mut payload = vec![b'x'; MAX_FAILURE_LOG_BYTES * 2];
        payload.extend_from_slice(b"failure-tail-marker");
        fs::write(&source, payload).unwrap();

        assert!(preserve_failure_tail(&source, &destination).unwrap());

        let preserved = fs::read(&destination).unwrap();
        assert!(preserved.len() <= MAX_FAILURE_LOG_BYTES);
        assert!(preserved.starts_with(FAILURE_LOG_HEADER));
        assert!(preserved.ends_with(b"failure-tail-marker"));
        let _ = fs::remove_file(source);
        let _ = fs::remove_file(destination);
    }

    #[test]
    fn missing_or_empty_worker_logs_do_not_create_failure_snapshots() {
        let source = test_path("empty-source");
        let destination = test_path("empty-destination");
        fs::write(&destination, b"stale failure").unwrap();
        assert!(!preserve_failure_tail(&source, &destination).unwrap());
        assert!(!destination.exists());
        fs::write(&destination, b"stale failure").unwrap();
        fs::write(&source, []).unwrap();
        assert!(!preserve_failure_tail(&source, &destination).unwrap());
        assert!(!destination.exists());
        let _ = fs::remove_file(source);
    }
}
