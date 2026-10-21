use crate::StorageError;

pub(crate) struct LeaseSnapshot {
    pub owner: String,
    pub expires_ts: u64,
    pub fencing_token: u64,
}

/// Selects the fencing token for a claim while keeping owner conflicts identical across stores.
pub(crate) fn claim_fencing_token(
    current: Option<&LeaseSnapshot>,
    owner: &str,
    now: u64,
    run_id: u64,
) -> Result<u64, StorageError> {
    let Some(current) = current else {
        return Ok(1);
    };
    if current.expires_ts > now && current.owner != owner {
        return Err(StorageError::LeaseHeld {
            run_id,
            owner: current.owner.clone(),
        });
    }
    let fencing_token = current.fencing_token.max(1);
    if current.expires_ts <= now {
        fencing_token.checked_add(1).ok_or_else(|| {
            StorageError::Conflict(format!(
                "run_id {run_id} fencing token 已达最大值，拒绝接管租约"
            ))
        })
    } else {
        Ok(fencing_token)
    }
}
