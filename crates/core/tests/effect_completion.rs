use proptest::prelude::*;
use software_licensing::{
    Activation, Command, Effect, EffectCompletion, ErrorCode, Failure, LicenseRefreshOutcome,
    Operation, Policy, ProviderReply, Record, Reply, Session, Validation,
};

const NOW: f64 = 1_000.0;

fn session() -> Session {
    let mut session = Session::default();
    restore(&mut session);
    session
}

fn restore(session: &mut Session) -> Reply {
    session
        .dispatch(Command::Restore {
            policy: Policy::default(),
            record: None,
            now: NOW,
        })
        .unwrap()
}

fn activation() -> Activation {
    Activation {
        source: "fixture".into(),
        plan_identifier: "paid".into(),
        activated_at: NOW,
        license_key: None,
        activation_identifier: Some("fixture-instance".into()),
        expires_at: None,
    }
}

fn pending_write(reply: Reply) -> (u64, Record) {
    let Some(Effect::Persist { id, record }) = reply.effect else {
        panic!("Expected a persistence effect");
    };
    (id, record)
}

fn provider_id(reply: Reply, expected: Operation) -> u64 {
    let Some(Effect::Provider { id, operation, .. }) = reply.effect else {
        panic!("Expected a provider effect");
    };
    assert_eq!(operation, expected);
    id
}

fn persist(session: &mut Session, id: u64, success: bool) -> Reply {
    session
        .complete_effect(id, EffectCompletion::Persisted(success), Some(NOW))
        .unwrap()
}

fn assert_failure(result: Result<impl std::fmt::Debug, Failure>, code: ErrorCode) {
    assert_eq!(result.unwrap_err().code, code);
}

#[test]
fn native_provider_completion_survives_cancellation_without_repeating_a_mutation() {
    let mut session = session();
    for (command, operation, result) in [
        (
            Command::Activate {
                license_key: None,
                now: NOW,
            },
            Operation::Activate,
            ProviderReply::Activated {
                activation: activation(),
            },
        ),
        (
            Command::Deactivate { now: NOW },
            Operation::Deactivate,
            ProviderReply::Deactivated,
        ),
    ] {
        let activated = operation == Operation::Activate;
        let (journal_id, journal) = pending_write(session.dispatch(command).unwrap());
        assert_eq!(journal.pending_activation.is_some(), activated);
        assert_eq!(journal.pending_deactivation, !activated);
        let id = provider_id(persist(&mut session, journal_id, true), operation);
        assert_failure(
            session.complete_provider_effect(id, Some(NOW), false),
            ErrorCode::OperationInProgress,
        );
        assert_failure(
            session.stage_provider_completion(journal_id, ProviderReply::Deactivated),
            ErrorCode::OperationInProgress,
        );
        session.stage_provider_completion(id, result).unwrap();
        assert!(!session.has_deferred_completion());
        assert_failure(
            session.stage_provider_completion(id, ProviderReply::Deactivated),
            ErrorCode::OperationInProgress,
        );
        for result in [
            session.resume_completion(Some(NOW), false),
            session.dispatch(Command::Cancel { id, now: NOW }),
            session.dispatch(Command::Provider {
                id,
                result: ProviderReply::Deactivated,
                now: NOW,
            }),
            session.complete_effect(
                id,
                EffectCompletion::Provider(Some(ProviderReply::Deactivated)),
                Some(NOW),
            ),
        ] {
            assert_failure(result, ErrorCode::OperationInProgress);
        }
        assert_failure(
            session.complete_provider_effect(id, None, true),
            ErrorCode::InvalidData,
        );
        assert_failure(
            session.resume_completion(Some(f64::NAN), false),
            ErrorCode::InvalidData,
        );
        assert!(session.has_deferred_completion());
        let (write_id, candidate) =
            pending_write(session.resume_completion(Some(NOW), true).unwrap());
        assert!(!session.has_deferred_completion());
        assert_failure(
            session.stage_provider_completion(id, ProviderReply::Deactivated),
            ErrorCode::OperationInProgress,
        );
        let failed = persist(&mut session, write_id, false);
        assert_eq!(failed.error.unwrap().code, ErrorCode::StorageFailure);
        let (retry_id, retry_record) = pending_write(
            session
                .dispatch(Command::RetryPersistence { now: NOW })
                .unwrap(),
        );
        assert_ne!(retry_id, write_id);
        assert_eq!(retry_record, candidate);
        let completed = persist(&mut session, retry_id, true);
        assert_eq!(completed.state.plan.is_licensed, activated);
        assert_eq!(completed.state.has_activation, activated);
        assert!(!completed.state.has_pending_persistence && completed.effect.is_none());
        assert_eq!(
            completed.outcome,
            (!activated).then_some(LicenseRefreshOutcome::Deactivated)
        );

        let mut reopened = Session::default();
        reopened
            .stage_stored_record(&serde_json::to_vec(&candidate).unwrap())
            .unwrap();
        assert_eq!(restore(&mut reopened).state, completed.state);
    }
}

