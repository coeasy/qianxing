//! Market data processing pipeline.

use crate::quality::{QualityGate, QualityStatus};

pub struct MarketPipeline<T> {
    quality: QualityGate<T>,
}

impl<T> MarketPipeline<T> {
    pub fn new(quality: QualityGate<T>) -> Self {
        Self { quality }
    }

    pub fn process(&self, item: &T) -> QualityStatus {
        self.quality.validate(item)
    }
}
