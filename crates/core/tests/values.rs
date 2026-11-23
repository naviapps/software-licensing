use serde_json::json;
use software_licensing::{
    Activation, ErrorCode, Failure, Policy, Validation, identifier, plan_identifier,
};

fn activation() -> Activation {
    Activation {
        source: " fixture ".into(),
        plan_identifier: " paid ".into(),
        activated_at: 1000.0,
        license_key: Some(" A\u{200b} B\t\0 ".into()),
        activation_identifier: Some(" instance ".into()),
        expires_at: Some(1001.0),
    }
}

#[test]
fn identifiers_keep_exact_spelling_under_the_utf8_limit() {
    let decomposed = "Cafe\u{301}";
    assert_eq!(
        identifier(&format!(" {decomposed}\u{2003}")).unwrap(),
        decomposed
    );
    let limit = "é".repeat(2048);
    assert_eq!(identifier(&format!(" {limit} ")).unwrap(), limit);
    for rejected in [" ", "source\0inside", &format!("{limit}a")] {
        assert_eq!(
            identifier(rejected),
            Err(Failure::new(ErrorCode::InvalidData))
        );
    }
    assert_eq!(plan_identifier(" paid ").unwrap(), "paid");
    assert_eq!(plan_identifier(" Unlicensed ").unwrap(), "Unlicensed");
    assert_eq!(
        plan_identifier(" unlicensed "),
        Err(Failure::new(ErrorCode::InvalidData))
    );
}

#[test]
fn activation_normalizes_credentials_and_checks_dates_and_byte_limits() {
    let normalized = activation().normalized().unwrap();
    assert_eq!(normalized.source, "fixture");
    assert_eq!(normalized.plan_identifier, "paid");
    assert_eq!(normalized.license_key.as_deref(), Some("AB"));
    assert_eq!(
        normalized.activation_identifier.as_deref(),
        Some("instance")
    );
    assert_eq!(normalized.activated_at, 1000.0);
    assert_eq!(normalized.expires_at, Some(1001.0));
    let mut missing = activation();
    missing.license_key = Some(" \u{200b} ".into());
    missing.activation_identifier = Some(" \t ".into());
    let missing = missing.normalized().unwrap();
    assert_eq!(missing.license_key, None);
    assert_eq!(missing.activation_identifier, None);
    let mut unlicensed = activation();
    unlicensed.plan_identifier = " unlicensed ".into();
    assert_eq!(
        unlicensed.normalized(),
        Err(Failure::new(ErrorCode::InvalidData))
    );

    for (activated_at, expires_at, accepted) in [
        (-62135596800.0, Some(253402300799.0), true),
        (-62135596801.0, None, false),
        (253402300799.0, None, true),
        (253402300800.0, None, false),
        (1000.0, Some(1001.0), true),
        (1000.0, Some(1000.0), false),
        (1000.0, Some(999.0), false),
        (1000.0, Some(f64::NAN), false),
    ] {
        let mut value = activation();
        value.activated_at = activated_at;
        value.expires_at = expires_at;
        assert_eq!(
            value.normalized().err(),
            (!accepted).then_some(Failure::new(ErrorCode::InvalidData)),
            "{activated_at} / {expires_at:?}"
        );
    }

    for length in [2048, 2049] {
        let raw = format!(" {} ", "é".repeat(length));
        for key in [false, true] {
            let mut value = activation();
            if key {
                value.license_key = Some(raw.clone());
            } else {
                value.activation_identifier = Some(raw.clone());
            }
            assert_eq!(
                value.normalized().err(),
                (length > 2048).then_some(Failure::new(ErrorCode::InvalidData)),
                "length = {length}, key = {key}"
            );
        }
    }
}

#[test]
fn validation_normalizes_success_and_rejects_details_on_denial() {
    let paid = Validation {
        is_valid: true,
        plan_identifier: Some(" paid ".into()),
        expires_at: Some(253402300799.0),
    };
    assert_eq!(
        paid.validated().unwrap().plan_identifier.as_deref(),
        Some("paid")
    );
    let denied = Validation {
        is_valid: false,
        plan_identifier: None,
        expires_at: None,
    };
    assert_eq!(denied.clone().validated().unwrap(), denied);
    for (plan, expiry) in [
        (Some("paid"), None),
        (None, Some(1000.0)),
        (Some("unlicensed"), Some(f64::NAN)),
    ] {
        let result = Validation {
            plan_identifier: plan.map(str::to_owned),
            expires_at: expiry,
            ..denied.clone()
        };
        assert_eq!(
            result.validated(),
            Err(Failure::new(ErrorCode::UnexpectedProviderResponse))
        );
    }
}

#[test]
fn every_refresh_interval_shares_inclusive_bounds_and_default_retry() {
    let policy = serde_json::from_value::<Policy>(json!({
        "isEnabled": true,
        "validationInterval": 3600,
        "failureGracePeriod": 600,
        "serverFailureGracePeriod": 300,
    }))
    .unwrap();
    assert_eq!(policy.failure_retry_interval, 60.0);
    assert_eq!(Policy::default().failure_retry_interval, 60.0);
    for (value, accepted) in [
        (0.0, true),
        (3155760000.0, true),
        (-1.0, false),
        (3155760001.0, false),
        (f64::NAN, false),
        (f64::INFINITY, false),
    ] {
        for candidate in [
            Policy {
                validation_interval: value,
                ..policy.clone()
            },
            Policy {
                failure_grace_period: value,
                ..policy.clone()
            },
            Policy {
                server_failure_grace_period: value,
                ..policy.clone()
            },
            Policy {
                failure_retry_interval: value,
                ..policy.clone()
            },
        ] {
            assert_eq!(
                candidate.validated().err(),
                (!accepted).then_some(Failure::new(ErrorCode::InvalidData)),
                "{value}"
            );
        }
    }
}
