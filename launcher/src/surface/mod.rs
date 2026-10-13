//! The browser as a program surface in the pane that launched Yazi. This process owns the
//! pane's tty (through the Tern session) and supervises Yazi on a private PTY; the two talk
//! through the runtime inbox and `ya emit-to`. Closing the surface returns the shell its prompt.

mod actions;
mod editor;
mod format;
mod handover;
mod input;
mod model;
mod panesize;
mod preview;
mod theme;
mod view;
mod window;

use std::collections::VecDeque;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::time::{Duration, Instant};

use tern_sdk::{Event, Input, Options, Session, Surface, SurfaceOptions};

use crate::backend::Backend;
use crate::inbox::{FileVersion, Inbox, LISTING_CAP, STATE_CAP};
use crate::supervisor::{self, Launch, Startup, Step};
use crate::sys::{
    install_job_control, install_signals, pending_signal, request_stop, stop_group,
    take_stop_request,
};
use actions::Actions;
use handover::Link;
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
/// How long Yazi gets to exit after it was told to quit before the launcher stops it.
const QUIT_WAIT: Duration = Duration::from_secs(3);
/// Yazi notifications shown at most per snapshot; the rest only reach the log.
const NOTICE_BATCH: usize = 3;
/// Bounds of how long a Yazi notification stays, whatever timeout it asked for.
const NOTICE_SPAN: (f64, f64) = (2.0, 10.0);

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

/// The pane's TSP session is gone only while the browser is suspended.
struct App {
    session: Option<Session<Msg>>,
    link: Link,
    sf: Surface,
    log: Log,
    ui: Ui,
    actions: Actions,
    snapshots: Snapshots,
    previews: Previews,
    window: Window,
    pane: PaneSize,
    notice: Option<(Level, String, Instant)>,
    /// How long `notice` stays.
    notice_for: Duration,
    /// Yazi notifications waiting for the notice slot.
    backlog: VecDeque<(Level, String, Duration)>,
    /// The newest Yazi notification id already queued; Yazi's ids only grow.
    seen_notice: Option<u64>,
    /// Set once Yazi was told to quit: when to stop it if it has not exited.
    quit_deadline: Option<Instant>,
    home: String,
    cols: u32,
    rows: u16,
    dirty: bool,
    drawn_role: String,
    last_frame: Option<Instant>,
}

impl App {
    /// The live session; absent only while [`suspend`](Self::suspend) runs.
    fn session(&mut self) -> io::Result<&mut Session<Msg>> {
        self.session
            .as_mut()
            .ok_or_else(|| io::Error::other("the TSP session is closed"))
    }

    /// Suspends the browser like Yazi's own Ctrl+Z (`signals.rs`: restore the terminal, stop the
    /// process group): the session closes, which removes the surface and returns the pane's tty to
    /// the shell, then Yazi's group and this one stop. The shell regains the foreground and
    /// reports the job; `fg` continues Yazi's group and attaches a new session and surface.
    fn suspend(&mut self, backend: &Backend) -> io::Result<()> {
        if let Some(session) = self.session.take() {
            if let Err(error) = session.close() {
                self.log.line(format!("session close failed: {error}"));
            }
        }
        // Yazi's tasks pause with it; its own group is not the shell's job.
        backend.signal(libc::SIGSTOP);
        stop_group();
        backend.signal(libc::SIGCONT);
        let Some((session, link, sf)) = attach(&mut self.log)? else {
            return Err(io::Error::other(
                "Tern no longer answers after the suspension",
            ));
        };
        self.cols = session.caps().cols;
        self.session = Some(session);
        self.link = link;
        self.sf = sf;
        // Blob ids and the frame baseline belonged to the closed session.
        self.previews.forget();
        self.drawn_role.clear();
        self.last_frame = None;
        self.dirty = true;
        Ok(())
    }

    fn notify(&mut self, level: Level, text: String) {
        self.log.line(format!("notice {level:?}: {text}"));
        self.notice = Some((level, text, Instant::now()));
        self.notice_for = NOTICE;
        self.dirty = true;
    }

    /// Queues the Yazi notifications not seen yet: Yazi draws them on its hidden terminal.
    fn absorb_notices(&mut self) {
        let Some(yz) = &self.snapshots.current else {
            return;
        };
        let seen = self.seen_notice;
        let fresh: Vec<_> = yz
            .notices
            .iter()
            .filter(|notice| seen.is_none_or(|seen| notice.id > seen))
            .cloned()
            .collect();
        let Some(newest) = fresh.iter().map(|notice| notice.id).max() else {
            return;
        };
        self.seen_notice = Some(newest);
        let skipped = fresh.len().saturating_sub(NOTICE_BATCH);
        for (index, notice) in fresh.into_iter().enumerate() {
            let level = match notice.level.as_str() {
                "error" => Level::Error,
                "warn" => Level::Warn,
                _ => Level::Info,
            };
            let text = if notice.title.is_empty() {
                notice.content.clone()
            } else {
                format!("{}: {}", notice.title, notice.content)
            };
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            self.log.line(format!("yazi notice {level:?}: {text}"));
            if index >= skipped {
                let span = notice.timeout.clamp(NOTICE_SPAN.0, NOTICE_SPAN.1);
                self.backlog
                    .push_back((level, text, Duration::from_secs_f64(span)));
            }
        }
    }

