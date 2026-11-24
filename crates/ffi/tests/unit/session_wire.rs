use super as ffi;
use serde_json::{Value, json};
use std::ptr::NonNull;

struct SessionHandle(NonNull<ffi::SessionHandle>);

struct WireReply {
    code: u32,
    metadata: Value,
    record: Option<Vec<u8>>,
}

impl SessionHandle {
    fn new() -> Self {
        Self(NonNull::new(ffi::software_licensing_session_create()).unwrap())
    }

    fn dispatch(&self, command: Value) -> WireReply {
        let input = serde_json::to_vec(&command).unwrap();
        self.dispatch_bytes(&input)
    }

    fn dispatch_bytes(&self, input: &[u8]) -> WireReply {
        let result = unsafe {
            ffi::software_licensing_session_dispatch(self.0.as_ptr(), input.as_ptr(), input.len())
        };
        read_result(result)
    }

    fn send(&self, command: Value) -> Value {
        let reply = self.dispatch(command);
        assert_eq!(reply.code, 0, "{}", reply.metadata);
        assert!(reply.record.is_none());
        reply.metadata
    }

    fn write(&self, command: Value) -> (u64, Vec<u8>) {
        let reply = self.dispatch(command);
        assert_eq!(reply.code, 0, "{}", reply.metadata);
        let effect = &reply.metadata["effect"];
        let id = effect["id"].as_u64().unwrap();
        assert_eq!(effect, &json!({"kind": "persist", "id": id}));
        let bytes = reply.record.unwrap();
        let record: software_licensing::Record = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record.version, 2);
        (id, bytes)
    }

    fn restore(&self, now: u64) -> Value {
        self.send(json!({"command":"restore", "now":now, "policy":{
            "isEnabled":true, "validationInterval":86400,
            "failureGracePeriod":604800, "serverFailureGracePeriod":86400,
        }}))
    }

    fn persisted(&self, id: u64, success: bool, now: u64) -> Value {
        self.send(receipt(id, success, now))
    }

    fn stage(&self, record: &[u8]) {
        let result = unsafe {
            ffi::software_licensing_session_stage_record(
                self.0.as_ptr(),
                record.as_ptr(),
                record.len(),
            )
        };
        let reply = read_result(result);
        assert_eq!(reply.code, 0);
        assert_eq!(reply.metadata, Value::Null);
        assert!(reply.record.is_none());
    }
}

#[test]
fn duplicate_provider_fields_release_the_lease_without_clearing_the_journal() {
    let session = SessionHandle::new();
    session.restore(1000);
    let (id, _) = session.write(json!({
        "command":"activate", "licenseKey":"FIXTURE-KEY", "now":1000
    }));
    let provider = session.persisted(id, true, 1000);
    let id = provider["effect"]["id"].as_u64().unwrap();
    let duplicate = format!(
        r#"{{"command":"completeEffect","id":{id},"now":1001,
              "completion":{{"kind":"provider","result":{{"kind":"failed",
              "failure":{{"code":"transportFailure","code":"invalidLicense"}}}}}}}}"#
    );
    let wire = session.dispatch_bytes(duplicate.as_bytes());
    assert_eq!(wire.code, 0);
    assert!(wire.record.is_none());
    let reply = wire.metadata;
    assert_eq!(reply["error"]["code"], "invalidData");
    assert_eq!(reply["state"]["isActivating"], false);
    assert_eq!(reply["state"]["hasPendingActivation"], true);
    assert_eq!(reply["effect"], Value::Null);
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        unsafe { ffi::software_licensing_session_destroy(self.0.as_ptr()) };
    }
}

