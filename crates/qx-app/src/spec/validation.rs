//! Shared validation helpers for application spec identities.

/// Identities become output filenames. Reject path separators instead of sanitizing them,
/// which could silently map two distinct run IDs to the same artifact.
pub(super) fn is_identity_token(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
}
