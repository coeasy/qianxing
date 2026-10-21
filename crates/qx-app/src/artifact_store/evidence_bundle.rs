//! Canonical JSON round-trip validation for persisted EvidenceBundles.

use qx_spec::FoundationDocument;

/// Serialize an EvidenceBundle and validate it through qx-spec's canonical reader.
///
/// Producers provide a diagnostic for the unlikely case that a valid in-memory value does not
/// round-trip through the published schema. Schema and serialization failures keep qx-spec's
/// original diagnostics.
pub fn serialize_verified_evidence_bundle<F>(
    bundle: &qx_spec::EvidenceBundle,
    mismatch_message: F,
) -> Result<String, String>
where
    F: FnOnce(&qx_spec::EvidenceBundle, &qx_spec::EvidenceBundle) -> String,
{
    let payload = bundle.to_json()?;
    let readback = qx_spec::EvidenceBundle::from_json(&payload)?;
    if readback != *bundle {
        return Err(mismatch_message(bundle, &readback));
    }
    Ok(payload)
}
