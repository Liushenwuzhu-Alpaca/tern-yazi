//! The view: a pure function from the snapshot, UI state and preview load state to the surface tree.
//! Roles and keys follow the plugin view so the stylesheets and tests select the same nodes.

use std::collections::HashSet;

use serde_json::json;
use tern_sdk::ui::{self, Node, Span, View};

use super::format::{self, path_segments};
use super::input::{Editor, Field, FieldEdit, Level, List, Mode, Msg, Ui};
use super::model::{basename, join_path, parent_dir, relative, Folder, Snapshot, TaskManager};
use super::preview::{Blob, Load, Preview, Stamp};
use super::theme::{self, Theme, THEMES};

type N = Node<Msg>;

/// Region wrappers: the theme role and then the region role, so scoped sheets can target
/// both (program sheets cannot style the region element itself).
pub(crate) const BROWSER: &str = "main.theme.region.browser";

fn list_id(list: List) -> String {
    match list {
        List::Parent => format!("{BROWSER}.columns.parent.parent_list"),
        List::Files => format!("{BROWSER}.columns.current.files_list"),
        List::Child => format!("{BROWSER}.columns.preview.child_list"),
        List::Themes => "layer.theme.region.themes.list".to_owned(),
    }
}

/// What the startup screen explains while there is no snapshot.
pub(crate) enum Waiting<'a> {
    Starting,
    Failed(&'a str),
}

pub(crate) struct Frame<'a> {
    pub(crate) yz: Option<&'a Snapshot>,
    pub(crate) waiting: Waiting<'a>,
    pub(crate) ui: &'a Ui,
    pub(crate) preview: Option<&'a Load>,
    pub(crate) notice: Option<(Level, &'a str)>,
    pub(crate) cols: u32,
    pub(crate) rows: u32,
    pub(crate) home: &'a str,
    /// Withholds list selections for one frame so native lists reveal them after a relayout.
    pub(crate) reveal_withheld: bool,
}

fn span(text: impl Into<String>, style: &str) -> Span {
    let span = ui::span(text);
    if style.is_empty() {
        span
    } else {
        span.style(style)
    }
}

fn spans(parts: Vec<Span>) -> N {
    ui::text(parts).into()
}

fn muted(text: impl Into<String>) -> N {
    spans(vec![span(text, "muted")])
}

fn card(head: Vec<Span>, children: Vec<N>) -> ui::Card<Msg> {
    ui::card().head(head).children(children)
}

/// The preview's metadata line: kind, size, modification time and permissions.
fn metadata(kind: String, hover: Option<&super::model::Hover>) -> N {
    let size = hover.and_then(|hover| hover.size);
    let mtime = hover.and_then(|hover| hover.mtime);
    let permissions = format::permissions(hover.and_then(|hover| hover.permissions.as_deref()));
    ui::row()
        .key("metadata")
        .role("yazi.preview-metadata")
        .prop("gap", "xs")
        .prop("wrap", true)
        .children([
            muted(kind),
            ui::text(format::size(size)).role("yazi.preview-size").into(),
            ui::text(format::mtime(mtime))
                .role("yazi.preview-mtime")
                .tone("muted")
                .into(),
            ui::text(permissions)
                .role("yazi.preview-permissions")
                .tone("muted")
                .into(),
        ])
        .into()
}

fn button(key: &str, label: impl Into<N>, msg: Msg) -> N {
    ui::node("el")
        .prop("tag", "button")
        .key(key)
        .child(label)
        .on_click(msg)
}

/// A region's root node becomes the region element, which program sheets cannot select, so
/// the theme wrapper (theme and layout role) and the region wrapper the sheets address sit
/// below a bare root.
fn region(theme_role: &str, name: &str, children: Vec<N>) -> N {
    ui::col()
        .child(
            ui::col().key("theme").role(theme_role).child(
                ui::col()
                    .key("region")
                    .role(format!("yazi.{name}"))
                    .children(children),
            ),
        )
        .into()
}

