//! The private runtime inbox shared with the Yazi plugin: snapshots, replies, and the owned
//! cleanup of every file this supervisor created.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::sys::{cstr, last_error, private_dir};

pub(crate) const STATE_CAP: usize = 256 * 1024;
pub(crate) const LISTING_CAP: usize = 16 * 1024 * 1024;

/// Inode, size and modification time (seconds, nanoseconds) of an inbox file.
pub(crate) type FileVersion = (u64, i64, i64, i64);

pub(crate) struct Inbox {
    pub(crate) dir: OwnedFd,
    pub(crate) cid: String,
    pub(crate) token: String,
    pub(crate) cleanup: bool,
}

impl Inbox {
    pub(crate) fn new() -> io::Result<Self> {
        let runtime = env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty());
        let base = runtime
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let parent = private_dir(&base)?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(parent.as_raw_fd(), &mut stat) } != 0 {
            return Err(last_error());
        }
        let uid = unsafe { libc::geteuid() };
        if runtime.is_some() && (stat.st_uid != uid || stat.st_mode & 0o022 != 0) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "XDG_RUNTIME_DIR must be user-owned and not writable by others",
            ));
        }
        let name = cstr(OsStr::new("tern-yazi"))?;
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0
            && last_error().raw_os_error() != Some(libc::EEXIST)
        {
            return Err(last_error());
        }
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(last_error());
        }
        let dir = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::fstat(dir.as_raw_fd(), &mut stat) } != 0 {
            return Err(last_error());
        }
        if stat.st_uid != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "tern-yazi runtime directory belongs to another user",
            ));
        }
        if unsafe { libc::fchmod(dir.as_raw_fd(), 0o700) } != 0 {
            return Err(last_error());
        }
        let mut inbox = Self {
            dir,
            cid: String::new(),
            token: String::new(),
            cleanup: false,
        };
        for _ in 0..128 {
            let mut random = [0u8; 20];
            File::open("/dev/urandom")?.read_exact(&mut random)?;
            let cid = (u32::from_ne_bytes(random[..4].try_into().unwrap()) & 0x3fff_ffff)
                .max(1)
                .to_string();
            if [format!("state-{cid}.json"), format!("listing-{cid}.json")]
                .iter()
                .any(|name| inbox.exists(name))
            {
                continue;
            }
            // The lock file is the ID reservation: its exclusive create fails for a taken ID.
            match inbox.create(&format!("client-{cid}.lock")) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
            inbox.cid = cid;
            inbox.token = random[4..]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            inbox.cleanup = true;
            return Ok(inbox);
        }
        Err(io::Error::other(
            "unable to allocate an unused Yazi client ID",
        ))
    }

    pub(crate) fn exists(&self, name: &str) -> bool {
        let Ok(name) = cstr(OsStr::new(name)) else {
            return true;
        };
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        unsafe {
            libc::fstatat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            ) == 0
        }
    }

    pub(crate) fn create(&self, name: &str) -> io::Result<File> {
        let name = cstr(OsStr::new(name))?;
        let fd = unsafe {
            libc::openat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(last_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(last_error());
        }
        Ok(file)
    }

    pub(crate) fn read(
        &self,
        name: &str,
        cap: usize,
        private_mode: bool,
    ) -> io::Result<(Vec<u8>, SystemTime)> {
        let name = cstr(OsStr::new(name))?;
        let fd = unsafe {
            libc::openat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(last_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.len() > cap as u64
            || (private_mode && metadata.mode() & 0o077 != 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe managed session file",
            ));
        }
        let modified = metadata.modified()?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(cap as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > cap {
            return Err(io::Error::other("oversized managed session file"));
        }
        Ok((bytes, modified))
    }

    /// Identifies a file version without reading it.
    pub(crate) fn stamp(&self, name: &str) -> Option<FileVersion> {
        let name = cstr(OsStr::new(name)).ok()?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let found = unsafe {
            libc::fstatat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0;
        found.then_some((stat.st_ino, stat.st_size, stat.st_mtime, stat.st_mtime_nsec))
    }

    pub(crate) fn remove(&self, name: &str) {
        let Ok(name) = cstr(OsStr::new(name)) else {
            return;
        };
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
            && stat.st_uid == unsafe { libc::geteuid() }
            && stat.st_mode & libc::S_IFMT == libc::S_IFREG
        {
            unsafe {
                libc::unlinkat(self.dir.as_raw_fd(), name.as_ptr(), 0);
            }
        }
    }

    pub(crate) fn ready(&self) -> io::Result<bool> {
        // Both snapshots inherit the caller's umask inside the private inbox.
        let (bytes, _) = match self.read(&format!("state-{}.json", self.cid), STATE_CAP, false) {
            Ok(snapshot) => snapshot,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let state: serde_json::Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let valid_client = state.get("client_id").is_some_and(|cid| {
            cid.as_str() == Some(self.cid.as_str())
                || cid
                    .as_u64()
                    .is_some_and(|id| self.cid.parse::<u64>().ok() == Some(id))
        });
        let Some(epoch) = state
            .get("listing_epoch")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
        else {
            return Ok(false);
        };
        let Some(revision) = state
            .get("listing_revision")
            .and_then(|v| v.as_u64())
            .filter(|v| *v > 0)
        else {
            return Ok(false);
        };
        if !valid_client
            || state
                .get("seq")
                .and_then(|v| v.as_u64())
                .is_none_or(|seq| seq == 0)
        {
            return Ok(false);
        }
        let (bytes, _) = match self.read(&format!("listing-{}.json", self.cid), LISTING_CAP, false)
        {
            Ok(snapshot) => snapshot,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let listing: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let valid_files = listing.get("files").is_some_and(|files| {
            files.is_array() || files.as_object().is_some_and(|files| files.is_empty())
        });
        Ok(listing.get("epoch").and_then(|v| v.as_str()) == Some(epoch)
            && listing.get("revision").and_then(|v| v.as_u64()) == Some(revision)
            && listing.get("cwd").and_then(|v| v.as_str()).is_some()
            && valid_files)
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        if !self.cleanup {
            return;
        }
        for name in [
            format!("state-{}.json", self.cid),
            format!("state-{}.tmp", self.cid),
            format!("listing-{}.json", self.cid),
            format!("listing-{}.tmp", self.cid),
            format!("client-{}.lock", self.cid),
        ] {
            self.remove(&name);
        }
        let prefix = format!("reply-{}-{}-", self.cid, self.token);
        // Readdir through the pinned directory rather than a replaceable pathname.
        if let Ok(entries) = fs::read_dir(format!("/proc/self/fd/{}", self.dir.as_raw_fd())) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with(&prefix)
                        && (name.ends_with(".json") || name.ends_with(".json.tmp"))
                    {
                        self.remove(name);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    use super::Inbox;
    use crate::sys::private_dir;
    use std::env;
    use std::fs;
    use std::os::unix::fs::DirBuilderExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A private inbox in a throwaway directory.
    pub(crate) struct Fixture {
        pub(crate) inbox: Inbox,
        pub(crate) path: PathBuf,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = env::temp_dir().join(format!(
                "tern-yazi-unit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self {
                inbox: Inbox {
                    dir: private_dir(&path).unwrap(),
                    cid: "12345".to_owned(),
                    token: "a".repeat(32),
                    cleanup: false,
                },
                path,
            }
        }

        /// Publishes `value` as the plugin does: write a temporary file, then rename it.
        pub(crate) fn publish(&self, name: &str, value: &serde_json::Value) {
            let tmp = self.path.join(format!("{name}.tmp"));
            fs::write(&tmp, serde_json::to_vec(value).unwrap()).unwrap();
            fs::rename(tmp, self.path.join(name)).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::Fixture;

    #[test]
    fn initial_snapshot_waits_for_matching_listing_epoch_and_revision() {
        let fixture = Fixture::new();
        let state = serde_json::json!({
            "client_id": fixture.inbox.cid, "seq": 1,
            "listing_epoch": "current", "listing_revision": 2
        });
        fixture.publish("state-12345.json", &state);
        assert!(!fixture.inbox.ready().unwrap());
        let mut listing = serde_json::json!({
            "epoch": "current", "revision": 1, "cwd": "/tmp/files", "files": ["alpha.txt"]
        });
        fixture.publish("listing-12345.json", &listing);
        assert!(!fixture.inbox.ready().unwrap());
        listing["epoch"] = "previous".into();
        listing["revision"] = 2.into();
        fixture.publish("listing-12345.json", &listing);
        assert!(!fixture.inbox.ready().unwrap());
        listing["epoch"] = "current".into();
        fixture.publish("listing-12345.json", &listing);
        assert!(fixture.inbox.ready().unwrap());
    }
}
