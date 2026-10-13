//! Yazi's lent terminal on the pane. Yazi leaves its alternate screen to run a subprocess on
//! its own terminal (`shell --block`, the fzf and zoxide jumps, task inspection); the surface
//! then steps aside and the pane's tty is relayed to Yazi's PTY until Yazi takes it back, so
//! the subprocess runs and stays in the pane as if Yazi ran there.

use std::cell::{Cell, RefCell};
use std::io::{self, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use tern_sdk::term::{self, Terminal, Tty};
use tern_sdk::{Options, Session, Surface};

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
/// How long the suspension may wait for flow-control credit before the pane is lent anyway.
const SUSPEND_WAIT: Duration = Duration::from_secs(1);
/// How long, after Yazi takes its screen back, the pane's answers to Yazi's last queries may take.
const ANSWER_WAIT: Duration = Duration::from_millis(300);

/// What the lent terminal shares with the session's terminal.
#[derive(Clone, Default)]
pub(crate) struct Link {
    /// Writes the session has made, to see when a queued frame goes out.
    writes: Rc<Cell<u64>>,
    /// TSP input the relay set aside (acks, resizes, ...), for the session to read first.
    held: Rc<RefCell<Vec<u8>>>,
}

/// The session's terminal: the pane's tty, plus the TSP input held while the pane was lent.
struct Pane {
    tty: Tty,
    link: Link,
}

impl Terminal for Pane {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.tty.write(bytes)?;
        self.link.writes.set(self.link.writes.get() + 1);
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<Option<usize>> {
        let mut held = self.link.held.borrow_mut();
        if held.is_empty() {
            drop(held);
            return self.tty.read(buf, timeout);
        }
        let count = held.len().min(buf.len());
        buf[..count].copy_from_slice(&held[..count]);
        held.drain(..count);
        Ok(Some(count))
    }

    fn restore(&mut self) -> io::Result<()> {
        self.tty.restore()
    }

    fn modes(&mut self, paste: bool, kitty: bool) {
        self.tty.modes(paste, kitty);
    }
}

/// [`Session::connect`] over a [`Pane`]: `Ok(None)` where the SDK would not speak TSP
/// (`TERN_TSP=0`, tmux, screen, zellij, no tty, or no answer).
pub(crate) fn connect(options: Options) -> Result<Option<(Session<Msg>, Link)>, tern_sdk::Error> {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if std::env::var_os("TERN_TSP").is_some_and(|v| v == "0")
        || set("TMUX")
        || set("STY")
        || set("ZELLIJ")
        || !term::is_tty()
    {
        return Ok(None);
    }
    let link = Link::default();
    let pane = Pane {
        tty: Tty::open()?,
        link: link.clone(),
    };
    Ok(Session::with_terminal(pane, options)?.map(|session| (session, link)))
}

/// Splits the pane's input into Tern's TSP messages and everything else, which is Yazi's.
#[derive(Debug, Default)]
pub(crate) struct Filter {
    held: Vec<u8>,
    /// Inside a TSP message: `Some(true)` for OSC, which BEL may end too.
    inside: Option<bool>,
    escaped: bool,
}

impl Filter {
    pub(crate) fn feed(&mut self, bytes: &[u8], keys: &mut Vec<u8>, tsp: &mut Vec<u8>) {
        for &byte in bytes {
            if let Some(osc) = self.inside {
                tsp.push(byte);
                if (osc && byte == 0x07) || (self.escaped && byte == b'\\') {
                    self.inside = None;
                    self.escaped = false;
                } else {
                    self.escaped = byte == 0x1b;
                }
                continue;
            }
            if self.held.is_empty() && byte != 0x1b {
                keys.push(byte);
                continue;
            }
            self.held.push(byte);
            if let Some(intro) = TSP.iter().find(|intro| **intro == self.held.as_slice()) {
                self.inside = Some(intro[1] == b']');
                tsp.append(&mut self.held);
            } else if !TSP.iter().any(|intro| intro.starts_with(&self.held)) {
                // Only the first held byte is an Escape; a new one may start an introducer.
                let last = self.held.pop().unwrap_or_default();
                keys.append(&mut self.held);
                if last == 0x1b {
                    self.held.push(last);
                } else {
                    keys.push(last);
                }
            }
        }
    }

    /// Releases a held partial introducer after quiet input: a lone Escape key.
    pub(crate) fn flush(&mut self, keys: &mut Vec<u8>) {
        keys.append(&mut self.held);
    }

    /// At hand-back, input still undecided belongs to the session again.
    fn finish(mut self, tsp: &mut Vec<u8>) {
        tsp.append(&mut self.held);
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
    link: &Link,
    sf: Surface,
    backend: &mut Backend,
    pane: &mut PaneSize,
) -> io::Result<()> {
    let sdk = |error: tern_sdk::Error| io::Error::other(error.to_string());
    // The suspension waits behind frames out of credit; only once it is written does the
    // pane's input belong to Yazi.
    let writes = link.writes.get();
    session.suspend(sf).map_err(sdk)?;
    let deadline = Instant::now() + SUSPEND_WAIT;
    while link.writes.get() == writes && Instant::now() < deadline {
        session.pump(Duration::from_millis(10)).map_err(sdk)?;
    }
    write_out(MODES_OFF)?;
    let result = relay(backend, pane, link);
    write_out(MODES_ON)?;
    // Acks held during the relay restore the credit the resumption may need.
    session.pump(Duration::ZERO).map_err(sdk)?;
    session.resume(sf).map_err(sdk)?;
    result
}

fn relay(backend: &mut Backend, pane: &mut PaneSize, link: &Link) -> io::Result<()> {
    let mut filter = Filter::default();
    let mut buf = [0u8; 4096];
    let mut keys = Vec::with_capacity(buf.len());
    // Yazi queries the pane just before it takes its screen back; the answers, which end with
    // the device attributes, are still Yazi's.
    let mut owed = false;
    let mut returned: Option<Instant> = None;
    let result = loop {
        backend.drain()?;
        let output = backend.lent.take();
        if !output.is_empty() {
            write_out(&output)?;
            owed |= da1_query(&output);
        }
        if pending_signal() != 0 || backend.poll_child()?.is_some() {
            break Ok(());
        }
        if !backend.lent.active {
            let since = *returned.get_or_insert_with(Instant::now);
            if !owed || since.elapsed() >= ANSWER_WAIT {
                break Ok(());
            }
        }
        if let Some((rows, cols)) = pane.current(super::pane_size()) {
            backend.resize(rows, cols);
        }
        keys.clear();
        match read_in(&mut buf) {
            Ok(Some(0)) => break Ok(()),
            Ok(Some(count)) => {
                filter.feed(&buf[..count], &mut keys, &mut link.held.borrow_mut());
            }
            Ok(None) => filter.flush(&mut keys),
            Err(error) => break Err(error),
        }
        if !keys.is_empty() {
            let answers = owed && keys[0] == 0x1b && backend.cooked();
            owed &= !da1_reply(&keys);
            // Yazi leaves raw mode right after the burst when a task of its own (bulk rename)
            // takes the terminal back, without waiting for the answers. Typed into the line
            // discipline they would become the input of the task's prompt.
            if !answers {
                backend.type_in(&keys)?;
            }
        }
    };
    filter.finish(&mut link.held.borrow_mut());
    result
}

/// Whether `output` asks the terminal for its primary device attributes (`CSI c`, `CSI 0 c`).
fn da1_query(output: &[u8]) -> bool {
    output.windows(3).any(|w| w == b"\x1b[c") || output.windows(4).any(|w| w == b"\x1b[0c")
}

/// Whether `input` holds a primary device attributes answer (`CSI ? Ps ; ... c`).
fn da1_reply(input: &[u8]) -> bool {
    input.windows(3).enumerate().any(|(at, w)| {
        w == b"\x1b[?"
            && input[at + 3..]
                .iter()
                .position(|b| !(b.is_ascii_digit() || *b == b';'))
                .is_some_and(|end| end > 0 && input[at + 3 + end] == b'c')
    })
}

#[cfg(test)]
mod tests {
    use super::{da1_query, da1_reply, Filter};

    fn run(chunks: &[&[u8]]) -> (Vec<u8>, Vec<u8>) {
        let mut filter = Filter::default();
        let (mut keys, mut tsp) = (Vec::new(), Vec::new());
        for chunk in chunks {
            filter.feed(chunk, &mut keys, &mut tsp);
        }
        (keys, tsp)
    }

    #[test]
    fn sets_aside_tsp_messages_split_across_reads() {
        let (keys, tsp) = run(&[
            b"a\x1b_ts",
            b"p;e;{\"ev\":\"ack\"}\x1b",
            b"\\b\x1b]877;tsp;e;{}\x07c",
        ]);
        assert_eq!(keys, b"abc");
        assert_eq!(tsp, b"\x1b_tsp;e;{\"ev\":\"ack\"}\x1b\\\x1b]877;tsp;e;{}\x07");
    }

    #[test]
    fn passes_keys_and_other_escape_sequences() {
        let (keys, tsp) = run(&[b"\x1b[A\x1b\x1b_x"]);
        assert_eq!(keys, b"\x1b[A\x1b\x1b_x");
        assert!(tsp.is_empty());
        let mut filter = Filter::default();
        let (mut keys, mut tsp) = (Vec::new(), Vec::new());
        filter.feed(b"q\x1b", &mut keys, &mut tsp);
        assert_eq!(keys, b"q");
        filter.flush(&mut keys);
        assert_eq!(keys, b"q\x1b");
    }

    #[test]
    fn recognizes_the_device_attributes_that_end_a_query_burst() {
        assert!(da1_query(b"\x1b]11;?\x07\x1b[>q\x1b[0c\x1b[1K"));
        assert!(da1_query(b"\x1b[c"));
        assert!(!da1_query(b"\x1b[?12$p\x1b[16t"));
        assert!(da1_reply(b"\x1b[?1u\x1bP>|kitty(0.48.2)\x1b\\\x1b[?62;22c"));
        assert!(!da1_reply(b"\x1b[?1u\x1b[?12;2$y"));
        assert!(!da1_reply(b"\x1b[?c"));
        assert!(!da1_reply(b"\x1b[?62;22"));
    }
}
