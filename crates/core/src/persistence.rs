use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;

/// Confidential intent saved before issuing a remote activation. A restart cannot
/// establish whether its request ran, so automatic activation replay is forbidden.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingActivation {
    pub license_key: Option<String>,
    pub started_at: f64,
}
impl std::fmt::Debug for PendingActivation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingActivation([redacted])")
    }
}
impl Drop for PendingActivation {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.license_key.zeroize();
    }
}

/// One atomic secure-storage record. Never put this record in preferences or logs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Record {
    pub version: u32,
    pub activation: Option<Activation>,
    pub status: Status,
    pub last_validated_at: Option<f64>,
    pub grace_period_expires_at: Option<f64>,
    pub last_refresh_failure: Option<Failure>,
    pub pending_deactivation: bool,
    #[serde(default)]
    pub observed_at: Option<f64>,
    #[serde(default)]
    pub next_refresh_at: Option<f64>,
    #[serde(default)]
    pub pending_activation: Option<PendingActivation>,
    #[serde(default)]
    pub last_operation_failure: Option<Failure>,
}
impl Default for Record {
    fn default() -> Self {
        Self {
            version: 2,
            activation: None,
            status: Status::Unlicensed,
            last_validated_at: None,
            grace_period_expires_at: None,
            last_refresh_failure: None,
            pending_deactivation: false,
            observed_at: None,
            next_refresh_at: None,
            pending_activation: None,
            last_operation_failure: None,
        }
    }
}
impl Record {
    pub fn validated(mut self) -> Result<Self, Failure> {
        if !matches!(self.version, 1 | 2) {
            return Err(Failure::new(ErrorCode::UnsupportedVersion));
        }
        if let Some(a) = self.activation.take() {
            self.activation = Some(a.normalized()?);
        }
        if let Some(attempt) = &mut self.pending_activation {
            valid_time(attempt.started_at)?;
            if let Some(key) = &mut attempt.license_key {
                *key = normalize_key(key);
                if key.is_empty() || key.len() > 4096 {
                    return Err(Failure::new(ErrorCode::InvalidData));
                }
            }
            if self.pending_deactivation {
                return Err(Failure::new(ErrorCode::InvalidData));
            }
        }
        for t in [
            self.last_validated_at,
            self.grace_period_expires_at,
            self.observed_at,
            self.next_refresh_at,
        ]
        .into_iter()
        .flatten()
        {
            valid_time(t)?;
        }
        if (self.status.is_licensed() && self.activation.is_none())
            || (self.status == Status::GracePeriod && self.grace_period_expires_at.is_none())
            || (self.status != Status::GracePeriod && self.grace_period_expires_at.is_some())
            || (self.pending_deactivation
                && (self.activation.is_none() || self.status != Status::Deactivated))
        {
            return Err(Failure::new(ErrorCode::InvalidData));
        }
        // Upgrade values in memory; initialization never replaces stored bytes.
        self.version = 2;
        self.observed_at = self.observed_at.or(self.last_validated_at);
        if self.pending_deactivation && self.last_operation_failure.is_none() {
            // Older first-release journals inherited the last validation's deadline.
            // A saved failure, however, owns its own release retry deadline.
            self.next_refresh_at = None;
        }
        Ok(self)
    }
    pub fn effective_status(&self, now: f64) -> Status {
        if valid_time(now).is_err() {
            return Status::Invalid;
        }
        if self.pending_deactivation {
            return Status::Deactivated;
        }
        if !self.status.is_licensed() {
            return self.status;
        }
        if self.last_validated_at.is_some_and(|t| t > now + 1.0) {
            return Status::Invalid;
        }
        if self.activation.as_ref().is_some_and(|a| a.expired(now)) {
            return Status::Expired;
        }
        if self.status == Status::GracePeriod
            && self.grace_period_expires_at.is_some_and(|t| t <= now)
        {
            return Status::Invalid;
        }
        self.status
    }
}

