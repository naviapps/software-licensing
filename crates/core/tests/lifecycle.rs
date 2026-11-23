use serde_json::json;
use software_licensing::*;

fn call(s: &mut Session, value: serde_json::Value) -> Reply {
    s.dispatch(serde_json::from_value(value).unwrap()).unwrap()
}
fn session() -> Session {
    let mut s = Session::default();
    call(
        &mut s,
        json!({"command":"restore","policy":Policy::default(),"now":1000}),
    );
    s
}

#[test]
fn activation_intent_is_durable_before_provider_and_unknown_results_never_replay() {
    let mut s = session();
    let prepared = call(
        &mut s,
        json!({"command":"activate","licenseKey":"PRIVATE-KEY","now":1000}),
    );
    assert!(matches!(prepared.effect, Some(Effect::Persist { .. })));
    let failed = fail_persistence(&mut s, prepared, 1000.0);
    assert_eq!(failed.error.unwrap().code, ErrorCode::StorageFailure);
    let retry = call(&mut s, json!({"command":"retryPersistence","now":1000}));
    let Some(Effect::Persist { record, .. }) = &retry.effect else {
        panic!()
    };
    let before_request = serde_json::to_value(record).unwrap();
    let start = persist(&mut s, retry, 1000.0);
    let Some(Effect::Provider { id, .. }) = start.effect else {
        panic!()
    };
    let unknown = call(
        &mut s,
        json!({"command":"provider","id":id,"result":{"kind":"failed","failure":{"code":"transportFailure"}},"now":1001}),
    );
    let Some(Effect::Persist { record, .. }) = &unknown.effect else {
        panic!()
    };
    let after_failure = serde_json::to_value(record).unwrap();
    let saved = persist(&mut s, unknown, 1001.0);
    assert_eq!(saved.error.unwrap().code, ErrorCode::MutationOutcomeUnknown);
    assert!(saved.state.has_pending_activation);
    assert!(!saved.state.plan.is_licensed);
    assert_eq!(
        saved.state.last_operation_failure.unwrap().code,
        ErrorCode::TransportFailure
    );
    for record in [before_request, after_failure] {
        let mut restarted = Session::default();
        call(
            &mut restarted,
            json!({"command":"restore","policy":Policy::default(),"record":record,"now":1002}),
        );
        for command in ["activate", "refresh", "deactivate"] {
            let error = restarted
                .dispatch(serde_json::from_value(json!({"command":command,"now":1002})).unwrap())
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::ActivationPending);
        }
        assert!(!call(&mut restarted, json!({"command":"needsRefresh","now":1002})).needs_refresh);
        let resolved = call(
            &mut restarted,
            json!({"command":"confirmActivationNotAppliedAfterExternalVerification","now":1002}),
        );
        assert!(
            !persist(&mut restarted, resolved, 1002.0)
                .state
                .has_pending_activation
        );
    }
}

#[test]
fn failed_activation_result_write_preserves_mutation_evidence_until_retry_and_restore() {
    for outcome in [MutationOutcome::NotApplied, MutationOutcome::Unknown] {
        let mut session = session();
        let verified_activation = activation();
        let key = verified_activation.license_key.clone().unwrap();
        let now = 1000.0;
        let intent = call(
            &mut session,
            json!({"command":"activate","licenseKey":key,"now":now}),
        );
        let started = persist(&mut session, intent, now);
        let Some(Effect::Provider {
            id,
            operation: Operation::Activate,
            ..
        }) = started.effect
        else {
            panic!("Expected activation after saving the intent");
        };
        let original = Failure {
            status_code: Some(503),
            retry_after_seconds: Some(120),
            mutation_outcome: Some(outcome),
            ..Failure::new(ErrorCode::ServerFailure)
        };
        let completion = session
            .dispatch(Command::Provider {
                id,
                result: ProviderReply::Failed {
                    failure: original.clone(),
                },
                now,
            })
            .unwrap();
        let Some(Effect::Persist {
            id: failed_id,
            record,
        }) = &completion.effect
        else {
            panic!("Expected a write for the failed activation result");
        };
        let failed_id = *failed_id;
        let candidate = serde_json::to_vec(record).unwrap();
        let failed = fail_persistence(&mut session, completion, now);
        assert_eq!(failed.error, Some(Failure::new(ErrorCode::StorageFailure)));
        assert!(failed.state.has_pending_persistence && failed.state.has_pending_activation);
        assert!(!failed.state.plan.is_licensed && failed.effect.is_none());
        assert!(failed.state.last_operation_failure.is_none());

        let retry = session
            .dispatch(Command::RetryPersistence { now: now + 1.0 })
            .unwrap();
        let Some(Effect::Persist {
            id: retry_id,
            record,
        }) = &retry.effect
        else {
            panic!("Expected a retry write for the same result");
        };
        assert_ne!(*retry_id, failed_id);
        assert_eq!(serde_json::to_vec(record).unwrap(), candidate);
        let saved = persist(&mut session, retry, now + 1.0);
        let unknown = outcome == MutationOutcome::Unknown;
        let reported = match outcome {
            MutationOutcome::NotApplied => original.clone(),
            MutationOutcome::Unknown => Failure {
                code: ErrorCode::MutationOutcomeUnknown,
                ..original.clone()
            },
        };
        assert_eq!(saved.error, Some(reported));
        assert_eq!(saved.state.last_operation_failure.as_ref(), Some(&original));
        assert_eq!(saved.state.has_pending_activation, unknown);
        assert!(!saved.state.has_pending_persistence && saved.effect.is_none());
        let idle = session
            .dispatch(Command::RetryPersistence { now: now + 1.0 })
            .unwrap();
        assert!(idle.effect.is_none() && idle.error.is_none());

        let mut restarted = Session::default();
        restarted.stage_stored_record(&candidate).unwrap();
        let now = now + 2.0;
        let restored = call(
            &mut restarted,
            json!({"command":"restore","policy":Policy::default(),"now":now}),
        );
        assert_eq!(restored.state, saved.state);
        assert!(restored.effect.is_none() && restored.error.is_none());
        let next = restarted.dispatch(Command::Activate {
            license_key: Some(key),
            now,
        });
        if unknown {
            assert_eq!(next.unwrap_err().code, ErrorCode::ActivationPending);
            let recovered = restarted
                .dispatch(Command::AdoptExternallyVerifiedActivation {
                    activation: verified_activation,
                    now,
                })
                .unwrap();
            let accepted = persist(&mut restarted, recovered, now);
            assert!(accepted.state.plan.is_licensed && !accepted.state.has_pending_activation);
            assert!(accepted.state.last_operation_failure.is_none());
            assert!(accepted.effect.is_none());
        } else {
            let next = next.unwrap();
            assert!(matches!(next.effect, Some(Effect::Persist { .. })));
        }
    }
}

#[test]
fn cancellation_before_provider_entry_clears_only_the_unapplied_attempt() {
    let mut s = session();
    let prepared = call(
        &mut s,
        json!({"command":"activate","licenseKey":"PRIVATE-KEY","now":1000}),
    );
    let start = persist(&mut s, prepared, 1000.0);
    let Some(Effect::Provider { id, .. }) = start.effect else {
        panic!()
    };
    let cancelled = call(
        &mut s,
        json!({"command":"provider","id":id,"result":{"kind":"failed","failure":{"code":"cancelled","mutationOutcome":"notApplied"}},"now":1000}),
    );
    let saved = persist(&mut s, cancelled, 1000.0);
    assert_eq!(saved.error.unwrap().code, ErrorCode::Cancelled);
    assert!(!saved.state.has_pending_activation);
    assert!(
        call(
            &mut s,
            json!({"command":"activate","licenseKey":"PRIVATE-KEY","now":1000})
        )
        .effect
        .is_some()
    );
}

#[test]
fn externally_verified_recovery_must_match_the_pending_key() {
    let mut s = session();
    let prepared = call(
        &mut s,
        json!({"command":"activate","licenseKey":"PRIVATE-KEY","now":1000}),
    );
    let Some(Effect::Persist { record, .. }) = &prepared.effect else {
        panic!()
    };
    let record = serde_json::to_value(record).unwrap();
    persist(&mut s, prepared, 1000.0);
    let mut restarted = Session::default();
    call(
        &mut restarted,
        json!({"command":"restore","policy":Policy::default(),"record":record,"now":1001}),
    );
    let mut foreign = activation();
    foreign.license_key = Some("other-key".into());
    assert_eq!(restarted.dispatch(serde_json::from_value(json!({"command":"adoptExternallyVerifiedActivation","activation":foreign,"now":1001})).unwrap()).unwrap_err().code, ErrorCode::InvalidLicense);
    assert!(restarted.snapshot(1001.0).has_pending_activation);
    let recovered = call(
        &mut restarted,
        json!({"command":"adoptExternallyVerifiedActivation","activation":activation(),"now":1001}),
    );
    let saved = persist(&mut restarted, recovered, 1001.0);
    assert!(saved.state.plan.is_licensed);
    assert!(!saved.state.has_pending_activation);
}

#[test]
fn verified_replacement_preserves_an_occupied_remote_identity_for_release() {
    for credentials in ["key-and-instance", "key-only", "instance-only"] {
        let mut original = activation();
        if credentials == "key-only" {
            original.activation_identifier = None;
        } else if credentials == "instance-only" {
            original.license_key = None;
        }
        let mut s = session();
        let adopted = s
            .dispatch(Command::AdoptExternallyVerifiedActivation {
                activation: original.clone(),
                now: 1000.0,
            })
            .unwrap();
        persist(&mut s, adopted, 1000.0);
        let before = s.snapshot(1100.0);

        let mut different_source = original.clone();
        different_source.source = "other-provider".into();
        let mut different_key = original.clone();
        different_key.license_key = Some("OTHER-KEY".into());
        let mut different_instance = original.clone();
        different_instance.activation_identifier = Some("other-instance".into());
        let mut callback = original.clone();
        callback.license_key = None;
        callback.activation_identifier = None;
        for replacement in [
            different_source,
            different_key,
            different_instance,
            callback,
        ] {
            let error = s
                .dispatch(Command::AdoptExternallyVerifiedActivation {
                    activation: replacement,
                    now: 1100.0,
                })
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::ActivationAlreadyPresent);
            assert_eq!(s.snapshot(1100.0), before);
        }

        let release = s.dispatch(Command::Deactivate { now: 1100.0 }).unwrap();
        let Some(Effect::Persist { record, .. }) = &release.effect else {
            panic!("Expected release marker");
        };
        assert_eq!(record.activation.as_ref(), Some(&original));
        let release = persist(&mut s, release, 1100.0);
        let Some(Effect::Provider {
            operation: Operation::Deactivate,
            activation: Some(saved),
            ..
        }) = release.effect
        else {
            panic!("Expected release of the original identity");
        };
        assert_eq!(saved, original);
    }
}