    /// Moves the next waiting Yazi notification into the empty notice slot.
    fn next_notice(&mut self) {
        if self.notice.is_none() {
            if let Some((level, text, span)) = self.backlog.pop_front() {
                self.notice = Some((level, text, Instant::now()));
                self.notice_for = span;
                self.dirty = true;
            }
        }
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
                Effect::Quitting => {
                    self.quit_deadline
                        .get_or_insert_with(|| Instant::now() + QUIT_WAIT);
                }
                Effect::Suspend => request_stop(),
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
            if matches!(then, input::Then::Quit { .. }) {
                // Without a working quit request the old stop path still ends the session.
                self.log.line("quit request failed; stopping Yazi");
                self.quit_deadline = Some(Instant::now());
            }
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
            let Some(session) = self.session.as_mut() else {
                return;
            };
            match session.blob(bytes, Some(mime)) {
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
        let sf = self.sf;
        self.session()?.render(sf, tree).map_err(sdk)?;
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
            self.absorb_notices();
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
            .is_some_and(|(_, _, at)| at.elapsed() >= self.notice_for)
        {
            self.notice = None;
            self.dirty = true;
        }
        self.next_notice();
    }
}

/// Connects the pane's TSP session and opens the browser's surface with its stylesheets;
/// `Ok(None)` where Tern does not answer.
fn attach(log: &mut Log) -> io::Result<Option<(Session<Msg>, Link, Surface)>> {
    // The session saves these and restores them on close, so a hangup that arrives while the
    // pane goes away still ends in the bounded shutdown rather than the default action.
    install_signals();
    let Some((mut session, link)) =
        handover::connect(Options::new().app("tern-yazi")).map_err(sdk)?
    else {
        return Ok(None);
    };
    // Ours replace the session's restore-and-die handlers, so a signal ends the loop and the
    // session still restores the tty on close.
    install_signals();
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
    for (name, css) in theme::SHEETS {
        if let Err(error) = session.stylesheet(sf, name, Some(css)) {
            log.line(format!("stylesheet {name} failed: {error}"));
        }
    }
    Ok(Some((session, link, sf)))
}

/// Runs Yazi under this pane's surface; without TSP, hands the arguments to the original Yazi.
pub(crate) fn run(launch: &Launch, real: &OsStr, args: &[OsString]) -> io::Result<i32> {
    let mut log = Log::open();
    let Some((session, link, sf)) = attach(&mut log)? else {
        return supervisor::passthrough(real, args);
    };
    let caller_umask = supervisor::prepare();
    // A stop request (`kill -TSTP`) suspends the browser like Ctrl+Z instead of stopping the
    // process under the surface.
    install_job_control();
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
    let mut ui = Ui::new(theme::initial());
    ui.chooser = launch.chooser;
    let mut app = App {
        session: Some(session),
        link,
        sf,
        log,
        ui,
        actions: Actions::new(&inbox.cid, &inbox.token),
        snapshots: Snapshots::new(&inbox.cid),
        previews: Previews::new(),
        window: Window::new(env::var("TERN_PANE").unwrap_or_default()),
        pane: PaneSize::new(&env::var("TERN_PANE").unwrap_or_default()),
        notice: None,
        notice_for: NOTICE,
        backlog: VecDeque::new(),
        seen_notice: None,
        quit_deadline: None,
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
        if backend.lent.active {
            let Some(session) = app.session.as_mut() else {
                break Err(io::Error::other("the TSP session is closed"));
            };
            if let Err(error) =
                handover::lend(session, &app.link, app.sf, &mut backend, &mut app.pane)
            {
                break Err(error);
            }
            app.dirty = true;
            continue;
        }
        if take_stop_request() {
            if let Err(error) = app.suspend(&backend) {
                break Err(error);
            }
            continue;
        }
        app.tick(&inbox, &mut backend, ready);
        if app.quit_deadline.is_some_and(|at| Instant::now() >= at) {
            app.log.line("Yazi did not exit after quit; stopping it");
            break Ok(0);
        }
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
        let next = match app.session() {
            Ok(session) => session.next(Some(wait)).map_err(sdk),
            Err(error) => Err(error),
        };
        match next {
            Ok(Some(input)) => {
                if let Some(code) = app.input(input) {
                    break Ok(code);
                }
            }
            Ok(None) => {}
            Err(error) => break Err(error),
        }
        let sf = app.sf;
        if app
            .session
            .as_ref()
            .is_some_and(|session| !session.is_open(sf))
        {
            break Ok(0);
        }
    };
    let App {
        session,
        sf,
        mut log,
        ..
    } = app;
    if let Some(mut session) = session {
        if session.is_open(sf) {
            let _ = session.close_surface(sf, false);
        }
        if let Err(error) = session.close() {
            log.line(format!("session close failed: {error}"));
        }
    }
    startup.report(&backend, &result);
    supervisor::shutdown(backend);
    drop(inbox);
    result
}
