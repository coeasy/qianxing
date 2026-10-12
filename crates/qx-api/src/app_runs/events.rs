use super::{valid_run_route_id, RUN_EVENTS_ROUTE_TEMPLATE};

pub(crate) fn accepts_run_event_query(route: &str, query: &str) -> bool {
    let full_route = if query.is_empty() {
        route.to_string()
    } else {
        format!("{route}?{query}")
    };
    events_route_cursor(&full_route).is_some()
}

pub(crate) fn events_route_cursor(route: &str) -> Option<(&str, u64)> {
    let (path, query) = route.split_once('?').unwrap_or((route, ""));
    let prefix = RUN_EVENTS_ROUTE_TEMPLATE.strip_suffix("{run_id}/events")?;
    let run_id = path.strip_prefix(prefix)?.strip_suffix("/events")?;
    if !valid_run_route_id(run_id) {
        return None;
    }
    let cursor = if query.is_empty() {
        0
    } else {
        let (key, value) = query.split_once('=')?;
        if key != "after_seq" || value.contains('&') {
            return None;
        }
        value.parse().ok()?
    };
    Some((run_id, cursor))
}
