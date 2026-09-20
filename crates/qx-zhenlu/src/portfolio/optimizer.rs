use crate::TargetPosition;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizerConstraint {
    pub max_positions: usize,
}

pub fn optimize(
    targets: Vec<TargetPosition>,
    constraint: &OptimizerConstraint,
) -> Vec<TargetPosition> {
    targets.into_iter().take(constraint.max_positions).collect()
}
