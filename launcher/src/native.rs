//! Helper modes Yazi reaches through the PATH shim: the native shell handoff, the terminal side of
//! that handoff, and the no-replace rename.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::backend::BRIDGE_ENV;
use crate::inbox::{Inbox, NATIVE_CAP};
use crate::sys::{cstr, install_signals, private_dir, invalid, json_number, json_text, last_error, pending_signal, utf8};

pub(crate) struct NativeRequest<'a> {
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

pub(crate) fn native_shell(args: &[OsString]) -> io::Result<i32> {
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
        let signal = pending_signal();
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

pub(crate) fn request_present(inbox: &Inbox, name: &str, expected: &[u8]) -> bool {
    inbox
        .read(name, NATIVE_CAP, true)
        .is_ok_and(|(bytes, _)| bytes == expected)
}

pub(crate) struct NativeResult<'a> {
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
    pub(crate) fn write(&mut self, status: i32) -> io::Result<()> {
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

pub(crate) struct NativeWatchdog {
    pipe: OwnedFd,
    pid: libc::pid_t,
    armed: bool,
}

impl NativeWatchdog {
    pub(crate) fn new() -> io::Result<Self> {
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

pub(crate) struct NativeChild {
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

pub(crate) fn native_terminal(path: &Path) -> io::Result<i32> {
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
        let signal = pending_signal();
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
    if wait && pending_signal() == 0 {
        eprintln!("\n[exit {status}] Press Enter to close.");
        loop {
            if pending_signal() != 0 || inbox.live_pane(owner).ok() != Some(pane) {
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

// The Lua filesystem API only exposes overwriting rename. Keep collision
// refusal atomic at the existing Linux process boundary instead of stat+rename.
pub(crate) fn rename_no_replace(args: &[OsString]) -> io::Result<i32> {
    if args.len() != 2 {
        return Err(invalid("rename requires source and destination"));
    }
    let source = cstr(&args[0])?;
    let target = cstr(&args[1])?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(last_error());
    }
    Ok(0)
}
