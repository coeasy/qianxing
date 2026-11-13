//! Read port for bounded application artifacts.
//!
//! The application layer treats references as opaque keys. Storage adapters own path
//! authorization and turn those keys into bounded streams.

use crate::{AppError, AppErrorCategory};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

pub(crate) const MAX_ARTIFACT_LINE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ARTIFACT_ROWS: u64 = 5_000_000;

/// Logical content identity for an artifact whose bytes have a SHA-256 digest.
///
/// The identifier deliberately excludes a storage path. File-backed readers can still use a
/// legacy reference as a compatibility locator, while database and object-store readers can
/// resolve the content identity directly.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactId(String);

impl ArtifactId {
    pub fn from_sha256_hex(value: impl Into<String>) -> Result<Self, AppError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "ArtifactId 必须是 64 位小写 SHA-256 十六进制摘要",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A content identity paired with an optional legacy location from an older artifact schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactLocatorV1 {
    pub id: ArtifactId,
    pub legacy_reference: String,
}

/// An opened artifact stream and the byte length observed by its storage adapter.
pub struct ArtifactStream {
    pub byte_len: u64,
    pub reader: Box<dyn BufRead + Send>,
}

impl ArtifactStream {
    pub fn new(byte_len: u64, reader: impl BufRead + Send + 'static) -> Self {
        Self {
            byte_len,
            reader: Box::new(reader),
        }
    }
}

/// Read-only storage port used by report verification and projection.
///
/// `reference` is deliberately opaque here. File-backed adapters may treat it as a path;
/// database and object-store adapters can resolve a logical artifact ID instead.
pub trait ArtifactReader: Send + Sync {
    fn open(&self, reference: &str) -> Result<ArtifactStream, AppError>;

    /// Compare two storage references at the adapter boundary. Opaque stores compare keys
    /// exactly; path-backed adapters may normalize aliases after applying their own access rules.
    fn equivalent_references(&self, left: &str, right: &str) -> Result<bool, AppError> {
        Ok(left == right)
    }

    /// Resolve an artifact by its logical content identity. The optional reference is only a
    /// compatibility locator for existing path-based RunRecord documents.
    fn open_by_id(
        &self,
        artifact_id: &ArtifactId,
        legacy_reference: Option<&str>,
    ) -> Result<ArtifactStream, AppError> {
        let Some(reference) = legacy_reference else {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                format!("存储适配器无法解析 ArtifactId {}", artifact_id.as_str()),
            ));
        };
        self.open(reference)
    }
}

/// Compatibility adapter for local callers whose artifact references are filesystem paths.
/// Network/API surfaces should provide a root-authorized adapter instead.
#[derive(Clone, Copy, Debug, Default)]
pub struct FileArtifactReader;

impl ArtifactReader for FileArtifactReader {
    fn open(&self, reference: &str) -> Result<ArtifactStream, AppError> {
        let path = Path::new(reference);
        let file = File::open(path).map_err(|error| {
            AppError::from_io(&format!("读取研究产物 {}", path.display()), &error)
        })?;
        let metadata = file.metadata().map_err(|error| {
            AppError::from_io(&format!("读取研究产物元数据 {}", path.display()), &error)
        })?;
        if !metadata.is_file() {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                format!("研究产物 {} 不是普通文件", path.display()),
            ));
        }
        Ok(ArtifactStream::new(metadata.len(), BufReader::new(file)))
    }

    fn equivalent_references(&self, left: &str, right: &str) -> Result<bool, AppError> {
        let left = Path::new(left)
            .canonicalize()
            .map_err(|error| AppError::from_io("解析左侧研究产物引用", &error))?;
        let right = Path::new(right)
            .canonicalize()
            .map_err(|error| AppError::from_io("解析右侧研究产物引用", &error))?;
        Ok(left == right)
    }
}

