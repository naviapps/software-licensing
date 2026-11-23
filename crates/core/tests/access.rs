use serde_json::{Value, json};
use software_licensing::{AccessInput, Activation, ErrorCode, Failure, Operation, access_reply};

fn input(
    operation: Operation,
    activation: Option<Activation>,
    has_access: Option<bool>,
) -> AccessInput {
    AccessInput {
        source: " host-membership ".into(),
        plan_identifier: " paid ".into(),
        operation,
        activation,
        license_key: None,
        has_access,
        now: 2000.0,
    }
}

fn matching_activation() -> Activation {
    Activation {
        source: "host-membership".into(),
        plan_identifier: "paid".into(),
        activated_at: 1000.0,
        license_key: None,
        activation_identifier: None,
        expires_at: None,
    }
}

#[track_caller]
fn expect_reply(input: AccessInput, reply: Value) {
    assert_eq!(
        serde_json::to_value(access_reply(input).unwrap()).unwrap(),
        json!({ "needsAccessCheck": reply.is_null(), "reply": reply }),
    );
}

fn rejected(code: &str) -> Value {
    json!({
        "kind": "failed",
        "failure": { "code": code, "mutationOutcome": "notApplied" },
    })
}

#[test]
fn activation_checks_access_and_returns_a_current_credential_free_record() {
    let activated = json!({
        "kind": "activated",
        "activation": {
            "source": "host-membership",
            "planIdentifier": "paid",
            "activatedAt": 2000.0,
        }
    });
    for (access, expected) in [
        (None, json!(null)),
        (Some(false), rejected("invalidLicense")),
        (Some(true), activated),
    ] {
        expect_reply(input(Operation::Activate, None, access), expected);
    }
}

#[test]
fn activation_rejects_key_requests_even_if_the_host_reports_access() {
    for access in [None, Some(false), Some(true)] {
        expect_reply(
            AccessInput {
                license_key: Some("TEST-KEY".into()),
                ..input(Operation::Activate, None, access)
            },
            rejected("invalidLicenseKey"),
        );
    }
}

#[test]
fn validation_checks_access_for_matching_records_and_deactivation_never_checks_access() {
    let validated = json!({ "kind": "validated", "validation": { "isValid": true } });
    for (access, expected) in [
        (None, json!(null)),
        (Some(false), rejected("invalidLicense")),
        (Some(true), validated),
    ] {
        expect_reply(
            input(Operation::Validate, Some(matching_activation()), access),
            expected,
        );
        expect_reply(
            input(Operation::Deactivate, Some(matching_activation()), access),
            json!({ "kind": "deactivated" }),
        );
    }
}

#[test]
fn existing_callback_records_keep_the_hosts_chosen_source_and_plan() {
    let request = |operation, activation, access| AccessInput {
        source: "setapp".into(),
        plan_identifier: "setapp".into(),
        ..input(operation, activation, access)
    };
    let mut existing = matching_activation();
    existing.source = "setapp".into();
    existing.plan_identifier = "setapp".into();
    expect_reply(
        request(Operation::Validate, Some(existing.clone()), None),
        json!(null),
    );
    expect_reply(
        request(Operation::Validate, Some(existing), Some(true)),
        json!({"kind": "validated", "validation": {"isValid": true}}),
    );
    expect_reply(
        request(Operation::Activate, None, Some(true)),
        json!({
            "kind": "activated",
            "activation": {"source": "setapp", "planIdentifier": "setapp", "activatedAt": 2000.0}
        }),
    );
}

#[test]
fn validation_and_deactivation_reject_mismatched_records_without_checking_access() {
    let mut other_source = matching_activation();
    other_source.source = "other".into();
    let mut other_plan = matching_activation();
    other_plan.plan_identifier = "other".into();
    let mut key = matching_activation();
    key.license_key = Some("TEST-KEY".into());
    let mut instance = matching_activation();
    instance.activation_identifier = Some("instance".into());
    let mut expiry = matching_activation();
    expiry.expires_at = Some(4000.0);
    for activation in [
        None,
        Some(other_source),
        Some(other_plan),
        Some(key),
        Some(instance),
        Some(expiry),
    ] {
        for access in [None, Some(true)] {
            expect_reply(
                input(Operation::Validate, activation.clone(), access),
                json!({ "kind": "validated", "validation": { "isValid": false } }),
            );
            expect_reply(
                input(Operation::Deactivate, activation.clone(), access),
                rejected("invalidLicense"),
            );
        }
    }
}

#[test]
fn access_callbacks_require_a_valid_clock_source_and_plan() {
    let request = || input(Operation::Activate, None, None);
    let expect_invalid = |input| {
        assert!(
            matches!(access_reply(input), Err(error) if error == Failure::new(ErrorCode::InvalidData))
        );
    };
    for now in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 253402300800.0] {
        expect_invalid(AccessInput { now, ..request() });
    }
    for (source, plan) in [
        ("", "paid"),
        ("host-membership", "  "),
        ("host-membership", "unlicensed"),
    ] {
        expect_invalid(AccessInput {
            source: source.into(),
            plan_identifier: plan.into(),
            ..request()
        });
    }
}
