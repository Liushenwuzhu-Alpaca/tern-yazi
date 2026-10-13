//! Real GUI interaction tests. No state or backend response is synthesized.
//! Run through tests/interaction.sh on Wayland/X11; prerequisites fail rather than skip.
//! Tern 0.7 `serve` runs without a daemon, so these tests explicitly use a daemon-backed window.

use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::env;
use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(40);
const ROWS: &str = "[data-role=\"yazi.column.current\"] .sf-item:is([data-item],[data-id])";
const PARENT_ROWS: &str = "[data-role=\"yazi.column.parent\"] .sf-item:is([data-item],[data-id])";
const CHILD_ROWS: &str = "[data-role=\"yazi.column.preview\"] .sf-item:is([data-item],[data-id])";
const PREVIEW: &str = "[data-role=\"yazi.preview\"]";
const SURFACE_BROWSER: &str = "[data-role=\"yazi.browser\"]";

fn row_selector(scope: &str, name: &str) -> String {
    let list = match scope {
        ROWS => "main.theme.region.browser.columns.current.files_list",
        PARENT_ROWS => "main.theme.region.browser.columns.parent.parent_list",
        CHILD_ROWS => "main.theme.region.browser.columns.preview.child_list",
        _ => unreachable!("Unknown authoritative list scope"),
    };
    let id = serde_json::to_string(&format!("{list}.{name}")).unwrap();
    format!("{scope}:is([data-item={id}],[data-id={id}])")
}

type TestResult<T> = Result<T, String>;

fn quote(value: impl AsRef<str>) -> String {
    format!("'{}'", value.as_ref().replace('\'', "'\\''"))
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn names(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn hover(value: &Value) -> String {
    value["hovered"]["url"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn selected(value: &Value) -> BTreeSet<String> {
    names(value, "selected_urls").into_iter().collect()
}

/// `YYYY-MM-DD HH:MM`, the preview's local modification time.
fn is_local_minute(text: &str) -> bool {
    let text = text.trim();
    text.len() == 16
        && text.char_indices().all(|(at, c)| match at {
            4 | 7 => c == '-',
            10 => c == ' ',
            13 => c == ':',
            _ => c.is_ascii_digit(),
        })
}

fn ui_text(value: &Value) -> String {
    fn visit(value: &Value, parts: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for key in ["text", "title", "alt", "value"] {
                    if let Some(Value::String(part)) = object.get(key) {
                        parts.push(part.clone());
                    }
                }
                for (key, child) in object {
                    if matches!(key.as_str(), "children" | "nodes" | "tree") {
                        visit(child, parts);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    visit(item, parts);
                }
            }
            _ => {}
        }
    }
    let mut parts = Vec::new();
    visit(value, &mut parts);
    parts.join("\n")
}

fn nodes(value: &Value) -> Vec<&Value> {
    value["nodes"]
        .as_array()
        .map(|v| v.iter().collect())
        .unwrap_or_default()
}

fn metadata_rect(value: &Value) -> TestResult<[f64; 4]> {
    let found = nodes(value);
    if found.len() != 1 {
        return Err(format!("Expected one metadata layout target, got {value}"));
    }
    let rect = &found[0]["rect"];
    let result = [
        rect[0].as_f64().ok_or("Missing metadata rect x")?,
        rect[1].as_f64().ok_or("Missing metadata rect y")?,
        rect[2].as_f64().ok_or("Missing metadata rect width")?,
        rect[3].as_f64().ok_or("Missing metadata rect height")?,
    ];
    if result[2] <= 0.0 || result[3] <= 0.0 {
        return Err(format!("Metadata layout target is not visible: {value}"));
    }
    Ok(result)
}

fn metadata_contains(outer: [f64; 4], inner: [f64; 4]) -> bool {
    inner[0] >= outer[0] - 1.0
        && inner[1] >= outer[1] - 1.0
        && inner[0] + inner[2] <= outer[0] + outer[2] + 1.0
        && inner[1] + inner[3] <= outer[1] + outer[3] + 1.0
}

fn metadata_above(first: [f64; 4], second: [f64; 4]) -> bool {
    first[1] + first[3] <= second[1] + 1.0
}

fn metadata_before(first: [f64; 4], second: [f64; 4]) -> bool {
    first[0] + first[2] <= second[0] + 1.0
        && first[1] < second[1] + second[3]
        && second[1] < first[1] + first[3]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Process {
    pid: i32,
    started: u64,
}

fn process(pid: i32) -> Option<(Process, i32, char)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(") ")?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some((
        Process {
            pid,
            started: fields.get(19)?.parse().ok()?,
        },
        fields.get(1)?.parse().ok()?,
        fields.first()?.chars().next()?,
    ))
}

impl Process {
    fn alive(self) -> bool {
        process(self.pid).is_some_and(|(current, _, state)| current == self && state != 'Z')
    }
    fn signal(self, signal: i32) {
        if self.alive() {
            // Identity is checked before signaling: never target a reused PID.
            unsafe {
                libc::kill(self.pid, signal);
            }
        }
    }
}

fn descendants(root: Process) -> BTreeSet<Process> {
    let mut all = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<i32>().ok())
            {
                if let Some(info) = process(pid) {
                    all.push(info);
                }
            }
        }
    }
    let mut result = BTreeSet::from([root]);
    loop {
        let before = result.len();
        for (child, parent, _) in &all {
            if result.iter().any(|item| item.pid == *parent) {
                result.insert(*child);
            }
        }
        if result.len() == before {
            return result;
        }
    }
}

