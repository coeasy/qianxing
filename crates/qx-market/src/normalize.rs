//! Canonical market data normalization.

pub trait Normalizer<Input, Output> {
    fn normalize(&self, input: Input) -> Output;
}
