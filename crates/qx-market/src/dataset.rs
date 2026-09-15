//! Dataset artifact metadata for reproducible research.

#[derive(Debug, Clone)]
pub struct DatasetArtifact {
    pub id: String,
    pub source: String,
    pub version: String,
    pub checksum: String,
}
