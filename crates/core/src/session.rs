use crate::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;
use std::cell::Cell;

#[derive(Deserialize)]
#[serde(
    tag = "command",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Command {
    Restore {
        policy: Policy,
        record: Option<Value>,
        now: f64,
    },
    Snapshot {
        now: f64,
    },
    NeedsRefresh {
        now: f64,
    },
    /// Authorizes replacing this idle session from storage without resetting
    /// an observed clock floor. Call again at the replacement timestamp.
    CheckReload {
        now: f64,
    },
    Activate {
        license_key: Option<String>,
        now: f64,
    },
    /// The host has already verified authority, scope and current entitlement.
    /// This command only validates values and persists them; it never calls a provider.
    AdoptExternallyVerifiedActivation {
        activation: Activation,
        now: f64,
    },
    /// The host has authoritative external proof that the saved activation did not apply.
    ConfirmActivationNotAppliedAfterExternalVerification {
        now: f64,
    },
    /// The host has authoritative external proof that the saved instance was released.
    ConfirmDeactivationAfterExternalVerification {
        now: f64,
    },
    Refresh {
        #[serde(default)]
        only_if_due: bool,
        now: f64,
    },
    Deactivate {
        now: f64,
    },
    Provider {
        id: u64,
        result: ProviderReply,
        now: f64,
    },
    Persisted {
        id: u64,
        success: bool,
        now: f64,
    },
    RetryPersistence {
        now: f64,
    },
    Cancel {
        id: u64,
        now: f64,
    },
}
#[derive(Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Effect {
    Provider {
        id: u64,
        operation: Operation,
        license_key: Option<String>,
        activation: Option<Activation>,
    },
    Persist {
        id: u64,
        record: Record,
    },
}
impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider { id, operation, .. } => f
                .debug_struct("Provider")
                .field("id", id)
                .field("operation", operation)
                .finish_non_exhaustive(),
            Self::Persist { id, .. } => f
                .debug_struct("Persist")
                .field("id", id)
                .finish_non_exhaustive(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProviderReply {
    Activated { activation: Activation },
    Validated { validation: Validation },
    Deactivated,
    Failed { failure: Failure },
}

/// An outcome from a host effect that has already run.
pub enum EffectCompletion {
    /// An invalid provider payload has no decoded reply.
    Provider(Option<ProviderReply>),
    Persisted(bool),
}
/// Final refresh decision. Validation outcomes describe access after persistence,
/// evaluated at completion time. Failures without a completed decision return an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LicenseRefreshOutcome {
    Validated,
    GracePeriod,
    Expired,
    Invalid,
    Deactivated,
    SkippedActivationInProgress,
    SkippedRefreshInProgress,
    SkippedDeactivationInProgress,
    SkippedNoActivation,
    SkippedRefreshDisabled,
    SkippedNotDue,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub state: State,
    pub effect: Option<Effect>,
    pub outcome: Option<LicenseRefreshOutcome>,
    pub error: Option<Failure>,
    pub needs_refresh: bool,
}
struct Pending {
    id: u64,
    awaiting_completion: bool,
    record: Record,
    after: Option<Effect>,
    outcome: Option<LicenseRefreshOutcome>,
    verified_at: Option<f64>,
    error: Option<Failure>,
}
struct Active {
    id: u64,
    operation: Operation,
    provider_completion: Option<ProviderReply>,
}

impl Active {
    fn new(id: u64, operation: Operation) -> Self {
        Self {
            id,
            operation,
            provider_completion: None,
        }
    }
}

/// A serialized session. No network, clock, keychain or UI dependencies.
#[derive(Default)]
pub struct Session {
    record: Record,
    policy: Policy,
    initialized: bool,
    staged_record: Option<Value>,
    sequence: u64,
    active: Option<Active>,
    pending: Option<Pending>,
    deferred_completion: Option<(u64, EffectCompletion)>,
    observed_at: Cell<Option<f64>>,
}
impl Session {
    /// Decode present storage bytes into a fresh session before the host
    /// samples time. The subsequent restore applies policy and schema checks.
    pub fn stage_stored_record(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        if self.initialized || self.staged_record.is_some() {
            return Err(Failure::new(ErrorCode::OperationInProgress));
        }
        self.staged_record = Some(decode_stored_record(bytes)?);
        Ok(())
    }

