//! Portfolio state.

#[derive(Debug, Default)]
pub struct Portfolio {
    pub cash: i64,
    pub position_value: i64,
}

impl Portfolio {
    pub fn equity(&self) -> i64 {
        self.cash + self.position_value
    }
}