fn tty_size(program: Process) -> TestResult<(u16, u16)> {
    let tty = File::open(format!("/proc/{}/fd/0", program.pid)).map_err(|e| e.to_string())?;
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe { libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ, &mut size) } != 0 {
        return Err(format!(
            "Cannot inspect real PTY dimensions: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok((size.ws_row, size.ws_col))
}

fn pane_info(state: &Value, pane: u64) -> Option<&Value> {
    state["panes"]
        .as_array()?
        .iter()
        .find(|info| info["pane"].as_u64() == Some(pane))
}

fn pane_ids(state: &Value) -> BTreeSet<u64> {
    state["panes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|info| info["pane"].as_u64())
        .collect()
}

fn tab_layout(state: &Value, tab: u64) -> Option<&Value> {
    state["layouts"]
        .as_array()?
        .iter()
        .find(|layout| layout["tab"].as_u64() == Some(tab))
}

#[derive(Clone)]
struct Client {
    cid: String,
    owner: u64,
    supervisor: Process,
    backend: Process,
    cwd: PathBuf,
    owner_shell: Option<Process>,
    original_tab: Option<u64>,
    original_layout: Value,
    original_panes: BTreeSet<u64>,
}

struct Harness {
    root: PathBuf,
    tern: PathBuf,
    repo: PathBuf,
    daemon_process: Option<Process>,
    renderer: Option<Child>,
    owned: BTreeSet<Process>,
    clients: Vec<Client>,
    case: &'static str,
    success: bool,
    last: String,
}

impl Harness {
    fn new() -> TestResult<Self> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let root = env::temp_dir().join(format!("tyi-{}-{unique:x}", std::process::id()));
        DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|e| e.to_string())?;
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("Missing repository parent")?
            .to_path_buf();
        let tern_name = env::var_os("TERN").unwrap_or_else(|| "tern".into());
        let tern = if Path::new(&tern_name).components().count() > 1 {
            PathBuf::from(&tern_name)
        } else {
            env::split_paths(&env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join(&tern_name))
                .find(|path| path.is_file())
                .ok_or("Required Tern binary is absent; this interaction suite cannot be skipped")?
        };
        let tern = fs::canonicalize(tern)
            .map_err(|error| format!("Cannot resolve Tern binary: {error}"))?;
        let mut harness = Self {
            root,
            tern,
            repo,
            daemon_process: None,
            renderer: None,
            owned: BTreeSet::new(),
            clients: Vec::new(),
            case: "runtime prerequisites",
            success: false,
            last: String::new(),
        };
        harness.setup()?;
        Ok(harness)
    }

    fn path(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }
    fn inbox(&self) -> PathBuf {
        self.path("runtime/tern-yazi")
    }
    fn snapshot(&self, client: &Client) -> Value {
        self.snapshot_id(&client.cid)
    }

    fn snapshot_id(&self, cid: &str) -> Value {
        let Some(mut state) = read_json(&self.inbox().join(format!("state-{cid}.json"))) else {
            return Value::Null;
        };
        let Some(mut listing) = read_json(&self.inbox().join(format!("listing-{cid}.json"))) else {
            return Value::Null;
        };
        if !state["listing_epoch"].is_string()
            || state["listing_epoch"] != listing["epoch"]
            || !state["listing_revision"].is_number()
            || state["listing_revision"] != listing["revision"]
        {
            return Value::Null;
        }
        let (Some(dynamic), Some(immutable)) = (state.as_object_mut(), listing.as_object_mut())
        else {
            return Value::Null;
        };
        for field in [
            "cwd",
            "files",
            "search",
            "file_dirs",
            "file_icons",
            "file_count",
            "parent",
            "preview",
            "selected_urls",
            "marked_urls",
        ] {
            if let Some(value) = immutable.remove(field) {
                dynamic.insert(field.into(), value);
            }
        }
        let apply_icons = |scope: &mut Value, overrides: Value| {
            let Value::Object(overrides) = overrides else {
                return;
            };
            let Some(scope) = scope.as_object_mut() else {
                return;
            };
            if let Some(icons) = scope
                .entry("file_icons")
                .or_insert_with(|| json!({}))
                .as_object_mut()
            {
                icons.extend(overrides);
            }
        };
        for (scope, field) in [
            ("", "icon_overrides"),
            ("parent", "parent_icon_overrides"),
            ("preview", "preview_icon_overrides"),
        ] {
            let overrides = state
                .as_object_mut()
                .and_then(|state| state.remove(field))
                .unwrap_or(Value::Null);
            if scope.is_empty() {
                apply_icons(&mut state, overrides);
            } else if let Some(value) = state.get_mut(scope) {
                apply_icons(value, overrides);
            }
        }
        state
    }

    fn command(&self, program: impl AsRef<Path>) -> Command {
        let mut command = Command::new(program.as_ref());
        if let (Some(display), Some(runtime)) = (
            env::var_os("WAYLAND_DISPLAY"),
            env::var_os("XDG_RUNTIME_DIR"),
        ) {
            if Path::new(&display).is_relative() {
                command.env("WAYLAND_DISPLAY", Path::new(&runtime).join(display));
            }
        }
        // Inherited pane identities, integration and config must not reach this daemon.
        for (key, _) in env::vars_os() {
            if key.to_string_lossy().starts_with("TERN_")
                || key.to_string_lossy().starts_with("YAZI_")
            {
                command.env_remove(key);
            }
        }
        command
            .current_dir(&self.root)
            .env("TERN_CONFIG_DIR", self.path("config"))
            .env("TERN_DAEMON_SOCKET", self.path("daemon.sock"))
            .env("XDG_RUNTIME_DIR", self.path("runtime"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("XDG_CONFIG_HOME", self.path("xdg-config"))
            .env("YAZI_CONFIG_HOME", self.path("yazi"))
            .env("SHELL", self.path("bin/shell"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    text(&self.path("bin")),
                    env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("BASH_ENV")
            .env_remove("ENV")
            .env_remove("ZDOTDIR");
        command
    }

    fn execute(&self, program: impl AsRef<Path>, args: &[String]) -> TestResult<String> {
        let mut command = self.command(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| format!("Cannot execute runtime command: {e}"))?;
        let mut stdout = child.stdout.take().ok_or("Missing stdout pipe")?;
        let mut stderr = child.stderr.take().ok_or("Missing stderr pipe")?;
        let out = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
            bytes
        });
        let err = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        });
        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Runtime command exceeded its 20-second deadline".into());
            }
            thread::sleep(POLL);
        };
        let stdout = String::from_utf8_lossy(&out.join().map_err(|_| "stdout reader panicked")?)
            .into_owned();
        let stderr = String::from_utf8_lossy(&err.join().map_err(|_| "stderr reader panicked")?)
            .into_owned();
        if !status.success() {
            return Err(format!(
                "Runtime command exited {status}: {stderr}\n{stdout}"
            ));
        }
        Ok(stdout)
    }

    fn cli(&self, args: &[&str]) -> TestResult<String> {
        self.execute(
            &self.tern,
            &args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )
    }

    fn ctl(&self, verb: &str, args: &[&str]) -> TestResult<Value> {
        let mut command = vec![
            "ctl".to_owned(),
            "--control".to_owned(),
            text(&self.path("control.sock")),
            verb.to_owned(),
        ];
        command.extend(args.iter().map(|s| (*s).to_owned()));
        let output = self.execute(&self.tern, &command)?;
        let value: Value = serde_json::from_str(&output)
            .map_err(|e| format!("Invalid ctl {verb} response: {e}; {output}"))?;
        if value["ok"] == false {
            return Err(format!("ctl {verb} rejected the action: {value}"));
        }
        Ok(value)
    }

    fn tree(&self, selector: &str) -> TestResult<Value> {
        let selectors = [selector];
        self.ctl("tree", if selector.is_empty() { &[] } else { &selectors })
    }

    fn key(&self, key: &str) -> TestResult<()> {
        self.ctl("key", &[key]).map(|_| ())
    }
    fn focus(&self, pane: u64) -> TestResult<()> {
        self.cli(&["focus", &pane.to_string()]).map(|_| ())
    }
    fn paste(&self, pane: u64, input: &str) -> TestResult<()> {
        self.cli(&["send", &pane.to_string(), "paste", input])
            .map(|_| ())
    }

    /// Empties the open input line: Ctrl+A moves to the start, Ctrl+K kills to the end.
    fn clear_line(&self, pane: u64) -> TestResult<()> {
        for chord in ["ctrl+a", "ctrl+k"] {
            self.cli(&["send", &pane.to_string(), "keys", chord])?;
        }
        Ok(())
    }

    /// The window's panes, tabs and layout trees, as the fixture plugin observes them.
    fn layout_state(&self) -> TestResult<Value> {
        self.ctl("plugins", &["run", "plugin.layout-fixture.inspect"])?;
        read_json(&self.path("fixture/observed.json"))
            .ok_or("Real window fixture returned no observation".into())
    }

    /// The surface draws in the launching pane: startup never adds a pane.
    fn startup_panes(&self, original: &BTreeSet<u64>) -> TestResult<()> {
        // Read app state only: never send an event or command to wake startup.
        let state = self.ctl("state", &[])?;
        let panes = state["panes"]
            .as_array()
            .ok_or("Startup observation has no real panes")?;
        for pane in panes {
            let id = pane["id"].as_u64().ok_or("Startup pane has no actual ID")?;
            if !original.contains(&id) {
                return Err(format!("Startup created an extra pane: {state}"));
            }
        }
        Ok(())
    }

    fn shell_prompt(&mut self, owner: u64) -> TestResult<()> {
        self.wait("original real shell reaches its foreground prompt", |h| {
            let state = h.layout_state()?;
            Ok(pane_info(&state, owner)
                .filter(|info| info["at_prompt"] == true)
                .map(|_| ()))
        })
    }

    fn wait<T>(
        &mut self,
        expected: &str,
        mut observe: impl FnMut(&Self) -> TestResult<Option<T>>,
    ) -> TestResult<T> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            match observe(self) {
                Ok(Some(value)) => return Ok(value),
                Ok(None) => self.last = "condition not yet satisfied".into(),
                Err(error) => self.last = error,
            }
            if let Some(child) = self.renderer.as_mut() {
                if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                    return Err(format!(
                        "Expected {expected}; owned Tern window exited {status}; logs at {}",
                        text(&self.root)
                    ));
                }
            }
            if Instant::now() >= deadline {
                return Err(format!("Expected {expected}; actual: {}", self.last));
            }
            thread::sleep(POLL);
        }
    }

    fn expect_state(
        &mut self,
        client: &Client,
        expected: &str,
        predicate: impl Fn(&Value) -> bool,
    ) -> TestResult<Value> {
        self.wait(expected, |h| {
            let value = h.snapshot(client);
            if predicate(&value) {
                Ok(Some(value))
            } else {
                Err(format!("snapshot {}", value))
            }
        })
    }

    fn expect_ui(
        &mut self,
        selector: &str,
        expected: &str,
        predicate: impl Fn(&Value) -> bool,
    ) -> TestResult<Value> {
        self.wait(expected, |h| {
            let value = h.tree(selector)?;
            if predicate(&value) {
                Ok(Some(value))
            } else {
                Err(format!(
                    "visible UI: {}; element data: {value}",
                    ui_text(&value)
                ))
            }
        })
    }

    fn expect_row_icon(
        &mut self,
        client: &Client,
        folder: Option<&str>,
        selector: &str,
        name: &str,
        expected: Value,
    ) -> TestResult<Value> {
        let description = format!(
            "Yazi exports {expected} for {name} in {}",
            folder.unwrap_or("current")
        );
        let snapshot = self.expect_state(client, &description, |s| {
            let listing = folder.map_or(s, |key| &s[key]);
            names(listing, "files").iter().any(|entry| entry == name)
                && listing["file_icons"][name] == expected
        })?;
        // Compare the actual row with the authoritative value just exported.
        // Trim the blank selection marker only; glyph, separator and name stay exact.
        // None of these fixture names or configured glyphs begins with whitespace.
        // Blank/false icons must not insert a generic glyph.
        let listing = folder.map_or(&snapshot, |key| &snapshot[key]);
        let glyph = listing["file_icons"][name].as_str().unwrap_or_default();
        let label = if glyph.is_empty() {
            name.to_owned()
        } else {
            format!("{glyph} {name}")
        };
        self.expect_ui(
            &row_selector(selector, name),
            &format!("row exactly matches {label:?}"),
            |v| {
                nodes(v).iter().any(|node| {
                    node["text"]
                        .as_str()
                        .is_some_and(|text| text.trim_start() == label)
                })
            },
        )?;
        Ok(snapshot)
    }

    fn expect_metadata_text(&mut self, selector: &str, expected: &str) -> TestResult<Value> {
        self.expect_ui(
            selector,
            &format!("exact metadata text {expected:?}"),
            |v| {
                let found = nodes(v);
                found.len() == 1 && found[0]["text"].as_str().unwrap_or_default().trim() == expected
            },
        )
    }

    fn expect_metadata_selection(&mut self, selector: &str, path: &Path) -> TestResult<()> {
        let expected = text(path);
        self.expect_ui(
            selector,
            "selected row retains its full authoritative path",
            |v| {
                let found = nodes(v);
                found.len() == 1
                    && found[0]["title"].as_str() == Some(expected.as_str())
                    && metadata_rect(v).is_ok()
            },
        )?;
        Ok(())
    }

    fn expect_metadata_header_layout(&mut self, column: &str, content: &str) -> TestResult<()> {
        let card_selector = format!("[data-role=\"{column}\"]");
        let head_selector = format!("{card_selector} > .sf-card-head");
        let content_selector = format!("{card_selector} {content}");
        self.wait(
            "real card heading stays above its own bounded content",
            |h| {
                let card = metadata_rect(&h.tree(&card_selector)?)?;
                let head = metadata_rect(&h.tree(&head_selector)?)?;
                let content = metadata_rect(&h.tree(&content_selector)?)?;
                if metadata_contains(card, head)
                    && metadata_contains(card, content)
                    && metadata_above(head, content)
                {
                    Ok(Some(()))
                } else {
                    Err(format!(
                        "Column {column}: card {card:?}, head {head:?}, content {content:?}"
                    ))
                }
            },
        )
    }

    fn expect_metadata_file_layout(&mut self) -> TestResult<()> {
        self.wait("footer [mode|size] name and permissions [percent|position] capsules have real ordered bounds", |h| {
            let footer = metadata_rect(&h.tree("[data-role=\"yazi.status\"]")?)?;
            let left = metadata_rect(&h.tree("[data-role^=\"yazi.status-left\"]")?)?;
            let mode = metadata_rect(&h.tree("[data-role=\"yazi.status-mode\"]")?)?;
            let size = metadata_rect(&h.tree("[data-role=\"yazi.status-size\"]")?)?;
            let name = metadata_rect(&h.tree("[data-role=\"yazi.status-name\"]")?)?;
            let right = metadata_rect(&h.tree("[data-role=\"yazi.status-right\"]")?)?;
            let permissions = metadata_rect(&h.tree("[data-role=\"yazi.status-permissions\"]")?)?;
            let progress = metadata_rect(&h.tree("[data-role^=\"yazi.status-progress\"]")?)?;
            let percent = metadata_rect(&h.tree("[data-role=\"yazi.status-percent\"]")?)?;
            let position = metadata_rect(&h.tree("[data-role=\"yazi.status-position\"]")?)?;
            // Capsule segments touch: no gap between mode/size or percent/position.
            let joined = |a: [f64; 4], b: [f64; 4]| (b[0] - (a[0] + a[2])).abs() < 1.0;
            if [left, name, right].into_iter().all(|r| metadata_contains(footer, r))
                && [mode, size].into_iter().all(|r| metadata_contains(left, r))
                && [permissions, progress].into_iter().all(|r| metadata_contains(right, r))
                && [percent, position].into_iter().all(|r| metadata_contains(progress, r))
                && joined(mode, size)
                && joined(percent, position)
                && metadata_before(left, name)
                && metadata_before(name, right)
                && name[0] + name[2] >= right[0] - 16.0
                && right[0] + right[2] >= footer[0] + footer[2] - 1.0
                && metadata_before(permissions, progress)
            {
                Ok(Some(()))
            } else {
                Err(format!("Footer {footer:?}: left {left:?} (mode {mode:?}, size {size:?}), name {name:?}, right {right:?}, permissions {permissions:?}, progress {progress:?} (percent {percent:?}, position {position:?})"))
            }
        })?;
        self.expect_metadata_header_layout("yazi.column.parent", ".sf-list-scroll.max")?;
        self.expect_metadata_header_layout("yazi.column.current", ".sf-list-scroll.max")?;
        self.expect_metadata_header_layout(
            "yazi.column.preview",
            "[data-role=\"yazi.preview-metadata\"]",
        )?;
        self.wait(
            "fixed preview metadata bounds the independent content scroller",
            |h| {
                let card = metadata_rect(&h.tree("[data-role=\"yazi.column.preview\"]")?)?;
                let metadata = metadata_rect(&h.tree("[data-role=\"yazi.preview-metadata\"]")?)?;
                let preview = metadata_rect(&h.tree(PREVIEW)?)?;
                if [metadata, preview]
                    .into_iter()
                    .all(|r| metadata_contains(card, r))
                    && metadata_above(metadata, preview)
                {
                    Ok(Some(()))
                } else {
                    Err(format!(
                        "Preview card {card:?}: metadata {metadata:?}, content {preview:?}"
                    ))
                }
            },
        )
    }

    fn expect_file_metadata(
        &mut self,
        client: &Client,
        name: &str,
        size: (u64, &str),
        permissions: &str,
        language: &str,
    ) -> TestResult<()> {
        let (bytes, size_label) = size;
        let path = text(&client.cwd.join(name));
        let snapshot = self.expect_state(
            client,
            "authoritative hovered file metadata matches the private fixture",
            |s| {
                hover(s) == path
                    && s["hovered"]["dir"] == false
                    && s["hovered"]["size"].as_u64() == Some(bytes)
                    && s["hovered"]["permissions"].as_str() == Some(permissions)
            },
        )?;
        for role in ["yazi.status-size", "yazi.preview-size"] {
            self.expect_metadata_text(&format!("[data-role=\"{role}\"]"), size_label)?;
        }
        for role in ["yazi.status-permissions", "yazi.preview-permissions"] {
            self.expect_metadata_text(&format!("[data-role=\"{role}\"]"), permissions)?;
        }
        self.expect_metadata_text("[data-role=\"yazi.status-name\"]", name)?;
        self.expect_metadata_text("[data-role=\"yazi.column.preview\"] > .sf-card-head", name)?;
        let parent_name = self
            .root
            .file_name()
            .ok_or("Private fixture root has no basename")?
            .to_string_lossy()
            .into_owned();
        self.expect_metadata_text(
            "[data-role=\"yazi.column.parent\"] > .sf-card-head",
            &parent_name,
        )?;
        let files = names(&snapshot, "files");
        let index = files
            .iter()
            .position(|n| n == name)
            .ok_or("Hovered fixture is absent from authoritative files")?
            + 1;
        self.expect_metadata_text(
            "[data-role=\"yazi.column.current\"] > .sf-card-head",
            &format!("metadata ({index}/{})", files.len()),
        )?;
        self.expect_metadata_text("[data-role=\"yazi.status-mode\"]", "NOR")?;
        self.expect_metadata_text(
            "[data-role=\"yazi.status-percent\"]",
            &format!("{}%", index * 100 / files.len()),
        )?;
        self.expect_metadata_text(
            "[data-role=\"yazi.status-position\"]",
            &format!("{index}/{}", files.len()),
        )?;
        self.expect_metadata_text(
            "[data-role=\"yazi.preview-metadata\"] > .sf-text:not([data-role])",
            language,
        )?;
        self.expect_ui(
            "[data-role=\"yazi.preview-metadata\"] > *",
            "fixed metadata contains only semantic language, size, mtime and permissions",
            |v| nodes(v).len() == 4,
        )?;
        self.expect_ui(
            "[data-role=\"yazi.preview-mtime\"]",
            "file preview shows its local modification time",
            |v| nodes(v).len() == 1 && is_local_minute(&ui_text(v)),
        )?;
        self.expect_ui(
            "[data-role=\"yazi.column.preview\"]",
            "file header/preview have no Source/Info tabs or line-count badges",
            |v| {
                let label = ui_text(v);
                !label.contains("Source") && !label.contains("Info") && !label.contains(" lines")
            },
        )?;
        self.expect_ui(
            "[data-role=\"yazi.status\"]",
            "footer never duplicates selection counters",
            |v| {
                let label = ui_text(v);
                !label.contains(" sel") && !label.contains(" selected")
            },
        )?;
        self.expect_metadata_selection(&format!("{ROWS}.sel"), &client.cwd.join(name))?;
        self.expect_metadata_selection(&format!("{PARENT_ROWS}.sel"), &client.cwd)?;
        self.expect_metadata_file_layout()
    }

    fn write_fixture(&self, path: &str, contents: impl AsRef<[u8]>) -> TestResult<()> {
        fs::write(self.path(path), contents).map_err(|e| e.to_string())
    }

    fn executable(&self, path: &str, contents: &str) -> TestResult<()> {
        self.write_fixture(path, contents)?;
        fs::set_permissions(self.path(path), fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())
    }

    fn setup(&mut self) -> TestResult<()> {
        if !["WAYLAND_DISPLAY", "DISPLAY"]
            .iter()
            .any(|key| env::var_os(key).is_some_and(|v| !v.is_empty()))
        {
            return Err("A real Wayland/X11 display is required: Tern 0.7 serve cannot attach the managed daemon; no headless fallback or skipped test is permitted".into());
        }
        for binary in ["/bin/bash", "/usr/bin/yazi"] {
            if !Path::new(binary).is_file() {
                return Err(format!("Required real runtime is missing: {binary}"));
            }
        }
        for dir in [
            "config",
            "runtime",
            "data",
            "cache",
            "xdg-config",
            "yazi/plugins",
            "bin",
            "libexec",
            "shots",
            "files/child",
            "files/copydest",
            "files/cutdest",
            "other",
            "home/Downloads",
            "home/.config",
            "fixture",
        ] {
            fs::create_dir_all(self.path(dir)).map_err(|e| e.to_string())?;
        }
        fs::set_permissions(self.path("runtime"), fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        symlink(
            self.repo.join("yazi-plugin/tern.yazi"),
            self.path("yazi/plugins/tern.yazi"),
        )
        .map_err(|e| e.to_string())?;
        self.write_fixture("yazi/init.lua", "require(\"tern\"):setup()\n")?;
        self.write_fixture("yazi/yazi.toml", "[mgr]\nshow_hidden = false\nsort_by = \"alphabetical\"\nsort_sensitive = true\nsort_dir_first = true\n")?;
        // Tern may hydrate a shell from its login environment. Re-establish the
        // fixture's private paths in Bash itself before testing typed commands.
        let mut bashrc = String::new();
        for (key, value) in [
            (
                "PATH",
                format!(
                    "{}:{}",
                    text(&self.path("bin")),
                    env::var("PATH").unwrap_or_default()
                ),
            ),
            ("YAZI_CONFIG_HOME", text(&self.path("yazi"))),
            // Surface diagnostics; the tty belongs to the surface.
            ("TERN_YAZI_LOG", text(&self.path("surface.log"))),
            ("TERN_CONFIG_DIR", text(&self.path("config"))),
            ("TERN_DAEMON_SOCK", text(&self.path("daemon.sock"))),
            ("XDG_CONFIG_HOME", text(&self.path("config"))),
            ("XDG_RUNTIME_DIR", text(&self.path("runtime"))),
            ("XDG_DATA_HOME", text(&self.path("data"))),
            ("XDG_CACHE_HOME", text(&self.path("cache"))),
            // The g-prefix jumps (`gh`, `gc`, `gd`) are relative to the home directory.
            ("HOME", text(&self.path("home"))),
            ("EDITOR", text(&self.path("bin/fake-editor"))),
        ] {
            bashrc.push_str(&format!("export {key}={}\n", quote(value)));
        }
        bashrc.push_str("PROMPT_COMMAND='status=$?; printf \"\\033]133;D;%s\\007\" \"$status\"'\nPS1='\\[\\e]133;A\\a\\]\\w\\$ \\[\\e]133;B\\a\\]'\ntrap 'printf \"\\033]133;C\\007\"' DEBUG\n");
        self.write_fixture("bashrc", bashrc)?;
        self.executable(
            "bin/shell",
            &format!(
                "#!/bin/sh\n# Keep the private prompt hooks in a non-login interactive shell.\nwhile [ \"$#\" -gt 0 ]; do\n    case \"$1\" in -l|--login) shift ;; *) break ;; esac\ndone\nexec /bin/bash --noprofile --rcfile {} -i \"$@\"\n" ,
                quote(text(&self.path("bashrc")))
            ),
        )?;
        // The bulk rename editor: it prefixes every line of the file Yazi lends it, then reads
        // the tty in raw mode for a moment like a real editor, which consumes the terminal
        // answers that arrive while the pane is lent.
        self.executable(
            "bin/fake-editor",
            "#!/bin/sh\nsed -i 's/^/x-/' \"$1\"\n",
        )?;
        self.executable("bin/yazi", &format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > {}/supervisor-$$.pid\nexec {} --real /usr/bin/yazi -- \"$@\"\n", quote(text(&self.root)), quote(env!("CARGO_BIN_EXE_tern-yazi-launch"))))?;
        // Match the installed prefix: only bin is on PATH, helper and original metadata live in libexec.
        symlink(
            env!("CARGO_BIN_EXE_tern-yazi-launch"),
            self.path("libexec/tern-yazi-launch"),
        )
        .map_err(|e| e.to_string())?;
        symlink("/usr/bin/yazi", self.path("libexec/yazi-original")).map_err(|e| e.to_string())?;
        self.write_fixture(
            "libexec/tern-yazi-launch.real",
            format!("{}\n", text(&self.path("libexec/yazi-original"))),
        )?;
        for (path, data) in [
            ("files/Alpha.txt", "UPPER_ALPHA_CONTENT\n"),
            ("files/alpha.txt", "lower_alpha_content\n"),
            ("files/beta.txt", "BETA_CONTENT\n"),
            ("files/copy.txt", "COPY_ORIGINAL\n"),
            ("files/cut.txt", "CUT_ORIGINAL\n"),
            ("files/notes.md", "# Markdown\n\n**MARKDOWN_PREVIEW_CONTENT**\n"),
            ("files/image.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"40\" height=\"40\"><rect width=\"40\" height=\"40\" fill=\"red\"/></svg>"),
            ("files/雪's 100%.txt", "UNICODE_QUOTE_PERCENT_CONTENT\n"),
            ("files/trash-a.txt", "TRASH_A\n"),
            ("files/trash-b.txt", "TRASH_B\n"),
            ("files/.hidden.txt", "HIDDEN_CONTENT\n"),
            ("files/child/inside.txt", "CHILD_CONTENT\n"),
            ("other/other.txt", "OTHER_CLIENT_CONTENT\n"),
        ] { self.write_fixture(path, data)?; }
        File::create(self.path("files/oversized.txt"))
            .and_then(|file| file.set_len(1024 * 1024 + 1))
            .map_err(|e| e.to_string())?;
        self.execute(&self.tern, &["--version".into()])?;
        self.execute("/usr/bin/yazi", &["--version".into()])?;
        self.execute("ya", &["--version".into()])?;
        self.cli(&["plugin", "link", &text(&self.repo)])?;
        self.write_fixture("fixture/plugin.toml", "schema = 1\nid = \"layout-fixture\"\nname = \"Layout Inspection Fixture\"\nversion = \"1\"\nwindow = \"window.luau\"\n")?;
        let fixture_root =
            serde_json::to_string(&text(&self.path("fixture"))).map_err(|e| e.to_string())?;
        self.write_fixture(
            "fixture/window.luau",
            format!("local ROOT = {fixture_root}\n")
                + r#"
-- Observe the real window through public APIs; never synthesize pane state.
local function observed_tree(node)
    if not node then return tern.json.null end
    return { pane = node.pane, split = node.split, ratio = node.ratio,
        first = observed_tree(node[1]), second = observed_tree(node[2]) }
end
tern.command({ id = "inspect", title = "Inspect the window layout", run = function(cx)
    local layouts = {}
    for _, tab in ipairs(cx.session:tabs()) do
        local layout = cx.session:layout(tab.id)
        table.insert(layouts, { tab = layout.tab, root = observed_tree(layout.root),
            floats = layout.floats, zoomed = layout.zoomed, focus = layout.focus })
    end
    tern.fs.write(ROOT .. "/observed.json", tern.json.encode({ panes = cx.session:panes(),
        tabs = cx.session:tabs(), layouts = layouts, focused = cx.session:focused() }))
end })
"#,
        )?;
        self.cli(&["plugin", "link", &text(&self.path("fixture"))])?;
        let renderer_log = File::create(self.path("renderer.log")).map_err(|e| e.to_string())?;
        let hook = env::var_os("TERN_YAZI_TEST_WINDOW_HOOK");
        let mut renderer = if hook.is_some() {
            let mut command = self.command("/bin/sh");
            command.args([
                "-c",
                "kill -STOP $$; exec \"$@\"",
                "tern-yazi-test-renderer",
                &text(&self.tern),
            ]);
            command
        } else {
            self.command(&self.tern)
        };
        renderer
            .args([
                "--control",
                &text(&self.path("control.sock")),
                "--dir",
                &text(&self.path("files")),
            ])
            .stdin(Stdio::piped())
            .stdout(renderer_log.try_clone().map_err(|e| e.to_string())?)
            .stderr(renderer_log)
            .process_group(0);
        self.renderer = Some(
            renderer
                .spawn()
                .map_err(|e| format!("Cannot start owned daemon-backed Tern window: {e}"))?,
        );
        if let Some(hook) = hook.as_ref() {
            let pid = self.renderer.as_ref().ok_or("Missing owned renderer")?.id();
            self.wait("owned renderer is stopped before creating a window", |_| {
                Ok(process(pid as i32)
                    .filter(|(_, _, state)| *state == 'T')
                    .map(|_| ()))
            })?;
            self.execute(
                Path::new(hook),
                &[pid.to_string(), text(&self.root), "start".into()],
            )?;
            if unsafe { libc::kill(pid as i32, libc::SIGCONT) } != 0 {
                return Err(format!(
                    "Cannot release owned renderer: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        self.daemon_process = Some(self.wait(
            "autostarted private daemon process and socket",
            |h| {
                Ok(h.path("daemon.sock")
                    .exists()
                    .then(|| h.private_daemon())
                    .flatten())
            },
        )?);
        if hook.is_some() {
            self.wait("owned control endpoint", |h| {
                Ok(h.path("control.sock").exists().then_some(()))
            })?;
            // Initialize the viewport before launching Yazi; this is not
            // an artificial wake of the managed backend startup path.
            self.ctl("shot", &["00-initial-owned-window"])?;
        }
        self.wait("real foreground Bash prompt", |h| {
            let state = h.ctl("state", &[])?;
            if state["focused"]["prompt"] == true {
                Ok(Some(()))
            } else {
                Err(format!(
                    "initial window state: {}",
                    json!({"focused": state["focused"], "tabs": state["tabs"]})
                ))
            }
        })?;
        Ok(())
    }

    fn private_daemon(&self) -> Option<Process> {
        let socket = text(&self.path("daemon.sock"));
        for entry in fs::read_dir("/proc").ok()?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<i32>().ok())
            else {
                continue;
            };
            let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            let args: Vec<_> = cmdline.split(|byte| *byte == 0).collect();
            if args.contains(&b"daemon".as_slice()) && args.contains(&socket.as_bytes()) {
                if let Some((daemon, _, _)) = process(pid) {
                    return Some(daemon);
                }
            }
        }
        None
    }

    fn pid_markers(&self) -> BTreeSet<i32> {
        fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.strip_prefix("supervisor-")?
                    .strip_suffix(".pid")?
                    .parse()
                    .ok()
            })
            .collect()
    }

    /// Opens a new tab running the private shell in `cwd`; waits for its focused prompt.
    fn shell_tab(&mut self, cwd: &Path) -> TestResult<u64> {
        let created = self.cli(&[
            "new",
            "tab",
            "--json",
            "--cwd",
            &text(cwd),
            "--",
            &text(&self.path("bin/shell")),
        ])?;
        let created: Value = serde_json::from_str(&created).map_err(|e| e.to_string())?;
        let owner = created["block"]
            .as_u64()
            .ok_or("New tab returned no shell block")?;
        self.focus(owner)?;
        self.wait(
            "returned shell block is the focused real pane at its prompt",
            |h| {
                let state = h.ctl("state", &[])?;
                let layout = h.layout_state()?;
                Ok((state["focused"]["id"].as_u64() == Some(owner)
                    && state["focused"]["cwd"] == text(cwd)
                    && layout["focused"].as_u64() == Some(owner)
                    && pane_info(&layout, owner).is_some_and(|info| info["at_prompt"] == true))
                .then_some(()))
            },
        )?;
        Ok(owner)
    }

    fn launch(&mut self, cwd: PathBuf, new_tab: bool) -> TestResult<Client> {
        self.launch_with_config(cwd, new_tab, self.path("yazi"))
    }

    fn launch_with_config(
        &mut self,
        cwd: PathBuf,
        new_tab: bool,
        config: PathBuf,
    ) -> TestResult<Client> {
        self.launch_with(cwd, new_tab, config, "", None)
    }

    /// Launches Yazi with extra, already quoted arguments; `status` receives Yazi's exit status.
    fn launch_with(
        &mut self,
        cwd: PathBuf,
        new_tab: bool,
        config: PathBuf,
        extra: &str,
        status: Option<&Path>,
    ) -> TestResult<Client> {
        let owner = if new_tab {
            self.shell_tab(&cwd)?
        } else {
            self.ctl("state", &[])?["focused"]["id"]
                .as_u64()
                .ok_or("No real shell owner pane")?
        };
        let original = self.layout_state()?;
        let original_tab = pane_info(&original, owner)
            .and_then(|info| info["tab"].as_u64())
            .ok_or("Launching shell has no real tab")?;
        let original_layout = tab_layout(&original, original_tab)
            .ok_or("Launching shell has no real layout")?
            .clone();
        let original_panes = pane_ids(&original);
        let shell_marker = self.path(&format!("owner-shell-{owner}.pid"));
        self.cli(&[
            "run",
            &owner.to_string(),
            &format!("\x15printf '%s\\n' \"$$\" > {}", quote(text(&shell_marker))),
        ])?;
        let owner_shell = self.wait("actual original Bash PID", |_| {
            Ok(fs::read_to_string(&shell_marker)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .and_then(process)
                .map(|(shell, _, _)| shell))
        })?;
        self.shell_prompt(owner)?;
        let before = self.pid_markers();
        let known: BTreeSet<_> = fs::read_dir(self.inbox())
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        let line = format!(
            "cd {} && YAZI_CONFIG_HOME={} yazi {extra} {}{}",
            quote(text(&cwd)),
            quote(text(&config)),
            quote(text(&cwd)),
            status
                .map(|path| format!("; printf '%s\\n' \"$?\" > {}", quote(text(path))))
                .unwrap_or_default()
        );
        self.paste(owner, &line)?;
        self.key("Enter")?;
        let supervisor = self.wait("actual managed supervisor process", |h| {
            h.startup_panes(&original_panes)?;
            let pid = h
                .pid_markers()
                .difference(&before)
                .copied()
                .find(|pid| process(*pid).is_some());
            Ok(pid.and_then(process).map(|(p, _, _)| p))
        })?;
        self.owned.insert(supervisor);
        let client = self.wait(
            "real Yazi publishes its first snapshot to the surface supervisor",
            |h| {
                if !supervisor.alive() {
                    return Err(format!(
                        "Managed supervisor exited before its first snapshot; owner output: {}",
                        h.capture(owner)?
                    ));
                }
                let entries = fs::read_dir(h.inbox()).map_err(|e| e.to_string())?;
                for entry in entries.flatten() {
                    if known.contains(&entry.file_name()) {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let Some(cid) = name
                        .strip_prefix("state-")
                        .and_then(|s| s.strip_suffix(".json"))
                    else {
                        continue;
                    };
                    if h.snapshot_id(cid)["cwd"] != text(&cwd) {
                        continue;
                    }
                    let Some(backend) = descendants(supervisor).into_iter().find(|p| {
                        fs::read_link(format!("/proc/{}/exe", p.pid))
                            .is_ok_and(|path| path == Path::new("/usr/bin/yazi"))
                    }) else {
                        continue;
                    };
                    return Ok(Some(Client {
                        cid: cid.to_owned(),
                        owner,
                        supervisor,
                        backend,
                        cwd: cwd.clone(),
                        owner_shell: Some(owner_shell),
                        original_tab: Some(original_tab),
                        original_layout: original_layout.clone(),
                        original_panes: original_panes.clone(),
                    }));
                }
                Ok(None)
            },
        )?;
        self.owned.extend(descendants(supervisor));
        self.clients.push(client.clone());
        self.expect_ui(
            ROWS,
            "the shell pane draws the browser on its program surface",
            |value| !nodes(value).is_empty(),
        )?;
        self.wait("the surface replaces nothing: same panes, layout and focus", |h| {
            let state = h.layout_state()?;
            let owner = pane_info(&state, client.owner).ok_or("Managed launch destroyed its owner")?;
            let layout = tab_layout(&state, original_tab).ok_or("Managed launch lost original tab")?;
            let failing = if owner["tab"].as_u64() != Some(original_tab) { "owner left its tab" }
                else if pane_ids(&state) != client.original_panes { "pane set changed" }
                else if layout["root"] != client.original_layout["root"] { "root tree changed" }
                else if layout["floats"] != client.original_layout["floats"] { "floats changed" }
                else if state["focused"].as_u64() != Some(client.owner) { "focus left the owner" }
                else { "" };
            if !failing.is_empty() {
                return Err(format!("Surface launch changed the layout ({failing}): {state}; original {original}"));
            }
            Ok(Some(()))
        })?;
        Ok(client)
    }

    /// After the surface closes: the same Bash is back at its prompt in the untouched layout.
    fn prompt_returned(&mut self, client: &Client) -> TestResult<()> {
        self.wait("owner shell is back at its prompt with no surface", |h| {
            let state = h.layout_state()?;
            let owner = pane_info(&state, client.owner).ok_or("Original shell pane vanished")?;
            if owner["at_prompt"] != true {
                return Err(format!("owner not at its prompt: {owner}"));
            }
            let tab = client
                .original_tab
                .ok_or("Managed owner had no original tab")?;
            let layout = tab_layout(&state, tab).ok_or("Original tab disappeared")?;
            if owner["tab"].as_u64() != Some(tab)
                || layout["root"] != client.original_layout["root"]
            {
                return Err(format!(
                    "layout changed: {layout}; original {}",
                    client.original_layout
                ));
            }
            if !nodes(&h.tree(SURFACE_BROWSER)?).is_empty() {
                return Err("program surface still drawn".into());
            }
            Ok(Some(()))
        })?;
        let marker = self.path(&format!("shell-return-{}.txt", client.cid));
        self.cli(&[
            "run",
            &client.owner.to_string(),
            &format!("printf '%s\\n' \"$$\" > {}", quote(text(&marker))),
        ])?;
        self.wait("the original Bash executes another real command", |_| {
            Ok(fs::read_to_string(&marker)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
                .filter(|pid| client.owner_shell.is_some_and(|shell| shell.pid == *pid))
                .map(|_| ()))
        })
    }

    /// The pane's real `(rows, cols)` from Tern's window state; its tty winsize is frozen while a
    /// surface is live, so this is the size Yazi's PTY follows.
    fn pane_size(&self, pane: u64) -> TestResult<Option<(u16, u16)>> {
        let state = self.ctl("state", &[])?;
        Ok(state["panes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["id"].as_u64() == Some(pane))
            .map(|entry| {
                (
                    entry["rows"].as_u64().unwrap_or(0) as u16,
                    entry["cols"].as_u64().unwrap_or(0) as u16,
                )
            }))
    }

    /// The Yazi PTY follows the pane's size; both processes stay alive meanwhile.
    fn settled(&mut self, client: &Client, duration: Duration) -> TestResult<(u16, u16)> {
        let mut stable = None;
        let size = self.wait("Yazi's PTY settles at the surface pane's size", |h| {
            let Some(pane) = h.pane_size(client.owner)? else {
                return Ok(None);
            };
            if tty_size(client.backend)? != pane {
                stable = None;
                return Ok(None);
            }
            match stable {
                Some((previous, since)) if previous == pane => Ok((Instant::now()
                    .duration_since(since)
                    >= Duration::from_millis(600))
                .then_some(pane)),
                _ => {
                    stable = Some((pane, Instant::now()));
                    Ok(None)
                }
            }
        })?;
        let begin = Instant::now();
        while begin.elapsed() < duration {
            if !client.supervisor.alive() || !client.backend.alive() {
                return Err("A real surface process exited while idle".into());
            }
            if tty_size(client.backend)? != size {
                return Err(format!("Idle PTY size changed from {size:?}"));
            }
            thread::sleep(POLL);
        }
        Ok(size)
    }

    fn click_node(&mut self, selector: &str, label: &str, double: bool) -> TestResult<()> {
        let global_selector = matches!(selector, ROWS | PARENT_ROWS | CHILD_ROWS)
            .then(|| row_selector(selector, label));
        let selector = global_selector.as_deref().unwrap_or(selector);
        let node = self.wait(&format!("visible clickable {label}"), |h| {
            let value = h.tree(selector)?;
            Ok(nodes(&value)
                .into_iter()
                .find(|node| {
                    theme_visible(node)
                        && (node["text"]
                            .as_str()
                            .is_some_and(|s| s == label || s.ends_with(&format!(" {label}")))
                            || node["title"]
                                .as_str()
                                .is_some_and(|s| s.ends_with(&format!("/{label}"))))
                })
                .cloned())
        })?;
        let rect = node["rect"]
            .as_array()
            .ok_or("Visible row has no actual geometry")?;
        let x = rect[0].as_f64().ok_or("Missing rect x")?
            + rect[2].as_f64().ok_or("Missing rect width")? / 2.0;
        let y = rect[1].as_f64().ok_or("Missing rect y")?
            + rect[3].as_f64().ok_or("Missing rect height")? / 2.0;
        self.ctl(
            if double { "dblclick" } else { "click" },
            &[&x.to_string(), &y.to_string()],
        )?;
        Ok(())
    }

    fn open_path_editor(&mut self, cwd: &str) -> TestResult<()> {
        self.expect_ui(
            "[data-role=\"yazi.path-header\"]",
            "breadcrumbs reflect the authoritative cwd before clicking",
            |v| {
                nodes(v)
                    .iter()
                    .any(|node| node["title"].as_str() == Some(cwd))
            },
        )?;
        self.click_target("[data-role=\"yazi.path-edit\"]")?;
        self.expect_ui(
            "[data-role=\"yazi.path-input\"]",
            "directory editor is mounted before typing",
            |v| ui_text(v).contains(cwd),
        )?;
        Ok(())
    }

    fn click_target(&mut self, selector: &str) -> TestResult<()> {
        let value = self.expect_ui(selector, "path control has a visible hit target", |v| {
            nodes(v)
                .iter()
                .any(|node| node["rect"][2].as_f64().unwrap_or(0.0) > 0.0)
        })?;
        let node = nodes(&value)
            .into_iter()
            .find(|node| node["rect"][2].as_f64().unwrap_or(0.0) > 0.0)
            .ok_or("No visible path control")?;
        let rect = &node["rect"];
        let x = rect[0].as_f64().unwrap_or(0.0) + rect[2].as_f64().unwrap_or(0.0) / 2.0;
        let y = rect[1].as_f64().unwrap_or(0.0) + rect[3].as_f64().unwrap_or(0.0) / 2.0;
        self.ctl("click", &[&x.to_string(), &y.to_string()])
            .map(|_| ())
    }

    fn click_file(&mut self, client: &Client, name: &str) -> TestResult<()> {
        self.click_node(ROWS, name, false)?;
        let path = text(&client.cwd.join(name));
        self.expect_state(client, &format!("hover {path}"), |s| hover(s) == path)?;
        Ok(())
    }

    fn hover_stays(&self, client: &Client, expected: &str) -> TestResult<()> {
        let deadline = Instant::now() + Duration::from_millis(400);
        while Instant::now() < deadline {
            let actual = hover(&self.snapshot(client));
            if actual != expected {
                return Err(format!(
                    "Leader changed hover: expected {expected}, actual {actual}"
                ));
            }
            thread::sleep(POLL);
        }
        Ok(())
    }

    fn shot(&self, name: &str) -> TestResult<()> {
        self.ctl("shot", &[name]).map(|_| ())
    }

    /// Takes a screenshot and copies the PNG out of the artifact root into `dir`.
    fn keep_shot(&self, name: &str, dir: &Path) -> TestResult<PathBuf> {
        let shot = self.ctl("shot", &[name])?;
        let png = shot["png"]
            .as_str()
            .ok_or_else(|| format!("Screenshot has no PNG: {shot}"))?;
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let kept = dir.join(format!("{name}.png"));
        fs::copy(self.root.join(png), &kept).map_err(|e| e.to_string())?;
        Ok(kept)
    }

    fn opened(&mut self, path: &Path) -> TestResult<u64> {
        self.wait(
            &format!("focused Tern file block loads exact path {}", text(path)),
            |h| {
                let state = h.ctl("state", &[])?;
                let block = state["file_blocks"].as_array().and_then(|blocks| {
                    blocks
                        .iter()
                        .find(|b| b["path"] == text(path) && b["load"] == "ready")
                });
                Ok(block
                    .and_then(|b| b["id"].as_u64())
                    .filter(|id| state["focused"]["id"].as_u64() == Some(*id)))
            },
        )
    }

    fn close_file(&mut self, client: &Client, file: u64) -> TestResult<()> {
        self.cli(&["close", &file.to_string()])?;
        self.focus(client.owner)?;
        self.expect_ui(
            ROWS,
            "the browser is visible again after the Tern file closes",
            |v| !nodes(v).is_empty(),
        )?;
        Ok(())
    }

    /// Types `input` on the `:` (blocking) or `;` (background) shell line and submits it.
    fn shell(&mut self, client: &Client, input: &str, block: bool) -> TestResult<()> {
        self.focus(client.owner)?;
        self.key(if block { ":" } else { ";" })?;
        self.paste(client.owner, input)?;
        self.expect_ui("", "command bar contains the actual pasted script", |v| {
            ui_text(v).contains(input)
        })?;
        self.key("Enter")?;
        // Submitting closes the line at once, as Yazi's input does.
        self.expect_ui(
            "[data-role=\"yazi.command-bubble.shell\"]",
            "submitting closes the shell line",
            |v| nodes(v).is_empty(),
        )?;
        Ok(())
    }

    fn capture(&self, pane: u64) -> TestResult<String> {
        self.cli(&["capture", &pane.to_string()])
    }

    fn removed(&mut self, client: &Client, watched: &BTreeSet<Process>) -> TestResult<()> {
        self.wait(
            "owned supervisor/backend/command descendants exit and private controls disappear",
            |h| {
                let prefixes = [
                    format!("state-{}", client.cid),
                    format!("listing-{}", client.cid),
                    format!("client-{}.lock", client.cid),
                    format!("reply-{}-", client.cid),
                ];
                let remaining: Vec<_> = fs::read_dir(h.inbox())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        prefixes.iter().any(|p| name.starts_with(p)).then_some(name)
                    })
                    .collect();
                let alive: Vec<_> = watched
                    .iter()
                    .filter(|p| p.alive())
                    .map(|p| p.pid)
                    .collect();
                if remaining.is_empty() && alive.is_empty() {
                    Ok(Some(()))
                } else {
                    Err(format!("controls {remaining:?}, owned live PIDs {alive:?}"))
                }
            },
        )
    }

    fn diagnostics(&self, failure: &str) {
        let mut report = format!(
            "case: {}\nexpected/actual: {failure}\nprivate artifacts: {}\n",
            self.case,
            text(&self.root)
        );
        for client in &self.clients {
            report.push_str(&format!(
                "client {} owner {} pane {}\nsnapshot {}\n",
                client.cid,
                client.owner,
                client.owner,
                self.snapshot(client)
            ));
            if let Ok(output) = self.capture(client.owner) {
                report.push_str(&format!("owner terminal capture:\n{output}\n"));
            }
        }
        if let Ok(state) = self.ctl("state", &[]) {
            // Do not dump request JSON, process environments or daemon authentication data.
            report.push_str(&format!("visible state: {}\n", json!({"focused": state["focused"], "file_blocks": state["file_blocks"], "tabs": state["tabs"]})));
            if let Some(pane) = state["focused"]["id"].as_u64() {
                if let Ok(output) = self.capture(pane) {
                    report.push_str(&format!("focused terminal capture:\n{output}\n"));
                }
            }
        }
        if let Ok(tree) = self.tree("") {
            report.push_str(&format!("visible UI text:\n{}\n", ui_text(&tree)));
        }
        let _ = fs::write(self.path("failure.txt"), &report);
        eprintln!("{report}owned window log: {}/renderer.log; daemon state/logs are isolated under the artifact root", text(&self.root));
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let (Some(hook), Some(child)) = (
            env::var_os("TERN_YAZI_TEST_WINDOW_HOOK"),
            self.renderer.as_ref(),
        ) {
            if let Err(error) = self.execute(
                Path::new(&hook),
                &[child.id().to_string(), text(&self.root), "stop".into()],
            ) {
                eprintln!("Owned window hook cleanup failed: {error}");
            }
        }
        let mut infrastructure = BTreeSet::new();
        if let Some(child) = &self.renderer {
            if let Some((renderer, _, _)) = process(child.id() as i32) {
                infrastructure.extend(descendants(renderer));
            }
        }
        if let Some(daemon) = self.daemon_process.or_else(|| self.private_daemon()) {
            infrastructure.extend(descendants(daemon));
        }
        for client in &self.clients {
            self.owned.extend(descendants(client.supervisor));
        }
        // Ask the real supervisors to perform bounded process-group shutdown first.
        for client in &self.clients {
            client.supervisor.signal(libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.owned.iter().any(|p| p.alive()) && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        for owned in &self.owned {
            owned.signal(libc::SIGKILL);
        }
        if let Some(child) = &mut self.renderer {
            // Only groups created by this harness are terminated; no global matching.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                thread::sleep(POLL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        // The GUI's autostarted daemon may setsid; it is not covered by its process group.
        for owned in &infrastructure {
            owned.signal(libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while infrastructure.iter().any(|p| p.alive()) && Instant::now() < deadline {
            thread::sleep(POLL);
        }
        for owned in &infrastructure {
            owned.signal(libc::SIGKILL);
        }
        if self.success && env::var_os("TERN_YAZI_KEEP_INTERACTION_ARTIFACTS").is_none() {
            let _ = fs::remove_dir_all(&self.root);
        } else {
            eprintln!(
                "Retained private interaction artifacts: {}",
                text(&self.root)
            );
        }
    }
}

fn interaction_flow(h: &mut Harness) -> TestResult<()> {
    h.case = "cold startup and idle PTY size";
    let first = h.launch(h.path("files"), false)?;
    h.settled(&first, Duration::from_secs(7))?;
    h.expect_ui(ROWS, "the surface stays interactive after idle", |v| {
        !nodes(v).is_empty()
    })?;
    h.shot("01-cold-start")?;

    h.case = "header reserves a blank command row without duplicated status";
    h.expect_ui(
        ".sf",
        "headers and footer contain no selection counters",
        |v| {
            let text = ui_text(v);
            !text.contains("Synced") && !text.contains(" selected") && !text.contains(" sel")
        },
    )?;
    h.expect_ui(
        "[data-role=\"yazi.input-row\"]",
        "idle command row is blank",
        |v| nodes(v).len() == 1 && ui_text(v).trim().is_empty(),
    )?;
    let columns = h.tree("[data-role=\"yazi.column.current\"]")?;
    let idle_y = nodes(&columns)
        .first()
        .and_then(|n| n["rect"][1].as_f64())
        .ok_or("No visible current-column geometry")?;
    h.key(";")?;
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.shell\"]",
        "shell editor occupies the reserved command row",
        |v| nodes(v).len() == 1 && theme_visible(nodes(v)[0]),
    )?;
    h.expect_ui(
        "[data-role=\"yazi.column.current\"]",
        "command editing does not shift the file columns",
        |v| {
            nodes(v)
                .first()
                .and_then(|n| n["rect"][1].as_f64())
                .is_some_and(|y| (y - idle_y).abs() < 1.0)
        },
    )?;
    // The first Escape only leaves insert mode; the second cancels the line.
    h.key("Escape")?;
    h.key("Escape")?;
    h.expect_ui(
        "[data-role=\"yazi.input-row\"]",
        "cancel restores the blank reserved row",
        |v| ui_text(v).trim().is_empty(),
    )?;

    h.case = "real mouse cursor, following keyboard, directory activation and parent hover";
    h.click_file(&first, "alpha.txt")?;
    let entries = names(&h.snapshot(&first), "files");
    let index = entries
        .iter()
        .position(|s| s == "alpha.txt")
        .ok_or("Listing lacks alpha.txt")?;
    let next = entries
        .get(index + 1)
        .ok_or("Fixture has no next row")?
        .clone();
    h.key("j")?;
    h.expect_state(&first, "j follows clicked cursor", |s| {
        hover(s) == text(&first.cwd.join(&next))
    })?;
    h.click_node(ROWS, "child", true)?;
    h.expect_state(&first, "double-click enters real child directory", |s| {
        s["cwd"] == text(&first.cwd.join("child"))
    })?;
    h.key("h")?;
    h.expect_state(
        &first,
        "h returns to parent and preserves child hover",
        |s| s["cwd"] == text(&first.cwd) && hover(s) == text(&first.cwd.join("child")),
    )?;
    h.key("H")?;
    h.expect_state(&first, "H uses directory history", |s| {
        s["cwd"] == text(&first.cwd.join("child"))
    })?;
    h.key("L")?;
    h.expect_state(&first, "L restores forward directory history", |s| {
        s["cwd"] == text(&first.cwd)
    })?;

    h.case = "breadcrumb clicks and full directory editing use the Yazi actor";
    let crumbs = "[data-role=\"yazi.path-segment\"]";
    let root_name = h.root.file_name().unwrap().to_str().unwrap().to_owned();
    let root_path = text(&h.root);
    let breadcrumb_root = text(&h.path("breadcrumbs"));
    h.click_node(crumbs, &root_name, false)?;
    h.expect_state(
        &first,
        "ancestor click changes the authoritative directory",
        |s| s["cwd"] == root_path,
    )?;
    h.click_target("[data-role=\"yazi.path-edit\"]")?;
    h.expect_ui(
        "[data-role=\"yazi.path-input\"]",
        "editor contains the complete ancestor path",
        |v| ui_text(v).contains(&root_path),
    )?;
    h.paste(first.owner, "files")?;
    h.key("Enter")?;
    h.expect_state(
        &first,
        "relative directory input returns to the file fixture",
        |s| s["cwd"] == text(&first.cwd),
    )?;
    h.open_path_editor(&text(&first.cwd))?;
    h.click_target("[data-role=\"yazi.path-input\"]")?;
    h.key("x")?;
    h.expect_ui(
        "[data-role=\"yazi.path-input\"]",
        "pointer editing inserts instead of replacing the selected path",
        |v| {
            nodes(v)
                .iter()
                .any(|node| node["text"].as_str() == Some(&format!("{}x", text(&first.cwd))))
        },
    )?;
    h.click_target("[data-role=\"yazi.path-input\"]")?;
    h.key("x")?;
    h.expect_ui(
        "[data-role=\"yazi.path-input\"]",
        "a repeated input click does not select the whole path again",
        |v| {
            nodes(v)
                .iter()
                .any(|node| node["text"].as_str() == Some(&format!("{}xx", text(&first.cwd))))
        },
    )?;
    h.key("Escape")?;
    h.key("Escape")?;
    h.click_target("[data-role=\"yazi.path-edit\"]")?;
    h.paste(first.owner, "/does/not/exist/tern-yazi")?;
    h.key("Escape")?;
    h.key("Escape")?;
    h.expect_ui(
        crumbs,
        "Escape restores breadcrumbs without navigating",
        |v| !nodes(v).is_empty(),
    )?;
    h.expect_state(&first, "cancel preserves Yazi cwd", |s| {
        s["cwd"] == text(&first.cwd)
    })?;

    let mut long_dir = h.path("breadcrumbs");
    for index in 0..8 {
        long_dir = long_dir.join(format!("ancestor-{index}-long-directory"));
    }
    let leaf = "雪 long directory ".repeat(8);
    long_dir = long_dir.join(&leaf);
    fs::create_dir_all(&long_dir).map_err(|e| e.to_string())?;
    h.open_path_editor(&text(&first.cwd))?;
    h.paste(first.owner, &text(&long_dir))?;
    h.key("Enter")?;
    h.expect_state(&first, "long Unicode directory input is lossless", |s| {
        s["cwd"] == text(&long_dir)
    })?;
    h.expect_ui(
        crumbs,
        "current long directory remains visible with a full-path tooltip",
        |v| {
            nodes(v).iter().any(|node| {
                node["title"] == text(&long_dir) && node["rect"][2].as_f64().unwrap_or(0.0) > 0.0
            })
        },
    )?;
    let wide_header = h.tree("[data-role=\"yazi.path-header\"]")?;
    let wide_width = nodes(&wide_header)[0]["rect"][2]
        .as_f64()
        .ok_or("No wide header geometry")?;
    h.ctl("resize", &["480", "640"])?;
    let header = h.expect_ui(
        "[data-role=\"yazi.path-header\"]",
        "resize narrows the header before containment checks",
        |v| {
            nodes(v).iter().any(|node| {
                node["rect"][2]
                    .as_f64()
                    .is_some_and(|width| width < wide_width)
            })
        },
    )?;
    let header_rect = &nodes(&header)[0]["rect"];
    let header_right =
        header_rect[0].as_f64().unwrap_or(0.0) + header_rect[2].as_f64().unwrap_or(0.0);
    h.expect_ui(
        crumbs,
        "narrow layout preserves the current directory inside the header",
        |v| {
            nodes(v).iter().any(|node| {
                node["title"] == text(&long_dir)
                    && node["rect"][2].as_f64().unwrap_or(0.0) > 0.0
                    && node["rect"][0].as_f64().unwrap_or(10000.0)
                        + node["rect"][2].as_f64().unwrap_or(10000.0)
                        <= header_right + 1.0
            })
        },
    )?;
    h.shot("04-narrow-long-breadcrumbs")?;
    h.ctl("resize", &["1280", "900"])?;
    h.click_target("[data-role=\"yazi.path-more\"]")?;
    h.click_node(
        "[data-role=\"yazi.path-ancestors\"] [data-role=\"yazi.path-segment\"]",
        "breadcrumbs",
        false,
    )?;
    h.expect_state(
        &first,
        "collapsed ancestor menu navigates to the exact ancestor",
        |s| s["cwd"] == breadcrumb_root,
    )?;
    h.open_path_editor(&breadcrumb_root)?;
    h.paste(first.owner, &text(&first.cwd.join("alpha.txt")))?;
    h.key("Enter")?;
    h.expect_ui(
        "[data-role=\"yazi.path-input\"]",
        "file paths are rejected while the editor remains available",
        |v| ui_text(v).contains("alpha.txt"),
    )?;
    h.expect_ui("*", "Yazi reports the directory-only rejection", |v| {
        ui_text(v).contains("Target is not a directory")
    })?;
    h.expect_state(&first, "rejected file path does not change cwd", |s| {
        s["cwd"] == breadcrumb_root
    })?;
    h.key("Escape")?;
    h.key("Escape")?;
    h.open_path_editor(&breadcrumb_root)?;
    h.paste(first.owner, &text(&first.cwd))?;
    h.key("Enter")?;
    h.expect_state(&first, "absolute input returns to the fixture", |s| {
        s["cwd"] == text(&first.cwd)
    })?;
    h.shot("04-breadcrumb-navigation")?;
    h.click_node(crumbs, "files", false)?;
    h.expect_ui(
        "[data-role=\"yazi.path-segment\"]:hover",
        "pointer hovers the exact current-directory breadcrumb",
        |v| {
            nodes(v)
                .iter()
                .any(|node| node["title"] == text(&first.cwd))
        },
    )?;
    h.shot("04-breadcrumb-hover")?;

    h.case = "gg leader, G/Home, Space, visual selection and Escape";
    h.key("G")?;
    let list = names(&h.snapshot(&first), "files");
    let bottom = text(&first.cwd.join(list.last().ok_or("Empty listing")?));
    h.expect_state(&first, "G reaches final row", |s| hover(s) == bottom)?;
    h.key("g")?;
    h.expect_state(&first, "first g does not jump", |s| hover(s) == bottom)?;
    h.hover_stays(&first, &bottom)?;
    h.expect_ui(
        "[data-role=\"yazi.status-name\"]",
        "leader keeps visible final filename",
        |v| ui_text(v).contains(list.last().unwrap()),
    )?;
    h.key("g")?;
    let top = text(&first.cwd.join(&list[0]));
    h.expect_state(&first, "second g reaches first row", |s| hover(s) == top)?;
    h.key("G")?;
    h.key("Home")?;
    h.expect_state(&first, "Home reaches first row", |s| hover(s) == top)?;
    h.key("Space")?;
    let second_path = text(&first.cwd.join(&list[1]));
    h.expect_state(&first, "Space selects and advances cursor", |s| {
        selected(s).contains(&top) && hover(s) == second_path
    })?;
    h.key("k")?;
    h.expect_state(&first, "k restores first row", |s| hover(s) == top)?;
    h.key("Space")?;
    h.expect_state(
        &first,
        "Space toggles selected item off and advances",
        |s| selected(s).is_empty() && hover(s) == second_path,
    )?;
    h.key("Home")?;
    h.key("v")?;
    h.key("j")?;
    h.expect_state(&first, "v extends visual range", |s| {
        s["mode"] == "select" && names(s, "marked_urls").len() >= 2
    })?;
    h.key("Escape")?;
    h.expect_state(&first, "Escape clears visual range without closing", |s| {
        s["mode"] == "normal" && names(s, "marked_urls").is_empty()
    })?;
    h.key("Escape")?;
    h.expect_state(
        &first,
        "normal Escape clears visual selection before the unset case",
        |s| selected(s).is_empty(),
    )?;
    h.key("Home")?;
    h.expect_state(&first, "Home settles before selecting the first row", |s| {
        hover(s) == top
    })?;
    h.key("Space")?;
    h.expect_state(&first, "selection available before V", |s| {
        selected(s).contains(&top)
    })?;
    h.key("k")?;
    h.key("V")?;
    h.key("j")?;
    h.expect_state(&first, "V creates visual unset range", |s| {
        s["mode"] == "unset" && names(s, "marked_urls").len() >= 2
    })?;
    h.key("Escape")?;
    h.key("Escape")?;
    h.expect_state(&first, "Escape clears selection and keeps panel", |s| {
        s["mode"] == "normal" && selected(s).is_empty()
    })?;
    h.expect_ui(ROWS, "Escape never closes live browser", |v| {
        !nodes(v).is_empty()
    })?;

    h.case = "smartcase filtering, Enter, visual Escape priority and stale bar";
    h.key("f")?;
    h.paste(first.owner, "alpha")?;
    h.expect_state(&first, "lowercase filter matches both cases", |s| {
        names(s, "files").into_iter().collect::<BTreeSet<_>>()
            == BTreeSet::from(["Alpha.txt".into(), "alpha.txt".into()])
    })?;
    h.key("Enter")?;
    h.expect_state(&first, "Enter retains filter", |s| {
        s["filter"].as_str().is_some_and(|f| f.contains("alpha"))
    })?;
    h.key("v")?;
    h.key("j")?;
    h.expect_state(&first, "filtered visual range is active", |s| {
        s["mode"] == "select"
    })?;
    h.key("Escape")?;
    h.expect_state(
        &first,
        "first Escape clears visual mode but preserves filter",
        |s| {
            s["mode"] == "normal"
                && names(s, "files").len() == 2
                && s["filter"].as_str().is_some_and(|f| f.contains("alpha"))
        },
    )?;
    h.key("Escape")?;
    h.expect_state(&first, "second Escape removes filter", |s| {
        names(s, "files").len() > 2 && s["filter"].as_str().unwrap_or_default().is_empty()
    })?;
    h.key("f")?;
    h.paste(first.owner, "Alpha")?;
    h.key("Enter")?;
    h.expect_state(&first, "uppercase filter is case-sensitive", |s| {
        names(s, "files") == vec!["Alpha.txt"]
    })?;
    h.key("Escape")?;
    h.expect_state(&first, "uppercase filter clears", |s| {
        names(s, "files").len() > 2
    })?;

    h.case = "slash find moves hover without filtering the listing";
    h.key("Escape")?;
    h.expect_state(&first, "find cases begin without retained selection", |s| {
        selected(s).is_empty()
    })?;
    h.key("Home")?;
    let full_listing = names(
        &h.expect_state(&first, "find baseline listing is complete", |s| {
            !names(s, "files").is_empty()
        })?,
        "files",
    );
    h.key("/")?;
    h.paste(first.owner, "alpha")?;
    h.key("Enter")?;
    h.expect_state(
        &first,
        "slash finds the first match without hiding entries",
        |s| hover(s) == text(&first.cwd.join("Alpha.txt")) && names(s, "files") == full_listing,
    )?;
    h.key("n")?;
    h.expect_state(&first, "n advances to the next find match", |s| {
        hover(s) == text(&first.cwd.join("alpha.txt"))
    })?;
    h.key("N")?;
    h.expect_state(&first, "N returns to the previous find match", |s| {
        hover(s) == text(&first.cwd.join("Alpha.txt"))
    })?;
    h.key("Escape")?;
    h.key("n")?;
    h.hover_stays(&first, &text(&first.cwd.join("Alpha.txt")))?;
    h.key("G")?;
    h.key("?")?;
    h.paste(first.owner, "alpha")?;
    h.key("Enter")?;
    h.expect_state(
        &first,
        "question mark searches backward without filtering",
        |s| hover(s) == text(&first.cwd.join("alpha.txt")) && names(s, "files") == full_listing,
    )?;
    h.key("Escape")?;
    h.key("f")?;
    h.paste(first.owner, "beta")?;
    h.key("Enter")?;
    h.expect_state(&first, "filter remains distinct from find", |s| {
        names(s, "files") == vec!["beta.txt"]
    })?;
    h.key("/")?;
    h.paste(first.owner, "beta")?;
    h.key("Enter")?;
    h.key("Escape")?;
    h.expect_state(
        &first,
        "Escape clears find before the retained filter",
        |s| names(s, "files") == vec!["beta.txt"] && s["filter"] == "beta",
    )?;
    h.key("Escape")?;
    h.expect_state(&first, "the following Escape clears the filter", |s| {
        names(s, "files") == full_listing
    })?;

    h.case = "UTF-8, punctuation paste/editing and hidden listing";
    h.key("f")?;
    h.paste(first.owner, "雪's 100%雪")?;
    h.expect_ui("", "UTF-8 paste enters filter unchanged", |v| {
        ui_text(v).contains("雪's 100%雪")
    })?;
    h.key("Backspace")?;
    h.expect_ui("", "Backspace erases one complete UTF-8 codepoint", |v| {
        ui_text(v).contains("雪's 100%") && !ui_text(v).contains("雪's 100%雪")
    })?;
    h.key("Enter")?;
    h.expect_state(
        &first,
        "Unicode apostrophe percent filter matches actual filename",
        |s| names(s, "files") == vec!["雪's 100%.txt"],
    )?;
    h.key("Escape")?;
    h.expect_state(&first, "hidden row initially absent", |s| {
        names(s, "files").len() > 2 && !names(s, "files").contains(&".hidden.txt".into())
    })?;
    h.key(".")?;
    h.expect_state(&first, "period exposes actual hidden file", |s| {
        names(s, "files").contains(&".hidden.txt".into())
    })?;
    h.expect_ui(ROWS, "hidden file is rendered", |v| {
        ui_text(v).contains(".hidden.txt")
    })?;
    h.key(".")?;
    h.expect_state(&first, "period hides file again", |s| {
        !names(s, "files").contains(&".hidden.txt".into())
    })?;

    h.case = "Tern owns real text, Markdown and image previews";
    h.key("Escape")?;
    h.expect_state(
        &first,
        "preview/open cases begin without retained selection",
        |s| selected(s).is_empty(),
    )?;
    h.click_file(&first, "oversized.txt")?;
    h.expect_ui(
        "[data-role=\"yazi.preview-unavailable\"]",
        "oversized preview shows a compact failure state without internal diagnostics",
        |v| {
            let text = ui_text(v);
            nodes(v)
                .iter()
                .any(|node| node["rect"][2].as_f64().is_some_and(|width| width > 0.0))
                && ["runtime error", "stack traceback", "max_bytes"]
                    .iter()
                    .all(|detail| !text.contains(detail))
        },
    )?;
    h.shot("02-preview-limit-no-traceback")?;
    for (name, content) in [
        ("alpha.txt", "lower_alpha_content"),
        ("notes.md", "MARKDOWN_PREVIEW_CONTENT"),
    ] {
        h.click_file(&first, name)?;
        h.expect_ui(PREVIEW, "actual file contents in preview", |v| {
            ui_text(v).contains(content)
        })?;
    }
    h.click_file(&first, "image.svg")?;
    h.expect_ui(
        "[data-role=\"yazi.image\"] svg[role=\"img\"] rect",
        "actual SVG fixture is drawn with visible square geometry",
        |v| {
            nodes(v).iter().any(|node| {
                let rect = &node["rect"];
                let width = rect[2].as_f64().unwrap_or(0.0);
                let height = rect[3].as_f64().unwrap_or(0.0);
                width >= 32.0 && height >= 32.0 && (width - height).abs() < 1.0
            })
        },
    )?;
    h.shot("02-image-preview")?;
    h.case = "semicolon background has real delayed effects without terminal";
    let marker = h.path("background.txt");
    let script = format!(
        "sleep 0.3; printf 'BACKGROUND_REAL_EFFECT\\n' > {}",
        quote(text(&marker))
    );
    let terminal_count = h.ctl("state", &[])?["panes"].as_array().map_or(0, Vec::len);
    h.shell(&first, &script, false)?;
    h.wait("real background command writes its delayed marker", |h| {
        let state = h.ctl("state", &[])?;
        if state["focused"]["id"].as_u64() != Some(first.owner) {
            return Err("Background command stole focus into a terminal".into());
        }
        if state["panes"].as_array().map_or(0, Vec::len) != terminal_count {
            return Err("Background command created an unexpected terminal pane".into());
        }
        Ok(fs::read_to_string(&marker)
            .ok()
            .filter(|s| s == "BACKGROUND_REAL_EFFECT\n")
            .map(|_| ()))
    })?;

    shortcut_flow(h, &first)?;

    h.case = "Trash exact selection, Escape cancellation, confirmed disposable paths";
    h.click_file(&first, "trash-a.txt")?;
    h.key("Space")?;
    h.expect_state(&first, "first disposable file selected", |s| {
        selected(s).contains(&text(&first.cwd.join("trash-a.txt")))
    })?;
    h.click_file(&first, "trash-b.txt")?;
    h.key("Space")?;
    let paths = BTreeSet::from([
        text(&first.cwd.join("trash-a.txt")),
        text(&first.cwd.join("trash-b.txt")),
    ]);
    h.expect_state(&first, "two exact disposable paths selected", |s| {
        selected(s) == paths
    })?;
    h.key("d")?;
    h.expect_ui(
        "[data-role=\"yazi.trash-confirm\"]",
        "confirmation shows both exact selected paths",
        |v| paths.iter().all(|path| ui_text(v).contains(path)),
    )?;
    h.shot("07-exact-trash-confirmation")?;
    h.key("Escape")?;
    h.expect_ui(
        "[data-role=\"yazi.trash-confirm\"]",
        "Escape dismisses destructive confirmation",
        |v| nodes(v).is_empty(),
    )?;
    h.expect_state(&first, "cancel leaves selection unchanged", |s| {
        selected(s) == paths
    })?;
    for path in &paths {
        if !Path::new(path).is_file() {
            return Err(format!("Trash cancellation removed fixture {path}"));
        }
    }
    h.key("d")?;
    h.expect_ui(
        "[data-role=\"yazi.trash-confirm\"]",
        "confirmed paths remain exact",
        |v| paths.iter().all(|path| ui_text(v).contains(path)),
    )?;
    h.key("Enter")?;
    h.wait(
        "only confirmed disposable paths move to isolated XDG Trash",
        |h| {
            let infos = h.path("data/Trash/info");
            let mut originals = BTreeSet::new();
            if let Ok(entries) = fs::read_dir(infos) {
                for entry in entries.flatten() {
                    if let Ok(contents) = fs::read_to_string(entry.path()) {
                        for line in contents.lines() {
                            if let Some(path) = line.strip_prefix("Path=") {
                                originals.insert(path.to_owned());
                            }
                        }
                    }
                }
            }
            if paths
                .iter()
                .all(|p| !Path::new(p).exists() && originals.contains(p))
                && first.cwd.join("alpha.txt").is_file()
                && first.cwd.join("beta.txt").is_file()
            {
                Ok(Some(()))
            } else {
                Err(format!(
                    "trash original paths {originals:?}, expected {paths:?}"
                ))
            }
        },
    )?;

    h.case = "two clients preserve independent cwd and hover";
    let second = h.launch(h.path("other"), true)?;
    if first.cid == second.cid || first.owner == second.owner {
        return Err("Two actual managed launches reused a pinned identity".into());
    }
    h.click_file(&second, "other.txt")?;
    let second_before = h.snapshot(&second);
    h.focus(first.owner)?;
    h.click_file(&first, "beta.txt")?;
    h.expect_state(&second, "other client cwd and hover do not change", |s| {
        s["cwd"] == second_before["cwd"] && hover(s) == hover(&second_before)
    })?;
    h.focus(second.owner)?;
    h.expect_ui(
        ROWS,
        "second client does not render first client files",
        |v| ui_text(v).contains("other.txt") && !ui_text(v).contains("beta.txt"),
    )?;

    h.case = "q cleans only the owning real backend process group";
    h.focus(first.owner)?;
    let watched = descendants(first.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&first, &watched)?;
    h.prompt_returned(&first)?;
    if !second.supervisor.alive() || !second.backend.alive() {
        return Err("Closing first client killed the unrelated second client".into());
    }
    h.focus(second.owner)?;
    h.expect_ui(ROWS, "second client survives first client q", |v| {
        ui_text(v).contains("other.txt")
    })?;

    h.case = "closing the surface pane cleans its own backend";
    let watched = descendants(second.supervisor);
    h.owned.extend(watched.iter().copied());
    h.cli(&["close", &second.owner.to_string()])?;
    h.removed(&second, &watched)?;

    h.case = "backend crash closes the surface and returns the shell prompt";
    let fourth = h.launch(h.path("other"), true)?;
    let watched = descendants(fourth.supervisor);
    h.owned.extend(watched.iter().copied());
    fourth.backend.signal(libc::SIGKILL);
    h.removed(&fourth, &watched)?;
    h.prompt_returned(&fourth)
}

/// Window actions from the surface (Tern file opens, terminal, clipboard), `:` through Yazi,
/// and the palette entry and Ctrl+Alt+Y.
fn window_flow(h: &mut Harness) -> TestResult<()> {
    h.case = "o, Enter, multi-select and double-click open paths in Tern";
    let first = h.launch(h.path("files"), true)?;
    h.click_file(&first, "alpha.txt")?;
    h.key("o")?;
    let file = h.opened(&first.cwd.join("alpha.txt"))?;
    h.shot("03-tern-text-open")?;
    h.close_file(&first, file)?;
    h.click_file(&first, "Alpha.txt")?;
    h.key("Space")?;
    h.expect_state(&first, "selected A differs from hovered B", |s| {
        selected(s) == BTreeSet::from([text(&first.cwd.join("Alpha.txt"))])
            && hover(s) != text(&first.cwd.join("Alpha.txt"))
    })?;
    h.click_file(&first, "beta.txt")?;
    h.key("Enter")?;
    let file = h.opened(&first.cwd.join("Alpha.txt"))?;
    let state = h.ctl("state", &[])?;
    if state["file_blocks"].as_array().is_some_and(|blocks| {
        blocks
            .iter()
            .any(|b| b["path"] == text(&first.cwd.join("beta.txt")))
    }) {
        return Err("Enter opened hovered B despite authoritative selected A".into());
    }
    h.close_file(&first, file)?;
    h.key("Escape")?;
    h.expect_state(&first, "selection cleared before exact double-click", |s| {
        selected(s).is_empty()
    })?;
    h.click_file(&first, "Alpha.txt")?;
    h.key("Space")?;
    h.expect_state(&first, "first multiple-open target selected", |s| {
        selected(s).contains(&text(&first.cwd.join("Alpha.txt")))
    })?;
    h.click_file(&first, "alpha.txt")?;
    h.key("Space")?;
    let targets = BTreeSet::from([
        text(&first.cwd.join("Alpha.txt")),
        text(&first.cwd.join("alpha.txt")),
    ]);
    h.expect_state(
        &first,
        "both authoritative multiple-open targets selected",
        |s| selected(s) == targets,
    )?;
    h.click_file(&first, "beta.txt")?;
    h.key("o")?;
    let files = h.wait(
        "Tern loads every selected path, never the unselected hover",
        |h| {
            let state = h.ctl("state", &[])?;
            let blocks = state["file_blocks"]
                .as_array()
                .ok_or("No Tern file blocks")?;
            if blocks
                .iter()
                .any(|b| b["path"] == text(&first.cwd.join("beta.txt")))
            {
                return Err("Multiple-open included the unselected hovered path".into());
            }
            let files: Vec<_> = blocks
                .iter()
                .filter(|b| {
                    b["load"] == "ready" && b["path"].as_str().is_some_and(|p| targets.contains(p))
                })
                .filter_map(|b| b["id"].as_u64())
                .collect();
            Ok((files.len() == targets.len()).then_some(files))
        },
    )?;
    for file in files {
        h.cli(&["close", &file.to_string()])?;
    }
    h.focus(first.owner)?;
    h.key("Escape")?;
    h.expect_state(&first, "multiple-open selection clears", |s| {
        selected(s).is_empty()
    })?;
    h.click_node(ROWS, "notes.md", true)?;
    let file = h.opened(&first.cwd.join("notes.md"))?;
    h.shot("04-tern-markdown-open")?;
    h.close_file(&first, file)?;
    h.click_file(&first, "image.svg")?;
    // Preview action buttons are removed by design; Enter opens the hovered file.
    h.key("Enter")?;
    let file = h.opened(&first.cwd.join("image.svg"))?;
    h.shot("05-tern-image-open")?;
    h.close_file(&first, file)?;

    h.case = "Shift+W opens a terminal below the browser in its directory";
    let panes = pane_ids(&h.layout_state()?);
    h.key("W")?;
    let terminal = h.wait("a shell pane below the browser at the browser's cwd", |h| {
        let state = h.layout_state()?;
        let added: Vec<u64> = pane_ids(&state).difference(&panes).copied().collect();
        let [pane] = added[..] else {
            return Ok(None);
        };
        let info = pane_info(&state, pane).ok_or("New pane has no info")?;
        let tab = info["tab"].as_u64().and_then(|tab| tab_layout(&state, tab));
        let below = tab.is_some_and(|tab| {
            let root = &tab["root"];
            root["split"] == "down"
                && root["first"]["pane"].as_u64() == Some(first.owner)
                && root["second"]["pane"].as_u64() == Some(pane)
        });
        Ok((below && info["cwd"] == text(&first.cwd) && info["at_prompt"] == true).then_some(pane))
    })?;
    h.cli(&["close", &terminal.to_string()])?;
    h.focus(first.owner)?;
    h.expect_ui(
        ROWS,
        "the browser keeps its pane after the terminal closes",
        |v| nodes(v).iter().any(|node| theme_visible(node)),
    )?;

    h.case = "cc and cf copy the hovered path and name through OSC 52";
    let saved = Command::new("wl-paste")
        .arg("--no-newline")
        .output()
        .map_err(|e| format!("wl-paste is required: {e}"))?;
    h.click_file(&first, "beta.txt")?;
    let copied = (|| -> TestResult<()> {
        for (second, expected) in [
            ("c", text(&first.cwd.join("beta.txt"))),
            ("f", "beta.txt".to_owned()),
        ] {
            h.key("c")?;
            h.key(second)?;
            h.wait(&format!("the clipboard holds {expected}"), |_| {
                let clip = Command::new("wl-paste")
                    .arg("--no-newline")
                    .output()
                    .map_err(|e| e.to_string())?;
                Ok((clip.stdout == expected.as_bytes()).then_some(()))
            })?;
        }
        Ok(())
    })();
    // Hand the user's clipboard back; wl-copy keeps serving it from the background.
    let mut restore = Command::new("wl-copy");
    if saved.status.success() {
        restore.stdin(Stdio::piped());
    } else {
        restore.arg("--clear").stdin(Stdio::null());
    }
    let mut restore = restore
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut stdin) = restore.stdin.take() {
        stdin.write_all(&saved.stdout).map_err(|e| e.to_string())?;
    }
    restore.wait().map_err(|e| e.to_string())?;
    copied?;

    h.case = ": runs through Yazi; macros keep Unicode, quotes, percent and the cwd";
    h.click_file(&first, "雪's 100%.txt")?;
    h.key("Space")?;
    h.expect_state(&first, "Unicode selected path", |s| {
        selected(s).contains(&text(&first.cwd.join("雪's 100%.txt")))
    })?;
    h.click_file(&first, "beta.txt")?;
    let marker = h.path("shell-args.txt");
    let cwd_marker = h.path("shell-cwd.txt");
    let script = format!(
        "printf '%%s\\n' %h %s > {}; pwd > {}",
        quote(text(&marker)),
        quote(text(&cwd_marker))
    );
    h.shell(&first, &script, true)?;
    h.wait(
        "the shell expands hovered %h and selected %s paths exactly",
        |_| {
            let actual = fs::read_to_string(&marker).map_err(|e| e.to_string())?;
            let expected = format!(
                "{}\n{}\n",
                text(&first.cwd.join("beta.txt")),
                text(&first.cwd.join("雪's 100%.txt"))
            );
            let actual_cwd = fs::read_to_string(&cwd_marker).map_err(|e| e.to_string())?;
            if actual == expected && actual_cwd.trim_end() == text(&first.cwd) {
                Ok(Some(()))
            } else {
                Err(format!(
                    "actual argv {actual:?}, expected {expected:?}, cwd {actual_cwd:?}"
                ))
            }
        },
    )?;
    h.key("Escape")?;
    h.expect_state(&first, "macro selection cleared", |s| {
        selected(s).is_empty()
    })?;

    // `ctl key` does not reach the window keymap (built-in Ctrl+Shift+T does not fire either),
    // so the chord is checked where the keymap shows it, the palette row, and its action is run
    // by name exactly as the chord runs it.
    h.case = "the palette entry and the Ctrl+Alt+Y action open the browser in a new pane";
    for trigger in ["palette", "plugin.tern-yazi.open"] {
        let shell = h.shell_tab(&h.path("files"))?;
        let panes = pane_ids(&h.layout_state()?);
        let before = h.pid_markers();
        let known: BTreeSet<_> = fs::read_dir(h.inbox())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        if trigger == "palette" {
            h.ctl("plugins", &["run", "palette"])?;
            // The browser's own tab is titled Yazi; query the command title so it is the only row.
            h.ctl("type", &["\"Open File Browser\""])?;
            h.expect_ui(
                "",
                "the palette lists Yazi: Open File Browser with its Ctrl+Alt+Y chord",
                |v| {
                    // Query highlights split the row into runs; compare it without whitespace.
                    let row: String = ui_text(v).split_whitespace().collect();
                    row.contains("Yazi:OpenFileBrowserCtrlAltY")
                },
            )?;
            h.key("Enter")?;
        } else {
            h.ctl("plugins", &["run", trigger])?;
        }
        let (pane, supervisor) =
            h.wait(
                &format!("{trigger} splits a focused pane right of the shell"),
                |h| {
                    let state = h.layout_state()?;
                    let added: Vec<u64> = pane_ids(&state).difference(&panes).copied().collect();
                    let supervisor = h
                        .pid_markers()
                        .difference(&before)
                        .copied()
                        .find_map(process)
                        .map(|(p, _, _)| p);
                    let (&[pane], Some(supervisor)) = (&added[..], supervisor) else {
                        return Ok(None);
                    };
                    let tab = pane_info(&state, pane)
                        .and_then(|info| info["tab"].as_u64())
                        .and_then(|tab| tab_layout(&state, tab));
                    let right = tab.is_some_and(|tab| {
                        tab["root"]["split"] == "right"
                            && tab["root"]["first"]["pane"].as_u64() == Some(shell)
                            && tab["root"]["second"]["pane"].as_u64() == Some(pane)
                    });
                    Ok((right && state["focused"].as_u64() == Some(pane))
                        .then_some((pane, supervisor)))
                },
            )?;
        h.owned.insert(supervisor);
        h.wait("the new browser starts in the shell's directory", |h| {
            let fresh = fs::read_dir(h.inbox())
                .map_err(|e| e.to_string())?
                .flatten()
                .filter(|e| !known.contains(&e.file_name()));
            Ok(fresh
                .filter_map(|e| {
                    e.file_name()
                        .to_str()?
                        .strip_prefix("state-")?
                        .strip_suffix(".json")
                        .map(str::to_owned)
                })
                .any(|cid| h.snapshot_id(&cid)["cwd"] == text(&h.path("files")))
                .then_some(()))
        })?;
        h.expect_ui(ROWS, "the new pane draws the browser", |v| {
            nodes(v).iter().any(|node| theme_visible(node))
        })?;
        h.shot(&format!(
            "07-open-{}",
            if trigger == "palette" {
                "palette"
            } else {
                "shortcut"
            }
        ))?;
        let watched = descendants(supervisor);
        h.owned.extend(watched.iter().copied());
        h.key("q")?;
        h.wait("q ends Yazi and closes its pane", |h| {
            let gone = !pane_ids(&h.layout_state()?).contains(&pane);
            Ok((gone && !supervisor.alive() && watched.iter().all(|p| !p.alive())).then_some(()))
        })?;
        h.cli(&["close", &shell.to_string()])?;
    }
    Ok(())
}

fn glyph_interaction_flow(h: &mut Harness) -> TestResult<()> {
    h.case = "isolated theme overrides export authoritative glyphs";
    // Keep icon fixtures out of the original suite's visible rows and later mutations.
    for dir in [
        "glyph-files/current/child",
        "glyph-files/current/conditional-dir",
        "glyph-files/parent-dir",
        "glyph-members/folder",
    ] {
        fs::create_dir_all(h.path(dir)).map_err(|e| e.to_string())?;
    }
    for (path, contents) in [
        ("glyph-files/current/override.rs", "// CUSTOM_FILE_ICON\n"),
        (
            "glyph-files/current/extension.rs",
            "// CUSTOM_EXTENSION_ICON\n",
        ),
        ("glyph-files/current/empty.txt", "EMPTY_ICON_TEXT\n"),
        ("glyph-files/current/cursor-a", "CURSOR_A\n"),
        ("glyph-files/current/cursor-b", "CURSOR_B\n"),
        (
            "glyph-files/current/child/inside.rs",
            "// CUSTOM_CHILD_ICON\n",
        ),
        (
            "glyph-files/current/child/child-empty.txt",
            "CHILD_EMPTY_ICON\n",
        ),
        (
            "glyph-files/current/child/plain-child",
            "CHILD_CONDITION_ICON\n",
        ),
        ("glyph-files/parent.rs", "// CUSTOM_PARENT_ICON\n"),
        ("glyph-files/parent-empty.txt", "PARENT_EMPTY_ICON\n"),
        ("glyph-members/folder/member.rs", "VIRTUAL_NESTED_MEMBER\n"),
        ("glyph-members/plain.rs", "VIRTUAL_PLAIN_MEMBER\n"),
    ] {
        h.write_fixture(path, contents)?;
    }
    h.executable("glyph-files/current/exec-fixture", "#!/bin/sh\nexit 0\n")?;
    symlink("override.rs", h.path("glyph-files/current/link-fixture"))
        .map_err(|e| e.to_string())?;
    h.execute(
        "tar",
        &[
            "-cf".into(),
            text(&h.path("glyph-files/current/fixture.tar")),
            "-C".into(),
            text(&h.path("glyph-members")),
            "folder/".into(),
            "plain.rs".into(),
        ],
    )?;

    // v26.9.1 icon.rs merges prepend_* before default rules; files precede exts/conds.
    // th.icon:match(file, { hovered = file.is_hovered }) uses each folder's own cursor.
    // https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-config/src/theme/icon.rs
    // https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-actor/src/lives/file.rs
    // https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-plugin/src/theme/icon.rs
    let configured_theme = r#"[icon]
prepend_dirs = [
    { name = "current", text = "" },
    { name = "child", text = "" },
]
prepend_files = [
    { name = "override.rs", text = "" },
    { name = "parent.rs", text = "" },
    { name = "inside.rs", text = "" },
    { name = "empty.txt", text = "" },
    { name = "parent-empty.txt", text = "" },
    { name = "child-empty.txt", text = "" },
]
prepend_exts = [{ name = "rs", text = "" }]
prepend_conds = [
    { if = "hovered", text = "" },
    { if = "dir", text = "" },
    { if = "exec", text = "" },
    { if = "link", text = "" },
    { if = "!dir", text = "" },
]
"#;
    let no_icon_theme = "[icon]\nglobs = []\ndirs = []\nfiles = []\nexts = []\nconds = []\n";
    for (config, theme) in [
        ("yazi-glyphs", configured_theme),
        ("yazi-no-icons", no_icon_theme),
    ] {
        fs::create_dir_all(h.path(&format!("{config}/plugins"))).map_err(|e| e.to_string())?;
        symlink(
            h.repo.join("yazi-plugin/tern.yazi"),
            h.path(&format!("{config}/plugins/tern.yazi")),
        )
        .map_err(|e| e.to_string())?;
        for file in ["init.lua", "yazi.toml"] {
            fs::copy(
                h.path(&format!("yazi/{file}")),
                h.path(&format!("{config}/{file}")),
            )
            .map_err(|e| e.to_string())?;
        }
        h.write_fixture(&format!("{config}/theme.toml"), theme)?;
    }

    let client =
        h.launch_with_config(h.path("glyph-files/current"), true, h.path("yazi-glyphs"))?;
    h.click_file(&client, "child")?;
    for (folder, selector, name, glyph) in [
        (None, ROWS, "child", ""),
        (None, ROWS, "conditional-dir", ""),
        (None, ROWS, "override.rs", ""),
        (None, ROWS, "extension.rs", ""),
        (None, ROWS, "empty.txt", ""),
        (Some("parent"), PARENT_ROWS, "current", ""),
        (Some("parent"), PARENT_ROWS, "parent-dir", ""),
        (Some("parent"), PARENT_ROWS, "parent.rs", ""),
        (Some("parent"), PARENT_ROWS, "parent-empty.txt", ""),
        (Some("preview"), CHILD_ROWS, "inside.rs", ""),
        (Some("preview"), CHILD_ROWS, "child-empty.txt", ""),
        (Some("preview"), CHILD_ROWS, "plain-child", ""),
    ] {
        h.expect_row_icon(&client, folder, selector, name, json!(glyph))?;
    }

    h.case = "hover conditions update both departed and newly hovered rows";
    h.click_file(&client, "conditional-dir")?;
    h.expect_row_icon(&client, None, ROWS, "conditional-dir", json!(""))?;
    h.click_file(&client, "child")?;
    h.expect_row_icon(&client, None, ROWS, "conditional-dir", json!(""))?;
    h.click_file(&client, "cursor-a")?;
    let before = h.expect_row_icon(&client, None, ROWS, "cursor-a", json!(""))?;
    h.expect_row_icon(&client, None, ROWS, "cursor-b", json!(""))?;
    h.key("j")?;
    h.expect_state(
        &client,
        "keyboard hover changes icons without changing directory entries",
        |s| {
            hover(s) == text(&client.cwd.join("cursor-b"))
                && s["files"] == before["files"]
                && s["selected_urls"] == before["selected_urls"]
                && s["file_icons"]["cursor-a"] == ""
                && s["file_icons"]["cursor-b"] == ""
                && s["seq"].as_u64() > before["seq"].as_u64()
        },
    )?;
    h.expect_row_icon(&client, None, ROWS, "cursor-a", json!(""))?;
    h.expect_row_icon(&client, None, ROWS, "cursor-b", json!(""))?;
    h.click_file(&client, "empty.txt")?;
    h.expect_row_icon(&client, None, ROWS, "empty.txt", json!(""))?;

    h.case = "exec and link predicates come from real files; icon-only changes survive dedupe";
    h.click_file(&client, "override.rs")?;
    h.expect_row_icon(&client, None, ROWS, "override.rs", json!(""))?;
    h.expect_row_icon(&client, None, ROWS, "exec-fixture", json!(""))?;
    h.expect_row_icon(&client, None, ROWS, "link-fixture", json!(""))?;
    h.settled(&client, Duration::from_secs(1))?;
    for (mode, glyph) in [(0o600, ""), (0o700, "")] {
        let before = h.snapshot(&client);
        fs::set_permissions(
            client.cwd.join("exec-fixture"),
            fs::Permissions::from_mode(mode),
        )
        .map_err(|e| e.to_string())?;
        // No actor/ping/forced export: the surface must redraw and publish this
        // icon-only mutation with the exact same fields used by snapshot deduplication.
        h.expect_state(
            &client,
            "chmod changes only icon metadata and advances the snapshot",
            |s| {
                s["file_icons"]["exec-fixture"] == glyph
                    && s["seq"].as_u64() > before["seq"].as_u64()
                    && [
                        "cwd",
                        "files",
                        "file_dirs",
                        "parent",
                        "preview",
                        "hovered",
                        "selected",
                        "selected_urls",
                        "mode",
                        "marked_urls",
                        "filter",
                        "finder",
                        "tasks",
                    ]
                    .iter()
                    .all(|key| s[*key] == before[*key])
            },
        )?;
        h.expect_row_icon(&client, None, ROWS, "exec-fixture", json!(glyph))?;
    }
    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&client, &watched)?;

    h.case = "a separate all-empty theme returns nil icons, exported as false in all columns";
    // Theme files are loaded at session startup: do not assume dynamic reload support.
    let client =
        h.launch_with_config(h.path("glyph-files/current"), true, h.path("yazi-no-icons"))?;
    h.click_file(&client, "child")?;
    let snapshot = h.expect_state(
        &client,
        "every live entry has an explicit false icon",
        |s| {
            [None, Some("parent"), Some("preview")].iter().all(|scope| {
                let listing = scope.map_or(s, |key| &s[key]);
                let entries = names(listing, "files");
                !entries.is_empty()
                    && listing["file_icons"].as_object().is_some_and(|icons| {
                        icons.len() == entries.len()
                            && entries
                                .iter()
                                .all(|name| icons.get(name) == Some(&Value::Bool(false)))
                    })
            })
        },
    )?;
    for (folder, selector) in [
        (None, ROWS),
        (Some("parent"), PARENT_ROWS),
        (Some("preview"), CHILD_ROWS),
    ] {
        let listing = folder.map_or(&snapshot, |key| &snapshot[key]);
        for name in names(listing, "files") {
            h.expect_row_icon(&client, folder, selector, &name, json!(false))?;
        }
    }

    h.case = "only virtual archive member strings receive generic directory and file glyphs";
    h.click_file(&client, "fixture.tar")?;
    h.expect_row_icon(&client, None, ROWS, "fixture.tar", json!(false))?;
    h.expect_ui(
        "[data-role=\"yazi.preview\"] *",
        "real archive rows use generic glyphs even when Yazi has no real-file icon",
        |v| {
            [" folder/", " folder/member.rs", " plain.rs"]
                .iter()
                .all(|label| {
                    nodes(v)
                        .iter()
                        .any(|node| node["text"].as_str() == Some(*label))
                })
        },
    )?;
    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&client, &watched)?;
    Ok(())
}

