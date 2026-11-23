use serde::{Deserialize, Serialize};
use unicode_general_category::{GeneralCategory, get_general_category};
use zeroize::Zeroize;

/// Stable errors never contain provider bodies, keys, or arbitrary diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    InvalidData,
    /// Unsupported persisted record schema; not a native ABI mismatch.
    UnsupportedVersion,
    IncompatibleAbiVersion,
    InvalidLicenseKey,
    InvalidLicense,
    ExpiredLicense,
    ActivationLimitReached,
    ActivationAlreadyPresent,
    ActivationPending,
    MutationOutcomeUnknown,
    InvalidProviderConfiguration,
    UnexpectedProviderResponse,
    TransportFailure,
    RequestFailure,
    ServerFailure,
    RateLimited,
    StorageFailure,
    OperationInProgress,
    PersistencePending,
    DeactivationPending,
    ClockRollback,
    GracePeriodExpired,
    Cancelled,
    UnsupportedPlatform,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Failure {
    pub code: ErrorCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    /// Minimum server-requested delay, retained after one provider execution ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_outcome: Option<MutationOutcome>,
}
impl Failure {
    pub const fn new(code: ErrorCode) -> Self {
        Self {
            code,
            status_code: None,
            retry_after_seconds: None,
            mutation_outcome: None,
        }
    }
    pub fn mutation_outcome(&self) -> MutationOutcome {
        self.mutation_outcome.unwrap_or(match self.code {
            ErrorCode::TransportFailure
            | ErrorCode::ServerFailure
            | ErrorCode::UnexpectedProviderResponse
            | ErrorCode::Cancelled
            | ErrorCode::MutationOutcomeUnknown => MutationOutcome::Unknown,
            _ => MutationOutcome::NotApplied,
        })
    }
}

/// A transport cancellation cannot prove that a remote mutation did not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MutationOutcome {
    NotApplied,
    Unknown,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.code)
    }
}
impl std::error::Error for Failure {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    #[default]
    Unlicensed,
    Active,
    GracePeriod,
    Expired,
    Invalid,
    Deactivated,
}
impl Status {
    pub fn is_licensed(self) -> bool {
        matches!(self, Self::Active | Self::GracePeriod)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Operation {
    Activate,
    Validate,
    Deactivate,
}

/// Credential-bearing provider record; Debug intentionally redacts all fields.
#[derive(Clone, PartialEq, Serialize, Deserialize, Zeroize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[zeroize(drop)]
pub struct Activation {
    pub source: String,
    pub plan_identifier: String,
    pub activated_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}
impl std::fmt::Debug for Activation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Activation([redacted])")
    }
}
impl Activation {
    pub fn normalized(mut self) -> Result<Self, Failure> {
        self.source = identifier(&self.source)?;
        self.plan_identifier = plan_identifier(&self.plan_identifier)?;
        valid_time(self.activated_at)?;
        if let Some(t) = self.expires_at {
            valid_time(t)?;
            if t <= self.activated_at {
                return Err(Failure::new(ErrorCode::InvalidData));
            }
        }
        self.license_key = self
            .license_key
            .take()
            .map(|v| normalize_key(&v))
            .filter(|v| !v.is_empty());
        self.activation_identifier = self
            .activation_identifier
            .take()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        if self.license_key.as_ref().is_some_and(|s| s.len() > 4096)
            || self
                .activation_identifier
                .as_ref()
                .is_some_and(|s| s.len() > 4096)
        {
            return Err(Failure::new(ErrorCode::InvalidData));
        }
        Ok(self)
    }
    pub fn expired(&self, now: f64) -> bool {
        self.expires_at.is_some_and(|t| t <= now)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Validation {
    pub is_valid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<f64>,
}
impl Validation {
    pub fn validated(mut self) -> Result<Self, Failure> {
        if !self.is_valid && (self.plan_identifier.is_some() || self.expires_at.is_some()) {
            return Err(Failure::new(ErrorCode::UnexpectedProviderResponse));
        }
        if let Some(p) = &self.plan_identifier {
            self.plan_identifier = Some(plan_identifier(p)?);
        }
        if let Some(t) = self.expires_at {
            valid_time(t)?;
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    pub is_enabled: bool,
    pub validation_interval: f64,
    pub failure_grace_period: f64,
    pub server_failure_grace_period: f64,
    #[serde(default = "default_failure_retry_interval")]
    pub failure_retry_interval: f64,
}
fn default_failure_retry_interval() -> f64 {
    60.0
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            is_enabled: true,
            validation_interval: 86400.0,
            failure_grace_period: 604800.0,
            server_failure_grace_period: 86400.0,
            failure_retry_interval: default_failure_retry_interval(),
        }
    }
}
impl Policy {
    pub fn validated(self) -> Result<Self, Failure> {
        if [
            self.validation_interval,
            self.failure_grace_period,
            self.server_failure_grace_period,
            self.failure_retry_interval,
        ]
        .iter()
        .any(|v| !v.is_finite() || *v < 0.0 || *v > 3155760000.0)
        {
            return Err(Failure::new(ErrorCode::InvalidData));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub identifier: String,
    pub is_licensed: bool,
    pub expires_at: Option<f64>,
}

/// UI-safe snapshot. Credentials are only available through provider effects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub plan: Plan,
    pub source: Option<String>,
    pub status: Status,
    pub last_validated_at: Option<f64>,
    pub grace_period_expires_at: Option<f64>,
    pub last_refresh_failure: Option<Failure>,
    pub is_activating: bool,
    pub is_refreshing: bool,
    pub is_deactivating: bool,
    pub has_activation: bool,
    pub has_pending_persistence: bool,
    pub has_pending_deactivation: bool,
    /// Next local access deadline. Re-evaluation performs no provider request.
    pub next_transition_at: Option<f64>,
    /// Earliest automatic validation time. The host owns timers and manual refresh.
    pub next_refresh_at: Option<f64>,
    pub has_pending_activation: bool,
    pub last_operation_failure: Option<Failure>,
}

pub fn valid_time(t: f64) -> Result<(), Failure> {
    if t.is_finite() && (-62135596800.0..=253402300799.0).contains(&t) {
        Ok(())
    } else {
        Err(Failure::new(ErrorCode::InvalidData))
    }
}
pub fn identifier(value: &str) -> Result<String, Failure> {
    let value = value.trim();
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        Err(Failure::new(ErrorCode::InvalidData))
    } else {
        Ok(value.to_owned())
    }
}
pub fn plan_identifier(value: &str) -> Result<String, Failure> {
    let value = identifier(value)?;
    if value == "unlicensed" {
        Err(Failure::new(ErrorCode::InvalidData))
    } else {
        Ok(value)
    }
}
pub fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| {
            !c.is_whitespace()
                && !matches!(
                    get_general_category(*c),
                    GeneralCategory::Control | GeneralCategory::Format
                )
        })
        .collect()
}