/// Decode a persisted JSON document without confusing a present null with absence.
pub fn decode_record_json(bytes: &[u8]) -> Result<Value, Failure> {
    let value: Value = decode(bytes)?;
    if value.is_null() {
        return Err(Failure::new(ErrorCode::InvalidData));
    }
    Ok(value)
}

/// Decode bytes returned by a host's storage. Only an absent host value is an
/// empty store. Arrays retain their unsupported-schema error during restore.
pub fn decode_stored_record(bytes: &[u8]) -> Result<Value, Failure> {
    // Swift custom stores could already supply UTF-16 or UTF-32 JSON via
    // Foundation. Both bridges accept these encodings and write UTF-8.
    let value = match bytes {
        [0xef, 0xbb, 0xbf, rest @ ..] => parse_stored_json(rest),
        [0xff, 0xfe, 0, 0, rest @ ..] => decode_utf32(rest, u32::from_le_bytes),
        [0, 0, 0xfe, 0xff, rest @ ..] => decode_utf32(rest, u32::from_be_bytes),
        [0xff, 0xfe, rest @ ..] => decode_utf16(rest, u16::from_le_bytes),
        [0xfe, 0xff, rest @ ..] => decode_utf16(rest, u16::from_be_bytes),
        [_, 0, 0, 0, ..] => decode_utf32(bytes, u32::from_le_bytes),
        [0, 0, 0, _, ..] => decode_utf32(bytes, u32::from_be_bytes),
        [_, 0, ..] => decode_utf16(bytes, u16::from_le_bytes),
        [0, _, ..] => decode_utf16(bytes, u16::from_be_bytes),
        _ => parse_stored_json(bytes),
    }?;
    if !matches!(value, Value::Object(_) | Value::Array(_)) {
        return Err(Failure::new(ErrorCode::StorageFailure));
    }
    // Legacy hosts normalized JSON before sending it under the command limit.
    // Count the compact UTF-8 representation without retaining a serialized copy.
    serde_json::to_writer(RecordBudget(MAX_BYTES), &value)
        .map_err(|_| Failure::new(ErrorCode::InvalidData))?;
    Ok(value)
}

struct RecordBudget(usize);

impl io::Write for RecordBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or(io::ErrorKind::FileTooLarge)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn parse_stored_json(bytes: &[u8]) -> Result<Value, Failure> {
    serde_json::from_slice(bytes).map_err(|_| Failure::new(ErrorCode::StorageFailure))
}

fn decode_utf16(bytes: &[u8], read: fn([u8; 2]) -> u16) -> Result<Value, Failure> {
    let (units, tail) = bytes.as_chunks::<2>();
    if !tail.is_empty() {
        return Err(Failure::new(ErrorCode::StorageFailure));
    }
    let text = char::decode_utf16(units.iter().copied().map(read))
        .collect::<Result<String, _>>()
        .map_err(|_| Failure::new(ErrorCode::StorageFailure))?;
    parse_stored_json(zeroize::Zeroizing::new(text).as_bytes())
}

fn decode_utf32(bytes: &[u8], read: fn([u8; 4]) -> u32) -> Result<Value, Failure> {
    let (units, tail) = bytes.as_chunks::<4>();
    if !tail.is_empty() {
        return Err(Failure::new(ErrorCode::StorageFailure));
    }
    let text = units
        .iter()
        .copied()
        .map(read)
        .map(char::from_u32)
        .collect::<Option<String>>()
        .ok_or_else(|| Failure::new(ErrorCode::StorageFailure))?;
    parse_stored_json(zeroize::Zeroizing::new(text).as_bytes())
}

/// Restore the canonical record only. Unsupported schemas never replace stored bytes.
pub fn restore(value: Option<Value>) -> Result<Record, Failure> {
    let Some(value) = value else {
        return Ok(Record::default());
    };
    if !matches!(value.get("version").and_then(Value::as_u64), Some(1 | 2)) {
        return Err(Failure::new(ErrorCode::UnsupportedVersion));
    }
    serde_json::from_value::<Record>(value)
        .map_err(|_| Failure::new(ErrorCode::InvalidData))?
        .validated()
}