fn metadata_footer_headers(h: &mut Harness) -> TestResult<()> {
    h.case = "private metadata fixtures";
    for dir in [
        "metadata",
        "metadata/many/nested",
        "metadata/contents/nested",
        "metadata/empty",
        "metadata/unreadable",
    ] {
        fs::create_dir_all(h.path(dir)).map_err(|e| e.to_string())?;
    }
    let large = format!("// {:<28}\n", "METADATA_PREVIEW_LINE").repeat(2048);
    if large.len() != 64 * 1024 {
        return Err("Private text preview fixture must contain exactly 65536 bytes".into());
    }
    h.write_fixture("metadata/large.rs", large)?;
    h.write_fixture("metadata/private.txt", b"private\n")?;
    h.write_fixture("metadata/contents/direct.bin", vec![b'a'; 137])?;
    h.write_fixture("metadata/contents/nested/child.bin", vec![b'b'; 211])?;
    h.write_fixture("metadata/contents/.hidden.bin", vec![b'c'; 19])?;
    h.write_fixture("metadata/regular.txt", vec![b'd'; 23])?;
    for index in 0..37 {
        h.write_fixture(&format!("metadata/many/direct-{index:02}.txt"), b"direct\n")?;
    }
    for index in 0..5 {
        h.write_fixture(
            &format!("metadata/many/nested/grandchild-{index}.txt"),
            b"grandchild\n",
        )?;
    }
    h.write_fixture("metadata/unreadable/not-empty.txt", b"not empty\n")?;
    for (path, mode) in [
        ("metadata", 0o700),
        ("metadata/large.rs", 0o640),
        ("metadata/private.txt", 0o600),
        ("metadata/many", 0o751),
        ("metadata/empty", 0o700),
        ("metadata/unreadable", 0o000),
    ] {
        fs::set_permissions(h.path(path), fs::Permissions::from_mode(mode))
            .map_err(|e| e.to_string())?;
    }
    let unreadable = h.path("metadata/unreadable");
    // Restore access even on an assertion failure so owned-artifact cleanup remains possible.
    let result = (|| {
        match fs::read_dir(&unreadable) {
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(error) => return Err(format!("Private unreadable fixture failed for the wrong reason: {error}")),
            Ok(_) => return Err("Unknown-count fixture requires an unprivileged runtime: mode 000 did not deny real directory loading".into()),
        }
        let client = h.launch(h.path("metadata"), true)?;
        h.expect_state(
            &client,
            "private current folder has seven authoritative immediate entries",
            |s| s["file_count"].as_u64() == Some(7) && names(s, "files").len() == 7,
        )?;

        h.case = "file footer/header exact metadata and bounded preview";
        h.click_file(&client, "large.rs")?;
        h.expect_file_metadata(
            &client,
            "large.rs",
            (65536, "64.0 KB"),
            "-rw-r-----",
            "rust",
        )?;
        h.expect_ui(
            PREVIEW,
            "large real text fixture produces a preview rather than an unavailable fallback",
            |v| ui_text(v).contains("METADATA_PREVIEW_LINE"),
        )?;
        h.shot("09-file-metadata")?;

        h.case = "same-hover permission-only snapshots preserve uppercase special-mode semantics";
        let large_path = text(&client.cwd.join("large.rs"));
        let original_seq = h.snapshot(&client)["seq"]
            .as_u64()
            .ok_or("Large file snapshot has no sequence")?;
        fs::set_permissions(
            client.cwd.join("large.rs"),
            fs::Permissions::from_mode(0o4640),
        )
        .map_err(|e| e.to_string())?;
        let special = h.expect_state(
            &client,
            "real chmod 04640 emits a new same-URL metadata-only snapshot",
            |s| {
                hover(s) == large_path
                    && s["seq"].as_u64().is_some_and(|seq| seq > original_seq)
                    && s["hovered"]["size"].as_u64() == Some(65536)
                    && s["hovered"]["permissions"] == "-rwSr-----"
            },
        )?;
        h.expect_file_metadata(
            &client,
            "large.rs",
            (65536, "64.0 KB"),
            "-rwSr-----",
            "rust",
        )?;
        h.shot("09-special-permissions")?;
        let special_seq = special["seq"]
            .as_u64()
            .ok_or("Special-mode snapshot has no sequence")?;
        fs::set_permissions(
            client.cwd.join("large.rs"),
            fs::Permissions::from_mode(0o640),
        )
        .map_err(|e| e.to_string())?;
        h.expect_state(
            &client,
            "restoring 0640 updates permissions without a hover move",
            |s| {
                hover(s) == large_path
                    && s["seq"].as_u64().is_some_and(|seq| seq > special_seq)
                    && s["hovered"]["permissions"] == "-rw-r-----"
            },
        )?;
        h.expect_file_metadata(
            &client,
            "large.rs",
            (65536, "64.0 KB"),
            "-rw-r-----",
            "rust",
        )?;

        h.case = "file-to-file selection keeps exact metadata without selection counters";
        h.key("Space")?;
        let selected_path = text(&client.cwd.join("large.rs"));
        let private_path = text(&client.cwd.join("private.txt"));
        h.expect_state(
            &client,
            "Space selects large file and advances the actual Yazi cursor",
            |s| selected(s) == BTreeSet::from([selected_path.clone()]) && hover(s) == private_path,
        )?;
        h.expect_file_metadata(&client, "private.txt", (8, "8 B"), "-rw-------", "text")?;
        h.key("Escape")?;
        h.expect_state(
            &client,
            "Escape clears the real selection before metadata navigation",
            |s| selected(s).is_empty(),
        )?;
        h.key("k")?;
        h.expect_file_metadata(
            &client,
            "large.rs",
            (65536, "64.0 KB"),
            "-rw-r-----",
            "rust",
        )?;

        h.case = "file-to-folder shows recursive content bytes and uncapped immediate count";
        h.click_file(&client, "many")?;
        let many_path = text(&client.cwd.join("many"));
        let many = h.expect_state(&client, "38 direct children are counted before the 30-entry preview cap; grandchildren are excluded", |s| {
            hover(s) == many_path
                && s["hovered"]["dir"] == true
                && s["hovered"]["permissions"] == "drwxr-x--x"
                && s["preview"]["cwd"] == many_path
                && s["preview"]["file_count"].as_u64() == Some(38)
                && names(&s["preview"], "files").len() == 30
        })?;
        h.expect_metadata_text(
            "[data-role=\"yazi.column.preview\"] > .sf-card-head",
            "many · 38 entries",
        )?;
        h.expect_state(
            &client,
            "folder content bytes include descendants without directory inodes",
            |s| hover(s) == many_path && s["hovered"]["size"].as_u64() == Some(314),
        )?;
        h.expect_metadata_text("[data-role=\"yazi.status-size\"]", "314 B")?;
        h.expect_metadata_text("[data-role=\"yazi.status-permissions\"]", "drwxr-x--x")?;
        h.expect_metadata_text("[data-role=\"yazi.status-name\"]", "many")?;
        h.expect_ui(
            CHILD_ROWS,
            "mounted child pool contains only authoritative capped immediate entries",
            |v| {
                !nodes(v).is_empty()
                    && nodes(v).len() <= 30
                    && nodes(v).iter().all(|n| {
                        n["title"].as_str().is_some_and(|p| {
                            Path::new(p).parent() == Some(client.cwd.join("many").as_path())
                                && names(&many["preview"], "files").contains(
                                    &Path::new(p)
                                        .file_name()
                                        .unwrap()
                                        .to_string_lossy()
                                        .into_owned(),
                                )
                        })
                    })
            },
        )?;
        let children = names(&many["preview"], "files");
        h.expect_metadata_selection(
            &format!("{CHILD_ROWS}.sel"),
            &client.cwd.join("many").join(&children[0]),
        )?;
        h.expect_metadata_selection(&format!("{ROWS}.sel"), &client.cwd.join("many"))?;
        h.expect_metadata_header_layout("yazi.column.preview", ".sf-list-scroll.max")?;
        h.expect_ui(
            "[data-role=\"yazi.preview-metadata\"]",
            "directory preview shows its kind, mtime and permissions",
            |v| {
                let label = ui_text(v);
                nodes(v).len() == 1 && label.contains("directory") && label.contains("drwx")
            },
        )?;
        h.expect_ui(
            "[data-role=\"yazi.preview-mtime\"]",
            "directory preview shows its local modification time",
            |v| nodes(v).len() == 1 && is_local_minute(&ui_text(v)),
        )?;
        h.shot("10-folder-count-before-cap")?;

        h.case =
            "folder footer sums 137 plus nested 211 plus hidden 19 without changing Yazi state";
        h.click_file(&client, "regular.txt")?;
        h.expect_state(&client, "regular file exports its real 23 bytes", |s| {
            s["hovered"]["size"].as_u64() == Some(23)
        })?;
        h.expect_metadata_text("[data-role=\"yazi.status-size\"]", "23 B")?;
        h.key("Space")?;
        h.expect_state(
            &client,
            "folder usage starts with a real retained file selection",
            |s| selected(s) == BTreeSet::from([text(&client.cwd.join("regular.txt"))]),
        )?;
        h.click_file(&client, "contents")?;
        let calculation_state = h.expect_state(
            &client,
            "folder calculation starts from a coherent actor/listing pair",
            |s| hover(s) == text(&client.cwd.join("contents")) && s["hovered"]["dir"] == true,
        )?;
        let contents_path = text(&client.cwd.join("contents"));
        h.expect_state(
            &client,
            "recursive folder usage becomes 367 bytes with hidden files included",
            |s| {
                hover(s) == contents_path
                    && s["hovered"]["size"].as_u64() == Some(367)
                    && [
                        "cwd",
                        "files",
                        "cursor",
                        "filter",
                        "finder",
                        "mode",
                        "selected_urls",
                        "marked_urls",
                    ]
                    .iter()
                    .all(|key| s[*key] == calculation_state[*key])
            },
        )?;
        h.expect_metadata_text("[data-role=\"yazi.status-size\"]", "367 B")?;
        h.expect_metadata_text("[data-role=\"yazi.status-name\"]", "contents")?;
        // Start repeated real directory calculations and immediately leave their URL.
        // No injected callbacks or forced snapshots substitute for async ownership.
        for _ in 0..8 {
            h.click_node(ROWS, "many", false)?;
            h.click_node(ROWS, "regular.txt", false)?;
        }
        h.expect_state(
            &client,
            "old folder completion cannot replace the current regular-file bytes",
            |s| {
                hover(s) == text(&client.cwd.join("regular.txt"))
                    && s["hovered"]["size"].as_u64() == Some(23)
            },
        )?;
        let stable = h.snapshot(&client);
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            let current = h.snapshot(&client);
            if !current.is_null()
                && (current["hovered"] != stable["hovered"]
                    || [
                        "files",
                        "cursor",
                        "filter",
                        "finder",
                        "selected_urls",
                        "marked_urls",
                    ]
                    .iter()
                    .any(|key| current[*key] != stable[*key]))
            {
                return Err(format!(
                    "Stale directory computation changed current file: {current}"
                ));
            }
            h.expect_metadata_text("[data-role=\"yazi.status-size\"]", "23 B")?;
            thread::sleep(POLL);
        }
        h.key("Escape")?;
        h.expect_state(
            &client,
            "Escape clears the retained size-fixture selection",
            |s| selected(s).is_empty(),
        )?;

        h.case = "loaded empty folder is zero rather than unknown";
        h.click_file(&client, "empty")?;
        let empty_path = text(&client.cwd.join("empty"));
        h.expect_state(
            &client,
            "real empty directory has an authoritative loaded zero count",
            |s| {
                hover(s) == empty_path
                    && s["preview"]["cwd"] == empty_path
                    && s["preview"]["file_count"].as_u64() == Some(0)
                    && s["hovered"]["size"].as_u64() == Some(0)
                    && names(&s["preview"], "files").is_empty()
                    && s["hovered"]["permissions"] == "drwx------"
            },
        )?;
        h.expect_metadata_text(
            "[data-role=\"yazi.column.preview\"] > .sf-card-head",
            "empty · 0 entries",
        )?;
        h.expect_metadata_text("[data-role=\"yazi.status-size\"]", "0 B")?;
        h.expect_metadata_text("[data-role=\"yazi.status-permissions\"]", "drwx------")?;
        h.expect_metadata_header_layout("yazi.column.preview", ".sf-card-body")?;

        h.case = "failed real directory loading keeps count unknown, never fake zero";
        h.click_file(&client, "unreadable")?;
        let unreadable_path = text(&unreadable);
        let first = h.expect_state(
            &client,
            "mode 000 directory hover reports actual permissions",
            |s| {
                hover(s) == unreadable_path
                    && s["hovered"]["dir"] == true
                    && s["hovered"]["permissions"] == "d---------"
            },
        )?;
        let initial_seq = first["seq"]
            .as_u64()
            .ok_or("Unreadable hover snapshot has no sequence")?;
        // Revisit after a real cursor hop; managed idle sessions do not poll snapshots.
        h.key("j")?;
        h.expect_state(
            &client,
            "cursor leaves the failed directory for the real large file",
            |s| hover(s) == text(&client.cwd.join("large.rs")),
        )?;
        h.key("k")?;
        h.expect_state(
            &client,
            "later live snapshot retains unknown count for genuine PermissionDenied loading",
            |s| {
                hover(s) == unreadable_path
                    && s["seq"].as_u64().is_some_and(|seq| seq > initial_seq)
                    && (s["preview"].is_null() || s["preview"]["cwd"] == unreadable_path)
                    && s["preview"].get("file_count").is_none()
            },
        )?;
        h.expect_metadata_text(
            "[data-role=\"yazi.column.preview\"] > .sf-card-head",
            "unreadable · — entries",
        )?;
        h.expect_metadata_text("[data-role=\"yazi.status-permissions\"]", "d---------")?;
        h.expect_metadata_header_layout("yazi.column.preview", ".sf-card-body")?;
        h.expect_ui(
            "[data-role=\"yazi.column.preview\"]",
            "failed loading never masquerades as a loaded empty directory",
            |v| {
                let label = ui_text(v);
                !label.contains("0 entries") && !label.contains("(empty directory)")
            },
        )?;
        h.shot("11-unreadable-folder-unknown")?;

        h.case = "folder-to-file restores only exact hovered metadata";
        h.click_file(&client, "private.txt")?;
        h.expect_file_metadata(&client, "private.txt", (8, "8 B"), "-rw-------", "text")?;
        let watched = descendants(client.supervisor);
        h.owned.extend(watched.iter().copied());
        h.key("q")?;
        h.removed(&client, &watched)?;
        Ok(())
    })();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("Cannot restore private unreadable fixture permissions: {e}"))?;
    result
}

