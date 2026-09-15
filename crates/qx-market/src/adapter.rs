//! Market adapter boundary.
//!
//! Adapters convert external exchange formats into qx-market canonical types.

pub trait MarketAdapter<Input, Output> {
    fn normalize(&self, input: Input) -> Output;
}