pub(crate) fn view(frame: &Frame) -> View<Msg> {
    let theme = &THEMES[frame.ui.theme];
    let role = theme::role(theme, frame.cols);
    let Some(yz) = frame.yz else {
        return View::new().main(region(&role, "main", vec![startup(&frame.waiting)]));
    };
    let browser = Browser { frame, yz, theme };
    let (main, path_layer) = browser.main();
    let mut view = View::new()
        .main(region(&role, "main", vec![main]))
        .dock(region(&role, "dock", browser.dock()));
    let layer = match &frame.ui.mode {
        Mode::Trash(paths) => Some(trash_layer(paths, frame.rows)),
        Mode::Theme { at, .. } => Some(theme_layer(*at)),
        Mode::Tasks => Some(tasks_layer(&yz.task_manager)),
        _ => path_layer,
    };
    if let Some(layer) = layer {
        view = view.layer(region(&role, "layer", vec![layer]));
    }
    view
}

fn startup(waiting: &Waiting) -> N {
    let (title, detail) = match waiting {
        Waiting::Starting => (
            "Starting Yazi",
            "Waiting for Yazi to publish its first snapshot.",
        ),
        Waiting::Failed(error) => ("Yazi snapshot unavailable", *error),
    };
    card(
        vec![span(title, "warn")],
        vec![
            ui::text(detail).into(),
            ui::text("Press q to close.").into(),
        ],
    )
    .into()
}

fn trash_layer(paths: &[String], rows: u32) -> N {
    let items = paths.iter().enumerate().map(|(index, path)| {
        ui::item(serde_json::to_string(path).unwrap_or_default()).key((index + 1).to_string())
    });
    let list = ui::list()
        .key("paths")
        .max_lines(rows.saturating_sub(8).max(4))
        .children(items);
    ui::node("card")
        .key("trash")
        .role("yazi.trash-confirm")
        .child(ui::text(
            "Move these exact paths to Trash? Cancel changes nothing.",
        ))
        .child(list)
        .child(
            ui::row()
                .key("controls")
                .child(button("cancel", ui::text("Cancel [Esc]"), Msg::TrashCancel))
                .child(
                    ui::node("el")
                        .prop("tag", "button")
                        .key("confirm")
                        .tone("warn")
                        .child(ui::text("Move to Trash [Enter]"))
                        .on_click(Msg::TrashConfirm),
                ),
        )
}

fn theme_layer(at: usize) -> N {
    let prefix = format!("{}.", list_id(List::Themes));
    let (select, activate) = (prefix.clone(), prefix);
    let items = THEMES
        .iter()
        .map(|theme| ui::item(theme.title).key(theme.id).detail(theme.detail));
    let list = ui::list()
        .key("list")
        .selected(format!("{}.{}", list_id(List::Themes), THEMES[at].id))
        .max_lines(THEMES.len() as u32)
        .children(items)
        .on_select(move |item| {
            Msg::Select(
                List::Themes,
                item.strip_prefix(&select).unwrap_or(item).to_owned(),
            )
        })
        .on_activate(move |item| {
            Msg::Activate(
                List::Themes,
                item.strip_prefix(&activate).unwrap_or(item).to_owned(),
            )
        });
    ui::node("card")
        .key("themes")
        .role("yazi.theme-picker")
        .prop("head", "Layout theme")
        .child(list)
        .child(muted("j/k preview · Enter keep · Esc restore"))
}

/// Yazi's task manager: each task's title, then its progress, as Yazi's `Tasks` component.
fn tasks_layer(manager: &TaskManager) -> N {
    let rows: Vec<N> = manager
        .rows
        .iter()
        .enumerate()
        .map(|(index, task)| {
            let hovered = index == manager.cursor;
            let icon = match task.kind.as_str() {
                "FileCopy" => '\u{f0c5}',
                "FileCut" => '\u{f0c4}',
                "FileDelete" => '\u{f1f8}',
                "FileDownload" => '\u{f019}',
                "FileUpload" => '\u{f093}',
                _ => '\u{f085}',
            };
            let title = span(
                format!("{icon}  {}", task.title),
                if hovered { "accent" } else { "" },
            );
            let file = matches!(
                task.kind.as_str(),
                "FileCopy" | "FileCut" | "FileDelete" | "FileDownload" | "FileUpload"
            );
            let progress: N = if file {
                let percent = if task.cooked {
                    "Cleaning…".to_owned()
                } else {
                    format!("{:3}%", task.percent.floor() as i64)
                };
                let label = format!(
                    "{percent} - {} / {}   {}/{}",
                    format::size(task.processed_bytes),
                    format::size(task.total_bytes),
                    task.success_files,
                    task.total_files,
                );
                let bar = ui::progress()
                    .value((task.percent / 100.0).clamp(0.0, 1.0))
                    .label(label);
                Node::from(bar).role(if task.failed || task.failed_files > 0 {
                    "yazi.task-progress.error"
                } else {
                    "yazi.task-progress"
                })
            } else {
                muted(if task.cooked {
                    "Cleaning…"
                } else if task.running {
                    "Running…"
                } else {
                    "Failed, press Enter to view log…"
                })
            };
            ui::col()
                .key(format!("task{index}"))
                .role(if hovered { "yazi.task.hovered" } else { "yazi.task" })
                .children([spans(vec![title]), progress])
                .into()
        })
        .collect();
    let body = if rows.is_empty() {
        muted("No tasks")
    } else {
        ui::col().key("tasks").children(rows).into()
    };
    ui::node("card")
        .key("tasks")
        .role("yazi.task-manager")
        .prop("head", "Tasks")
        .child(body)
        .child(muted("j/k move · Enter inspect · x cancel · Esc close"))
}