fn theme_visible(node: &Value) -> bool {
    node["rect"][2].as_f64().unwrap_or(0.0) > 0.0 && node["rect"][3].as_f64().unwrap_or(0.0) > 0.0
}

fn extended_preview_flow(h: &mut Harness) -> TestResult<()> {
    let preview_dir = h.path("previews");
    fs::create_dir_all(&preview_dir).map_err(|e| e.to_string())?;

    let mut sample_pdf = fs::read("/usr/share/cups/data/default.pdf").unwrap_or_else(|_| {
        b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Count 1/Kids[3 0 R]>>endobj\n3 0 obj<</Type/Page/MediaBox[0 0 612 792]/Parent 2 0 R/Resources<<>>>>endobj\nxref\n0 4\n0000000000 65535 f \n0000000010 00000 n \n0000000053 00000 n \n0000000102 00000 n \ntrailer<</Size 4/Root 1 0 R>>\nstartxref\n178\n%%EOF\n".to_vec()
    });
    // Comment lines past %%EOF push the file over the old 1 MiB text-preview cap; poppler
    // ignores them, so the cover must still render.
    let padding = format!("%{}\n", "p".repeat(1022));
    while sample_pdf.len() <= 1 << 20 {
        sample_pdf.extend_from_slice(padding.as_bytes());
    }
    fs::write(preview_dir.join("sample.pdf"), sample_pdf).map_err(|e| e.to_string())?;
    fs::write(
        preview_dir.join("scene.wrl"),
        "#VRML V2.0 utf8\nShape { appearance Appearance { material Material { diffuseColor 1 0 0 } } geometry Box { size 2 2 2 } }\n",
    )
    .map_err(|e| e.to_string())?;

    // An 8x8 red RGB PNG.
    let pixel: [u8; 75] = [
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 8, 0, 0, 0, 8, 8, 2,
        0, 0, 0, 75, 109, 41, 220, 0, 0, 0, 18, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 128,
        21, 97, 23, 29, 180, 18, 0, 40, 255, 63, 193, 110, 236, 223, 97, 0, 0, 0, 0, 73, 69, 78,
        68, 174, 66, 96, 130,
    ];
    fs::write(preview_dir.join("pixel.png"), pixel).map_err(|e| e.to_string())?;
    fs::write(
        preview_dir.join("main.rs"),
        "fn main() {\n    let marker = \"CODE_PREVIEW_MARKER\";\n}\n",
    )
    .map_err(|e| e.to_string())?;

    let client = h.launch(preview_dir, true)?;

    h.case = "raster image and source code previews";
    h.click_file(&client, "pixel.png")?;
    h.expect_ui(
        &format!("{PREVIEW} [data-role=\"yazi.image\"] img"),
        "PNG preview draws the uploaded image",
        |v| nodes(v).len() == 1 && theme_visible(nodes(v)[0]),
    )?;
    h.click_file(&client, "main.rs")?;
    h.expect_ui(PREVIEW, "Rust source preview shows the file text", |v| {
        ui_text(v).contains("CODE_PREVIEW_MARKER")
    })?;

    h.case = "real PDF preview renders metadata card and cover image node";
    h.click_file(&client, "sample.pdf")?;
    h.expect_ui(
        &format!("{PREVIEW} [data-role=\"yazi.pdf-preview\"]"),
        "PDF preview renders the actual pdfinfo page count",
        |v| nodes(v).len() == 1 && theme_visible(nodes(v)[0]) && ui_text(v).contains("1 pages"),
    )?;
    // The column must give the zero-basis image node real height.
    h.expect_ui(
        &format!("{PREVIEW} [data-role=\"yazi.pdf-preview\"] [data-role=\"yazi.image\"] img.sf-img-frame"),
        "PDF preview renders a visible rasterized cover image",
        |v| nodes(v).len() == 1 && theme_visible(nodes(v)[0]),
    )?;
    h.shot("15-pdf-preview")?;

    h.case = "text 3D model preview renders Tern's live model figure";
    h.click_file(&client, "scene.wrl")?;
    // A VRML box tessellates to 12 triangles only when the scene really parsed.
    h.expect_ui(
        &format!("{PREVIEW} [data-role=\"yazi.model-preview\"] .m3d-facts"),
        "VRML preview renders a parsed model figure",
        |v| {
            nodes(v).len() == 1 && theme_visible(nodes(v)[0]) && ui_text(v).contains("12 triangles")
        },
    )?;
    h.shot("16-model-preview")?;

    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&client, &watched)
}

