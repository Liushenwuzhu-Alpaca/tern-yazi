use std::env;
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

// Linux PTY and signal operations stay at the process boundary.
#[link(name = "util")]
extern "C" {}

static SIGNAL: AtomicI32 = AtomicI32::new(0);
extern "C" fn on_signal(signal: libc::c_int) {
    SIGNAL.store(signal, Ordering::Relaxed);
}

fn cstr(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

fn private_dir(path: &Path) -> io::Result<OwnedFd> {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Owner {
    Shell(u64),
    Standalone(u64),
}

impl Owner {
    fn pane(self) -> u64 {
        match self {
            Self::Shell(pane) | Self::Standalone(pane) => pane,
        }
    }
}

struct LaunchArgs {
    real: OsString,
    args: Vec<OsString>,
    standalone_pane: Option<u64>,
}

fn launch_args(mut input: impl Iterator<Item = OsString>) -> io::Result<LaunchArgs> {
    let mut option = input.next();
    let standalone_pane = if option.as_deref() == Some(OsStr::new("--standalone-pane")) {
        let pane = input
            .next()
            .and_then(|value| value.to_str().and_then(|value| value.parse::<u64>().ok()))
            .filter(|pane| *pane > 0)
            .ok_or_else(|| invalid("expected a positive native pane ID after --standalone-pane"))?;
        option = input.next();
        Some(pane)
    } else {
        None
    };
    if option.as_deref() != Some(OsStr::new("--real")) {
        return Err(invalid("expected --real ORIGINAL -- [Yazi arguments]"));
    }
    let real = input
        .next()
        .ok_or_else(|| invalid("missing original Yazi binary"))?;
    if input.next().as_deref() != Some(OsStr::new("--")) {
        return Err(invalid("expected -- before Yazi arguments"));
    }
    Ok(LaunchArgs {
        real,
        args: input.collect(),
        standalone_pane,
    })
}

struct Inbox {
    dir: OwnedFd,
    cid: String,
    token: String,
    cleanup: bool,
}

impl Inbox {
    fn new() -> io::Result<Self> {
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

    fn exists(&self, name: &str) -> bool {
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

    fn create(&self, name: &str) -> io::Result<File> {
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

    fn read(
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

    fn remove(&self, name: &str) {
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

    fn stop_requested(&self) -> bool {
        self.read(&format!("managed-{}.stop", self.cid), 1024, true)
            .is_ok_and(|(bytes, _)| {
                std::str::from_utf8(&bytes).is_ok_and(|text| text.trim() == self.token)
            })
    }

    fn identity(&self, owner: Owner, cwd: &Path) -> serde_json::Value {
        let mut value = serde_json::json!({"client_id":self.cid,"token":self.token,
            "owner_pane":owner.pane(),"cwd":cwd.to_string_lossy()});
        match owner {
            Owner::Shell(_) => value["owner_kind"] = "shell".into(),
            Owner::Standalone(pane) => {
                value["owner_kind"] = "standalone".into();
                value["native_pane"] = pane.into();
            }
        }
        value
    }

    fn heartbeat(&self, owner: Owner, cwd: &Path, seq: u64) -> io::Result<()> {
        let name = format!("managed-{}.json", self.cid);
        let tmp = format!("{name}.tmp");
        let mut value = self.identity(owner, cwd);
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

    fn ready(&self) -> io::Result<bool> {
        // Both snapshots inherit the caller's umask inside the private inbox.
        let (bytes, _) = match self.read(&format!("state-{}.json", self.cid), NATIVE_CAP, false) {
            Ok(snapshot) => snapshot,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let state: serde_json::Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let valid_client = state.get("client_id").is_some_and(|cid| {
            cid.as_str() == Some(self.cid.as_str())
                || cid.as_u64().is_some_and(|id| self.cid.parse::<u64>().ok() == Some(id))
        });
        let Some(epoch) = state.get("listing_epoch").and_then(|v| v.as_str()).filter(|v| !v.is_empty()) else {
            return Ok(false);
        };
        let Some(revision) = state.get("listing_revision").and_then(|v| v.as_u64()).filter(|v| *v > 0) else {
            return Ok(false);
        };
        if !valid_client || !state.get("seq").and_then(|v| v.as_u64()).is_some_and(|seq| seq > 0) {
            return Ok(false);
        }
        let (bytes, _) = match self.read(&format!("listing-{}.json", self.cid), LISTING_CAP, false) {
            Ok(snapshot) => snapshot,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let listing: serde_json::Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
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

// A stable private lock inode serializes starts across host/backend reloads.
// It is intentionally retained: unlinking a flock file would allow split locks.
struct StandaloneBinding<'a> {
    inbox: &'a Inbox,
    pane: u64,
    _lock: File,
}

impl<'a> StandaloneBinding<'a> {
    fn new(inbox: &'a Inbox, pane: u64, cwd: &Path) -> io::Result<Self> {
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
        let mut identity = inbox.identity(Owner::Standalone(pane), cwd);
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

// A fixed-size startup-only diagnostic tail. No ANSI/control bytes reach stderr.
struct StartupDiagnostic {
    bytes: [u8; 4096],
    next: usize,
    len: usize,
    escape: u8,
}

impl StartupDiagnostic {
    fn new() -> Self {
        Self {
            bytes: [0; 4096],
            next: 0,
            len: 0,
            escape: 0,
        }
    }

    fn push(&mut self, byte: u8) {
        self.bytes[self.next] = byte;
        self.next = (self.next + 1) % self.bytes.len();
        self.len = (self.len + 1).min(self.bytes.len());
    }

    fn feed(&mut self, byte: u8) {
        if byte == 0x1b {
            self.escape = if self.escape == 3 || self.escape == 4 {
                4
            } else {
                1
            };
            return;
        }
        match self.escape {
            1 => {
                self.escape = match byte {
                    b'[' => 2,
                    b']' | b'P' | b'^' | b'_' => 3,
                    b'(' | b')' | b'*' | b'+' => 5,
                    _ => 0,
                }
            }
            2 => {
                if (0x40..=0x7e).contains(&byte) {
                    self.escape = 0;
                }
            }
            3 => {
                if byte == 7 {
                    self.escape = 0;
                }
            }
            4 => self.escape = if byte == b'\\' { 0 } else { 3 },
            5 => self.escape = 0,
            _ => {
                if byte == b'\n' || byte == b'\t' || (0x20..=0x7e).contains(&byte) {
                    self.push(byte);
                } else if byte >= 0x80 {
                    // Byte escapes preserve non-ASCII error paths without terminal control ambiguity.
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    for escaped in [
                        b'\\',
                        b'x',
                        HEX[(byte >> 4) as usize],
                        HEX[(byte & 15) as usize],
                    ] {
                        self.push(escaped);
                    }
                }
            }
        }
    }

    fn print(&self) {
        if self.len == 0 {
            return;
        }
        let mut stderr = io::stderr().lock();
        let _ = stderr.write_all(
            b"tern-yazi: last startup output (ANSI stripped, non-ASCII bytes escaped):\n",
        );
        let start = (self.next + self.bytes.len() - self.len) % self.bytes.len();
        let first = self.len.min(self.bytes.len() - start);
        let _ = stderr.write_all(&self.bytes[start..start + first]);
        let _ = stderr.write_all(&self.bytes[..self.len - first]);
        let _ = stderr.write_all(b"\n");
    }
}

struct Backend {
    pid: libc::pid_t,
    master: OwnedFd,
    status: Option<i32>,
    queries: Vec<u8>,
    stopped: bool,
    diagnostic: StartupDiagnostic,
    capture_startup: bool,
}

impl Backend {
    fn spawn(
        real: &OsStr,
        args: &[OsString],
        cid: &str,
        caller_umask: libc::mode_t,
        bridge: &str,
        path: &OsStr,
    ) -> io::Result<Self> {
        let mut dimensions = libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let mut terminal: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut terminal) } == 0
            && terminal.ws_row > 0
            && terminal.ws_col > 0
        {
            dimensions = terminal;
        }
        let mut master: RawFd = -1;
        let pid = unsafe {
            libc::forkpty(
                &mut master,
                std::ptr::null_mut(),
                std::ptr::null(),
                &dimensions,
            )
        };
        if pid < 0 {
            return Err(last_error());
        }
        if pid == 0 {
            unsafe {
                libc::umask(caller_umask);
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
                libc::signal(libc::SIGHUP, libc::SIG_DFL);
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            }
            let error = Command::new(real)
                .arg("--client-id")
                .arg(cid)
                .args(args)
                .env("PATH", path)
                .env(BRIDGE_ENV, bridge)
                .env("TERM", "xterm-256color")
                .env_remove("TERM_PROGRAM")
                .env_remove("TERM_PROGRAM_VERSION")
                .env_remove("KITTY_WINDOW_ID")
                .env_remove("WEZTERM_PANE")
                .env_remove("TERN_PANE")
                .env_remove("TERN_PANE_SOCKET")
                .env_remove("TERN_WINDOW_SOCKET")
                .env_remove("TERN_IDENTITY")
                .exec();
            eprintln!("tern-yazi: cannot execute original Yazi: {error}");
            unsafe {
                libc::_exit(127);
            }
        }
        let backend = Self {
            pid,
            master: unsafe { OwnedFd::from_raw_fd(master) },
            status: None,
            queries: Vec::with_capacity(32),
            stopped: false,
            diagnostic: StartupDiagnostic::new(),
            capture_startup: true,
        };
        if unsafe { libc::fcntl(backend.master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
            return Err(last_error());
        }
        if unsafe { libc::fcntl(backend.master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(last_error());
        }
        Ok(backend)
    }

    fn poll_child(&mut self) -> io::Result<Option<i32>> {
        if self.status.is_none() {
            let mut status = 0;
            let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
            if result == self.pid {
                self.status = Some(status);
            } else if result < 0 && last_error().kind() != io::ErrorKind::Interrupted {
                return Err(last_error());
            }
        }
        Ok(self.status)
    }

    fn signal(&self, signal: i32) {
        unsafe {
            libc::kill(-self.pid, signal);
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 {
            return;
        }
        let dimensions = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &dimensions);
        }
    }

    fn drain(&mut self) -> io::Result<()> {
        let mut bytes = [0u8; 8192];
        // Bound each poll's work, including a pathological output flood.
        for _ in 0..16 {
            let count = unsafe {
                libc::read(
                    self.master.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if count <= 0 {
                if count == 0 {
                    return Ok(());
                }
                let error = last_error();
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.raw_os_error() == Some(libc::EIO)
                {
                    return Ok(());
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            for byte in &bytes[..count as usize] {
                if self.capture_startup {
                    self.diagnostic.feed(*byte);
                }
                if *byte == 0x1b {
                    self.queries.clear();
                    self.queries.push(*byte);
                    continue;
                }
                if self.queries.is_empty() {
                    continue;
                }
                self.queries.push(*byte);
                let response: Option<&[u8]> = match self.queries.as_slice() {
                    b"\x1b[6n" => Some(b"\x1b[1;1R"),
                    b"\x1b[?6n" => Some(b"\x1b[?1;1R"),
                    b"\x1b[c" | b"\x1b[0c" => Some(b"\x1b[?1;2c"),
                    _ => None,
                };
                if let Some(response) = response {
                    unsafe {
                        libc::write(
                            self.master.as_raw_fd(),
                            response.as_ptr().cast(),
                            response.len(),
                        );
                    }
                    self.queries.clear();
                } else if self.queries.len() >= 16 || byte.is_ascii_alphabetic() {
                    self.queries.clear();
                }
            }
        }
        Ok(())
    }

    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.signal(libc::SIGTERM);
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            let _ = self.poll_child();
            let _ = self.drain();
            if unsafe { libc::kill(-self.pid, 0) } != 0
                && last_error().raw_os_error() == Some(libc::ESRCH)
            {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.signal(libc::SIGKILL);
        if self.status.is_none() {
            loop {
                let mut status = 0;
                if unsafe { libc::waitpid(self.pid, &mut status, 0) } == self.pid {
                    self.status = Some(status);
                    break;
                }
                if last_error().kind() != io::ErrorKind::Interrupted {
                    break;
                }
            }
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.stop();
    }
}

fn child_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

fn passthrough(real: &OsStr, args: &[OsString]) -> io::Result<i32> {
    Err(Command::new(real).args(args).exec())
}

const NATIVE_CAP: usize = 256 * 1024;
const LISTING_CAP: usize = 16 * 1024 * 1024;
const BRIDGE_ENV: &str = "TERN_YAZI_BRIDGE";

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn json_text<'a>(value: &'a serde_json::Value, key: &str) -> io::Result<&'a str> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid("invalid native shell metadata"))
}

fn json_number(value: &serde_json::Value, key: &str) -> io::Result<u64> {
    value
        .get(key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| invalid("invalid native shell metadata"))
}

fn utf8(value: &OsStr) -> io::Result<&str> {
    value.to_str().ok_or_else(|| {
        invalid("native shell protocol requires UTF-8 arguments, paths and environment")
    })
}

fn install_signals() {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

impl Inbox {
    fn path(&self) -> io::Result<PathBuf> {
        fs::read_link(format!("/proc/self/fd/{}", self.dir.as_raw_fd()))
    }

    fn connect(path: &Path, cid: &str, token: &str) -> io::Result<Self> {
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

    fn live_pane(&self, owner: u64) -> io::Result<u64> {
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

    fn atomic_json(
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

    fn shim(&self, helper: &Path) -> io::Result<PathBuf> {
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

    fn remove_tree(&self, name: &str) {
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

struct NativeRequest<'a> {
    inbox: &'a Inbox,
    name: String,
    file: File,
}

impl Drop for NativeRequest<'_> {
    fn drop(&mut self) {
        // Serialize revocation with the terminal helper's final validation and spawn.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_EX);
        }
        self.inbox.remove(&self.name);
        let base = self.name.trim_end_matches(".json");
        self.inbox.remove(&format!("{base}.started"));
        self.inbox.remove(&format!("{base}.result"));
        self.inbox.remove(&format!("{base}.result.tmp"));
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn native_shell(args: &[OsString]) -> io::Result<i32> {
    let Some(raw) = env::var_os(BRIDGE_ENV) else {
        return Err(Command::new("/bin/sh").arg0("sh").args(args).exec());
    };
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1
        || unsafe { libc::isatty(libc::STDOUT_FILENO) } != 1
    {
        return Err(Command::new("/bin/sh").arg0("sh").args(args).exec());
    }
    let bridge: serde_json::Value =
        serde_json::from_slice(raw.as_bytes()).map_err(io::Error::other)?;
    let inbox = Inbox::connect(
        Path::new(json_text(&bridge, "inbox")?),
        json_text(&bridge, "cid")?,
        json_text(&bridge, "token")?,
    )?;
    let owner = json_number(&bridge, "owner_pane")?;
    let pane = inbox.live_pane(owner)?;
    let id = format!("{}-{}-{}", inbox.cid, inbox.token, std::process::id());
    let name = format!("native-shell-{id}.json");
    let argv: Vec<&str> = args
        .iter()
        .map(|arg| utf8(arg))
        .collect::<io::Result<_>>()?;
    let mut environment = serde_json::Map::new();
    for (key, value) in env::vars_os() {
        if key != BRIDGE_ENV {
            environment.insert(
                utf8(&key)?.to_owned(),
                serde_json::Value::String(utf8(&value)?.to_owned()),
            );
        }
    }
    match bridge.get("original_path") {
        Some(serde_json::Value::String(path)) => {
            environment.insert("PATH".into(), serde_json::Value::String(path.clone()));
        }
        Some(serde_json::Value::Null) => {
            environment.remove("PATH");
        }
        _ => return Err(invalid("invalid native shell original PATH")),
    }
    let cwd = env::current_dir()?;
    let umask = unsafe { libc::umask(0) };
    unsafe {
        libc::umask(umask);
    }
    let request = serde_json::json!({"version":1,"id":id,"cid":inbox.cid,"token":inbox.token,
        "owner_pane":owner,"pane":pane,"helper":json_text(&bridge,"helper")?,
        "cwd":utf8(cwd.as_os_str())?,"argv":argv,"env":environment,"wait":true,"umask":umask});
    let file = inbox.atomic_json(&name, &request, false)?;
    let pending = NativeRequest {
        inbox: &inbox,
        name,
        file,
    };
    install_signals();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut started = false;
    loop {
        let signal = SIGNAL.load(Ordering::Relaxed);
        if signal != 0 {
            return Ok(128 + signal);
        }
        if inbox.live_pane(owner)? != pane {
            return Err(invalid("native shell window changed"));
        }
        if !started {
            if let Ok((bytes, _)) = inbox.read(&format!("native-shell-{id}.started"), 1024, true) {
                started = bytes == id.as_bytes();
            }
            if !started && Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native terminal did not start within 15s",
                ));
            }
        }
        if started {
            match inbox.read(&format!("native-shell-{id}.result"), 1024, true) {
                Ok((bytes, _)) => {
                    let result: serde_json::Value =
                        serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                    if json_text(&result, "id")? != id {
                        return Err(invalid("native shell result identity mismatch"));
                    }
                    let status = json_number(&result, "status")?;
                    if status > 255 {
                        return Err(invalid("invalid native shell exit status"));
                    }
                    drop(pending);
                    return Ok(status as i32);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn request_present(inbox: &Inbox, name: &str, expected: &[u8]) -> bool {
    inbox
        .read(name, NATIVE_CAP, true)
        .is_ok_and(|(bytes, _)| bytes == expected)
}

struct NativeResult<'a> {
    inbox: &'a Inbox,
    name: &'a str,
    bytes: &'a [u8],
    id: &'a str,
    owner: u64,
    pane: u64,
    reported: bool,
    file: &'a File,
}

impl NativeResult<'_> {
    fn write(&mut self, status: i32) -> io::Result<()> {
        if unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(last_error());
        }
        let published = (|| {
            if !request_present(self.inbox, self.name, self.bytes)
                || self.inbox.live_pane(self.owner)? != self.pane
            {
                return Err(invalid("native shell request was revoked"));
            }
            self.inbox.atomic_json(
                &format!("native-shell-{}.result", self.id),
                &serde_json::json!({"id":self.id,"status":status}),
                false,
            )?;
            Ok(())
        })();
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
        published?;
        self.reported = true;
        Ok(())
    }
}

impl Drop for NativeResult<'_> {
    fn drop(&mut self) {
        if !self.reported {
            let _ = self.write(1);
        }
    }
}

struct NativeWatchdog {
    pipe: OwnedFd,
    pid: libc::pid_t,
    armed: bool,
}

impl NativeWatchdog {
    fn new() -> io::Result<Self> {
        let mut pipe = [-1; 2];
        if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(last_error());
        }
        let reader = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
        let writer = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(last_error());
        }
        if pid == 0 {
            let fd = reader.as_raw_fd();
            unsafe {
                libc::setsid();
                for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGPIPE] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                // Own only the read pipe, never a terminal or a request lock.
                if fd > 0 {
                    libc::syscall(libc::SYS_close_range, 0u32, (fd - 1) as u32, 0u32);
                }
                libc::syscall(libc::SYS_close_range, (fd + 1) as u32, u32::MAX, 0u32);
                let mut target: libc::pid_t = 0;
                let count = libc::read(
                    fd,
                    (&mut target as *mut libc::pid_t).cast(),
                    std::mem::size_of::<libc::pid_t>(),
                );
                if count != std::mem::size_of::<libc::pid_t>() as isize || target <= 0 {
                    libc::_exit(0);
                }
                let mut stop = 0u8;
                loop {
                    let count = libc::read(fd, (&mut stop as *mut u8).cast(), 1);
                    if count > 0 {
                        libc::_exit(0);
                    }
                    if count < 0 && last_error().kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                // EOF covers SIGKILL of the wrapper, which cannot run its cleanup.
                libc::kill(-target, libc::SIGTERM);
                libc::usleep(500_000);
                libc::kill(-target, libc::SIGKILL);
                libc::_exit(0);
            }
        }
        drop(reader);
        Ok(Self {
            pipe: writer,
            pid,
            armed: false,
        })
    }
}

impl Drop for NativeWatchdog {
    fn drop(&mut self) {
        unsafe {
            if self.armed {
                let stop = 1u8;
                libc::write(self.pipe.as_raw_fd(), (&stop as *const u8).cast(), 1);
            } else {
                let stop: libc::pid_t = -1;
                libc::write(
                    self.pipe.as_raw_fd(),
                    (&stop as *const libc::pid_t).cast(),
                    std::mem::size_of::<libc::pid_t>(),
                );
            }
            let mut status = 0;
            while libc::waitpid(self.pid, &mut status, 0) < 0
                && last_error().kind() == io::ErrorKind::Interrupted
            {}
        }
    }
}

struct NativeChild {
    child: std::process::Child,
    foreground: libc::pid_t,
    _watchdog: NativeWatchdog,
}

impl Drop for NativeChild {
    fn drop(&mut self) {
        let pid = self.child.id() as libc::pid_t;
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            let _ = self.child.try_wait();
            if unsafe { libc::kill(-pid, 0) } != 0
                && last_error().raw_os_error() == Some(libc::ESRCH)
            {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = self.child.wait();
        unsafe {
            libc::tcsetpgrp(libc::STDIN_FILENO, self.foreground);
        }
    }
}

fn native_terminal(path: &Path) -> io::Result<i32> {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1
        || unsafe { libc::isatty(libc::STDOUT_FILENO) } != 1
    {
        return Err(invalid(
            "native terminal requires visible terminal stdin and stdout",
        ));
    }
    let directory = path
        .parent()
        .ok_or_else(|| invalid("missing native request inbox"))?;
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| invalid("invalid native request path"))?;
    if !path.is_absolute() || !name.starts_with("native-shell-") || !name.ends_with(".json") {
        return Err(invalid("invalid native request path"));
    }
    // Pin the parent before opening the request with no-follow semantics.
    let dir = private_dir(directory)?;
    let cname = cstr(OsStr::new(name))?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            cname.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(last_error());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.len() > NATIVE_CAP as u64
    {
        return Err(invalid("unsafe native shell request"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(NATIVE_CAP as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > NATIVE_CAP {
        return Err(invalid("native shell request exceeds 256 KiB"));
    }
    let request: serde_json::Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let inbox = Inbox::connect(
        directory,
        json_text(&request, "cid")?,
        json_text(&request, "token")?,
    )?;
    let id = json_text(&request, "id")?;
    let prefix = format!("{}-{}-", inbox.cid, inbox.token);
    if json_number(&request, "version")? != 1
        || name != format!("native-shell-{id}.json")
        || !id
            .strip_prefix(&prefix)
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|v| v.is_ascii_digit()))
        || fs::canonicalize(json_text(&request, "helper")?)?
            != fs::canonicalize(env::current_exe()?)?
    {
        return Err(invalid("native shell request identity mismatch"));
    }
    let owner = json_number(&request, "owner_pane")?;
    let pane = json_number(&request, "pane")?;
    let umask = json_number(&request, "umask")?;
    if owner == 0 || pane == 0 || umask > 0o777 {
        return Err(invalid("invalid native shell execution context"));
    }
    let cwd = Path::new(json_text(&request, "cwd")?);
    if !cwd.is_absolute() {
        return Err(invalid("native shell cwd must be absolute"));
    }
    let argv = request
        .get("argv")
        .and_then(|v| v.as_array())
        .ok_or_else(|| invalid("invalid native shell argv"))?
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| invalid("invalid native shell argument"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let environment = request
        .get("env")
        .and_then(|v| v.as_object())
        .ok_or_else(|| invalid("invalid native shell environment"))?;
    let wait = request
        .get("wait")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| invalid("invalid native shell wait policy"))?;
    let mut command = Command::new("/bin/sh");
    command.arg0("sh").args(argv).current_dir(cwd).env_clear();
    for (key, value) in environment {
        if key.is_empty() || key.contains('=') || key.as_bytes().contains(&0) {
            return Err(invalid("invalid native shell environment key"));
        }
        let value = value
            .as_str()
            .ok_or_else(|| invalid("invalid native shell environment value"))?;
        if value.as_bytes().contains(&0) {
            return Err(invalid("invalid native shell environment value"));
        }
        if key != BRIDGE_ENV {
            command.env(key, value);
        }
    }
    for key in ["TERM", "TERM_PROGRAM", "COLORTERM"] {
        match env::var_os(key) {
            Some(value) => {
                command.env(key, value);
            }
            None => {
                command.env_remove(key);
            }
        }
    }
    install_signals();
    let mut watchdog = NativeWatchdog::new()?;
    let watchdog_fd = watchdog.pipe.as_raw_fd();
    unsafe {
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(last_error());
            }
            let pid = libc::getpid();
            if libc::write(
                watchdog_fd,
                (&pid as *const libc::pid_t).cast(),
                std::mem::size_of::<libc::pid_t>(),
            ) != std::mem::size_of::<libc::pid_t>() as isize
            {
                return Err(last_error());
            }
            if libc::tcsetpgrp(libc::STDIN_FILENO, pid) != 0 {
                return Err(last_error());
            }
            libc::umask(umask as libc::mode_t);
            for signal in [
                libc::SIGINT,
                libc::SIGTERM,
                libc::SIGHUP,
                libc::SIGPIPE,
                libc::SIGTTOU,
                libc::SIGTTIN,
                libc::SIGTSTP,
            ] {
                libc::signal(signal, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(last_error());
    }
    if inbox.live_pane(owner)? != pane || !request_present(&inbox, name, &bytes) {
        return Err(invalid("native shell request is no longer live"));
    }
    let mut started = inbox.create(&format!("native-shell-{id}.started"))?;
    started.write_all(id.as_bytes())?;
    started.flush()?;
    let mut result = NativeResult {
        inbox: &inbox,
        name,
        bytes: &bytes,
        id,
        owner,
        pane,
        reported: false,
        file: &file,
    };
    let foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
    let child = command.spawn()?;
    watchdog.armed = true;
    let mut child = NativeChild {
        child,
        foreground,
        _watchdog: watchdog,
    };
    unsafe {
        libc::tcsetpgrp(libc::STDIN_FILENO, child.child.id() as libc::pid_t);
        libc::kill(-(child.child.id() as libc::pid_t), libc::SIGCONT);
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
    let status = loop {
        if let Some(status) = child.child.try_wait()? {
            use std::os::unix::process::ExitStatusExt;
            break status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1));
        }
        let signal = SIGNAL.load(Ordering::Relaxed);
        if signal != 0
            || inbox.live_pane(owner).ok() != Some(pane)
            || !request_present(&inbox, name, &bytes)
        {
            drop(child);
            let _ = result.write(if signal == 0 { 1 } else { 128 + signal });
            return Ok(0);
        }
        thread::sleep(Duration::from_millis(20));
    };
    drop(child);
    result.write(status)?;
    if wait && SIGNAL.load(Ordering::Relaxed) == 0 {
        eprintln!("\n[exit {status}] Press Enter to close.");
        loop {
            if SIGNAL.load(Ordering::Relaxed) != 0 || inbox.live_pane(owner).ok() != Some(pane) {
                break;
            }
            let mut poll = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, 20) };
            if ready < 0 && last_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
            if ready > 0 {
                if poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                    break;
                }
                if poll.revents & libc::POLLIN != 0 {
                    let mut byte = 0u8;
                    let count =
                        unsafe { libc::read(libc::STDIN_FILENO, (&mut byte as *mut u8).cast(), 1) };
                    if count <= 0 || byte == b'\n' || byte == b'\r' {
                        break;
                    }
                }
            }
        }
    }
    Ok(0)
}

fn run() -> io::Result<i32> {
    let first = env::args_os().next();
    if first.as_deref().and_then(|v| Path::new(v).file_name()) == Some(OsStr::new("sh")) {
        return native_shell(&env::args_os().skip(1).collect::<Vec<_>>());
    }
    match env::args_os().nth(1).as_deref() {
        Some(mode) if mode == "--native-shell" => {
            return native_shell(&env::args_os().skip(2).collect::<Vec<_>>());
        }
        Some(mode) if mode == "--native-terminal" => {
            let path = env::args_os()
                .nth(2)
                .ok_or_else(|| invalid("missing native shell request path"))?;
            if env::args_os().nth(3).is_some() {
                return Err(invalid("unexpected native terminal arguments"));
            }
            return native_terminal(Path::new(&path));
        }
        _ => {}
    }
    let LaunchArgs {
        real,
        args,
        standalone_pane,
    } = launch_args(env::args_os().skip(1))?;
    let native = env::var_os("TERN_PANE").is_some_and(|v| !v.is_empty());
    if standalone_pane.is_none() && !native {
        return passthrough(&real, &args);
    }
    for arg in &args {
        if arg == "--" {
            break;
        }
        if standalone_pane.is_none()
            && (arg == "--help" || arg == "--version" || arg == "-h" || arg == "-V")
        {
            return passthrough(&real, &args);
        }
        if arg == "--client-id" || arg.as_bytes().starts_with(b"--client-id=") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--client-id is managed by tern-yazi; use original Yazi for an explicit ID",
            ));
        }
    }
    let real_path = fs::canonicalize(&real)?;
    if !real_path.is_file() || real_path == fs::canonicalize(env::current_exe()?)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "original Yazi must be a different executable file",
        ));
    }
    let owner = if let Some(pane) = standalone_pane {
        Owner::Standalone(pane)
    } else {
        Owner::Shell(
            env::var("TERN_PANE")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .ok_or_else(|| invalid("invalid TERN_PANE"))?,
        )
    };
    let pane = owner.pane();
    let cwd = env::current_dir()?;
    let caller_umask = unsafe { libc::umask(0o077) };
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        // Reap orphaned descendants after the Yazi process group is terminated.
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }
    let inbox = Inbox::new()?;
    let helper = fs::canonicalize(env::current_exe()?)?;
    let shim = inbox.shim(&helper)?;
    let original_path = env::var_os("PATH");
    let original_path_text = original_path.as_deref().map(utf8).transpose()?;
    let bridge = serde_json::to_string(&serde_json::json!({"cid":inbox.cid,"token":inbox.token,
        "owner_pane":pane,"inbox":utf8(inbox.path()?.as_os_str())?,
        "helper":utf8(helper.as_os_str())?,"original_path":original_path_text}))
    .map_err(io::Error::other)?;
    let mut path = shim.into_os_string();
    path.push(":");
    path.push(
        original_path
            .as_deref()
            .unwrap_or_else(|| OsStr::new("/usr/local/bin:/usr/bin:/bin")),
    );
    let binding = standalone_pane
        .map(|pane| StandaloneBinding::new(&inbox, pane, &cwd))
        .transpose()?;
    let mut backend = Backend::spawn(
        real_path.as_os_str(),
        &args,
        &inbox.cid,
        caller_umask,
        &bridge,
        &path,
    )?;
    let started = Instant::now();
    let mut published: Option<Instant> = None;
    let mut heartbeat = Instant::now();
    let mut seq = 0u64;
    let mut last_lease: Option<(SystemTime, Vec<u8>)> = None;
    let mut lease_seen: Option<Instant> = None;
    let mut last_size = (0u16, 0u16);
    let result: io::Result<i32> = loop {
        let now = Instant::now();
        let signal = SIGNAL.load(Ordering::Relaxed);
        if signal != 0 {
            backend.signal(signal);
            break Ok(128 + signal);
        }
        if standalone_pane.is_some() && published.is_none() && inbox.stop_requested() {
            break Ok(0);
        }
        if let Err(error) = backend.drain() {
            break Err(error);
        }
        match backend.poll_child() {
            Ok(Some(status)) => {
                if published.is_none() {
                    eprintln!("tern-yazi: Yazi exited before publishing a snapshot (status {}); enable require(\"tern\"):setup() in Yazi init.lua", child_code(status));
                }
                break Ok(if published.is_none() && child_code(status) == 0 {
                    1
                } else {
                    child_code(status)
                });
            }
            Err(error) => break Err(error),
            _ => {}
        }
        if published.is_none() {
            match inbox.ready() {
                Ok(true) => {
                    seq += 1;
                    if let Err(error) = inbox.heartbeat(owner, &cwd, seq) {
                        break Err(error);
                    }
                    published = Some(now);
                    backend.capture_startup = false;
                    heartbeat = now;
                }
                Ok(false) => {}
                Err(error) => break Err(error),
            }
            if published.is_none() && now.duration_since(started) >= Duration::from_secs(15) {
                break Err(io::Error::new(io::ErrorKind::TimedOut, "Yazi did not publish an initial snapshot within 15s; install the tern.yazi plugin and enable require(\"tern\"):setup() in init.lua"));
            }
        }
        if let Some(published_at) = published {
            if now.duration_since(heartbeat) >= Duration::from_millis(500) {
                seq += 1;
                if let Err(error) = inbox.heartbeat(owner, &cwd, seq) {
                    break Err(error);
                }
                heartbeat = now;
            }
            if inbox.stop_requested() {
                break Ok(0);
            }
            if let Ok((bytes, modified)) =
                inbox.read(&format!("managed-{}.lease", inbox.cid), 1024, true)
            {
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    let mut fields = text.split_whitespace();
                    if fields.next() == Some(inbox.token.as_str())
                        && fields
                            .next()
                            .and_then(|v| v.parse::<u64>().ok())
                            .is_some_and(|v| {
                                v > 0 && standalone_pane.map_or(true, |pane| v == pane)
                            })
                    {
                        let size = (
                            fields
                                .next()
                                .and_then(|v| v.parse::<u16>().ok())
                                .unwrap_or(0),
                            fields
                                .next()
                                .and_then(|v| v.parse::<u16>().ok())
                                .unwrap_or(0),
                        );
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
        }
        let mut poll = libc::pollfd {
            fd: backend.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe {
            libc::poll(&mut poll, 1, 25);
        }
    };
    if published.is_none()
        && SIGNAL.load(Ordering::Relaxed) == 0
        && result.as_ref().map_or(true, |code| *code != 0)
    {
        backend.diagnostic.print();
    }
    backend.stop();
    // Subreaper collects grandchildren after the bounded process-group shutdown.
    let reap_deadline = Instant::now() + Duration::from_millis(500);
    loop {
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid < 0 || Instant::now() >= reap_deadline {
            break;
        }
        if pid == 0 {
            thread::sleep(Duration::from_millis(10));
        }
    }
    drop(backend);
    drop(binding);
    drop(inbox);
    result
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tern-yazi: {error}");
            1
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::sync::atomic::AtomicU32;

    struct Fixture {
        inbox: Inbox,
        path: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
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

    fn parse(args: &[&str]) -> io::Result<LaunchArgs> {
        launch_args(args.iter().map(|arg| OsString::from(*arg)))
    }

    #[test]
    fn standalone_argv_keeps_native_identity_separate_from_yazi_arguments() {
        let launch = parse(&[
            "--standalone-pane",
            "42",
            "--real",
            "/usr/bin/yazi",
            "--",
            "--cwd-file",
            "/tmp/path with spaces",
            "/tmp/files",
        ])
        .unwrap();
        assert_eq!(launch.standalone_pane, Some(42));
        assert_eq!(launch.real, OsStr::new("/usr/bin/yazi"));
        assert_eq!(
            launch.args,
            ["--cwd-file", "/tmp/path with spaces", "/tmp/files"].map(OsString::from)
        );
    }

    #[test]
    fn shell_argv_preserves_existing_interface_and_non_utf8_arguments() {
        let path = OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]);
        let launch = launch_args(
            [
                OsString::from("--real"),
                OsString::from("/usr/bin/yazi"),
                OsString::from("--"),
                path.clone(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(launch.standalone_pane, None);
        assert_eq!(launch.args, vec![path]);
    }

    #[test]
    fn standalone_argv_rejects_missing_invalid_identity_or_delimiters() {
        for pane in ["0", "-1", "native", "18446744073709551616"] {
            assert!(parse(&["--standalone-pane", pane, "--real", "/usr/bin/yazi", "--"]).is_err());
        }
        for args in [
            vec!["--standalone-pane"],
            vec!["--standalone-pane", "42"],
            vec!["--standalone-pane", "42", "/usr/bin/yazi", "--"],
            vec!["--standalone-pane", "42", "--real"],
            vec!["--standalone-pane", "42", "--real", "/usr/bin/yazi"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn health_distinguishes_native_owned_and_shell_owned_supervisors() {
        let fixture = Fixture::new();
        let shell = fixture
            .inbox
            .identity(Owner::Shell(17), Path::new("/tmp/files"));
        assert_eq!(shell["owner_pane"], 17);
        assert_eq!(shell["owner_kind"], "shell");
        assert!(shell.get("native_pane").is_none());
        fixture
            .inbox
            .heartbeat(Owner::Standalone(42), Path::new("/tmp/files"), 7)
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
            .identity(Owner::Standalone(42), Path::new("/tmp/files"));
        replacement["token"] = "b".repeat(32).into();
        fixture
            .inbox
            .atomic_json("native-42.json", &replacement, true)
            .unwrap();
        drop(binding);
        assert!(fixture.inbox.exists("native-42.json"));
    }

    #[test]
    fn initial_snapshot_waits_for_matching_listing_epoch_and_revision() {
        let fixture = Fixture::new();
        let state = serde_json::json!({
            "client_id": fixture.inbox.cid, "seq": 1,
            "listing_epoch": "current", "listing_revision": 2
        });
        fixture.inbox.atomic_json("state-12345.json", &state, false).unwrap();
        assert!(!fixture.inbox.ready().unwrap());
        let mut listing = serde_json::json!({
            "epoch": "current", "revision": 1, "cwd": "/tmp/files", "files": ["alpha.txt"]
        });
        fixture.inbox.atomic_json("listing-12345.json", &listing, false).unwrap();
        assert!(!fixture.inbox.ready().unwrap());
        listing["epoch"] = "previous".into();
        listing["revision"] = 2.into();
        fixture.inbox.atomic_json("listing-12345.json", &listing, true).unwrap();
        assert!(!fixture.inbox.ready().unwrap());
        listing["epoch"] = "current".into();
        fixture.inbox.atomic_json("listing-12345.json", &listing, true).unwrap();
        assert!(fixture.inbox.ready().unwrap());
    }

    #[test]
    fn native_binding_replaces_crashed_owner_record_and_partial_write() {
        let fixture = Fixture::new();
        let mut previous = fixture
            .inbox
            .identity(Owner::Standalone(42), Path::new("/tmp/previous"));
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