#[test]
fn native_validation_cancelled_at_clock_failure_cannot_be_applied_by_a_later_task() {
    let mut session = session();
    let (write, _) = pending_write(
        session
            .dispatch(Command::AdoptExternallyVerifiedActivation {
                activation: activation(),
                now: NOW,
            })
            .unwrap(),
    );
    let initial = persist(&mut session, write, true).state;
    let id = provider_id(
        session
            .dispatch(Command::Refresh {
                only_if_due: false,
                now: NOW,
            })
            .unwrap(),
        Operation::Validate,
    );
    session
        .stage_provider_completion(
            id,
            ProviderReply::Validated {
                validation: Validation {
                    is_valid: false,
                    plan_identifier: None,
                    expires_at: None,
                },
            },
        )
        .unwrap();
    assert_failure(
        session.complete_provider_effect(id, None, true),
        ErrorCode::InvalidData,
    );
    let resumed = session.resume_completion(Some(NOW), false).unwrap();
    assert_eq!(resumed.error.unwrap().code, ErrorCode::Cancelled);
    assert!(resumed.effect.is_none() && resumed.outcome.is_none());
    assert!(!session.has_deferred_completion());
    assert_eq!(resumed.state, initial);
}

fn unavailable_clock() -> impl Strategy<Value = Option<f64>> {
    prop_oneof![
        Just(None),
        Just(Some(f64::NAN)),
        Just(Some(f64::NEG_INFINITY)),
        Just(Some(253_402_300_800.0)),
    ]
}