fn shortcut_flow(h: &mut Harness, first: &Client) -> TestResult<()> {
    h.case = "yank copy and cut register downstream paste outcomes";
    for (name, key, dest, source_remains) in [
        ("copy.txt", "y", "copydest", true),
        ("cut.txt", "x", "cutdest", false),
    ] {
        h.click_file(first, name)?;
        h.key(key)?;
        h.click_node(ROWS, dest, true)?;
        h.expect_state(
            first,
            "UI navigation reaches isolated paste destination",
            |s| s["cwd"] == text(&first.cwd.join(dest)),
        )?;
        // Keyboard 'p' triggers paste action.
        h.key("p")?;
        let source = first.cwd.join(name);
        let destination = first.cwd.join(dest).join(name);
        h.wait("yank register drives actual copy/cut filesystem task", |_| {
            let contents = fs::read_to_string(&destination).unwrap_or_default();
            let expected = if source_remains { "COPY_ORIGINAL\n" } else { "CUT_ORIGINAL\n" };
            if contents == expected && source.exists() == source_remains { Ok(Some(())) }
            else { Err(format!("destination {} contents {contents:?}; source exists {} expected {source_remains}", text(&destination), source.exists())) }
        })?;
        h.key("h")?;
        h.expect_state(first, "return from paste destination", |s| {
            s["cwd"] == text(&first.cwd)
        })?;
    }
    // Test unyank ('Y'): yank copy.txt, then unyank ('Y'), verify yank is cleared.
    let unyank_dir = first.cwd.join("unyankdest");
    fs::create_dir_all(&unyank_dir).map_err(|e| e.to_string())?;
    // The new directory shifts the rows; click only after the listing shows it,
    // or the click lands on the row that used to be at copy.txt's position.
    let unyank_row = row_selector(ROWS, "unyankdest");
    h.wait("unyankdest row is listed before clicking", |h| {
        Ok(nodes(&h.tree(&unyank_row)?)
            .iter()
            .any(|node| theme_visible(node))
            .then_some(()))
    })?;
    h.click_file(first, "copy.txt")?;
    h.key("y")?;
    h.key("Y")?;
    h.click_node(ROWS, "unyankdest", true)?;
    h.expect_state(
        first,
        "UI navigation reaches unyankdest after unyank",
        |s| s["cwd"] == text(&unyank_dir),
    )?;
    h.key("p")?;
    thread::sleep(Duration::from_millis(200));
    let copied_in_dest = unyank_dir.join("copy.txt");
    if copied_in_dest.exists() {
        return Err(
            "Unyank failed: copy.txt was pasted into unyankdest despite unyank ('Y')".to_string(),
        );
    }
    h.key("h")?;
    let _ = fs::remove_dir_all(&unyank_dir);
    h.expect_state(first, "return from unyankdest after unyank check", |s| {
        s["cwd"] == text(&first.cwd)
    })?;
    h.case = "tab management via tt, 1-9, brackets and Ctrl+c";
    // First t sets pending leader; does not create tab yet.
    h.key("t")?;
    // Second t creates tab 2 in current working directory.
    h.key("t")?;
    h.expect_ui(
        "[data-role=\"yazi.tab-bar\"]",
        "tab bar appears when multiple tabs exist",
        |v| nodes(v).len() == 1,
    )?;
    // In tab 2, navigate into child directory.
    h.click_node(ROWS, "child", true)?;
    h.expect_state(first, "tab 2 navigates into child directory", |s| {
        s["cwd"] == text(&first.cwd.join("child"))
    })?;
    h.expect_ui(
        "[data-role=\"yazi.tab-bar\"]",
        "tab labels follow real directories",
        |v| ui_text(v).contains("1 files") && ui_text(v).contains("2 child"),
    )?;
    h.click_node(
        "[data-role=\"yazi.tab\"], [data-role=\"yazi.tab-active\"]",
        "1 files",
        false,
    )?;
    h.expect_state(first, "mouse switches to tab one", |s| {
        s["cwd"] == text(&first.cwd) && s["tabs"]["idx"] == 1
    })?;
    // Switch to tab 1 (number '1') -> restores root cwd!
    h.key("1")?;
    h.expect_state(first, "switch to tab 1 restores root cwd", |s| {
        s["cwd"] == text(&first.cwd)
    })?;
    // Switch to tab 2 (number '2') -> restores child cwd!
    h.key("2")?;
    h.expect_state(first, "switch to tab 2 restores child cwd", |s| {
        s["cwd"] == text(&first.cwd.join("child"))
    })?;
    // Relative switch prev '[' -> tab 1 root cwd!
    h.key("[")?;
    h.expect_state(first, "relative switch [ to tab 1 restores root cwd", |s| {
        s["cwd"] == text(&first.cwd)
    })?;
    // Relative switch next ']' -> tab 2 child cwd!
    h.key("]")?;
    h.expect_state(
        first,
        "relative switch ] to tab 2 restores child cwd",
        |s| s["cwd"] == text(&first.cwd.join("child")),
    )?;
    h.key("{")?;
    h.expect_state(first, "swap left moves child tab to first index", |s| {
        s["tabs"]["idx"] == 1 && s["tabs"]["items"][0]["name"] == "child"
    })?;
    h.key("}")?;
    h.expect_state(first, "swap right restores child tab index", |s| {
        s["tabs"]["idx"] == 2 && s["tabs"]["items"][1]["name"] == "child"
    })?;
    h.shot("tabs-keyboard-and-mouse")?;
    // Close tab 2 via Ctrl+c -> returns to tab 1 root cwd!
    h.cli(&["send", &first.owner.to_string(), "keys", "ctrl+c"])?;
    h.expect_state(first, "closing tab 2 returns to tab 1 root cwd", |s| {
        s["cwd"] == text(&first.cwd)
    })?;
    h.expect_ui(
        "[data-role=\"yazi.tab-bar\"]",
        "tab bar hides when only single tab remains",
        |v| nodes(v).is_empty(),
    )?;
    h.case = "interactive create ('a') and rename ('r') shortcuts";
    // Press 'a' to enter create mode
    h.key("a")?;
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.create\"]",
        "create prompt bar opens with active badge",
        |v| nodes(v).len() == 1,
    )?;
    // Cancel with Escape
    h.key("Escape")?;
    h.key("Escape")?;
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.create\"]",
        "Escape dismisses create bar",
        |v| nodes(v).is_empty(),
    )?;
    // Re-enter create mode and submit a new file
    h.key("a")?;
    h.paste(first.owner, "created-probe.txt")?;
    h.key("Enter")?;
    let created_path = first.cwd.join("created-probe.txt");
    h.wait(
        "created file exists on filesystem and appears in Yazi listing",
        |h| {
            let snapshot = h.snapshot(first);
            let files = names(&snapshot, "files");
            if created_path.exists() && files.contains(&"created-probe.txt".to_string()) {
                Ok(Some(()))
            } else {
                Err(format!(
                    "created file exists: {}, files listing: {files:?}",
                    created_path.exists()
                ))
            }
        },
    )?;
    // Hover the newly created file and press 'r' to rename
    h.click_file(first, "created-probe.txt")?;
    h.key("r")?;
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.rename\"]",
        "rename prompt bar opens with prefilled name",
        |v| ui_text(v).contains("created-probe.txt"),
    )?;
    // First Escape leaves insert mode and keeps the line open; `x` then deletes the
    // character under the caret in normal mode; the second Escape cancels the rename.
    h.key("Escape")?;
    h.expect_ui(
        "[data-role=\"yazi.command-vi\"]",
        "first Escape switches the rename line to normal mode",
        |v| ui_text(v).contains("NORMAL"),
    )?;
    h.key("x")?;
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.rename\"]",
        "normal-mode x deletes the character under the caret",
        |v| ui_text(v) == "created-prob.txt",
    )?;
    h.key("Escape")?;
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.rename\"]",
        "second Escape cancels the rename line",
        |v| nodes(v).is_empty(),
    )?;
    // Re-enter rename mode, clear, type new name, and submit
    h.key("r")?;
    h.clear_line(first.owner)?;
    for key in ["q", "d", "j", "k", "y"] {
        h.key(key)?;
    }
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.rename\"]",
        "shortcut letters stay inside rename editor",
        |v| ui_text(v) == "qdjky",
    )?;
    h.clear_line(first.owner)?;
    h.paste(first.owner, "renamed-probe.txt")?;
    h.key("Enter")?;
    let renamed_path = first.cwd.join("renamed-probe.txt");
    h.wait(
        "renamed file exists on filesystem and original no longer exists",
        |h| {
            let snapshot = h.snapshot(first);
            let files = names(&snapshot, "files");
            if renamed_path.exists()
                && !created_path.exists()
                && files.contains(&"renamed-probe.txt".to_string())
                && hover(&snapshot) == text(&renamed_path)
            {
                Ok(Some(()))
            } else {
                Err(format!(
                    "renamed exists: {}, old exists: {}, files: {files:?}",
                    renamed_path.exists(),
                    created_path.exists()
                ))
            }
        },
    )?;
    let _ = fs::remove_file(renamed_path);

    h.case = "create nested directories, POSIX special filenames and failure preservation";
    for name in ["nested/newdir/", "雪's\\literal.txt", "trailing\\"] {
        h.key("a")?;
        h.paste(first.owner, name)?;
        h.key("Enter")?;
        let target = first.cwd.join(name.trim_end_matches('/'));
        h.expect_state(first, "created target is authoritative hover", |s| {
            hover(s) == text(&target)
        })?;
        if !target.exists()
            || (name.ends_with('/') && !target.is_dir())
            || (name.ends_with('\\') && !target.is_file())
        {
            return Err(format!("Incorrect created target {}", text(&target)));
        }
        if name.contains('/') {
            h.key("h")?;
            h.expect_state(first, "return to root after nested reveal", |s| {
                s["cwd"] == text(&first.cwd)
            })?;
        }
    }
    h.click_file(first, "nested")?;
    // The snapshot file leads the surface; rename targets the surface's hover, so wait for it.
    h.expect_ui(
        CHILD_ROWS,
        "the child column follows the hovered directory",
        |v| ui_text(v).contains("newdir"),
    )?;
    h.key("r")?;
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.rename\"]",
        "rename edits the clicked directory",
        |v| ui_text(v) == "nested",
    )?;
    h.clear_line(first.owner)?;
    h.paste(first.owner, "renamed-directory")?;
    h.key("Enter")?;
    h.expect_state(
        first,
        "directory rename preserves contents and hover",
        |s| hover(s) == text(&first.cwd.join("renamed-directory")),
    )?;
    if !first.cwd.join("renamed-directory/newdir").is_dir() || first.cwd.join("nested").exists() {
        return Err("Directory rename lost nested contents".into());
    }
    h.click_file(first, "copy.txt")?;
    let original = fs::read(first.cwd.join("copy.txt")).map_err(|e| e.to_string())?;
    for (op, name) in [
        ("create", "copy.txt"),
        ("rename", "beta.txt"),
        ("create", "copy.txt/blocked"),
        ("create", "/"),
    ] {
        h.key(if op == "create" { "a" } else { "r" })?;
        if op == "rename" {
            h.clear_line(first.owner)?;
        }
        h.paste(first.owner, name)?;
        h.key("Enter")?;
        if matches!(name, "copy.txt" | "beta.txt") {
            // An existing destination is Yazi's `Overwrite file?`; cancelling returns to the
            // line, which Escape twice then closes. Nothing was overwritten.
            h.expect_ui(
                "[data-role=\"yazi.overwrite-confirm\"]",
                "an existing destination asks before overwriting",
                |v| {
                    let text = ui_text(v);
                    text.contains("Overwrite file?")
                        && text.contains("Will overwrite the following file:")
                        && text.contains(&format!("/{name}"))
                },
            )?;
            for _ in 0..3 {
                h.key("Escape")?;
            }
        }
        // Notices live in the surface's input row, outside the file lists, so the
        // observable contract is the authoritative no-op: cursor, listing and
        // bytes stay exactly as they were.
        h.expect_state(
            first,
            "rejected operation leaves authoritative state untouched",
            |s| {
                hover(s) == text(&first.cwd.join("copy.txt"))
                    && names(s, "files").contains(&"copy.txt".to_string())
                    && names(s, "files").contains(&"beta.txt".to_string())
            },
        )?;
        h.expect_ui(
            "[data-role=\"yazi.input-row\"]",
            "rejected operation closes the prompt without pretending success",
            |v| ui_text(v).trim().is_empty(),
        )?;
        if fs::read(first.cwd.join("copy.txt")).map_err(|e| e.to_string())? != original
            || fs::read_to_string(first.cwd.join("beta.txt")).map_err(|e| e.to_string())?
                != "BETA_CONTENT\n"
        {
            return Err("Rejected operation changed an existing file".into());
        }
    }
    h.shot("create-rename-collisions")?;

    Ok(())
}

