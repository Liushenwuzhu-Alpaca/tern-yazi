//! Process-boundary helpers: libc wrappers, private directory traversal, signal capture and the
//! JSON accessors shared by the helper modes.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

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

pub(crate) fn utf8(value: &OsStr) -> io::Result<&str> {
    value
        .to_str()
        .ok_or_else(|| invalid("the launcher requires UTF-8 arguments and paths"))
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

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn on_stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// Catches the terminal stop signal (`kill -TSTP`) as a suspension request for the main loop
/// instead of stopping the process under the surface.
pub(crate) fn install_job_control() {
    unsafe {
        libc::signal(libc::SIGTSTP, on_stop as *const () as libc::sighandler_t);
    }
}

/// Asks the main loop to suspend, as Ctrl+Z does.
pub(crate) fn request_stop() {
    STOP.store(true, Ordering::Relaxed);
}

/// Whether a suspension was requested since the last call.
pub(crate) fn take_stop_request() -> bool {
    STOP.swap(false, Ordering::Relaxed)
}

/// Stops this process group, as a shell expects of a foreground job, and returns once it
/// continues. The default disposition applies meanwhile: a handler would not stop anything. A
/// group the shell has orphaned discards the stop, so this then returns at once.
pub(crate) fn stop_group() {
    unsafe {
        libc::signal(libc::SIGTSTP, libc::SIG_DFL);
        libc::kill(0, libc::SIGTSTP);
    }
    install_job_control();
    // A request that arrived while stopped is satisfied by having been stopped.
    STOP.store(false, Ordering::Relaxed);
}
