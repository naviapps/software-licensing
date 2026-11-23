use super::*;
use serde_json::{Value, json};

#[test]
fn stored_record_decoding_reserves_json_null_for_absence() {
    assert_eq!(decode_stored_record(None).unwrap(), Value::Null);
    for valid in [
        json!({"version": 2}),
        json!([]),
        json!(true),
        json!("value"),
    ] {
        let bytes = serde_json::to_vec(&valid).unwrap();
        assert_eq!(decode_stored_record(Some(bytes)).unwrap(), valid);
    }
    for invalid in [b"null".as_slice(), b" \r\nnull\t", b"{", b""] {
        assert_eq!(
            decode_stored_record(Some(invalid.to_vec()))
                .unwrap_err()
                .code,
            ErrorCode::InvalidData
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_ffi_load_preserves_raw_dpapi_null_as_an_invalid_record() {
    use software_licensing_windows::WindowsLicenseStorage;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct OwnedDirectory(PathBuf);
    impl Drop for OwnedDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "software-licensing-ffi-test-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&temp).unwrap();
    let directory = OwnedDirectory(temp);
    let namespace = "com.example.software-licensing.ffi-fixture";
    let storage = WindowsLicenseStorage::new(&directory.0, namespace).unwrap();
    let input = serde_json::to_vec(&json!({
        "directory": directory.0.to_str().unwrap(),
        "namespace": namespace,
        "operation": "load",
    }))
    .unwrap();
    let load = || {
        let owner = unsafe { software_licensing_windows_storage(input.as_ptr(), input.len()) };
        let view = unsafe { software_licensing_json_view(owner) };
        let json = serde_json::from_slice::<Value>(unsafe {
            std::slice::from_raw_parts(view.data, view.length)
        });
        unsafe { software_licensing_json_destroy(owner) };
        (view.code, json.unwrap())
    };

    assert_eq!(load(), (0, Value::Null));
    let raw = b" \nnull\t";
    storage.save(raw).unwrap();
    let target = directory.0.join("license.dpapi");
    let encrypted = fs::read(&target).unwrap();
    assert_eq!(load(), (1, json!({"code": "invalidData"})));
    assert_eq!(fs::read(target).unwrap(), encrypted);
    assert_eq!(storage.load().unwrap().as_deref(), Some(raw.as_slice()));

    storage.save(br#"{"version":2}"#).unwrap();
    assert_eq!(load(), (0, json!({"version": 2})));
    storage.delete().unwrap();
    assert_eq!(load(), (0, Value::Null));
}