/// Read a UTF-8 artifact while enforcing both the adapter's observed size and the actual
/// number of bytes returned by its stream.
pub fn read_text_bounded(
    reader: &dyn ArtifactReader,
    reference: &str,
    byte_limit: u64,
    label: &str,
) -> Result<String, AppError> {
    let artifact = reader.open(reference)?;
    read_stream_text_bounded(artifact, reference, None, byte_limit, label)
}

pub(crate) fn read_text_bounded_by_id(
    reader: &dyn ArtifactReader,
    artifact_id: &ArtifactId,
    legacy_reference: Option<&str>,
    byte_limit: u64,
    label: &str,
) -> Result<String, AppError> {
    let artifact = reader.open_by_id(artifact_id, legacy_reference)?;
    read_stream_text_bounded(
        artifact,
        artifact_id.as_str(),
        Some(artifact_id),
        byte_limit,
        label,
    )
}

fn read_stream_text_bounded(
    artifact: ArtifactStream,
    reference: &str,
    expected_id: Option<&ArtifactId>,
    byte_limit: u64,
    label: &str,
) -> Result<String, AppError> {
    if artifact.byte_len > byte_limit {
        return Err(artifact_too_large(reference, byte_limit));
    }
    let mut bytes = Vec::new();
    artifact
        .reader
        .take(byte_limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| AppError::from_io(&format!("读取{label} {reference}"), &error))?;
    if bytes.len() as u64 > byte_limit {
        return Err(artifact_too_large(reference, byte_limit));
    }
    verify_content_id(expected_id, &bytes, reference)?;
    String::from_utf8(bytes).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("{label} {reference} 不是有效 UTF-8: {error}"),
        )
    })
}

fn verify_content_id(
    expected_id: Option<&ArtifactId>,
    bytes: &[u8],
    reference: &str,
) -> Result<(), AppError> {
    let Some(expected_id) = expected_id else {
        return Ok(());
    };
    let actual = qx_strategy::sha256_hex(bytes);
    if actual != expected_id.as_str() {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!(
                "研究产物 ArtifactId 内容摘要不匹配 {reference}: 期望={} 实际={actual}",
                expected_id.as_str()
            ),
        ));
    }
    Ok(())
}

pub(crate) fn sha256_artifact_bounded(
    reader: &dyn ArtifactReader,
    artifact_id: &ArtifactId,
    legacy_reference: Option<&str>,
    byte_limit: u64,
) -> Result<String, AppError> {
    let artifact = reader.open_by_id(artifact_id, legacy_reference)?;
    if artifact.byte_len > byte_limit {
        return Err(artifact_too_large(artifact_id.as_str(), byte_limit));
    }
    qx_strategy::file_digest::sha256_reader_hex_bounded(artifact.reader, byte_limit).map_err(
        |error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!(
                    "计算研究产物 {} 的 SHA-256 失败: {error}",
                    artifact_id.as_str()
                ),
            )
        },
    )
}

pub fn artifact_too_large(reference: &str, byte_limit: u64) -> AppError {
    AppError::new(
        AppErrorCategory::Timeout,
        format!("研究产物 {reference} 超过读取大小上限 {byte_limit} bytes"),
    )
}

/// Read one UTF-8 line without allocating beyond the line budget.
pub fn read_bounded_line(
    reader: &mut dyn BufRead,
    reference: &str,
) -> Result<Option<(String, usize)>, AppError> {
    let mut bytes = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| AppError::from_io(&format!("读取研究产物 {reference}"), &error))?;
        if available.is_empty() {
            break;
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if bytes.len().saturating_add(count) > MAX_ARTIFACT_LINE_BYTES {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                format!("研究产物 {reference} 单行超过 1 MiB 上限"),
            ));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if bytes.last() == Some(&b'\n') {
            break;
        }
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    let byte_len = bytes.len();
    let line = String::from_utf8(bytes).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("研究产物 {reference} 包含无效 UTF-8: {error}"),
        )
    })?;
    Ok(Some((line, byte_len)))
}

