//! The browser as a program surface in the pane that launched Yazi. This process owns the
//! pane's tty (through the Tern session) and supervises Yazi on a private PTY; the two talk
//! through the runtime inbox and `ya emit-to`. Closing the surface returns the shell its prompt.

mod actions;
mod format;
mod input;
mod model;
mod panesize;
mod preview;
mod theme;
mod view;
mod window;

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::time::{Duration, Instant};

use tern_sdk::{Event, Input, Options, Session, Surface, SurfaceOptions};

use crate::backend::Backend;
use crate::inbox::{FileVersion, Inbox, LISTING_CAP, STATE_CAP};
use crate::supervisor::{self, Launch, Startup, Step};
use crate::sys::{install_signals, pending_signal};
use actions::Actions;
use input::{Effect, Level, Msg, Ui};
use model::{Listing, Snapshot, StateRecord};
use panesize::PaneSize;
use preview::{Blob, Load, Preview, Previews};
use view::{Frame, Waiting};
use window::Window;

const FRAME: Duration = Duration::from_millis(33);
const TICK: Duration = Duration::from_millis(25);
/// How long a frame may wait for a preview that is still loading, to avoid an empty flash.
const PREVIEW_GRACE: Duration = Duration::from_millis(40);
const NOTICE: Duration = Duration::from_secs(5);

/// Diagnostics go to `TERN_YAZI_LOG` when set: the tty belongs to the surface.
struct Log(Option<File>);

impl Log {
    fn open() -> Self {
        Self(
            env::var_os("TERN_YAZI_LOG")
                .filter(|path| !path.is_empty())
                .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok()),
        )
    }

    fn line(&mut self, line: impl AsRef<str>) {
        if let Some(file) = &mut self.0 {
            let _ = writeln!(file, "{}", line.as_ref());
        }
    }
}

fn sdk(error: tern_sdk::Error) -> io::Error {
    io::Error::other(error.to_string())
}

/// The pane tty's size in cells. Tern freezes it while the surface is live, so `PaneSize`
/// is the source that follows the pane; this one covers startup and a Tern without it.
fn pane_size() -> Option<(u16, u16)> {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
    (ok && size.ws_row > 0 && size.ws_col > 0).then_some((size.ws_row, size.ws_col))
}

/// Joined snapshots, re-read only when the files change.
struct Snapshots {
    state_name: String,
    listing_name: String,
    stamps: (Option<FileVersion>, Option<FileVersion>),
    listing: Option<Listing>,
    current: Option<Snapshot>,
    failure: Option<String>,
}

impl Snapshots {
    fn new(cid: &str) -> Self {
        Self {
            state_name: format!("state-{cid}.json"),
            listing_name: format!("listing-{cid}.json"),
            stamps: (None, None),
            listing: None,
            current: None,
            failure: None,
        }
    }

    /// Reads a newly published generation; true when the snapshot or its failure changed.
    fn refresh(&mut self, inbox: &Inbox) -> bool {
        let stamps = (
            inbox.stamp(&self.state_name),
            inbox.stamp(&self.listing_name),
        );
        if stamps == self.stamps {
            return false;
        }
        self.stamps = stamps;
        let before = (self.current.clone(), self.failure.clone());
        match self.load(inbox) {
            Ok(Some(snapshot)) => {
                self.current = Some(snapshot);
                self.failure = None;
            }
            // The actor replaces the listing before the state; wait for a matching pair.
            Ok(None) => {}
            Err(error) => self.failure = Some(error),
        }
        before != (self.current.clone(), self.failure.clone())
    }

    fn load(&mut self, inbox: &Inbox) -> Result<Option<Snapshot>, String> {
        let (bytes, _) = inbox
            .read(&self.state_name, STATE_CAP, false)
            .map_err(|error| {
                format!("Cannot read complete Yazi state (256 KiB maximum): {error}")
            })?;
        let record = StateRecord::parse(&bytes, &inbox.cid)?;
        let stale = self.listing.as_ref().is_none_or(|listing| {
            listing.epoch != record.epoch || listing.revision != record.revision
        });
        if stale {
            let (bytes, _) =
                inbox
                    .read(&self.listing_name, LISTING_CAP, false)
                    .map_err(|error| {
                        format!("Cannot read complete Yazi listing (16 MiB maximum): {error}")
                    })?;
            self.listing = Some(Listing::parse(&bytes)?);
        }
        Ok(self
            .listing
            .as_ref()
            .and_then(|listing| record.join(listing)))
    }
}

struct App {
    session: Session<Msg>,
    sf: Surface,
    log: Log,
    ui: Ui,
    actions: Actions,
    snapshots: Snapshots,
    previews: Previews,
    window: Window,
    pane: PaneSize,
    notice: Option<(Level, String, Instant)>,
    home: String,
    cols: u32,
    rows: u16,
    dirty: bool,
    drawn_role: String,
    last_frame: Option<Instant>,
}

