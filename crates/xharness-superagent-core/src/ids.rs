use sha2::{Digest, Sha256};

/// Same string-array encoding and truncation as the reference `key(...values)`.
pub fn key(values: &[&str]) -> Result<String, serde_json::Error> {
    let encoded = serde_json::to_vec(values)?;
    let digest = Sha256::digest(encoded);
    Ok(digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
