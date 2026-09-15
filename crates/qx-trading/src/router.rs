//! Order routing boundary.

pub trait OrderRouter<Order, Target> {
    fn route(&self, order: &Order) -> Target;
}
