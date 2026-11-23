//! User-scoped Windows secure storage adapter.
use software_licensing::{ErrorCode, Failure, MAX_BYTES, identifier};
use std::path::{Path, PathBuf};

/// One host-owned directory and namespace. DPAPI binds data to the current user;
/// the namespace is additional entropy, not a secret or machine fingerprint.
pub struct WindowsLicenseStorage {
    directory: PathBuf,
    namespace: String,
}
impl WindowsLicenseStorage {
    pub fn new(directory: impl Into<PathBuf>, namespace: &str) -> Result<Self, Failure> {
        let directory = directory.into();
        if !directory.is_absolute() {
            return Err(Failure::new(ErrorCode::InvalidData));
        }
        Ok(Self {
            directory,
            namespace: identifier(namespace)?,
        })
    }
    pub fn load(&self) -> Result<Option<Vec<u8>>, Failure> {
        native::load(&self.directory, &self.namespace)
    }
    pub fn save(&self, bytes: &[u8]) -> Result<(), Failure> {
        if bytes.is_empty() || bytes.len() > MAX_BYTES {
            return Err(Failure::new(ErrorCode::InvalidData));
        }
        native::save(&self.directory, &self.namespace, bytes)
    }
    pub fn delete(&self) -> Result<(), Failure> {
        native::delete(&self.directory)
    }
}
#[cfg(any(windows, test))]
mod temporary {
    use std::{
        fs::{self, File, OpenOptions},
        io::{self, ErrorKind},
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    pub(super) const MAX_ATTEMPTS: u64 = 32;

    struct OwnedTemporary {
        path: PathBuf,
        remove_on_drop: bool,
    }
    impl Drop for OwnedTemporary {
        fn drop(&mut self) {
            if self.remove_on_drop {
                let _ = fs::remove_file(&self.path);
            }
        }
    }

    pub(super) fn with_file(
        directory: &Path,
        serial: &AtomicU64,
        operation: impl FnOnce(File, &Path) -> io::Result<()>,
    ) -> io::Result<()> {
        for _ in 0..MAX_ATTEMPTS {
            let path = directory.join(format!(
                "license-{}-{}.tmp",
                std::process::id(),
                serial.fetch_add(1, Ordering::Relaxed)
            ));
            let file = match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            // Cleanup ownership starts only after create_new succeeds. The
            // operation owns the handle, so it closes before this guard runs.
            let mut temporary = OwnedTemporary {
                path,
                remove_on_drop: true,
            };
            operation(file, &temporary.path)?;
            // A successful operation has moved the file to its final target.
            temporary.remove_on_drop = false;
            return Ok(());
        }
        Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "temporary file candidates already exist",
        ))
    }
}
#[cfg(not(windows))]
mod native {
    use super::*;
    pub fn load(_: &Path, _: &str) -> Result<Option<Vec<u8>>, Failure> {
        Err(Failure::new(ErrorCode::UnsupportedPlatform))
    }
    pub fn save(_: &Path, _: &str, _: &[u8]) -> Result<(), Failure> {
        Err(Failure::new(ErrorCode::UnsupportedPlatform))
    }
    pub fn delete(_: &Path) -> Result<(), Failure> {
        Err(Failure::new(ErrorCode::UnsupportedPlatform))
    }
}
#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        fs,
        io::{Read, Write},
        os::windows::{ffi::OsStrExt, fs::MetadataExt},
        sync::atomic::AtomicU64,
    };
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
        Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
            MoveFileExW,
        },
    };
    use zeroize::Zeroizing;
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    const NAME: &str = "license.dpapi";
    fn failure() -> Failure {
        Failure::new(ErrorCode::StorageFailure)
    }
    fn check_directory(path: &Path) -> Result<(), Failure> {
        for parent in path.ancestors() {
            match fs::symlink_metadata(parent) {
                Ok(m) if !m.is_dir() || m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
                    return Err(failure());
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(failure()),
            }
        }
        Ok(())
    }
    fn check_file(path: &Path) -> Result<(), Failure> {
        match fs::symlink_metadata(path) {
            Ok(m) if !m.is_file() || m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
                Err(failure())
            }
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(failure()),
        }
    }
    struct DpapiBlob(CRYPT_INTEGER_BLOB);
    impl Drop for DpapiBlob {
        fn drop(&mut self) {
            if !self.0.pbData.is_null() {
                // CryptUnprotectData owns a LocalAlloc plaintext buffer.
                unsafe {
                    zeroize::Zeroize::zeroize(std::slice::from_raw_parts_mut(
                        self.0.pbData,
                        self.0.cbData as usize,
                    ));
                    LocalFree(self.0.pbData.cast());
                }
            }
        }
    }
    fn crypt(bytes: &[u8], namespace: &str, encrypt: bool) -> Result<Vec<u8>, Failure> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: bytes.len().try_into().map_err(|_| failure())?,
            pbData: bytes.as_ptr().cast_mut(),
        };
        let entropy = CRYPT_INTEGER_BLOB {
            cbData: namespace.len().try_into().map_err(|_| failure())?,
            pbData: namespace.as_ptr().cast_mut(),
        };
        let mut output = DpapiBlob(CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        });
        let ok = unsafe {
            if encrypt {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    &entropy,
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output.0,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    &entropy,
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output.0,
                )
            }
        };
        if ok == 0 || output.0.pbData.is_null() {
            return Err(failure());
        }
        Ok(
            unsafe { std::slice::from_raw_parts(output.0.pbData, output.0.cbData as usize) }
                .to_vec(),
        )
    }
    pub fn load(directory: &Path, namespace: &str) -> Result<Option<Vec<u8>>, Failure> {
        check_directory(directory)?;
        let path = directory.join(NAME);
        check_file(&path)?;
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(failure()),
        };
        let mut encrypted = Vec::new();
        file.take((MAX_BYTES * 2 + 1) as u64)
            .read_to_end(&mut encrypted)
            .map_err(|_| failure())?;
        if encrypted.is_empty() || encrypted.len() > MAX_BYTES * 2 {
            return Err(failure());
        }
        let bytes = crypt(&encrypted, namespace, false)?;
        if bytes.len() > MAX_BYTES {
            return Err(failure());
        }
        Ok(Some(bytes))
    }
    pub fn save(directory: &Path, namespace: &str, bytes: &[u8]) -> Result<(), Failure> {
        check_directory(directory)?;
        fs::create_dir_all(directory).map_err(|_| failure())?;
        let target = directory.join(NAME);
        check_file(&target)?;
        let encrypted = Zeroizing::new(crypt(bytes, namespace, true)?);
        temporary::with_file(directory, &SERIAL, |mut file, temporary| {
            file.write_all(&encrypted)?;
            file.sync_all()?;
            drop(file);
            let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
        .map_err(|_| failure())
    }
    pub fn delete(directory: &Path) -> Result<(), Failure> {
        check_directory(directory)?;
        let path = directory.join(NAME);
        check_file(&path)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(failure()),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/temporary.rs"]
mod tests;