#[test]
fn verified_renewal_can_update_the_same_normalized_remote_identity() {
    let mut original = activation();
    original.expires_at = Some(1100.0);
    let mut s = session();
    let adopted = s
        .dispatch(Command::AdoptExternallyVerifiedActivation {
            activation: original,
            now: 1000.0,
        })
        .unwrap();
    persist(&mut s, adopted, 1000.0);
    assert_eq!(s.snapshot(1200.0).status, Status::Expired);

    let mut renewal = activation();
    renewal.source = " test ".into();
    renewal.license_key = Some(" PRIVATE\u{200b}- KEY\n".into());
    renewal.activation_identifier = Some(" instance ".into());
    renewal.plan_identifier = "renewed-plan".into();
    renewal.activated_at = 1100.0;
    renewal.expires_at = Some(2000.0);
    let expected = renewal.clone().normalized().unwrap();
    let renewed = s
        .dispatch(Command::AdoptExternallyVerifiedActivation {
            activation: renewal,
            now: 1200.0,
        })
        .unwrap();
    let Some(Effect::Persist { record, .. }) = &renewed.effect else {
        panic!("Expected renewal persistence");
    };
    assert_eq!(record.activation.as_ref(), Some(&expected));
    let renewed = persist(&mut s, renewed, 1200.0);
    assert!(renewed.state.plan.is_licensed);
    assert_eq!(renewed.state.plan.identifier, "renewed-plan");
    assert_eq!(renewed.state.plan.expires_at, Some(2000.0));
    let release = s.dispatch(Command::Deactivate { now: 1201.0 }).unwrap();
    let release = persist(&mut s, release, 1201.0);
    let Some(Effect::Provider {
        operation: Operation::Deactivate,
        activation: Some(saved),
        ..
    }) = release.effect
    else {
        panic!("Expected release of the renewed identity");
    };
    assert_eq!(saved, expected);
}

#[test]
fn verified_replacement_can_replace_a_callback_without_remote_credentials() {
    let mut callback = activation();
    callback.source = "host-membership".into();
    callback.license_key = None;
    callback.activation_identifier = None;
    let mut s = session();
    let adopted = s
        .dispatch(Command::AdoptExternallyVerifiedActivation {
            activation: callback,
            now: 1000.0,
        })
        .unwrap();
    persist(&mut s, adopted, 1000.0);
    let replacement = s
        .dispatch(Command::AdoptExternallyVerifiedActivation {
            activation: activation(),
            now: 1100.0,
        })
        .unwrap();
    let Some(Effect::Persist { record, .. }) = &replacement.effect else {
        panic!("Expected verified replacement persistence");
    };
    assert_eq!(record.activation.as_ref(), Some(&activation()));
    let saved = persist(&mut s, replacement, 1100.0);
    assert!(saved.state.plan.is_licensed);
    assert_eq!(saved.state.source.as_deref(), Some("test"));
}

#[test]
fn a_mismatched_activation_reply_retains_the_pending_request_and_uncertain_error() {
    let mut s = session();
    let prepared = s
        .dispatch(Command::Activate {
            license_key: Some(" PRIVATE\u{200b}- KEY\n".into()),
            now: 1000.0,
        })
        .unwrap();
    let started = persist(&mut s, prepared, 1000.0);
    let Some(Effect::Provider {
        id,
        license_key,
        operation: Operation::Activate,
        ..
    }) = started.effect
    else {
        panic!("Expected activation request");
    };
    assert_eq!(license_key.as_deref(), Some("PRIVATE-KEY"));
    let mut unrelated = activation();
    unrelated.license_key = Some(" OTHER\u{200b}- KEY\n".into());
    unrelated.activation_identifier = Some("unrelated-instance".into());
    let rejected = s
        .dispatch(Command::Provider {
            id,
            result: ProviderReply::Activated {
                activation: unrelated,
            },
            now: 1001.0,
        })
        .unwrap();
    assert!(rejected.error.is_none());
    assert!(!rejected.state.plan.is_licensed && !rejected.state.has_activation);
    let Some(Effect::Persist { record, .. }) = &rejected.effect else {
        panic!("Expected durable uncertain-result metadata");
    };
    assert!(record.activation.is_none());
    let attempt = record.pending_activation.as_ref().unwrap();
    assert_eq!(attempt.license_key.as_deref(), Some("PRIVATE-KEY"));
    assert_eq!(attempt.started_at, 1000.0);
    let original_failure = Failure::new(ErrorCode::UnexpectedProviderResponse);
    assert_eq!(
        record.last_operation_failure.as_ref(),
        Some(&original_failure)
    );
    let stored = serde_json::to_value(record).unwrap();
    assert!(!stored.to_string().contains("OTHER-KEY"));
    assert!(!stored.to_string().contains("unrelated-instance"));
    let saved = persist(&mut s, rejected, 1001.0);
    let failure = saved.error.unwrap();
    assert_eq!(failure.code, ErrorCode::MutationOutcomeUnknown);
    assert_eq!(failure.mutation_outcome, Some(MutationOutcome::Unknown));
    assert!(saved.state.has_pending_activation);
    assert_eq!(
        saved.state.last_operation_failure,
        Some(original_failure.clone())
    );

    let mut restarted = Session::default();
    let restored = restarted
        .dispatch(Command::Restore {
            policy: Policy::default(),
            record: Some(stored),
            now: 1002.0,
        })
        .unwrap();
    assert!(restored.effect.is_none());
    assert!(restored.state.has_pending_activation && !restored.state.has_activation);
    assert_eq!(
        restored.state.last_operation_failure,
        Some(original_failure)
    );
    assert_eq!(
        restarted
            .dispatch(Command::Activate {
                license_key: Some("PRIVATE-KEY".into()),
                now: 1002.0,
            })
            .unwrap_err()
            .code,
        ErrorCode::ActivationPending
    );
    let recovered = restarted
        .dispatch(Command::AdoptExternallyVerifiedActivation {
            activation: activation(),
            now: 1002.0,
        })
        .unwrap();
    let saved = persist(&mut restarted, recovered, 1002.0);
    assert!(saved.state.plan.is_licensed && !saved.state.has_pending_activation);
    assert!(saved.state.last_operation_failure.is_none());
}

#[test]
fn an_activation_reply_may_omit_its_key_or_return_the_same_normalized_key() {
    for returned_key in [None, Some(" PRIVATE\u{200b}- KEY\n".to_owned())] {
        let mut s = session();
        let prepared = s
            .dispatch(Command::Activate {
                license_key: Some("PRIVATE-KEY".into()),
                now: 1000.0,
            })
            .unwrap();
        let started = persist(&mut s, prepared, 1000.0);
        let Some(Effect::Provider { id, .. }) = started.effect else {
            panic!("Expected activation request");
        };
        let mut returned = activation();
        returned.license_key = returned_key;
        let activated = s
            .dispatch(Command::Provider {
                id,
                result: ProviderReply::Activated {
                    activation: returned,
                },
                now: 1001.0,
            })
            .unwrap();
        let Some(Effect::Persist { record, .. }) = &activated.effect else {
            panic!("Expected activation persistence");
        };
        assert_eq!(record.activation.as_ref(), Some(&activation()));
        assert!(record.pending_activation.is_none());
        let saved = persist(&mut s, activated, 1001.0);
        assert!(saved.error.is_none());
        assert!(saved.state.plan.is_licensed && !saved.state.has_pending_activation);
    }
}

#[test]
fn observed_expiry_cannot_be_reversed_by_wall_clock_but_verified_renewal_recovers() {
    let mut s = session();
    let mut a = activation();
    a.expires_at = Some(1500.0);
    let r = call(
        &mut s,
        json!({"command":"adoptExternallyVerifiedActivation","activation":a,"now":1000}),
    );
    persist(&mut s, r, 1000.0);
    assert_eq!(s.snapshot(1600.0).status, Status::Expired);
    let rollback = s.snapshot(1400.0);
    assert!(!rollback.plan.is_licensed);
    assert_eq!(
        rollback.last_refresh_failure.unwrap().code,
        ErrorCode::ClockRollback
    );
    let r = validation(&mut s, true, Some(3000.0), 1400.0);
    assert!(!r.state.plan.is_licensed);
    let saved = persist(&mut s, r, 1400.0);
    assert_eq!(saved.state.status, Status::Active);
    assert_eq!(saved.state.next_transition_at, Some(3000.0));
}

#[test]
fn reload_admission_preserves_observed_expiry_until_time_or_authority_recovers() {
    let mut s = session();
    let mut a = activation();
    a.expires_at = Some(1500.0);
    let reply = call(
        &mut s,
        json!({"command":"adoptExternallyVerifiedActivation","activation":a,"now":1000}),
    );
    persist(&mut s, reply, 1000.0);
    assert_eq!(s.snapshot(1600.0).status, Status::Expired);
    for now in [1400.0, 1599.5] {
        let error = s
            .dispatch(serde_json::from_value(json!({"command":"checkReload","now":now})).unwrap())
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ClockRollback);
        assert!(!s.snapshot(now).plan.is_licensed);
    }
    let admitted = call(&mut s, json!({"command":"checkReload","now":1600}));
    assert!(admitted.effect.is_none() && !admitted.state.plan.is_licensed);
    let renewal = validation(&mut s, true, Some(3000.0), 1400.0);
    persist(&mut s, renewal, 1400.0);
    assert!(
        call(&mut s, json!({"command":"checkReload","now":1400}))
            .state
            .plan
            .is_licensed
    );
}

