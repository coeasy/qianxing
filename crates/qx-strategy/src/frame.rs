//! 跨进程策略数据面的版本化分帧协议。
//!
//! `framed_json` 是 JSONL 的兼容增强：payload 仍是已经校验的策略 JSON，
//! 但传输使用固定头、长度边界、序号和 CRC32，避免逐行扫描、无限行和
//! 半包/粘包歧义。后续可以在不改变外层生命周期和序号语义的前提下，
//! 将 payload 替换为 QXCB/Arrow/定长列式编码。

use std::io::{self, Read};

pub const STRATEGY_FRAME_MAGIC: [u8; 4] = *b"QXSF";
pub const STRATEGY_FRAME_VERSION: u16 = 1;
pub const STRATEGY_FRAME_HEADER_LEN: usize = 24;
pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StrategyFrameKind {
    Request = 1,
    Response = 2,
    Error = 3,
}

impl StrategyFrameKind {
    fn from_raw(value: u8) -> Result<Self, String> {
        match value {
            1 => Ok(Self::Request),
            2 => Ok(Self::Response),
            3 => Ok(Self::Error),
            other => Err(format!("未知策略分帧 kind: {other}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrategyFrame {
    pub kind: StrategyFrameKind,
    pub sequence: u64,
    pub payload: Vec<u8>,
}

impl StrategyFrame {
    pub fn request(sequence: u64, payload: Vec<u8>) -> Self {
        Self {
            kind: StrategyFrameKind::Request,
            sequence,
            payload,
        }
    }

    pub fn response(sequence: u64, payload: Vec<u8>) -> Self {
        Self {
            kind: StrategyFrameKind::Response,
            sequence,
            payload,
        }
    }

    pub fn error(sequence: u64, payload: Vec<u8>) -> Self {
        Self {
            kind: StrategyFrameKind::Error,
            sequence,
            payload,
        }
    }

    pub fn encode(&self, max_frame_bytes: usize) -> Result<Vec<u8>, String> {
        let total = STRATEGY_FRAME_HEADER_LEN
            .checked_add(self.payload.len())
            .ok_or_else(|| "策略分帧长度溢出".to_string())?;
        if total > max_frame_bytes || total > u32::MAX as usize {
            return Err(format!(
                "策略分帧超过大小上限: bytes={total} max={max_frame_bytes}"
            ));
        }
        let mut encoded = Vec::with_capacity(total);
        encoded.extend_from_slice(&STRATEGY_FRAME_MAGIC);
        encoded.extend_from_slice(&STRATEGY_FRAME_VERSION.to_le_bytes());
        encoded.push(self.kind as u8);
        encoded.push(0); // flags，保留给后续压缩/编码协商。
        encoded.extend_from_slice(&self.sequence.to_le_bytes());
        encoded.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        encoded.extend_from_slice(&crc32(&self.payload).to_le_bytes());
        encoded.extend_from_slice(&self.payload);
        Ok(encoded)
    }

    pub fn read_from<R: Read>(reader: &mut R, max_frame_bytes: usize) -> Result<Self, String> {
        let mut header = [0_u8; STRATEGY_FRAME_HEADER_LEN];
        reader.read_exact(&mut header).map_err(format_read_error)?;
        if header[..4] != STRATEGY_FRAME_MAGIC {
            return Err("策略分帧 magic 不匹配".into());
        }
        let version = u16::from_le_bytes([header[4], header[5]]);
        if version != STRATEGY_FRAME_VERSION {
            return Err(format!("不支持的策略分帧版本: {version}"));
        }
        if header[7] != 0 {
            return Err("策略分帧 flags 含未支持位".into());
        }
        let kind = StrategyFrameKind::from_raw(header[6])?;
        let sequence = u64::from_le_bytes(header[8..16].try_into().expect("fixed header"));
        let payload_len =
            u32::from_le_bytes(header[16..20].try_into().expect("fixed header")) as usize;
        let total = STRATEGY_FRAME_HEADER_LEN
            .checked_add(payload_len)
            .ok_or_else(|| "策略分帧长度溢出".to_string())?;
        if total > max_frame_bytes {
            return Err(format!(
                "策略分帧超过大小上限: bytes={total} max={max_frame_bytes}"
            ));
        }
        let expected_crc = u32::from_le_bytes(header[20..24].try_into().expect("fixed header"));
        let mut payload = vec![0_u8; payload_len];
        reader.read_exact(&mut payload).map_err(format_read_error)?;
        let actual_crc = crc32(&payload);
        if expected_crc != actual_crc {
            return Err(format!(
                "策略分帧 CRC32 不匹配: expected={expected_crc:08x} actual={actual_crc:08x}"
            ));
        }
        Ok(Self {
            kind,
            sequence,
            payload,
        })
    }
}

fn format_read_error(error: io::Error) -> String {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        "策略分帧输入提前结束".into()
    } else {
        format!("读取策略分帧失败: {error}")
    }
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
    use std::io::Cursor;

    #[test]
    fn frame_round_trip_preserves_sequence_and_payload() {
        let frame = StrategyFrame::request(42, b"{\"request_id\":\"r1\"}".to_vec());
        let encoded = frame.encode(DEFAULT_MAX_FRAME_BYTES).unwrap();
        assert_eq!(
            encoded.len(),
            STRATEGY_FRAME_HEADER_LEN + frame.payload.len()
        );
        let restored =
            StrategyFrame::read_from(&mut Cursor::new(encoded), DEFAULT_MAX_FRAME_BYTES).unwrap();
        assert_eq!(restored, frame);
    }

    #[test]
    fn frame_rejects_crc_and_size_errors() {
        let frame = StrategyFrame::response(1, b"payload".to_vec());
        let mut encoded = frame.encode(DEFAULT_MAX_FRAME_BYTES).unwrap();
        *encoded.last_mut().unwrap() ^= 1;
        assert!(
            StrategyFrame::read_from(&mut Cursor::new(encoded), DEFAULT_MAX_FRAME_BYTES)
                .unwrap_err()
                .contains("CRC32")
        );

        let frame = StrategyFrame::request(1, vec![0; 10]);
        assert!(frame.encode(STRATEGY_FRAME_HEADER_LEN + 9).is_err());
    }

    #[test]
    fn frame_rejects_unknown_flags_and_version() {
        let frame = StrategyFrame::request(1, b"x".to_vec());
        let mut encoded = frame.encode(DEFAULT_MAX_FRAME_BYTES).unwrap();
        encoded[7] = 1;
        assert!(
            StrategyFrame::read_from(&mut Cursor::new(encoded), DEFAULT_MAX_FRAME_BYTES)
                .unwrap_err()
                .contains("flags")
        );
    }
}