#[test]
fn real_browser_shortcuts() {
    let mut harness = Harness::new().expect("Real shortcut runtime setup");
    let result = harness
        .launch(harness.path("files"), false)
        .and_then(|client| {
            shortcut_flow(&mut harness, &client)?;
            harness.shot("shortcuts-complete")?;
            let watched = descendants(client.supervisor);
            harness.owned.extend(watched.iter().copied());
            harness.key("q")?;
            harness.removed(&client, &watched)
        });
    match result {
        Ok(()) => {
            harness.success = true;
            println!("Real shortcut cases passed.");
        }
        Err(error) => {
            harness.diagnostics(&error);
            panic!(
                "Shortcut case '{}' failed: {error}; artifacts {}",
                harness.case,
                text(&harness.root)
            );
        }
    }
}

fn conclude(harness: &mut Harness, result: TestResult<()>) {
    match result {
        Ok(()) => {
            harness.success = true;
            println!("All real interaction cases passed (owned GUI, isolated daemon).");
        }
        Err(error) => {
            harness.diagnostics(&error);
            panic!(
                "Interaction case '{}' failed: {error}; retained artifacts {}",
                harness.case,
                text(&harness.root)
            );
        }
    }
}

#[test]
fn real_browser_interactions() {
    let mut harness = Harness::new()
        .unwrap_or_else(|error| panic!("Real interaction prerequisites/setup failed: {error}"));
    let result = interaction_flow(&mut harness)
        .and_then(|()| glyph_interaction_flow(&mut harness))
        .and_then(|()| metadata_footer_headers(&mut harness))
        .and_then(|()| extended_preview_flow(&mut harness));
    conclude(&mut harness, result);
}