    /// Check whether a completed effect is waiting for a valid host timestamp.
    pub fn has_deferred_completion(&self) -> bool {
        self.deferred_completion.is_some()
    }

    /// Retain a native provider reply before notifying the host that its work is done.
    /// Only the initiating host may complete this receipt; it is not yet retryable.
    pub fn stage_provider_completion(
        &mut self,
        id: u64,
        reply: ProviderReply,
    ) -> Result<(), Failure> {
        if self.pending.is_some() || self.has_deferred_completion() {
            return Err(Failure::new(ErrorCode::OperationInProgress));
        }
        let active = self
            .active
            .as_mut()
            .filter(|active| active.id == id && active.provider_completion.is_none())
            .ok_or_else(|| Failure::new(ErrorCode::OperationInProgress))?;
        active.provider_completion = Some(reply);
        Ok(())
    }

    /// Complete a staged provider receipt after the initiating host observes cancellation.
    /// A cancelled validation is discarded; known mutation outcomes remain eligible to save.
    /// A missing or invalid timestamp makes this outcome eligible for `resume_completion`.
    pub fn complete_provider_effect(
        &mut self,
        id: u64,
        now: Option<f64>,
        cancelled: bool,
    ) -> Result<Reply, Failure> {
        let active = self
            .active
            .as_mut()
            .filter(|active| active.id == id)
            .ok_or_else(|| Failure::new(ErrorCode::OperationInProgress))?;
        let reply = active
            .provider_completion
            .take()
            .ok_or_else(|| Failure::new(ErrorCode::OperationInProgress))?;
        let reply = if cancelled && active.operation == Operation::Validate {
            ProviderReply::Failed {
                failure: Failure::new(ErrorCode::Cancelled),
            }
        } else {
            reply
        };
        self.complete_effect(id, EffectCompletion::Provider(Some(reply)), now)
    }

    /// Acknowledge a finished effect. A missing or invalid timestamp keeps its
    /// outcome pending so the host never has to run the effect again.
    pub fn complete_effect(
        &mut self,
        id: u64,
        completion: EffectCompletion,
        now: Option<f64>,
    ) -> Result<Reply, Failure> {
        let current = match &completion {
            EffectCompletion::Provider(_) => {
                self.pending.is_none()
                    && self.active.as_ref().is_some_and(|active| {
                        active.id == id && active.provider_completion.is_none()
                    })
            }
            EffectCompletion::Persisted(_) => self
                .pending
                .as_ref()
                .is_some_and(|p| p.id == id && p.awaiting_completion),
        };
        if !current || self.has_deferred_completion() {
            return Err(Failure::new(ErrorCode::OperationInProgress));
        }
        match now.filter(|time| valid_time(*time).is_ok()) {
            Some(now) => self.apply_completion(id, completion, now),
            None => {
                self.deferred_completion = Some((id, completion));
                Err(Failure::new(ErrorCode::InvalidData))
            }
        }
    }

    /// Apply the retained outcome. Acknowledging a failed save leaves it
    /// retryable when cancellation prevents issuing another save.
    pub fn resume_completion(
        &mut self,
        now: Option<f64>,
        cancelled: bool,
    ) -> Result<Reply, Failure> {
        let now = now
            .filter(|time| valid_time(*time).is_ok())
            .ok_or_else(|| Failure::new(ErrorCode::InvalidData))?;
        let (id, completion) = self
            .deferred_completion
            .take()
            .ok_or_else(|| Failure::new(ErrorCode::OperationInProgress))?;
        let failed_save = matches!(completion, EffectCompletion::Persisted(false));
        let mut reply = self.apply_completion(id, completion, now)?;
        if failed_save {
            if !cancelled {
                return self.dispatch(Command::RetryPersistence { now });
            }
            reply.error = Some(Failure::new(ErrorCode::Cancelled));
        }
        Ok(reply)
    }