#[test]
fn reload_admission_never_discards_provider_or_persistence_ownership() {
    let mut s = session();
    let pending = call(&mut s, json!({"command":"activate","now":1000}));
    let rejected = |s: &mut Session| {
        let error = s
            .dispatch(serde_json::from_value(json!({"command":"checkReload","now":1000})).unwrap())
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::OperationInProgress);
    };
    rejected(&mut s);
    let ready = persist(&mut s, pending, 1000.0);
    assert!(matches!(ready.effect, Some(Effect::Provider { .. })));
    rejected(&mut s);
}

#[test]
fn explicit_refresh_queries_follow_the_inclusive_deadline_without_effects() {
    let mut s = activated();
    let deadline = s.snapshot(1000.0).next_refresh_at.unwrap();
    for (now, due) in [
        (deadline - 0.25, false),
        (deadline, true),
        (deadline + 0.25, true),
        (deadline - 0.25, false),
    ] {
        let reply = s.dispatch(Command::NeedsRefresh { now }).unwrap();
        assert_eq!(reply.needs_refresh, due);
        assert_eq!(reply.state.next_refresh_at, Some(deadline));
        assert!(reply.effect.is_none() && reply.outcome.is_none() && reply.error.is_none());
    }
    assert_eq!(
        s.dispatch(Command::CheckReload {
            now: deadline - 0.25
        })
        .unwrap_err()
        .code,
        ErrorCode::ClockRollback
    );
    assert_eq!(
        s.dispatch(Command::NeedsRefresh {
            now: 253402300800.0
        })
        .unwrap_err()
        .code,
        ErrorCode::InvalidData
    );
}

#[test]
fn validation_cooldown_survives_restore_and_keeps_the_original_grace_deadline() {
    let policy = Policy {
        validation_interval: 3600.0,
        failure_grace_period: 600.0,
        server_failure_grace_period: 300.0,
        ..Policy::default()
    };
    for (retry_after, deadline) in [(None, 2060.0), (Some(10), 2060.0), (Some(120), 2120.0)] {
        let mut original = Session::default();
        call(
            &mut original,
            json!({"command":"restore","policy":policy,"now":2000}),
        );
        let adoption = call(
            &mut original,
            json!({"command":"adoptExternallyVerifiedActivation","activation":activation(),"now":2000}),
        );
        let active = persist(&mut original, adoption, 2000.0);
        assert_eq!(active.state.next_refresh_at, Some(5600.0));
        let early = call(
            &mut original,
            json!({"command":"refresh","onlyIfDue":true,"now":2000}),
        );
        assert_eq!(early.outcome, Some(LicenseRefreshOutcome::SkippedNotDue));
        assert!(early.effect.is_none() && !early.needs_refresh);

        let request = call(&mut original, json!({"command":"refresh","now":2000}));
        let Some(Effect::Provider {
            id,
            operation: Operation::Validate,
            ..
        }) = request.effect
        else {
            panic!("Expected manual validation before the automatic deadline");
        };
        let rate_limit = Failure {
            status_code: Some(429),
            retry_after_seconds: retry_after,
            ..Failure::new(ErrorCode::RateLimited)
        };
        let failed = call(
            &mut original,
            json!({"command":"provider","id":id,"result":{"kind":"failed","failure":rate_limit},"now":2000}),
        );
        let Some(Effect::Persist { record, .. }) = &failed.effect else {
            panic!("Expected a durable validation cooldown");
        };
        assert_eq!(record.next_refresh_at, Some(deadline));
        assert_eq!(record.grace_period_expires_at, Some(2300.0));
        let stored = serde_json::to_vec(record).unwrap();
        let saved = persist(&mut original, failed, 2000.0);
        assert_eq!(saved.outcome, Some(LicenseRefreshOutcome::GracePeriod));
        assert!(!saved.needs_refresh);

        let mut restarted = Session::default();
        restarted.stage_stored_record(&stored).unwrap();
        let restored = call(
            &mut restarted,
            json!({"command":"restore","policy":policy,"now":2000}),
        );
        assert!(restored.effect.is_none() && restored.error.is_none());
        assert_eq!(restored.state, saved.state);
        for now in [deadline - 1.0, deadline - 0.25] {
            let skipped = call(
                &mut restarted,
                json!({"command":"refresh","onlyIfDue":true,"now":now}),
            );
            assert_eq!(skipped.outcome, Some(LicenseRefreshOutcome::SkippedNotDue));
            assert!(skipped.effect.is_none() && !skipped.needs_refresh);
        }
        let due = call(
            &mut restarted,
            json!({"command":"refresh","onlyIfDue":true,"now":deadline}),
        );
        let Some(Effect::Provider {
            id,
            operation: Operation::Validate,
            license_key: None,
            activation: Some(credential),
        }) = due.effect
        else {
            panic!("Expected validation of the saved instance at the exact deadline");
        };
        assert_eq!(credential, activation());
        let renewal = call(
            &mut restarted,
            json!({"command":"provider","id":id,"result":{"kind":"validated","validation":{"isValid":true}},"now":deadline}),
        );
        let committed = persist(&mut restarted, renewal, deadline);
        assert_eq!(committed.outcome, Some(LicenseRefreshOutcome::Validated));
        assert_eq!(committed.state.next_refresh_at, Some(deadline + 3600.0));
        assert_eq!(committed.state.last_refresh_failure, None);

        // Manual refresh remains an explicit request during an automatic cooldown.
        assert!(matches!(
            call(
                &mut original,
                json!({"command":"refresh","now":deadline - 1.0})
            )
            .effect,
            Some(Effect::Provider {
                operation: Operation::Validate,
                ..
            })
        ));
    }
}

#[test]
fn automatic_refresh_honors_saved_failure_cooldown_during_clock_rollback() {
    let mut s = activated();
    s.snapshot(2000.0);
    let request = call(&mut s, json!({"command":"refresh","now":1500}));
    let Some(Effect::Provider { id, .. }) = request.effect else {
        panic!("Expected manual validation during rollback");
    };
    let failed = call(
        &mut s,
        json!({"command":"provider","id":id,"result":{"kind":"failed","failure":{"code":"rateLimited","statusCode":429,"retryAfterSeconds":120}},"now":1500}),
    );
    let Some(Effect::Persist { record, .. }) = &failed.effect else {
        panic!("Expected saved cooldown");
    };
    assert_eq!(record.observed_at, Some(2000.0));
    assert_eq!(record.next_refresh_at, Some(1620.0));
    let stored = serde_json::to_value(record).unwrap();
    let saved = persist(&mut s, failed, 1500.0);
    assert_eq!(saved.state.status, Status::Invalid);
    assert!(!saved.state.plan.is_licensed);
    assert!(!saved.needs_refresh);

    let mut restarted = Session::default();
    let restored = call(
        &mut restarted,
        json!({"command":"restore","record":stored,"policy":Policy::default(),"now":1500}),
    );
    assert!(restored.effect.is_none());
    assert!(!restored.needs_refresh);
    for target in [&mut s, &mut restarted] {
        for now in [1500.0, 1619.0] {
            let eligibility = call(target, json!({"command":"needsRefresh","now":now}));
            assert!(!eligibility.needs_refresh);
            let skipped = call(
                target,
                json!({"command":"refresh","onlyIfDue":true,"now":now}),
            );
            assert_eq!(skipped.outcome, Some(LicenseRefreshOutcome::SkippedNotDue));
            assert!(skipped.effect.is_none());
            assert_eq!(skipped.state.next_refresh_at, Some(1620.0));
            assert_eq!(skipped.state.status, Status::Invalid);
            assert!(!skipped.state.plan.is_licensed);
        }
        let due = call(
            target,
            json!({"command":"refresh","onlyIfDue":true,"now":1620}),
        );
        assert!(matches!(
            due.effect,
            Some(Effect::Provider {
                operation: Operation::Validate,
                ..
            })
        ));
        assert!(!due.state.plan.is_licensed);
    }
}