impl App {
    fn notify(&mut self, level: Level, text: String) {
        self.log.line(format!("notice {level:?}: {text}"));
        self.notice = Some((level, text, Instant::now()));
        self.dirty = true;
    }

    /// Applies reducer effects; `Some(code)` ends the session.
    fn apply(&mut self, effects: Vec<Effect>) -> Option<i32> {
        for effect in effects {
            match effect {
                Effect::Request(body, then) => self.actions.push(body, then),
                Effect::Notice(level, text) => self.notify(level, text),
                Effect::SaveTheme(at) => {
                    if let Err(error) = theme::save(at) {
                        self.notify(
                            Level::Error,
                            format!("Cannot save the layout theme: {error}"),
                        );
                    }
                }
                Effect::Open(paths) => {
                    self.log.line(format!("open {paths:?}"));
                    self.window.open(paths)
                }
                Effect::Terminal(cwd) => self.window.terminal(cwd),
                Effect::Copy(text) => match window::copy(&text) {
                    Ok(()) => {
                        self.notify(Level::Info, format!("Copied {}", text.replace('\n', ", ")))
                    }
                    Err(error) => self.notify(Level::Error, format!("Cannot copy: {error}")),
                },
                Effect::Quit => return Some(0),
            }
        }
        self.dirty = true;
        None
    }

    fn input(&mut self, input: Input<Msg>) -> Option<i32> {
        match input {
            Input::Key(key) => {
                let Some(yz) = self.snapshots.current.as_ref() else {
                    let quit = key.is("q") || key.is("escape") || key.is("ctrl+c");
                    return quit.then_some(0);
                };
                let effects = self.ui.key(&key, yz, &self.home);
                self.apply(effects)
            }
            Input::Msg(msg, _) => {
                let yz = self.snapshots.current.as_ref()?;
                let effects = self.ui.msg(msg, yz, &self.home);
                self.apply(effects)
            }
            Input::Event(Event::Resize(resize)) => {
                self.log.line(format!(
                    "resize cols={} visible={:?}",
                    resize.cols, resize.visible
                ));
                if resize.cols > 0 && resize.cols != self.cols {
                    self.cols = resize.cols;
                    self.dirty = true;
                }
                None
            }
            Input::Event(Event::Error(error)) => {
                self.log.line(format!(
                    "surface error: {} (sheet {:?}, id {:?})",
                    error.msg, error.sheet, error.id
                ));
                None
            }
            Input::Event(_) => None,
        }
    }

    fn reply(&mut self, then: input::Then, reply: input::Reply) {
        if !reply.ok {
            if !reply.rejected {
                self.actions.clear();
                self.ui.reset();
            }
            let detail = reply
                .error
                .clone()
                .unwrap_or_else(|| "No acknowledgement".to_owned());
            self.notify(Level::Error, format!("Yazi request failed: {detail}"));
        }
        let effects = self.ui.reply(then, &reply);
        self.apply(effects);
    }

    /// Uploads a finished preview's image bytes as a session blob.
    fn upload(&mut self) {
        let Some(preview) = self.previews.ready_mut() else {
            return;
        };
        let blob = match preview {
            Preview::Image { blob, .. } => blob,
            Preview::Pdf { cover, .. } => cover,
            _ => return,
        };
        if let Some(Blob::Bytes(..)) = blob {
            let Some(Blob::Bytes(bytes, mime)) = blob.take() else {
                return;
            };
            match self.session.blob(bytes, Some(mime)) {
                Ok(id) => *blob = Some(Blob::Id(id)),
                Err(error) => self.log.line(format!("blob failed: {error}")),
            }
        }
    }

    fn render(&mut self) -> io::Result<()> {
        let yz = self.snapshots.current.as_ref();
        let target = yz.and_then(|yz| view::preview_target(yz, &self.ui));
        let state = target.as_ref().and_then(|target| self.previews.get(target));
        if let Some(Load::Loading(started)) = state {
            if started.elapsed() < PREVIEW_GRACE {
                return Ok(());
            }
        }
        if self.last_frame.is_some_and(|at| at.elapsed() < FRAME) {
            return Ok(());
        }
        let role = theme::role(&theme::THEMES[self.ui.theme], self.cols);
        // A layout swap changes row heights: withhold list selections for one frame, then
        // restore them so the native lists reveal the cursor again.
        let relayout = yz.is_some() && !self.drawn_role.is_empty() && role != self.drawn_role;
        let failure = self.snapshots.failure.as_deref();
        let frame = Frame {
            yz: if failure.is_some() { None } else { yz },
            waiting: failure.map_or(Waiting::Starting, Waiting::Failed),
            ui: &self.ui,
            preview: state,
            notice: self
                .notice
                .as_ref()
                .map(|(level, text, _)| (*level, text.as_str())),
            cols: self.cols,
            rows: u32::from(self.rows),
            home: &self.home,
            reveal_withheld: relayout,
        };
        let tree = view::view(&frame);
        self.session.render(self.sf, tree).map_err(sdk)?;
        self.last_frame = Some(Instant::now());
        self.drawn_role = role;
        self.dirty = relayout;
        Ok(())
    }