#[test]
fn real_window_interactions() {
    let mut harness = Harness::new()
        .unwrap_or_else(|error| panic!("Real interaction prerequisites/setup failed: {error}"));
    let result = window_flow(&mut harness);
    conclude(&mut harness, result);
}

/// Layout theme ids in picker order.
const LAYOUT_THEMES: [&str; 8] = [
    "current",
    "aurora",
    "editorial",
    "amber",
    "blueprint",
    "brutalist",
    "zen",
    "swiss",
];
const THEME_SHOTS: &str = "/tmp/tyi-inline-p1";
const PICKER: &str = "[data-role=\"yazi.theme-picker\"]";

fn theme_scope(id: &str) -> String {
    format!("[data-role^=\"yazi.theme.{id}\"]")
}

fn first_rect(value: &Value) -> [f64; 4] {
    let rect = nodes(value)
        .first()
        .map(|node| node["rect"].clone())
        .unwrap_or(Value::Null);
    [0, 1, 2, 3].map(|at| rect[at].as_f64().unwrap_or(0.0))
}

/// Surface-only behavior: the layout theme picker and re-layout on pane size changes.
fn surface_flow(h: &mut Harness) -> TestResult<()> {
    h.case = "T opens the layout theme picker; Escape restores the previewed theme";
    let client = h.launch(h.path("files"), true)?;
    let settings = h.path("config/tern-yazi/settings.json");
    h.key("T")?;
    h.expect_ui(
        &format!("{PICKER} .sf-item"),
        "the picker lists every layout theme",
        |v| {
            let rows: Vec<_> = nodes(v).iter().map(|node| ui_text(node)).collect();
            [
                "Current",
                "Aurora Glass",
                "Editorial Paper",
                "Amber Ledger",
                "Blueprint Draft",
            ]
            .iter()
            .chain(&["Brutalist Block", "Zen Minimal", "Swiss Grid"])
            .all(|title| rows.iter().any(|row| row.starts_with(title)))
        },
    )?;
    h.key("j")?;
    h.expect_ui(
        &theme_scope("aurora"),
        "j previews the next theme live",
        |v| !nodes(v).is_empty(),
    )?;
    h.key("Escape")?;
    h.expect_ui(PICKER, "Escape closes the picker", |v| nodes(v).is_empty())?;
    h.expect_ui(
        &theme_scope("current"),
        "Escape restores the original theme",
        |v| !nodes(v).is_empty(),
    )?;
    if settings.exists() {
        return Err("Escape persisted a theme choice".into());
    }

    h.case = "the picker switches all 8 themes in light and dark appearances";
    for (appearance, tern_theme) in [("light", "light-catppuccin"), ("dark", "dark-catppuccin")] {
        // The isolated window follows the system appearance; pin it through the light slot.
        h.ctl("theme", &["light", tern_theme])?;
        h.wait(&format!("Tern shows the {appearance} appearance"), |h| {
            Ok((h.ctl("state", &[])?["theme"] == appearance).then_some(()))
        })?;
        for (index, id) in LAYOUT_THEMES.iter().enumerate() {
            if index > 0 {
                for key in ["T", "j", "Enter"] {
                    h.key(key)?;
                }
                h.wait(&format!("Enter persists {id}"), |_| {
                    Ok(read_json(&settings)
                        .filter(|s| s["theme"] == *id)
                        .map(|_| ()))
                })?;
            }
            h.expect_ui(PICKER, "the picker is closed", |v| nodes(v).is_empty())?;
            h.expect_ui(
                &format!("{} {ROWS}", theme_scope(id)),
                &format!("{id} draws the file rows"),
                |v| nodes(v).iter().any(|node| theme_visible(node)),
            )?;
            h.keep_shot(&format!("{id}-{appearance}"), Path::new(THEME_SHOTS))?;
        }
        h.key("T")?;
        for _ in 1..LAYOUT_THEMES.len() {
            h.key("k")?;
        }
        h.key("Enter")?;
        h.wait("the picker returns to the first theme", |_| {
            Ok(read_json(&settings)
                .filter(|s| s["theme"] == "current")
                .map(|_| ()))
        })?;
    }
    h.ctl("theme", &["light", "light-catppuccin"])?;

    h.case = "the surface re-lays out when its pane is split and restored";
    // Tern keeps the pane tty's winsize while a surface is open, so the pane's real size comes
    // from the window state; assert both the drawn layout and Yazi's PTY.
    let pane_size = h.settled(&client, Duration::ZERO)?;
    let full = first_rect(&h.tree(SURFACE_BROWSER)?);
    for (side, narrower) in [("down", false), ("right", true)] {
        let created = h.cli(&[
            "split",
            &client.owner.to_string(),
            side,
            "--json",
            "--keep-open",
            "--",
            "sleep",
            "600",
        ])?;
        let created: Value = serde_json::from_str(&created).map_err(|e| e.to_string())?;
        let other = created["block"].as_u64().ok_or("Split returned no block")?;
        h.expect_ui(
            SURFACE_BROWSER,
            &format!("split {side} re-lays out the browser"),
            |v| {
                let rect = first_rect(v);
                if narrower {
                    rect[2] > 0.0 && rect[2] < full[2] * 0.75 && (rect[3] - full[3]).abs() < 2.0
                } else {
                    rect[3] > 0.0 && rect[3] < full[3] * 0.75 && (rect[2] - full[2]).abs() < 2.0
                }
            },
        )?;
        h.expect_ui(ROWS, "file rows stay visible in the smaller pane", |v| {
            nodes(v).iter().any(|node| theme_visible(node))
        })?;
        // Tern freezes the pane tty; the window's own state is the row source Yazi's PTY follows.
        h.wait(
            "Yazi's PTY follows the split pane's rows and columns",
            |h| {
                let Some(real) = h.pane_size(client.owner)? else {
                    return Ok(None);
                };
                Ok((real != pane_size && tty_size(client.backend)? == real).then_some(()))
            },
        )?;
        h.keep_shot(&format!("resize-split-{side}"), Path::new(THEME_SHOTS))?;
        h.cli(&["close", &other.to_string()])?;
        h.focus(client.owner)?;
        h.expect_ui(
            SURFACE_BROWSER,
            "the browser grows back to the full pane",
            |v| {
                let rect = first_rect(v);
                (rect[2] - full[2]).abs() < 2.0 && (rect[3] - full[3]).abs() < 2.0
            },
        )?;
        if h.settled(&client, Duration::ZERO)? != pane_size {
            return Err("Yazi's PTY no longer matches the pane after the split closed".into());
        }
    }
    h.key("j")?;
    h.expect_state(&client, "keys still reach Yazi after the re-layouts", |s| {
        hover(s) == text(&client.cwd.join("copydest"))
    })?;

    h.case = "q closes the surface and returns the shell prompt";
    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&client, &watched)?;
    h.prompt_returned(&client)
}

#[test]
fn real_surface_interactions() {
    let mut harness = Harness::new()
        .unwrap_or_else(|error| panic!("Real interaction prerequisites/setup failed: {error}"));
    let result = surface_flow(&mut harness);
    conclude(&mut harness, result);
}

/// Waits until Yazi has lent its terminal: the pane shows `marker` from the borrowing process.
fn lent(h: &mut Harness, pane: u64, marker: &str) -> TestResult<()> {
    h.wait(&format!("the pane shows {marker:?} from Yazi's lent terminal"), |h| {
        Ok(h.capture(pane)?.contains(marker).then_some(()))
    })
}

/// Yazi parity: recursive search, the lent terminal (`:`, fzf, zoxide, task logs), task manager.
fn parity_flow(h: &mut Harness) -> TestResult<()> {
    let other = h.path("other");
    // Zoxide reads its database from the fixture's XDG_DATA_HOME, as Yazi's zoxide plugin does.
    h.execute("zoxide", &["add".into(), text(&other)])?;
    let client = h.launch(h.path("files"), true)?;
    let files = client.cwd.clone();
    let inside = text(&files.join("child/inside.txt"));

    h.case = "s searches file names recursively via fd; Ctrl+S ends the search";
    h.key("s")?;
    h.expect_ui("[data-role=\"yazi.status-mode\"]", "the search line opens", |v| {
        ui_text(v).contains("SEARCH")
    })?;
    h.paste(client.owner, "inside")?;
    h.expect_ui("", "the search line shows the query", |v| {
        let text = ui_text(v);
        text.contains("Search [s]") && text.contains("inside")
    })?;
    h.key("Enter")?;
    h.expect_state(&client, "fd lists the nested match below the searched directory", |s| {
        s["search"] == "inside"
            && s["cwd"] == text(&files)
            && names(s, "files") == ["child/inside.txt"]
    })?;
    h.expect_ui(
        "[data-role=\"yazi.path-search\"]",
        "the path bar marks the search",
        |v| ui_text(v).contains("search: inside"),
    )?;
    h.expect_state(&client, "the hovered result exports its real path", |s| {
        hover(s) == inside
    })?;
    h.cli(&["send", &client.owner.to_string(), "keys", "ctrl+s"])?;
    h.expect_state(&client, "Ctrl+S returns to the searched directory", |s| {
        s["search"].is_null() && s["cwd"] == text(&files) && names(s, "files").contains(&"beta.txt".into())
    })?;

    h.case = "S searches file contents recursively via ripgrep";
    h.key("S")?;
    h.paste(client.owner, "CHILD_CONTENT")?;
    h.expect_ui("", "the content search line shows the query", |v| {
        let text = ui_text(v);
        text.contains("Search [S]") && text.contains("CHILD_CONTENT")
    })?;
    h.key("Enter")?;
    h.expect_state(&client, "ripgrep lists the file whose content matches", |s| {
        s["search"] == "CHILD_CONTENT" && names(s, "files") == ["child/inside.txt"]
    })?;
    h.cli(&["send", &client.owner.to_string(), "keys", "ctrl+s"])?;
    h.expect_state(&client, "Ctrl+S ends the content search", |s| {
        s["search"].is_null() && s["cwd"] == text(&files)
    })?;

    h.case = "w opens Yazi's task manager; Enter shows the task log; x cancels the task";
    let started = Instant::now();
    h.shell(&client, "printf 'INSPECT_LOG\\n'; sleep 5", false)?;
    h.key("w")?;
    h.expect_ui(
        "[data-role=\"yazi.task-manager\"]",
        "the task manager lists the background task",
        |v| ui_text(v).contains("sleep 5"),
    )?;
    h.expect_state(&client, "Yazi's task snapshots reach the surface", |s| {
        s["task_manager"]["items"]
            .as_array()
            .is_some_and(|items| items.len() == 1 && items[0]["running"] == true)
    })?;
    h.key("Enter")?;
    lent(h, client.owner, "INSPECT_LOG")?;
    h.key("q")?;
    h.expect_ui(
        "[data-role=\"yazi.task-manager\"]",
        "q leaves the log and the task manager returns",
        |v| ui_text(v).contains("sleep 5"),
    )?;
    h.key("x")?;
    h.expect_state(&client, "x cancels the task", |s| {
        // Lua encodes the empty row list as `{}`.
        s["task_manager"]["items"].as_array().is_none_or(Vec::is_empty)
            && s["tasks"]["total"] == 0
    })?;
    if started.elapsed() >= Duration::from_secs(5) {
        return Err("the task may have ended by itself before x; cancellation unproven".into());
    }
    h.key("Escape")?;
    h.expect_ui(
        "[data-role=\"yazi.task-manager\"]",
        "Escape closes the task manager",
        |v| nodes(v).is_empty(),
    )?;

    h.case = "z picks a file with fzf on the lent terminal and reveals it";
    h.key("z")?;
    lent(h, client.owner, "child/inside.txt")?;
    h.paste(client.owner, "inside")?;
    h.key("Enter")?;
    h.expect_state(&client, "fzf's pick is revealed in Yazi", |s| {
        s["cwd"] == text(&files.join("child")) && hover(s) == inside
    })?;
    h.expect_ui(SURFACE_BROWSER, "the surface returns after fzf", |v| {
        !nodes(v).is_empty()
    })?;

    h.case = "Z jumps to a zoxide directory on the lent terminal";
    h.key("Z")?;
    lent(h, client.owner, &text(&other))?;
    h.key("Enter")?;
    h.expect_state(&client, "Yazi enters the zoxide pick", |s| {
        s["cwd"] == text(&other)
    })?;

    h.case = ": lends the pane to the blocking command; input reaches it and the surface returns";
    let marker = h.path("lent-input.txt");
    let script = format!(
        "printf 'LENT_PROMPT> '; read -r line; printf '%%s\\n' \"$line\" > {}",
        quote(text(&marker))
    );
    h.shell(&client, &script, true)?;
    lent(h, client.owner, "LENT_PROMPT>")?;
    h.paste(client.owner, "typed through")?;
    h.key("Enter")?;
    h.wait("the command reads the pane's input", |_| {
        Ok(fs::read_to_string(&marker)
            .ok()
            .filter(|s| s == "typed through\n")
            .map(|_| ()))
    })?;
    h.expect_ui(SURFACE_BROWSER, "the surface returns when Yazi does", |v| {
        !nodes(v).is_empty()
    })?;
    let root = text(&h.root);
    h.key("h")?;
    h.expect_state(&client, "keys reach Yazi again after the hand-back", |s| {
        s["cwd"] == root
    })?;

    h.case = "Ctrl+Z stops the browser and Yazi and returns the shell; fg restores a working browser";
    let state_of = |p: Process| process(p.pid).map(|(_, _, state)| state);
    h.cli(&["send", &client.owner.to_string(), "keys", "ctrl+z"])?;
    h.wait("the launcher and Yazi are stopped by Ctrl+Z", |_| {
        let (launcher, yazi) = (state_of(client.supervisor), state_of(client.backend));
        if launcher == Some('T') && yazi == Some('T') {
            Ok(Some(()))
        } else {
            Err(format!("launcher state {launcher:?}, Yazi state {yazi:?}"))
        }
    })?;
    h.shell_prompt(client.owner)?;
    h.wait("the surface is gone and the shell reports the stopped job", |h| {
        if !nodes(&h.tree(SURFACE_BROWSER)?).is_empty() {
            return Err("program surface still drawn".into());
        }
        let screen = h.capture(client.owner)?;
        // The job notice is localized; its job-table prefix is not.
        Ok(screen.contains("[1]+").then_some(()))
    })?;
    let alive = h.path("suspend-alive.txt");
    h.cli(&[
        "run",
        &client.owner.to_string(),
        &format!("printf 'shell usable\\n' > {}", quote(text(&alive))),
    ])?;
    h.wait("the shell runs commands while the browser is stopped", |_| {
        Ok(fs::read_to_string(&alive)
            .ok()
            .filter(|s| s == "shell usable\n")
            .map(|_| ()))
    })?;
    h.cli(&["run", &client.owner.to_string(), "fg"])?;
    h.wait("fg continues the launcher and Yazi", |_| {
        let (launcher, yazi) = (state_of(client.supervisor), state_of(client.backend));
        if launcher.is_some_and(|s| s != 'T') && yazi.is_some_and(|s| s != 'T') {
            Ok(Some(()))
        } else {
            Err(format!("launcher state {launcher:?}, Yazi state {yazi:?}"))
        }
    })?;
    h.expect_ui(SURFACE_BROWSER, "the surface returns after fg", |v| {
        !nodes(v).is_empty()
    })?;
    let before = h.snapshot(&client);
    let last = names(&before, "files").last().cloned().unwrap_or_default();
    let step = if hover(&before).ends_with(&last) { "k" } else { "j" };
    let from = hover(&before);
    h.key(step)?;
    h.expect_state(&client, "keys reach the continued Yazi after fg", |s| {
        hover(s) != from
    })?;
    // An external stop request suspends the same way instead of stopping the process under the
    // surface.
    client.supervisor.signal(libc::SIGTSTP);
    h.wait("an external SIGTSTP stops the launcher and Yazi", |_| {
        let (launcher, yazi) = (state_of(client.supervisor), state_of(client.backend));
        if launcher == Some('T') && yazi == Some('T') {
            Ok(Some(()))
        } else {
            Err(format!("launcher state {launcher:?}, Yazi state {yazi:?}"))
        }
    })?;
    h.shell_prompt(client.owner)?;
    h.cli(&["run", &client.owner.to_string(), "fg"])?;
    h.expect_ui(SURFACE_BROWSER, "the surface returns after the second fg", |v| {
        !nodes(v).is_empty()
    })?;

    h.case = "q quits; the blocking command's output stays in the pane, as with Yazi";
    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&client, &watched)?;
    h.prompt_returned(&client)?;
    let output = h.capture(client.owner)?;
    if !output.contains("LENT_PROMPT> typed through") {
        return Err(format!("the lent command's output is gone: {output}"));
    }
    Ok(())
}