struct Browser<'a> {
    frame: &'a Frame<'a>,
    yz: &'a Snapshot,
    theme: &'a Theme,
}

/// The current directory as the browser presents it.
struct Focus<'a> {
    cwd: &'a str,
    files: &'a [String],
    active: Option<&'a str>,
    index: usize,
    url: String,
    hovered: Option<&'a super::model::Hover>,
    is_dir: Option<bool>,
}

/// The file whose preview the browser shows, if it is a known regular file.
pub(crate) fn preview_target(yz: &Snapshot, ui: &Ui) -> Option<Stamp> {
    let focus = focus(yz, ui);
    (focus.active.is_some() && focus.is_dir == Some(false)).then(|| Stamp {
        size: focus.hovered.and_then(|hover| hover.size),
        mtime: focus.hovered.and_then(|hover| hover.mtime),
        path: focus.url,
    })
}

fn focus<'a>(yz: &'a Snapshot, ui: &'a Ui) -> Focus<'a> {
    let cwd = ui.cwd(yz);
    let files = yz
        .directory(cwd)
        .map_or(&[][..], |folder| &folder.files[..]);
    let hovered_name = yz
        .hovered
        .as_ref()
        .filter(|_| yz.current.cwd == cwd)
        .map(|hover| relative(cwd, &hover.url));
    let active = ui
        .active_file
        .as_deref()
        .or(hovered_name)
        .or(files.first().map(String::as_str));
    let index = active
        .and_then(|name| files.iter().position(|file| file == name))
        .map_or(1, |at| at + 1);
    let url = active.map_or_else(|| cwd.to_owned(), |name| join_path(cwd, name));
    let hovered = yz.hovered.as_ref().filter(|hover| hover.url == url);
    Focus {
        cwd,
        files,
        active,
        index,
        url,
        hovered,
        is_dir: active.and_then(|name| yz.is_dir(cwd, name)),
    }
}

