//! Lifecycle owner for the temporary mmap files used by shared-memory strategy workers.

use qx_strategy::{SharedRingConfig, SharedRingReader, SharedRingWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_RING_ID: AtomicU64 = AtomicU64::new(1);

struct RingFileGuard(Vec<PathBuf>);

impl RingFileGuard {
    fn register(&mut self, path: PathBuf) {
        self.0.push(path);
    }

    fn disown(&mut self) {
        self.0.clear();
    }
}

impl Drop for RingFileGuard {
    fn drop(&mut self) {
        for path in self.0.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub struct StrategySharedRings {
    process_id: u32,
    input_path: PathBuf,
    output_path: PathBuf,
    input: Option<SharedRingWriter>,
    output: Option<SharedRingReader>,
}

impl StrategySharedRings {
    pub fn create(config: SharedRingConfig) -> Result<Self, String> {
        Self::create_in(config, &std::env::temp_dir())
    }

    pub fn create_in(config: SharedRingConfig, directory: &Path) -> Result<Self, String> {
        config
            .validate()
            .map_err(|error| format!("共享 ring 配置非法: {error}"))?;
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("创建共享 ring 目录失败 {}: {error}", directory.display()))?;
        let process_id = std::process::id();
        let ring_id = NEXT_RING_ID.fetch_add(1, Ordering::Relaxed);
        let base = directory.join(format!("qianxing-strategy-ring-{process_id}-{ring_id}"));
        let input_path = base.with_extension("input");
        let output_path = base.with_extension("output");
        let mut guard = RingFileGuard(Vec::new());
        guard.register(input_path.clone());
        guard.register(output_path.clone());
        let input = SharedRingWriter::create(&input_path, config)
            .map_err(|error| format!("创建输入 ring 失败: {error}"))?;
        let output_writer = SharedRingWriter::create(&output_path, config)
            .map_err(|error| format!("创建输出 ring 失败: {error}"))?;
        drop(output_writer);
        let output = SharedRingReader::open(&output_path, config)
            .map_err(|error| format!("打开输出 ring 失败: {error}"))?;
        guard.disown();
        Ok(Self {
            process_id,
            input_path,
            output_path,
            input: Some(input),
            output: Some(output),
        })
    }

    pub fn process_id(&self) -> u32 {
        self.process_id
    }

    pub fn input_path(&self) -> &Path {
        &self.input_path
    }

    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    pub fn input_mut(&mut self) -> Option<&mut SharedRingWriter> {
        self.input.as_mut()
    }

    pub fn output_mut(&mut self) -> Option<&mut SharedRingReader> {
        self.output.as_mut()
    }
}

impl Drop for StrategySharedRings {
    fn drop(&mut self) {
        self.input.take();
        self.output.take();
        let _ = std::fs::remove_file(&self.input_path);
        let _ = std::fs::remove_file(&self.output_path);
    }
}
