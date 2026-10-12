//! The input reducer: keys and pointer messages become UI state changes plus effects (Yazi
//! requests, notices, theme persistence, quit). Pure: no I/O, no session.

use serde_json::{json, Map, Value};
use tern_sdk::keys::Key;

use super::model::{basename, join_path, parent_dir, Snapshot};
use super::theme::{self, THEMES};

/// A text field edited by keys, with UTF-16 caret offsets as Tern's input node expects.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Editor {
    pub(crate) text: String,
    pub(crate) cursor: usize,
    pub(crate) anchor: usize,
}

/// The fields the view draws as native inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Path,
    Rename,
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

fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The byte offset of UTF-16 offset `at`, clamped to the end.
fn byte_at(text: &str, at: usize) -> usize {
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units >= at {
            return byte;
        }
        units += c.len_utf16();
    }
    text.len()
}

impl Editor {
    pub(crate) fn new(text: String, cursor: usize, anchor: usize) -> Self {
        Self {
            text,
            cursor,
            anchor,
        }
    }

    /// Applies a native edit unless it was made against text this editor no longer holds.
    fn edit(&mut self, edit: &FieldEdit) {
        if edit.len == utf16_len(&self.text) {
            self.replace(&edit.text, Some((edit.from, edit.to)));
            self.cursor = edit.cursor;
            self.anchor = edit.cursor;
        }
    }

    /// Replaces UTF-16 `[from, to)` (default: the selection) and collapses the caret after it.
    pub(crate) fn replace(&mut self, insert: &str, range: Option<(usize, usize)>) {
        let (from, to) = range.unwrap_or((
            self.cursor.min(self.anchor),
            self.cursor.max(self.anchor),
        ));
        let (start, end) = (byte_at(&self.text, from), byte_at(&self.text, to.max(from)));
        self.text.replace_range(start..end, insert);
        self.cursor = utf16_len(&self.text[..start]) + utf16_len(insert);
        self.anchor = self.cursor;
    }

    fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = utf16_len(&self.text);
    }

    fn erase(&mut self, forward: bool) {
        if self.cursor != self.anchor {
            return self.replace("", None);
        }
        let at = byte_at(&self.text, self.cursor);
        let span = if forward {
            self.text[at..].chars().next().map(|c| (at, at + c.len_utf8()))
        } else {
            self.text[..at].chars().next_back().map(|c| (at - c.len_utf8(), at))
        };
        if let Some((start, end)) = span {
            let from = utf16_len(&self.text[..start]);
            self.replace("", Some((from, from + utf16_len(&self.text[start..end]))));
        }
    }

    fn step(&mut self, right: bool, extend: bool) {
        let at = byte_at(&self.text, self.cursor);
        let next = if right {
            self.text[at..].chars().next().map_or(at, |c| at + c.len_utf8())
        } else {
            self.text[..at].chars().next_back().map_or(at, |c| at - c.len_utf8())
        };
        self.cursor = utf16_len(&self.text[..next]);
        if !extend {
            self.anchor = self.cursor;
        }
    }

    fn edge(&mut self, end: bool, extend: bool) {
        self.cursor = if end { utf16_len(&self.text) } else { 0 };
        if !extend {
            self.anchor = self.cursor;
        }
    }

    /// Applies an editing key; false when the key is not an editing key.
    fn key(&mut self, key: &Key) -> bool {
        match key.name.as_str() {
            "a" if key.ctrl => self.select_all(),
            "backspace" => self.erase(false),
            "delete" => self.erase(true),
            "home" | "end" => self.edge(key.name == "end", key.shift),
            "left" | "right" => self.step(key.name == "right", key.shift),
            _ => match typed(key) {
                Some(text) => self.replace(&text, None),
                None => return false,
            },
        }
        true
    }
}

/// Removes the last character.
fn erase_last(text: &mut String) {
    text.pop();
}

