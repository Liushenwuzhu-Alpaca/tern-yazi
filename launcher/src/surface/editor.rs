//! A single-line text editor with Yazi's vi-like input modes. Pure: no I/O, no session.
//!
//! Yazi's input box (`shell`, `search`, `find`, `filter`, `create`, `rename`, `cd`) opens in
//! insert mode; the first Esc switches to normal mode, a second Esc cancels. The bindings
//! mirror the `[input]` section of Yazi's default keymap and the semantics of its
//! `yazi-widgets` input: operators (`d`, `c`, `y`) wait for a motion, and doubling an operator
//! (`dd`, `cc`, `yy`) takes the whole line.
//!
//! Every line mode shares one entry point, [`Editor::key`]. The caller reacts to the returned
//! [`Edit`]; the caret, selection and vi mode are read from the editor for drawing.

use tern_sdk::keys::Key;

/// The result of one key for the owner of an [`Editor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    /// The text is as before; the caret or the vi mode may still have moved.
    Unchanged,
    /// The text changed (live modes send their request now).
    Changed,
    /// Esc in normal mode, or Ctrl+C: the input is abandoned.
    Cancel,
    /// Enter, in either vi mode.
    Submit,
}

/// A text field edited by keys, with UTF-16 caret offsets as Tern's input node expects.
///
/// `cursor` and `anchor` are UTF-16 offsets; in insert mode they are the caret and the
/// selection. In normal mode the caret is a block over the character at `cursor`
/// (see [`Editor::caret`] for what to draw).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Editor {
    pub(crate) text: String,
    pub(crate) cursor: usize,
    pub(crate) anchor: usize,
    vi: Vi,
    op: Op,
    /// The editor's own clipboard: `d`/`c`/`x`/`s` cut into it, `y` copies, `p`/`P` paste.
    register: String,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// The state when the current insert session (or normal-mode change) began.
    session: Option<Snap>,
}

/// Text and UTF-16 cursor, for undo.
type Snap = (String, usize);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Vi {
    #[default]
    Insert,
    Normal,
    /// `r`: the next typed character replaces the one under the caret.
    Replace,
}

impl Vi {
    /// How far the caret may sit past the last character.
    fn delta(self) -> usize {
        usize::from(self != Vi::Insert)
    }
}

/// A pending normal-mode operation; character indices, not UTF-16 offsets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Op {
    #[default]
    None,
    /// Visual mode: the selection runs from here to the caret, inclusive.
    Select(usize),
    Delete {
        cut: bool,
        insert: bool,
        start: usize,
    },
    Yank(usize),
}

