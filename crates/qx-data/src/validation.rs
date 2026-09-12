#[derive(Clone, Debug)]
pub struct ValidationReport {
    pub checked: usize,
    pub errors: Vec<String>,
}

impl ValidationReport {
    pub fn ok(checked: usize) -> Self {
        Self { checked, errors: Vec::new() }
    }

    pub fn valid(&self) -> bool {
        self.errors.is_empty()
    }
}

pub fn validate_timestamps(values: &[u64]) -> Result<(), String> {
    if values.windows(2).any(|w| w[0] >= w[1]) {
        return Err("timestamps must be strictly increasing".into());
    }
    Ok(())
}
