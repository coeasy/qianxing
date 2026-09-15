//! Pluggable risk rules.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskDecision {
    Allow,
    Reject,
    Reduce,
}

pub trait RiskRule<T> {
    fn evaluate(&self, input: &T) -> RiskDecision;
}

#[derive(Default)]
pub struct RiskRuleEngine<T> {
    rules: Vec<Box<dyn RiskRule<T>>>,
}

impl<T> RiskRuleEngine<T> {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn evaluate(&self, input: &T) -> RiskDecision {
        for rule in &self.rules {
            match rule.evaluate(input) {
                RiskDecision::Allow => {}
                decision => return decision,
            }
        }
        RiskDecision::Allow
    }
}
