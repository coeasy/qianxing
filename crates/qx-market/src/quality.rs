//! Market data quality gate.
//!
//! The quality layer validates external data before entering the kernel.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityStatus {
    Accepted,
    Rejected,
}

pub trait QualityRule<T> {
    fn check(&self, value: &T) -> QualityStatus;
}

pub struct QualityGate<T> {
    rules: Vec<Box<dyn QualityRule<T>>>,
}

impl<T> QualityGate<T> {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn add_rule<R>(&mut self, rule: R)
    where
        R: QualityRule<T> + 'static,
    {
        self.rules.push(Box::new(rule));
    }

    pub fn validate(&self, value: &T) -> QualityStatus {
        for rule in &self.rules {
            if rule.check(value) == QualityStatus::Rejected {
                return QualityStatus::Rejected;
            }
        }

        QualityStatus::Accepted
    }
}
