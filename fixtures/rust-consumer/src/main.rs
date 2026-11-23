use software_licensing::{Activation, Command, Effect, ErrorCode, Policy, Session, Status};

fn main() {
    let mut session = Session::default();
    let restored = session
        .dispatch(Command::Restore {
            policy: Policy::default(),
            record: None,
            now: 1000.0,
        })
        .unwrap();
    assert!(!restored.state.plan.is_licensed);

    // Synthetic entitlement and a caller-owned persistence acknowledgement; no I/O.
    let staged = session
        .dispatch(Command::AdoptExternallyVerifiedActivation {
            activation: Activation {
                source: "fixture".into(),
                plan_identifier: "paid".into(),
                activated_at: 1000.0,
                license_key: None,
                activation_identifier: None,
                expires_at: Some(1020.0),
            },
            now: 1000.0,
        })
        .unwrap();
    assert!(!staged.state.plan.is_licensed);
    let Some(Effect::Persist { id, .. }) = staged.effect else {
        panic!("Expected a persistence effect for the synthetic entitlement");
    };

    let acknowledge = || Command::Persisted {
        id,
        success: true,
        now: 1001.0,
    };
    let saved = session.dispatch(acknowledge()).unwrap();
    assert_eq!(saved.state.status, Status::Active);
    assert!(saved.state.plan.is_licensed);
    assert_eq!(
        session.dispatch(acknowledge()).unwrap_err().code,
        ErrorCode::OperationInProgress
    );

    let expired = session.dispatch(Command::Snapshot { now: 1020.0 }).unwrap();
    assert_eq!(expired.state.status, Status::Expired);
    assert!(!expired.state.plan.is_licensed);
    assert!(expired.effect.is_none());
    println!("Standalone Rust Git SoftwareLicensing consumer passed persistence and expiry.");
}
