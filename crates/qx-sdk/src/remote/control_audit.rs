//! Control audit pagination and bounded convenience collection for the remote Rust client.

use super::{RemoteApiClient, RemoteApiError, RemoteTransport};

const AGGREGATE_LIMIT: usize = 10_000;

pub(super) fn get_command_audit<T: RemoteTransport>(
    client: &RemoteApiClient<T>,
) -> Result<Vec<crate::ControlAuditRecordV1>, RemoteApiError> {
    let mut records = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen_cursors = std::collections::BTreeSet::new();
    loop {
        let page = get_command_audit_page(client, cursor.as_deref(), 100, None)?;
        records.extend(page.entries.into_iter().map(|entry| entry.record));
        if records.len() > AGGREGATE_LIMIT {
            return Err(RemoteApiError::InvalidJson(
                "control audit exceeds the convenience collection limit; use pages".into(),
            ));
        }
        let Some(next) = page.next_cursor else {
            return Ok(records);
        };
        if !seen_cursors.insert(next.clone()) {
            return Err(RemoteApiError::InvalidJson(
                "control audit repeated a page cursor".into(),
            ));
        }
        cursor = Some(next);
    }
}

pub(super) fn get_command_audit_page<T: RemoteTransport>(
    client: &RemoteApiClient<T>,
    cursor: Option<&str>,
    limit: usize,
    request_id: Option<&str>,
) -> Result<crate::ControlAuditPageV1, RemoteApiError> {
    if !(1..=crate::CONTROL_AUDIT_MAX_PAGE_SIZE).contains(&limit) {
        return Err(RemoteApiError::InvalidJson(
            "control audit limit is outside the allowed range".into(),
        ));
    }
    if cursor.is_some_and(str::is_empty) {
        return Err(RemoteApiError::InvalidJson(
            "control audit cursor must be non-empty text".into(),
        ));
    }
    if request_id.is_some_and(|value| value.trim().is_empty()) {
        return Err(RemoteApiError::InvalidJson(
            "request_id must be non-empty text".into(),
        ));
    }
    if cursor.is_some() && request_id.is_some() {
        return Err(RemoteApiError::InvalidJson(
            "control audit cursor cannot be combined with request_id".into(),
        ));
    }
    if request_id.is_some() && limit < 2 {
        return Err(RemoteApiError::InvalidJson(
            "request_id lookup requires a page limit of at least 2".into(),
        ));
    }
    let path = command_audit_path(cursor, limit, request_id)?;
    let response = client.get_discovery(&path)?;
    let page: crate::ControlAuditPageV1 = serde_json::from_value(response)
        .map_err(|error| RemoteApiError::InvalidJson(error.to_string()))?;
    if page.entries.len() > limit
        || request_id.is_some_and(|expected| {
            page.has_more
                || page
                    .entries
                    .iter()
                    .any(|entry| entry.record.request_id.as_str() != expected)
        })
    {
        return Err(RemoteApiError::InvalidJson(
            "control audit page exceeds its limit or does not match request_id".into(),
        ));
    }
    Ok(page)
}

fn command_audit_path(
    cursor: Option<&str>,
    limit: usize,
    request_id: Option<&str>,
) -> Result<String, RemoteApiError> {
    let mut path = format!("/control/audit?limit={limit}");
    if let Some(request_id) = request_id {
        if request_id.trim().is_empty() {
            return Err(RemoteApiError::InvalidJson(
                "request_id must be non-empty text".into(),
            ));
        }
        path.push_str("&request_id=");
        path.push_str(&encode_query_component(request_id));
    }
    if let Some(cursor) = cursor {
        if cursor.is_empty() {
            return Err(RemoteApiError::InvalidJson(
                "control audit cursor must be non-empty text".into(),
            ));
        }
        path.push_str("&cursor=");
        path.push_str(&encode_query_component(cursor));
    }
    Ok(path)
}

fn encode_query_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}