    /// One pass over everything that is not terminal input.
    fn tick(&mut self, inbox: &Inbox, backend: &mut Backend, ready: bool) {
        while let Some(failure) = self.window.failure() {
            self.notify(Level::Error, failure);
        }
        if ready && self.snapshots.refresh(inbox) {
            if let Some(yz) = &self.snapshots.current {
                self.ui.sync(yz, self.actions.pending_filter());
            }
            self.dirty = true;
        }
        if let Some((rows, cols)) = self.pane.current(pane_size()) {
            if rows != self.rows {
                self.rows = rows;
                self.dirty = true;
            }
            backend.resize(rows, cols);
        }
        while let Some((then, reply)) = self.actions.poll(inbox) {
            self.reply(then, reply);
        }
        match self
            .snapshots
            .current
            .as_ref()
            .and_then(|yz| view::preview_target(yz, &self.ui))
        {
            Some(target) => self.previews.want(target),
            None => self.previews.forget(),
        }
        if self.previews.poll() {
            self.upload();
            self.dirty = true;
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, _, at)| at.elapsed() >= NOTICE)
        {
            self.notice = None;
            self.dirty = true;
        }
    }
}

/// Runs Yazi under this pane's surface; without TSP, hands the arguments to the original Yazi.
pub(crate) fn run(launch: &Launch, real: &OsStr, args: &[OsString]) -> io::Result<i32> {
    // The session saves these and restores them on close, so a hangup that arrives while the
    // pane goes away still ends in the bounded shutdown rather than the default action.
    install_signals();
    let Some(mut session) =
        Session::<Msg>::connect(Options::new().app("tern-yazi")).map_err(sdk)?
    else {
        return supervisor::passthrough(real, args);
    };
    // Ours replace the session's restore-and-die handlers, so a signal ends the loop and the
    // session still restores the tty on close.
    let caller_umask = supervisor::prepare();
    let opened = session.open(
        SurfaceOptions::screen()
            .title("Yazi")
            .role("tern_yazi.browser")
            .keep(false),
    );
    let sf = match opened {
        Ok(sf) => sf,
        Err(error) => {
            let _ = session.close();
            return Err(sdk(error));
        }
    };
    let mut log = Log::open();
    for (name, css) in theme::SHEETS {
        if let Err(error) = session.stylesheet(sf, name, Some(css)) {
            log.line(format!("stylesheet {name} failed: {error}"));
        }
    }
    let inbox = match Inbox::new() {
        Ok(inbox) => inbox,
        Err(error) => {
            let _ = session.close();
            return Err(error);
        }
    };
    let mut backend = match supervisor::spawn(&inbox, launch, caller_umask) {
        Ok(backend) => backend,
        Err(error) => {
            let _ = session.close();
            return Err(error);
        }
    };
    let cols = session.caps().cols;
    let mut app = App {
        session,
        sf,
        log,
        ui: Ui::new(theme::initial()),
        actions: Actions::new(&inbox.cid, &inbox.token),
        snapshots: Snapshots::new(&inbox.cid),
        previews: Previews::new(),
        window: Window::new(env::var("TERN_PANE").unwrap_or_default()),
        pane: PaneSize::new(&env::var("TERN_PANE").unwrap_or_default()),
        notice: None,
        home: env::var("HOME").unwrap_or_default(),
        cols,
        rows: pane_size().map_or(24, |(rows, _)| rows),
        dirty: true,
        drawn_role: String::new(),
        last_frame: None,
    };
    let mut startup = Startup::new();
    let mut ready = false;
    let result = loop {
        let signal = pending_signal();
        if signal != 0 {
            backend.signal(signal);
            break Ok(128 + signal);
        }
        match startup.poll(&inbox, &mut backend) {
            Ok(Step::Starting) => {}
            Ok(Step::Ready) => ready = true,
            Ok(Step::Exited(code)) => break Ok(code),
            Err(error) => break Err(error),
        }
        app.tick(&inbox, &mut backend, ready);
        if app.dirty {
            if let Err(error) = app.render() {
                break Err(error);
            }
        }
        let wait = match app.last_frame {
            Some(at) if app.dirty => FRAME
                .saturating_sub(at.elapsed())
                .clamp(Duration::from_millis(1), TICK),
            _ => TICK,
        };
        match app.session.next(Some(wait)) {
            Ok(Some(input)) => {
                if let Some(code) = app.input(input) {
                    break Ok(code);
                }
            }
            Ok(None) => {}
            Err(error) => break Err(sdk(error)),
        }
        if !app.session.is_open(app.sf) {
            break Ok(0);
        }
    };
    let App {
        mut session,
        sf,
        mut log,
        ..
    } = app;
    if session.is_open(sf) {
        let _ = session.close_surface(sf, false);
    }
    if let Err(error) = session.close() {
        log.line(format!("session close failed: {error}"));
    }
    startup.report(&backend, &result);
    supervisor::shutdown(backend);
    drop(inbox);
    result
}
