//! Bounded C ABI for provider-neutral licensing and native access callbacks.

#[cfg(test)]
#[path = "../tests/unit/session_wire.rs"]
mod session_wire_tests;
use serde::Deserialize;
use serde_json::value::RawValue;
use software_licensing::{
    Activation, Command, EffectCompletion, ErrorCode, Failure, Policy, Reply, Session, Validation,
};
use std::sync::Mutex;
use zeroize::Zeroizing;

/// Native ABI version. Record version 2 reads and upgrades version 1.
pub const SOFTWARE_LICENSING_ABI_VERSION: u32 = 6u32;

pub struct SessionHandle(Mutex<Session>);

pub struct Json {
    code: u32,
    bytes: Zeroizing<Vec<u8>>,
    persistence_record: Option<Zeroizing<Vec<u8>>>,
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
            persistence_record: None,
        }))
    }

    fn reply(value: Result<Reply, Failure>) -> *mut Self {
        match value {
            Ok(reply) => {
                let (bytes, persistence_record) = reply.into_wire_parts();
                Box::into_raw(Box::new(Self {
                    code: 0,
                    bytes,
                    persistence_record,
                }))
            }
            Err(error) => Self::result::<()>(Err(error)),
        }
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
    SOFTWARE_LICENSING_ABI_VERSION
}
#[unsafe(no_mangle)]
pub extern "C" fn software_licensing_session_create() -> *mut SessionHandle {
    Box::into_raw(Box::new(SessionHandle(Mutex::new(Session::default()))))
}

/// Returns 1 when a completed effect is waiting for a valid timestamp, else 0.
///
/// # Safety
/// The session must be live for the call. Null means no saved completion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_session_has_completion(
    session: *const SessionHandle,
) -> u32 {
    u32::from(
        unsafe { session.as_ref() }
            .and_then(|session| session.0.lock().ok())
            .is_some_and(|session| session.has_deferred_completion()),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum CompletionCommand {
    CompleteEffect,
    ResumeCompletion,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum CompletionKind {
    Provider,
    Persisted,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionInput<'a> {
    kind: CompletionKind,
    #[serde(borrow)]
    result: Option<&'a RawValue>,
    success: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionRequest<'a> {
    command: CompletionCommand,
    now: Option<f64>,
    id: Option<u64>,
    #[serde(borrow)]
    completion: Option<CompletionInput<'a>>,
    cancelled: Option<bool>,
}

fn dispatch(session: &Mutex<Session>, bytes: &[u8]) -> Result<Reply, Failure> {
    if let Ok(command) = software_licensing::decode::<Command>(bytes) {
        return session.lock().map_err(|_| invalid())?.dispatch(command);
    }
    // Completion is a binding protocol. Rust provider drivers continue to
    // admit their own provider replies through typed Session commands.
    let request: CompletionRequest<'_> = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    match (
        request.command,
        request.id,
        request.completion,
        request.cancelled,
    ) {
        (CompletionCommand::CompleteEffect, Some(id), Some(completion), None) => {
            let completion = match (completion.kind, completion.result, completion.success) {
                (CompletionKind::Provider, result, None) => {
                    // Decode the original JSON so duplicate keys stay invalid.
                    EffectCompletion::Provider(
                        result
                            .and_then(|raw| software_licensing::decode(raw.get().as_bytes()).ok()),
                    )
                }
                (CompletionKind::Persisted, None, Some(success)) => {
                    EffectCompletion::Persisted(success)
                }
                _ => return Err(invalid()),
            };
            session
                .lock()
                .map_err(|_| invalid())?
                .complete_effect(id, completion, request.now)
        }
        (CompletionCommand::ResumeCompletion, None, None, Some(cancelled)) => session
            .lock()
            .map_err(|_| invalid())?
            .resume_completion(request.now, cancelled),
        _ => Err(invalid()),
    }
}
/// Dispatches bounded UTF-8 JSON and never logs input. Effect IDs are opaque.
/// Save the bytes from software_licensing_json_persistence_record once per persist
/// effect and acknowledge its exact ID once with persisted or completeEffect.
/// completeEffect takes id, now and a completion object with kind provider (result)
/// or persisted (success). A null now retains the outcome. resumeCompletion takes
/// now and cancelled and applies it once. Retry only after failure. Each retry
/// has a fresh ID.
///
/// adoptExternallyVerifiedActivation requires host-verified authority, product scope
/// and current entitlement. It performs value checks and persistence without a
/// provider check.
///
/// # Safety
/// Session must be live and input readable for length bytes. Calls are serialized
/// and no host callbacks run under the session lock. Returned JSON is owned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_session_dispatch(
    session: *const SessionHandle,
    data: *const u8,
    length: usize,
) -> *mut Json {
    let result = (|| {
        let session = unsafe { session.as_ref() }.ok_or_else(invalid)?;
        dispatch(&session.0, unsafe { input(data, length)? })
    })();
    Json::reply(result)
}

/// Decodes present storage bytes into a fresh session. Dispatch restore without an
/// inline record to use the staged value. Skip staging for absent storage.
/// The JSON size limit applies to the normalized record.
///
/// # Safety
/// Session must be live and the borrowed byte range readable for length bytes.
/// Returned JSON is owned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_session_stage_record(
    session: *const SessionHandle,
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
            .0
            .lock()
            .map_err(|_| invalid())?
            .stage_stored_record(bytes)
    })();
    Json::result(result)
}
/// # Safety
/// Session must be null or exclusively owned with no calls remaining.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_session_destroy(session: *mut SessionHandle) {
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
}

fn decode_stored_record(bytes: Option<Vec<u8>>) -> Result<serde_json::Value, Failure> {
    let Some(bytes) = bytes else {
        return Ok(serde_json::Value::Null);
    };
    software_licensing::decode_record_json(&Zeroizing::new(bytes))
}

/// On Windows this uses the supplied host-owned private directory. A save
/// encrypts the exact bytes of the borrowed UTF-8 JSON record.
///
/// # Safety
/// The request must be readable for length bytes. For a save, record must be
/// readable for record_length bytes. Pass null and zero when loading or deleting.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_windows_storage(
    request: *const u8,
    length: usize,
    record: *const u8,
    record_length: usize,
) -> *mut Json {
    let result = (|| {
        let v: StorageInput = software_licensing::decode(unsafe { input(request, length)? })?;
        let store =
            software_licensing_windows::WindowsLicenseStorage::new(v.directory, &v.namespace)?;
        let absent_record = record.is_null() && record_length == 0;
        match v.operation.as_str() {
            "load" if absent_record => decode_stored_record(store.load()?),
            "save" => {
                let bytes = unsafe { input(record, record_length)? };
                software_licensing::decode_record_json(bytes)?;
                store.save(bytes)?;
                Ok(serde_json::Value::Null)
            }
            "delete" if absent_record => {
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

/// Returns a view with code 0 for a value or 1 for a static failure.
///
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
/// Borrows the UTF-8 storage record for this reply's persist effect. Copy the
/// bytes before destroying the owner. An absent record or null owner returns
/// null and writes zero to length, if supplied.
///
/// # Safety
/// Owner must be live until all borrowed byte reads finish. Length may be null
/// when no count is needed, or writable for one `usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn software_licensing_json_persistence_record(
    owner: *const Json,
    length: *mut usize,
) -> *const u8 {
    let bytes = unsafe { owner.as_ref() }.and_then(|reply| reply.persistence_record.as_deref());
    if let Some(length) = unsafe { length.as_mut() } {
        *length = bytes.map_or(0, Vec::len);
    }
    bytes.map_or(std::ptr::null(), |record| record.as_ptr())
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
