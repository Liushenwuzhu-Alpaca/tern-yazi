//! The private runtime inbox shared with the Yazi plugin: snapshots, replies, bridge records and
//! the owned cleanup of every file this supervisor created.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::sys::{cstr, invalid, json_number, json_text, last_error, private_dir};

pub(crate) const NATIVE_CAP: usize = 256 * 1024;
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
            let names = [
                format!("state-{cid}.json"),
                format!("state-{cid}.tmp"),
                format!("listing-{cid}.json"),
                format!("listing-{cid}.tmp"),
                format!("managed-{cid}.json"),
                format!("managed-{cid}.lease"),
                format!("managed-{cid}.stop"),
                format!("managed-{cid}.lock"),
            ];
            if names.iter().any(|name| inbox.exists(name)) {
                continue;
            }
            let token = random[4..]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let lock = format!("managed-{cid}.lock");
            match inbox.create(&lock) {
                Ok(mut file) => {
                    inbox.cid = cid;
                    inbox.token = token;
                    inbox.cleanup = true;
                    file.write_all(inbox.token.as_bytes())?;
                    inbox.create(&format!("managed-{}.lease", inbox.cid))?;
                    inbox.create(&format!("managed-{}.stop", inbox.cid))?;
                    return Ok(inbox);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
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

    pub(crate) fn stop_requested(&self) -> bool {
        self.read(&format!("managed-{}.stop", self.cid), 1024, true)
            .is_ok_and(|(bytes, _)| {
                std::str::from_utf8(&bytes).is_ok_and(|text| text.trim() == self.token)
            })
    }

    /// The record a standalone native block uses to find and authenticate its supervisor.
    pub(crate) fn identity(&self, pane: u64, cwd: &Path) -> serde_json::Value {
        serde_json::json!({"client_id":self.cid,"token":self.token,"owner_kind":"standalone",
            "owner_pane":pane,"native_pane":pane,"cwd":cwd.to_string_lossy()})
    }

    pub(crate) fn heartbeat(&self, pane: u64, cwd: &Path, seq: u64) -> io::Result<()> {
        let name = format!("managed-{}.json", self.cid);
        let tmp = format!("{name}.tmp");
        let mut value = self.identity(pane, cwd);
        value["seq"] = seq.into();
        let mut file = self.create(&tmp)?;
        if let Err(error) = serde_json::to_writer(&mut file, &value)
            .map_err(io::Error::other)
            .and_then(|_| file.flush())
        {
            self.remove(&tmp);
            return Err(error);
        }
        drop(file);
        let source = cstr(OsStr::new(&tmp))?;
        let target = cstr(OsStr::new(&name))?;
        if unsafe {
            libc::renameat(
                self.dir.as_raw_fd(),
                source.as_ptr(),
                self.dir.as_raw_fd(),
                target.as_ptr(),
            )
        } != 0
        {
            self.remove(&tmp);
            return Err(last_error());
        }
        Ok(())
    }

    pub(crate) fn ready(&self) -> io::Result<bool> {
        // Both snapshots inherit the caller's umask inside the private inbox.
        let (bytes, _) = match self.read(&format!("state-{}.json", self.cid), NATIVE_CAP, false) {
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

    pub(crate) fn path(&self) -> io::Result<PathBuf> {
        fs::read_link(format!("/proc/self/fd/{}", self.dir.as_raw_fd()))
    }

    pub(crate) fn connect(path: &Path, cid: &str, token: &str) -> io::Result<Self> {
        if cid.is_empty()
            || !cid.bytes().all(|v| v.is_ascii_digit())
            || token.len() != 32
            || !token.bytes().all(|v| v.is_ascii_hexdigit())
        {
            return Err(invalid("invalid native session identity"));
        }
        let dir = private_dir(path)?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(dir.as_raw_fd(), &mut stat) } != 0 {
            return Err(last_error());
        }
        if stat.st_uid != unsafe { libc::geteuid() } || stat.st_mode & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe native shell inbox",
            ));
        }
        Ok(Self {
            dir,
            cid: cid.to_owned(),
            token: token.to_owned(),
            cleanup: false,
        })
    }

    pub(crate) fn live_pane(&self, owner: u64) -> io::Result<u64> {
        let (health, modified) =
            self.read(&format!("managed-{}.json", self.cid), NATIVE_CAP, true)?;
        let value: serde_json::Value = serde_json::from_slice(&health).map_err(io::Error::other)?;
        let fresh = |time: SystemTime| {
            SystemTime::now()
                .duration_since(time)
                .is_ok_and(|age| age < Duration::from_secs(8))
        };
        if !fresh(modified)
            || json_text(&value, "client_id")? != self.cid
            || json_text(&value, "token")? != self.token
            || json_number(&value, "owner_pane")? != owner
        {
            return Err(invalid("native shell session is no longer live"));
        }
        let (lease, modified) = self.read(&format!("managed-{}.lease", self.cid), 1024, true)?;
        let mut fields = std::str::from_utf8(&lease)
            .map_err(io::Error::other)?
            .split_whitespace();
        if !fresh(modified) || fields.next() != Some(self.token.as_str()) {
            return Err(invalid("native shell window lease is no longer live"));
        }
        fields
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .ok_or_else(|| invalid("invalid native shell window pane"))
    }

    pub(crate) fn atomic_json(
        &self,
        name: &str,
        value: &serde_json::Value,
        replace: bool,
    ) -> io::Result<File> {
        let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
        if bytes.len() > NATIVE_CAP {
            return Err(invalid("native shell request exceeds 256 KiB"));
        }
        let tmp = format!("{name}.tmp");
        let mut file = self.create(&tmp)?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.flush()?;
            let source = cstr(OsStr::new(&tmp))?;
            let target = cstr(OsStr::new(name))?;
            let published = unsafe {
                if replace {
                    libc::renameat(
                        self.dir.as_raw_fd(),
                        source.as_ptr(),
                        self.dir.as_raw_fd(),
                        target.as_ptr(),
                    )
                } else {
                    libc::linkat(
                        self.dir.as_raw_fd(),
                        source.as_ptr(),
                        self.dir.as_raw_fd(),
                        target.as_ptr(),
                        0,
                    )
                }
            };
            if published != 0 {
                return Err(last_error());
            }
            Ok(())
        })();
        self.remove(&tmp);
        result?;
        Ok(file)
    }

    pub(crate) fn shim(&self, helper: &Path) -> io::Result<PathBuf> {
        let name = format!("shim-{}-{}", self.cid, self.token);
        let cname = cstr(OsStr::new(&name))?;
        if unsafe { libc::mkdirat(self.dir.as_raw_fd(), cname.as_ptr(), 0o700) } != 0 {
            return Err(last_error());
        }
        let directory = private_dir(&self.path()?.join(&name))?;
        let source = cstr(helper.as_os_str())?;
        let target = cstr(OsStr::new("sh"))?;
        if unsafe { libc::symlinkat(source.as_ptr(), directory.as_raw_fd(), target.as_ptr()) } != 0
        {
            return Err(last_error());
        }
        Ok(self.path()?.join(name))
    }

    pub(crate) fn remove_tree(&self, name: &str) {
        let Ok(name) = cstr(OsStr::new(name)) else {
            return;
        };
        let fd = unsafe {
            libc::openat(
                self.dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return;
        }
        let dir = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(dir.as_raw_fd(), &mut stat) } != 0
            || stat.st_uid != unsafe { libc::geteuid() }
        {
            return;
        }
        if let Ok(entries) = fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd())) {
            for entry in entries.flatten() {
                let Ok(child) = cstr(&entry.file_name()) else {
                    continue;
                };
                // Owned staging contains only regular files and the private sh symlink.
                unsafe {
                    libc::unlinkat(dir.as_raw_fd(), child.as_ptr(), 0);
                }
            }
        }
        unsafe {
            libc::unlinkat(self.dir.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR);
        }
    }
}