proptest! {
    #[test]
    fn deferred_storage_receipt_never_repeats_or_overwrites_an_attempt(
        failed_writes in prop::collection::vec(any::<bool>(), 0..8),
        clock_retries in prop::collection::vec(unavailable_clock(), 1..8),
        unavailable_at_submission in unavailable_clock(),
    ) {
        // A fresh ID belongs to exactly one storage call. The fixtures model the
        // return from each host call, never repeat an already accepted receipt, and
        // retry only the write for the same native record.
        let mut session = session();
        let (mut current, record) = pending_write(session.dispatch(
            Command::AdoptExternallyVerifiedActivation { activation: activation(), now: NOW },
        ).unwrap());
        let candidate = serde_json::to_vec(&record).unwrap();
        let mut retired = Vec::new();

        for cancel in failed_writes {
            let failed = session.complete_effect(
                current, EffectCompletion::Persisted(false), unavailable_at_submission,
            ).unwrap_err();
            prop_assert_eq!(failed.code, ErrorCode::InvalidData);
            prop_assert!(session.has_deferred_completion());
            for &clock in &clock_retries {
                let repeat = session.resume_completion(clock, cancel).unwrap_err();
                prop_assert_eq!(repeat.code, ErrorCode::InvalidData);
                prop_assert!(session.has_deferred_completion());
            }

            let duplicate = session.complete_effect(
                current, EffectCompletion::Persisted(true), Some(NOW),
            ).unwrap_err();
            prop_assert_eq!(duplicate.code, ErrorCode::OperationInProgress);
            let legacy = session.dispatch(Command::Persisted {
                id: current, success: true, now: NOW,
            }).unwrap_err();
            prop_assert_eq!(legacy.code, ErrorCode::OperationInProgress);

            let resumed = session.resume_completion(Some(NOW), cancel).unwrap();
            prop_assert!(!session.has_deferred_completion());
            prop_assert!(!resumed.state.plan.is_licensed && resumed.state.has_pending_persistence);
            let retry = if cancel {
                prop_assert_eq!(resumed.error.unwrap().code, ErrorCode::Cancelled);
                prop_assert!(resumed.effect.is_none());
                session.dispatch(Command::RetryPersistence { now: NOW }).unwrap()
            } else {
                prop_assert!(resumed.error.is_none());
                resumed
            };
            let (new_id, next_record) = pending_write(retry);
            prop_assert_eq!(serde_json::to_vec(&next_record).unwrap(), &candidate[..]);
            prop_assert!(new_id > current);
            retired.push(current);
            current = new_id;
        }

        for id in retired {
            for success in [true, false] {
                let stale = session.complete_effect(
                    id, EffectCompletion::Persisted(success), Some(NOW),
                ).unwrap_err();
                prop_assert_eq!(stale.code, ErrorCode::OperationInProgress);
            }
        }

        let deferred = session.complete_effect(
            current, EffectCompletion::Persisted(true), unavailable_at_submission,
        ).unwrap_err();
        prop_assert_eq!(deferred.code, ErrorCode::InvalidData);
        let mut snapshot = session.dispatch(Command::Snapshot { now: NOW }).unwrap();
        prop_assert!(!snapshot.state.plan.is_licensed && snapshot.state.has_pending_persistence);
        // Cancellation cannot discard a successful save, including its access decision.
        let saved = session.resume_completion(Some(NOW), true).unwrap();
        prop_assert!(saved.state.plan.is_licensed && !saved.state.has_pending_persistence);
        prop_assert!(saved.effect.is_none() && saved.error.is_none());
        prop_assert!(!session.has_deferred_completion());
        snapshot = session.dispatch(Command::RetryPersistence { now: NOW }).unwrap();
        prop_assert!(snapshot.effect.is_none() && snapshot.error.is_none());

        let mut reopened = Session::default();
        reopened.stage_stored_record(&candidate).unwrap();
        let restored = reopened.dispatch(Command::Restore {
            policy: Policy::default(), record: None, now: NOW,
        }).unwrap();
        prop_assert_eq!(restored.state, saved.state);
    }

    #[test]
    fn a_deferred_provider_result_applies_only_to_its_own_effect(
        clock_retries in prop::collection::vec(unavailable_clock(), 1..8),
        malformed: bool,
        cancelled: bool,
    ) {
        let mut session = session();
        let write = session.dispatch(Command::Activate { license_key: None, now: NOW }).unwrap();
        let (prepare_id, _) = pending_write(write);
        let provider = session.complete_effect(
            prepare_id, EffectCompletion::Persisted(true), Some(NOW),
        ).unwrap();
        let Some(Effect::Provider { id, operation: Operation::Activate, .. }) = provider.effect else {
            panic!("An activation starts only after its intent is durable");
        };
        let completion = || EffectCompletion::Provider(
            (!malformed).then(|| ProviderReply::Activated { activation: activation() }),
        );
        let stale = session.complete_effect(prepare_id, completion(), None).unwrap_err();
        prop_assert_eq!(stale.code, ErrorCode::OperationInProgress);
        prop_assert!(!session.has_deferred_completion());

        let error = session.complete_effect(id, completion(), None).unwrap_err();
        prop_assert_eq!(error.code, ErrorCode::InvalidData);
        for clock in clock_retries {
            let retained = session.resume_completion(clock, cancelled).unwrap_err();
            prop_assert_eq!(retained.code, ErrorCode::InvalidData);
            prop_assert!(session.has_deferred_completion());
        }
        let duplicate = session.complete_effect(id, completion(), Some(NOW)).unwrap_err();
        prop_assert_eq!(duplicate.code, ErrorCode::OperationInProgress);
        let legacy = session.dispatch(Command::Cancel { id, now: NOW }).unwrap_err();
        prop_assert_eq!(legacy.code, ErrorCode::OperationInProgress);

        let recovered = session.resume_completion(Some(NOW), cancelled).unwrap();
        prop_assert!(!session.has_deferred_completion());
        if malformed {
            prop_assert_eq!(recovered.error.unwrap().code, ErrorCode::InvalidData);
            prop_assert!(recovered.effect.is_none());
            let snapshot = recovered.state;
            prop_assert!(snapshot.has_pending_activation);
            prop_assert!(!snapshot.is_activating && !snapshot.has_pending_persistence);
            prop_assert!(!snapshot.plan.is_licensed);
        } else {
            let (save_id, _) = pending_write(recovered);
            let stale = session.complete_effect(id, completion(), Some(NOW)).unwrap_err();
            prop_assert_eq!(stale.code, ErrorCode::OperationInProgress);
            let durable = session.complete_effect(
                save_id, EffectCompletion::Persisted(true), Some(NOW),
            ).unwrap();
            prop_assert!(durable.state.plan.is_licensed);
            prop_assert!(!durable.state.has_pending_activation && durable.effect.is_none());
        }
    }
}
