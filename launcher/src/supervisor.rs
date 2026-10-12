//! What every supervised launch shares: argument parsing, process setup, the bridge environment,
//! the initial-snapshot handshake and bounded shutdown.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::backend::{child_code, Backend};
use crate::inbox::Inbox;
use crate::sys::{install_signals, invalid, pending_signal, utf8};

const STARTUP_LIMIT: Duration = Duration::from_secs(15);

pub(crate) struct LaunchArgs {
    pub(crate) real: OsString,
    pub(crate) args: Vec<OsString>,
    pub(crate) standalone_pane: Option<u64>,
}

pub(crate) fn launch_args(mut input: impl Iterator<Item = OsString>) -> io::Result<LaunchArgs> {
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

/// A validated launch of the original Yazi under supervision.
pub(crate) struct Launch {
    pub(crate) real: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) cwd: PathBuf,
}

/// What to do with a launch: supervise it, or hand the arguments to the original Yazi.
pub(crate) enum Plan {
    Supervise(Launch),
    Passthrough,
}

pub(crate) fn plan(real: &OsStr, args: &[OsString], standalone: bool) -> io::Result<Plan> {
    for arg in args {
        if arg == "--" {
            break;
        }
        if !standalone && (arg == "--help" || arg == "--version" || arg == "-h" || arg == "-V") {
            return Ok(Plan::Passthrough);
        }
        if arg == "--client-id" || arg.as_bytes().starts_with(b"--client-id=") {
            return Err(invalid(
                "--client-id is managed by tern-yazi; use original Yazi for an explicit ID",
            ));
        }
    }
    let real = fs::canonicalize(real)?;
    if !real.is_file() || real == fs::canonicalize(env::current_exe()?)? {
        return Err(invalid("original Yazi must be a different executable file"));
    }
    Ok(Plan::Supervise(Launch {
        real,
        args: args.to_vec(),
        cwd: env::current_dir()?,
    }))
}

pub(crate) fn passthrough(real: &OsStr, args: &[OsString]) -> io::Result<i32> {
    Err(Command::new(real).args(args).exec())
}

/// Process-wide setup before the backend exists; returns the caller's umask for Yazi.
pub(crate) fn prepare() -> libc::mode_t {
    let caller_umask = unsafe { libc::umask(0o077) };
    install_signals();
    // Reap orphaned descendants after the Yazi process group is terminated.
    unsafe {
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }
    caller_umask
}

/// Starts Yazi with the PATH shim and the bridge record its helpers authenticate with.
pub(crate) fn spawn(
    inbox: &Inbox,
    launch: &Launch,
    owner_pane: u64,
    caller_umask: libc::mode_t,
) -> io::Result<Backend> {
    let helper = fs::canonicalize(env::current_exe()?)?;
    let shim = inbox.shim(&helper)?;
    let original_path = env::var_os("PATH");
    let original_path_text = original_path.as_deref().map(utf8).transpose()?;
    let bridge = serde_json::to_string(&serde_json::json!({"cid":inbox.cid,"token":inbox.token,
        "owner_pane":owner_pane,"inbox":utf8(inbox.path()?.as_os_str())?,
        "helper":utf8(helper.as_os_str())?,"original_path":original_path_text}))
    .map_err(io::Error::other)?;
    let mut path = shim.into_os_string();
    path.push(":");
    path.push(
        original_path
            .as_deref()
            .unwrap_or_else(|| OsStr::new("/usr/local/bin:/usr/bin:/bin")),
    );
    Backend::spawn(
        launch.real.as_os_str(),
        &launch.args,
        &inbox.cid,
        caller_umask,
        &bridge,
        &path,
    )
}

pub(crate) enum Step {
    Starting,
    Ready(Instant),
    Exited(i32),
}

/// Tracks the backend until Yazi publishes its first complete snapshot.
pub(crate) struct Startup {
    started: Instant,
    published: Option<Instant>,
    early_exit: Option<i32>,
}

impl Startup {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            published: None,
            early_exit: None,
        }
    }

    pub(crate) fn poll(&mut self, inbox: &Inbox, backend: &mut Backend) -> io::Result<Step> {
        backend.drain()?;
        if let Some(status) = backend.poll_child()? {
            let code = child_code(status);
            if self.published.is_none() {
                self.early_exit = Some(code);
                return Ok(Step::Exited(if code == 0 { 1 } else { code }));
            }
            return Ok(Step::Exited(code));
        }
        if let Some(at) = self.published {
            return Ok(Step::Ready(at));
        }
        if inbox.ready()? {
            let now = Instant::now();
            self.published = Some(now);
            backend.capture_startup = false;
            return Ok(Step::Ready(now));
        }
        if self.started.elapsed() >= STARTUP_LIMIT {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "Yazi did not publish an initial snapshot within 15s; install the tern.yazi plugin and enable require(\"tern\"):setup() in init.lua"));
        }
        Ok(Step::Starting)
    }

    /// Explains a failed start on stderr; callers own the terminal again by now.
    pub(crate) fn report(&self, backend: &Backend, result: &io::Result<i32>) {
        if let Some(code) = self.early_exit {
            eprintln!("tern-yazi: Yazi exited before publishing a snapshot (status {code}); enable require(\"tern\"):setup() in Yazi init.lua");
        }
        if self.published.is_none()
            && pending_signal() == 0
            && result.as_ref().map_or(true, |code| *code != 0)
        {
            backend.diagnostic.print();
        }
    }
}

/// Stops the Yazi process group, then collects orphans the subreaper inherited.
pub(crate) fn shutdown(mut backend: Backend) {
    backend.stop();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

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
}
