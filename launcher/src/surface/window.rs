//! Window actions. Opening paths and terminals goes through the `tern` CLI, which acts beside
//! this pane (`TERN_PANE`); the calls run off the main loop and only failures come back. The
//! clipboard is OSC 52 on the pane's tty, which Tern puts on the system clipboard.

use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// One double click can reach us as two list activations; the same paths again within this
/// window are that click, not a second request.
const REPEAT: Duration = Duration::from_millis(500);

pub(crate) struct Window {
    pane: String,
    failures: Receiver<String>,
    report: Sender<String>,
    opened: Option<(Vec<String>, Instant)>,
}

impl Window {
    pub(crate) fn new(pane: String) -> Self {
        let (report, failures) = mpsc::channel();
        Self {
            pane,
            failures,
            report,
            opened: None,
        }
    }

    /// Opens each path in Tern as a click on it would, beside this pane.
    pub(crate) fn open(&mut self, paths: Vec<String>) {
        if self
            .opened
            .as_ref()
            .is_some_and(|(last, at)| *last == paths && at.elapsed() < REPEAT)
        {
            return;
        }
        self.opened = Some((paths.clone(), Instant::now()));
        let mut args = vec!["open".to_owned(), "--".to_owned()];
        args.extend(paths);
        self.run("Cannot open in Tern", args);
    }

    /// Splits a shell below this pane in `cwd`.
    pub(crate) fn terminal(&self, cwd: String) {
        let args = ["split", &self.pane, "down", "--cwd", &cwd].map(str::to_owned);
        self.run("Cannot open a terminal", args.to_vec());
    }

    /// The next failed action, as notice text.
    pub(crate) fn failure(&self) -> Option<String> {
        self.failures.try_recv().ok()
    }

    fn run(&self, failure: &'static str, args: Vec<String>) {
        let report = self.report.clone();
        thread::spawn(move || {
            let output = Command::new("tern")
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output();
            let detail = match output {
                Ok(output) if output.status.success() => return,
                Ok(output) => match String::from_utf8_lossy(&output.stderr).trim() {
                    "" => output.status.to_string(),
                    stderr => stderr.to_owned(),
                },
                Err(error) => error.to_string(),
            };
            let _ = report.send(format!("{failure}: {detail}"));
        });
    }
}

/// Puts `text` on the clipboard through OSC 52.
pub(crate) fn copy(text: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = chunk.iter().enumerate().fold(0u32, |acc, (at, byte)| {
            acc | u32::from(*byte) << (16 - 8 * at)
        });
        for at in 0..4 {
            encoded.push(if at <= chunk.len() {
                char::from(ALPHABET[(triple >> (18 - 6 * at) & 0x3f) as usize])
            } else {
                '='
            });
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_pads_like_rfc_4648() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded);
        }
        assert_eq!(base64("/tmp/é".as_bytes()), "L3RtcC/DqQ==");
    }
}
