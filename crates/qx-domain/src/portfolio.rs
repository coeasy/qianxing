use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Portfolio {
    pub id: String,
    pub positions: BTreeMap<String, i128>,
}
