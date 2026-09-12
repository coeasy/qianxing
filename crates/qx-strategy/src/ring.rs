//! 单生产者/单消费者共享内存环形缓冲。
//!
//! Ring 只负责安全传输已经编码的策略分帧，不解析策略语义。一个 ring
//! 必须恰好有一个 writer 和一个 reader；多生产者/多消费者应在上层按
//! strategy instance 分片，不能把多个进程直接写入同一 ring。

use crate::frame::{StrategyFrame, DEFAULT_MAX_FRAME_BYTES};
use memmap2::{MmapMut, MmapOptions};
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: [u8; 4] = *b"QXRB";
const VERSION: u32 = 1;
const HEADER_BYTES: usize = 64;
const SLOT_HEADER_BYTES: usize = 16;
const WRITE_SEQ_OFFSET: usize = 16;
const READ_SEQ_OFFSET: usize = 24;
const SLOT_COMMIT_OFFSET: usize = 0;
const SLOT_LEN_OFFSET: usize = 8;
const SLOT_CRC_OFFSET: usize = 12;
pub const DEFAULT_RING_CAPACITY: u32 = 1024;
pub const DEFAULT_RING_SLOT_BYTES: u32 = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedRingConfig {
    pub capacity: u32,
    pub slot_bytes: u32,
}

impl SharedRingConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.capacity < 2 || !self.capacity.is_power_of_two() {
            return Err("共享 ring capacity 必须是大于等于2的2次幂".into());
        }
        if self.slot_bytes as usize <= SLOT_HEADER_BYTES
            || !self.slot_bytes.is_multiple_of(8)
            || self.slot_bytes as usize > 64 * 1024 * 1024
        {
            return Err("共享 ring slot_bytes 必须是8的倍数且在16..=67108864 内".into());
        }
        let total = HEADER_BYTES
            .checked_add(self.capacity as usize * self.slot_bytes as usize)
            .ok_or_else(|| "共享 ring 文件大小溢出".to_string())?;
        if total > isize::MAX as usize {
            return Err("共享 ring 文件大小超过平台限制".into());
        }
        Ok(())
    }

    pub fn max_payload_bytes(&self) -> usize {
        self.slot_bytes as usize - SLOT_HEADER_BYTES
    }

    fn total_bytes(&self) -> Result<usize, String> {
        self.validate()?;
        Ok(HEADER_BYTES + self.capacity as usize * self.slot_bytes as usize)
    }
}

impl Default for SharedRingConfig {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_RING_CAPACITY,
            slot_bytes: DEFAULT_RING_SLOT_BYTES,
        }
    }
}

#[derive(Debug)]
pub enum SharedRingError {
    Io(io::Error),
    Invalid(String),
    Full,
    Empty,
    Corrupt(String),
}

impl std::fmt::Display for SharedRingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "共享 ring IO 失败: {error}"),
            Self::Invalid(error) => write!(f, "共享 ring 配置非法: {error}"),
            Self::Full => write!(f, "共享 ring 已满"),
            Self::Empty => write!(f, "共享 ring 为空"),
            Self::Corrupt(error) => write!(f, "共享 ring 数据损坏: {error}"),
        }
    }
}

impl std::error::Error for SharedRingError {}

impl From<io::Error> for SharedRingError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct SharedRingWriter {
    path: PathBuf,
    _file: File,
    mmap: MmapMut,
    config: SharedRingConfig,
}

pub struct SharedRingReader {
    path: PathBuf,
    _file: File,
    mmap: MmapMut,
    config: SharedRingConfig,
}

