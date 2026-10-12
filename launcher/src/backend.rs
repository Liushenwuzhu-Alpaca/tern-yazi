//! The invisible Yazi process: a PTY-backed process group, startup diagnostics, terminal query
//! answers and bounded termination.

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::sys::last_error;

pub(crate) const BRIDGE_ENV: &str = "TERN_YAZI_BRIDGE";

// A fixed-size startup-only diagnostic tail. No ANSI/control bytes reach stderr.
pub(crate) struct StartupDiagnostic {
    bytes: [u8; 4096],
    next: usize,
    len: usize,
    escape: u8,
}

impl StartupDiagnostic {
    pub(crate) fn new() -> Self {
        Self {
            bytes: [0; 4096],
            next: 0,
            len: 0,
            escape: 0,
        }
    }

    pub(crate) fn push(&mut self, byte: u8) {
        self.bytes[self.next] = byte;
        self.next = (self.next + 1) % self.bytes.len();
        self.len = (self.len + 1).min(self.bytes.len());
    }

    pub(crate) fn feed(&mut self, byte: u8) {
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

    pub(crate) fn print(&self) {
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

pub(crate) struct Backend {
    pub(crate) pid: libc::pid_t,
    pub(crate) master: OwnedFd,
    pub(crate) status: Option<i32>,
    pub(crate) queries: Vec<u8>,
    pub(crate) stopped: bool,
    pub(crate) diagnostic: StartupDiagnostic,
    pub(crate) capture_startup: bool,
}

impl Backend {
    pub(crate) fn spawn(
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
            let mut command = Command::new(real);
            command
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
                .env_remove("TERN_IDENTITY");
            if let Ok(cwd) = env::current_dir() {
                command.env("PWD", cwd);
            }
            let error = command.exec();
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

    /// Sleeps until Yazi writes to its PTY or `ms` elapse.
    pub(crate) fn wait_readable(&self, ms: i32) {
        let mut poll = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe {
            libc::poll(&mut poll, 1, ms);
        }
    }

    pub(crate) fn poll_child(&mut self) -> io::Result<Option<i32>> {
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

    pub(crate) fn signal(&self, signal: i32) {
        unsafe {
            libc::kill(-self.pid, signal);
        }
    }

    pub(crate) fn resize(&self, rows: u16, cols: u16) {
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

    pub(crate) fn drain(&mut self) -> io::Result<()> {
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

    pub(crate) fn stop(&mut self) {
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

pub(crate) fn child_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}
