//! The pane's real size in cells. While a program surface is live, Tern freezes the pane
//! tty's winsize and `tern inspect` rows, and the surface `Resize` event carries only
//! columns. `tern ctl state`, which Tern's window answers for the pane it runs in, still
//! reports every pane's current columns and rows, so a helper thread polls it and Yazi's
//! PTY follows. Without that command the tty size at hand is the only source.

use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use serde_json::Value;

/// Between two polls; each one costs a short `tern` process.
const POLL: Duration = Duration::from_millis(500);
/// Between two polls while `tern ctl` fails, e.g. on a Tern without the command.
const BACKOFF: Duration = Duration::from_secs(5);

pub(crate) struct PaneSize {
    sizes: Receiver<(u16, u16)>,
    latest: Option<(u16, u16)>,
}

impl PaneSize {
    /// Starts polling for the pane `pane` (`TERN_PANE`); nothing is polled without one.
    pub(crate) fn new(pane: &str) -> Self {
        let (send, sizes) = mpsc::channel();
        let pane = pane.to_owned();
        if !pane.is_empty() {
            thread::spawn(move || {
                let mut last = None;
                loop {
                    let size = state().and_then(|state| parse(&state, &pane));
                    if let Some(size) = size.filter(|size| Some(*size) != last) {
                        last = Some(size);
                        if send.send(size).is_err() {
                            return;
                        }
                    }
                    thread::sleep(if size.is_some() { POLL } else { BACKOFF });
                }
            });
        }
        Self {
            sizes,
            latest: None,
        }
    }

    /// The newest `(rows, cols)` the window reported, else `fallback`.
    pub(crate) fn current(&mut self, fallback: Option<(u16, u16)>) -> Option<(u16, u16)> {
        while let Ok(size) = self.sizes.try_recv() {
            self.latest = Some(size);
        }
        self.latest.or(fallback)
    }
}

fn state() -> Option<String> {
    let output = Command::new("tern")
        .args(["ctl", "state"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `(rows, cols)` of pane `pane` in a `tern ctl state` reply.
fn parse(state: &str, pane: &str) -> Option<(u16, u16)> {
    let pane: u64 = pane.parse().ok()?;
    let state: Value = serde_json::from_str(state).ok()?;
    let entry = state
        .get("panes")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_u64) == Some(pane))?;
    let rows = u16::try_from(entry.get("rows")?.as_u64()?).ok()?;
    let cols = u16::try_from(entry.get("cols")?.as_u64()?).ok()?;
    (rows > 0 && cols > 0).then_some((rows, cols))
}

#[cfg(test)]
mod tests {
    use super::parse;

    const STATE: &str =
        r#"{"ok":true,"panes":[{"cols":104,"id":7,"rows":43},{"cols":28,"id":9,"rows":20}]}"#;

    #[test]
    fn reads_the_size_of_the_named_pane() {
        assert_eq!(parse(STATE, "9"), Some((20, 28)));
        assert_eq!(parse(STATE, "7"), Some((43, 104)));
    }

    #[test]
    fn ignores_unknown_or_malformed_input() {
        assert_eq!(parse(STATE, "8"), None);
        assert_eq!(parse(STATE, "pane"), None);
        assert_eq!(parse("not json", "7"), None);
        assert_eq!(
            parse(r#"{"panes":[{"id":7,"rows":0,"cols":5}]}"#, "7"),
            None
        );
    }
}