impl Browser<'_> {
    fn main(&self) -> (N, Option<N>) {
        let focus = focus(self.yz, self.frame.ui);
        let mut rows: Vec<N> = Vec::new();
        if self.yz.tabs.len() > 1 {
            rows.push(self.tabs());
        }
        let (header, path_layer) = self.path_header(focus.cwd);
        rows.push(header);
        rows.push(self.input_row());
        rows.push(self.columns(&focus));
        let browser = ui::col()
            .role("yazi.browser")
            .key("browser")
            .prop("gap", "sm")
            .children(rows);
        (browser.into(), path_layer)
    }

    fn tabs(&self) -> N {
        let tabs = self.yz.tabs.iter().enumerate().map(|(index, tab)| {
            let label = if tab.name.is_empty() {
                format!("{} {}", index + 1, index + 1)
            } else {
                format!("{} {}", index + 1, tab.name)
            };
            ui::badge(label)
                .tone(if tab.active { "accent" } else { "muted" })
                .key(format!("tab{}", index + 1))
                .role(if tab.active {
                    "yazi.tab-active"
                } else {
                    "yazi.tab"
                })
                .prop("active", tab.active)
                .on_click(Msg::TabSwitch(index))
        });
        ui::row()
            .role("yazi.tab-bar")
            .key("tabs")
            .prop("gap", "xs")
            .children(tabs)
            .into()
    }

    fn path_header(&self, cwd: &str) -> (N, Option<N>) {
        let mut children: Vec<N> = Vec::new();
        let mut layer = None;
        if let Mode::Path(editor) = &self.frame.ui.mode {
            children.push(field(
                "field",
                Field::Path,
                Some("yazi.path-input"),
                editor,
                Some("Directory path"),
            ));
            children.push(button("go", ui::text("Go"), Msg::PathSubmit).role("yazi.path-go"));
            children.push(
                button("cancel", ui::text("Cancel"), Msg::PathCancel).role("yazi.path-cancel"),
            );
        } else {
            let segments = path_segments(cwd, self.frame.home);
            let budget = self.frame.cols.saturating_sub(12).max(8) as usize;
            let cap = (budget / 2).clamp(4, 24);
            let width = |label: &str| label.chars().count().min(cap);
            let mut first = segments.len();
            let mut used = width(&segments[segments.len() - 1].label) + 5;
            while first > 2 {
                let cost = width(&segments[first - 2].label) + 3;
                if used + cost > budget {
                    break;
                }
                used += cost;
                first -= 1;
            }
            let crumb = |index: usize| -> N {
                let segment = &segments[index - 1];
                button(
                    &format!("dir{index}"),
                    ui::text(segment.label.clone()).wrap("none").truncate("end"),
                    Msg::PathCd(segment.path.clone()),
                )
                .role("yazi.path-segment")
                .title(segment.path.clone())
                .prop("min", json!({"w": 0}))
                .prop("max", json!({"w": format!("{cap}ch")}))
            };
            children.push(crumb(1));
            if first > 2 {
                children.push(
                    button("more", ui::text("…"), Msg::PathMore)
                        .role("yazi.path-more")
                        .title("Show ancestor directories"),
                );
            }
            for index in first.max(2)..=segments.len() {
                children.push(
                    ui::text(">")
                        .key(format!("sep{index}"))
                        .tone("muted")
                        .shrink(0.0)
                        .into(),
                );
                children.push(crumb(index));
            }
            // A Yazi search lists its results under the directory it searched, as Yazi's
            // `(search: subject)` header suffix says.
            if let Some(subject) = self.yz.current.search.as_deref() {
                if cwd == self.yz.current.cwd {
                    children.push(
                        Node::from(ui::badge(format!("search: {subject}")).tone("accent"))
                            .key("search")
                            .role("yazi.path-search")
                            .shrink(0.0),
                    );
                }
            }
            // The tail remains a distinct target even when every segment is truncated.
            children.push(
                button("edit", ui::text(" "), Msg::PathEdit)
                    .role("yazi.path-edit")
                    .title("Edit directory path")
                    .grow(1.0)
                    .shrink(0.0)
                    .prop("min", json!({"w": "3ch"})),
            );
            if self.frame.ui.path_menu {
                let mut ancestors: Vec<N> = (2..first).map(crumb).collect();
                ancestors.push(
                    button("cancel", ui::text("Cancel [Esc]"), Msg::PathCancel)
                        .role("yazi.path-cancel"),
                );
                layer = Some(
                    ui::node("card")
                        .key("ancestors")
                        .role("yazi.path-ancestors")
                        .child(ui::text("Ancestor directories"))
                        .child(ui::col().children(ancestors)),
                );
            }
        }
        let header = ui::node("card")
            .key("path")
            .role("yazi.path-header")
            .title(cwd)
            .child(
                ui::row()
                    .key("content")
                    .role("yazi.path-content")
                    .prop("gap", "xs")
                    .children(children),
            );
        (header, layer)
    }

    /// One reserved line, so editors do not shift the file columns.
    fn input_row(&self) -> N {
        let ui = self.frame.ui;
        let caret = |tone: &str| span(" ▎", tone);
        let bubble = |text: String, tone: &str| spans(vec![span(text, "strong"), caret(tone)]);
        let parts: Option<(&str, N, N, &str)> = match &ui.mode {
            Mode::Command { query, block } => {
                let sigil = if *block { ':' } else { ';' };
                Some((
                    "shell",
                    ui::badge(format!("Shell [{sigil}]")).tone("warn").into(),
                    bubble(format!(" {sigil}{query}"), "warn"),
                    "Cancel [Esc]",
                ))
            }
            Mode::Find { query, previous } => Some((
                "find",
                ui::badge(if *previous { "Find [?]" } else { "Find [/]" })
                    .tone("accent")
                    .into(),
                bubble(format!(" {query}"), "accent"),
                "Cancel [Esc]",
            )),
            Mode::Search { query, content } => Some((
                "search",
                ui::badge(if *content { "Search [S]" } else { "Search [s]" })
                    .tone("accent")
                    .into(),
                bubble(format!(" {query}"), "accent"),
                "Cancel [Esc]",
            )),
            Mode::Filter => Some((
                "filter",
                ui::badge("Filter [f]").tone("accent").into(),
                bubble(format!(" {}", ui.filter_query), "accent"),
                "Clear [Esc]",
            )),
            Mode::Create(query) => Some((
                "create",
                ui::badge("Create [a]").tone("accent").into(),
                bubble(format!(" {query}"), "accent"),
                "Cancel [Esc]",
            )),
            Mode::Rename { editor, .. } => Some((
                "rename",
                ui::badge("Rename [r]").tone("accent").into(),
                field("rename", Field::Rename, None, editor, None),
                "Cancel [Esc]",
            )),
            _ => None,
        };
        let row = match parts {
            Some((kind, label, bubble, cancel)) => ui::row().children([
                label.role("yazi.command-label"),
                bubble.role(format!("yazi.command-bubble.{kind}")),
                Node::from(ui::badge(cancel).tone("muted"))
                    .role("yazi.command-cancel")
                    .on_click(Msg::ClearInput),
            ]),
            None => ui::row().child(match self.frame.notice {
                Some((level, text)) => spans(vec![span(
                    text,
                    if level == Level::Error {
                        "error"
                    } else {
                        "muted"
                    },
                )])
                .role("yazi.notice"),
                None => ui::text(" ").into(),
            }),
        };
        row.key("input")
            .role("yazi.input-row")
            .prop("min", json!({"h": "1.5lines"}))
            .prop("max", json!({"h": "1.5lines"}))
            .into()
    }

    fn columns(&self, focus: &Focus) -> N {
        let lines = self.frame.rows.max(1);
        let yz = self.yz;
        let selected: HashSet<&str> = yz.selected_urls.iter().map(String::as_str).collect();
        let parent = self.parent_column(focus.cwd, lines, &selected);
        let middle_title = format!(
            "{} ({}/{})",
            basename(focus.cwd),
            focus.index,
            focus.files.len()
        );
        let middle = match yz.directory(focus.cwd).filter(|_| !focus.files.is_empty()) {
            Some(folder) => self.list(folder, lines, List::Files, focus.active, &selected),
            None => muted(if focus.cwd == yz.current.cwd {
                "(empty directory)"
            } else {
                "Waiting for Yazi directory metadata..."
            }),
        };
        let middle = card(vec![span(middle_title.clone(), "strong")], vec![middle])
            .role("yazi.column.current")
            .key("current")
            .title(format!("{middle_title}\n{}", focus.cwd));
        let right = self
            .preview_column(focus, lines, &selected)
            .role("yazi.column.preview")
            .key("preview")
            .title(if focus.active.is_some() {
                focus.url.clone()
            } else {
                "Preview".to_owned()
            });
        ui::row()
            .role("yazi.columns")
            .key("columns")
            .children([parent.into(), Node::from(middle), right.into()])
            .into()
    }

    fn parent_column(&self, cwd: &str, lines: u32, selected: &HashSet<&str>) -> ui::Card<Msg> {
        let parent = parent_dir(cwd);
        let folder = parent.and_then(|parent| self.yz.directory(parent));
        let (Some(parent), Some(folder)) = (parent, folder) else {
            let note = if cwd == "/" {
                "(root)"
            } else {
                "Waiting for Yazi directory metadata..."
            };
            return card(vec![span("Parent", "muted")], vec![muted(note)])
                .role("yazi.column.parent")
                .key("parent");
        };
        let title = match basename(parent) {
            "" => "/",
            name => name,
        };
        card(
            vec![span(title, "muted")],
            vec![self.list(folder, lines, List::Parent, Some(basename(cwd)), selected)],
        )
        .role("yazi.column.parent")
        .key("parent")
        .title(parent)
    }

    fn preview_column(&self, focus: &Focus, lines: u32, selected: &HashSet<&str>) -> ui::Card<Msg> {
        let yz = self.yz;
        let Some(active) = focus.active else {
            let note = if focus.cwd == yz.current.cwd {
                "(no file selected)"
            } else {
                "Waiting for Yazi directory metadata..."
            };
            return card(vec![span("Preview", "muted")], vec![muted(note)]);
        };
        match focus.is_dir {
            Some(true) => {
                let folder = yz.directory(&focus.url);
                let count = folder.and_then(|folder| folder.count);
                let heading = vec![
                    span(active, "strong"),
                    span(
                        match count {
                            Some(1) => " · 1 entry".to_owned(),
                            Some(count) => format!(" · {count} entries"),
                            None => " · — entries".to_owned(),
                        },
                        "muted",
                    ),
                ];
                let meta = metadata("directory".to_owned(), focus.hovered);
                let body = match folder {
                    Some(folder)
                        if count != Some(0) && !(count.is_none() && folder.files.is_empty()) =>
                    {
                        let first = folder.files.first().map(String::as_str);
                        self.list(folder, lines, List::Child, first, selected)
                    }
                    Some(_) if count == Some(0) => muted("(empty directory)"),
                    _ => muted("Waiting for Yazi directory preview..."),
                };
                card(heading, vec![meta, body])
            }
            None => card(
                vec![span(active, "strong")],
                vec![muted("Waiting for Yazi file metadata...")],
            ),
            Some(false) => {
                let (kind, content) = match self.frame.preview {
                    Some(Load::Ready(preview)) => {
                        (preview.kind(active), self.preview(preview, active, lines))
                    }
                    _ => (String::new(), ui::col().into()),
                };
                card(
                    vec![span(active, "strong")],
                    vec![
                        metadata(kind, focus.hovered),
                        ui::col()
                            .role("yazi.preview")
                            .prop("gap", "none")
                            .child(content)
                            .into(),
                    ],
                )
            }
        }
    }

    fn preview(&self, preview: &Preview, active: &str, lines: u32) -> N {
        let image = |blob: &str, reserve: u32| -> N {
            ui::image()
                .blob(blob)
                .alt(active)
                .role("yazi.image")
                .prop(
                    "max",
                    json!({"w": 1, "h": format!("{}lines", lines.saturating_sub(reserve).max(1))}),
                )
                .into()
        };
        let skip = |text: &str| -> String {
            let skip = self.yz.preview_skip;
            if skip == 0 {
                return text.to_owned();
            }
            let mut rest = text;
            for _ in 0..skip {
                match rest.find('\n') {
                    Some(at) => rest = &rest[at + 1..],
                    None => break,
                }
            }
            rest.to_owned()
        };
        match preview {
            Preview::Unavailable { limit } => ui::col()
                .role("yazi.preview-unavailable")
                .prop("gap", "none")
                .child(spans(vec![span(
                    format!("Preview unavailable ({} limit)", format::size(Some(*limit))),
                    "warn",
                )]))
                .into(),
            Preview::Archive(items) if items.is_empty() => muted("Archive empty or unreadable"),
            Preview::Archive(items) => {
                let mut entries: Vec<N> = items
                    .iter()
                    .take(18)
                    .map(|item| {
                        let (glyph, tone) = format::virtual_icon(item, item.ends_with('/'));
                        spans(vec![span(glyph, tone), span(item.clone(), "")])
                    })
                    .collect();
                if items.len() > 18 {
                    entries.push(muted(format!(
                        "… and {} more files inside",
                        items.len() - 18
                    )));
                }
                ui::col()
                    .child(spans(vec![span(
                        format!("Archive Contents ({} items)", items.len()),
                        "warn",
                    )]))
                    .child(ui::rule().label(""))
                    .child(ui::col().children(entries))
                    .into()
            }
            Preview::Image { size: 0, .. } => muted("Image file is empty"),
            Preview::Image {
                ext, size, blob, ..
            } => match blob {
                Some(Blob::Id(id)) => ui::col()
                    .role("yazi.image-preview")
                    .prop("gap", "sm")
                    .child(image(id, 12))
                    .child(muted(format!(
                        "Image ({}, {})",
                        ext.to_uppercase(),
                        format::size(Some(*size as u64))
                    )))
                    .into(),
                _ => spans(vec![span("Image preview unavailable", "warn")]),
            },
            Preview::Pdf { meta, cover } => {
                let pages = meta.get("Pages").map_or_else(
                    || "PDF Document".to_owned(),
                    |pages| format!("{pages} pages"),
                );
                let mut head = vec![span("PDF", "badge"), span(format!(" {pages}"), "strong")];
                if let Some(version) = meta.get("PDF version") {
                    head.push(span(format!(" (v{version})"), "muted"));
                }
                let mut children = vec![spans(head)];
                if let Some(title) = meta.get("Title").filter(|title| !title.is_empty()) {
                    children.push(muted(title.clone()));
                }
                match cover {
                    Some(Blob::Id(id)) => {
                        children.push(image(id, 14));
                        children.push(muted("Click image to zoom"));
                    }
                    _ => children.push(muted("Page cover rasterization pending")),
                }
                ui::col()
                    .role("yazi.pdf-preview")
                    .prop("gap", "xs")
                    .children(children)
                    .into()
            }
            Preview::PdfUnavailable => muted("PDF preview unavailable"),
            Preview::Mermaid(text) => ui::md(format!("```mermaid\n{text}\n```")).into(),
            Preview::Markdown(text) => ui::md(skip(text)).into(),
            Preview::Model { fence, text } => {
                // The fence must outlast any backtick run inside the file.
                let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
                let ticks = "`".repeat(longest.max(2) + 1);
                let body = if text.ends_with('\n') {
                    text.clone()
                } else {
                    format!("{text}\n")
                };
                ui::col()
                    .role("yazi.model-preview")
                    .prop("gap", "none")
                    .child(ui::md(format!("{ticks}{fence}\n{body}{ticks}")))
                    .into()
            }
            Preview::Binary { size } => ui::col()
                .child(muted("Binary file (preview suppressed)"))
                .child(spans(vec![span(
                    format!("Size: {}", format::size(Some(*size as u64))),
                    "strong",
                )]))
                .into(),
            Preview::Code { text, lang } => ui::code(skip(text)).lang(*lang).into(),
        }
    }

    fn list(
        &self,
        folder: &Folder,
        lines: u32,
        list: List,
        selected_name: Option<&str>,
        selected: &HashSet<&str>,
    ) -> N {
        let yz = self.yz;
        let files = &folder.files;
        let id = list_id(list);
        let selected_index =
            selected_name.and_then(|name| files.iter().position(|file| file == name));
        // Large listings send a window around the cursor; native lists scroll within it.
        let range = if files.len() <= 150 {
            0..files.len()
        } else {
            let center = selected_index.unwrap_or(0);
            let start = center.saturating_sub(60);
            let end = (start + 120).min(files.len());
            end.saturating_sub(120)..end
        };
        let hovered = yz.hovered.as_ref();
        let items = range.map(|index| {
            let name = &files[index];
            let url = join_path(&folder.cwd, name);
            let is_dir = folder.dirs.get(name).copied().or_else(|| {
                hovered
                    .filter(|hover| hover.url == url)
                    .and_then(|hover| hover.dir)
            });
            let (glyph, tone) = format::file_icon(folder.icon(name), name, is_dir);
            let is_selected = selected.contains(url.as_str());
            let mut label = Vec::with_capacity(5);
            if self.theme.ordinals {
                label.push(span(format!("{:02} ", index + 1), "muted"));
            }
            label.push(span(
                if is_selected { "✓ " } else { "  " },
                if is_selected { "success" } else { "muted" },
            ));
            if list == List::Files {
                let mark = match (yz.marked_urls.contains(&url), yz.mode.as_str()) {
                    (false, _) => "",
                    (true, "unset") => "- ",
                    (true, _) => "+ ",
                };
                label.push(span(mark, "warning"));
            }
            let plain = list == List::Parent && self.theme.plain_parent;
            label.push(span(if plain { String::new() } else { glyph }, tone));
            let strong = list == List::Parent && Some(name.as_str()) == selected_name;
            label.push(span(name.clone(), if strong { "strong" } else { "" }));
            ui::item(label).key(name.clone()).title(url)
        });
        let prefix = format!("{id}.");
        let (on_select, on_activate) = (prefix.clone(), prefix);
        let mut node = ui::list()
            .key(match list {
                List::Parent => "parent_list",
                List::Files => "files_list",
                List::Child => "child_list",
                List::Themes => "list",
            })
            .max_lines(lines)
            .children(items)
            .on_select(move |item| {
                Msg::Select(
                    list,
                    item.strip_prefix(&on_select).unwrap_or(item).to_owned(),
                )
            })
            .on_activate(move |item| {
                Msg::Activate(
                    list,
                    item.strip_prefix(&on_activate).unwrap_or(item).to_owned(),
                )
            });
        if let (Some(name), false) = (selected_name, self.frame.reveal_withheld) {
            if selected_index.is_some() {
                node = node.selected(format!("{id}.{name}"));
            }
        }
        node.into()
    }

    fn dock(&self) -> Vec<N> {
        let mut rows = vec![self.status()];
        if self.theme.tray {
            if let Some(tray) = selection_tray(&self.yz.selected_urls) {
                rows.push(tray);
            }
        }
        rows
    }

    fn status(&self) -> N {
        let focus = focus(self.yz, self.frame.ui);
        let yz = self.yz;
        let mode = &self.frame.ui.mode;
        let label = match mode {
            Mode::Command { .. } => "CMD",
            Mode::Find { .. } => "FIND",
            Mode::Search { .. } => "SEARCH",
            Mode::Tasks => "TASKS",
            Mode::Filter => "FILTER",
            Mode::Create(_) => "CREATE",
            Mode::Rename { .. } => "RENAME",
            _ => match yz.mode.as_str() {
                "select" => "VIS",
                "unset" => "UNSET",
                _ => "NOR",
            },
        };
        let warn = matches!(
            mode,
            Mode::Command { .. } | Mode::Create(_) | Mode::Rename { .. }
        ) || yz.mode != "normal";
        let pill = if warn { ".warn" } else { "" };
        let total = focus.files.len();
        let percent = (focus.index * 100).checked_div(total).unwrap_or(0);
        let size = focus.hovered.and_then(|hover| hover.size);
        let permissions =
            format::permissions(focus.hovered.and_then(|hover| hover.permissions.as_deref()));
        let permission_spans = permissions
            .chars()
            .map(|c| span(c.to_string(), format::permission_tone(c)))
            .collect::<Vec<_>>();
        ui::row()
            .role("yazi.status")
            .prop("gap", "xs")
            .children([
                N::from(
                    ui::row()
                        .role(format!("yazi.status-left{pill}"))
                        .prop("gap", "none")
                        .child(ui::text(label).role("yazi.status-mode"))
                        .child(ui::text(format::size(size)).role("yazi.status-size")),
                ),
                ui::text(vec![span(focus.active.unwrap_or(""), "strong")])
                    .role("yazi.status-name")
                    .into(),
                ui::row()
                    .role("yazi.status-right")
                    .prop("gap", "xs")
                    .child(ui::text(permission_spans).role("yazi.status-permissions"))
                    .child(
                        ui::row()
                            .role(format!("yazi.status-progress{pill}"))
                            .prop("gap", "none")
                            .child(ui::text(format!("{percent}%")).role("yazi.status-percent"))
                            .child(
                                ui::text(format!("{}/{}", focus.index, total.max(1)))
                                    .role("yazi.status-position"),
                            ),
                    )
                    .into(),
            ])
            .into()
    }
}

