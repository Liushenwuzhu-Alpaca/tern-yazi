//! The standalone supervisor: Yazi owned by a native Tern plugin block (`--standalone-pane`). The
//! block discovers it through the heartbeat and keeps it alive with its lease.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crate::inbox::{Inbox, NATIVE_CAP};
use crate::supervisor::{self, Launch, Startup, Step};
use crate::sys::{cstr, invalid, last_error, pending_signal};

// A stable private lock inode serializes starts across host/backend reloads.
// It is intentionally retained: unlinking a flock file would allow split locks.
pub(crate) struct StandaloneBinding<'a> {
    inbox: &'a Inbox,
    pane: u64,
    _lock: File,
}

impl<'a> StandaloneBinding<'a> {
    pub(crate) fn new(inbox: &'a Inbox, pane: u64, cwd: &Path) -> io::Result<Self> {
        let name = cstr(OsStr::new(&format!("native-{pane}.lock")))?;
        let fd = unsafe {
            libc::openat(
                inbox.dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(last_error());
        }
        let lock = unsafe { File::from_raw_fd(fd) };
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(lock.as_raw_fd(), &mut stat) } != 0 {
            return Err(last_error());
        }
        if stat.st_uid != unsafe { libc::geteuid() }
            || stat.st_mode & libc::S_IFMT != libc::S_IFREG
            || stat.st_mode & 0o077 != 0
        {
            return Err(invalid("unsafe standalone native pane lock"));
        }
        if unsafe { libc::fchmod(lock.as_raw_fd(), 0o600) } != 0 {
            return Err(last_error());
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = last_error();
            return Err(if error.kind() == io::ErrorKind::WouldBlock {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "standalone supervisor already owns this native pane",
                )
            } else {
                error
            });
        }
        let binding = Self {
            inbox,
            pane,
            _lock: lock,
        };
        let name = format!("native-{pane}.json");
        // A crashed previous owner may have left an incomplete atomic write.
        inbox.remove(&format!("{name}.tmp"));
        let mut identity = inbox.identity(pane, cwd);
        identity["source_cwd"] = identity["cwd"].clone();
        // The pane lock grants exclusive publication, including replacement after a crash.
        if let Err(error) = inbox.atomic_json(&name, &identity, true) {
            inbox.remove(&format!("{name}.tmp"));
            return Err(error);
        }
        Ok(binding)
    }
}

impl Drop for StandaloneBinding<'_> {
    fn drop(&mut self) {
        let name = format!("native-{}.json", self.pane);
        if let Ok((bytes, _)) = self.inbox.read(&name, NATIVE_CAP, true) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if value["client_id"].as_str() == Some(self.inbox.cid.as_str())
                    && value["token"].as_str() == Some(self.inbox.token.as_str())
                    && value["native_pane"].as_u64() == Some(self.pane)
                {
                    self.inbox.remove(&name);
                }
            }
        }
    }
}

pub(crate) fn run(launch: &Launch, pane: u64) -> io::Result<i32> {
    let caller_umask = supervisor::prepare();
    let inbox = Inbox::new()?;
    let binding = StandaloneBinding::new(&inbox, pane, &launch.cwd)?;
    let mut backend = supervisor::spawn(&inbox, launch, pane, caller_umask)?;
    let mut startup = Startup::new();
    let mut heartbeat = Instant::now();
    let mut seq = 0u64;
    let mut last_lease: Option<(SystemTime, Vec<u8>)> = None;
    let mut lease_seen: Option<Instant> = None;
    let mut last_size = (0u16, 0u16);
    let result: io::Result<i32> = loop {
        let now = Instant::now();
        let signal = pending_signal();
        if signal != 0 {
            backend.signal(signal);
            break Ok(128 + signal);
        }
        if inbox.stop_requested() {
            break Ok(0);
        }
        let published_at = match startup.poll(&inbox, &mut backend) {
            Ok(Step::Ready(at)) => at,
            Ok(Step::Starting) => {
                backend.wait_readable(25);
                continue;
            }
            Ok(Step::Exited(code)) => break Ok(code),
            Err(error) => break Err(error),
        };
        if seq == 0 || now.duration_since(heartbeat) >= Duration::from_millis(500) {
            seq += 1;
            if let Err(error) = inbox.heartbeat(pane, &launch.cwd, seq) {
                break Err(error);
            }
            heartbeat = now;
        }
        if let Ok((bytes, modified)) =
            inbox.read(&format!("managed-{}.lease", inbox.cid), 1024, true)
        {
            if let Some(size) = lease_size(&bytes, &inbox.token, pane) {
                if last_lease
                    .as_ref()
                    .is_none_or(|old| old.0 != modified || old.1 != bytes)
                {
                    lease_seen = Some(now);
                    last_lease = Some((modified, bytes));
                }
                if size != last_size {
                    backend.resize(size.0, size.1);
                    last_size = size;
                }
            }
        }
        match lease_seen {
            Some(last) if now.duration_since(last) >= Duration::from_secs(8) => {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native window lease expired; stopping Yazi",
                ))
            }
            None if now.duration_since(published_at) >= Duration::from_secs(15) => {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no native window attached within 15s; enable the tern-yazi Tern plugin",
                ))
            }
            _ => {}
        }
        backend.wait_readable(25);
    };
    startup.report(&backend, &result);
    supervisor::shutdown(backend);
    drop(binding);
    drop(inbox);
    result
}