    fn apply_completion(
        &mut self,
        id: u64,
        completion: EffectCompletion,
        now: f64,
    ) -> Result<Reply, Failure> {
        match completion {
            EffectCompletion::Persisted(success) => {
                self.dispatch(Command::Persisted { id, success, now })
            }
            EffectCompletion::Provider(result) => {
                let reply = result
                    .ok_or_else(|| Failure::new(ErrorCode::InvalidData))
                    .and_then(|result| self.dispatch(Command::Provider { id, result, now }));
                match reply {
                    Ok(reply) => Ok(reply),
                    Err(failure) => {
                        // Release the lease without dropping the mutation journal.
                        // Publish the resulting idle state with the original failure.
                        let mut reply = self.dispatch(Command::Cancel { id, now })?;
                        reply.error = Some(failure);
                        Ok(reply)
                    }
                }
            }
        }
    }

    pub fn dispatch(&mut self, command: Command) -> Result<Reply, Failure> {
        let now = match &command {
            Command::Restore { now, .. }
            | Command::Snapshot { now }
            | Command::NeedsRefresh { now }
            | Command::CheckReload { now }
            | Command::Activate { now, .. }
            | Command::AdoptExternallyVerifiedActivation { now, .. }
            | Command::ConfirmActivationNotAppliedAfterExternalVerification { now }
            | Command::ConfirmDeactivationAfterExternalVerification { now }
            | Command::Refresh { now, .. }
            | Command::Deactivate { now }
            | Command::Provider { now, .. }
            | Command::Persisted { now, .. }
            | Command::RetryPersistence { now }
            | Command::Cancel { now, .. } => *now,
        };
        valid_time(now)?;
        self.observe(now);
        if !self.initialized && !matches!(command, Command::Restore { .. }) {
            return Err(Failure::new(ErrorCode::InvalidData));
        }
        if (self.has_deferred_completion()
            || self
                .active
                .as_ref()
                .is_some_and(|active| active.provider_completion.is_some()))
            && matches!(
                command,
                Command::Provider { .. } | Command::Persisted { .. } | Command::Cancel { .. }
            )
        {
            return Err(Failure::new(ErrorCode::OperationInProgress));
        }
        let mut effect = None;
        let mut outcome = None;
        let mut error = None;
        match command {
            Command::Restore { policy, record, .. } => {
                if self.initialized {
                    return Err(Failure::new(ErrorCode::OperationInProgress));
                }
                if record.is_some() && self.staged_record.is_some() {
                    return Err(Failure::new(ErrorCode::InvalidData));
                }
                let policy = policy.validated()?;
                let record = restore(record.or_else(|| self.staged_record.take()))?;
                self.record = record;
                if let Some(observed) = self.record.observed_at {
                    self.observe(observed);
                }
                self.policy = policy;
                self.initialized = true;
            }
            Command::Snapshot { .. } | Command::NeedsRefresh { .. } => {}
            Command::CheckReload { .. } => {
                if self.active.is_some() || self.pending.is_some() {
                    return Err(Failure::new(ErrorCode::OperationInProgress));
                }
                // Even subsecond rollback could revive an observed expiration
                // if a new session discarded this in-memory clock floor.
                if self
                    .observed_at
                    .get()
                    .is_some_and(|observed| observed > now)
                {
                    return Err(Failure::new(ErrorCode::ClockRollback));
                }
            }
            Command::Activate { license_key, .. } => {
                self.require_idle()?;
                if self.record.pending_activation.is_some() {
                    return Err(Failure::new(ErrorCode::ActivationPending));
                }
                let key = license_key.map(|s| zeroize::Zeroizing::new(normalize_key(&s)));
                if key.as_ref().is_some_and(|k| k.is_empty() || k.len() > 4096) {
                    return Err(Failure::new(ErrorCode::InvalidLicenseKey));
                }
                if self.record.pending_deactivation {
                    return Err(Failure::new(ErrorCode::DeactivationPending));
                }
                if let Some(stored) = &self.record.activation
                    && let Some(stored_key) = &stored.license_key
                {
                    if key.as_ref().is_none_or(|key| key.as_str() != stored_key) {
                        return Err(Failure::new(ErrorCode::ActivationAlreadyPresent));
                    }
                    // Re-entering the saved key validates the existing instance;
                    // it never consumes another remote activation slot.
                    effect = Some(self.start_validation()?);
                } else if self
                    .record
                    .activation
                    .as_ref()
                    .is_some_and(|stored| stored.activation_identifier.is_some())
                {
                    // A keyless remote instance still needs release before replacement.
                    return Err(Failure::new(ErrorCode::ActivationAlreadyPresent));
                } else {
                    let provider = Effect::Provider {
                        id: self.next_id()?,
                        operation: Operation::Activate,
                        license_key: key.as_ref().map(|k| k.to_string()),
                        activation: None,
                    };
                    let mut record = self.record.clone();
                    record.pending_activation = Some(PendingActivation {
                        license_key: key.as_ref().map(|k| k.to_string()),
                        started_at: now,
                    });
                    record.last_operation_failure = None;
                    effect = Some(self.stage(record, Some(provider), None, false, None)?);
                }
            }
            Command::AdoptExternallyVerifiedActivation { activation, .. } => {
                self.require_idle()?;
                if self.record.pending_deactivation {
                    return Err(Failure::new(ErrorCode::DeactivationPending));
                }
                let activation = activation.normalized()?;
                if let Some(attempt) = &self.record.pending_activation
                    && attempt.license_key != activation.license_key
                {
                    return Err(Failure::new(ErrorCode::InvalidLicense));
                }
                if let Some(stored) = &self.record.activation
                    && (stored.license_key.is_some() || stored.activation_identifier.is_some())
                    && (stored.source != activation.source
                        || stored.license_key != activation.license_key
                        || stored.activation_identifier != activation.activation_identifier)
                {
                    // Verified entitlement does not release the saved remote instance.
                    return Err(Failure::new(ErrorCode::ActivationAlreadyPresent));
                }
                if activation.expired(now) {
                    return Err(Failure::new(ErrorCode::ExpiredLicense));
                }
                effect =
                    Some(self.stage(Self::activated(activation, now), None, None, true, None)?);
            }
            Command::ConfirmActivationNotAppliedAfterExternalVerification { .. } => {
                self.require_idle()?;
                if self.record.pending_activation.is_none() {
                    return Err(Failure::new(ErrorCode::InvalidData));
                }
                let mut record = self.record.clone();
                record.pending_activation = None;
                record.last_operation_failure = None;
                effect = Some(self.stage(record, None, None, false, None)?);
            }
            Command::ConfirmDeactivationAfterExternalVerification { .. } => {
                self.require_idle()?;
                if !self.record.pending_deactivation {
                    return Err(Failure::new(ErrorCode::InvalidData));
                }
                effect = Some(self.stage_deactivation()?);
            }
            Command::Refresh { only_if_due, .. } => {
                if self.record.pending_activation.is_some() && self.active.is_none() {
                    return Err(Failure::new(ErrorCode::ActivationPending));
                }
                if let Some(a) = &self.active {
                    outcome = Some(match a.operation {
                        Operation::Activate => LicenseRefreshOutcome::SkippedActivationInProgress,
                        Operation::Validate => LicenseRefreshOutcome::SkippedRefreshInProgress,
                        Operation::Deactivate => {
                            LicenseRefreshOutcome::SkippedDeactivationInProgress
                        }
                    });
                } else {
                    self.require_idle()?;
                    if self.record.pending_deactivation && only_if_due && !self.needs_refresh(now) {
                        outcome = Some(LicenseRefreshOutcome::SkippedNotDue);
                    } else if self.record.pending_deactivation {
                        // Refresh resumes a saved release; it must never validate it as active.
                        return self.dispatch(Command::Deactivate { now });
                    } else if self.record.activation.is_none() {
                        outcome = Some(LicenseRefreshOutcome::SkippedNoActivation);
                    } else if !self.policy.is_enabled {
                        outcome = Some(LicenseRefreshOutcome::SkippedRefreshDisabled);
                    } else if only_if_due && !self.needs_refresh(now) {
                        outcome = Some(LicenseRefreshOutcome::SkippedNotDue);
                    } else {
                        effect = Some(self.start_validation()?);
                    }
                }
            }
            Command::Deactivate { .. } => {
                self.require_idle()?;
                if self.record.pending_activation.is_some() {
                    return Err(Failure::new(ErrorCode::ActivationPending));
                }
                let id = self.next_id()?;
                if self.record.activation.is_none() {
                    outcome = Some(LicenseRefreshOutcome::Deactivated);
                } else {
                    let provider = Effect::Provider {
                        id,
                        operation: Operation::Deactivate,
                        license_key: None,
                        activation: self.record.activation.clone(),
                    };
                    if self.record.pending_deactivation {
                        self.active = Some(Active::new(id, Operation::Deactivate));
                        effect = Some(provider);
                    } else {
                        let mut record = self.record.clone();
                        record.status = Status::Deactivated;
                        record.pending_deactivation = true;
                        record.grace_period_expires_at = None;
                        record.last_refresh_failure = None;
                        record.last_operation_failure = None;
                        // A normal validation deadline must not delay release recovery.
                        record.next_refresh_at = None;
                        effect = Some(self.stage(record, Some(provider), None, false, None)?);
                    }
                }
            }
            Command::Provider { id, result, .. } => {
                if self.pending.is_some() || self.active.as_ref().is_none_or(|a| a.id != id) {
                    return Err(Failure::new(ErrorCode::OperationInProgress));
                }
                let operation = self.active.as_ref().unwrap().operation;
                match (operation, result) {
                    (Operation::Activate, ProviderReply::Activated { activation }) => {
                        let Ok(mut activation) = activation.normalized() else {
                            return self.provider_error_reply(id, now);
                        };
                        if let Some(requested_key) = self
                            .record
                            .pending_activation
                            .as_ref()
                            .and_then(|attempt| attempt.license_key.as_ref())
                            && activation
                                .license_key
                                .as_ref()
                                .is_some_and(|key| key != requested_key)
                        {
                            // An unrelated identity cannot resolve the journaled request.
                            return self.provider_error_reply(id, now);
                        }
                        if activation.license_key.is_none() {
                            activation.license_key = self
                                .record
                                .pending_activation
                                .as_ref()
                                .and_then(|a| a.license_key.clone());
                        }
                        // Preserve a remotely created instance even if it expires in flight.
                        // It grants no access, but must remain available for release or renewal.
                        let expired = activation.expired(now);
                        let mut record = Self::activated(activation, now);
                        if expired {
                            record.status = Status::Expired;
                        }
                        effect = Some(self.stage(record, None, None, true, None)?);
                    }
                    (Operation::Validate, ProviderReply::Validated { validation }) => {
                        let Ok(validation) = validation.validated() else {
                            return self.provider_error_reply(id, now);
                        };
                        let mut record = self.record.clone();
                        let activation = record
                            .activation
                            .as_mut()
                            .ok_or_else(|| Failure::new(ErrorCode::InvalidData))?;
                        record.last_validated_at = Some(now);
                        record.grace_period_expires_at = None;
                        record.last_refresh_failure = None;
                        record.next_refresh_at =
                            Some(self.deadline(now, self.policy.validation_interval));
                        if validation.is_valid {
                            if let Some(plan) = validation.plan_identifier {
                                activation.plan_identifier = plan;
                            }
                            if validation
                                .expires_at
                                .is_some_and(|t| t <= activation.activated_at)
                            {
                                return self.provider_error_reply(id, now);
                            }
                            activation.expires_at = validation.expires_at;
                            // Expired credentials remain available for renewal validation and deactivation.
                            record.status = if activation.expired(now) {
                                Status::Expired
                            } else {
                                Status::Active
                            };
                        } else {
                            record.status = Status::Invalid;
                        }
                        let result = Self::refresh_outcome(record.effective_status(now));
                        effect = Some(self.stage(
                            record,
                            None,
                            Some(result),
                            validation.is_valid,
                            None,
                        )?);
                    }
                    (Operation::Deactivate, ProviderReply::Deactivated) => {
                        effect = Some(self.stage_deactivation()?);
                    }
                    (_, ProviderReply::Failed { failure }) => {
                        if operation != Operation::Validate {
                            let unknown = failure.mutation_outcome() == MutationOutcome::Unknown;
                            let mut record = self.record.clone();
                            if operation == Operation::Activate && !unknown {
                                record.pending_activation = None;
                            }
                            record.last_operation_failure = Some(failure.clone());
                            if record.pending_deactivation {
                                record.next_refresh_at = Some(
                                    self.deadline(
                                        now,
                                        self.policy
                                            .failure_retry_interval
                                            .max(failure.retry_after_seconds.unwrap_or(0) as f64),
                                    ),
                                );
                            }
                            let mut reported = failure;
                            if unknown {
                                reported.code = ErrorCode::MutationOutcomeUnknown;
                                reported.mutation_outcome = Some(MutationOutcome::Unknown);
                            }
                            effect = Some(self.stage(record, None, None, false, Some(reported))?);
                        } else if failure.code == ErrorCode::Cancelled {
                            self.active = None;
                            error = Some(failure);
                        } else {
                            let record = self.validation_failure(failure, now);
                            let result = Self::refresh_outcome(record.effective_status(now));
                            effect = Some(self.stage(record, None, Some(result), false, None)?);
                        }
                    }
                    _ => return self.provider_error_reply(id, now),
                }
            }
            Command::Persisted { id, success, .. } => {
                if self
                    .pending
                    .as_ref()
                    .is_none_or(|p| p.id != id || !p.awaiting_completion)
                {
                    return Err(Failure::new(ErrorCode::OperationInProgress));
                }
                if success {
                    let pending = self.pending.take().unwrap();
                    if let Some(verified) = pending.verified_at {
                        self.observed_at.set(Some(verified.max(now)));
                    }
                    self.record = pending.record;
                    effect = pending.after;
                    outcome = pending.outcome.map(|outcome| match outcome {
                        LicenseRefreshOutcome::Validated
                        | LicenseRefreshOutcome::GracePeriod
                        | LicenseRefreshOutcome::Expired
                        | LicenseRefreshOutcome::Invalid => {
                            Self::refresh_outcome(self.snapshot(now).status)
                        }
                        other => other,
                    });
                    error = pending.error;
                    self.active = match &effect {
                        Some(Effect::Provider { id, operation, .. }) => {
                            Some(Active::new(*id, *operation))
                        }
                        _ => None,
                    };
                } else {
                    self.pending.as_mut().unwrap().awaiting_completion = false;
                    self.active = None;
                    error = Some(Failure::new(ErrorCode::StorageFailure));
                }
            }
            Command::RetryPersistence { .. } => {
                if let Some(pending) = &self.pending {
                    if pending.awaiting_completion {
                        return Err(Failure::new(ErrorCode::OperationInProgress));
                    }
                    let id = self.next_id()?;
                    let pending = self.pending.as_mut().unwrap();
                    pending.id = id;
                    pending.awaiting_completion = true;
                    effect = Some(Effect::Persist {
                        id,
                        record: pending.record.clone(),
                    });
                }
            }
            Command::Cancel { id, .. } => {
                if self.pending.is_some() {
                    return Err(Failure::new(ErrorCode::PersistencePending));
                }
                if self.active.as_ref().is_none_or(|a| a.id != id) {
                    return Err(Failure::new(ErrorCode::OperationInProgress));
                }
                self.active = None;
                error = Some(Failure::new(ErrorCode::Cancelled));
            }
        }
        Ok(Reply {
            state: self.snapshot(now),
            effect,
            outcome,
            error,
            needs_refresh: self.needs_refresh(now),
        })
    }
    fn require_idle(&self) -> Result<(), Failure> {
        if self.pending.is_some() {
            Err(Failure::new(ErrorCode::PersistencePending))
        } else if self.active.is_some() {
            Err(Failure::new(ErrorCode::OperationInProgress))
        } else {
            Ok(())
        }
    }
    fn provider_error_reply(&mut self, id: u64, now: f64) -> Result<Reply, Failure> {
        self.dispatch(Command::Provider {
            id,
            result: ProviderReply::Failed {
                failure: Failure::new(ErrorCode::UnexpectedProviderResponse),
            },
            now,
        })
    }
    fn start_validation(&mut self) -> Result<Effect, Failure> {
        let id = self.next_id()?;
        let operation = Operation::Validate;
        self.active = Some(Active::new(id, operation));
        Ok(Effect::Provider {
            id,
            operation,
            license_key: None,
            activation: self.record.activation.clone(),
        })
    }
    fn next_id(&mut self) -> Result<u64, Failure> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| Failure::new(ErrorCode::InvalidData))?;
        Ok(self.sequence)
    }
    fn stage(
        &mut self,
        record: Record,
        after: Option<Effect>,
        outcome: Option<LicenseRefreshOutcome>,
        verified: bool,
        error: Option<Failure>,
    ) -> Result<Effect, Failure> {
        // Persistence receipts belong to one write attempt, never to the provider
        // operation that preceded or follows it. Late acknowledgements cannot
        // commit another attempt or advance a later phase of deactivation.
        let id = self.next_id()?;
        let mut record = record;
        let verified_at = if verified {
            record.last_validated_at
        } else {
            None
        };
        record.observed_at = verified_at.or(self.observed_at.get());
        if verified {
            record.next_refresh_at = record
                .last_validated_at
                .map(|now| self.deadline(now, self.policy.validation_interval));
        }
        self.pending = Some(Pending {
            id,
            awaiting_completion: true,
            record: record.clone(),
            after,
            outcome,
            verified_at,
            error,
        });
        Ok(Effect::Persist { id, record })
    }
    fn activated(activation: Activation, now: f64) -> Record {
        Record {
            activation: Some(activation),
            status: Status::Active,
            last_validated_at: Some(now),
            ..Record::default()
        }
    }
    fn stage_deactivation(&mut self) -> Result<Effect, Failure> {
        self.stage(
            Record {
                status: Status::Deactivated,
                ..Record::default()
            },
            None,
            Some(LicenseRefreshOutcome::Deactivated),
            false,
            None,
        )
    }
    fn refresh_outcome(status: Status) -> LicenseRefreshOutcome {
        match status {
            Status::Active => LicenseRefreshOutcome::Validated,
            Status::GracePeriod => LicenseRefreshOutcome::GracePeriod,
            Status::Expired => LicenseRefreshOutcome::Expired,
            Status::Invalid | Status::Unlicensed => LicenseRefreshOutcome::Invalid,
            Status::Deactivated => LicenseRefreshOutcome::Deactivated,
        }
    }
    fn validation_failure(&self, failure: Failure, now: f64) -> Record {
        let mut record = self.record.clone();
        let was_licensed = record.effective_status(now).is_licensed();
        record.status = match failure.code {
            ErrorCode::ExpiredLicense => Status::Expired,
            _ => Status::Invalid,
        };
        record.last_refresh_failure = Some(failure.clone());
        let grace = match failure.code {
            ErrorCode::TransportFailure
            | ErrorCode::RequestFailure
            | ErrorCode::UnexpectedProviderResponse => Some(self.policy.failure_grace_period),
            ErrorCode::ServerFailure | ErrorCode::RateLimited => {
                Some(self.policy.server_failure_grace_period)
            }
            _ => None,
        };
        let retry_interval = if grace.is_some() {
            self.policy.failure_retry_interval
        } else {
            self.policy.validation_interval
        };
        record.next_refresh_at = Some(self.deadline(
            now,
            retry_interval.max(failure.retry_after_seconds.unwrap_or(0) as f64),
        ));
        if let Some(grace) = grace.filter(|g| *g > 0.0 && was_licensed)
            && let Some(last) = record.last_validated_at.filter(|t| *t <= now + 1.0)
        {
            let mut until = (last + grace).min(253402300799.0);
            if let Some(prior) = record.grace_period_expires_at {
                until = until.min(prior);
            }
            if let Some(expiry) = record.activation.as_ref().and_then(|a| a.expires_at) {
                until = until.min(expiry);
            }
            if until > now {
                record.status = Status::GracePeriod;
                record.grace_period_expires_at = Some(until);
            }
        }
        if record.status != Status::GracePeriod {
            record.grace_period_expires_at = None;
        }
        record
    }
    fn snapshot_record(&self, now: f64) -> Cow<'_, Record> {
        let Some(pending) = &self.pending else {
            return Cow::Borrowed(&self.record);
        };
        if pending.record.pending_deactivation {
            return Cow::Borrowed(&pending.record);
        }
        // A pending write may restrict an existing grant, but never establish or
        // renew one. Keep the committed identity/plan until storage acknowledges it.
        if !self.record.effective_status(now).is_licensed() {
            return Cow::Borrowed(&self.record);
        }
        let mut record = self.record.clone();
        let candidate = &pending.record;
        let status = candidate.effective_status(now);
        if !status.is_licensed() {
            record.status = status;
            record.grace_period_expires_at = None;
            record.last_validated_at = candidate.last_validated_at;
            record.last_refresh_failure = candidate.last_refresh_failure.clone();
        } else {
            if status == Status::GracePeriod {
                record.status = Status::GracePeriod;
                record.grace_period_expires_at = earliest_deadline(
                    record.grace_period_expires_at,
                    candidate.grace_period_expires_at,
                );
                record.last_refresh_failure = candidate.last_refresh_failure.clone();
            }
            if let (Some(current), Some(candidate)) =
                (record.activation.as_mut(), candidate.activation.as_ref())
            {
                current.expires_at = earliest_deadline(current.expires_at, candidate.expires_at);
            }
        }
        Cow::Owned(record)
    }

    pub fn snapshot(&self, now: f64) -> State {
        self.observe(now);
        let effective_now = self.observed_at.get().unwrap_or(now).max(now);
        let record = self.snapshot_record(effective_now);
        let rolled_back = now + 1.0 < effective_now;
        let status = if valid_time(now).is_err() || (rolled_back && record.status.is_licensed()) {
            Status::Invalid
        } else {
            record.effective_status(effective_now)
        };
        let licensed = status.is_licensed();
        State {
            plan: Plan {
                identifier: if licensed {
                    record.activation.as_ref().unwrap().plan_identifier.clone()
                } else {
                    "unlicensed".into()
                },
                is_licensed: licensed,
                expires_at: if licensed {
                    record.activation.as_ref().and_then(|a| a.expires_at)
                } else {
                    None
                },
            },
            source: record.activation.as_ref().map(|a| a.source.clone()),
            status,
            last_validated_at: record.last_validated_at,
            grace_period_expires_at: if status == Status::GracePeriod {
                record.grace_period_expires_at
            } else {
                None
            },
            last_refresh_failure: if (rolled_back
                || record.last_validated_at.is_some_and(|t| t > now + 1.0))
                && record.status.is_licensed()
            {
                Some(Failure::new(ErrorCode::ClockRollback))
            } else {
                record.last_refresh_failure.clone()
            },
            is_activating: self
                .active
                .as_ref()
                .is_some_and(|a| a.operation == Operation::Activate),
            is_refreshing: self
                .active
                .as_ref()
                .is_some_and(|a| a.operation == Operation::Validate),
            is_deactivating: self
                .active
                .as_ref()
                .is_some_and(|a| a.operation == Operation::Deactivate),
            has_activation: record.activation.is_some(),
            has_pending_persistence: self.pending.is_some(),
            has_pending_deactivation: record.pending_deactivation,
            next_transition_at: if licensed {
                [
                    record.activation.as_ref().and_then(|a| a.expires_at),
                    if status == Status::GracePeriod {
                        record.grace_period_expires_at
                    } else {
                        None
                    },
                ]
                .into_iter()
                .flatten()
                .filter(|t| *t > effective_now)
                .min_by(f64::total_cmp)
            } else {
                None
            },
            next_refresh_at: self.next_refresh_at(),
            has_pending_activation: record.pending_activation.is_some(),
            last_operation_failure: record.last_operation_failure.clone(),
        }
    }
    fn observe(&self, now: f64) {
        if valid_time(now).is_ok() {
            self.observed_at
                .set(Some(self.observed_at.get().map_or(now, |old| old.max(now))));
        }
    }
    fn deadline(&self, now: f64, interval: f64) -> f64 {
        (now + interval).min(253402300799.0)
    }
    fn next_refresh_at(&self) -> Option<f64> {
        if self.record.pending_deactivation {
            return self.record.next_refresh_at;
        }
        if self.record.pending_activation.is_some()
            || self.record.activation.is_none()
            || !self.policy.is_enabled
        {
            return None;
        }
        self.record.next_refresh_at.or_else(|| {
            self.record
                .last_validated_at
                .map(|last| self.deadline(last, self.policy.validation_interval))
        })
    }
    fn needs_refresh(&self, now: f64) -> bool {
        self.pending.is_some()
            || (self.record.pending_deactivation
                && self.record.next_refresh_at.is_none_or(|t| now >= t))
            || (self.record.activation.is_some()
                && self.record.pending_activation.is_none()
                && !self.record.pending_deactivation
                && self.policy.is_enabled
                && self.next_refresh_at().is_none_or(|t| now >= t))
    }
}

fn earliest_deadline(first: Option<f64>, second: Option<f64>) -> Option<f64> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (first, second) => first.or(second),
    }
}