/// An editable field showing the caret and selection of `editor`.
fn field(
    key: &str,
    field: Field,
    role: Option<&str>,
    editor: &Editor,
    placeholder: Option<&str>,
) -> N {
    let mut node = ui::node("input")
        .key(key)
        .prop("text", &editor.text)
        .prop("sendable", true)
        .grow(1.0)
        .prop("min", json!({"w": 0}))
        .on_focus(Msg::FieldFocus(field))
        .on_edit(move |edit| {
            Msg::FieldEdit(
                field,
                FieldEdit {
                    from: edit.from,
                    to: edit.to,
                    text: edit.text.clone(),
                    cursor: edit.cursor,
                    len: edit.len,
                },
            )
        })
        .on_send(move |text| Msg::FieldSend(field, text.to_owned()))
        .prop("cursor", editor.cursor)
        .prop("anchor", editor.anchor);
    if let Some(role) = role {
        node = node.role(role);
    }
    if let Some(placeholder) = placeholder {
        node = node.prop("placeholder", placeholder);
    }
    node
}

fn selection_tray(urls: &[String]) -> Option<N> {
    if urls.is_empty() {
        return None;
    }
    let mut paths: Vec<&String> = urls.iter().collect();
    paths.sort();
    let mut chips: Vec<N> = paths
        .iter()
        .take(6)
        .map(|path| {
            ui::text(basename(path))
                .key(path.as_str())
                .role("yazi.selection-chip")
                .title(path.as_str())
                .wrap("none")
                .truncate("end")
                .into()
        })
        .collect();
    if paths.len() > 6 {
        chips.push(
            ui::text(format!("+{}", paths.len() - 6))
                .key("overflow")
                .role("yazi.selection-overflow")
                .into(),
        );
    }
    Some(
        ui::row()
            .key("selection")
            .role("yazi.selection-tray")
            .prop("gap", "xs")
            .children(chips)
            .into(),
    )
}