fn read_result(owner: *mut ffi::Json) -> WireReply {
    let view = unsafe { ffi::software_licensing_json_view(owner) };
    assert!(!view.data.is_null());
    let bytes = unsafe { std::slice::from_raw_parts(view.data, view.length) };
    let metadata = serde_json::from_slice(bytes);
    let mut length = usize::MAX;
    let record = unsafe { ffi::software_licensing_json_persistence_record(owner, &mut length) };
    let record = if record.is_null() {
        assert_eq!(length, 0);
        None
    } else {
        assert!(length > 0);
        Some(unsafe { std::slice::from_raw_parts(record, length) }.to_vec())
    };
    unsafe { ffi::software_licensing_json_destroy(owner) };
    WireReply {
        code: view.code,
        metadata: metadata.unwrap(),
        record,
    }
}

fn receipt(id: u64, success: bool, now: u64) -> Value {
    json!({"command":"persisted", "id":id, "success":success, "now":now})
}

#[test]
fn failed_saves_require_current_receipts_and_preserve_rejected_access_on_reopen() {
    assert_eq!(ffi::software_licensing_abi_version(), 6);
    let session = SessionHandle::new();
    session.restore(1000);
    let (first, expected_record) = session.write(json!({
        "command":"adoptExternallyVerifiedActivation", "now":1000,
        "activation":{"source":"fixture", "planIdentifier":"paid",
            "activatedAt":1000, "licenseKey":"FIXTURE-KEY"},
    }));
    let before_save = session.send(json!({"command":"snapshot", "now":1000}));
    assert_eq!(before_save["state"]["plan"]["isLicensed"], false);
    assert_eq!(before_save["state"]["hasPendingPersistence"], true);
    let retry = json!({"command":"retryPersistence", "now":1000});
    let reject = |command| {
        let reply = session.dispatch(command);
        assert_eq!(reply.code, 1);
        assert_eq!(reply.metadata, json!({"code":"operationInProgress"}));
        assert!(reply.record.is_none());
    };
    reject(retry.clone());
    let failed = session.persisted(first, false, 1000);
    assert_eq!(failed["error"], json!({"code":"storageFailure"}));
    assert_eq!(failed["state"], before_save["state"]);

    let (second, record) = session.write(retry);
    assert_ne!(second, first);
    assert_eq!(record, expected_record);
    for success in [true, false] {
        reject(receipt(first, success, 1000));
    }
    let saved = session.persisted(second, true, 1000);
    assert_eq!(saved["state"]["plan"]["isLicensed"], true);
    assert_eq!(saved["state"]["hasPendingPersistence"], false);
    reject(receipt(second, true, 1000));

    let validation = session.send(json!({"command":"refresh", "now":1100}));
    assert_eq!(validation["effect"]["kind"], "provider");
    assert_eq!(validation["effect"]["operation"], "validate");
    let validation_id = validation["effect"]["id"].as_u64().unwrap();
    let (rejected, rejected_record) = session.write(json!({
        "command":"provider", "id":validation_id, "now":1100,
        "result":{"kind":"validated", "validation":{"isValid":false}},
    }));
    let failed = session.persisted(rejected, false, 1100);
    assert_eq!(failed["error"], json!({"code":"storageFailure"}));
    let denied = session.send(json!({"command":"snapshot", "now":1101}));
    assert_eq!(denied["state"]["plan"]["isLicensed"], false);
    assert_eq!(denied["state"]["status"], "invalid");
    assert_eq!(denied["state"]["hasActivation"], true);
    assert_eq!(denied["state"]["hasPendingPersistence"], true);

    let (retry, retry_record) = session.write(json!({"command":"retryPersistence", "now":1101}));
    assert_ne!(retry, rejected);
    assert_eq!(retry_record, rejected_record);
    let committed = session.persisted(retry, true, 1101);
    assert_eq!(committed["outcome"], "invalid");
    assert_eq!(committed["state"]["hasPendingPersistence"], false);
    drop(session);

    let reopened = SessionHandle::new();
    reopened.stage(&retry_record);
    let restored = reopened.restore(1102);
    assert_eq!(restored["effect"], Value::Null);
    assert_eq!(restored["state"], committed["state"]);
}
