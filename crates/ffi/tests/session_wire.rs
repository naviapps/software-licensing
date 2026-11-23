// The shipped static and dynamic libraries do not provide an rlib for integration tests.
#[path = "../src/lib.rs"]
mod ffi;

use serde_json::{Value, json};
use software_licensing::Session;
use std::{ptr::NonNull, sync::Mutex};

struct SessionHandle(NonNull<Mutex<Session>>);

impl SessionHandle {
    fn new() -> Self {
        Self(NonNull::new(ffi::software_licensing_session_create()).unwrap())
    }

    fn dispatch(&self, command: Value) -> (u32, Value) {
        let input = serde_json::to_vec(&command).unwrap();
        let result = unsafe {
            ffi::software_licensing_session_dispatch(self.0.as_ptr(), input.as_ptr(), input.len())
        };
        read_result(result)
    }

    fn send(&self, command: Value) -> Value {
        let (code, body) = self.dispatch(command);
        assert_eq!(code, 0, "{body}");
        body
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

    fn stage(&self, record: &Value) {
        let input = serde_json::to_vec(record).unwrap();
        let result = unsafe {
            ffi::software_licensing_session_stage_record(
                self.0.as_ptr(),
                input.as_ptr(),
                input.len(),
            )
        };
        assert_eq!(read_result(result), (0, Value::Null));
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        unsafe { ffi::software_licensing_session_destroy(self.0.as_ptr()) };
    }
}

fn read_result(owner: *mut ffi::Json) -> (u32, Value) {
    let view = unsafe { ffi::software_licensing_json_view(owner) };
    assert!(!view.data.is_null());
    let bytes = unsafe { std::slice::from_raw_parts(view.data, view.length) };
    let body = serde_json::from_slice(bytes);
    unsafe { ffi::software_licensing_json_destroy(owner) };
    (view.code, body.unwrap())
}

fn receipt(id: u64, success: bool, now: u64) -> Value {
    json!({"command":"persisted", "id":id, "success":success, "now":now})
}

fn write(reply: &Value) -> (u64, &Value) {
    assert_eq!(reply["effect"]["kind"], "persist");
    let record = &reply["effect"]["record"];
    assert!(record.is_object());
    (reply["effect"]["id"].as_u64().unwrap(), record)
}

#[test]
fn failed_saves_require_current_receipts_and_preserve_rejected_access_on_reopen() {
    assert_eq!(ffi::software_licensing_abi_version(), 4);
    let session = SessionHandle::new();
    session.restore(1000);
    let intent = session.send(json!({
        "command":"adoptExternallyVerifiedActivation", "now":1000,
        "activation":{"source":"fixture", "planIdentifier":"paid",
            "activatedAt":1000, "licenseKey":"FIXTURE-KEY"},
    }));
    let (first, expected_record) = write(&intent);
    let before_save = session.send(json!({"command":"snapshot", "now":1000}));
    assert_eq!(before_save["state"]["plan"]["isLicensed"], false);
    assert_eq!(before_save["state"]["hasPendingPersistence"], true);
    let retry = json!({"command":"retryPersistence", "now":1000});
    let reject = |command| {
        assert_eq!(
            session.dispatch(command),
            (1, json!({"code":"operationInProgress"}))
        );
    };
    reject(retry.clone());
    let failed = session.persisted(first, false, 1000);
    assert_eq!(failed["error"], json!({"code":"storageFailure"}));
    assert_eq!(failed["state"], before_save["state"]);

    let pending = session.send(retry);
    let (second, record) = write(&pending);
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
    let invalid = session.send(json!({
        "command":"provider", "id":validation_id, "now":1100,
        "result":{"kind":"validated", "validation":{"isValid":false}},
    }));
    let (rejected, rejected_record) = write(&invalid);
    let failed = session.persisted(rejected, false, 1100);
    assert_eq!(failed["error"], json!({"code":"storageFailure"}));
    let denied = session.send(json!({"command":"snapshot", "now":1101}));
    assert_eq!(denied["state"]["plan"]["isLicensed"], false);
    assert_eq!(denied["state"]["status"], "invalid");
    assert_eq!(denied["state"]["hasActivation"], true);
    assert_eq!(denied["state"]["hasPendingPersistence"], true);

    let pending = session.send(json!({"command":"retryPersistence", "now":1101}));
    let (retry, retry_record) = write(&pending);
    assert_ne!(retry, rejected);
    assert_eq!(retry_record, rejected_record);
    let committed = session.persisted(retry, true, 1101);
    assert_eq!(committed["outcome"], "invalid");
    assert_eq!(committed["state"]["hasPendingPersistence"], false);
    drop(session);

    let reopened = SessionHandle::new();
    reopened.stage(retry_record);
    let restored = reopened.restore(1102);
    assert_eq!(restored["effect"], Value::Null);
    assert_eq!(restored["state"], committed["state"]);
}