#[test]
fn version_one_records_upgrade_in_memory_and_keep_the_next_due_refresh() {
    const LAST_VALIDATED_AT: f64 = 2_000.0;
    const DEFAULT_VALIDATION_INTERVAL: f64 = 86_400.0;
    const FIRST_REFRESH_DUE: f64 = LAST_VALIDATED_AT + DEFAULT_VALIDATION_INTERVAL;
    const SECOND_REFRESH_DUE: f64 = FIRST_REFRESH_DUE + DEFAULT_VALIDATION_INTERVAL;
    const RETRY_AT: f64 = FIRST_REFRESH_DUE + 1.0;

    let mut legacy_activation = activation();
    legacy_activation.source = "発行元-🧪".into();
    legacy_activation.plan_identifier = "有効-𠮷".into();
    let mut old = serde_json::to_value(Record {
        version: 1,
        activation: Some(legacy_activation.clone()),
        status: Status::Active,
        last_validated_at: Some(LAST_VALIDATED_AT),
        ..Record::default()
    })
    .unwrap();
    let fields = old.as_object_mut().unwrap();
    for name in [
        "observedAt",
        "nextRefreshAt",
        "pendingActivation",
        "lastOperationFailure",
    ] {
        fields.remove(name);
    }
    let json = serde_json::to_string(&old).unwrap();
    let utf16 = |encode: fn(u16) -> [u8; 2]| json.encode_utf16().flat_map(encode);
    let utf32 = |encode: fn(u32) -> [u8; 4]| json.chars().map(u32::from).flat_map(encode);
    for encoded in [
        json.as_bytes().to_vec(),
        [0xef, 0xbb, 0xbf].into_iter().chain(json.bytes()).collect(),
        format!("{json}{}", " ".repeat(MAX_BYTES)).into_bytes(),
        utf16(u16::to_le_bytes).collect(),
        utf16(u16::to_be_bytes).collect(),
        [0xff, 0xfe]
            .into_iter()
            .chain(utf16(u16::to_le_bytes))
            .collect(),
        [0xfe, 0xff]
            .into_iter()
            .chain(utf16(u16::to_be_bytes))
            .collect(),
        utf32(u32::to_le_bytes).collect(),
        utf32(u32::to_be_bytes).collect(),
        [0xff, 0xfe, 0, 0]
            .into_iter()
            .chain(utf32(u32::to_le_bytes))
            .collect(),
        [0, 0, 0xfe, 0xff]
            .into_iter()
            .chain(utf32(u32::to_be_bytes))
            .collect(),
    ] {
        let mut s = Session::default();
        s.stage_stored_record(&encoded).unwrap();
        let restored = call(
            &mut s,
            json!({"command":"restore","policy":Policy::default(),"now": LAST_VALIDATED_AT - 0.25}),
        );
        assert!(restored.effect.is_none() && restored.error.is_none());
        assert!(restored.state.plan.is_licensed && restored.state.has_activation);
        assert_eq!(
            restored.state.source.as_deref(),
            Some(legacy_activation.source.as_str())
        );
        assert_eq!(
            restored.state.plan.identifier,
            legacy_activation.plan_identifier
        );
        assert!(!restored.state.has_pending_activation && !restored.state.has_pending_deactivation);
        assert!(
            restored.state.last_operation_failure.is_none()
                && !restored.state.has_pending_persistence
        );

        // A v1 record without observedAt still owns the last successful validation time.
        assert_eq!(
            s.dispatch(Command::CheckReload {
                now: LAST_VALIDATED_AT - 0.125,
            })
            .unwrap_err()
            .code,
            ErrorCode::ClockRollback,
        );
        assert!(
            s.dispatch(Command::CheckReload {
                now: LAST_VALIDATED_AT,
            })
            .unwrap()
            .effect
            .is_none()
        );
        let idle_retry = call(
            &mut s,
            json!({"command":"retryPersistence","now": LAST_VALIDATED_AT + 100.0}),
        );
        assert_eq!(idle_retry.state.next_refresh_at, Some(FIRST_REFRESH_DUE));
        assert!(idle_retry.effect.is_none() && !idle_retry.needs_refresh);

        let not_due = call(
            &mut s,
            json!({"command":"refresh","onlyIfDue":true,"now": FIRST_REFRESH_DUE - 1.0}),
        );
        assert_eq!(not_due.outcome, Some(LicenseRefreshOutcome::SkippedNotDue));
        assert!(not_due.effect.is_none() && !not_due.needs_refresh);
        let due = call(
            &mut s,
            json!({"command":"refresh","onlyIfDue":true,"now": FIRST_REFRESH_DUE}),
        );
        let Some(Effect::Provider {
            id,
            operation: Operation::Validate,
            license_key: None,
            activation: Some(credential),
        }) = due.effect
        else {
            panic!("Expected validation of the retained remote instance")
        };
        assert_eq!(credential, legacy_activation);

        let validated = call(
            &mut s,
            json!({"command":"provider","id":id,"result":{"kind":"validated","validation":{"isValid":true}},"now": FIRST_REFRESH_DUE}),
        );
        let Some(Effect::Persist { record, .. }) = &validated.effect else {
            panic!("Expected the first write after the successful validation")
        };
        assert_eq!(record.version, 2);
        assert_eq!(record.activation.as_ref(), Some(&legacy_activation));
        assert_eq!(record.next_refresh_at, Some(SECOND_REFRESH_DUE));
        let candidate_bytes = serde_json::to_vec(record).unwrap();
        let failed = fail_persistence(&mut s, validated, FIRST_REFRESH_DUE);
        assert_eq!(failed.error.unwrap().code, ErrorCode::StorageFailure);
        assert!(failed.state.has_pending_persistence && failed.effect.is_none());
        assert_eq!(failed.state.next_refresh_at, Some(FIRST_REFRESH_DUE));

        let retry = call(
            &mut s,
            json!({"command":"retryPersistence","now": RETRY_AT}),
        );
        let Some(Effect::Persist { record, .. }) = &retry.effect else {
            panic!("Expected the same record without another provider request")
        };
        assert_eq!(serde_json::to_vec(record).unwrap(), candidate_bytes);
        let saved = persist(&mut s, retry, RETRY_AT);
        assert!(!saved.state.has_pending_persistence && saved.effect.is_none());
        assert_eq!(saved.state.next_refresh_at, Some(SECOND_REFRESH_DUE));

        let mut restarted = Session::default();
        restarted.stage_stored_record(&candidate_bytes).unwrap();
        let reopened = call(
            &mut restarted,
            json!({"command":"restore","policy":Policy::default(),"now": RETRY_AT}),
        );
        assert_eq!(reopened.state, saved.state);
        assert!(reopened.effect.is_none() && reopened.error.is_none());
    }
}