impl Drop for Inbox {
    fn drop(&mut self) {
        if !self.cleanup {
            return;
        }
        for name in [
            format!("managed-{}.json", self.cid),
            format!("managed-{}.json.tmp", self.cid),
            format!("managed-{}.lease", self.cid),
            format!("managed-{}.stop", self.cid),
            format!("managed-{}.lock", self.cid),
            format!("state-{}.json", self.cid),
            format!("state-{}.tmp", self.cid),
            format!("listing-{}.json", self.cid),
            format!("listing-{}.tmp", self.cid),
        ] {
            self.remove(&name);
        }
        let prefix = format!("reply-{}-{}-", self.cid, self.token);
        let native_prefix = format!("native-shell-{}-{}-", self.cid, self.token);
        let prep_prefix = format!("prep-{}-{}", self.cid, self.token);
        let shim = format!("shim-{}-{}", self.cid, self.token);
        // Readdir through the pinned directory rather than a replaceable pathname.
        if let Ok(entries) = fs::read_dir(format!("/proc/self/fd/{}", self.dir.as_raw_fd())) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with(&native_prefix)
                        && [".json", ".json.tmp", ".started", ".result", ".result.tmp"]
                            .iter()
                            .any(|suffix| name.ends_with(suffix))
                    {
                        self.remove(name);
                    }
                    if name == shim
                        || name == prep_prefix
                        || name == format!("{prep_prefix}.cleanup")
                        || name
                            .strip_prefix(&format!("{prep_prefix}-"))
                            .is_some_and(|suffix| {
                                let suffix = suffix.strip_suffix(".cleanup").unwrap_or(suffix);
                                suffix.split('-').all(|part| {
                                    !part.is_empty() && part.bytes().all(|v| v.is_ascii_digit())
                                })
                            })
                    {
                        self.remove_tree(name);
                    }
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
    use super::NATIVE_CAP;
    use std::io::{self, Write};

    #[test]
    fn initial_snapshot_waits_for_matching_listing_epoch_and_revision() {
        let fixture = Fixture::new();
        let state = serde_json::json!({
            "client_id": fixture.inbox.cid, "seq": 1,
            "listing_epoch": "current", "listing_revision": 2
        });
        fixture
            .inbox
            .atomic_json("state-12345.json", &state, false)
            .unwrap();
        assert!(!fixture.inbox.ready().unwrap());
        let mut listing = serde_json::json!({
            "epoch": "current", "revision": 1, "cwd": "/tmp/files", "files": ["alpha.txt"]
        });
        fixture
            .inbox
            .atomic_json("listing-12345.json", &listing, false)
            .unwrap();
        assert!(!fixture.inbox.ready().unwrap());
        listing["epoch"] = "previous".into();
        listing["revision"] = 2.into();
        fixture
            .inbox
            .atomic_json("listing-12345.json", &listing, true)
            .unwrap();
        assert!(!fixture.inbox.ready().unwrap());
        listing["epoch"] = "current".into();
        fixture
            .inbox
            .atomic_json("listing-12345.json", &listing, true)
            .unwrap();
        assert!(fixture.inbox.ready().unwrap());
    }

    #[test]
    fn native_handoff_publication_does_not_replace_an_existing_request() {
        let fixture = Fixture::new();
        let first = serde_json::json!({"id": "first", "argv": ["echo", "first"]});
        let second = serde_json::json!({"id": "second", "argv": ["echo", "second"]});
        fixture
            .inbox
            .atomic_json("native-shell-test.json", &first, false)
            .unwrap();
        let error = fixture
            .inbox
            .atomic_json("native-shell-test.json", &second, false)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        let (bytes, _) = fixture
            .inbox
            .read("native-shell-test.json", NATIVE_CAP, true)
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            first
        );
        assert!(!fixture.inbox.exists("native-shell-test.json.tmp"));
    }

    #[test]
    fn stop_before_initial_snapshot_requires_the_allocated_token() {
        let fixture = Fixture::new();
        assert!(!fixture.inbox.stop_requested());
        let name = "managed-12345.stop";
        fixture
            .inbox
            .create(name)
            .unwrap()
            .write_all(b"wrong-token\n")
            .unwrap();
        assert!(!fixture.inbox.stop_requested());
        fixture.inbox.remove(name);
        let mut file = fixture.inbox.create(name).unwrap();
        writeln!(file, "{}", fixture.inbox.token).unwrap();
        drop(file);
        assert!(fixture.inbox.stop_requested());
    }
}
