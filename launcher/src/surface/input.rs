//! The input reducer: keys and pointer messages become UI state changes plus effects (Yazi
//! requests, notices, theme persistence, quit). Pure: no I/O, no session.

use serde_json::{json, Map, Value};
use tern_sdk::keys::Key;

use super::editor::{utf16_len, Edit, Editor};
use super::model::{basename, join_path, parent_dir, relative, Snapshot};
use super::theme::{self, THEMES};

/// The fields the view draws as native inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Path,
    Rename,
    /// Any other line mode (shell, find, search, filter, create): the one open line.
    Line,
}

/// Native editing replaced UTF-16 `[from, to)` of a text Tern saw as `len` units long.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FieldEdit {
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) text: String,
    pub(crate) cursor: usize,
    pub(crate) len: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Mode {
    Normal,
    Path(Editor),
    /// The `:` (Yazi `shell --block`) or `;` (Yazi background task) shell line. Yazi runs a
    /// blocking shell on the terminal it lends to the pane; a background one is a Yazi task.
    Command {
        editor: Editor,
        block: bool,
    },
    Find {
        editor: Editor,
        previous: bool,
    },
    /// Yazi's recursive search line: `s` by name via fd, `S` by content via ripgrep.
    Search {
        editor: Editor,
        content: bool,
    },
    /// Yazi's task manager (`w`), open in Yazi while this mode lasts.
    Tasks,
    /// Live filter line; `Ui::filter_query` mirrors its text and outlives it.
    Filter(Editor),
    Create(Editor),
    Rename {
        editor: Editor,
        target: String,
    },
    /// Exact paths Yazi prepared for Trash, awaiting confirmation.
    Trash(Vec<String>),
    /// Yazi has unfinished tasks and asks (`quit_title`/`quit_body`) whether to quit anyway.
    /// `titles` are known only while Yazi's task manager is open; `count` always is.
    Quit {
        no_cwd: bool,
        count: u64,
        titles: Vec<String>,
    },
    /// The theme picker; `original` is restored on cancel.
    Theme {
        at: usize,
        original: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum List {
    Parent,
    Files,
    Child,
    Themes,
}

/// What the view's handlers send back.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Msg {
    FieldFocus(Field),
    FieldEdit(Field, FieldEdit),
    FieldSend(Field, String),
    PathEdit,
    PathCancel,
    PathMore,
    PathSubmit,
    PathCd(String),
    ClearInput,
    TabSwitch(usize),
    TrashConfirm,
    TrashCancel,
    QuitConfirm,
    QuitCancel,
    Select(List, String),
    Activate(List, String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Jump {
    query: Option<String>,
    target: String,
}

/// What to do when Yazi answers a request.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Then {
    Nothing,
    Jump(Jump),
    TrashPrepared,
    TrashQueued,
    /// Yazi's answer to a quit request: unfinished tasks to confirm, or it is quitting.
    Quit {
        no_cwd: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Effect {
    /// A Yazi request body (without its id) and its continuation.
    Request(Value, Then),
    Notice(Level, String),
    SaveTheme(usize),
    /// Window actions: open paths in Tern, a terminal in a directory, text to the clipboard.
    Open(Vec<String>),
    Terminal(String),
    Copy(String),
    /// Yazi was told to quit; the surface waits for its exit.
    Quitting,
    /// Stops the browser and Yazi and returns the shell, until `fg` (Yazi's `suspend`).
    Suspend,
}

/// Yazi's answer to one request.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Reply {
    pub(crate) ok: bool,
    pub(crate) rejected: bool,
    pub(crate) error: Option<String>,
    pub(crate) paths: Vec<String>,
    /// Unfinished Yazi tasks a quit request found, with their titles when known.
    pub(crate) tasks: u64,
    pub(crate) titles: Vec<String>,
    /// Yazi accepted the quit and is exiting.
    pub(crate) quitting: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    G,
    T,
    C,
}

pub(crate) struct Ui {
    pub(crate) mode: Mode,
    /// The filter query; it outlives filter editing while Yazi keeps the filter.
    pub(crate) filter_query: String,
    /// Optimistic cursor and directory until Yazi publishes its own.
    pub(crate) active_file: Option<String>,
    pub(crate) optimistic_cwd: Option<String>,
    pub(crate) path_menu: bool,
    pub(crate) theme: usize,
    /// Launched with `--chooser-file`: Enter and `o` choose files through Yazi's `open`.
    pub(crate) chooser: bool,
    path_jump: Option<Jump>,
    pending: Option<Pending>,
    source: Option<(String, Option<String>)>,
    filter_seq: Option<u64>,
}

/// `{op:"action", action, args:{positional, options}}`, the actor's argument shape.
fn action(name: &str, positional: &[&str], options: Value) -> Effect {
    let options = match options {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    Effect::Request(
        json!({"op": "action", "action": name,
            "args": {"positional": positional, "options": options}}),
        Then::Nothing,
    )
}

fn request(body: Value) -> Effect {
    Effect::Request(body, Then::Nothing)
}

/// A Yazi task manager action: `tasks:show`, `tasks:arrow prev`, ...
fn tasks(action: &str, step: Option<&str>) -> Effect {
    let mut body = json!({"op": "tasks", "action": action});
    if let Some(step) = step {
        body["step"] = json!(step);
    }
    request(body)
}

/// Resolves a typed directory against `cwd`; `..` is left for the filesystem to resolve.
pub(crate) fn resolve_directory(query: &str, cwd: &str, home: &str) -> Option<String> {
    if query.is_empty() || query.contains(['\0', '\r', '\n']) {
        return None;
    }
    let mut query = query.to_owned();
    if query == "~" || query.starts_with("~/") {
        if home.is_empty() {
            return None;
        }
        query = format!("{home}{}", &query[1..]);
    }
    if !query.starts_with('/') {
        query = join_path(cwd, &query);
    }
    Some(query)
}

/// What `cc`, `cd`, `cf` and `cn` copy, like Yazi's `copy`: the selection, else the hovered
/// entry, one per line.
fn copy_paths(what: &str, yz: &Snapshot) -> Option<String> {
    let urls: Vec<&str> = if yz.selected_urls.is_empty() {
        yz.hovered.iter().map(|hover| hover.url.as_str()).collect()
    } else {
        yz.selected_urls.iter().map(String::as_str).collect()
    };
    let lines: Vec<&str> = urls
        .into_iter()
        .map(|url| match what {
            "d" => parent_dir(url).unwrap_or(url),
            "f" => basename(url),
            "n" => {
                let name = basename(url);
                name.rfind('.')
                    .filter(|dot| *dot > 0)
                    .map_or(name, |dot| &name[..dot])
            }
            _ => url,
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

impl Ui {
    pub(crate) fn new(theme: usize) -> Self {
        Self {
            mode: Mode::Normal,
            filter_query: String::new(),
            active_file: None,
            optimistic_cwd: None,
            path_menu: false,
            theme,
            path_jump: None,
            chooser: false,
            pending: None,
            source: None,
            filter_seq: None,
        }
    }

    /// The directory the browser shows: optimistic until Yazi confirms.
    pub(crate) fn cwd<'a>(&'a self, yz: &'a Snapshot) -> &'a str {
        self.optimistic_cwd.as_deref().unwrap_or(&yz.current.cwd)
    }

    /// Reconciles optimistic state with a newly published snapshot.
    pub(crate) fn sync(&mut self, yz: &Snapshot, filter_pending: bool) {
        let hovered = yz.hovered.as_ref().map(|hover| hover.url.clone());
        let source = (yz.current.cwd.clone(), hovered);
        if self.source.as_ref().map(|s| &s.0) != Some(&source.0) {
            self.pending = None;
        }
        if self.source.as_ref() != Some(&source) {
            self.active_file = None;
            self.optimistic_cwd = None;
            self.source = Some(source);
        }
        // The published snapshot can prove a submitted path before its reply arrives;
        // keys typed after the jump must reach Yazi, not the stale editor.
        if let Some(jump) = self.path_jump.take_if(|jump| jump.target == yz.current.cwd) {
            self.close_path(&jump);
        }
        // Only pending filter mutations own the local query.
        if !matches!(self.mode, Mode::Filter(_))
            && !filter_pending
            && self.filter_seq != Some(yz.seq)
        {
            if let Some(filter) = &yz.filter {
                self.filter_query = filter.clone().unwrap_or_default();
                self.filter_seq = Some(yz.seq);
            }
        }
    }

    fn close_path(&mut self, jump: &Jump) {
        if let Mode::Path(editor) = &self.mode {
            if jump.query.as_deref() == Some(editor.text.as_str()) {
                self.mode = Mode::Normal;
            }
        }
    }

    /// Forgets optimistic state after a failed request.
    pub(crate) fn reset(&mut self) {
        self.active_file = None;
        self.optimistic_cwd = None;
    }

    pub(crate) fn reply(&mut self, then: Then, reply: &Reply) -> Vec<Effect> {
        let mut effects = Vec::new();
        match then {
            Then::Nothing => {}
            Then::Jump(jump) => {
                if self.path_jump.as_ref() == Some(&jump) {
                    self.path_jump = None;
                }
                if reply.ok {
                    self.reset();
                    self.close_path(&jump);
                }
            }
            Then::TrashPrepared if reply.ok => {
                if reply.paths.is_empty() {
                    effects.push(Effect::Notice(Level::Info, "Nothing to trash".to_owned()));
                } else {
                    self.mode = Mode::Trash(reply.paths.clone());
                }
            }
            Then::TrashQueued if reply.ok => effects.push(Effect::Notice(
                Level::Info,
                "Trash queued in Yazi; completion is reported by Yazi tasks".to_owned(),
            )),
            Then::Quit { no_cwd } if reply.ok => {
                if reply.quitting {
                    effects.push(Effect::Quitting);
                } else if reply.tasks > 0 {
                    self.mode = Mode::Quit {
                        no_cwd,
                        count: reply.tasks,
                        titles: reply.titles.clone(),
                    };
                }
            }
            Then::Quit { .. } => {}
            Then::TrashPrepared | Then::TrashQueued => {}
        }
        effects
    }

    fn jump(&mut self, target: String) -> Effect {
        let query = match &self.mode {
            Mode::Path(editor) => Some(editor.text.clone()),
            _ => None,
        };
        let jump = Jump { query, target };
        self.path_menu = false;
        self.path_jump = Some(jump.clone());
        Effect::Request(json!({"op": "cd", "target": jump.target}), Then::Jump(jump))
    }

    fn submit_path(&mut self, yz: &Snapshot, home: &str) -> Effect {
        let Mode::Path(editor) = &self.mode else {
            unreachable!("path submission outside path mode");
        };
        match resolve_directory(&editor.text, &yz.current.cwd, home) {
            Some(target) => self.jump(target),
            None => Effect::Notice(
                Level::Error,
                "Invalid directory: enter a directory path on one line".to_owned(),
            ),
        }
    }

    fn trash_decision(&mut self, confirm: bool) -> Vec<Effect> {
        let Mode::Trash(paths) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return Vec::new();
        };
        if !confirm {
            return Vec::new();
        }
        vec![Effect::Request(
            json!({"op": "trash_commit", "paths": paths}),
            Then::TrashQueued,
        )]
    }

    /// Yazi's `quit` (`q`) or `quit --no-cwd-file` (`Q`): Yazi answers with unfinished tasks to
    /// confirm, or quits itself.
    fn quit(&self, no_cwd: bool) -> Effect {
        Effect::Request(
            json!({"op": "quit", "no_cwd_file": no_cwd}),
            Then::Quit { no_cwd },
        )
    }

    fn quit_decision(&mut self, no_cwd: bool, confirm: bool) -> Vec<Effect> {
        self.mode = Mode::Normal;
        if !confirm {
            return Vec::new();
        }
        vec![Effect::Request(
            json!({"op": "quit", "no_cwd_file": no_cwd, "confirmed": true}),
            Then::Quit { no_cwd },
        )]
    }

    fn clear_find(&mut self) -> Effect {
        self.mode = Mode::Normal;
        action("escape", &[], json!({"find": true}))
    }

    fn clear_filter(&mut self) -> Effect {
        self.mode = Mode::Normal;
        self.filter_query.clear();
        request(json!({"op": "filter", "query": ""}))
    }

    /// The editor of the open line mode, whichever it is.
    fn line_editor(&mut self) -> Option<&mut Editor> {
        match &mut self.mode {
            Mode::Path(editor)
            | Mode::Command { editor, .. }
            | Mode::Find { editor, .. }
            | Mode::Search { editor, .. }
            | Mode::Filter(editor)
            | Mode::Create(editor)
            | Mode::Rename { editor, .. } => Some(editor),
            _ => None,
        }
    }

    /// Feeds a key to the open line's editor and acts on what it reports.
    fn line_key(&mut self, key: &Key, yz: &Snapshot, home: &str) -> Vec<Effect> {
        let Some(editor) = self.line_editor() else {
            return Vec::new();
        };
        match editor.key(key) {
            Edit::Unchanged => Vec::new(),
            Edit::Changed => self.line_changed(),
            Edit::Cancel => match self.mode {
                Mode::Find { .. } => vec![self.clear_find()],
                Mode::Filter(_) => vec![self.clear_filter()],
                _ => {
                    self.mode = Mode::Normal;
                    Vec::new()
                }
            },
            Edit::Submit => self.line_submit(yz, home),
        }
    }

    /// The line's text changed: live modes (find, filter) tell Yazi at once.
    fn line_changed(&mut self) -> Vec<Effect> {
        match &self.mode {
            Mode::Find { editor, previous } => {
                let body = json!({"op": "find", "query": editor.text, "previous": previous});
                self.active_file = None;
                vec![request(body)]
            }
            Mode::Filter(editor) => {
                self.filter_query = editor.text.clone();
                self.active_file = None;
                vec![request(
                    json!({"op": "filter", "query": self.filter_query, "done": false}),
                )]
            }
            _ => Vec::new(),
        }
    }

    /// Enter on the open line: run it and close it (a path waits for Yazi's answer).
    fn line_submit(&mut self, yz: &Snapshot, home: &str) -> Vec<Effect> {
        match &self.mode {
            Mode::Path(_) => return vec![self.submit_path(yz, home)],
            Mode::Rename { .. } => return self.submit_rename(),
            _ => {}
        }
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        match mode {
            Mode::Command { editor, block } if !editor.text.is_empty() => {
                vec![request(json!({"op": "command", "action": "shell",
                    "args": {"positional": [editor.text], "options": {"block": block}}}))]
            }
            Mode::Search { editor, content } if !editor.text.is_empty() => {
                let via = if content { "rg" } else { "fd" };
                vec![request(
                    json!({"op": "search", "query": editor.text, "via": via}),
                )]
            }
            Mode::Find { editor, previous } => {
                self.active_file = None;
                vec![request(
                    json!({"op": "find", "query": editor.text, "previous": previous}),
                )]
            }
            Mode::Filter(editor) => {
                self.filter_query = editor.text;
                self.active_file = None;
                vec![request(
                    json!({"op": "filter", "query": self.filter_query, "done": true}),
                )]
            }
            Mode::Create(editor) if !editor.text.is_empty() => {
                vec![request(json!({"op": "create", "name": editor.text}))]
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn key(&mut self, key: &Key, yz: &Snapshot, home: &str) -> Vec<Effect> {
        let name = key.name.as_str();
        let escape = name == "escape" || (key.ctrl && name == "[");
        let enter = name == "enter";
        let plain = !key.shift && !key.ctrl && !key.alt && !key.meta;
        let pending = self.pending.take();
        match &mut self.mode {
            Mode::Quit { no_cwd, .. } => {
                let no_cwd = *no_cwd;
                let cancel = escape || (key.ctrl && name == "c") || (plain && name == "n");
                let confirm = enter || (plain && name == "y");
                return if cancel || confirm {
                    self.quit_decision(no_cwd, confirm)
                } else {
                    Vec::new()
                };
            }
            Mode::Trash(_) => {
                return if escape || enter {
                    self.trash_decision(enter)
                } else {
                    Vec::new()
                };
            }
            Mode::Theme { at, original } => {
                let (at, original) = (*at, *original);
                if escape {
                    self.theme = original;
                    self.mode = Mode::Normal;
                } else if enter {
                    self.mode = Mode::Normal;
                    return vec![Effect::SaveTheme(at)];
                } else if matches!(name, "j" | "down" | "k" | "up") {
                    let down = matches!(name, "j" | "down");
                    let at = if down {
                        (at + 1).min(THEMES.len() - 1)
                    } else {
                        at.saturating_sub(1)
                    };
                    self.theme = at;
                    self.mode = Mode::Theme { at, original };
                }
                return Vec::new();
            }
            _ if self.path_menu => {
                if escape {
                    self.path_menu = false;
                }
                return Vec::new();
            }
            Mode::Path(_)
            | Mode::Command { .. }
            | Mode::Search { .. }
            | Mode::Find { .. }
            | Mode::Filter(_)
            | Mode::Create(_)
            | Mode::Rename { .. } => return self.line_key(key, yz, home),
            Mode::Tasks => {
                let effect = if escape || name == "w" || (key.ctrl && name == "c") {
                    self.mode = Mode::Normal;
                    tasks("close", None)
                } else if enter {
                    tasks("inspect", None)
                } else {
                    match name {
                        "k" | "up" => tasks("arrow", Some("prev")),
                        "j" | "down" => tasks("arrow", Some("next")),
                        "x" => tasks("cancel", None),
                        _ => return Vec::new(),
                    }
                };
                return vec![effect];
            }
            Mode::Normal => {}
        }
        self.normal_key(key, yz, pending, escape, enter, plain)
    }

    fn normal_key(
        &mut self,
        key: &Key,
        yz: &Snapshot,
        pending: Option<Pending>,
        escape: bool,
        enter: bool,
        plain: bool,
    ) -> Vec<Effect> {
        let name = key.name.as_str();
        if key.alt || key.meta {
            return Vec::new();
        }
        if escape {
            return vec![action("escape", &[], json!({}))];
        }
        let minus = name == "-" || name == "minus";
        if key.ctrl {
            match name {
                "a" => return vec![action("toggle_all", &[], json!({"state": "on"}))],
                "r" => return vec![action("toggle_all", &[], json!({}))],
                // Yazi's `<C-c>` is `close`: a tab, or on the last tab the same quit as `q`.
                "c" if yz.tabs.len() <= 1 => return vec![self.quit(false)],
                "c" => return vec![action("close", &[], json!({}))],
                "z" => return vec![Effect::Suspend],
                "s" => return vec![action("escape", &[], json!({"search": true}))],
                _ if minus && !key.shift => return vec![action("hardlink", &[], json!({}))],
                "d" | "u" | "f" | "b" => {}
                _ => return Vec::new(),
            }
        }
        let upper = key.shift || name.chars().any(char::is_uppercase);
        let page_down = name == "page_down" || (key.ctrl && matches!(name, "d" | "f"));
        let page_up = name == "page_up" || (key.ctrl && matches!(name, "u" | "b"));
        if page_down || page_up {
            let half = (key.ctrl && matches!(name, "d" | "u")) || (!key.ctrl && key.shift);
            let step = format!(
                "{}{}",
                if page_up { "-" } else { "" },
                if half { "50%" } else { "100%" }
            );
            return vec![action("arrow", &[&step], json!({}))];
        }
        if key.ctrl {
            return Vec::new();
        }
        if name == "end" || (name == "g" && upper) {
            return vec![action("arrow", &["bot"], json!({}))];
        }
        if name == "home" {
            return vec![action("arrow", &["top"], json!({}))];
        }
        if name == "g" && plain {
            if pending == Some(Pending::G) {
                return vec![action("arrow", &["top"], json!({}))];
            }
            self.pending = Some(Pending::G);
            return Vec::new();
        }
        if name == "t" && plain {
            if pending == Some(Pending::T) {
                return vec![action("tab_create", &[], json!({"current": true}))];
            }
            self.pending = Some(Pending::T);
            return Vec::new();
        }
        if name == "n" && pending != Some(Pending::C) {
            return vec![action("find_arrow", &[], json!({"previous": upper}))];
        }
        if plain && pending == Some(Pending::C) && matches!(name, "c" | "d" | "f" | "n") {
            return copy_paths(name, yz).map(Effect::Copy).into_iter().collect();
        }
        if name == "c" && plain {
            self.pending = Some(Pending::C);
            return Vec::new();
        }
        let letter = name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic();
        if upper && letter {
            return match name {
                "v" => vec![action("visual_mode", &[], json!({"unset": true}))],
                "h" => vec![action("back", &[], json!({}))],
                "l" => vec![action("forward", &[], json!({}))],
                "p" => vec![action("paste", &[], json!({"force": true}))],
                "y" | "x" => vec![action("unyank", &[], json!({}))],
                "j" => vec![action("seek", &["5"], json!({}))],
                "k" => vec![action("seek", &["-5"], json!({}))],
                "z" => vec![action("plugin", &["zoxide"], json!({}))],
                "q" => vec![self.quit(true)],
                "s" => {
                    self.mode = Mode::Search {
                        editor: Editor::default(),
                        content: true,
                    };
                    Vec::new()
                }
                // Yazi's default keymap leaves `W` unbound; it opens a terminal below.
                "w" => vec![Effect::Terminal(self.cwd(yz).to_owned())],
                // Yazi's default keymap leaves `T` unbound; it opens the theme picker.
                "t" => {
                    self.mode = Mode::Theme {
                        at: self.theme,
                        original: self.theme,
                    };
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }
        let effect = match name {
            "j" | "down" => action("arrow", &["1"], json!({})),
            "k" | "up" => action("arrow", &["-1"], json!({})),
            "h" | "left" => action("leave", &[], json!({})),
            "l" | "right" => action("enter", &[], json!({})),
            // Enter on a directory enters it; otherwise Enter and `o` open the selection, or
            // the hovered entry, in Tern.
            _ if enter || name == "o" => match &yz.hovered {
                // With --chooser-file Yazi's `open` writes the choice and exits; a directory
                // without a selection still enters on Enter.
                _ if self.chooser
                    && !(enter
                        && yz.selected_urls.is_empty()
                        && yz
                            .hovered
                            .as_ref()
                            .is_some_and(|hover| hover.dir == Some(true))) =>
                {
                    action("open", &[], json!({}))
                }
                _ if !yz.selected_urls.is_empty() => Effect::Open(yz.selected_urls.clone()),
                Some(hover) if enter && hover.dir == Some(true) => action("enter", &[], json!({})),
                Some(hover) => Effect::Open(vec![hover.url.clone()]),
                None => return Vec::new(),
            },
            "space" | " " => request(json!({"op": "toggle_advance"})),
            "v" => action("visual_mode", &[], json!({})),
            "y" => action("yank", &[], json!({})),
            "x" => action("yank", &[], json!({"cut": true})),
            "p" => action("paste", &[], json!({})),
            "z" => action("plugin", &["fzf"], json!({})),
            _ if minus && !upper => action("link", &[], json!({})),
            "_" | "underscore" => action("link", &[], json!({"relative": true})),
            _ if minus => action("link", &[], json!({"relative": true})),
            "d" => Effect::Request(json!({"op": "trash_prepare"}), Then::TrashPrepared),
            "f" => {
                let query = self.filter_query.clone();
                let end = utf16_len(&query);
                self.mode = Mode::Filter(Editor::new(query, end, end));
                return Vec::new();
            }
            "/" | "?" => {
                self.mode = Mode::Find {
                    editor: Editor::default(),
                    previous: name == "?" || key.shift,
                };
                return Vec::new();
            }
            "w" => {
                self.mode = Mode::Tasks;
                tasks("show", None)
            }
            "s" => {
                self.mode = Mode::Search {
                    editor: Editor::default(),
                    content: false,
                };
                return Vec::new();
            }
            ":" | "colon" | ";" | "semicolon" => {
                self.mode = Mode::Command {
                    editor: Editor::default(),
                    block: key.shift || matches!(name, ":" | "colon"),
                };
                return Vec::new();
            }
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" if !upper => {
                let index = (name.as_bytes()[0] - b'1').to_string();
                action("tab_switch", &[&index], json!({}))
            }
            "{" | "braceleft" => action("tab_swap", &["-1"], json!({})),
            "}" | "braceright" => action("tab_swap", &["1"], json!({})),
            "[" | "bracketleft" if upper => action("tab_swap", &["-1"], json!({})),
            "]" | "bracketright" if upper => action("tab_swap", &["1"], json!({})),
            "[" | "bracketleft" => action("tab_switch", &["-1"], json!({"relative": true})),
            "]" | "bracketright" => action("tab_switch", &["1"], json!({"relative": true})),
            "a" => {
                self.mode = Mode::Create(Editor::default());
                return Vec::new();
            }
            "r" => {
                if let Some(hover) = &yz.hovered {
                    let name = basename(&hover.url).to_owned();
                    // The caret lands before a file's extension, like Yazi's own rename.
                    let stem = match name.rfind('.') {
                        Some(dot) if dot > 0 && hover.dir != Some(true) => dot,
                        _ => name.len(),
                    };
                    let caret = utf16_len(&name[..stem]);
                    self.mode = Mode::Rename {
                        editor: Editor::new(name, caret, caret),
                        target: hover.url.clone(),
                    };
                }
                return Vec::new();
            }
            "." | "period" => action("hidden", &[], json!({"state": "toggle"})),
            "q" => self.quit(false),
            _ => return Vec::new(),
        };
        vec![effect]
    }

    fn submit_rename(&mut self) -> Vec<Effect> {
        let Mode::Rename { editor, target } = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            return Vec::new();
        };
        if editor.text.is_empty() {
            return Vec::new();
        }
        vec![request(
            json!({"op": "rename", "target": target, "new_name": editor.text}),
        )]
    }

    fn editor(&mut self, field: Field) -> Option<&mut Editor> {
        let open = match self.mode {
            Mode::Path(_) => Field::Path,
            Mode::Rename { .. } => Field::Rename,
            _ => Field::Line,
        };
        if open == field {
            self.line_editor()
        } else {
            None
        }
    }

    pub(crate) fn msg(&mut self, msg: Msg, yz: &Snapshot, home: &str) -> Vec<Effect> {
        if matches!(self.mode, Mode::Quit { .. }) {
            let no_cwd = matches!(self.mode, Mode::Quit { no_cwd: true, .. });
            return match msg {
                Msg::QuitConfirm => self.quit_decision(no_cwd, true),
                Msg::QuitCancel => self.quit_decision(no_cwd, false),
                _ => Vec::new(),
            };
        }
        if matches!(self.mode, Mode::Trash(_)) {
            return match msg {
                Msg::TrashConfirm => self.trash_decision(true),
                Msg::TrashCancel => self.trash_decision(false),
                _ => Vec::new(),
            };
        }
        self.pending = None;
        match msg {
            // Select-all belongs only to entering the editor; after a click, keys append.
            Msg::FieldFocus(field) => {
                if let Some(editor) = self.editor(field) {
                    editor.focus();
                }
            }
            Msg::FieldEdit(field, edit) => {
                if let Some(editor) = self.editor(field) {
                    editor.edit(&edit);
                    return self.line_changed();
                }
            }
            Msg::FieldSend(field, text) => {
                let Some(editor) = self.editor(field) else {
                    return Vec::new();
                };
                editor.text = text;
                return self.line_submit(yz, home);
            }
            Msg::PathEdit => {
                let cwd = yz.current.cwd.clone();
                let end = utf16_len(&cwd);
                self.mode = Mode::Path(Editor::new(cwd, end, 0));
                self.path_menu = false;
            }
            Msg::PathCancel => {
                if matches!(self.mode, Mode::Path(_)) {
                    self.mode = Mode::Normal;
                }
                self.path_menu = false;
            }
            Msg::PathMore => self.path_menu = !self.path_menu,
            Msg::PathSubmit if matches!(self.mode, Mode::Path(_)) => {
                return vec![self.submit_path(yz, home)];
            }
            Msg::PathCd(target) => return vec![self.jump(target)],
            Msg::ClearInput => match &self.mode {
                Mode::Find { .. } => return vec![self.clear_find()],
                Mode::Filter(_) => return vec![self.clear_filter()],
                Mode::Command { .. }
                | Mode::Search { .. }
                | Mode::Create(_)
                | Mode::Rename { .. } => self.mode = Mode::Normal,
                Mode::Tasks => {
                    self.mode = Mode::Normal;
                    return vec![tasks("close", None)];
                }
                _ => {}
            },
            Msg::TabSwitch(index) => {
                return vec![action("tab_switch", &[&index.to_string()], json!({}))];
            }
            Msg::Select(List::Themes, id) => {
                if let (Mode::Theme { original, .. }, Some(at)) = (&self.mode, theme::index(&id)) {
                    let original = *original;
                    self.theme = at;
                    self.mode = Mode::Theme { at, original };
                }
            }
            Msg::Activate(List::Themes, id) => {
                if let Some(at) = theme::index(&id) {
                    self.theme = at;
                    self.mode = Mode::Normal;
                    return vec![Effect::SaveTheme(at)];
                }
            }
            Msg::Select(list, name) => return self.list_event(list, &name, false, yz),
            Msg::Activate(list, name) => return self.list_event(list, &name, true, yz),
            Msg::PathSubmit
            | Msg::TrashConfirm
            | Msg::TrashCancel
            | Msg::QuitConfirm
            | Msg::QuitCancel => {}
        }
        Vec::new()
    }

    fn list_event(&mut self, list: List, name: &str, activate: bool, yz: &Snapshot) -> Vec<Effect> {
        let cwd = self.cwd(yz).to_owned();
        let base = match list {
            List::Parent => parent_dir(&cwd).map(str::to_owned),
            List::Files => Some(cwd.clone()),
            List::Child => self
                .active_file
                .clone()
                .or_else(|| {
                    yz.hovered
                        .as_ref()
                        .map(|hover| relative(&cwd, &hover.url).to_owned())
                })
                .map(|active| join_path(&cwd, &active)),
            List::Themes => None,
        };
        let Some(base) = base else {
            return Vec::new();
        };
        let Some(folder) = yz.directory(&base) else {
            return Vec::new();
        };
        if !folder.files.iter().any(|file| file == name) {
            return Vec::new();
        }
        let target = join_path(&base, name);
        let is_dir = yz.is_dir(&base, name);
        if activate {
            return match is_dir {
                Some(true) => {
                    self.optimistic_cwd = Some(target.clone());
                    self.active_file = None;
                    vec![request(
                        json!({"op": "activate", "target": target, "action": "enter"}),
                    )]
                }
                // With --chooser-file a double-click on a file chooses it through Yazi.
                _ if self.chooser => vec![
                    action("reveal", &[&target], json!({"no_dummy": true})),
                    action("open", &[], json!({})),
                ],
                _ => vec![Effect::Open(vec![target])],
            };
        }
        if base != yz.current.cwd {
            self.optimistic_cwd = Some(base);
        }
        self.active_file = Some(name.to_owned());
        vec![action("reveal", &[&target], json!({"no_dummy": true}))]
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{Folder, Hover, Tab};
    use super::*;

    fn key(name: &str) -> Key {
        Key {
            text: (name.chars().count() == 1).then(|| name.to_owned()),
            ..Key::named(name)
        }
    }

    fn shifted(name: &str) -> Key {
        Key {
            shift: true,
            text: Some(name.to_uppercase()),
            ..Key::named(name)
        }
    }

    fn snapshot() -> Snapshot {
        let mut current = Folder {
            cwd: "/tmp/files".to_owned(),
            files: vec!["child".to_owned(), "notes.md".to_owned()],
            ..Folder::default()
        };
        current.dirs.insert("child".to_owned(), true);
        current.dirs.insert("notes.md".to_owned(), false);
        Snapshot {
            seq: 1,
            current,
            hovered: Some(Hover {
                url: "/tmp/files/notes.md".to_owned(),
                dir: Some(false),
                ..Hover::default()
            }),
            ..Snapshot::default()
        }
    }

    fn body(effects: &[Effect]) -> &Value {
        match effects {
            [Effect::Request(body, _)] => body,
            other => panic!("expected one request, got {other:?}"),
        }
    }

    #[test]
    fn rename_starts_before_the_extension_and_submits_the_target() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        assert!(ui.key(&key("r"), &yz, "/home/u").is_empty());
        let Mode::Rename { editor, .. } = &ui.mode else {
            panic!("rename mode expected");
        };
        assert_eq!(editor.cursor, 5);
        ui.key(&key("2"), &yz, "/home/u");
        let effects = ui.key(&key("enter"), &yz, "/home/u");
        assert_eq!(
            body(&effects),
            &json!({"op": "rename", "target": "/tmp/files/notes.md", "new_name": "notes2.md"})
        );
        assert_eq!(ui.mode, Mode::Normal);
    }

    #[test]
    fn native_field_events_edit_and_submit() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        ui.key(&key("r"), &yz, "/home/u");
        ui.msg(Msg::FieldFocus(Field::Rename), &yz, "/home/u");
        let edit = |from, to, text: &str, len| FieldEdit {
            from,
            to,
            text: text.to_owned(),
            cursor: from + 1,
            len,
        };
        // An edit against text the editor no longer holds is stale and ignored.
        ui.msg(
            Msg::FieldEdit(Field::Rename, edit(0, 0, "x", 3)),
            &yz,
            "/home/u",
        );
        ui.msg(
            Msg::FieldEdit(Field::Rename, edit(5, 5, "2", 8)),
            &yz,
            "/home/u",
        );
        let Mode::Rename { editor, .. } = &ui.mode else {
            panic!("rename mode expected");
        };
        assert_eq!(
            (editor.text.as_str(), editor.cursor, editor.anchor),
            ("notes2.md", 6, 6)
        );
        // A path field event cannot touch the rename editor.
        assert!(ui
            .msg(Msg::FieldSend(Field::Path, "/".to_owned()), &yz, "/home/u")
            .is_empty());
        let effects = ui.msg(
            Msg::FieldSend(Field::Rename, "final.md".to_owned()),
            &yz,
            "/home/u",
        );
        assert_eq!(
            body(&effects),
            &json!({"op": "rename", "target": "/tmp/files/notes.md", "new_name": "final.md"})
        );
        assert_eq!(ui.mode, Mode::Normal);
    }

    #[test]
    fn filter_and_find_send_each_change() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        ui.key(&key("f"), &yz, "");
        let effects = ui.key(&key("n"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "filter", "query": "n", "done": false})
        );
        let effects = ui.key(&key("enter"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "filter", "query": "n", "done": true})
        );
        assert_eq!(ui.filter_query, "n");
        ui.key(&key("?"), &yz, "");
        let effects = ui.key(&key("c"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "find", "query": "c", "previous": true})
        );
        // The first Esc only leaves insert mode; the second cancels the find.
        assert!(ui.key(&key("escape"), &yz, "").is_empty());
        assert!(matches!(ui.mode, Mode::Find { .. }));
        let effects = ui.key(&key("escape"), &yz, "");
        assert_eq!(body(&effects)["args"]["options"], json!({"find": true}));
        assert_eq!(ui.mode, Mode::Normal);
    }

    /// Opens a line mode with `opener` and types `text` into it.
    fn typed_line(ui: &mut Ui, yz: &Snapshot, opener: &Key, text: &str) {
        ui.key(opener, yz, "/home/u");
        for c in text.chars() {
            ui.key(&key(&c.to_string()), yz, "/home/u");
        }
    }

    #[test]
    fn every_line_mode_cancels_on_the_second_escape() {
        let yz = snapshot();
        for opener in [
            key(":"),
            key(";"),
            key("s"),
            shifted("s"),
            key("a"),
            key("r"),
            key("f"),
            key("/"),
            shifted("?"),
        ] {
            let mut ui = Ui::new(0);
            typed_line(&mut ui, &yz, &opener, "ab");
            assert_ne!(ui.mode, Mode::Normal, "{opener:?} opens a line");
            ui.key(&key("escape"), &yz, "/home/u");
            let editor = ui.line_editor().expect("still a line after one Esc");
            assert_eq!(editor.vi_label(), Some("NORMAL"), "{opener:?}");
            ui.key(&key("escape"), &yz, "/home/u");
            assert_eq!(ui.mode, Mode::Normal, "{opener:?} cancels on Esc Esc");
        }
        let mut ui = Ui::new(0);
        ui.msg(Msg::PathEdit, &yz, "/home/u");
        ui.key(&key("escape"), &yz, "/home/u");
        assert!(matches!(ui.mode, Mode::Path(_)));
        ui.key(&key("escape"), &yz, "/home/u");
        assert_eq!(ui.mode, Mode::Normal);
    }

    #[test]
    fn live_modes_send_every_normal_mode_change() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        typed_line(&mut ui, &yz, &key("f"), "abc");
        assert!(ui.key(&key("escape"), &yz, "").is_empty());
        // `x` removes the character under the caret (the last one after Esc).
        let effects = ui.key(&key("x"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "filter", "query": "ab", "done": false})
        );
        assert_eq!(ui.filter_query, "ab");
        // Motions change nothing, so they send nothing.
        assert!(ui.key(&key("0"), &yz, "").is_empty());
        let effects = ui.key(&key("x"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "filter", "query": "b", "done": false})
        );
        // `dd` empties the line; Enter submits it from normal mode.
        assert!(ui.key(&key("d"), &yz, "").is_empty());
        let effects = ui.key(&key("d"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "filter", "query": "", "done": false})
        );
        let effects = ui.key(&key("enter"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "filter", "query": "", "done": true})
        );
        assert_eq!(ui.mode, Mode::Normal);

        // Esc Esc clears the filter, exactly as a single Esc did before.
        typed_line(&mut ui, &yz, &key("f"), "q");
        ui.key(&key("escape"), &yz, "");
        let effects = ui.key(&key("escape"), &yz, "");
        assert_eq!(body(&effects), &json!({"op": "filter", "query": ""}));
        assert_eq!((ui.filter_query.as_str(), &ui.mode), ("", &Mode::Normal));

        typed_line(&mut ui, &yz, &key("/"), "cat");
        ui.key(&key("escape"), &yz, "");
        ui.key(&key("b"), &yz, "");
        let effects = ui.key(&key("D"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "find", "query": "", "previous": false})
        );
        let effects = ui.key(&key("enter"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "find", "query": "", "previous": false})
        );
    }

    #[test]
    fn enter_submits_from_normal_mode() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        typed_line(&mut ui, &yz, &key(":"), "ls");
        ui.key(&key("escape"), &yz, "");
        let effects = ui.key(&key("enter"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "command", "action": "shell",
                "args": {"positional": ["ls"], "options": {"block": true}}})
        );
        assert_eq!(ui.mode, Mode::Normal);

        // Normal-mode edits land in what is submitted.
        typed_line(&mut ui, &yz, &key("a"), "xb.txt");
        ui.key(&key("escape"), &yz, "");
        ui.key(&key("0"), &yz, "");
        ui.key(&key("x"), &yz, "");
        let effects = ui.key(&key("enter"), &yz, "");
        assert_eq!(body(&effects), &json!({"op": "create", "name": "b.txt"}));

        typed_line(&mut ui, &yz, &shifted("s"), "todo");
        ui.key(&key("escape"), &yz, "");
        ui.key(&key("c"), &yz, "");
        ui.key(&key("c"), &yz, "");
        for c in "fixme".chars() {
            ui.key(&key(&c.to_string()), &yz, "");
        }
        let effects = ui.key(&key("enter"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "search", "query": "fixme", "via": "rg"})
        );
    }

    #[test]
    fn native_line_events_reach_live_modes() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        ui.key(&key("/"), &yz, "");
        ui.msg(Msg::FieldFocus(Field::Line), &yz, "");
        let edit = FieldEdit {
            from: 0,
            to: 0,
            text: "n".to_owned(),
            cursor: 1,
            len: 0,
        };
        // Rename's native field cannot touch a find line.
        assert!(ui
            .msg(Msg::FieldEdit(Field::Rename, edit.clone()), &yz, "")
            .is_empty());
        let effects = ui.msg(Msg::FieldEdit(Field::Line, edit), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "find", "query": "n", "previous": false})
        );
        let effects = ui.msg(Msg::FieldSend(Field::Line, "no".to_owned()), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "find", "query": "no", "previous": false})
        );
        assert_eq!(ui.mode, Mode::Normal);
    }

    #[test]
    fn normal_keys_map_to_actor_actions() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        let arrow = ui.key(&key("j"), &yz, "");
        assert_eq!(
            body(&arrow),
            &json!({"op": "action", "action": "arrow", "args": {"positional": ["1"], "options": {}}})
        );
        assert!(ui.key(&key("g"), &yz, "").is_empty());
        assert_eq!(
            body(&ui.key(&key("g"), &yz, ""))["args"]["positional"],
            json!(["top"])
        );
        assert_eq!(
            body(&ui.key(&shifted("g"), &yz, ""))["args"]["positional"],
            json!(["bot"])
        );
        let half = ui.key(
            &Key {
                ctrl: true,
                ..Key::named("u")
            },
            &yz,
            "",
        );
        assert_eq!(body(&half)["args"]["positional"], json!(["-50%"]));
        assert_eq!(
            body(&ui.key(&key("3"), &yz, ""))["args"]["positional"],
            json!(["2"])
        );
        // `q` goes through Yazi (its task check), not straight to a stop.
        assert_eq!(
            ui.key(&key("q"), &yz, ""),
            vec![Effect::Request(
                json!({"op": "quit", "no_cwd_file": false}),
                Then::Quit { no_cwd: false }
            )]
        );
        let suspend = Key {
            ctrl: true,
            ..Key::named("z")
        };
        assert_eq!(ui.key(&suspend, &yz, ""), vec![Effect::Suspend]);
    }

    fn quit_reply(tasks: u64, quitting: bool) -> Reply {
        Reply {
            ok: true,
            tasks,
            titles: vec!["Copy a".to_owned()],
            quitting,
            ..Reply::default()
        }
    }

    #[test]
    fn quit_keys_carry_the_cwd_file_flag() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        let upper = ui.key(&shifted("q"), &yz, "");
        assert_eq!(body(&upper), &json!({"op": "quit", "no_cwd_file": true}));
        let ctrl_c = ui.key(
            &Key {
                ctrl: true,
                ..Key::named("c")
            },
            &yz,
            "",
        );
        assert_eq!(body(&ctrl_c), &json!({"op": "quit", "no_cwd_file": false}));
        // With several tabs Ctrl+C closes the active tab, as Yazi's `close` does.
        let mut tabbed = snapshot();
        tabbed.tabs = vec![Tab::default(), Tab::default()];
        let ctrl_c = ui.key(
            &Key {
                ctrl: true,
                ..Key::named("c")
            },
            &tabbed,
            "",
        );
        assert_eq!(body(&ctrl_c)["action"], "close");
    }

    #[test]
    fn quit_without_tasks_waits_for_yazi_to_exit() {
        let mut ui = Ui::new(0);
        let effects = ui.reply(Then::Quit { no_cwd: false }, &quit_reply(0, true));
        assert_eq!(effects, vec![Effect::Quitting]);
        assert_eq!(ui.mode, Mode::Normal);
    }

    #[test]
    fn quit_with_tasks_confirms_cancels_and_forwards_the_flag() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        let effects = ui.reply(Then::Quit { no_cwd: true }, &quit_reply(2, false));
        assert!(effects.is_empty());
        assert_eq!(
            ui.mode,
            Mode::Quit {
                no_cwd: true,
                count: 2,
                titles: vec!["Copy a".to_owned()]
            }
        );
        // Other keys change nothing; Esc, `n` and Ctrl+C cancel without a request.
        assert!(ui.key(&key("j"), &yz, "").is_empty());
        assert!(matches!(ui.mode, Mode::Quit { .. }));
        for cancel in [
            key("escape"),
            key("n"),
            Key {
                ctrl: true,
                ..Key::named("c")
            },
        ] {
            ui.mode = Mode::Quit {
                no_cwd: true,
                count: 2,
                titles: vec![],
            };
            assert!(ui.key(&cancel, &yz, "").is_empty());
            assert_eq!(ui.mode, Mode::Normal);
        }
        // Enter and `y` confirm: the same request, confirmed, with the flag kept.
        for confirm in [key("enter"), key("y")] {
            ui.mode = Mode::Quit {
                no_cwd: true,
                count: 2,
                titles: vec![],
            };
            let effects = ui.key(&confirm, &yz, "");
            assert_eq!(
                body(&effects),
                &json!({"op": "quit", "no_cwd_file": true, "confirmed": true})
            );
            assert_eq!(ui.mode, Mode::Normal);
        }
        // Clicking the layer's buttons decides the same way.
        ui.mode = Mode::Quit {
            no_cwd: false,
            count: 1,
            titles: vec![],
        };
        assert!(ui.msg(Msg::QuitCancel, &yz, "").is_empty());
        assert_eq!(ui.mode, Mode::Normal);
        ui.mode = Mode::Quit {
            no_cwd: false,
            count: 1,
            titles: vec![],
        };
        let effects = ui.msg(Msg::QuitConfirm, &yz, "");
        assert_eq!(body(&effects)["confirmed"], true);
        // A confirmed quit is acknowledged as quitting.
        let effects = ui.reply(Then::Quit { no_cwd: false }, &quit_reply(0, true));
        assert_eq!(effects, vec![Effect::Quitting]);
    }

    #[test]
    fn chooser_enter_and_o_open_through_yazi_except_to_enter_directories() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        // Without --chooser-file a file opens in Tern.
        assert!(matches!(
            ui.key(&key("enter"), &yz, "")[..],
            [Effect::Open(_)]
        ));
        ui.chooser = true;
        for name in ["enter", "o"] {
            let effects = ui.key(&key(name), &yz, "");
            assert_eq!(
                body(&effects),
                &json!({"op": "action", "action": "open", "args": {"positional": [], "options": {}}})
            );
        }
        // A selection is chosen as a whole, even from a directory.
        let mut dir = snapshot();
        dir.hovered = Some(Hover {
            url: "/tmp/files/child".to_owned(),
            dir: Some(true),
            ..Hover::default()
        });
        assert_eq!(body(&ui.key(&key("enter"), &dir, ""))["action"], "enter");
        assert_eq!(body(&ui.key(&key("o"), &dir, ""))["action"], "open");
        dir.selected_urls = vec!["/tmp/files/notes.md".to_owned()];
        assert_eq!(body(&ui.key(&key("enter"), &dir, ""))["action"], "open");
    }

    #[test]
    fn colon_queues_a_blocking_shell_and_semicolon_a_background_one() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        for (opener, block) in [(key(":"), true), (shifted(";"), true), (key(";"), false)] {
            assert!(ui.key(&opener, &yz, "").is_empty());
            assert_eq!(
                ui.mode,
                Mode::Command {
                    editor: Editor::default(),
                    block
                }
            );
            ui.key(&key("l"), &yz, "");
            ui.key(&key("s"), &yz, "");
            let effects = ui.key(&key("enter"), &yz, "");
            assert_eq!(
                body(&effects),
                &json!({"op": "command", "action": "shell",
                    "args": {"positional": ["ls"], "options": {"block": block}}})
            );
            assert_eq!(ui.mode, Mode::Normal);
        }
    }

    #[test]
    fn open_follows_the_selection_then_the_hovered_entry() {
        let mut yz = snapshot();
        let mut ui = Ui::new(0);
        let note = vec!["/tmp/files/notes.md".to_owned()];
        assert_eq!(ui.key(&key("o"), &yz, ""), vec![Effect::Open(note.clone())]);
        assert_eq!(ui.key(&key("enter"), &yz, ""), vec![Effect::Open(note)]);
        assert!(ui.key(&shifted("o"), &yz, "").is_empty());
        yz.hovered = Some(Hover {
            url: "/tmp/files/child".to_owned(),
            dir: Some(true),
            ..Hover::default()
        });
        assert_eq!(body(&ui.key(&key("enter"), &yz, ""))["action"], "enter");
        assert_eq!(
            ui.key(&key("o"), &yz, ""),
            vec![Effect::Open(vec!["/tmp/files/child".to_owned()])]
        );
        yz.selected_urls = vec![
            "/tmp/files/child".to_owned(),
            "/tmp/files/notes.md".to_owned(),
        ];
        assert_eq!(
            ui.key(&key("enter"), &yz, ""),
            vec![Effect::Open(yz.selected_urls.clone())]
        );
        yz.hovered = None;
        yz.selected_urls.clear();
        assert!(ui.key(&key("o"), &yz, "").is_empty());
    }

    #[test]
    fn copy_prefix_copies_like_yazi() {
        let mut yz = snapshot();
        let mut ui = Ui::new(0);
        let mut copy = |second: &str, yz: &Snapshot| {
            assert!(ui.key(&key("c"), yz, "").is_empty());
            ui.key(&key(second), yz, "")
        };
        assert_eq!(
            copy("c", &yz),
            vec![Effect::Copy("/tmp/files/notes.md".to_owned())]
        );
        assert_eq!(copy("d", &yz), vec![Effect::Copy("/tmp/files".to_owned())]);
        assert_eq!(copy("f", &yz), vec![Effect::Copy("notes.md".to_owned())]);
        assert_eq!(copy("n", &yz), vec![Effect::Copy("notes".to_owned())]);
        yz.selected_urls = vec![
            "/tmp/files/.env".to_owned(),
            "/tmp/files/a.tar.gz".to_owned(),
        ];
        assert_eq!(copy("n", &yz), vec![Effect::Copy(".env\na.tar".to_owned())]);
        // Any other key ends the prefix and acts on its own.
        assert_eq!(body(&copy("j", &yz))["action"], "arrow");
        assert_eq!(body(&ui.key(&key("n"), &yz, ""))["action"], "find_arrow");
    }

    #[test]
    fn shift_w_opens_a_terminal_in_the_shown_directory() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        assert_eq!(
            ui.key(&shifted("w"), &yz, ""),
            vec![Effect::Terminal("/tmp/files".to_owned())]
        );
        ui.optimistic_cwd = Some("/tmp/files/child".to_owned());
        assert_eq!(
            ui.key(&shifted("w"), &yz, ""),
            vec![Effect::Terminal("/tmp/files/child".to_owned())]
        );
    }

    #[test]
    fn trash_needs_prepared_paths_and_confirmation() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        let effects = ui.key(&key("d"), &yz, "");
        assert!(matches!(
            &effects[..],
            [Effect::Request(_, Then::TrashPrepared)]
        ));
        let reply = Reply {
            ok: true,
            paths: vec!["/tmp/files/notes.md".to_owned()],
            ..Reply::default()
        };
        assert!(ui.reply(Then::TrashPrepared, &reply).is_empty());
        assert!(ui.key(&key("j"), &yz, "").is_empty());
        let effects = ui.key(&key("enter"), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "trash_commit", "paths": ["/tmp/files/notes.md"]})
        );
        let empty = Reply {
            ok: true,
            ..Reply::default()
        };
        let notice = ui.reply(Then::TrashPrepared, &empty);
        assert!(matches!(&notice[..], [Effect::Notice(Level::Info, _)]));
        assert_eq!(ui.mode, Mode::Normal);
    }

    #[test]
    fn theme_picker_previews_and_restores() {
        let yz = snapshot();
        let mut ui = Ui::new(2);
        ui.key(&shifted("t"), &yz, "");
        ui.key(&key("j"), &yz, "");
        assert_eq!(ui.theme, 3);
        ui.key(&key("escape"), &yz, "");
        assert_eq!((ui.theme, &ui.mode), (2, &Mode::Normal));
        ui.key(&shifted("t"), &yz, "");
        ui.key(&key("k"), &yz, "");
        assert_eq!(ui.key(&key("enter"), &yz, ""), vec![Effect::SaveTheme(1)]);
        let effects = ui.msg(Msg::Activate(List::Themes, "zen".to_owned()), &yz, "");
        assert_eq!(
            effects,
            vec![Effect::SaveTheme(theme::index("zen").unwrap())]
        );
    }

    #[test]
    fn path_editing_resolves_and_jumps() {
        assert_eq!(
            resolve_directory("~/x", "/tmp", "/home/u").as_deref(),
            Some("/home/u/x")
        );
        assert_eq!(
            resolve_directory("sub", "/tmp", "").as_deref(),
            Some("/tmp/sub")
        );
        assert_eq!(resolve_directory("a\nb", "/tmp", ""), None);
        let mut yz = snapshot();
        let mut ui = Ui::new(0);
        ui.msg(Msg::PathEdit, &yz, "");
        ui.key(&key("backspace"), &yz, "");
        ui.key(&key("/"), &yz, "");
        let effects = ui.key(&key("enter"), &yz, "");
        assert_eq!(body(&effects), &json!({"op": "cd", "target": "/"}));
        yz.current.cwd = "/".to_owned();
        ui.sync(&yz, false);
        assert_eq!(ui.mode, Mode::Normal);
    }

    #[test]
    fn list_clicks_reveal_files_and_enter_directories() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        let effects = ui.msg(Msg::Select(List::Files, "child".to_owned()), &yz, "");
        assert_eq!(
            body(&effects)["args"]["positional"],
            json!(["/tmp/files/child"])
        );
        assert_eq!(ui.active_file.as_deref(), Some("child"));
        let effects = ui.msg(Msg::Activate(List::Files, "child".to_owned()), &yz, "");
        assert_eq!(
            body(&effects),
            &json!({"op": "activate", "target": "/tmp/files/child", "action": "enter"})
        );
        assert_eq!(ui.optimistic_cwd.as_deref(), Some("/tmp/files/child"));
        assert!(ui
            .msg(Msg::Select(List::Files, "missing".to_owned()), &yz, "")
            .is_empty());
        ui.optimistic_cwd = None;
        let effects = ui.msg(Msg::Activate(List::Files, "notes.md".to_owned()), &yz, "");
        assert_eq!(
            effects,
            vec![Effect::Open(vec!["/tmp/files/notes.md".to_owned()])]
        );
    }

    #[test]
    fn snapshot_filter_wins_only_without_pending_edits() {
        let mut yz = snapshot();
        yz.filter = Some(Some("abc".to_owned()));
        let mut ui = Ui::new(0);
        ui.sync(&yz, true);
        assert_eq!(ui.filter_query, "");
        ui.sync(&yz, false);
        assert_eq!(ui.filter_query, "abc");
        yz.seq = 2;
        yz.filter = Some(None);
        ui.sync(&yz, false);
        assert_eq!(ui.filter_query, "");
    }
}