/// The text a key types, if any.
fn typed(key: &Key) -> Option<String> {
    if key.name == "paste" {
        return key.text.clone();
    }
    if key.ctrl || key.meta || key.alt {
        return None;
    }
    if let Some(text) = key.text.as_ref().filter(|text| !text.is_empty()) {
        return Some(text.clone());
    }
    if key.name == "space" {
        return Some(" ".to_owned());
    }
    let mut chars = key.name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if key.shift => Some(c.to_uppercase().collect()),
        (Some(c), None) => Some(c.to_string()),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Mode {
    Normal,
    Path(Editor),
    Command(String),
    Find { query: String, previous: bool },
    Filter,
    Create(String),
    Rename { editor: Editor, target: String },
    /// Exact paths Yazi prepared for Trash, awaiting confirmation.
    Trash(Vec<String>),
    /// The theme picker; `original` is restored on cancel.
    Theme { at: usize, original: usize },
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
    ShellQueued(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Info,
    Error,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Effect {
    /// A Yazi request body (without its id) and its continuation.
    Request(Value, Then),
    Notice(Level, String),
    SaveTheme(usize),
    Quit,
}

/// Yazi's answer to one request.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Reply {
    pub(crate) ok: bool,
    pub(crate) rejected: bool,
    pub(crate) error: Option<String>,
    pub(crate) paths: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    G,
    T,
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
        if self.mode != Mode::Filter && !filter_pending && self.filter_seq != Some(yz.seq) {
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
            Then::ShellQueued(query) if reply.ok => {
                effects.push(Effect::Notice(
                    Level::Info,
                    "Shell queued in Yazi; completion is reported by Yazi tasks".to_owned(),
                ));
                if self.mode == Mode::Command(query) {
                    self.mode = Mode::Normal;
                }
            }
            Then::TrashPrepared | Then::TrashQueued | Then::ShellQueued(_) => {}
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

    fn clear_find(&mut self) -> Effect {
        self.mode = Mode::Normal;
        action("escape", &[], json!({"find": true}))
    }

    fn clear_filter(&mut self) -> Effect {
        self.mode = Mode::Normal;
        self.filter_query.clear();
        request(json!({"op": "filter", "query": ""}))
    }

    pub(crate) fn key(&mut self, key: &Key, yz: &Snapshot, home: &str) -> Vec<Effect> {
        let name = key.name.as_str();
        let escape = name == "escape" || (key.ctrl && name == "[");
        let enter = name == "enter";
        let plain = !key.shift && !key.ctrl && !key.alt && !key.meta;
        let pending = self.pending.take();
        match &mut self.mode {
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
                    let at = if down { (at + 1).min(THEMES.len() - 1) } else { at.saturating_sub(1) };
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
            Mode::Path(editor) => {
                if escape {
                    self.mode = Mode::Normal;
                } else if enter {
                    return vec![self.submit_path(yz, home)];
                } else {
                    editor.key(key);
                }
                return Vec::new();
            }
            Mode::Command(query) => {
                if escape {
                    self.mode = Mode::Normal;
                } else if enter {
                    let query = query.clone();
                    if query.is_empty() {
                        self.mode = Mode::Normal;
                        return Vec::new();
                    }
                    return vec![Effect::Request(
                        json!({"op": "command", "action": "shell",
                            "args": {"positional": [query], "options": {"block": false}}}),
                        Then::ShellQueued(query),
                    )];
                } else if name == "backspace" {
                    erase_last(query);
                } else if let Some(text) = typed(key) {
                    query.push_str(&text);
                }
                return Vec::new();
            }
            Mode::Find { query, previous } => {
                let previous = *previous;
                if escape {
                    return vec![self.clear_find()];
                }
                let query = if enter {
                    let query = std::mem::take(query);
                    self.mode = Mode::Normal;
                    query
                } else {
                    if name == "backspace" {
                        erase_last(query);
                    } else if let Some(text) = typed(key) {
                        query.push_str(&text);
                    } else {
                        return Vec::new();
                    }
                    query.clone()
                };
                self.active_file = None;
                return vec![request(json!({"op": "find", "query": query, "previous": previous}))];
            }
            Mode::Filter => {
                if escape {
                    return vec![self.clear_filter()];
                }
                if enter {
                    self.mode = Mode::Normal;
                    return vec![request(json!({"op": "filter", "query": self.filter_query, "done": true}))];
                }
                if name == "backspace" {
                    erase_last(&mut self.filter_query);
                } else if let Some(text) = typed(key) {
                    self.filter_query.push_str(&text);
                } else {
                    return Vec::new();
                }
                self.active_file = None;
                return vec![request(json!({"op": "filter", "query": self.filter_query, "done": false}))];
            }
            Mode::Create(query) => {
                if escape {
                    self.mode = Mode::Normal;
                } else if enter {
                    let name = std::mem::take(query);
                    self.mode = Mode::Normal;
                    if !name.is_empty() {
                        return vec![request(json!({"op": "create", "name": name}))];
                    }
                } else if name == "backspace" {
                    erase_last(query);
                } else if let Some(text) = typed(key) {
                    query.push_str(&text);
                }
                return Vec::new();
            }
            Mode::Rename { editor, .. } => {
                if escape {
                    self.mode = Mode::Normal;
                } else if enter {
                    return self.submit_rename();
                } else {
                    editor.key(key);
                }
                return Vec::new();
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
                "c" => return vec![action("close", &[], json!({}))],
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
            let step = format!("{}{}", if page_up { "-" } else { "" }, if half { "50%" } else { "100%" });
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
        if name == "n" {
            return vec![action("find_arrow", &[], json!({"previous": upper}))];
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
            // Opening files in Tern belongs to the window integration; directories enter.
            _ if enter => match &yz.hovered {
                Some(hover) if hover.dir == Some(true) && yz.selected_urls.is_empty() => {
                    action("enter", &[], json!({}))
                }
                _ => return Vec::new(),
            },
            "space" | " " => request(json!({"op": "toggle_advance"})),
            "v" => action("visual_mode", &[], json!({})),
            "y" => action("yank", &[], json!({})),
            "x" => action("yank", &[], json!({"cut": true})),
            "p" => action("paste", &[], json!({})),
            _ if minus && !upper => action("link", &[], json!({})),
            "_" | "underscore" => action("link", &[], json!({"relative": true})),
            _ if minus => action("link", &[], json!({"relative": true})),
            "d" => Effect::Request(json!({"op": "trash_prepare"}), Then::TrashPrepared),
            "f" => {
                self.mode = Mode::Filter;
                return Vec::new();
            }
            "/" | "?" => {
                self.mode = Mode::Find {
                    query: String::new(),
                    previous: name == "?" || key.shift,
                };
                return Vec::new();
            }
            ";" | "semicolon" if !key.shift => {
                self.mode = Mode::Command(String::new());
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
                self.mode = Mode::Create(String::new());
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
            "q" => Effect::Quit,
            _ => return Vec::new(),
        };
        vec![effect]
    }

    fn submit_rename(&mut self) -> Vec<Effect> {
        let Mode::Rename { editor, target } = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return Vec::new();
        };
        if editor.text.is_empty() {
            return Vec::new();
        }
        vec![request(json!({"op": "rename", "target": target, "new_name": editor.text}))]
    }

    fn editor(&mut self, field: Field) -> Option<&mut Editor> {
        match (&mut self.mode, field) {
            (Mode::Path(editor), Field::Path) | (Mode::Rename { editor, .. }, Field::Rename) => Some(editor),
            _ => None,
        }
    }

    pub(crate) fn msg(&mut self, msg: Msg, yz: &Snapshot, home: &str) -> Vec<Effect> {
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
                    editor.edge(true, false);
                }
            }
            Msg::FieldEdit(field, edit) => {
                if let Some(editor) = self.editor(field) {
                    editor.edit(&edit);
                }
            }
            Msg::FieldSend(field, text) => {
                let Some(editor) = self.editor(field) else {
                    return Vec::new();
                };
                editor.text = text;
                return match field {
                    Field::Path => vec![self.submit_path(yz, home)],
                    Field::Rename => self.submit_rename(),
                };
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
                Mode::Filter => return vec![self.clear_filter()],
                Mode::Command(_) | Mode::Create(_) | Mode::Rename { .. } => self.mode = Mode::Normal,
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
            Msg::PathSubmit | Msg::TrashConfirm | Msg::TrashCancel => {}
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
                .or_else(|| yz.hovered.as_ref().map(|hover| basename(&hover.url).to_owned()))
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
                    vec![request(json!({"op": "activate", "target": target, "action": "enter"}))]
                }
                // Opening files in Tern belongs to the window integration.
                _ => Vec::new(),
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
    use super::*;
    use super::super::model::{Folder, Hover};

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
    fn editor_edits_in_utf16_units() {
        let mut editor = Editor::new("a😀b".to_owned(), 3, 3);
        editor.key(&key("backspace"));
        assert_eq!((editor.text.as_str(), editor.cursor), ("ab", 1));
        editor.key(&key("x"));
        assert_eq!((editor.text.as_str(), editor.cursor), ("axb", 2));
        editor.key(&Key { shift: true, ..Key::named("home") });
        assert_eq!((editor.cursor, editor.anchor), (0, 2));
        editor.key(&key("é"));
        assert_eq!((editor.text.as_str(), editor.cursor), ("éb", 1));
        editor.key(&key("delete"));
        assert_eq!(editor.text, "é");
        editor.key(&Key { ctrl: true, ..Key::named("a") });
        editor.key(&key("backspace"));
        assert_eq!((editor.text.as_str(), editor.cursor, editor.anchor), ("", 0, 0));
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
        let edit = |from, to, text: &str, len| FieldEdit { from, to, text: text.to_owned(), cursor: from + 1, len };
        // An edit against text the editor no longer holds is stale and ignored.
        ui.msg(Msg::FieldEdit(Field::Rename, edit(0, 0, "x", 3)), &yz, "/home/u");
        ui.msg(Msg::FieldEdit(Field::Rename, edit(5, 5, "2", 8)), &yz, "/home/u");
        let Mode::Rename { editor, .. } = &ui.mode else {
            panic!("rename mode expected");
        };
        assert_eq!((editor.text.as_str(), editor.cursor, editor.anchor), ("notes2.md", 6, 6));
        // A path field event cannot touch the rename editor.
        assert!(ui.msg(Msg::FieldSend(Field::Path, "/".to_owned()), &yz, "/home/u").is_empty());
        let effects = ui.msg(Msg::FieldSend(Field::Rename, "final.md".to_owned()), &yz, "/home/u");
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
        assert_eq!(body(&effects), &json!({"op": "filter", "query": "n", "done": false}));
        let effects = ui.key(&key("enter"), &yz, "");
        assert_eq!(body(&effects), &json!({"op": "filter", "query": "n", "done": true}));
        assert_eq!(ui.filter_query, "n");
        ui.key(&key("?"), &yz, "");
        let effects = ui.key(&key("c"), &yz, "");
        assert_eq!(body(&effects), &json!({"op": "find", "query": "c", "previous": true}));
        let effects = ui.key(&key("escape"), &yz, "");
        assert_eq!(body(&effects)["args"]["options"], json!({"find": true}));
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
        assert_eq!(body(&ui.key(&key("g"), &yz, ""))["args"]["positional"], json!(["top"]));
        assert_eq!(body(&ui.key(&shifted("g"), &yz, ""))["args"]["positional"], json!(["bot"]));
        let half = ui.key(&Key { ctrl: true, ..Key::named("u") }, &yz, "");
        assert_eq!(body(&half)["args"]["positional"], json!(["-50%"]));
        assert_eq!(body(&ui.key(&key("3"), &yz, ""))["args"]["positional"], json!(["2"]));
        assert_eq!(ui.key(&key("q"), &yz, ""), vec![Effect::Quit]);
        assert!(ui.key(&key(":"), &yz, "").is_empty());
    }

    #[test]
    fn trash_needs_prepared_paths_and_confirmation() {
        let yz = snapshot();
        let mut ui = Ui::new(0);
        let effects = ui.key(&key("d"), &yz, "");
        assert!(matches!(&effects[..], [Effect::Request(_, Then::TrashPrepared)]));
        let reply = Reply {
            ok: true,
            paths: vec!["/tmp/files/notes.md".to_owned()],
            ..Reply::default()
        };
        assert!(ui.reply(Then::TrashPrepared, &reply).is_empty());
        assert!(ui.key(&key("j"), &yz, "").is_empty());
        let effects = ui.key(&key("enter"), &yz, "");
        assert_eq!(body(&effects), &json!({"op": "trash_commit", "paths": ["/tmp/files/notes.md"]}));
        let empty = Reply { ok: true, ..Reply::default() };
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
        assert_eq!(effects, vec![Effect::SaveTheme(theme::index("zen").unwrap())]);
    }

    #[test]
    fn path_editing_resolves_and_jumps() {
        assert_eq!(resolve_directory("~/x", "/tmp", "/home/u").as_deref(), Some("/home/u/x"));
        assert_eq!(resolve_directory("sub", "/tmp", "").as_deref(), Some("/tmp/sub"));
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
        assert_eq!(body(&effects)["args"]["positional"], json!(["/tmp/files/child"]));
        assert_eq!(ui.active_file.as_deref(), Some("child"));
        let effects = ui.msg(Msg::Activate(List::Files, "child".to_owned()), &yz, "");
        assert_eq!(body(&effects), &json!({"op": "activate", "target": "/tmp/files/child", "action": "enter"}));
        assert_eq!(ui.optimistic_cwd.as_deref(), Some("/tmp/files/child"));
        assert!(ui.msg(Msg::Select(List::Files, "missing".to_owned()), &yz, "").is_empty());
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
