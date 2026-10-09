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
        Ok(unsafe { File::from_raw_fd(fd) })
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

    fn heartbeat(&self, pane: u64, cwd: &Path, seq: u64) -> io::Result<()> {
        let name = format!("managed-{}.json", self.cid);
        let tmp = format!("{name}.tmp");
        let value = serde_json::json!({ "client_id": self.cid, "token": self.token, "owner_pane": pane, "cwd": cwd.to_string_lossy(), "seq": seq });
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
        // Yazi snapshots inherit the caller's umask inside the private inbox.
        match self.read(&format!("state-{}.json", self.cid), 256 * 1024, false) {
            Ok((bytes, _)) => {
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                let cid = value.get("client_id").and_then(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .or_else(|| v.as_u64().map(|id| id.to_string()))
                });
                Ok(cid.as_deref() == Some(self.cid.as_str())
                    && value.get("cwd").and_then(|v| v.as_str()).is_some())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
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

fn run() -> io::Result<i32> {
    let mut input = env::args_os().skip(1);
    if input.next().as_deref() != Some(OsStr::new("--real")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected --real ORIGINAL -- [Yazi arguments]",
        ));
    }
    let real = input.next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "missing original Yazi binary")
    })?;
    if input.next().as_deref() != Some(OsStr::new("--")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected -- before Yazi arguments",
        ));
    }
    let args: Vec<OsString> = input.collect();
    let native = env::var_os("TERN_PANE").is_some_and(|v| !v.is_empty());
    if !native {
        return passthrough(&real, &args);
    }
    for arg in &args {
        if arg == "--" {
            break;
        }
        if arg == "--help" || arg == "--version" || arg == "-h" || arg == "-V" {
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
    let pane = env::var("TERN_PANE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid TERN_PANE"))?;
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
    let mut backend = Backend::spawn(real_path.as_os_str(), &args, &inbox.cid, caller_umask)?;
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
                    if let Err(error) = inbox.heartbeat(pane, &cwd, seq) {
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
                if let Err(error) = inbox.heartbeat(pane, &cwd, seq) {
                    break Err(error);
                }
                heartbeat = now;
            }
            if let Ok((bytes, _)) = inbox.read(&format!("managed-{}.stop", inbox.cid), 1024, true) {
                if std::str::from_utf8(&bytes).is_ok_and(|text| text.trim() == inbox.token) {
                    break Ok(0);
                }
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
                            .is_some_and(|v| v > 0)
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
