//! Provider-neutral licensing. Hosts execute effects; Rust owns every decision.
mod access;
mod model;
mod persistence;
mod session;

pub use access::{AccessDecision, AccessInput, access_reply};
pub use model::*;
pub use persistence::{
    PendingActivation, Record, decode_record_json, decode_stored_record, restore,
};
pub use session::{Command, Effect, LicenseRefreshOutcome, ProviderReply, Reply, Session};

/// Maximum JSON command, normalized custom-storage record or provider response size.
pub const MAX_BYTES: usize = 1024 * 1024;

/// Decode without putting input bytes or credentials into errors.
pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Failure> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(Failure::new(ErrorCode::InvalidData));
    }
    serde_json::from_slice(bytes).map_err(|_| Failure::new(ErrorCode::InvalidData))
}
