use super::*;
use std::{
    fs::{self, File},
    io::{self, Write},
    sync::atomic::{AtomicU64, Ordering},
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "software-licensing-windows-test-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path.canonicalize().unwrap()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create test directory: {error}"),
            }
        }
    }
    fn candidate(&self, serial: u64) -> PathBuf {
        self.0
            .join(format!("license-{}-{serial}.tmp", std::process::id()))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn collision_preserves_foreign_bytes_and_uses_next_candidate() {
    let directory = Directory::new();
    let foreign = directory.candidate(0);
    let completed = directory.0.join("completed");
    fs::write(&foreign, b"foreign recovery bytes").unwrap();

    temporary::with_file(&directory.0, &AtomicU64::new(0), |mut file, path| {
        assert_eq!(path, directory.candidate(1));
        file.write_all(b"replacement")?;
        file.sync_all()?;
        drop(file);
        fs::rename(path, &completed)
    })
    .unwrap();

    assert_eq!(fs::read(&foreign).unwrap(), b"foreign recovery bytes");
    assert_eq!(fs::read(completed).unwrap(), b"replacement");
    assert!(!directory.candidate(1).exists());
}

#[test]
fn collision_exhaustion_preserves_all_foreign_files_and_original_target() {
    let directory = Directory::new();
    let target = directory.0.join("license.dpapi");
    fs::write(&target, b"original target").unwrap();
    for serial in 0..temporary::MAX_ATTEMPTS {
        fs::write(directory.candidate(serial), serial.to_le_bytes()).unwrap();
    }

    let error = temporary::with_file(&directory.0, &AtomicU64::new(0), |_, _| {
        panic!("no candidate is owned")
    })
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(target).unwrap(), b"original target");
    for serial in 0..temporary::MAX_ATTEMPTS {
        assert_eq!(
            fs::read(directory.candidate(serial)).unwrap(),
            serial.to_le_bytes()
        );
    }
    assert!(!directory.candidate(temporary::MAX_ATTEMPTS).exists());
}

#[test]
fn failed_write_cleans_owned_file_and_preserves_foreign_file_and_target() {
    let directory = Directory::new();
    let target = directory.0.join("license.dpapi");
    let foreign = directory.candidate(0);
    fs::write(&target, b"original target").unwrap();
    fs::write(&foreign, b"foreign recovery bytes").unwrap();

    let result = temporary::with_file(&directory.0, &AtomicU64::new(0), |mut file, path| {
        file.write_all(b"partial write")?;
        drop(file);
        // Deterministically fail a write after bytes reached the owned file.
        File::open(path)?.write_all(b"remaining bytes")
    });

    assert!(result.is_err());
    assert!(!directory.candidate(1).exists());
    assert_eq!(fs::read(foreign).unwrap(), b"foreign recovery bytes");
    assert_eq!(fs::read(target).unwrap(), b"original target");
}

#[test]
fn failed_replacement_cleans_owned_file_and_preserves_original_target() {
    let directory = Directory::new();
    let target = directory.0.join("license.dpapi");
    fs::write(&target, b"original target").unwrap();

    let result = temporary::with_file(&directory.0, &AtomicU64::new(0), |mut file, _| {
        file.write_all(b"replacement")?;
        file.sync_all()?;
        drop(file);
        Err(io::Error::other("injected replacement failure"))
    });

    assert!(result.is_err());
    assert!(!directory.candidate(0).exists());
    assert_eq!(fs::read(target).unwrap(), b"original target");
}

#[cfg(windows)]
#[test]
fn native_save_replaces_target_and_round_trips_dpapi() {
    let directory = Directory::new();
    let storage = WindowsLicenseStorage::new(&directory.0, "temporary-tests").unwrap();

    storage.save(b"original").unwrap();
    assert_eq!(storage.load().unwrap().unwrap(), b"original");
    storage.save(b"replacement").unwrap();
    assert_eq!(storage.load().unwrap().unwrap(), b"replacement");
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[cfg(windows)]
#[test]
fn native_replacement_failure_preserves_ciphertext_and_cleans_owned_file() {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    let directory = Directory::new();
    let storage = WindowsLicenseStorage::new(&directory.0, "temporary-tests").unwrap();
    storage.save(b"original").unwrap();
    let target = directory.0.join("license.dpapi");
    let original = fs::read(&target).unwrap();
    // Deny delete sharing so MoveFileExW cannot replace the existing target.
    let lock = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&target)
        .unwrap();

    assert_eq!(
        storage.save(b"replacement").unwrap_err().code,
        ErrorCode::StorageFailure
    );
    assert_eq!(fs::read(&target).unwrap(), original);
    assert_eq!(storage.load().unwrap().unwrap(), b"original");
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    drop(lock);
}