#[test]
fn real_parity_interactions() {
    let mut harness = Harness::new()
        .unwrap_or_else(|error| panic!("Real interaction prerequisites/setup failed: {error}"));
    let result = parity_flow(&mut harness);
    conclude(&mut harness, result);
}

/// Waits for Yazi's exit to reach the shell and returns the status it printed.
fn exited(
    h: &mut Harness,
    client: &Client,
    watched: &BTreeSet<Process>,
    status: &Path,
) -> TestResult<String> {
    h.removed(client, watched)?;
    h.wait("the shell receives Yazi's exit status", |_| {
        Ok(fs::read_to_string(status)
            .ok()
            .filter(|s| s.ends_with('\n'))
            .map(|s| s.trim().to_owned()))
    })
}

/// Quit and chooser lifecycle: `q`/`Q` go through Yazi's own quit, which writes `--cwd-file`
/// and asks for confirmation while tasks run; `--chooser-file` turns Enter into a choice.
fn quit_flow(h: &mut Harness) -> TestResult<()> {
    let files = h.path("files");
    let yazi = h.path("yazi");
    let child = text(&files.join("child"));

    h.case = "q quits through Yazi, which writes --cwd-file and exits 0";
    let cwd_file = h.path("quit-cwd.txt");
    let status = h.path("quit-status.txt");
    let client = h.launch_with(
        files.clone(),
        true,
        yazi.clone(),
        &format!("--cwd-file {}", quote(text(&cwd_file))),
        Some(&status),
    )?;
    h.key("Enter")?;
    h.expect_state(&client, "Enter enters the first directory", |s| {
        s["cwd"] == child
    })?;
    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    let code = exited(h, &client, &watched, &status)?;
    if code != "0" {
        return Err(format!("Yazi exited {code} after q"));
    }
    let written = fs::read_to_string(&cwd_file).map_err(|e| format!("no cwd-file: {e}"))?;
    if written.trim() != child {
        return Err(format!("cwd-file holds {written:?}, expected {child}"));
    }

    h.case = "Q quits without writing --cwd-file";
    let cwd_file = h.path("quit-nocwd.txt");
    let status = h.path("quit-nocwd-status.txt");
    let client = h.launch_with(
        files.clone(),
        true,
        yazi.clone(),
        &format!("--cwd-file {}", quote(text(&cwd_file))),
        Some(&status),
    )?;
    h.key("Enter")?;
    h.expect_state(&client, "Enter enters the first directory", |s| {
        s["cwd"] == child
    })?;
    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("Q")?;
    let code = exited(h, &client, &watched, &status)?;
    if code != "0" || cwd_file.exists() {
        return Err(format!(
            "Q exited {code}; cwd-file exists: {}",
            cwd_file.exists()
        ));
    }

    h.case = "q with a running task asks first; Escape cancels and Enter quits";
    let status = h.path("quit-tasks-status.txt");
    let client = h.launch_with(files.clone(), true, yazi.clone(), "", Some(&status))?;
    // Long enough to outlive a slow run: the task must still be running at the second `q`.
    h.shell(&client, "sleep 90", false)?;
    h.expect_state(&client, "Yazi reports the running task", |s| {
        s["tasks"]["total"] == 1
    })?;
    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    const CONFIRM: &str = "[data-role=\"yazi.quit-confirm\"]";
    h.key("q")?;
    h.expect_ui(CONFIRM, "q shows the quit confirmation", |v| {
        let text = ui_text(v);
        text.contains("Quit?") && text.contains("unfinished")
    })?;
    h.shot("quit-confirmation")?;
    h.key("Escape")?;
    h.expect_ui(CONFIRM, "Escape closes the confirmation", |v| {
        nodes(v).is_empty()
    })?;
    h.key("j")?;
    let second = text(&files.join("copydest"));
    h.expect_state(&client, "the browser still moves after cancelling", |s| {
        hover(s) == second
    })?;
    h.key("q")?;
    h.expect_ui(CONFIRM, "q asks again", |v| !nodes(v).is_empty())?;
    h.key("Enter")?;
    let code = exited(h, &client, &watched, &status)?;
    if code != "0" {
        return Err(format!("Confirmed quit exited {code}"));
    }

    h.case = "Enter on a file writes it to --chooser-file and exits; Enter on a directory enters";
    let chosen = h.path("chosen.txt");
    let status = h.path("chosen-status.txt");
    let client = h.launch_with(
        files.clone(),
        true,
        yazi.clone(),
        &format!("--chooser-file {}", quote(text(&chosen))),
        Some(&status),
    )?;
    h.key("Enter")?;
    h.expect_state(&client, "Enter still enters a directory", |s| {
        s["cwd"] == child
    })?;
    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("Enter")?;
    let code = exited(h, &client, &watched, &status)?;
    let picked = fs::read_to_string(&chosen).map_err(|e| format!("no chooser file: {e}"))?;
    if code != "0" || picked.trim() != format!("{child}/inside.txt") {
        return Err(format!("Chooser exited {code} with {picked:?}"));
    }

    h.case = "a selection is chosen even while a directory is hovered";
    let chosen = h.path("chosen-selection.txt");
    let status = h.path("chosen-selection-status.txt");
    let client = h.launch_with(
        files.clone(),
        true,
        yazi.clone(),
        &format!("--chooser-file {}", quote(text(&chosen))),
        Some(&status),
    )?;
    h.key("Space")?;
    h.expect_state(&client, "Space selects the hovered directory", |s| {
        selected(s) == BTreeSet::from([child.clone()])
    })?;
    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("Enter")?;
    let code = exited(h, &client, &watched, &status)?;
    let picked = fs::read_to_string(&chosen).map_err(|e| format!("no chooser file: {e}"))?;
    if code != "0" || picked.trim() != child {
        return Err(format!("Chooser exited {code} with {picked:?}"));
    }
    Ok(())
}

#[test]
fn real_quit_interactions() {
    let mut harness = Harness::new()
        .unwrap_or_else(|error| panic!("Real interaction prerequisites/setup failed: {error}"));
    let result = quit_flow(&mut harness);
    conclude(&mut harness, result);
}

/// Yazi notifications reach the surface as notices.
fn notice_flow(h: &mut Harness) -> TestResult<()> {
    h.case = "a Yazi notification is shown as a surface notice";
    let plugin = h.path("yazi/plugins/notify.yazi");
    fs::create_dir_all(&plugin).map_err(|e| e.to_string())?;
    h.write_fixture(
        "yazi/plugins/notify.yazi/main.lua",
        "return { entry = function()\n  ya.notify { title = \"Fixture notice\", content = \"hello from the plugin\", timeout = 8, level = \"warn\" }\nend }\n",
    )?;
    let client = h.launch(h.path("files"), true)?;
    h.shell(&client, "ya emit plugin notify", false)?;
    h.expect_ui(
        "[data-role=\"yazi.notice\"]",
        "the notification text is drawn in the surface",
        |v| ui_text(v).contains("Fixture notice: hello from the plugin"),
    )?;
    h.shot("yazi-notice")?;
    // The notice is timed: it expires on its own.
    h.expect_ui("[data-role=\"yazi.notice\"]", "the notice expires", |v| {
        !ui_text(v).contains("Fixture notice")
    })?;
    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&client, &watched)
}

#[test]
fn real_notice_interactions() {
    let mut harness = Harness::new()
        .unwrap_or_else(|error| panic!("Real interaction prerequisites/setup failed: {error}"));
    let result = notice_flow(&mut harness);
    conclude(&mut harness, result);
}

const WHICH: &str = "[data-role=\"yazi.which\"]";
const TAB_BAR: &str = "[data-role=\"yazi.tab-bar\"]";

fn position(state: &Value, name: &str) -> Option<usize> {
    names(state, "files").iter().position(|file| file == name)
}

/// The sort, linemode, goto, tab, delete, opener, spot, help, hint, overwrite and bulk rename keys.
fn keys_flow(h: &mut Harness) -> TestResult<()> {
    let marker = h.path("opened-with.txt");
    let config = fs::read_to_string(h.path("yazi/yazi.toml")).map_err(|e| e.to_string())?;
    h.write_fixture(
        "yazi/yazi.toml",
        format!(
            "{config}\n[opener]\nprobe = [{{ run = \"echo %s > {}\", desc = \"Probe\" }}]\n\n[open]\nprepend_rules = [{{ url = \"*.md\", use = [\"probe\", \"edit\"] }}]\n",
            text(&marker)
        ),
    )?;
    for (name, data) in [
        ("over.txt", "OLD\n"),
        ("ren-src.txt", "SRC\n"),
        ("over2.txt", "OLD2\n"),
        ("perm.txt", "PERM\n"),
        ("bulk-a.txt", "A\n"),
        ("bulk-b.txt", "B\n"),
    ] {
        h.write_fixture(&format!("files/{name}"), data)?;
    }
    let client = h.launch(h.path("files"), true)?;
    let files = client.cwd.clone();
    let owner = client.owner;

    h.case = "a prefix key shows the keys that can follow it; Escape clears the hint";
    h.key("g")?;
    h.expect_ui(WHICH, "g lists its second keys", |v| {
        let text = ui_text(v);
        text.contains("Go to the home directory") && text.contains("Jump to a typed path")
    })?;
    h.key("Escape")?;
    h.expect_ui(WHICH, "Escape clears the hint", |v| nodes(v).is_empty())?;

    h.case = ", sorts (lowercase ascending, uppercase reversed) and m sets the linemode";
    h.key(",")?;
    h.expect_ui(WHICH, "the sort hint lists the sort keys", |v| {
        ui_text(v).contains("Sort by modified time")
    })?;
    h.key("S")?;
    h.expect_state(&client, ",S puts the largest file before the small ones", |s| {
        matches!(
            (position(s, "oversized.txt"), position(s, "Alpha.txt")),
            (Some(big), Some(small)) if big < small
        )
    })?;
    h.expect_ui(ROWS, ",S also switches the linemode to the size column", |v| {
        nodes(v).iter().any(|node| {
            let text = ui_text(node);
            text.contains("oversized.txt1M")
        })
    })?;
    h.key(",")?;
    h.key("s")?;
    h.expect_state(&client, ",s puts the largest file last", |s| {
        matches!(
            (position(s, "oversized.txt"), position(s, "ren-src.txt")),
            (Some(big), Some(small)) if big > small
        )
    })?;
    for (keys, shown) in [(["m", "n"], false), (["m", "s"], true)] {
        for key in keys {
            h.key(key)?;
        }
        h.expect_ui(ROWS, "the linemode column follows m", |v| {
            nodes(v).iter().any(|node| {
                let text = ui_text(node);
                text.contains("oversized.txt") && (text.contains("oversized.txt1M") == shown)
            })
        })?;
    }
    h.key(",")?;
    h.key("a")?;
    h.expect_state(&client, ",a restores the alphabetical order", |s| {
        matches!(
            (position(s, "oversized.txt"), position(s, "ren-src.txt")),
            (Some(big), Some(small)) if big < small
        )
    })?;
    h.key("m")?;
    h.key("n")?;

    h.case = "gh, gc and gd go to the home directory, ~/.config and ~/Downloads; g Space types a path";
    for (second, dir) in [("h", "home"), ("c", "home/.config"), ("d", "home/Downloads")] {
        h.key("g")?;
        h.key(second)?;
        let expected = text(&h.path(dir));
        h.expect_state(&client, &format!("g{second} goes to {expected}"), |s| {
            s["cwd"] == expected
        })?;
    }
    h.key("g")?;
    h.key("Space")?;
    let downloads = text(&h.path("home/Downloads"));
    h.expect_ui(
        "[data-role=\"yazi.path-input\"]",
        "g Space opens the path editor on the current directory",
        |v| ui_text(v).contains(&downloads),
    )?;
    h.clear_line(owner)?;
    h.paste(owner, &text(&files))?;
    h.key("Enter")?;
    h.expect_state(&client, "the typed path is entered", |s| s["cwd"] == text(&files))?;

    h.case = "t r renames the tab and the tab bar shows the name";
    h.key("t")?;
    h.key("t")?;
    h.expect_ui(TAB_BAR, "tt opens a second tab", |v| !nodes(v).is_empty())?;
    h.key("t")?;
    h.key("r")?;
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.tab-rename\"]",
        "t r opens the tab name line",
        |v| nodes(v).len() == 1,
    )?;
    h.clear_line(owner)?;
    h.paste(owner, "work")?;
    h.key("Enter")?;
    h.expect_ui(TAB_BAR, "the tab bar shows the new name", |v| {
        ui_text(v).contains("work")
    })?;
    h.cli(&["send", &owner.to_string(), "keys", "ctrl+c"])?;
    h.expect_ui(TAB_BAR, "the second tab closes", |v| nodes(v).is_empty())?;

    h.case = "~ and F1 show the help generated from the key tables; Escape closes it";
    for opener in ["~", "F1"] {
        h.key(opener)?;
        h.expect_ui("[data-role=\"yazi.help\"]", "the help layer opens", |v| {
            ui_text(v).contains("Move the cursor")
        })?;
        h.key("End")?;
        // The card text is truncated; the visible rows are the list's options.
        h.expect_ui(
            "[data-role=\"yazi.help\"] [role=\"option\"]",
            "End scrolls to the last key",
            |v| nodes(v).iter().any(|row| ui_text(row).contains("Sort randomly")),
        )?;
        h.key("Escape")?;
        h.expect_ui("[data-role=\"yazi.help\"]", "Escape closes the help", |v| {
            nodes(v).is_empty()
        })?;
    }

    h.case = "O lists the openers of the hovered file and runs the picked one";
    h.click_file(&client, "notes.md")?;
    h.key("O")?;
    h.expect_ui("[data-role=\"yazi.open-picker\"]", "O lists the openers", |v| {
        let text = ui_text(v);
        text.contains("Open with:") && text.contains("Probe")
    })?;
    h.key("Enter")?;
    h.wait("the picked opener ran on the hovered file", |_| {
        Ok(fs::read_to_string(&marker)
            .ok()
            .filter(|out| out.trim() == text(&files.join("notes.md")))
            .map(|_| ()))
    })?;
    h.expect_ui("[data-role=\"yazi.open-picker\"]", "the picker closes", |v| {
        nodes(v).is_empty()
    })?;

    h.case = "Tab spots the hovered file; cc copies the selected cell";
    let mime = Command::new("file")
        .args(["-bL", "--mime-type"])
        .arg(files.join("notes.md"))
        .output()
        .map_err(|e| format!("file is required: {e}"))?;
    let mime = String::from_utf8_lossy(&mime.stdout).trim().replace("/x-", "/");
    let saved = Command::new("wl-paste")
        .arg("--no-newline")
        .output()
        .map_err(|e| format!("wl-paste is required: {e}"))?;
    h.key("Tab")?;
    h.expect_ui("[data-role=\"yazi.spot\"]", "Tab opens the spot table", |v| {
        let text = ui_text(v);
        text.contains("Mimetype") && text.contains("Permissions") && text.contains(&mime)
    })?;
    let copied = (|| -> TestResult<()> {
        // The cursor starts on Created; Mimetype is two rows below.
        h.key("j")?;
        h.key("j")?;
        h.key("c")?;
        h.expect_ui(WHICH, "c shows the cell copy hint", |v| {
            ui_text(v).contains("Copy the selected cell")
        })?;
        h.key("c")?;
        h.wait(&format!("the clipboard holds {mime}"), |_| {
            let clip = Command::new("wl-paste")
                .arg("--no-newline")
                .output()
                .map_err(|e| e.to_string())?;
            Ok((clip.stdout == mime.as_bytes()).then_some(()))
        })
    })();
    let mut restore = Command::new("wl-copy");
    if saved.status.success() {
        restore.stdin(Stdio::piped());
    } else {
        restore.arg("--clear").stdin(Stdio::null());
    }
    let mut restore = restore
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut stdin) = restore.stdin.take() {
        stdin.write_all(&saved.stdout).map_err(|e| e.to_string())?;
    }
    restore.wait().map_err(|e| e.to_string())?;
    copied?;
    h.key("Tab")?;
    h.expect_ui("[data-role=\"yazi.spot\"]", "Tab closes the spot table", |v| {
        nodes(v).is_empty()
    })?;

    h.case = "D confirms before it deletes for good; Escape deletes nothing";
    h.click_file(&client, "perm.txt")?;
    h.key("D")?;
    h.expect_ui(
        "[data-role=\"yazi.delete-confirm\"]",
        "D asks for confirmation naming the exact path",
        |v| {
            let text = ui_text(v);
            text.contains("Permanently delete") && text.contains("perm.txt")
        },
    )?;
    h.key("Escape")?;
    h.expect_ui(
        "[data-role=\"yazi.delete-confirm\"]",
        "Escape closes the confirmation",
        |v| nodes(v).is_empty(),
    )?;
    if !files.join("perm.txt").exists() {
        return Err("Cancelling the permanent delete removed the file".into());
    }
    h.key("D")?;
    h.expect_ui(
        "[data-role=\"yazi.delete-confirm\"]",
        "D asks again",
        |v| !nodes(v).is_empty(),
    )?;
    h.key("Enter")?;
    h.expect_state(&client, "the file is gone from the listing", |s| {
        position(s, "perm.txt").is_none()
    })?;
    let trashed = h.path("data/Trash/files/perm.txt");
    if files.join("perm.txt").exists() || trashed.exists() {
        return Err("Permanent delete left the file or moved it to the trash".into());
    }

    h.case = "a create onto an existing file or directory asks before overwriting";
    h.key("a")?;
    h.paste(owner, "over.txt")?;
    h.key("Enter")?;
    h.expect_ui(
        "[data-role=\"yazi.overwrite-confirm\"]",
        "an existing file asks to overwrite",
        |v| {
            let text = ui_text(v);
            text.contains("Overwrite file?")
                && text.contains("Will overwrite the following file:")
                && text.contains(&text_of(&files.join("over.txt")))
        },
    )?;
    h.key("Escape")?;
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.create\"]",
        "cancelling returns to the create line with the typed name",
        |v| ui_text(v) == "over.txt",
    )?;
    h.key("Enter")?;
    h.expect_ui(
        "[data-role=\"yazi.overwrite-confirm\"]",
        "the same name asks again",
        |v| !nodes(v).is_empty(),
    )?;
    h.key("Enter")?;
    h.wait("confirming truncates the file", |_| {
        Ok((fs::metadata(files.join("over.txt")).is_ok_and(|m| m.len() == 0)).then_some(()))
    })?;
    h.key("a")?;
    h.paste(owner, "child/")?;
    h.key("Enter")?;
    h.expect_ui(
        "[data-role=\"yazi.overwrite-confirm\"]",
        "an existing directory asks too",
        |v| !nodes(v).is_empty(),
    )?;
    h.key("Enter")?;
    h.expect_ui(
        "[data-role=\"yazi.overwrite-confirm\"]",
        "confirming closes the layer",
        |v| nodes(v).is_empty(),
    )?;
    // The confirmed create reveals the directory when Yazi finishes; wait for that, or the
    // reveal would move the hover under the next case's click.
    h.expect_state(&client, "the existing directory is revealed", |s| {
        hover(s) == text(&files.join("child"))
    })?;
    if !files.join("child/inside.txt").is_file() {
        return Err("Overwriting a directory with a directory lost its contents".into());
    }

    h.case = "a rename onto an existing file asks before overwriting";
    h.click_file(&client, "ren-src.txt")?;
    h.key("r")?;
    // The line renames the hovered file the surface last saw; make sure that is the one.
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.rename\"]",
        "the rename line starts from the hovered file's name",
        |v| ui_text(v).contains("ren-src.txt"),
    )?;
    h.clear_line(owner)?;
    h.paste(owner, "over2.txt")?;
    h.key("Enter")?;
    h.expect_ui(
        "[data-role=\"yazi.overwrite-confirm\"]",
        "the rename asks to overwrite",
        |v| ui_text(v).contains(&text_of(&files.join("over2.txt"))),
    )?;
    h.key("Enter")?;
    h.wait("the rename replaced the destination", |_| {
        Ok((fs::read_to_string(files.join("over2.txt")).ok().as_deref() == Some("SRC\n")
            && !files.join("ren-src.txt").exists())
        .then_some(()))
    })?;

    h.case = "r on a selection runs Yazi's bulk rename on the lent terminal";
    h.click_file(&client, "bulk-a.txt")?;
    h.key("Space")?;
    h.key("Space")?;
    h.expect_state(&client, "two files are selected", |s| selected(s).len() == 2)?;
    h.key("r")?;
    lent(h, owner, "Continue to rename")?;
    // Yazi has left raw mode for its prompt, so the answer needs Enter.
    h.key("y")?;
    h.key("Enter")?;
    h.wait("the editor's names were applied", |_| {
        Ok((files.join("x-bulk-a.txt").exists()
            && files.join("x-bulk-b.txt").exists()
            && !files.join("bulk-a.txt").exists())
        .then_some(()))
    })?;
    h.expect_ui(SURFACE_BROWSER, "the surface returns after the bulk rename", |v| {
        !nodes(v).is_empty()
    })?;
    Ok(())
}

fn text_of(path: &Path) -> String {
    text(path)
}

#[test]
fn real_keys_interactions() {
    let mut harness = Harness::new()
        .unwrap_or_else(|error| panic!("Real interaction prerequisites/setup failed: {error}"));
    let result = keys_flow(&mut harness);
    conclude(&mut harness, result);
}