impl SharedRingWriter {
    /// 创建一个新的 ring 文件。调用方必须保证没有其他进程同时创建同一路径。
    pub fn create(
        path: impl AsRef<Path>,
        config: SharedRingConfig,
    ) -> Result<Self, SharedRingError> {
        let path = path.as_ref().to_path_buf();
        let total = config.total_bytes().map_err(SharedRingError::Invalid)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)?;
        file.set_len(total as u64)?;
        let mut mmap = unsafe { MmapOptions::new().len(total).map_mut(&file)? };
        initialize_header(&mut mmap, config)?;
        mmap.flush()?;
        Ok(Self {
            path,
            _file: file,
            mmap,
            config,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn config(&self) -> SharedRingConfig {
        self.config
    }

    pub fn try_push(&mut self, payload: &[u8]) -> Result<(), SharedRingError> {
        if payload.len() > self.config.max_payload_bytes() {
            return Err(SharedRingError::Invalid(format!(
                "payload bytes={} max={}",
                payload.len(),
                self.config.max_payload_bytes()
            )));
        }
        let write = load_seq(&self.mmap, WRITE_SEQ_OFFSET);
        let read = load_seq(&self.mmap, READ_SEQ_OFFSET);
        if write.wrapping_sub(read) >= u64::from(self.config.capacity) {
            return Err(SharedRingError::Full);
        }
        let slot = slot_offset(self.config, write);
        write_u32(&mut self.mmap, slot + SLOT_LEN_OFFSET, payload.len() as u32);
        write_u32(&mut self.mmap, slot + SLOT_CRC_OFFSET, crc32(payload));
        self.mmap[slot + SLOT_HEADER_BYTES..slot + SLOT_HEADER_BYTES + payload.len()]
            .copy_from_slice(payload);
        publish_seq(&mut self.mmap, slot + SLOT_COMMIT_OFFSET, write + 1);
        store_seq(&self.mmap, WRITE_SEQ_OFFSET, write + 1);
        Ok(())
    }

    pub fn try_push_frame(&mut self, frame: &StrategyFrame) -> Result<(), SharedRingError> {
        let encoded = frame
            .encode(DEFAULT_MAX_FRAME_BYTES)
            .map_err(SharedRingError::Invalid)?;
        self.try_push(&encoded)
    }
}

impl SharedRingReader {
    pub fn open(path: impl AsRef<Path>, config: SharedRingConfig) -> Result<Self, SharedRingError> {
        let path = path.as_ref().to_path_buf();
        config.validate().map_err(SharedRingError::Invalid)?;
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let metadata = file.metadata()?;
        let expected = config.total_bytes().map_err(SharedRingError::Invalid)? as u64;
        if metadata.len() != expected {
            return Err(SharedRingError::Invalid(format!(
                "ring 文件大小不匹配: actual={} expected={expected}",
                metadata.len()
            )));
        }
        let mmap = unsafe { MmapOptions::new().len(expected as usize).map_mut(&file)? };
        validate_header(&mmap, config)?;
        Ok(Self {
            path,
            _file: file,
            mmap,
            config,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn config(&self) -> SharedRingConfig {
        self.config
    }

    pub fn try_pop(&mut self) -> Result<Vec<u8>, SharedRingError> {
        let read = load_seq(&self.mmap, READ_SEQ_OFFSET);
        let write = load_seq(&self.mmap, WRITE_SEQ_OFFSET);
        if read == write {
            return Err(SharedRingError::Empty);
        }
        let slot = slot_offset(self.config, read);
        let committed = load_seq(&self.mmap, slot + SLOT_COMMIT_OFFSET);
        if committed != read + 1 {
            return Err(SharedRingError::Corrupt(format!(
                "slot commit sequence 不匹配: expected={} actual={committed}",
                read + 1
            )));
        }
        let length = read_u32(&self.mmap, slot + SLOT_LEN_OFFSET) as usize;
        if length > self.config.max_payload_bytes() {
            return Err(SharedRingError::Corrupt(format!(
                "slot length 超限: bytes={length} max={}",
                self.config.max_payload_bytes()
            )));
        }
        let expected_crc = read_u32(&self.mmap, slot + SLOT_CRC_OFFSET);
        let payload =
            self.mmap[slot + SLOT_HEADER_BYTES..slot + SLOT_HEADER_BYTES + length].to_vec();
        if crc32(&payload) != expected_crc {
            return Err(SharedRingError::Corrupt("slot CRC32 不匹配".into()));
        }
        store_seq(&self.mmap, READ_SEQ_OFFSET, read + 1);
        Ok(payload)
    }

    pub fn try_pop_frame(&mut self) -> Result<StrategyFrame, SharedRingError> {
        let payload = self.try_pop()?;
        StrategyFrame::read_from(&mut std::io::Cursor::new(payload), DEFAULT_MAX_FRAME_BYTES)
            .map_err(SharedRingError::Corrupt)
    }
}

fn initialize_header(mmap: &mut MmapMut, config: SharedRingConfig) -> Result<(), SharedRingError> {
    config.validate().map_err(SharedRingError::Invalid)?;
    mmap[..4].copy_from_slice(&MAGIC);
    write_u32(mmap, 4, VERSION);
    write_u32(mmap, 8, config.slot_bytes);
    write_u32(mmap, 12, config.capacity);
    store_seq(mmap, WRITE_SEQ_OFFSET, 0);
    store_seq(mmap, READ_SEQ_OFFSET, 0);
    Ok(())
}

fn validate_header(mmap: &MmapMut, config: SharedRingConfig) -> Result<(), SharedRingError> {
    if mmap[..4] != MAGIC
        || read_u32(mmap, 4) != VERSION
        || read_u32(mmap, 8) != config.slot_bytes
        || read_u32(mmap, 12) != config.capacity
    {
        return Err(SharedRingError::Invalid("ring header 不匹配".into()));
    }
    Ok(())
}

fn slot_offset(config: SharedRingConfig, sequence: u64) -> usize {
    HEADER_BYTES + (sequence as usize & (config.capacity as usize - 1)) * config.slot_bytes as usize
}

fn load_seq(mmap: &MmapMut, offset: usize) -> u64 {
    debug_assert_eq!(offset % size_of::<AtomicU64>(), 0);
    unsafe { (&*(mmap.as_ptr().add(offset) as *const AtomicU64)).load(Ordering::Acquire) }
}

fn store_seq(mmap: &MmapMut, offset: usize, value: u64) {
    debug_assert_eq!(offset % size_of::<AtomicU64>(), 0);
    unsafe { (&*(mmap.as_ptr().add(offset) as *const AtomicU64)).store(value, Ordering::Release) }
}

fn publish_seq(mmap: &mut MmapMut, offset: usize, value: u64) {
    debug_assert_eq!(offset % size_of::<AtomicU64>(), 0);
    unsafe {
        (&*(mmap.as_mut_ptr().add(offset) as *const AtomicU64)).store(value, Ordering::Release)
    }
}

fn write_u32(mmap: &mut MmapMut, offset: usize, value: u32) {
    mmap[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn read_u32(mmap: &MmapMut, offset: usize) -> u32 {
    u32::from_le_bytes(mmap[offset..offset + 4].try_into().expect("fixed u32"))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_PATH: AtomicUsize = AtomicUsize::new(0);

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "qianxing-strategy-ring-{}-{}.bin",
            std::process::id(),
            NEXT_PATH.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn spsc_ring_preserves_order_and_applies_capacity_backpressure() {
        let path = temp_path();
        let config = SharedRingConfig {
            capacity: 2,
            slot_bytes: 64,
        };
        let mut writer = SharedRingWriter::create(&path, config).unwrap();
        let mut reader = SharedRingReader::open(&path, config).unwrap();
        assert!(matches!(reader.try_pop(), Err(SharedRingError::Empty)));
        writer.try_push(b"one").unwrap();
        writer.try_push(b"two").unwrap();
        assert!(matches!(
            writer.try_push(b"three"),
            Err(SharedRingError::Full)
        ));
        assert_eq!(reader.try_pop().unwrap(), b"one");
        writer.try_push(b"three").unwrap();
        assert_eq!(reader.try_pop().unwrap(), b"two");
        assert_eq!(reader.try_pop().unwrap(), b"three");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ring_rejects_bad_header_and_payload_crc() {
        let path = temp_path();
        let config = SharedRingConfig {
            capacity: 2,
            slot_bytes: 64,
        };
        let mut writer = SharedRingWriter::create(&path, config).unwrap();
        writer.try_push(b"safe").unwrap();
        drop(writer);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let mut mmap = unsafe {
            MmapOptions::new()
                .len(HEADER_BYTES + 2 * 64)
                .map_mut(&file)
                .unwrap()
        };
        mmap[HEADER_BYTES + SLOT_HEADER_BYTES] ^= 1;
        mmap.flush().unwrap();
        drop(mmap);
        let mut reader = SharedRingReader::open(&path, config).unwrap();
        assert!(matches!(reader.try_pop(), Err(SharedRingError::Corrupt(_))));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ring_can_carry_versioned_strategy_frames() {
        let path = temp_path();
        let config = SharedRingConfig {
            capacity: 2,
            slot_bytes: 128,
        };
        let mut writer = SharedRingWriter::create(&path, config).unwrap();
        let mut reader = SharedRingReader::open(&path, config).unwrap();
        let frame = StrategyFrame::request(9, b"frame-payload".to_vec());
        writer.try_push_frame(&frame).unwrap();
        assert_eq!(reader.try_pop_frame().unwrap(), frame);
        let _ = fs::remove_file(path);
    }
}
