//! Execution boundary.

pub trait ExecutionGateway<Command, Result> {
    fn submit(&self, command: Command) -> Result;
}
