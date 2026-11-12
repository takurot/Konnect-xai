//! Atomic filename moves that never replace an existing destination.
use std::{io, path::Path};

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn native_path(value: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(value.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "rename path contains NUL"))
}

/// Move a file or directory without replacing any destination entry.
///
/// Uses the platform's atomic exclusive-rename operation. Unsupported
/// platforms/filesystems fail closed; there is no replacing or copy fallback.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let from = native_path(from)?;
    let to = native_path(to)?;
    // SAFETY: both pointers reference NUL-terminated byte paths alive for the
    // call. Exclusive flags prohibit replacing an entry, including a symlink.
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    // SAFETY: the same CString lifetime and pointer guarantees apply on macOS.
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let path = |value: &Path| {
        let mut encoded: Vec<u16> = value.as_os_str().encode_wide().collect();
        if encoded.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rename path contains NUL",
            ));
        }
        encoded.push(0);
        Ok(encoded)
    };
    let from = path(from)?;
    let to = path(to)?;
    // SAFETY: both UTF-16 buffers are NUL terminated and live for the call.
    // Flags 0 omit REPLACE_EXISTING and COPY_ALLOWED: neither an intervening
    // destination nor a cross-volume copy/delete fallback is permitted.
    let result = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
    };
    if result != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn rename_noreplace(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic exclusive rename is unavailable on this platform",
    ))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests {
    use super::*;

    #[test]
    fn moves_a_file_and_preserves_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::write(&from, "source").unwrap();
        rename_noreplace(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(std::fs::read_to_string(to).unwrap(), "source");
    }

    #[test]
    fn refuses_an_existing_file_without_changing_either_entry() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::write(&from, "source").unwrap();
        std::fs::write(&to, "external").unwrap();
        assert!(rename_noreplace(&from, &to).is_err());
        assert_eq!(std::fs::read_to_string(from).unwrap(), "source");
        assert_eq!(std::fs::read_to_string(to).unwrap(), "external");
    }

    #[test]
    fn moves_a_directory_but_refuses_an_existing_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::create_dir(&from).unwrap();
        std::fs::write(from.join("board"), "source").unwrap();
        std::fs::create_dir(&to).unwrap();
        assert!(rename_noreplace(&from, &to).is_err());
        assert_eq!(
            std::fs::read_to_string(from.join("board")).unwrap(),
            "source"
        );
        assert!(to.is_dir());
        std::fs::remove_dir(&to).unwrap();
        rename_noreplace(&from, &to).unwrap();
        assert_eq!(std::fs::read_to_string(to.join("board")).unwrap(), "source");
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::write(&from, "source").unwrap();
        std::os::unix::fs::symlink("missing", &to).unwrap();
        assert!(rename_noreplace(&from, &to).is_err());
        assert_eq!(std::fs::read_to_string(from).unwrap(), "source");
        assert_eq!(std::fs::read_link(to).unwrap(), Path::new("missing"));
    }

    #[test]
    fn embedded_nul_is_rejected_without_moving_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        std::fs::write(&from, "source").unwrap();
        let to = dir.path().join("to\0suffix");
        assert_eq!(
            rename_noreplace(&from, &to).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(std::fs::read_to_string(&from).unwrap(), "source");
        assert!(!dir.path().join("to").exists());
        assert_eq!(
            rename_noreplace(&dir.path().join("from\0suffix"), &dir.path().join("to"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    // Linux filesystems permit arbitrary byte names; macOS rejects creating
    // these names before a rename can be attempted.
    #[cfg(target_os = "linux")]
    #[test]
    fn native_non_utf8_names_are_preserved() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join(OsString::from_vec(b"from-\xff".to_vec()));
        let to = dir.path().join(OsString::from_vec(b"to-\xfe".to_vec()));
        std::fs::write(&from, "source").unwrap();
        rename_noreplace(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(std::fs::read_to_string(to).unwrap(), "source");
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn native_path_encoding_preserves_non_utf8_bytes() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let path = std::path::PathBuf::from(OsString::from_vec(b"name-\xff".to_vec()));
        assert_eq!(native_path(&path).unwrap().as_bytes(), b"name-\xff");
    }
}

#[cfg(all(test, not(any(target_os = "linux", target_os = "macos", windows))))]
mod unsupported_tests {
    use super::*;

    #[test]
    fn unsupported_platform_refuses_without_a_fallback() {
        assert_eq!(
            rename_noreplace(Path::new("from"), Path::new("to"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }
}