#[test]
fn stored_record_parse_errors_are_distinct_from_schema_errors_and_absence() {
    for bytes in [
        b"".as_slice(),
        b" \t\r\n",
        b"null",
        b" \t\r\nnull",
        b"false",
        b"42",
        br#""not a record""#,
        b"{",
        b"[] {}",
        b"{\xff}",
        b"\xff\xfe{",
        b"\xff\xfe\x00\xd8",
        b"\x00\x00\xfe\xff\xff\xff\xff\xff",
    ] {
        let error = decode_stored_record(bytes).unwrap_err();
        assert_eq!(error.code, ErrorCode::StorageFailure);
    }
    for (bytes, expected) in [
        (b"[]".as_slice(), ErrorCode::UnsupportedVersion),
        (
            br#"{"version":99}"#.as_slice(),
            ErrorCode::UnsupportedVersion,
        ),
        (br#"{"version":2}"#.as_slice(), ErrorCode::InvalidData),
    ] {
        let record = decode_stored_record(bytes).unwrap();
        assert_eq!(restore(Some(record)).unwrap_err().code, expected);
    }
    assert_eq!(
        decode_stored_record(&vec![b' '; MAX_BYTES + 1])
            .unwrap_err()
            .code,
        ErrorCode::StorageFailure
    );
    assert!(restore(None).is_ok());
}

#[test]
fn stored_record_budget_counts_compact_utf8() {
    for record in [
        Record {
            version: 1,
            activation: Some(activation()),
            status: Status::Active,
            ..Record::default()
        },
        Record {
            status: Status::Deactivated,
            ..Record::default()
        },
    ] {
        let json = serde_json::to_string(&record).unwrap();
        let spaces = " ".repeat(MAX_BYTES);
        for padded in [
            format!("{json}{spaces}"),
            format!("{{{spaces}{}", &json[1..]),
        ] {
            let value = decode_stored_record(padded.as_bytes()).unwrap();
            let restored = restore(Some(value)).unwrap();
            assert_eq!(restored.status, record.status);
            assert_eq!(restored.activation, record.activation);
        }
    }
    let available = MAX_BYTES - br#"{"extra":""}"#.len();
    let mut at_limit = format!(r#"{{"extra":"{}"}}"#, "x".repeat(available));
    assert_eq!(at_limit.len(), MAX_BYTES);
    assert!(decode_stored_record(at_limit.as_bytes()).is_ok());
    at_limit.insert(at_limit.len() - 2, 'x');
    assert_eq!(
        decode_stored_record(at_limit.as_bytes()).unwrap_err().code,
        ErrorCode::InvalidData
    );
}

#[test]
fn staged_storage_is_applied_once_at_the_restore_timestamp() {
    let record = Record {
        activation: Some(activation()),
        status: Status::Active,
        last_validated_at: Some(1000.0),
        ..Record::default()
    };
    let bytes = serde_json::to_vec(&record).unwrap();
    let mut session = Session::default();
    session.stage_stored_record(&bytes).unwrap();
    assert_eq!(
        session.stage_stored_record(b"null").unwrap_err().code,
        ErrorCode::OperationInProgress
    );
    let conflicting = Command::Restore {
        policy: Policy::default(),
        record: Some(serde_json::to_value(Record::default()).unwrap()),
        now: 1000.0,
    };
    assert_eq!(
        session.dispatch(conflicting).unwrap_err().code,
        ErrorCode::InvalidData
    );
    let restored = session
        .dispatch(Command::Restore {
            policy: Policy::default(),
            record: None,
            now: 1000.0,
        })
        .unwrap();
    assert!(restored.state.plan.is_licensed && restored.effect.is_none());
    assert_eq!(
        session.stage_stored_record(&bytes).unwrap_err().code,
        ErrorCode::OperationInProgress
    );
}

fn activation() -> Activation {
    Activation {
        source: "test".into(),
        plan_identifier: "paid".into(),
        activated_at: 900.0,
        license_key: Some("PRIVATE-KEY".into()),
        activation_identifier: Some("instance".into()),
        expires_at: None,
    }
}
fn persist(s: &mut Session, r: Reply, now: f64) -> Reply {
    let Some(Effect::Persist { id, record }) = r.effect else {
        panic!("Expected persistence");
    };
    // Every emitted secure record must restore without repairs.
    assert!(restore(Some(serde_json::to_value(record).unwrap())).is_ok());
    call(
        s,
        json!({"command":"persisted","id":id,"success":true,"now":now}),
    )
}
fn activated() -> Session {
    let mut s = session();
    let r = call(
        &mut s,
        json!({"command":"adoptExternallyVerifiedActivation","activation":activation(),"now":1000}),
    );
    persist(&mut s, r, 1000.0);
    s
}
fn refresh_failure(s: &mut Session, code: ErrorCode, now: f64) -> Reply {
    let r = call(s, json!({"command":"refresh","now":now}));
    let Some(Effect::Provider { id, .. }) = r.effect else {
        panic!("Expected provider");
    };
    let r = call(
        s,
        json!({"command":"provider","id":id,"result":{"kind":"failed","failure":{"code":code}},"now":now}),
    );
    persist(s, r, now)
}

fn validation(s: &mut Session, valid: bool, expires_at: Option<f64>, now: f64) -> Reply {
    let request = call(s, json!({"command":"refresh","now":now}));
    let Some(Effect::Provider { id, .. }) = request.effect else {
        panic!("Expected validation");
    };
    call(
        s,
        json!({"command":"provider","id":id,"result":{"kind":"validated","validation":{"isValid":valid,"expiresAt":expires_at}},"now":now}),
    )
}

fn fail_persistence(s: &mut Session, reply: Reply, now: f64) -> Reply {
    let Some(Effect::Persist { id, .. }) = reply.effect else {
        panic!("Expected persistence");
    };
    call(
        s,
        json!({"command":"persisted","id":id,"success":false,"now":now}),
    )
}

#[test]
fn verified_release_requires_a_pending_deactivation_and_an_idle_session() {
    for mut store in [session(), activated()] {
        let before = store.snapshot(1100.0);
        assert_eq!(
            store
                .dispatch(Command::ConfirmDeactivationAfterExternalVerification { now: 1100.0 })
                .unwrap_err()
                .code,
            ErrorCode::InvalidData
        );
        assert_eq!(store.snapshot(1100.0), before);
    }

    let mut store = activated();
    let marker = store.dispatch(Command::Deactivate { now: 1100.0 }).unwrap();
    assert_eq!(
        store
            .dispatch(Command::ConfirmDeactivationAfterExternalVerification { now: 1100.0 })
            .unwrap_err()
            .code,
        ErrorCode::PersistencePending
    );
    let release = persist(&mut store, marker, 1100.0);
    assert!(matches!(
        release.effect,
        Some(Effect::Provider {
            operation: Operation::Deactivate,
            ..
        })
    ));
    assert_eq!(
        store
            .dispatch(Command::ConfirmDeactivationAfterExternalVerification { now: 1100.0 })
            .unwrap_err()
            .code,
        ErrorCode::OperationInProgress
    );
}

#[test]
fn verified_release_retries_only_persistence_and_clears_the_saved_evidence_after_acknowledgement() {
    let mut store = activated();
    let marker = store.dispatch(Command::Deactivate { now: 1100.0 }).unwrap();
    let release = persist(&mut store, marker, 1100.0);
    let Some(Effect::Provider {
        id,
        operation: Operation::Deactivate,
        ..
    }) = release.effect
    else {
        panic!("Expected the saved release marker to enable a provider call");
    };
    let uncertainty = store
        .dispatch(Command::Provider {
            id,
            result: ProviderReply::Failed {
                failure: Failure::new(ErrorCode::ServerFailure),
            },
            now: 1101.0,
        })
        .unwrap();
    let Some(Effect::Persist { record, .. }) = &uncertainty.effect else {
        panic!("Expected to save the uncertain release");
    };
    let saved_failure = serde_json::to_vec(record).unwrap();
    let completed = persist(&mut store, uncertainty, 1101.0);
    assert_eq!(
        completed.error.unwrap().code,
        ErrorCode::MutationOutcomeUnknown
    );
    assert!(completed.effect.is_none());

    let mut restored = Session::default();
    restored.stage_stored_record(&saved_failure).unwrap();
    let opening = restored
        .dispatch(Command::Restore {
            policy: Policy::default(),
            record: None,
            now: 1102.0,
        })
        .unwrap();
    assert!(opening.effect.is_none() && opening.state.has_pending_deactivation);
    assert_eq!(
        opening.state.last_operation_failure,
        Some(Failure::new(ErrorCode::ServerFailure))
    );
    let decision = restored
        .dispatch(Command::ConfirmDeactivationAfterExternalVerification { now: 1102.0 })
        .unwrap();
    let Some(Effect::Persist { id: first, record }) = &decision.effect else {
        panic!("Expected only a confirmation write");
    };
    let first = *first;
    assert_eq!(
        *record,
        Record {
            status: Status::Deactivated,
            observed_at: record.observed_at,
            ..Record::default()
        }
    );
    let confirmation = serde_json::to_vec(record).unwrap();
    let failed = fail_persistence(&mut restored, decision, 1102.0);
    assert_eq!(failed.error.unwrap().code, ErrorCode::StorageFailure);
    assert!(failed.effect.is_none() && failed.state.has_pending_persistence);
    assert!(failed.state.has_pending_deactivation && failed.state.has_activation);
    assert_eq!(
        failed.state.last_operation_failure,
        opening.state.last_operation_failure
    );
    assert!(!failed.state.plan.is_licensed);

    let retry = restored
        .dispatch(Command::RetryPersistence { now: 1103.0 })
        .unwrap();
    let Some(Effect::Persist { id: second, record }) = &retry.effect else {
        panic!("Expected to retry the same write without a provider call");
    };
    assert_ne!(first, *second);
    assert_eq!(serde_json::to_vec(record).unwrap(), confirmation);
    assert_eq!(
        restored
            .dispatch(Command::Persisted {
                id: first,
                success: true,
                now: 1103.0
            })
            .unwrap_err()
            .code,
        ErrorCode::OperationInProgress
    );
    let saved = persist(&mut restored, retry, 1103.0);
    assert_eq!(saved.outcome, Some(LicenseRefreshOutcome::Deactivated));
    assert!(saved.effect.is_none() && !saved.state.has_pending_persistence);
    assert!(!saved.state.has_pending_deactivation && !saved.state.has_activation);
    assert_eq!(saved.state.last_operation_failure, None);

    let mut reopened = Session::default();
    reopened.stage_stored_record(&confirmation).unwrap();
    let checked = reopened
        .dispatch(Command::Restore {
            policy: Policy::default(),
            record: None,
            now: 1104.0,
        })
        .unwrap();
    assert_eq!(checked.state, saved.state);
    assert!(checked.effect.is_none());
}

#[test]
fn rejected_validation_restricts_access_before_and_after_failed_persistence() {
    let mut s = activated();
    let rejected = validation(&mut s, false, None, 1100.0);
    assert!(!rejected.state.plan.is_licensed);
    assert_eq!(rejected.state.status, Status::Invalid);
    assert!(rejected.state.has_activation);
    let failed = fail_persistence(&mut s, rejected, 1100.0);
    assert_eq!(failed.error.unwrap().code, ErrorCode::StorageFailure);
    assert!(!failed.state.plan.is_licensed);
    assert!(!s.snapshot(700000.0).plan.is_licensed);
    let retry = call(&mut s, json!({"command":"retryPersistence","now":1101}));
    assert!(!retry.state.plan.is_licensed);
    assert!(!persist(&mut s, retry, 1101.0).state.plan.is_licensed);
}

#[test]
fn failed_persistence_cannot_remove_the_validation_failure_grace_deadline() {
    for (expires_at, deadline, after_status, after_outcome) in [
        (
            None,
            605_800.0,
            Status::Invalid,
            LicenseRefreshOutcome::Invalid,
        ),
        (
            Some(1_200.0),
            1_200.0,
            Status::Expired,
            LicenseRefreshOutcome::Expired,
        ),
    ] {
        for (now, still_licensed) in [
            (deadline - 1.0, true),
            (deadline, false),
            (deadline + 1.0, false),
        ] {
            let mut s = session();
            let mut original = activation();
            original.expires_at = expires_at;
            let adopted = call(
                &mut s,
                json!({"command":"adoptExternallyVerifiedActivation","activation":original,"now":1000}),
            );
            persist(&mut s, adopted, 1000.0);
            let request = call(&mut s, json!({"command":"refresh","now":1100}));
            let Some(Effect::Provider {
                id,
                operation: Operation::Validate,
                ..
            }) = request.effect
            else {
                panic!("Expected validation")
            };
            let pending = call(
                &mut s,
                json!({"command":"provider","id":id,"result":{"kind":"failed","failure":{"code":"transportFailure"}},"now":1100}),
            );
            let Some(Effect::Persist { record, .. }) = &pending.effect else {
                panic!("Expected a grace record to save")
            };
            assert_eq!(record.grace_period_expires_at, Some(deadline));
            let original_record = serde_json::to_value(record).unwrap();
            assert_eq!(pending.state.status, Status::GracePeriod);
            assert_eq!(pending.state.next_transition_at, Some(deadline));
            let failed = fail_persistence(&mut s, pending, 1100.0);
            assert_eq!(failed.error.unwrap().code, ErrorCode::StorageFailure);
            assert!(failed.state.plan.is_licensed && failed.state.has_pending_persistence);
            assert_eq!(failed.state.grace_period_expires_at, Some(deadline));

            let expected_status = if still_licensed {
                Status::GracePeriod
            } else {
                after_status
            };
            let expected_outcome = if still_licensed {
                LicenseRefreshOutcome::GracePeriod
            } else {
                after_outcome
            };
            let before_retry = s.snapshot(now);
            assert_eq!(before_retry.status, expected_status);
            assert_eq!(before_retry.plan.is_licensed, still_licensed);
            assert_eq!(
                before_retry.next_transition_at,
                still_licensed.then_some(deadline)
            );
            let first_retry = call(&mut s, json!({"command":"retryPersistence","now":now}));
            let failed_again = fail_persistence(&mut s, first_retry, now);
            assert_eq!(failed_again.error.unwrap().code, ErrorCode::StorageFailure);
            assert_eq!(failed_again.state, before_retry);

            let second_retry = call(&mut s, json!({"command":"retryPersistence","now":now}));
            let Some(Effect::Persist { record, .. }) = &second_retry.effect else {
                panic!("Retry must save without re-running validation")
            };
            let saved_record = serde_json::to_value(record).unwrap();
            assert_eq!(saved_record, original_record);
            let saved = persist(&mut s, second_retry, now);
            assert_eq!(saved.outcome, Some(expected_outcome));
            assert_eq!(saved.state.status, expected_status);
            assert_eq!(saved.state.plan.is_licensed, still_licensed);
            assert!(!saved.state.has_pending_persistence && saved.effect.is_none());

            let mut restarted = Session::default();
            let reopened = call(
                &mut restarted,
                json!({"command":"restore","policy":Policy::default(),"record":saved_record,"now":now}),
            );
            assert_eq!(reopened.state, saved.state);
            assert!(reopened.effect.is_none());
            assert_eq!(restarted.snapshot(deadline).status, after_status);
        }
    }
}

#[test]
fn pending_renewal_cannot_extend_the_committed_access_deadline() {
    let mut s = session();
    let mut original = activation();
    original.expires_at = Some(2000.0);
    let initial = call(
        &mut s,
        json!({"command":"adoptExternallyVerifiedActivation","activation":original,"now":1000}),
    );
    persist(&mut s, initial, 1000.0);
    let pending = validation(&mut s, true, Some(3000.0), 1100.0);
    assert_eq!(pending.state.plan.expires_at, Some(2000.0));
    let failed = fail_persistence(&mut s, pending, 1100.0);
    assert_eq!(failed.state.plan.expires_at, Some(2000.0));
    assert!(!s.snapshot(2000.0).plan.is_licensed);
    let retry = call(&mut s, json!({"command":"retryPersistence","now":2500}));
    assert!(!retry.state.plan.is_licensed);
    let saved = persist(&mut s, retry, 2500.0);
    assert!(saved.state.plan.is_licensed);
    assert_eq!(saved.state.plan.expires_at, Some(3000.0));
}

#[test]
fn pending_validation_can_shorten_but_not_lift_access_limits() {
    let mut s = activated();
    let pending = validation(&mut s, true, Some(1500.0), 1100.0);
    assert_eq!(pending.state.plan.expires_at, Some(1500.0));
    fail_persistence(&mut s, pending, 1100.0);
    assert!(!s.snapshot(1500.0).plan.is_licensed);

    let mut s = activated();
    let grace = refresh_failure(&mut s, ErrorCode::TransportFailure, 1100.0);
    assert_eq!(grace.state.status, Status::GracePeriod);
    let pending = validation(&mut s, true, None, 1200.0);
    assert_eq!(pending.state.status, Status::GracePeriod);
    assert_eq!(pending.state.next_transition_at, Some(605800.0));
    fail_persistence(&mut s, pending, 1200.0);
    assert!(!s.snapshot(605800.0).plan.is_licensed);
}

#[test]
fn public_snapshots_fail_closed_for_invalid_clock_values() {
    let s = activated();
    for now in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 253402300800.0] {
        let state = s.snapshot(now);
        assert_eq!(state.status, Status::Invalid);
        assert!(!state.plan.is_licensed);
        assert_eq!(state.next_transition_at, None);
    }
}
#[test]
fn ui_snapshot_never_contains_credentials() {
    let mut s = activated();
    let r = call(&mut s, json!({"command":"snapshot","now":1001}));
    let data = serde_json::to_string(&r).unwrap();
    assert!(!data.contains("PRIVATE-KEY"));
    assert!(!data.contains("instance"));
    assert!(r.state.plan.is_licensed);
}
#[test]
fn persistence_failure_keeps_candidate_and_prevents_another_activation() {
    let mut s = session();
    let r = call(
        &mut s,
        json!({"command":"adoptExternallyVerifiedActivation","activation":activation(),"now":1000}),
    );
    let Some(Effect::Persist { id, .. }) = r.effect else {
        panic!()
    };
    let r = call(
        &mut s,
        json!({"command":"persisted","id":id,"success":false,"now":1000}),
    );
    assert!(!r.state.plan.is_licensed);
    assert!(r.state.has_pending_persistence);
    assert_eq!(r.error.unwrap().code, ErrorCode::StorageFailure);
    let e = s
        .dispatch(
            serde_json::from_value(json!({"command":"activate","licenseKey":"new","now":1000}))
                .unwrap(),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PersistencePending);
    let r = call(&mut s, json!({"command":"retryPersistence","now":1001}));
    let r = persist(&mut s, r, 1001.0);
    assert!(r.state.plan.is_licensed);
}

#[test]
fn persistence_retry_cannot_duplicate_an_outstanding_write() {
    let mut s = session();
    let r = call(
        &mut s,
        json!({"command":"adoptExternallyVerifiedActivation","activation":activation(),"now":1000}),
    );
    let error = s
        .dispatch(Command::RetryPersistence { now: 1000.0 })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::OperationInProgress);
    assert!(persist(&mut s, r, 1000.0).state.plan.is_licensed);
}

