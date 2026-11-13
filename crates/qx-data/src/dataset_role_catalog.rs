//! Supplier-signed, versioned instrument-role catalogs for live admission.

use crate::DatasetInstrumentRole;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DATASET_ROLE_CATALOG_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetInstrumentRoleCatalogV1 {
    pub schema_version: u32,
    pub catalog_id: String,
    pub supplier_id: String,
    pub source_version: String,
    pub issued_at_ms: u64,
    pub valid_from_ms: u64,
    pub valid_until_ms: u64,
    pub roles: BTreeMap<String, DatasetInstrumentRole>,
    pub key_id: String,
    pub signature_hex: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetRoleTrustedKeyV1 {
    pub supplier_id: String,
    pub public_key_hex: String,
}

#[derive(Serialize)]
struct UnsignedCatalog<'a> {
    schema_version: u32,
    catalog_id: &'a str,
    supplier_id: &'a str,
    source_version: &'a str,
    issued_at_ms: u64,
    valid_from_ms: u64,
    valid_until_ms: u64,
    roles: &'a BTreeMap<String, DatasetInstrumentRole>,
    key_id: &'a str,
}

impl DatasetInstrumentRoleCatalogV1 {
    pub fn new_unsigned_dataset_role_catalog_v1(
        catalog_id: String,
        supplier_id: String,
        source_version: String,
        issued_at_ms: u64,
        valid_from_ms: u64,
        valid_until_ms: u64,
        roles: BTreeMap<String, DatasetInstrumentRole>,
    ) -> Self {
        Self {
            schema_version: DATASET_ROLE_CATALOG_SCHEMA_VERSION,
            catalog_id,
            supplier_id,
            source_version,
            issued_at_ms,
            valid_from_ms,
            valid_until_ms,
            roles,
            key_id: "0".repeat(64),
            signature_hex: "0".repeat(128),
        }
    }

    pub fn ed25519_public_key_hex(seed: &[u8; 32]) -> Result<String, String> {
        let key_pair = Ed25519KeyPair::from_seed_unchecked(seed)
            .map_err(|_| "invalid Ed25519 seed for dataset role catalog".to_owned())?;
        Ok(encode_hex(key_pair.public_key().as_ref()))
    }

    pub fn sign_with_ed25519_seed(mut self, seed: &[u8; 32]) -> Result<Self, String> {
        self.key_id = "0".repeat(64);
        self.signature_hex = "0".repeat(128);
        self.validate_shape()?;
        let key_pair = Ed25519KeyPair::from_seed_unchecked(seed)
            .map_err(|_| "invalid Ed25519 seed for dataset role catalog".to_owned())?;
        let public_key = key_pair.public_key().as_ref();
        self.key_id = crate::sha256_hex_bytes(public_key);
        self.signature_hex = encode_hex(key_pair.sign(&self.signing_message()?).as_ref());
        Ok(self)
    }

    pub fn validate_for_instrument(
        &self,
        instrument: &str,
        expected_role: DatasetInstrumentRole,
        trusted_public_keys: &BTreeMap<String, DatasetRoleTrustedKeyV1>,
        at_ms: u64,
    ) -> Result<(), String> {
        self.validate_shape()?;
        if instrument.trim().is_empty() || !self.roles.contains_key(instrument) {
            return Err(format!(
                "dataset role catalog {} does not bind instrument {instrument}",
                self.catalog_id
            ));
        }
        let role = self.roles[instrument];
        if role == DatasetInstrumentRole::Unknown || role != expected_role {
            return Err(format!(
                "dataset role catalog role {:?} conflicts with market spec role {:?} for {instrument}",
                role, expected_role
            ));
        }
        if at_ms < self.valid_from_ms || at_ms >= self.valid_until_ms {
            return Err(format!(
                "dataset role catalog {} is not valid at timestamp {at_ms}",
                self.catalog_id
            ));
        }
        if self.issued_at_ms > at_ms {
            return Err("dataset role catalog was issued after the admission timestamp".into());
        }
        let trusted_key = trusted_public_keys
            .get(&self.key_id)
            .ok_or_else(|| format!("dataset role catalog signer {} is not trusted", self.key_id))?;
        if trusted_key.supplier_id != self.supplier_id {
            return Err(format!(
                "dataset role catalog supplier {} does not match trusted signer supplier {}",
                self.supplier_id, trusted_key.supplier_id
            ));
        }
        let public_key = decode_hex(&trusted_key.public_key_hex)?;
        let signature = decode_hex(&self.signature_hex)?;
        if public_key.len() != 32 || signature.len() != 64 {
            return Err("dataset role catalog Ed25519 key/signature has invalid length".into());
        }
        if crate::sha256_hex_bytes(&public_key) != self.key_id {
            return Err("dataset role catalog key_id does not match its trusted public key".into());
        }
        UnparsedPublicKey::new(&ED25519, public_key)
            .verify(&self.signing_message()?, &signature)
            .map_err(|_| "dataset role catalog Ed25519 signature is invalid".to_owned())
    }

    pub fn content_sha256(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("serialize dataset role catalog: {error}"))?;
        Ok(crate::sha256_hex_bytes(&bytes))
    }

    fn validate_shape(&self) -> Result<(), String> {
        if self.schema_version != DATASET_ROLE_CATALOG_SCHEMA_VERSION {
            return Err(format!(
                "unsupported dataset role catalog schema_version {}; expected {}",
                self.schema_version, DATASET_ROLE_CATALOG_SCHEMA_VERSION
            ));
        }
        if self.catalog_id.trim().is_empty()
            || self.supplier_id.trim().is_empty()
            || self.source_version.trim().is_empty()
            || self.issued_at_ms == 0
            || self.valid_from_ms == 0
            || self.valid_from_ms >= self.valid_until_ms
            || self.issued_at_ms > self.valid_until_ms
            || self.roles.is_empty()
            || self.key_id.len() != 64
            || self.key_id != self.key_id.to_ascii_lowercase()
            || !self.key_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.signature_hex.len() != 128
            || !self
                .signature_hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(
                "dataset role catalog metadata, validity, roles, or signature is invalid".into(),
            );
        }
        for (instrument, role) in &self.roles {
            let parsed = qx_core::InstrumentId::parse(instrument)
                .ok_or_else(|| format!("invalid dataset role catalog instrument {instrument}"))?;
            if parsed.to_string() != *instrument || *role == DatasetInstrumentRole::Unknown {
                return Err(format!(
                    "dataset role catalog instrument must be canonical and role known: {instrument}"
                ));
            }
        }
        Ok(())
    }

    /// Return the compact, field-ordered JSON payload covered by the Ed25519 signature.
    pub fn signing_payload(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&UnsignedCatalog {
            schema_version: self.schema_version,
            catalog_id: &self.catalog_id,
            supplier_id: &self.supplier_id,
            source_version: &self.source_version,
            issued_at_ms: self.issued_at_ms,
            valid_from_ms: self.valid_from_ms,
            valid_until_ms: self.valid_until_ms,
            roles: &self.roles,
            key_id: &self.key_id,
        })
        .map_err(|error| format!("serialize dataset role catalog signing payload: {error}"))
    }

    fn signing_message(&self) -> Result<Vec<u8>, String> {
        self.signing_payload()
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("dataset role catalog key/signature must be hexadecimal".into());
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair)
                .map_err(|_| "dataset role catalog key/signature must be hexadecimal".to_owned())?;
            u8::from_str_radix(text, 16)
                .map_err(|_| "dataset role catalog key/signature must be hexadecimal".to_owned())
        })
        .collect()
}
