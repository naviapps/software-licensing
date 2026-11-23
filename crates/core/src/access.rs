use crate::*;
use serde::{Deserialize, Serialize};

/// Host-owned native access callbacks share their value mapping here.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessInput {
    pub source: String,
    pub plan_identifier: String,
    pub operation: Operation,
    pub activation: Option<Activation>,
    pub license_key: Option<String>,
    pub has_access: Option<bool>,
    pub now: f64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessDecision {
    pub needs_access_check: bool,
    pub reply: Option<ProviderReply>,
}
pub fn access_reply(input: AccessInput) -> Result<AccessDecision, Failure> {
    valid_time(input.now)?;
    let source = identifier(&input.source)?;
    let plan = plan_identifier(&input.plan_identifier)?;
    let matches = input.activation.as_ref().is_some_and(|a| {
        a.source == source
            && a.plan_identifier == plan
            && a.license_key.is_none()
            && a.activation_identifier.is_none()
            && a.expires_at.is_none()
    });
    let reply = match input.operation {
        Operation::Activate if input.license_key.is_some() => Some(ProviderReply::Failed {
            failure: read_only_failure(ErrorCode::InvalidLicenseKey),
        }),
        Operation::Validate if !matches => Some(ProviderReply::Validated {
            validation: Validation {
                is_valid: false,
                plan_identifier: None,
                expires_at: None,
            },
        }),
        Operation::Deactivate => Some(if matches {
            ProviderReply::Deactivated
        } else {
            ProviderReply::Failed {
                failure: read_only_failure(ErrorCode::InvalidLicense),
            }
        }),
        _ => match input.has_access {
            None => None,
            Some(false) => Some(ProviderReply::Failed {
                failure: read_only_failure(ErrorCode::InvalidLicense),
            }),
            Some(true) if input.operation == Operation::Activate => {
                Some(ProviderReply::Activated {
                    activation: Activation {
                        source,
                        plan_identifier: plan,
                        activated_at: input.now,
                        license_key: None,
                        activation_identifier: None,
                        expires_at: None,
                    },
                })
            }
            Some(true) => Some(ProviderReply::Validated {
                validation: Validation {
                    is_valid: true,
                    plan_identifier: None,
                    expires_at: None,
                },
            }),
        },
    };
    Ok(AccessDecision {
        needs_access_check: reply.is_none(),
        reply,
    })
}

fn read_only_failure(code: ErrorCode) -> Failure {
    Failure {
        mutation_outcome: Some(MutationOutcome::NotApplied),
        ..Failure::new(code)
    }
}
