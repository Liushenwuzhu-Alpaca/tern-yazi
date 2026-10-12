//! Yazi's lent terminal on the pane. Yazi leaves its alternate screen to run a subprocess on
//! its own terminal (`shell --block`, the fzf and zoxide jumps, task inspection); the surface
//! then steps aside and the pane's tty is relayed to Yazi's PTY until Yazi takes it back, so
//! the subprocess runs and stays in the pane as if Yazi ran there.

use std::io::{self, Write};
use std::time::Duration;

use tern_sdk::{Session, Surface};

use super::input::Msg;
use super::panesize::PaneSize;
use crate::backend::Backend;
use crate::sys::{last_error, pending_signal};

/// The session's input modes, which Yazi's subprocess does not expect.
const MODES_OFF: &[u8] = b"\x1b[<u\x1b[?2004l";
const MODES_ON: &[u8] = b"\x1b[?2004h\x1b[>1u";
/// TSP message introducers (APC and OSC).
const TSP: [&[u8]; 2] = [b"\x1b_tsp;", b"\x1b]877;tsp;"];
const POLL_MS: i32 = 20;

/// Drops Tern's TSP messages from the pane's input; everything else is Yazi's.
#[derive(Debug, Default)]
pub(crate) struct Filter {
    held: Vec<u8>,
    /// Inside a TSP message: `Some(true)` for OSC, which BEL may end too.
    inside: Option<bool>,
    escaped: bool,
}

impl Filter {
    pub(crate) fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for &byte in bytes {
            if let Some(osc) = self.inside {
                if (osc && byte == 0x07) || (self.escaped && byte == b'\\') {
                    self.inside = None;
                    self.escaped = false;
                } else {
                    self.escaped = byte == 0x1b;
                }
                continue;
            }
            if self.held.is_empty() && byte != 0x1b {
                out.push(byte);
                continue;
            }
            self.held.push(byte);
            if let Some(intro) = TSP.iter().find(|intro| **intro == self.held.as_slice()) {
                self.inside = Some(intro[1] == b']');
                self.held.clear();
            } else if !TSP.iter().any(|intro| intro.starts_with(&self.held)) {
                // Only the first held byte is an Escape; a new one may start an introducer.
                let last = self.held.pop().unwrap_or_default();
                out.append(&mut self.held);
                if last == 0x1b {
                    self.held.push(last);
                } else {
                    out.push(last);
                }
            }
        }
    }

    /// Releases a held partial introducer after quiet input: a lone Escape key.
    pub(crate) fn flush(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.held);
    }
}

fn write_out(bytes: &[u8]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(bytes)?;
    out.flush()
}

/// Reads the pane's input, waiting at most one poll interval; `None` when nothing arrived.
fn read_in(buf: &mut [u8]) -> io::Result<Option<usize>> {
    let mut poll = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut poll, 1, POLL_MS) };
    if ready <= 0 {
        let error = last_error();
        return match ready {
            0 => Ok(None),
            _ if error.kind() == io::ErrorKind::Interrupted => Ok(None),
            _ => Err(error),
        };
    }
    let count = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
    if count < 0 {
        let error = last_error();
        return match error.kind() {
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => Ok(None),
            _ => Err(error),
        };
    }
    Ok(Some(count as usize))
}

/// Hands the pane to Yazi's subprocess until Yazi returns to its alternate screen, exits, or a
/// termination signal arrives; the surface is suspended meanwhile and resumed afterwards.
pub(crate) fn lend(
    session: &mut Session<Msg>,
    sf: Surface,
    backend: &mut Backend,
    pane: &mut PaneSize,
) -> io::Result<()> {
    let sdk = |error: tern_sdk::Error| io::Error::other(error.to_string());
    session.suspend(sf).map_err(sdk)?;
    // Settle the suspension's acknowledgement before the pane's input belongs to Yazi.
    session.pump(Duration::from_millis(30)).map_err(sdk)?;
    write_out(MODES_OFF)?;
    let result = relay(backend, pane);
    write_out(MODES_ON)?;
    session.resume(sf).map_err(sdk)?;
    result
}

fn relay(backend: &mut Backend, pane: &mut PaneSize) -> io::Result<()> {
    let mut filter = Filter::default();
    let mut buf = [0u8; 4096];
    let mut keys = Vec::with_capacity(buf.len());
    loop {
        backend.drain()?;
        let output = backend.lent.take();
        if !output.is_empty() {
            write_out(&output)?;
        }
        if !backend.lent.active || pending_signal() != 0 || backend.poll_child()?.is_some() {
            return Ok(());
        }
        if let Some((rows, cols)) = pane.current(super::pane_size()) {
            backend.resize(rows, cols);
        }
        keys.clear();
        match read_in(&mut buf)? {
            Some(0) => return Ok(()),
            Some(count) => filter.feed(&buf[..count], &mut keys),
            None => filter.flush(&mut keys),
        }
        if !keys.is_empty() {
            backend.type_in(&keys)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Filter;

    fn run(chunks: &[&[u8]]) -> Vec<u8> {
        let mut filter = Filter::default();
        let mut out = Vec::new();
        for chunk in chunks {
            filter.feed(chunk, &mut out);
        }
        out
    }

    #[test]
    fn drops_tsp_messages_split_across_reads() {
        let out = run(&[
            b"a\x1b_ts",
            b"p;e;{\"ev\":\"ack\"}\x1b",
            b"\\b\x1b]877;tsp;e;{}\x07c",
        ]);
        assert_eq!(out, b"abc");
    }

    #[test]
    fn passes_keys_and_other_escape_sequences() {
        assert_eq!(run(&[b"\x1b[A\x1b\x1b_x"]), b"\x1b[A\x1b\x1b_x");
        let mut filter = Filter::default();
        let mut out = Vec::new();
        filter.feed(b"q\x1b", &mut out);
        assert_eq!(out, b"q");
        filter.flush(&mut out);
        assert_eq!(out, b"q\x1b");
    }
}