/// The PTY size a valid lease (`token pane rows cols`) grants this supervisor.
fn lease_size(bytes: &[u8], token: &str, pane: u64) -> Option<(u16, u16)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut fields = text.split_whitespace();
    if fields.next() != Some(token) || fields.next()?.parse::<u64>().ok()? != pane {
        return None;
    }
    let mut size = || fields.next().and_then(|v| v.parse::<u16>().ok()).unwrap_or(0);
    Some((size(), size()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::fixture::Fixture;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn heartbeat_publishes_the_standalone_identity() {
        let fixture = Fixture::new();
        fixture
            .inbox
            .heartbeat(42, Path::new("/tmp/files"), 7)
            .unwrap();
        let (bytes, _) = fixture
            .inbox
            .read("managed-12345.json", NATIVE_CAP, true)
            .unwrap();
        let health: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(health["client_id"], fixture.inbox.cid);
        assert_eq!(health["token"], fixture.inbox.token);
        assert_eq!(health["owner_pane"], 42);
        assert_eq!(health["native_pane"], 42);
        assert_eq!(health["owner_kind"], "standalone");
        assert_eq!(health["cwd"], "/tmp/files");
        assert_eq!(health["seq"], 7);
    }

    #[test]
    fn lease_grants_size_only_for_this_token_and_pane() {
        let token = "a".repeat(32);
        assert_eq!(lease_size(format!("{token} 42 30 100").as_bytes(), &token, 42), Some((30, 100)));
        assert_eq!(lease_size(format!("{token} 42").as_bytes(), &token, 42), Some((0, 0)));
        assert_eq!(lease_size(format!("{token} 41 30 100").as_bytes(), &token, 42), None);
        assert_eq!(lease_size(b"wrong 42 30 100", &token, 42), None);
    }

    #[test]
    fn native_binding_is_private_and_serializes_reload_starts() {
        let fixture = Fixture::new();
        let binding = StandaloneBinding::new(&fixture.inbox, 42, Path::new("/tmp/files")).unwrap();
        let (bytes, _) = fixture
            .inbox
            .read("native-42.json", NATIVE_CAP, true)
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["client_id"], fixture.inbox.cid);
        assert_eq!(value["token"], fixture.inbox.token);
        assert_eq!(value["owner_kind"], "standalone");
        assert_eq!(value["owner_pane"], 42);
        assert_eq!(value["native_pane"], 42);
        assert_eq!(value["source_cwd"], "/tmp/files");
        assert_eq!(
            fs::metadata(fixture.path.join("native-42.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let error = StandaloneBinding::new(&fixture.inbox, 42, Path::new("/tmp/files"))
            .err()
            .expect("a reload must not acquire an already owned native pane");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        drop(binding);
        assert!(!fixture.inbox.exists("native-42.json"));
        assert!(fixture.inbox.exists("native-42.lock"));
        let next = StandaloneBinding::new(&fixture.inbox, 42, Path::new("/tmp/files")).unwrap();
        drop(next);
        assert!(!fixture.inbox.exists("native-42.json"));
    }

    #[test]
    fn native_binding_cleanup_does_not_remove_another_session_token() {
        let fixture = Fixture::new();
        let binding = StandaloneBinding::new(&fixture.inbox, 42, Path::new("/tmp/files")).unwrap();
        let mut replacement = fixture
            .inbox
            .identity(42, Path::new("/tmp/files"));
        replacement["token"] = "b".repeat(32).into();
        fixture
            .inbox
            .atomic_json("native-42.json", &replacement, true)
            .unwrap();
        drop(binding);
        assert!(fixture.inbox.exists("native-42.json"));
    }

    #[test]
    fn native_binding_replaces_crashed_owner_record_and_partial_write() {
        let fixture = Fixture::new();
        let mut previous = fixture
            .inbox
            .identity(42, Path::new("/tmp/previous"));
        previous["token"] = "b".repeat(32).into();
        fixture
            .inbox
            .atomic_json("native-42.json", &previous, false)
            .unwrap();
        drop(fixture.inbox.create("native-42.json.tmp").unwrap());
        let binding =
            StandaloneBinding::new(&fixture.inbox, 42, Path::new("/tmp/current")).unwrap();
        let (bytes, _) = fixture
            .inbox
            .read("native-42.json", NATIVE_CAP, true)
            .unwrap();
        let current: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(current["token"], fixture.inbox.token);
        assert_eq!(current["source_cwd"], "/tmp/current");
        assert!(!fixture.inbox.exists("native-42.json.tmp"));
        drop(binding);
        assert!(!fixture.inbox.exists("native-42.json"));
    }
}
