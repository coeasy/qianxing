//! Event reducers.

pub trait Reducer<Event, State> {
    fn reduce(&self, state: &mut State, event: &Event);
}

#[derive(Default)]
pub struct EventReducer;

impl<Event, State> Reducer<Event, State> for EventReducer {
    fn reduce(&self, _state: &mut State, _event: &Event) {
        // Domain-specific reducers are injected by higher layers.
    }
}