impl Op {
    fn start(self) -> Option<usize> {
        match self {
            Op::None => None,
            Op::Select(start) | Op::Yank(start) | Op::Delete { start, .. } => Some(start),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Space,
    Punct,
    Other,
}

impl Kind {
    fn of(c: char) -> Self {
        if c.is_whitespace() {
            Kind::Space
        } else if c.is_ascii_punctuation() {
            Kind::Punct
        } else {
            Kind::Other
        }
    }

    /// Whether a word (or, `far`, a WORD) boundary lies between two kinds.
    fn vary(self, other: Self, far: bool) -> bool {
        if far {
            (self == Kind::Space) != (other == Kind::Space)
        } else {
            self != other
        }
    }
}

/// What a key did to the editor, for [`Editor::key`].
enum Flow {
    Next,
    Cancel,
    Submit,
}

pub(crate) fn utf16_len(text: &str) -> usize {
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

/// The byte offset of character index `at`, clamped to the end.
fn byte_of(text: &str, at: usize) -> usize {
    text.char_indices()
        .nth(at)
        .map_or(text.len(), |(byte, _)| byte)
}

/// The UTF-16 offset of character index `at`, clamped to the end.
fn units_of(text: &str, at: usize) -> usize {
    utf16_len(&text[..byte_of(text, at)])
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

/// The single character a plain (unchorded, non-paste) key types.
fn plain(key: &Key) -> Option<char> {
    if key.name == "paste" {
        return None;
    }
    let text = typed(key)?;
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// How far a word-wise kill reaches: leading spaces, then one run of a single kind.
fn word_span(chars: impl Iterator<Item = char> + Clone) -> usize {
    let spaces = chars
        .clone()
        .take_while(|&c| Kind::of(c) == Kind::Space)
        .count();
    let mut rest = chars.skip(spaces);
    let Some(first) = rest.next() else {
        return spaces;
    };
    let kind = Kind::of(first);
    spaces + rest.take_while(|&c| Kind::of(c) == kind).count() + 1
}

impl Editor {
    pub(crate) fn new(text: String, cursor: usize, anchor: usize) -> Self {
        Self {
            text,
            cursor,
            anchor,
            ..Self::default()
        }
    }

    /// Puts the insert caret at the end of the text, e.g. when the field takes focus. Normal
    /// mode keeps its caret: a click must not move the vi cursor off a character.
    pub(crate) fn focus(&mut self) {
        if self.vi == Vi::Insert {
            self.cursor = utf16_len(&self.text);
            self.anchor = self.cursor;
        }
    }

    /// Applies a native edit unless it was made against text this editor no longer holds.
    /// Native typing is insert mode by definition.
    pub(crate) fn edit(&mut self, edit: &super::input::FieldEdit) {
        if edit.len != utf16_len(&self.text) {
            return;
        }
        if self.vi != Vi::Insert {
            let snap = (self.text.clone(), self.cursor);
            self.session.get_or_insert(snap);
            self.vi = Vi::Insert;
            self.op = Op::None;
        }
        self.replace(&edit.text, Some((edit.from, edit.to)));
        self.cursor = edit.cursor;
        self.anchor = edit.cursor;
    }

    /// The vi mode to show next to the field: `None` in insert mode.
    pub(crate) fn vi_label(&self) -> Option<&'static str> {
        match (self.vi, self.op) {
            (Vi::Insert, _) => None,
            (Vi::Replace, _) => Some("REPLACE"),
            (Vi::Normal, Op::Select(_)) => Some("VISUAL"),
            (Vi::Normal, _) => Some("NORMAL"),
        }
    }

    /// The `(anchor, cursor)` UTF-16 pair to draw. In insert mode it is the editing caret and
    /// selection; in normal mode the character under the caret is selected, which draws as a
    /// block (and in visual mode the whole selection, inclusive).
    pub(crate) fn caret(&self) -> (usize, usize) {
        if self.vi == Vi::Insert {
            return (self.anchor, self.cursor);
        }
        let at = self.index();
        let start = match self.op {
            Op::Select(start) => start.min(self.count().saturating_sub(1)),
            _ => at,
        };
        let (low, high) = (start.min(at), start.max(at));
        let (from, to) = (
            units_of(&self.text, low),
            units_of(&self.text, high.saturating_add(1)),
        );
        if at >= start {
            (from, to)
        } else {
            (to, from)
        }
    }

    /// Applies one key; see [`Edit`].
    ///
    /// Enter always submits. Esc leaves insert mode (and replace/visual/pending operators),
    /// and cancels only from plain normal mode; Ctrl+C cancels at once.
    pub(crate) fn key(&mut self, key: &Key) -> Edit {
        let old = self.text.clone();
        match self.dispatch(key) {
            Flow::Cancel => Edit::Cancel,
            Flow::Submit => Edit::Submit,
            Flow::Next if self.text == old => Edit::Unchanged,
            Flow::Next => Edit::Changed,
        }
    }

    fn dispatch(&mut self, key: &Key) -> Flow {
        let name = key.name.as_str();
        if name == "enter" {
            return Flow::Submit;
        }
        if key.ctrl && name == "c" {
            return Flow::Cancel;
        }
        if name == "escape" || (key.ctrl && name == "[") {
            if self.vi == Vi::Normal && self.op == Op::None {
                return Flow::Cancel;
            }
            self.escape();
            return Flow::Next;
        }
        if self.vi == Vi::Insert {
            if self.session.is_none() {
                self.session = Some((self.text.clone(), self.cursor));
            }
            if !self.shared_key(key) {
                if let Some(text) = typed(key) {
                    self.replace(&text, None);
                }
            }
            return Flow::Next;
        }
        let before: Snap = (self.text.clone(), self.cursor);
        let neutral = self.vi_key(key);
        if self.vi == Vi::Insert {
            self.session.get_or_insert(before);
        } else if !neutral && self.text != before.0 {
            self.undo.push(before);
            self.redo.clear();
        }
        Flow::Next
    }

    /// One non-insert key; true when it was an undo or redo, which manage history themselves.
    fn vi_key(&mut self, key: &Key) -> bool {
        if key.ctrl && key.name == "r" {
            self.redo();
            return true;
        }
        if self.vi == Vi::Replace {
            if let Some(text) = typed(key) {
                self.replace_char(&text);
                return false;
            }
        }
        if self.shared_key(key) || key.ctrl || key.alt || key.meta {
            return false;
        }
        if self.vi == Vi::Replace {
            return false;
        }
        let Some(ch) = plain(key) else {
            return false;
        };
        let (at, count) = (self.index(), self.count());
        match ch {
            'i' => self.insert(false),
            'a' => self.insert(true),
            'I' => {
                self.go(self.first_char());
                self.insert(false);
            }
            'A' => {
                self.go(count);
                self.insert(true);
            }
            'v' => self.visual(),
            'V' => {
                self.visual_line(true);
            }
            'r' => {
                if self.vi == Vi::Normal {
                    self.op = Op::None;
                    self.vi = Vi::Replace;
                }
            }
            'h' => self.go(at.saturating_sub(1)),
            'l' => self.go(count.min(at + 1)),
            'b' => self.backward(false),
            'B' => self.backward(true),
            'w' => self.forward(false, false),
            'W' => self.forward(true, false),
            'e' => self.forward(false, true),
            'E' => self.forward(true, true),
            '0' => self.go(0),
            '$' => self.go(count),
            '_' | '^' => self.go(self.first_char()),
            'd' => self.delete(true, false),
            'D' => {
                self.delete(true, false);
                self.go(self.count());
            }
            'c' => self.delete(true, true),
            'C' => {
                self.delete(true, true);
                self.go(self.count());
            }
            's' => {
                self.delete(true, true);
                self.go(self.count().min(self.index() + 1));
            }
            'S' => {
                self.go(0);
                self.delete(true, true);
                self.go(self.count());
            }
            'x' => {
                self.delete(true, false);
                if self.op != Op::None {
                    self.go(self.count().min(self.index() + 1));
                }
            }
            'y' => self.yank(),
            'p' => self.paste(false),
            'P' => self.paste(true),
            'u' if self.op == Op::None => {
                self.undo();
                return true;
            }
            'u' => self.casefy(false),
            'U' => self.casefy(true),
            _ => {}
        }
        false
    }

    /// Keys with the same meaning in insert and normal mode. False when not one of them.
    fn shared_key(&mut self, key: &Key) -> bool {
        let insert = self.vi == Vi::Insert;
        let extend = key.shift && insert;
        let (at, count) = (self.index(), self.count());
        let name = key.name.as_str();
        match (name, key.ctrl, key.alt) {
            ("left", false, false) | ("b", true, false) if insert => self.step(false, extend),
            ("right", false, false) | ("f", true, false) if insert => self.step(true, extend),
            ("left", false, false) | ("b", true, false) => self.go(at.saturating_sub(1)),
            ("right", false, false) | ("f", true, false) => self.go(count.min(at + 1)),
            ("left", true, _) | ("b", false, true) => self.backward(false),
            ("right", true, _) | ("f", false, true) => self.forward(false, true),
            ("home", false, false) | ("a", true, false) if !key.shift || name == "home" => {
                if insert {
                    self.edge(false, extend);
                } else {
                    self.go(0);
                }
            }
            ("end", false, false) | ("e", true, false) if !key.shift || name == "end" => {
                if insert {
                    self.edge(true, extend);
                } else {
                    self.go(count);
                }
            }
            ("a", true, false) => self.visual_line(false),
            ("e", true, false) => self.visual_line(true),
            ("backspace", false, false) | ("h", true, false) => {
                self.leave_visual();
                self.erase(false);
                self.clamp();
            }
            ("delete", false, false) | ("d", true, false) => {
                self.leave_visual();
                self.erase(true);
                self.clamp();
            }
            ("u", true, false) => self.kill(0, at),
            ("k", true, false) => self.kill(at, count),
            ("w", true, false) | ("backspace", true, _) => {
                let span = word_span(self.chars()[..at].iter().rev().copied());
                self.kill(at - span, at);
            }
            ("d", false, true) | ("delete", true, _) => {
                let span = word_span(self.chars()[at..].iter().copied());
                self.kill(at, at + span);
            }
            _ => return false,
        }
        true
    }

    // --- positions (character indices; `cursor`/`anchor` stay in UTF-16 units)

    fn chars(&self) -> Vec<char> {
        self.text.chars().collect()
    }

    fn count(&self) -> usize {
        self.text.chars().count()
    }

    /// The caret as a character index.
    fn index(&self) -> usize {
        self.text[..byte_at(&self.text, self.cursor)]
            .chars()
            .count()
    }

    fn first_char(&self) -> usize {
        self.text
            .chars()
            .position(|c| !c.is_whitespace())
            .unwrap_or(0)
    }

    /// Puts the caret on character index `at`; it only drags the anchor outside visual mode.
    fn set_index(&mut self, at: usize) {
        self.cursor = units_of(&self.text, at);
        if !matches!(self.op, Op::Select(_)) {
            self.anchor = self.cursor;
        }
    }

    /// Keeps the caret on a character in normal mode.
    fn clamp(&mut self) {
        let max = self.count().saturating_sub(self.vi.delta());
        if self.index() > max {
            self.set_index(max);
        }
    }

    /// Replaces character range `[from, to)` and returns what it removed.
    fn splice(&mut self, from: usize, to: usize, insert: &str) -> String {
        let (start, end) = (byte_of(&self.text, from), byte_of(&self.text, to.max(from)));
        let removed = self.text[start..end].to_owned();
        self.text.replace_range(start..end, insert);
        removed
    }

    // --- insert-mode primitives

    /// Replaces UTF-16 `[from, to)` (default: the selection) and collapses the caret after it.
    fn replace(&mut self, insert: &str, range: Option<(usize, usize)>) {
        let (from, to) =
            range.unwrap_or((self.cursor.min(self.anchor), self.cursor.max(self.anchor)));
        let (start, end) = (byte_at(&self.text, from), byte_at(&self.text, to.max(from)));
        self.text.replace_range(start..end, insert);
        self.cursor = utf16_len(&self.text[..start]) + utf16_len(insert);
        self.anchor = self.cursor;
    }

    fn erase(&mut self, forward: bool) {
        if self.cursor != self.anchor {
            return self.replace("", None);
        }
        let at = byte_at(&self.text, self.cursor);
        let span = if forward {
            self.text[at..]
                .chars()
                .next()
                .map(|c| (at, at + c.len_utf8()))
        } else {
            self.text[..at]
                .chars()
                .next_back()
                .map(|c| (at - c.len_utf8(), at))
        };
        if let Some((start, end)) = span {
            let from = utf16_len(&self.text[..start]);
            self.replace("", Some((from, from + utf16_len(&self.text[start..end]))));
        }
    }

    fn step(&mut self, right: bool, extend: bool) {
        let at = byte_at(&self.text, self.cursor);
        let next = if right {
            self.text[at..]
                .chars()
                .next()
                .map_or(at, |c| at + c.len_utf8())
        } else {
            self.text[..at]
                .chars()
                .next_back()
                .map_or(at, |c| at - c.len_utf8())
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

    /// Kills characters `[from, to)` and leaves the caret at `from`.
    fn kill(&mut self, from: usize, to: usize) {
        self.leave_visual();
        if self.splice(from, to, "").is_empty() {
            return;
        }
        self.set_index(from);
        self.clamp();
    }

    /// Visual mode ends when something else edits the text.
    fn leave_visual(&mut self) {
        if matches!(self.op, Op::Select(_)) {
            self.op = Op::None;
            self.anchor = self.cursor;
        }
    }

    // --- vi actions

    /// Moves the caret to `target` the way a motion does: it moves, or completes a pending
    /// operator over the span it covers (end exclusive).
    fn go(&mut self, target: usize) {
        self.resolve(target, false);
    }

    fn resolve(&mut self, target: usize, include: bool) {
        let count = self.count();
        let target = target.min(count);
        match self.op {
            Op::None | Op::Select(_) => self.set_index(target),
            Op::Delete { cut, insert, start } => {
                let (from, to) = self.span(start, target, include);
                let removed = self.splice(from, to, "");
                if cut && !removed.is_empty() {
                    self.register = removed;
                }
                self.op = Op::None;
                self.vi = if insert { Vi::Insert } else { Vi::Normal };
                self.set_index(from);
            }
            Op::Yank(start) => {
                let (from, to) = self.span(start, target, include);
                let (start, end) = (byte_of(&self.text, from), byte_of(&self.text, to));
                self.register = self.text[start..end].to_owned();
                self.op = Op::None;
            }
        }
        self.clamp();
    }

    /// The character range between an operator's start and `target`.
    fn span(&self, start: usize, target: usize, include: bool) -> (usize, usize) {
        let count = self.count();
        let (from, to) = (start.min(target), start.max(target));
        (from.min(count), (to + usize::from(include)).min(count))
    }

    /// `i`/`a`: enters insert mode before or after the character under the caret.
    fn insert(&mut self, append: bool) {
        if self.vi != Vi::Normal {
            return;
        }
        self.op = Op::None;
        self.vi = Vi::Insert;
        if append {
            self.go(self.count().min(self.index() + 1));
        }
    }

    fn escape(&mut self) {
        match self.vi {
            Vi::Normal => {
                self.leave_visual();
                self.op = Op::None;
            }
            Vi::Insert => {
                self.vi = Vi::Normal;
                self.op = Op::None;
                self.go(self.index().saturating_sub(1));
                if let Some(start) = self.session.take().filter(|s| s.0 != self.text) {
                    self.undo.push(start);
                    self.redo.clear();
                }
            }
            Vi::Replace => self.vi = Vi::Normal,
        }
    }

    fn visual(&mut self) {
        if self.vi != Vi::Normal {
            self.escape();
        }
        if matches!(self.op, Op::Select(_)) || self.text.is_empty() {
            return;
        }
        self.op = Op::Select(self.index());
        self.anchor = self.cursor;
    }

    /// Selects the whole line: BOL to EOL, or EOL to BOL (Ctrl+Shift+A).
    fn visual_line(&mut self, forward: bool) {
        let count = self.count();
        self.go(if forward { 0 } else { count });
        self.visual();
        self.go(if forward { self.count() } else { 0 });
    }

    fn delete(&mut self, cut: bool, insert: bool) {
        match self.op {
            Op::None => {
                self.op = Op::Delete {
                    cut,
                    insert,
                    start: self.index(),
                };
            }
            Op::Select(start) => {
                self.op = Op::Delete { cut, insert, start };
                self.resolve(self.index(), true);
                self.go(self.index());
            }
            Op::Delete { .. } => {
                self.op = Op::Delete {
                    cut,
                    insert,
                    start: 0,
                };
                self.go(self.count());
            }
            Op::Yank(_) => {}
        }
    }

    fn yank(&mut self) {
        match self.op {
            Op::None => self.op = Op::Yank(self.index()),
            Op::Select(start) => {
                self.op = Op::Yank(start);
                self.resolve(self.index(), true);
                self.go(self.index());
            }
            Op::Yank(_) => {
                self.op = Op::Yank(0);
                self.go(self.count());
            }
            Op::Delete { .. } => {}
        }
    }

    /// `p`/`P`: pastes the register after/before the caret, replacing a selection.
    fn paste(&mut self, before: bool) {
        if let Some(start) = self.op.start() {
            self.op = Op::Delete {
                cut: false,
                insert: false,
                start,
            };
            self.resolve(self.index(), true);
        }
        if self.register.is_empty() {
            return;
        }
        let register = self.register.clone();
        self.insert(!before);
        self.replace(&register, None);
        self.escape();
    }

    fn casefy(&mut self, upper: bool) {
        let Op::Select(start) = self.op else {
            return;
        };
        let (from, to) = self.span(start, self.index(), true);
        let (low, high) = (byte_of(&self.text, from), byte_of(&self.text, to));
        let mapped = if upper {
            self.text[low..high].to_uppercase()
        } else {
            self.text[low..high].to_lowercase()
        };
        self.text.replace_range(low..high, &mapped);
        self.op = Op::None;
        self.set_index(from);
        self.clamp();
    }

    /// `r` then a key: replaces the character under the caret.
    fn replace_char(&mut self, with: &str) {
        self.vi = Vi::Normal;
        let at = self.index();
        if at < self.count() {
            self.splice(at, at + 1, with);
        }
        self.set_index(at);
        self.clamp();
    }

    fn undo(&mut self) {
        if let Some(snap) = self.undo.pop() {
            let now = (self.text.clone(), self.cursor);
            self.redo.push(now);
            self.restore(snap);
        }
    }

    fn redo(&mut self) {
        if let Some(snap) = self.redo.pop() {
            let now = (self.text.clone(), self.cursor);
            self.undo.push(now);
            self.restore(snap);
        }
    }

    fn restore(&mut self, (text, cursor): Snap) {
        self.text = text;
        self.op = Op::None;
        self.cursor = cursor;
        self.anchor = cursor;
        self.clamp();
    }

    // --- word motions, ported from Yazi

    fn forward(&mut self, far: bool, end_of_word: bool) {
        let chars = self.chars();
        let at = self.index();
        let mut it = chars.iter().skip(at).enumerate();
        let Some(mut prev) = it.next().map(|(_, &c)| Kind::of(c)) else {
            return self.go(at);
        };
        let operating = !matches!(self.op, Op::None | Op::Select(_));
        for (i, &c) in it {
            let kind = Kind::of(c);
            let boundary = if end_of_word {
                prev != Kind::Space && prev.vary(kind, far) && i != 1
            } else {
                kind != Kind::Space && kind.vary(prev, far)
            };
            if boundary {
                let step = if end_of_word && !operating {
                    i - self.vi.delta()
                } else {
                    i
                };
                return self.go(at + step);
            }
            prev = kind;
        }
        self.go(chars.len());
    }

    fn backward(&mut self, far: bool) {
        let chars = self.chars();
        let at = self.index().min(chars.len());
        if at == 0 {
            return self.go(0);
        }
        let mut it = chars[..at].iter().rev().enumerate();
        let mut prev = Kind::of(*it.next().map(|(_, c)| c).unwrap_or(&' '));
        for (i, &c) in it {
            let kind = Kind::of(c);
            if prev != Kind::Space && prev.vary(kind, far) {
                return self.go(at - i);
            }
            prev = kind;
        }
        if prev != Kind::Space {
            self.go(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Key {
        Key {
            text: (name.chars().count() == 1).then(|| name.to_owned()),
            ..Key::named(name)
        }
    }

    fn ctrl(name: &str) -> Key {
        Key {
            ctrl: true,
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

    /// An editor in normal mode over `text`, caret on character `at`.
    fn normal(text: &str, at: usize) -> Editor {
        let mut editor = Editor::new(text.to_owned(), utf16_len(text), utf16_len(text));
        editor.key(&key("escape"));
        editor.set_index(at);
        editor
    }

    fn press(editor: &mut Editor, keys: &str) {
        for c in keys.chars() {
            let key = if c.is_uppercase() {
                shifted(&c.to_lowercase().to_string())
            } else if c == ' ' {
                Key::named("space")
            } else {
                key(&c.to_string())
            };
            editor.key(&key);
        }
    }

    #[test]
    fn insert_edits_in_utf16_units() {
        let mut editor = Editor::new("a😀b".to_owned(), 3, 3);
        assert_eq!(editor.key(&key("backspace")), Edit::Changed);
        assert_eq!((editor.text.as_str(), editor.cursor), ("ab", 1));
        editor.key(&key("x"));
        assert_eq!((editor.text.as_str(), editor.cursor), ("axb", 2));
        editor.key(&Key {
            shift: true,
            ..Key::named("home")
        });
        assert_eq!((editor.cursor, editor.anchor), (0, 2));
        editor.key(&key("é"));
        assert_eq!((editor.text.as_str(), editor.cursor), ("éb", 1));
        assert_eq!(editor.key(&key("delete")), Edit::Changed);
        assert_eq!(editor.text, "é");
        assert_eq!(editor.key(&key("left")), Edit::Unchanged);
        editor.key(&key("end"));
        editor.key(&ctrl("u"));
        assert_eq!(
            (editor.text.as_str(), editor.cursor, editor.anchor),
            ("", 0, 0)
        );
    }

    #[test]
    fn insert_chords_follow_yazi() {
        let mut editor = Editor::new("one two three".to_owned(), 7, 7);
        editor.key(&ctrl("w"));
        assert_eq!((editor.text.as_str(), editor.cursor), ("one  three", 4));
        editor.key(&ctrl("e"));
        assert_eq!(editor.cursor, 10);
        editor.key(&ctrl("a"));
        assert_eq!(editor.cursor, 0);
        editor.key(&ctrl("k"));
        assert_eq!(editor.text, "");
        press(&mut editor, "ab");
        editor.key(&ctrl("h"));
        assert_eq!(editor.text, "a");
        editor.key(&ctrl("a"));
        editor.key(&ctrl("d"));
        assert_eq!(editor.text, "");
    }

    #[test]
    fn first_escape_enters_normal_mode_and_second_cancels() {
        let mut editor = Editor::new("abc".to_owned(), 3, 3);
        assert_eq!(editor.vi_label(), None);
        assert_eq!(editor.key(&key("escape")), Edit::Unchanged);
        assert_eq!(editor.vi_label(), Some("NORMAL"));
        assert_eq!(editor.cursor, 2);
        assert_eq!(editor.key(&key("escape")), Edit::Cancel);
        assert_eq!(
            Editor::new(String::new(), 0, 0).key(&ctrl("c")),
            Edit::Cancel
        );
        let mut editor = Editor::new("abc".to_owned(), 3, 3);
        assert_eq!(editor.key(&ctrl("[")), Edit::Unchanged);
        assert_eq!(editor.key(&ctrl("[")), Edit::Cancel);
    }

    #[test]
    fn enter_submits_from_either_mode() {
        let mut editor = Editor::new("abc".to_owned(), 3, 3);
        assert_eq!(editor.key(&key("enter")), Edit::Submit);
        editor.key(&key("escape"));
        assert_eq!(editor.key(&key("enter")), Edit::Submit);
    }

    #[test]
    fn escape_clears_pending_operators_and_visual_before_cancelling() {
        let mut editor = normal("abc", 0);
        press(&mut editor, "d");
        assert_eq!(editor.key(&key("escape")), Edit::Unchanged);
        press(&mut editor, "v");
        assert_eq!(editor.vi_label(), Some("VISUAL"));
        assert_eq!(editor.key(&key("escape")), Edit::Unchanged);
        assert_eq!(editor.vi_label(), Some("NORMAL"));
        assert_eq!(editor.key(&key("escape")), Edit::Cancel);
        let mut editor = normal("abc", 0);
        press(&mut editor, "r");
        assert_eq!(editor.vi_label(), Some("REPLACE"));
        assert_eq!(editor.key(&key("escape")), Edit::Unchanged);
        assert_eq!(editor.vi_label(), Some("NORMAL"));
    }

    #[test]
    fn insert_commands_enter_insert_mode_at_the_right_place() {
        let mut editor = normal("one two", 2);
        press(&mut editor, "i");
        press(&mut editor, "X");
        assert_eq!(editor.text, "onXe two");
        let mut editor = normal("one two", 2);
        press(&mut editor, "aX");
        assert_eq!(editor.text, "oneX two");
        let mut editor = normal("  one", 4);
        press(&mut editor, "IX");
        assert_eq!(editor.text, "  Xone");
        let mut editor = normal("one", 0);
        press(&mut editor, "AX");
        assert_eq!(editor.text, "oneX");
    }

    #[test]
    fn motions_move_the_normal_caret() {
        let mut editor = normal("foo bar.baz  qux", 0);
        let at = |e: &Editor| e.cursor;
        press(&mut editor, "w");
        assert_eq!(at(&editor), 4);
        press(&mut editor, "w");
        assert_eq!(at(&editor), 7);
        press(&mut editor, "W");
        assert_eq!(at(&editor), 13);
        press(&mut editor, "b");
        assert_eq!(at(&editor), 8);
        press(&mut editor, "0");
        assert_eq!(at(&editor), 0);
        press(&mut editor, "e");
        assert_eq!(at(&editor), 2);
        press(&mut editor, "$");
        assert_eq!(at(&editor), 15);
        press(&mut editor, "h");
        assert_eq!(at(&editor), 14);
        press(&mut editor, "l");
        press(&mut editor, "l");
        assert_eq!(at(&editor), 15);
        press(&mut editor, "^");
        assert_eq!(at(&editor), 0);
        editor.key(&key("end"));
        assert_eq!(at(&editor), 15);
        editor.key(&key("home"));
        assert_eq!(at(&editor), 0);
        editor.key(&key("right"));
        assert_eq!(at(&editor), 1);
        editor.key(&key("left"));
        assert_eq!(at(&editor), 0);
        let mut editor = normal("  ab", 3);
        press(&mut editor, "_");
        assert_eq!(editor.cursor, 2);
    }

    #[test]
    fn x_deletes_the_character_under_the_caret() {
        let mut editor = normal("abc", 1);
        assert_eq!(editor.key(&key("x")), Edit::Changed);
        assert_eq!((editor.text.as_str(), editor.cursor), ("ac", 1));
        press(&mut editor, "x");
        assert_eq!((editor.text.as_str(), editor.cursor), ("a", 0));
        press(&mut editor, "x");
        assert_eq!(editor.text, "");
        assert_eq!(editor.key(&key("x")), Edit::Unchanged);
        let mut editor = normal("a😀b", 1);
        press(&mut editor, "x");
        assert_eq!((editor.text.as_str(), editor.cursor), ("ab", 1));
    }

    #[test]
    fn operators_take_a_motion_or_double_for_the_whole_line() {
        let mut editor = normal("foo bar baz", 4);
        press(&mut editor, "dw");
        assert_eq!((editor.text.as_str(), editor.cursor), ("foo baz", 4));
        press(&mut editor, "db");
        assert_eq!((editor.text.as_str(), editor.cursor), ("baz", 0));
        let mut editor = normal("foo bar", 1);
        press(&mut editor, "d$");
        assert_eq!((editor.text.as_str(), editor.cursor), ("f", 0));
        let mut editor = normal("foo bar", 4);
        press(&mut editor, "d0");
        assert_eq!(editor.text, "bar");
        let mut editor = normal("foo bar", 4);
        press(&mut editor, "dd");
        assert_eq!(editor.text, "");
        assert_eq!(editor.vi_label(), Some("NORMAL"));
        let mut editor = normal("foo bar", 4);
        press(&mut editor, "D");
        assert_eq!((editor.text.as_str(), editor.cursor), ("foo ", 3));
        let mut editor = normal("foo bar", 4);
        press(&mut editor, "de");
        assert_eq!(editor.text, "foo ");
        let mut editor = normal("foo bar", 0);
        press(&mut editor, "dl");
        assert_eq!(editor.text, "oo bar");
    }

    #[test]
    fn change_operators_enter_insert_mode() {
        let mut editor = normal("foo bar", 4);
        press(&mut editor, "cwX");
        assert_eq!(editor.text, "foo X");
        assert_eq!(editor.vi_label(), None);
        let mut editor = normal("foo bar", 4);
        press(&mut editor, "ccX");
        assert_eq!(editor.text, "X");
        let mut editor = normal("foo bar", 3);
        press(&mut editor, "CX");
        assert_eq!(editor.text, "fooX");
        let mut editor = normal("foo", 1);
        press(&mut editor, "sX");
        assert_eq!(editor.text, "fXo");
        let mut editor = normal("foo bar", 1);
        press(&mut editor, "SX");
        assert_eq!(editor.text, "X");
    }

    #[test]
    fn yank_and_paste_use_the_editors_register() {
        let mut editor = normal("foo bar", 0);
        press(&mut editor, "yw");
        assert_eq!((editor.text.as_str(), editor.cursor), ("foo bar", 0));
        press(&mut editor, "$p");
        assert_eq!(editor.text, "foo barfoo ");
        assert_eq!(editor.cursor, 10);
        let mut editor = normal("ab", 1);
        press(&mut editor, "yy");
        press(&mut editor, "P");
        assert_eq!(editor.text, "aabb");
        let mut editor = normal("ab", 0);
        press(&mut editor, "xp");
        assert_eq!(editor.text, "ba");
        let mut editor = normal("", 0);
        assert_eq!(editor.key(&key("p")), Edit::Unchanged);
    }

    #[test]
    fn visual_mode_selects_inclusively() {
        let mut editor = normal("foo bar", 0);
        press(&mut editor, "vll");
        assert_eq!(editor.caret(), (0, 3));
        press(&mut editor, "d");
        assert_eq!(editor.text, " bar");
        assert_eq!(editor.vi_label(), Some("NORMAL"));
        let mut editor = normal("foo bar", 4);
        press(&mut editor, "vly");
        assert_eq!(editor.text, "foo bar");
        press(&mut editor, "0P");
        assert_eq!(editor.text, "bafoo bar");
        let mut editor = normal("Foo", 0);
        press(&mut editor, "vlU");
        assert_eq!(editor.text, "FOo");
        press(&mut editor, "vlu");
        assert_eq!(editor.text, "foo");
        let mut editor = normal("foo bar", 2);
        press(&mut editor, "V");
        assert_eq!(editor.caret(), (0, 7));
        press(&mut editor, "c");
        assert_eq!(editor.text, "");
        assert_eq!(editor.vi_label(), None);
    }

    #[test]
    fn replace_swaps_one_character() {
        let mut editor = normal("abc", 1);
        press(&mut editor, "rX");
        assert_eq!((editor.text.as_str(), editor.cursor), ("aXc", 1));
        assert_eq!(editor.vi_label(), Some("NORMAL"));
    }

    #[test]
    fn undo_and_redo_walk_insert_sessions_and_normal_edits() {
        let mut editor = Editor::new(String::new(), 0, 0);
        press(&mut editor, "abc");
        editor.key(&key("escape"));
        press(&mut editor, "x");
        assert_eq!(editor.text, "ab");
        press(&mut editor, "u");
        assert_eq!(editor.text, "abc");
        press(&mut editor, "u");
        assert_eq!(editor.text, "");
        press(&mut editor, "u");
        assert_eq!(editor.text, "");
        editor.key(&ctrl("r"));
        assert_eq!(editor.text, "abc");
        editor.key(&ctrl("r"));
        assert_eq!(editor.text, "ab");
        let mut editor = normal("foo", 0);
        press(&mut editor, "cwbar");
        editor.key(&key("escape"));
        press(&mut editor, "u");
        assert_eq!(editor.text, "foo");
    }

    #[test]
    fn normal_mode_ignores_typing_and_chords_edit() {
        let mut editor = normal("foo", 1);
        assert_eq!(editor.key(&key("z")), Edit::Unchanged);
        assert_eq!(editor.key(&key("paste")), Edit::Unchanged);
        assert_eq!(editor.text, "foo");
        assert_eq!(editor.key(&key("backspace")), Edit::Changed);
        assert_eq!((editor.text.as_str(), editor.cursor), ("oo", 0));
        editor.key(&ctrl("e"));
        assert_eq!(editor.cursor, 1);
        editor.key(&ctrl("u"));
        assert_eq!(editor.text, "o");
    }

    #[test]
    fn caret_draws_a_block_in_normal_mode() {
        let mut editor = Editor::new("a😀b".to_owned(), 4, 4);
        assert_eq!(editor.caret(), (4, 4));
        editor.key(&key("escape"));
        assert_eq!(editor.caret(), (3, 4));
        press(&mut editor, "h");
        assert_eq!(editor.caret(), (1, 3));
        let empty = normal("", 0);
        assert_eq!(empty.caret(), (0, 0));
    }

    #[test]
    fn native_edits_put_the_editor_back_in_insert_mode() {
        let mut editor = normal("abc", 0);
        editor.edit(&crate::surface::input::FieldEdit {
            from: 3,
            to: 3,
            text: "d".to_owned(),
            cursor: 4,
            len: 3,
        });
        assert_eq!((editor.text.as_str(), editor.vi_label()), ("abcd", None));
    }
}
