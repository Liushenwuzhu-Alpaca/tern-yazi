//! Process-boundary helpers: libc wrappers, private directory traversal, signal capture and the
//! JSON accessors shared by the helper modes.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicI32, Ordering};

// Linux PTY and signal operations stay at the process boundary.
#[link(name = "util")]
extern "C" {}

static SIGNAL: AtomicI32 = AtomicI32::new(0);
extern "C" fn on_signal(signal: libc::c_int) {
    SIGNAL.store(signal, Ordering::Relaxed);
}

pub(crate) fn cstr(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
}

pub(crate) fn last_error() -> io::Error {
    io::Error::last_os_error()
}

pub(crate) fn private_dir(path: &Path) -> io::Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime directory must be absolute",
        ));
    }
    let root = cstr(OsStr::new("/"))?;
    let fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(last_error());
    }
    let mut current = unsafe { OwnedFd::from_raw_fd(fd) };
    for part in path.components() {
        match part {
            Component::RootDir => continue,
            Component::Normal(name) => {
                let name = cstr(name)?;
                let next = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if next < 0 {
                    return Err(last_error());
                }
                current = unsafe { OwnedFd::from_raw_fd(next) };
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsafe runtime directory path",
                ))
            }
        }
    }
    Ok(current)
}

pub(crate) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub(crate) fn json_text<'a>(value: &'a serde_json::Value, key: &str) -> io::Result<&'a str> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid("invalid native shell metadata"))
}

pub(crate) fn json_number(value: &serde_json::Value, key: &str) -> io::Result<u64> {
    value
        .get(key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| invalid("invalid native shell metadata"))
}

pub(crate) fn utf8(value: &OsStr) -> io::Result<&str> {
    value.to_str().ok_or_else(|| {
        invalid("native shell protocol requires UTF-8 arguments, paths and environment")
    })
}

pub(crate) fn install_signals() {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// The last captured termination signal, or 0.
pub(crate) fn pending_signal() -> i32 {
    SIGNAL.load(Ordering::Relaxed)
}
