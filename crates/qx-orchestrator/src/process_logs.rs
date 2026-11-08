//! Bounded stdout/stderr sinks for supervised workers.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};

pub(crate) const MAX_PROCESS_LOG_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) fn prepare(path: &Path) -> io::Result<File> {
    let rotated = rotated_path(path);
    match fs::remove_file(rotated) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    File::create(path)
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
}
