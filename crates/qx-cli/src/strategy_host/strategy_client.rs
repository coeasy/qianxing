use super::{DynamicCAbiStrategy, PythonStrategyClient};

pub(crate) enum ContractStrategyClient {
    Process(Box<PythonStrategyClient>),
    Native(Box<DynamicCAbiStrategy>),
}