/// Bounded streaming CSV check used by RunManifest verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CsvArtifactCheck {
    pub rows: u64,
    pub final_equity_raw: Option<i128>,
    pub final_equity_exact: Option<String>,
}

pub(crate) fn scan_csv_artifact(
    artifact_reader: &dyn ArtifactReader,
    reference: &str,
    byte_limit: u64,
    equity: bool,
) -> Result<CsvArtifactCheck, AppError> {
    let artifact = artifact_reader.open(reference)?;
    scan_csv_stream(artifact, reference, None, byte_limit, equity)
}

pub(crate) fn scan_csv_artifact_by_id(
    artifact_reader: &dyn ArtifactReader,
    locator: &ArtifactLocatorV1,
    byte_limit: u64,
    equity: bool,
) -> Result<CsvArtifactCheck, AppError> {
    let artifact = artifact_reader.open_by_id(&locator.id, Some(&locator.legacy_reference))?;
    scan_csv_stream(
        artifact,
        locator.id.as_str(),
        Some(&locator.id),
        byte_limit,
        equity,
    )
}

fn scan_csv_stream(
    mut artifact: ArtifactStream,
    reference: &str,
    expected_id: Option<&ArtifactId>,
    byte_limit: u64,
    equity: bool,
) -> Result<CsvArtifactCheck, AppError> {
    if artifact.byte_len > byte_limit {
        return Err(artifact_too_large(reference, byte_limit));
    }
    let mut bytes_seen = 0u64;
    let (header, header_bytes) = read_bounded_line(artifact.reader.as_mut(), reference)?
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("CSV 产物 {reference} 为空"),
            )
        })?;
    bytes_seen = bytes_seen.saturating_add(header_bytes as u64);
    let mut digest = expected_id.map(|_| qx_strategy::file_digest::Sha256Accumulator::new());
    if let Some(digest) = &mut digest {
        digest.update(header.as_bytes());
    }
    if bytes_seen > byte_limit {
        return Err(artifact_too_large(reference, byte_limit));
    }
    let header_cells = header
        .lines()
        .next()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .collect::<Vec<_>>();
    let equity_column = equity
        .then(|| header_cells.iter().position(|cell| *cell == "equity_raw"))
        .flatten();
    let exact_equity_column = equity
        .then(|| header_cells.iter().position(|cell| *cell == "equity_exact"))
        .flatten();
    let mut rows = 0u64;
    let mut final_equity_raw = None;
    let mut final_equity_exact = None;
    while let Some((line, line_bytes)) = read_bounded_line(artifact.reader.as_mut(), reference)? {
        bytes_seen = bytes_seen.saturating_add(line_bytes as u64);
        if let Some(digest) = &mut digest {
            digest.update(line.as_bytes());
        }
        if bytes_seen > byte_limit {
            return Err(artifact_too_large(reference, byte_limit));
        }
        if line.trim().is_empty() {
            continue;
        }
        rows = rows.saturating_add(1);
        if rows > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "研究报告明细超过 500 万行上限",
            ));
        }
        if let Some(column) = equity_column {
            final_equity_raw = line
                .split(',')
                .nth(column)
                .and_then(|value| value.trim().parse::<i128>().ok());
        }
        if let Some(column) = exact_equity_column {
            final_equity_exact = line
                .split(',')
                .nth(column)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
        }
    }
    if let (Some(expected_id), Some(digest)) = (expected_id, digest) {
        let actual = digest.finish_hex();
        if actual != expected_id.as_str() {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "研究产物 ArtifactId 内容摘要不匹配 {reference}: 期望={} 实际={actual}",
                    expected_id.as_str()
                ),
            ));
        }
    }
    Ok(CsvArtifactCheck {
        rows,
        final_equity_raw,
        final_equity_exact,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        read_bounded_line, read_text_bounded, read_text_bounded_by_id, scan_csv_artifact,
        scan_csv_artifact_by_id, ArtifactId, ArtifactLocatorV1, ArtifactReader, ArtifactStream,
        MAX_ARTIFACT_LINE_BYTES,
    };
    use crate::{AppError, AppErrorCategory};
    use std::io::Cursor;

    struct LyingReader;

    impl ArtifactReader for LyingReader {
        fn open(&self, _reference: &str) -> Result<ArtifactStream, AppError> {
            Ok(ArtifactStream::new(
                1,
                Cursor::new(b"stream-is-longer-than-metadata".to_vec()),
            ))
        }
    }

    #[test]
    fn bounded_text_reader_checks_actual_stream_bytes() {
        let error = read_text_bounded(&LyingReader, "memory:key", 8, "fixture")
            .expect_err("actual stream bytes must obey the limit");
        assert_eq!(error.category(), AppErrorCategory::Timeout);
    }

    struct CsvReader(Vec<u8>);

    impl ArtifactReader for CsvReader {
        fn open(&self, _reference: &str) -> Result<ArtifactStream, AppError> {
            Ok(ArtifactStream::new(
                self.0.len() as u64,
                Cursor::new(self.0.clone()),
            ))
        }
    }

    #[test]
    fn csv_scanner_counts_rows_and_checks_final_equity_without_buffering_the_file() {
        let reader = CsvReader(b"ts,equity_raw,position_raw\n1,10,0\n2,20,0\n".to_vec());
        let check = scan_csv_artifact(&reader, "memory:equity", 1024, true).unwrap();
        assert_eq!(check.rows, 2);
        assert_eq!(check.final_equity_raw, Some(20));
    }

    struct ContentIdReader(Vec<u8>);

    impl ArtifactReader for ContentIdReader {
        fn open(&self, _reference: &str) -> Result<ArtifactStream, AppError> {
            Err(AppError::new(
                AppErrorCategory::DataUnavailable,
                "path lookup is intentionally unavailable",
            ))
        }

        fn open_by_id(
            &self,
            _artifact_id: &ArtifactId,
            _legacy_reference: Option<&str>,
        ) -> Result<ArtifactStream, AppError> {
            Ok(ArtifactStream::new(
                self.0.len() as u64,
                Cursor::new(self.0.clone()),
            ))
        }
    }

    #[test]
    fn text_read_by_artifact_id_rejects_mismatched_content() {
        let id = ArtifactId::from_sha256_hex(qx_strategy::sha256_hex(b"expected text"))
            .expect("valid digest");
        let reader = ContentIdReader(b"different text".to_vec());
        let error = read_text_bounded_by_id(&reader, &id, None, 1024, "fixture")
            .expect_err("content must match its ArtifactId");
        assert_eq!(error.category(), AppErrorCategory::Conflict);
    }

    #[test]
    fn csv_scan_by_artifact_id_rejects_mismatched_content() {
        let expected = b"ts,equity_raw\n1,10\n2,20\n";
        let id =
            ArtifactId::from_sha256_hex(qx_strategy::sha256_hex(expected)).expect("valid digest");
        let reader = ContentIdReader(b"ts,equity_raw\n1,10\n2,21\n".to_vec());
        let locator = ArtifactLocatorV1 {
            id,
            legacy_reference: "legacy.csv".to_owned(),
        };
        let error = scan_csv_artifact_by_id(&reader, &locator, 1024, true)
            .expect_err("content must match its ArtifactId");
        assert_eq!(error.category(), AppErrorCategory::Conflict);
    }

    #[test]
    fn bounded_line_reader_rejects_a_line_before_growing_past_its_budget() {
        let bytes = vec![b'x'; MAX_ARTIFACT_LINE_BYTES + 1];
        let mut reader = Cursor::new(bytes.as_slice());
        let error = read_bounded_line(&mut reader, "memory:long-line")
            .expect_err("oversized line must be rejected");
        assert_eq!(error.category(), AppErrorCategory::Timeout);
    }
}
