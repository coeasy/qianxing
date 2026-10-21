//! Transport choices shared by the Strategy SDK and its process hosts.

use serde::{Deserialize, Serialize};

/// Wire protocol used to exchange strategy requests with a worker process.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyTransport {
    #[default]
    Jsonl,
    FramedJson,
    SharedMemoryJson,
    SharedMemoryColumnar,
}
