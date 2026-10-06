//! 流式文件摘要：大回测产物不必整份读入内存才能生成内容身份。

use ring::digest::{Context, SHA256};
use std::io::Read;
use std::path::Path;

/// 以固定 64 KiB 内存缓冲读取文件，返回小写 SHA-256 十六进制摘要。
pub fn sha256_file_hex(path: impl AsRef<Path>) -> Result<String, String> {
    let path = path.as_ref();
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("打开摘要文件失败 {}: {error}", path.display()))?;
    let mut context = Context::new(&SHA256);
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
    Ok(context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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
}
