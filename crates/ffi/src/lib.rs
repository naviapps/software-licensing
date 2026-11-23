//! Bounded C ABI for provider-neutral licensing and native access callbacks.

#[cfg(test)]
#[path = "../tests/unit/session_wire.rs"]
mod session_wire_tests;
use serde::Deserialize;
use software_licensing::{Activation, Command, ErrorCode, Failure, Policy, Session, Validation};
use std::sync::Mutex;
use zeroize::Zeroizing;

pub struct Json {
    code: u32,
    bytes: Zeroizing<Vec<u8>>,
}
#[repr(C)]
pub struct JsonView {
    pub code: u32,
    pub data: *const u8,
    pub length: usize,
}
impl Json {
    fn result<T: serde::Serialize>(value: Result<T, Failure>) -> *mut Self {
        let (code, bytes) = match value {
            Ok(v) => (0, serde_json::to_vec(&v).expect("finite licensing output")),
            Err(e) => (1, serde_json::to_vec(&e).expect("static error code")),
        };
        Box::into_raw(Box::new(Self {
            code,
            bytes: Zeroizing::new(bytes),
        }))
    }
}
fn invalid() -> Failure {
    Failure::new(ErrorCode::InvalidData)
}
unsafe fn input<'a>(data: *const u8, length: usize) -> Result<&'a [u8], Failure> {
    if data.is_null() || length == 0 || length > software_licensing::MAX_BYTES {
        return Err(invalid());
    }
    Ok(unsafe { std::slice::from_raw_parts(data, length) })
}
#[unsafe(no_mangle)]
pub extern "C" fn software_licensing_abi_version() -> u32 {
    4
}
#[unsafe(no_mangle)]
pub extern "C" fn software_licensing_session_create() -> *mut Mutex<Session> {
    Box::into_raw(Box::new(Mutex::new(Session::default())))
}
/// # Safety
/// Session must be live, input readable for length bytes. Calls are serialized;
/// there are no host callbacks under the session lock. Returned JSON is owned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_session_dispatch(
    session: *const Mutex<Session>,
    data: *const u8,
    length: usize,
) -> *mut Json {
    let result = (|| {
        let session = unsafe { session.as_ref() }.ok_or_else(invalid)?;
        let command: Command = software_licensing::decode(unsafe { input(data, length)? })?;
        session.lock().map_err(|_| invalid())?.dispatch(command)
    })();
    Json::result(result)
}

/// # Safety
/// Session must be live and the byte range must be readable for its length.
/// Call only for present storage bytes before dispatching restore.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_session_stage_record(
    session: *const Mutex<Session>,
    data: *const u8,
    length: usize,
) -> *mut Json {
    let result = (|| {
        let session = unsafe { session.as_ref() }.ok_or_else(invalid)?;
        let bytes = if length == 0 {
            &[]
        } else {
            if data.is_null() || length > isize::MAX as usize {
                return Err(invalid());
            }
            unsafe { std::slice::from_raw_parts(data, length) }
        };
        session
            .lock()
            .map_err(|_| invalid())?
            .stage_stored_record(bytes)
    })();
    Json::result(result)
}
/// # Safety
/// Session must be null or exclusively owned with no calls remaining.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_session_destroy(session: *mut Mutex<Session>) {
    if !session.is_null() {
        drop(unsafe { Box::from_raw(session) });
    }
}
#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
enum ValueInput {
    DefaultPolicy,
    Activation(Activation),
    Validation(Validation),
    Policy(Policy),
    LicenseKey(String),
    Identifier(String),
}
/// # Safety
/// Input must be readable for length bytes. Validation never performs effects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_value(data: *const u8, length: usize) -> *mut Json {
    let result = (|| {
        let value: ValueInput = software_licensing::decode(unsafe { input(data, length)? })?;
        match value {
            ValueInput::DefaultPolicy => serde_json::to_value(Policy::default()),
            ValueInput::Activation(v) => serde_json::to_value(v.normalized()?),
            ValueInput::Validation(v) => serde_json::to_value(v.validated()?),
            ValueInput::Policy(v) => serde_json::to_value(v.validated()?),
            ValueInput::LicenseKey(v) => {
                let v = software_licensing::normalize_key(&v);
                if v.is_empty() || v.len() > 4096 {
                    return Err(Failure::new(ErrorCode::InvalidLicenseKey));
                }
                Ok(serde_json::Value::String(v))
            }
            ValueInput::Identifier(v) => Ok(serde_json::Value::String(
                software_licensing::identifier(&v)?,
            )),
        }
        .map_err(|_| invalid())
    })();
    Json::result(result)
}
/// # Safety
/// Input must be readable for length bytes. Only maps an injected native access decision.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_access_reply(
    data: *const u8,
    length: usize,
) -> *mut Json {
    Json::result((|| {
        let value: software_licensing::AccessInput =
            software_licensing::decode(unsafe { input(data, length)? })?;
        software_licensing::access_reply(value)
    })())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StorageInput {
    directory: String,
    namespace: String,
    operation: String,
    record: Option<serde_json::Value>,
}

fn decode_stored_record(bytes: Option<Vec<u8>>) -> Result<serde_json::Value, Failure> {
    let Some(bytes) = bytes else {
        return Ok(serde_json::Value::Null);
    };
    software_licensing::decode_record_json(&Zeroizing::new(bytes))
}

/// # Safety
/// Input must be readable for length bytes. On Windows this performs effects in
/// the supplied host-owned private directory; on other OSes it is unsupported.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_windows_storage(
    data: *const u8,
    length: usize,
) -> *mut Json {
    let result = (|| {
        let v: StorageInput = software_licensing::decode(unsafe { input(data, length)? })?;
        let store =
            software_licensing_windows::WindowsLicenseStorage::new(v.directory, &v.namespace)?;
        match v.operation.as_str() {
            "load" => decode_stored_record(store.load()?),
            "save" => {
                let bytes = Zeroizing::new(
                    serde_json::to_vec(&v.record.ok_or_else(invalid)?).map_err(|_| invalid())?,
                );
                store.save(&bytes)?;
                Ok(serde_json::Value::Null)
            }
            "delete" => {
                store.delete()?;
                Ok(serde_json::Value::Null)
            }
            _ => Err(invalid()),
        }
    })();
    Json::result(result)
}

#[cfg(test)]
#[path = "../tests/unit/windows_storage.rs"]
mod windows_storage_tests;
/// # Safety
/// Owner must be live until all borrowed byte reads finish. Null returns an error view.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_json_view(owner: *const Json) -> JsonView {
    match unsafe { owner.as_ref() } {
        Some(v) => JsonView {
            code: v.code,
            data: v.bytes.as_ptr(),
            length: v.bytes.len(),
        },
        None => JsonView {
            code: 1,
            data: std::ptr::null(),
            length: 0,
        },
    }
}
/// # Safety
/// Owner must be null or exclusively owned, with no outstanding byte reads.
/// Native-owned response bytes are zeroized before allocation release.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_json_destroy(owner: *mut Json) {
    if !owner.is_null() {
        drop(unsafe { Box::from_raw(owner) });
    }
}