#[test]
fn failed_write_receipt_cannot_later_commit_the_candidate() {
    let mut s = session();
    let r = call(
        &mut s,
        json!({"command":"adoptExternallyVerifiedActivation","activation":activation(),"now":1000}),
    );
    let Some(Effect::Persist { id, .. }) = r.effect else {
        panic!("Expected persistence");
    };
    call(
        &mut s,
        json!({"command":"persisted","id":id,"success":false,"now":1000}),
    );
    let error = s
        .dispatch(Command::Persisted {
            id,
            success: true,
            now: 1001.0,
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::OperationInProgress);
    assert!(s.snapshot(1001.0).has_pending_persistence);
    assert!(!s.snapshot(1001.0).plan.is_licensed);
}

#[test]
fn retry_receipts_are_unique_and_release_only_the_matching_saved_candidate() {
    let mut s = activated();
    let marker = call(&mut s, json!({"command":"deactivate","now":1100}));
    let Some(Effect::Persist { id: first, record }) = &marker.effect else {
        panic!("Expected persistence before release");
    };
    let first = *first;
    assert!(record.pending_deactivation && record.activation.is_some());
    let marker_bytes = serde_json::to_vec(record).unwrap();
    let failed = fail_persistence(&mut s, marker, 1100.0);
    assert_eq!(failed.error, Some(Failure::new(ErrorCode::StorageFailure)));
    assert!(failed.effect.is_none() && failed.state.has_pending_persistence);
    assert!(!failed.state.plan.is_licensed);
    let retry = call(&mut s, json!({"command":"retryPersistence","now":1101}));
    let Some(Effect::Persist { id: second, record }) = &retry.effect else {
        panic!("Expected retry");
    };
    let second = *second;
    assert_ne!(first, second);
    assert_eq!(serde_json::to_vec(record).unwrap(), marker_bytes);
    assert_eq!(
        s.dispatch(Command::RetryPersistence { now: 1101.0 })
            .unwrap_err()
            .code,
        ErrorCode::OperationInProgress
    );
    for success in [true, false] {
        let error = s
            .dispatch(Command::Persisted {
                id: first,
                success,
                now: 1101.0,
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::OperationInProgress);
    }
    assert!(s.snapshot(1101.0).has_pending_persistence);
    assert!(!s.snapshot(1101.0).plan.is_licensed);
    let r = persist(&mut s, retry, 1101.0);
    let Some(Effect::Provider {
        id: release,
        operation: Operation::Deactivate,
        activation: Some(saved),
        ..
    }) = r.effect
    else {
        panic!("Expected one release after the saved marker");
    };
    assert_eq!(saved.activation_identifier.as_deref(), Some("instance"));
    let r = call(
        &mut s,
        json!({"command":"provider","id":release,"result":{"kind":"deactivated"},"now":1102}),
    );
    let Some(Effect::Persist {
        id: final_write,
        record,
    }) = &r.effect
    else {
        panic!("Expected persistence after release");
    };
    let final_write = *final_write;
    assert_ne!(first, final_write);
    assert_ne!(release, final_write);
    assert_ne!(second, final_write);
    assert!(!record.pending_deactivation && record.activation.is_none());
    let final_bytes = serde_json::to_vec(record).unwrap();
    let error = s
        .dispatch(Command::Persisted {
            id: first,
            success: true,
            now: 1102.0,
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::OperationInProgress);
    let failed = fail_persistence(&mut s, r, 1102.0);
    assert_eq!(failed.error, Some(Failure::new(ErrorCode::StorageFailure)));
    assert!(failed.effect.is_none() && failed.state.has_pending_persistence);
    assert!(failed.state.has_pending_deactivation && !failed.state.is_deactivating);
    assert!(!failed.state.plan.is_licensed);

    let retry = call(&mut s, json!({"command":"retryPersistence","now":1103}));
    let Some(Effect::Persist {
        id: final_retry,
        record,
    }) = &retry.effect
    else {
        panic!("A confirmed release retries storage without requesting release again");
    };
    assert_ne!(*final_retry, final_write);
    assert_eq!(serde_json::to_vec(record).unwrap(), final_bytes);
    let saved = persist(&mut s, retry, 1103.0);
    assert!(!saved.state.has_activation);
    assert!(!saved.state.has_pending_persistence);
    assert!(!saved.state.has_pending_deactivation);
    assert!(saved.effect.is_none() && saved.error.is_none());
    let idle = call(&mut s, json!({"command":"retryPersistence","now":1104}));
    assert!(idle.effect.is_none());

    let mut restarted = Session::default();
    restarted.stage_stored_record(&final_bytes).unwrap();
    let restored = call(
        &mut restarted,
        json!({"command":"restore","policy":Policy::default(),"now":1104}),
    );
    assert_eq!(restored.state, saved.state);
    assert!(restored.effect.is_none() && restored.error.is_none());
    assert!(!restored.needs_refresh);
}
#[test]
fn concurrent_operation_and_late_completion_cannot_replace_state() {
    let mut s = activated();
    let r = call(&mut s, json!({"command":"refresh","now":1000}));
    let Some(Effect::Provider { id, .. }) = r.effect else {
        panic!()
    };
    let r = call(&mut s, json!({"command":"refresh","now":1000}));
    assert_eq!(
        r.outcome,
        Some(LicenseRefreshOutcome::SkippedRefreshInProgress)
    );
    call(&mut s, json!({"command":"cancel","id":id,"now":1000}));
    assert!(s.dispatch(serde_json::from_value(json!({"command":"provider","id":id,"result":{"kind":"validated","validation":{"isValid":false}},"now":1000})).unwrap()).is_err());
    assert!(s.snapshot(1000.0).plan.is_licensed);
}
#[test]
fn grace_is_anchored_to_last_success_and_never_extended() {
    let mut s = activated();
    let r = refresh_failure(&mut s, ErrorCode::TransportFailure, 2000.0);
    assert_eq!(r.state.grace_period_expires_at, Some(605800.0));
    let r = refresh_failure(&mut s, ErrorCode::TransportFailure, 3000.0);
    assert_eq!(r.state.grace_period_expires_at, Some(605800.0));
    assert!(!s.snapshot(605800.0).plan.is_licensed);
    let r = refresh_failure(&mut s, ErrorCode::TransportFailure, 605801.0);
    assert!(!r.state.plan.is_licensed);
}
#[test]
fn authoritative_rejection_and_clock_rollback_never_get_grace() {
    for code in [
        ErrorCode::InvalidLicense,
        ErrorCode::ExpiredLicense,
        ErrorCode::ActivationLimitReached,
        ErrorCode::InvalidProviderConfiguration,
    ] {
        let mut s = activated();
        assert!(!refresh_failure(&mut s, code, 2000.0).state.plan.is_licensed);
    }
    let mut s = activated();
    let r = refresh_failure(&mut s, ErrorCode::TransportFailure, 990.0);
    assert!(!r.state.plan.is_licensed);
}
#[test]
fn rejection_cannot_be_resurrected_by_subsequent_network_failure() {
    let mut s = activated();
    refresh_failure(&mut s, ErrorCode::InvalidLicense, 2000.0);
    let r = refresh_failure(&mut s, ErrorCode::TransportFailure, 2001.0);
    assert!(!r.state.plan.is_licensed);
    assert!(r.state.has_activation);
}
#[test]
fn deactivation_is_durable_before_network_and_resumes_after_failure() {
    let mut s = activated();
    let r = call(&mut s, json!({"command":"deactivate","now":1100}));
    assert!(!r.state.plan.is_licensed);
    let r = persist(&mut s, r, 1100.0);
    let Some(Effect::Provider {
        id,
        operation: Operation::Deactivate,
        ..
    }) = r.effect
    else {
        panic!()
    };
    let r = call(
        &mut s,
        json!({"command":"provider","id":id,"result":{"kind":"failed","failure":{"code":"transportFailure"}},"now":1100}),
    );
    assert!(r.state.has_pending_deactivation);
    let r = persist(&mut s, r, 1100.0);
    assert_eq!(r.error.unwrap().code, ErrorCode::MutationOutcomeUnknown);
    let r = call(&mut s, json!({"command":"refresh","now":1200}));
    let Some(Effect::Provider { id, .. }) = r.effect else {
        panic!()
    };
    let r = call(
        &mut s,
        json!({"command":"provider","id":id,"result":{"kind":"deactivated"},"now":1200}),
    );
    let r = persist(&mut s, r, 1200.0);
    assert_eq!(r.outcome, Some(LicenseRefreshOutcome::Deactivated));
    assert!(!r.state.has_activation);
}
#[test]
fn interrupted_deactivation_resumes_immediately_after_restore() {
    let mut original = activated();
    let normal_validation = original.snapshot(1100.0).next_refresh_at.unwrap();
    assert!(normal_validation > 1100.0);

    let prepared = call(&mut original, json!({"command":"deactivate","now":1100}));
    let Some(Effect::Persist { record, .. }) = &prepared.effect else {
        panic!("Expected a durable release marker before contacting the provider");
    };
    assert!(record.pending_deactivation);
    assert_eq!(record.next_refresh_at, None);
    let current = serde_json::to_value(record).unwrap();
    let mut legacy = current.clone();
    legacy["nextRefreshAt"] = json!(normal_validation);

    for stored in [current, legacy] {
        let mut restarted = Session::default();
        let restored = call(
            &mut restarted,
            json!({"command":"restore","record":stored,"policy":Policy::default(),"now":1101}),
        );
        assert!(restored.effect.is_none());
        assert!(restored.state.has_pending_deactivation && !restored.state.plan.is_licensed);
        assert_eq!(restored.state.next_refresh_at, None);
        assert!(
            restored.needs_refresh,
            "A release without a reply must be due at restart"
        );
        let resumed = call(
            &mut restarted,
            json!({"command":"refresh","onlyIfDue":true,"now":1101}),
        );
        assert!(matches!(
            resumed.effect,
            Some(Effect::Provider {
                operation: Operation::Deactivate,
                activation: Some(_),
                ..
            })
        ));
    }
}

#[test]
fn release_cooldown_survives_restore_and_releases_the_same_instance_when_due() {
    for (failure, deadline) in [
        (Failure::new(ErrorCode::TransportFailure), 1160.0),
        (
            Failure {
                status_code: Some(429),
                retry_after_seconds: Some(120),
                ..Failure::new(ErrorCode::RateLimited)
            },
            1220.0,
        ),
        (
            Failure {
                retry_after_seconds: Some(90),
                ..Failure::new(ErrorCode::TransportFailure)
            },
            1190.0,
        ),
    ] {
        let mut original = activated();
        let prepared = call(&mut original, json!({"command":"deactivate","now":1100}));
        let started = persist(&mut original, prepared, 1100.0);
        let Some(Effect::Provider {
            id,
            operation: Operation::Deactivate,
            ..
        }) = started.effect
        else {
            panic!("Expected a release request after persistence");
        };
        let failed = call(
            &mut original,
            json!({"command":"provider","id":id,"result":{"kind":"failed","failure":failure},"now":1100}),
        );
        let Some(Effect::Persist { record, .. }) = &failed.effect else {
            panic!("Expected a durable release retry deadline");
        };
        let stored = serde_json::to_vec(record).unwrap();
        assert_eq!(record.next_refresh_at, Some(deadline));
        assert_eq!(record.last_operation_failure.as_ref(), Some(&failure));
        let saved = persist(&mut original, failed, 1100.0);
        let reported = match failure.mutation_outcome() {
            MutationOutcome::Unknown => Failure {
                code: ErrorCode::MutationOutcomeUnknown,
                mutation_outcome: Some(MutationOutcome::Unknown),
                ..failure
            },
            MutationOutcome::NotApplied => failure,
        };
        assert_eq!(saved.error, Some(reported));
        assert!(saved.state.has_pending_deactivation && !saved.state.plan.is_licensed);

        let mut restarted = Session::default();
        restarted.stage_stored_record(&stored).unwrap();
        let restored = call(
            &mut restarted,
            json!({"command":"restore","policy":Policy::default(),"now":1100}),
        );
        assert!(restored.effect.is_none() && restored.error.is_none() && !restored.needs_refresh);
        assert_eq!(restored.state, saved.state);
        for now in [deadline - 1.0, deadline - 0.25] {
            let skipped = call(
                &mut restarted,
                json!({"command":"refresh","onlyIfDue":true,"now":now}),
            );
            assert_eq!(skipped.outcome, Some(LicenseRefreshOutcome::SkippedNotDue));
            assert!(skipped.effect.is_none() && !skipped.needs_refresh);
        }
        let due = call(
            &mut restarted,
            json!({"command":"refresh","onlyIfDue":true,"now":deadline}),
        );
        let Some(Effect::Provider {
            id,
            operation: Operation::Deactivate,
            license_key: None,
            activation: Some(credential),
        }) = due.effect
        else {
            panic!("Expected release of the saved instance at the exact deadline");
        };
        assert_eq!(credential, activation());
        let finished = call(
            &mut restarted,
            json!({"command":"provider","id":id,"result":{"kind":"deactivated"},"now":deadline}),
        );
        let committed = persist(&mut restarted, finished, deadline);
        assert_eq!(committed.outcome, Some(LicenseRefreshOutcome::Deactivated));
        assert!(!committed.state.has_activation && !committed.state.has_pending_deactivation);
        assert_eq!(committed.state.next_refresh_at, None);
    }
}

#[test]
fn expired_credential_can_be_validated_after_subscription_renewal() {
    let mut s = session();
    let mut a = activation();
    a.expires_at = Some(1100.0);
    let r = call(
        &mut s,
        json!({"command":"adoptExternallyVerifiedActivation","activation":a,"now":1000}),
    );
    persist(&mut s, r, 1000.0);
    assert_eq!(s.snapshot(1200.0).status, Status::Expired);
    let r = call(&mut s, json!({"command":"refresh","now":1200}));
    let Some(Effect::Provider { id, .. }) = r.effect else {
        panic!()
    };
    let r = call(
        &mut s,
        json!({"command":"provider","id":id,"result":{"kind":"validated","validation":{"isValid":true,"expiresAt":2000}},"now":1200}),
    );
    assert!(persist(&mut s, r, 1200.0).state.plan.is_licensed);
}
#[test]
fn snapshot_equality_preserves_the_exact_spelling_of_source_and_plan_identifiers() {
    let snapshot = |source: &str, plan: &str| {
        let mut s = session();
        let mut activation = activation();
        activation.source = source.into();
        activation.plan_identifier = plan.into();
        let adopted = s
            .dispatch(Command::AdoptExternallyVerifiedActivation {
                activation,
                now: 1000.0,
            })
            .unwrap();
        persist(&mut s, adopted, 1000.0).state
    };
    let composed = "caf\u{00e9}";
    let decomposed = "cafe\u{0301}";
    let original = snapshot(composed, composed);
    let source_change = snapshot(decomposed, composed);
    let plan_change = snapshot(composed, decomposed);
    assert_ne!(original.source, source_change.source);
    assert_eq!(original.plan, source_change.plan);
    assert_eq!(original.source, plan_change.source);
    assert_ne!(original.plan, plan_change.plan);
    assert_ne!(original, source_change);
    assert_ne!(original, plan_change);
}

#[test]
fn unsupported_and_corrupt_secure_records_are_never_replaced() {
    assert_eq!(
        restore(Some(json!({"version":99}))).unwrap_err().code,
        ErrorCode::UnsupportedVersion
    );
    assert!(restore(Some(json!({"version":1}))).is_err());
    assert!(!format!("{:?}", activation()).contains("PRIVATE-KEY"));
    assert_eq!(normalize_key(" A\u{200b}\nB\tC\u{feff} "), "ABC");
}

#[test]
fn repeated_key_validates_existing_instance_and_replacement_requires_deactivation() {
    let mut s = activated();
    let r = call(
        &mut s,
        json!({"command":"activate","licenseKey":"PRIVATE-KEY","now":1000}),
    );
    assert!(matches!(
        r.effect,
        Some(Effect::Provider {
            operation: Operation::Validate,
            ..
        })
    ));
    let Some(Effect::Provider { id, .. }) = r.effect else {
        panic!()
    };
    call(&mut s, json!({"command":"cancel","id":id,"now":1000}));
    let error = s
        .dispatch(
            serde_json::from_value(
                json!({"command":"activate","licenseKey":"another-key","now":1000}),
            )
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ActivationAlreadyPresent);
}

#[test]
fn instance_only_activation_rejects_replacement_and_retains_its_release_identity() {
    let mut original = activation();
    original.license_key = None;
    let mut s = session();
    let adopted = s
        .dispatch(Command::AdoptExternallyVerifiedActivation {
            activation: original.clone(),
            now: 1000.0,
        })
        .unwrap();
    persist(&mut s, adopted, 1000.0);
    let before = s.snapshot(1001.0);
    for license_key in [None, Some("NEW-KEY".to_owned())] {
        let error = s
            .dispatch(Command::Activate {
                license_key,
                now: 1001.0,
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ActivationAlreadyPresent);
        assert_eq!(s.snapshot(1001.0), before);
    }
    let release = s.dispatch(Command::Deactivate { now: 1001.0 }).unwrap();
    let Some(Effect::Persist { record, .. }) = &release.effect else {
        panic!("Expected original instance in the release marker");
    };
    assert_eq!(record.activation.as_ref(), Some(&original));
    let release = persist(&mut s, release, 1001.0);
    let Some(Effect::Provider {
        operation: Operation::Deactivate,
        activation: Some(saved),
        ..
    }) = release.effect
    else {
        panic!("Expected release of the original instance");
    };
    assert_eq!(saved, original);
}

#[test]
fn local_deadlines_are_reported_and_expire_without_provider_or_storage_effects() {
    let mut s = session();
    let mut a = activation();
    a.expires_at = Some(2000.0);
    let r = call(
        &mut s,
        json!({"command":"adoptExternallyVerifiedActivation","activation":a,"now":1000}),
    );
    let r = persist(&mut s, r, 1000.0);
    assert_eq!(r.state.next_transition_at, Some(2000.0));
    let r = call(&mut s, json!({"command":"snapshot","now":2000}));
    assert_eq!(r.state.status, Status::Expired);
    assert!(r.state.has_activation);
    assert_eq!(r.state.next_transition_at, None);
    assert!(r.effect.is_none());

    let mut s = activated();
    let r = refresh_failure(&mut s, ErrorCode::TransportFailure, 1001.0);
    let deadline = r.state.grace_period_expires_at.unwrap();
    assert_eq!(r.state.next_transition_at, Some(deadline));
    let r = call(&mut s, json!({"command":"snapshot","now":deadline}));
    assert!(!r.state.plan.is_licensed);
    assert_eq!(r.state.next_transition_at, None);
    assert!(r.effect.is_none());
}

#[test]
fn malformed_validation_uses_bounded_failure_policy_and_clears_busy_state() {
    let mut s = activated();
    let r = call(&mut s, json!({"command":"refresh","now":1001}));
    let Some(Effect::Provider { id, .. }) = r.effect else {
        panic!()
    };
    let r = call(
        &mut s,
        json!({"command":"provider","id":id,
        "result":{"kind":"validated","validation":{"isValid":true,"expiresAt":800}},"now":1001}),
    );
    let r = persist(&mut s, r, 1001.0);
    assert!(!r.state.is_refreshing);
    assert_eq!(r.state.status, Status::GracePeriod);
    assert_eq!(
        r.state.last_refresh_failure.unwrap().code,
        ErrorCode::UnexpectedProviderResponse
    );
}

#[test]
fn activation_expiring_in_flight_retains_its_instance_without_granting_access() {
    let mut s = session();
    let r = call(
        &mut s,
        json!({"command":"activate","licenseKey":"PRIVATE-KEY","now":1000}),
    );
    let r = persist(&mut s, r, 1000.0);
    let Some(Effect::Provider { id, .. }) = r.effect else {
        panic!()
    };
    let mut a = activation();
    a.expires_at = Some(1500.0);
    let r = call(
        &mut s,
        json!({"command":"provider","id":id,
        "result":{"kind":"activated","activation":a},"now":1501}),
    );
    let r = persist(&mut s, r, 1501.0);
    assert_eq!(r.state.status, Status::Expired);
    assert!(r.state.has_activation);
    assert!(!r.state.plan.is_licensed);
    assert!(!r.state.is_activating);
    let r = call(&mut s, json!({"command":"deactivate","now":1501}));
    let r = persist(&mut s, r, 1501.0);
    assert!(matches!(
        r.effect,
        Some(Effect::Provider {
            operation: Operation::Deactivate,
            ..
        })
    ));
}

#[test]
fn refresh_outcome_wire_vocabulary_is_closed() {
    use LicenseRefreshOutcome::*;
    for (outcome, wire) in [
        (Validated, "validated"),
        (GracePeriod, "gracePeriod"),
        (Expired, "expired"),
        (Invalid, "invalid"),
        (Deactivated, "deactivated"),
        (SkippedActivationInProgress, "skippedActivationInProgress"),
        (SkippedRefreshInProgress, "skippedRefreshInProgress"),
        (
            SkippedDeactivationInProgress,
            "skippedDeactivationInProgress",
        ),
        (SkippedNoActivation, "skippedNoActivation"),
        (SkippedRefreshDisabled, "skippedRefreshDisabled"),
        (SkippedNotDue, "skippedNotDue"),
    ] {
        assert_eq!(serde_json::to_value(outcome).unwrap(), json!(wire));
        assert_eq!(
            serde_json::from_value::<LicenseRefreshOutcome>(json!(wire)).unwrap(),
            outcome
        );
    }
    for value in [
        json!("refreshed"),
        json!("activated"),
        json!("futureOutcome"),
        json!(0),
    ] {
        assert!(serde_json::from_value::<LicenseRefreshOutcome>(value).is_err());
    }
}

#[test]
fn refresh_skips_describe_the_actual_admission_decision() {
    use LicenseRefreshOutcome::*;
    let mut s = session();
    let reply = call(&mut s, json!({"command":"refresh","now":1000}));
    assert_eq!(reply.outcome, Some(SkippedNoActivation));
    assert!(reply.effect.is_none());
    let prepared = call(&mut s, json!({"command":"activate","now":1000}));
    persist(&mut s, prepared, 1000.0);
    assert_eq!(
        call(&mut s, json!({"command":"refresh","now":1000})).outcome,
        Some(SkippedActivationInProgress)
    );

    let mut s = activated();
    call(&mut s, json!({"command":"refresh","now":1000}));
    assert_eq!(
        call(&mut s, json!({"command":"refresh","now":1000})).outcome,
        Some(SkippedRefreshInProgress)
    );

    let mut s = activated();
    let release = call(&mut s, json!({"command":"deactivate","now":1000}));
    persist(&mut s, release, 1000.0);
    assert_eq!(
        call(&mut s, json!({"command":"refresh","now":1000})).outcome,
        Some(SkippedDeactivationInProgress)
    );

    let mut s = Session::default();
    let record = Record {
        activation: Some(activation()),
        status: Status::Active,
        ..Record::default()
    };
    let policy = Policy {
        is_enabled: false,
        ..Policy::default()
    };
    call(
        &mut s,
        json!({"command":"restore","policy":policy,"record":record,"now":1000}),
    );
    assert_eq!(
        call(&mut s, json!({"command":"refresh","now":1000})).outcome,
        Some(SkippedRefreshDisabled)
    );
}

#[test]
fn validation_outcomes_follow_the_committed_access_decision() {
    use LicenseRefreshOutcome::*;
    for (valid, expires, expected) in [
        (true, None, Validated),
        (false, None, Invalid),
        (true, Some(1050.0), Expired),
    ] {
        let mut s = activated();
        let pending = validation(&mut s, valid, expires, 1100.0);
        assert!(pending.outcome.is_none());
        assert_eq!(persist(&mut s, pending, 1100.0).outcome, Some(expected));
    }
    for (code, expected) in [
        (ErrorCode::TransportFailure, GracePeriod),
        (ErrorCode::InvalidLicense, Invalid),
        (ErrorCode::ExpiredLicense, Expired),
    ] {
        assert_eq!(
            refresh_failure(&mut activated(), code, 1100.0).outcome,
            Some(expected)
        );
    }
    let mut s = activated();
    let pending = validation(&mut s, true, Some(1200.0), 1100.0);
    assert_eq!(persist(&mut s, pending, 1200.0).outcome, Some(Expired));

    let mut s = activated();
    let pending = validation(&mut s, true, None, 1100.0);
    let failed = fail_persistence(&mut s, pending, 1100.0);
    assert!(failed.outcome.is_none());
    assert_eq!(failed.error.unwrap().code, ErrorCode::StorageFailure);
}

#[test]
fn final_refresh_outcome_matches_the_clock_aware_snapshot_after_persistence() {
    for retry_save in [false, true] {
        let mut s = activated();
        s.snapshot(2000.0);
        let request = call(&mut s, json!({"command":"refresh","now":1500}));
        let Some(Effect::Provider { id, .. }) = request.effect else {
            panic!("Expected validation");
        };
        let pending = call(
            &mut s,
            json!({"command":"provider","id":id,"result":{"kind":"failed","failure":{"code":"transportFailure"}},"now":1500}),
        );
        assert!(pending.outcome.is_none());
        assert_eq!(pending.state.status, Status::Invalid);
        let (pending, completed_at) = if retry_save {
            let failed = fail_persistence(&mut s, pending, 1500.0);
            assert_eq!(failed.error.unwrap().code, ErrorCode::StorageFailure);
            assert!(failed.outcome.is_none());
            (
                call(&mut s, json!({"command":"retryPersistence","now":1501})),
                1501.0,
            )
        } else {
            (pending, 1500.0)
        };
        let saved = persist(&mut s, pending, completed_at);
        assert_eq!(saved.state.status, Status::Invalid);
        assert!(!saved.state.plan.is_licensed && !saved.state.has_pending_persistence);
        assert_eq!(saved.outcome, Some(LicenseRefreshOutcome::Invalid));
        assert_eq!(saved.state, s.snapshot(completed_at));
        assert_eq!(
            saved.state.last_refresh_failure.unwrap().code,
            ErrorCode::ClockRollback
        );
    }
}

#[test]
fn existing_record_failure_codes_remain_readable_independently_of_abi() {
    let record = json!({
        "version":1, "activation":activation(), "status":"invalid",
        "lastValidatedAt":1000.0, "gracePeriodExpiresAt":null,
        "lastRefreshFailure":{"code":"unsupportedVersion"}, "pendingDeactivation":false
    });
    let restored = restore(Some(record.clone())).unwrap();
    assert_eq!(
        restored.last_refresh_failure.as_ref().unwrap().code,
        ErrorCode::UnsupportedVersion
    );
    assert_eq!(restored.version, 2);
    assert_eq!(
        serde_json::to_value(restored.activation).unwrap(),
        record["activation"]
    );
    assert_eq!(
        serde_json::to_value(ErrorCode::IncompatibleAbiVersion).unwrap(),
        json!("incompatibleAbiVersion")
    );
}
