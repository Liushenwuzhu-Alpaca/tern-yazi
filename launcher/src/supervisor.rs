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
}

pub(crate) fn launch_args(mut input: impl Iterator<Item = OsString>) -> io::Result<LaunchArgs> {
    if input.next().as_deref() != Some(OsStr::new("--real")) {
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
    })
}

/// A validated launch of the original Yazi under supervision.
pub(crate) struct Launch {
    pub(crate) real: PathBuf,
    pub(crate) args: Vec<OsString>,
    /// Yazi was given `--chooser-file`: opening a file chooses it and quits Yazi.
    pub(crate) chooser: bool,
}

/// What to do with a launch: supervise it, or hand the arguments to the original Yazi.
pub(crate) enum Plan {
    Supervise(Launch),
    Passthrough,
}

/// Whether the Yazi arguments name a chooser file, as `--chooser-file X` or `--chooser-file=X`.
fn has_chooser_file(args: &[OsString]) -> bool {
    args.iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--chooser-file" || arg.as_bytes().starts_with(b"--chooser-file="))
}

pub(crate) fn plan(real: &OsStr, args: &[OsString]) -> io::Result<Plan> {
    for arg in args {
        if arg == "--" {
            break;
        }
        if arg == "--help" || arg == "--version" || arg == "-h" || arg == "-V" {
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
        chooser: has_chooser_file(args),
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

/// Starts Yazi with the bridge record that names this binary as the plugin's rename helper.
pub(crate) fn spawn(
    inbox: &Inbox,
    launch: &Launch,
    caller_umask: libc::mode_t,
) -> io::Result<Backend> {
    let helper = fs::canonicalize(env::current_exe()?)?;
    let bridge = serde_json::json!({"helper": utf8(helper.as_os_str())?}).to_string();
    Backend::spawn(
        launch.real.as_os_str(),
        &launch.args,
        &inbox.cid,
        caller_umask,
        &bridge,
    )
}

pub(crate) enum Step {
    Starting,
    Ready,
    Exited(i32),
}

/// Tracks the backend until Yazi publishes its first complete snapshot.
pub(crate) struct Startup {
    started: Instant,
    published: bool,
    early_exit: Option<i32>,
}

impl Startup {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            published: false,
            early_exit: None,
        }
    }

    pub(crate) fn poll(&mut self, inbox: &Inbox, backend: &mut Backend) -> io::Result<Step> {
        backend.drain()?;
        if let Some(status) = backend.poll_child()? {
            let code = child_code(status);
            if !self.published {
                self.early_exit = Some(code);
                return Ok(Step::Exited(if code == 0 { 1 } else { code }));
            }
            return Ok(Step::Exited(code));
        }
        if self.published {
            return Ok(Step::Ready);
        }
        if inbox.ready()? {
            self.published = true;
            backend.capture_startup = false;
            return Ok(Step::Ready);
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
        if !self.published
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
        assert_eq!(launch.args, vec![path]);
    }

    #[test]
    fn argv_rejects_missing_delimiters() {
        for args in [
            vec![],
            vec!["/usr/bin/yazi", "--"],
            vec!["--real"],
            vec!["--real", "/usr/bin/yazi"],
            vec!["--real", "/usr/bin/yazi", "/tmp"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn chooser_file_is_detected_in_both_forms_before_the_delimiter() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(has_chooser_file(&args(&["--chooser-file", "/tmp/c"])));
        assert!(has_chooser_file(&args(&["/tmp", "--chooser-file=/tmp/c"])));
        assert!(!has_chooser_file(&args(&["--cwd-file", "/tmp/c"])));
        assert!(!has_chooser_file(&args(&[
            "--",
            "--chooser-file",
            "/tmp/c"
        ])));
        assert!(!has_chooser_file(&[]));
    }
}
