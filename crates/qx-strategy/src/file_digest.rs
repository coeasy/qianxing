//! 流式文件摘要：大回测产物不必整份读入内存才能生成内容身份。

use ring::digest::{Context, SHA256};
use std::io::Read;
use std::path::Path;

/// Incremental SHA-256 state for bounded stream consumers that also parse each chunk.
pub struct Sha256Accumulator(Context);

impl Sha256Accumulator {
    pub fn new() -> Self {
        Self(Context::new(&SHA256))
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub fn finish_hex(self) -> String {
        digest_to_hex(self.0.finish().as_ref())
    }
}

impl Default for Sha256Accumulator {
    fn default() -> Self {
        Self::new()
    }
}

fn digest_to_hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 以固定 64 KiB 内存缓冲读取文件，返回小写 SHA-256 十六进制摘要。
pub fn sha256_file_hex(path: impl AsRef<Path>) -> Result<String, String> {
    let path = path.as_ref();
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("打开摘要文件失败 {}: {error}", path.display()))?;
    let mut context = Sha256Accumulator::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("读取摘要文件失败 {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }
    Ok(context.finish_hex())
}

/// Hash a stream with a fixed-size buffer and reject data beyond `byte_limit`.
pub fn sha256_reader_hex_bounded(mut reader: impl Read, byte_limit: u64) -> Result<String, String> {
    let mut context = Sha256Accumulator::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let remaining_with_probe = byte_limit.saturating_sub(total).saturating_add(1);
        let read_limit =
            usize::try_from(remaining_with_probe.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = reader
            .read(&mut buffer[..read_limit])
            .map_err(|error| format!("读取摘要流失败: {error}"))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > byte_limit {
            return Err(format!("摘要流超过 {byte_limit} bytes 上限"));
        }
        context.update(&buffer[..read]);
    }
    Ok(context.finish_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_file_digest_matches_in_memory_digest() {
        let path = std::env::temp_dir().join(format!("qx-file-digest-{}.bin", std::process::id()));
        let payload = vec![0x5a; 160_000];
        std::fs::write(&path, &payload).unwrap();
        assert_eq!(sha256_file_hex(&path).unwrap(), crate::sha256_hex(&payload));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn incremental_digest_matches_single_buffer_digest() {
        let payload = vec![0xa5; 160_123];
        let mut digest = Sha256Accumulator::new();
        for chunk in payload.chunks(997) {
            digest.update(chunk);
        }
        assert_eq!(digest.finish_hex(), crate::sha256_hex(&payload));
    }
}
