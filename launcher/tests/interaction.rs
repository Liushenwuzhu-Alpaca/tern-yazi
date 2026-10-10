//! Real GUI interaction tests. No state or backend response is synthesized.
//! Run through tests/smoke_luau.sh on Wayland/X11; prerequisites fail rather than skip.
//! Tern 0.7 serve is standalone, so these tests explicitly use a daemon-backed window.

use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::env;
use std::fs::{self, DirBuilder, File};
use std::io::Read;
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

fn row_selector(scope: &str, name: &str) -> String {
    let list = match scope {
        ROWS => "main.browser.columns.current.files_list",
        PARENT_ROWS => "main.browser.columns.parent.parent_list",
        CHILD_ROWS => "main.browser.columns.preview.child_list",
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

fn replace_layout_pane(value: &Value, from: u64, to: u64) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| replace_layout_pane(v, from, to))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    let value = if key == "pane" && value.as_u64() == Some(from) {
                        json!(to)
                    } else {
                        replace_layout_pane(value, from, to)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        _ => value.clone(),
    }
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
    token: String,
    owner: u64,
    pane: u64,
    supervisor: Process,
    backend: Process,
    cwd: PathBuf,
    nonce: String,
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
    fn state_path(&self, client: &Client) -> PathBuf {
        self.inbox().join(format!("state-{}.json", client.cid))
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
            || state["listing_revision"] != listing["revision"] {
            return Value::Null;
        }
        let (Some(dynamic), Some(immutable)) = (state.as_object_mut(), listing.as_object_mut()) else {
            return Value::Null;
        };
        for field in ["cwd", "files", "file_dirs", "file_icons", "file_count", "parent", "preview", "selected_urls", "marked_urls"] {
            if let Some(value) = immutable.remove(field) { dynamic.insert(field.into(), value); }
        }
        let apply_icons = |scope: &mut Value, overrides: Value| {
            let Value::Object(overrides) = overrides else { return };
            let Some(scope) = scope.as_object_mut() else { return };
            if let Some(icons) = scope.entry("file_icons").or_insert_with(|| json!({})).as_object_mut() {
                icons.extend(overrides);
            }
        };
        for (scope, field) in [("", "icon_overrides"), ("parent", "parent_icon_overrides"), ("preview", "preview_icon_overrides")] {
            let overrides = state.as_object_mut().and_then(|state| state.remove(field)).unwrap_or(Value::Null);
            if scope.is_empty() { apply_icons(&mut state, overrides); }
            else if let Some(value) = state.get_mut(scope) { apply_icons(value, overrides); }
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

    fn fixture(&self, request: Value) -> TestResult<Value> {
        fs::write(
            self.path("fixture/request.json"),
            serde_json::to_vec(&request).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        self.ctl("plugins", &["run", "plugin.slot-fixture.inspect"])?;
        read_json(&self.path("fixture/observed.json"))
            .ok_or("Real window fixture returned no observation".into())
    }

    fn layout_state(&self) -> TestResult<Value> {
        self.fixture(json!({"op": "inspect"}))
    }

    fn startup_panes(&self, original: &BTreeSet<u64>, native: Option<u64>) -> TestResult<()> {
        // Read app state only: never send an event or command to wake native startup.
        let state = self.ctl("state", &[])?;
        let panes = state["panes"].as_array().ok_or("Startup observation has no real panes")?;
        let mut added = 0;
        for pane in panes {
            let id = pane["id"].as_u64().ok_or("Startup pane has no actual ID")?;
            if original.contains(&id) {
                continue;
            }
            added += 1;
            if added > 1 || native.is_some_and(|native| native != id) {
                return Err(format!("Native startup created an extra Task/terminal pane: {state}"));
            }
        }
        Ok(())
    }

    fn shell_prompt(&mut self, owner: u64) -> TestResult<()> {
        self.wait("original real shell reaches its foreground prompt", |h| {
            let state = h.layout_state()?;
            Ok(pane_info(&state, owner)
                .filter(|info| info["at_prompt"] == true && info["parked"] != true)
                .map(|_| ()))
        })
    }

    fn restored(&mut self, client: &Client, exact: bool, focus: Option<u64>) -> TestResult<()> {
        self.wait("same owner ID and original Bash survive native closure", |h| {
            let state = h.layout_state()?;
            let owner = pane_info(&state, client.owner).ok_or("Original shell pane vanished")?;
            if owner["parked"] == true || owner["tab"].as_u64().is_none() || owner["at_prompt"] != true {
                return Ok(None);
            }
            if !client.owner_shell.is_some_and(Process::alive) {
                return Err("Closing native block destroyed or replaced its original Bash process".into());
            }
            if pane_info(&state, client.pane).is_some() || focus.is_some_and(|pane| state["focused"].as_u64() != Some(pane)) {
                return Ok(None);
            }
            if exact {
                let tab = client.original_tab.ok_or("Managed owner had no original tab")?;
                let layout = tab_layout(&state, tab).ok_or("Original tab disappeared on preclose")?;
                if layout["root"] != client.original_layout["root"] || layout["floats"] != client.original_layout["floats"]
                    || layout["zoomed"] != client.original_layout["zoomed"] || owner["tab"].as_u64() != Some(tab) {
                    return Err(format!("Preclose failed to restore exact original slot/ratios/tab: {layout}; original {}", client.original_layout));
                }
            }
            Ok(Some(()))
        })?;
        let marker = self.path(&format!("shell-return-{}.txt", client.cid));
        self.cli(&[
            "run",
            &client.owner.to_string(),
            &format!("printf '%s\\n' \"$$\" > {}", quote(text(&marker))),
        ])?;
        self.wait(
            "restored original Bash executes another real command",
            |_| {
                Ok(fs::read_to_string(&marker)
                    .ok()
                    .and_then(|s| s.trim().parse::<i32>().ok())
                    .filter(|pid| client.owner_shell.is_some_and(|shell| shell.pid == *pid))
                    .map(|_| ()))
            },
        )
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
        // Compare the actual native row with the authoritative value just exported.
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
            &format!("native row exactly matches {label:?}"),
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
            "native selected row retains its full authoritative path",
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
        self.wait("footer mode/size/name and permission/progress/position have real ordered bounds", |h| {
            let footer = metadata_rect(&h.tree("[data-role=\"yazi.status\"]")?)?;
            let mode = metadata_rect(&h.tree("[data-role=\"yazi.status\"] > .sf-badge")?)?;
            let size = metadata_rect(&h.tree("[data-role=\"yazi.status-size\"]")?)?;
            let name = metadata_rect(&h.tree("[data-role=\"yazi.status-name\"]")?)?;
            let right = metadata_rect(&h.tree("[data-role=\"yazi.status-right\"]")?)?;
            let permissions = metadata_rect(&h.tree("[data-role=\"yazi.status-permissions\"]")?)?;
            let percent = metadata_rect(&h.tree("[data-role=\"yazi.status-right\"] > .sf-text:not([data-role=\"yazi.status-permissions\"])")?)?;
            let position = metadata_rect(&h.tree("[data-role=\"yazi.status-right\"] > .sf-badge")?)?;
            if [mode, size, name, right].into_iter().all(|r| metadata_contains(footer, r))
                && [permissions, percent, position].into_iter().all(|r| metadata_contains(right, r))
                && metadata_before(mode, size)
                && metadata_before(size, name)
                && metadata_before(name, right)
                && name[0] + name[2] >= right[0] - 16.0
                && right[0] + right[2] >= footer[0] + footer[2] - 1.0
                && metadata_before(permissions, percent)
                && metadata_before(percent, position)
            {
                Ok(Some(()))
            } else {
                Err(format!("Footer {footer:?}: mode {mode:?}, size {size:?}, name {name:?}, right {right:?}, permissions {permissions:?}, percent {percent:?}, position {position:?}"))
            }
        })?;
        self.expect_metadata_header_layout("yazi.column.parent", ".sf-list-scroll.max")?;
        self.expect_metadata_header_layout("yazi.column.current", ".sf-list-scroll.max")?;
        self.expect_metadata_header_layout(
            "yazi.column.preview",
            "[data-role=\"yazi.preview-metadata\"]",
        )?;
        self.wait("fixed preview metadata and actions bound the independent content scroller", |h| {
            let card = metadata_rect(&h.tree("[data-role=\"yazi.column.preview\"]")?)?;
            let metadata = metadata_rect(&h.tree("[data-role=\"yazi.preview-metadata\"]")?)?;
            let preview = metadata_rect(&h.tree(PREVIEW)?)?;
            let actions = metadata_rect(&h.tree("[data-role=\"yazi.preview-actions\"]")?)?;
            if [metadata, preview, actions].into_iter().all(|r| metadata_contains(card, r))
                && metadata_above(metadata, preview)
                && metadata_above(preview, actions)
            {
                Ok(Some(()))
            } else {
                Err(format!("Preview card {card:?}: metadata {metadata:?}, content {preview:?}, actions {actions:?}"))
            }
        })
    }

    fn expect_native_file_metadata(
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
        self.expect_metadata_text("[data-role=\"yazi.status\"] > .sf-badge", "NOR")?;
        self.expect_metadata_text(
            "[data-role=\"yazi.status-right\"] > .sf-text:not([data-role=\"yazi.status-permissions\"])",
            &format!("{}%", index * 100 / files.len()),
        )?;
        self.expect_metadata_text(
            "[data-role=\"yazi.status-right\"] > .sf-badge",
            &format!("{index}/{}", files.len()),
        )?;
        self.expect_metadata_text(
            "[data-role=\"yazi.preview-metadata\"] > .sf-text:not([data-role])",
            language,
        )?;
        self.expect_ui(
            "[data-role=\"yazi.preview-metadata\"] > *",
            "fixed metadata contains only semantic language, size and permissions",
            |v| nodes(v).len() == 3,
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
            ("PATH", format!("{}:{}", text(&self.path("bin")), env::var("PATH").unwrap_or_default())),
            ("YAZI_CONFIG_HOME", text(&self.path("yazi"))),
            ("TERN_CONFIG_DIR", text(&self.path("config"))),
            ("TERN_DAEMON_SOCK", text(&self.path("daemon.sock"))),
            ("XDG_CONFIG_HOME", text(&self.path("config"))),
            ("XDG_RUNTIME_DIR", text(&self.path("runtime"))),
            ("XDG_DATA_HOME", text(&self.path("data"))),
            ("XDG_CACHE_HOME", text(&self.path("cache"))),
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
            ("files/notes.md", "# Native Markdown\n\n**MARKDOWN_PREVIEW_CONTENT**\n"),
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
        self.write_fixture("fixture/plugin.toml", "schema = 1\nid = \"slot-fixture\"\nname = \"Native Slot Interaction Fixture\"\nversion = \"1\"\nwindow = \"window.luau\"\n")?;
        let fixture_root =
            serde_json::to_string(&text(&self.path("fixture"))).map_err(|e| e.to_string())?;
        self.write_fixture("fixture/window.luau", format!("local ROOT = {fixture_root}\n") + r#"
-- Observe and drive the real window through public APIs; never synthesize pane state.
local function observed_tree(node)
    if not node then return tern.json.null end
    return { pane = node.pane, split = node.split, ratio = node.ratio,
        first = observed_tree(node[1]), second = observed_tree(node[2]) }
end
tern.command({ id = "inspect", title = "Inspect native slot fixture", run = function(cx)
    local request = tern.json.decode(tern.fs.read(ROOT .. "/request.json", 65536))
    local result = nil
    if request.op == "shell-tab" then
        result = cx.layout:new_tab({ cwd = request.cwd }, { focus = true })
        assert(result, "No real shell tab was created")
    elseif request.op == "split" then
        result = cx.layout:split(request.pane, request.dir, { cwd = request.cwd }, { focus = true })
    elseif request.op == "resize" then
        local ok, err = cx.layout:resize(request.pane, request.dir, request.cells)
        assert(ok, err)
    elseif request.op == "float" then
        local ok, err = cx.layout:float(request.pane, request.over, request.corner, { focus = true })
        assert(ok, err)
    elseif request.op == "new" then
        result = cx:new_block("tern-yazi.companion", nil, request.how, { focus = true })
        assert(result, "No real native Yazi block was created")
    elseif request.op == "move-tab" then
        local tab, err = cx.layout:move_to_new_tab(request.pane, nil, { focus = true })
        assert(tab, err)
        result = tab
    elseif request.op ~= "inspect" then
        error("Unknown native slot fixture operation")
    end
    local layouts = {}
    for _, tab in ipairs(cx.session:tabs()) do
        local layout = cx.session:layout(tab.id)
        table.insert(layouts, { tab = layout.tab, root = observed_tree(layout.root),
            floats = layout.floats, zoomed = layout.zoomed, focus = layout.focus })
    end
    tern.fs.write(ROOT .. "/observed.json", tern.json.encode({ panes = cx.session:panes(),
        tabs = cx.session:tabs(), layouts = layouts, focused = cx.session:focused(), result = result }))
end })
"#)?;
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
            self.wait("owned native control endpoint", |h| {
                Ok(h.path("control.sock").exists().then_some(()))
            })?;
            // Initialize the native viewport before launching Yazi; this is not
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

    fn launch(&mut self, cwd: PathBuf, new_tab: bool) -> TestResult<Client> {
        self.launch_with_config(cwd, new_tab, self.path("yazi"))
    }

    fn launch_with_config(
        &mut self,
        cwd: PathBuf,
        new_tab: bool,
        config: PathBuf,
    ) -> TestResult<Client> {
        if new_tab {
            let created = self.cli(&[
                "new",
                "tab",
                "--json",
                "--cwd",
                &text(&cwd),
                "--",
                &text(&self.path("bin/shell")),
            ])?;
            let created: Value = serde_json::from_str(&created).map_err(|e| e.to_string())?;
            self.focus(
                created["block"]
                    .as_u64()
                    .ok_or("New tab returned no shell block")?,
            )?;
            self.wait("new real shell prompt", |h| {
                let state = h.ctl("state", &[])?;
                Ok(
                    (state["focused"]["prompt"] == true && state["focused"]["cwd"] == text(&cwd))
                        .then_some(()),
                )
            })?;
        }
        let owner = self.ctl("state", &[])?["focused"]["id"]
            .as_u64()
            .ok_or("No real shell owner pane")?;
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
            &format!("printf '%s\\n' \"$$\" > {}", quote(text(&shell_marker))),
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
        let line = format!(
            "cd {} && YAZI_CONFIG_HOME={} yazi {}",
            quote(text(&cwd)),
            quote(text(&config)),
            quote(text(&cwd))
        );
        self.paste(owner, &line)?;
        self.key("Enter")?;
        let supervisor = self.wait("actual managed supervisor process", |h| {
            h.startup_panes(&original_panes, None)?;
            let pid = h
                .pid_markers()
                .difference(&before)
                .copied()
                .find(|pid| process(*pid).is_some());
            Ok(pid.and_then(process).map(|(p, _, _)| p))
        })?;
        self.owned.insert(supervisor);
        let client = self.wait(
            "cold native block with real snapshot and live lease (read-only startup observations)",
            |h| {
                h.startup_panes(&original_panes, None)?;
                let entries = fs::read_dir(h.inbox()).map_err(|e| e.to_string())?;
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !supervisor.alive() {
                        return Err(format!(
                            "Managed supervisor exited before attachment; owner output: {}",
                            h.capture(owner)?
                        ));
                    }
                    let Some(cid) = name
                        .strip_prefix("managed-")
                        .and_then(|s| s.strip_suffix(".json"))
                    else {
                        continue;
                    };
                    let Some(health) = read_json(&entry.path()) else {
                        continue;
                    };
                    if health["owner_pane"].as_u64() != Some(owner) {
                        continue;
                    }
                    let token = health["token"]
                        .as_str()
                        .ok_or("Missing health identity")?
                        .to_owned();
                    let lease = fs::read_to_string(h.inbox().join(format!("managed-{cid}.lease")))
                        .map_err(|e| e.to_string())?;
                    let parts: Vec<_> = lease.split_whitespace().collect();
                    if parts.len() < 5 || parts[0] != token {
                        continue;
                    }
                    let pane = parts[1].parse::<u64>().map_err(|e| e.to_string())?;
                    let snapshot = h.snapshot_id(cid);
                    if snapshot["cwd"] != text(&cwd) {
                        continue;
                    }
                    let backend = descendants(supervisor)
                        .into_iter()
                        .find(|p| {
                            fs::read_link(format!("/proc/{}/exe", p.pid))
                                .is_ok_and(|path| path == Path::new("/usr/bin/yazi"))
                        })
                        .ok_or("Real Yazi child not yet observable")?;
                    return Ok(Some(Client {
                        cid: cid.to_owned(),
                        token,
                        owner,
                        pane,
                        supervisor,
                        backend,
                        cwd: cwd.clone(),
                        nonce: parts[4].to_owned(),
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
        self.wait("focused native Companion rather than ANSI Yazi", |h| {
            let state = h.ctl("state", &[])?;
            Ok((state["focused"]["id"].as_u64() == Some(client.pane)
                && state["focused"]["label"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("Companion")))
            .then_some(()))
        })?;
        self.wait("parked original owner and native block occupy exactly the original visual slot", |h| {
            let state = h.layout_state()?;
            let owner = pane_info(&state, client.owner).ok_or("Managed launch destroyed its owner")?;
            let native = pane_info(&state, client.pane).ok_or("Native pane is not observable")?;
            let layout = tab_layout(&state, original_tab).ok_or("Managed launch lost original tab")?;
            let mut expected_panes = client.original_panes.clone();
            expected_panes.insert(client.pane);
            if client.pane == client.owner || owner["parked"] != true || !owner["tab"].is_null()
                || native["parked"] == true || native["tab"].as_u64() != Some(original_tab)
                || pane_ids(&state) != expected_panes || state["tabs"].as_array().map_or(0, Vec::len) != original["tabs"].as_array().map_or(0, Vec::len)
                || layout["root"] != replace_layout_pane(&original_layout["root"], client.owner, client.pane)
                || layout["floats"] != replace_layout_pane(&original_layout["floats"], client.owner, client.pane)
                || native["floating"] != pane_info(&original, client.owner).ok_or("Original owner has no pane metadata")?["floating"]
                || layout["zoomed"] != original_layout["zoomed"] {
                return Err(format!("Managed launch added a visible split, zoomed or changed siblings/ratios: {state}; original {original}"));
            }
            Ok(Some(()))
        })?;
        self.expect_ui(ROWS, "actual native file rows", |value| {
            !nodes(value).is_empty()
        })?;
        Ok(client)
    }

    fn standalone(&mut self, pane: u64, cwd: PathBuf, original_panes: BTreeSet<u64>) -> TestResult<Client> {
        let client = self.wait("no-argument native block starts its own real Yazi without a terminal", |h| {
            h.startup_panes(&original_panes, Some(pane))?;
            let binding = read_json(&h.inbox().join(format!("native-{pane}.json"))).unwrap_or(Value::Null);
            let Some(cid) = binding["client_id"].as_str() else { return Ok(None) };
            let Some(token) = binding["token"].as_str() else { return Ok(None) };
            let health = read_json(&h.inbox().join(format!("managed-{cid}.json"))).unwrap_or(Value::Null);
            let snapshot = h.snapshot_id(cid);
            if health["owner_kind"] != "standalone" || health["owner_pane"].as_u64() != Some(pane)
                || binding["native_pane"].as_u64() != Some(pane) || health["token"] != token || snapshot["cwd"] != text(&cwd) {
                return Ok(None);
            }
            let lease = fs::read_to_string(h.inbox().join(format!("managed-{cid}.lease"))).unwrap_or_default();
            let fields: Vec<_> = lease.split_whitespace().collect();
            if fields.len() != 5 || fields[0] != token || fields[1].parse::<u64>().ok() != Some(pane) {
                return Ok(None);
            }
            for entry in fs::read_dir("/proc").map_err(|e| e.to_string())?.flatten() {
                let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
                if !fs::read_link(entry.path().join("exe")).is_ok_and(|exe| exe == Path::new("/usr/bin/yazi")) {
                    continue;
                }
                let args = fs::read(entry.path().join("cmdline")).unwrap_or_default();
                if !args.split(|byte| *byte == 0).any(|arg| arg == cid.as_bytes()) { continue; }
                let Some((backend, parent, _)) = process(pid) else { continue };
                let Some((supervisor, _, _)) = process(parent) else { continue };
                let command = fs::read(format!("/proc/{}/cmdline", supervisor.pid)).map_err(|e| e.to_string())?;
                let arguments: Vec<_> = command.split(|byte| *byte == 0).filter(|arg| !arg.is_empty()).collect();
                let helper = fs::canonicalize(h.path("libexec/tern-yazi-launch")).map_err(|e| e.to_string())?;
                let launched_helper = arguments.first().and_then(|arg| std::str::from_utf8(arg).ok())
                    .and_then(|path| fs::canonicalize(path).ok());
                let real = fs::read_to_string(h.path("libexec/tern-yazi-launch.real")).map_err(|e| e.to_string())?;
                if launched_helper != Some(helper)
                    || !arguments.windows(2).any(|pair| pair[0] == b"--real" && pair[1] == real.trim_end().as_bytes()) {
                    return Err("Native supervisor did not discover the installed-prefix helper and its original-path metadata".into());
                }
                let environment = fs::read(format!("/proc/{}/environ", supervisor.pid)).map_err(|e| e.to_string())?;
                if environment.split(|byte| *byte == 0).any(|entry|
                    entry.starts_with(b"TERN_YAZI_LAUNCHER_PATH=") || entry.starts_with(b"TERN_YAZI_REAL=")) {
                    return Err("Native installed-prefix startup was masked by a helper or original executable environment override".into());
                }
                return Ok(Some(Client { cid: cid.to_owned(), token: token.to_owned(), owner: pane, pane,
                    supervisor, backend, cwd: cwd.clone(), nonce: fields[4].to_owned(), owner_shell: None,
                    original_tab: None, original_layout: Value::Null, original_panes: BTreeSet::new() }));
            }
            Ok(None)
        })?;
        self.owned.extend(descendants(client.supervisor));
        self.clients.push(client.clone());
        self.expect_ui(
            ROWS,
            "standalone native block has actual usable file rows",
            |value| !nodes(value).is_empty(),
        )?;
        Ok(client)
    }

    fn pinned_pair(&mut self, client: &Client, before: &Value, focused: u64) -> TestResult<()> {
        self.wait("reload/reopen preserves real pane pair, token, nonce, layout and process identities", |h| {
            let state = h.layout_state()?;
            let lease = fs::read_to_string(h.inbox().join(format!("managed-{}.lease", client.cid))).unwrap_or_default();
            let fields: Vec<_> = lease.split_whitespace().collect();
            let health = read_json(&h.inbox().join(format!("managed-{}.json", client.cid))).unwrap_or(Value::Null);
            let parked = client.owner == client.pane || pane_info(&state, client.owner)
                .is_some_and(|owner| owner["parked"] == true && owner["tab"].is_null());
            if pane_ids(&state) == pane_ids(before) && state["layouts"] == before["layouts"]
                && state["focused"].as_u64() == Some(focused) && parked
                && health["token"] == client.token && fields.first().copied() == Some(client.token.as_str())
                && fields.get(1).and_then(|s| s.parse::<u64>().ok()) == Some(client.pane)
                && fields.get(4).copied() == Some(client.nonce.as_str())
                && client.supervisor.alive() && client.backend.alive() { Ok(Some(())) }
            else { Err(format!("Pinned native pair changed after reload/reopen: {state}; original {before}; lease {lease}")) }
        })
    }

    fn reopen_window(&mut self) -> TestResult<()> {
        let mut previous = self
            .renderer
            .take()
            .ok_or("Missing owned renderer to reopen")?;
        previous.kill().map_err(|e| e.to_string())?;
        previous.wait().map_err(|e| e.to_string())?;
        if !self.daemon_process.is_some_and(Process::alive) {
            return Err("Closing the renderer terminated its private session daemon".into());
        }
        let log = File::create(self.path("renderer-reopened.log")).map_err(|e| e.to_string())?;
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
            .args(["--control", &text(&self.path("control.sock"))])
            .stdin(Stdio::piped())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .process_group(0);
        self.renderer = Some(renderer.spawn().map_err(|e| e.to_string())?);
        if let Some(hook) = hook {
            let pid = self
                .renderer
                .as_ref()
                .ok_or("Missing reopened renderer")?
                .id();
            self.wait("reopened owned renderer is stopped before showing", |_| {
                Ok(process(pid as i32)
                    .filter(|(_, _, state)| *state == 'T')
                    .map(|_| ()))
            })?;
            self.execute(
                Path::new(&hook),
                &[pid.to_string(), text(&self.root), "start".into()],
            )?;
            if unsafe { libc::kill(pid as i32, libc::SIGCONT) } != 0 {
                return Err(format!(
                    "Cannot release reopened renderer: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        self.wait(
            "reopened real daemon-backed window has a control endpoint",
            |h| Ok(h.ctl("state", &[]).ok().map(|_| ())),
        )
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
                    theme_visible(node) && (node["text"]
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
        self.expect_state(client, &format!("native hover {path}"), |s| {
            hover(s) == path
        })?;
        Ok(())
    }

    fn hover_stays(&self, client: &Client, expected: &str) -> TestResult<()> {
        let deadline = Instant::now() + Duration::from_millis(400);
        while Instant::now() < deadline {
            let actual = hover(&self.snapshot(client));
            if actual != expected {
                return Err(format!(
                    "Leader changed native hover: expected {expected}, actual {actual}"
                ));
            }
            thread::sleep(POLL);
        }
        Ok(())
    }

    fn idle(&mut self, client: &Client, duration: Duration) -> TestResult<()> {
        // Observe private files and the real PTY only: no ctl, screenshot, event or render wake.
        let begin = Instant::now();
        let initial = read_json(&self.inbox().join(format!("managed-{}.json", client.cid)))
            .ok_or("Missing initial health")?["seq"]
            .as_u64()
            .unwrap_or(0);
        let mut stable = None;
        let initial_size = self.wait(
            "real native lease and backend PTY dimensions settle without an artificial UI wake",
            |h| {
                let lease =
                    fs::read_to_string(h.inbox().join(format!("managed-{}.lease", client.cid)))
                        .map_err(|e| e.to_string())?;
                let fields: Vec<_> = lease.split_whitespace().collect();
                let size = (
                    fields
                        .get(2)
                        .and_then(|s| s.parse::<u16>().ok())
                        .unwrap_or(0),
                    fields
                        .get(3)
                        .and_then(|s| s.parse::<u16>().ok())
                        .unwrap_or(0),
                );
                if size.0 <= 1 || size.1 <= 1 || tty_size(client.backend)? != size {
                    stable = None;
                    return Ok(None);
                }
                match stable {
                    Some((previous, since)) if previous == size => Ok((Instant::now()
                        .duration_since(since)
                        >= Duration::from_millis(600))
                    .then_some(size)),
                    _ => {
                        stable = Some((size, Instant::now()));
                        Ok(None)
                    }
                }
            },
        )?;
        let mut progressed = false;
        while begin.elapsed() < duration {
            let health = read_json(&self.inbox().join(format!("managed-{}.json", client.cid)))
                .ok_or("Health disappeared while idle")?;
            progressed |= health["seq"].as_u64().unwrap_or(0) > initial;
            let lease_path = self.inbox().join(format!("managed-{}.lease", client.cid));
            let lease = fs::read_to_string(&lease_path).map_err(|e| e.to_string())?;
            let fields: Vec<_> = lease.split_whitespace().collect();
            if fields.first().copied() != Some(client.token.as_str())
                || fields.get(1).and_then(|s| s.parse::<u64>().ok()) != Some(client.pane)
                || fields.get(4).copied() != Some(client.nonce.as_str())
            {
                return Err("Idle native lease lost its pinned client/pane identity".into());
            }
            let leased_size = (
                fields
                    .get(2)
                    .and_then(|s| s.parse::<u16>().ok())
                    .ok_or("Lease has no native rows")?,
                fields
                    .get(3)
                    .and_then(|s| s.parse::<u16>().ok())
                    .ok_or("Lease has no native cols")?,
            );
            let actual_size = tty_size(client.backend)?;
            if leased_size != initial_size || actual_size != initial_size {
                return Err(format!("Idle native lease/real PTY dimensions changed: lease {leased_size:?}, PTY {actual_size:?}, initial {initial_size:?}"));
            }
            let age = fs::metadata(&lease_path)
                .and_then(|m| m.modified())
                .map_err(|e| e.to_string())?
                .elapsed()
                .map_err(|e| e.to_string())?;
            if age >= Duration::from_secs(3)
                || !client.supervisor.alive()
                || !client.backend.alive()
            {
                return Err(format!(
                    "Idle lease expired or a real backend exited; lease age {age:?}"
                ));
            }
            thread::sleep(POLL);
        }
        if !progressed {
            return Err("Real supervisor health did not progress during idle observation".into());
        }
        Ok(())
    }

    fn shot(&self, name: &str) -> TestResult<()> {
        self.ctl("shot", &[name]).map(|_| ())
    }

    fn computed(&self, selector: &str, shot: &str) -> TestResult<Value> {
        let dump = self.ctl("dump", &[selector])?;
        let elements = dump["elements"].as_array().ok_or("Actual dump has no computed elements")?;
        if elements.len() != 1 || elements[0]["visible"] != true {
            return Err(format!("Expected one visible role root, not a child text source: {selector}: {dump}"));
        }
        self.shot(shot)?;
        Ok(elements[0].clone())
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
        self.focus(client.pane)?;
        self.expect_ui(ROWS, "native browser restored after Tern file close", |v| {
            !nodes(v).is_empty()
        })?;
        Ok(())
    }

    fn shell(&mut self, client: &Client, input: &str, background: bool) -> TestResult<Option<u64>> {
        self.focus(client.pane)?;
        self.key(if background { ";" } else { ":" })?;
        self.paste(client.pane, input)?;
        self.expect_ui("", "command bar contains the actual pasted script", |v| {
            ui_text(v).contains(input)
        })?;
        self.key("Enter")?;
        if background {
            return Ok(None);
        }
        self.wait("actual visible foreground native-shell terminal", |h| {
            let state = h.ctl("state", &[])?;
            let id = state["focused"]["id"].as_u64().unwrap_or(0);
            Ok((id != 0
                && id != client.pane
                && id != client.owner
                && state["focused"]["label"]
                    .as_str()
                    .is_some_and(|s| !s.starts_with("Companion"))
                && state["focused"]["prompt"] == false)
                .then_some(Some(id)))
        })
    }

    fn capture(&self, pane: u64) -> TestResult<String> {
        self.cli(&["capture", &pane.to_string()])
    }
    fn output(&mut self, pane: u64, expected: &str) -> TestResult<String> {
        self.wait(&format!("real terminal output {expected}"), |h| {
            let output = h.capture(pane)?;
            if output.contains(expected) {
                Ok(Some(output))
            } else {
                Err(format!("terminal capture: {output}"))
            }
        })
    }

    fn dismiss_shell(&mut self, client: &Client, pane: u64) -> TestResult<()> {
        self.focus(pane)?;
        self.key("Enter")?;
        self.wait("native shell pane closes after acknowledgment", |h| {
            let state = h.ctl("state", &[])?;
            Ok((state["focused"]["id"].as_u64() == Some(client.pane)).then_some(()))
        })?;
        self.expect_ui(ROWS, "browser remains live after foreground shell", |v| {
            !nodes(v).is_empty()
        })?;
        Ok(())
    }

    fn removed(&mut self, client: &Client, watched: &BTreeSet<Process>) -> TestResult<()> {
        self.wait(
            "owned supervisor/backend/command descendants exit and private controls disappear",
            |h| {
                let prefixes = [
                    format!("managed-{}", client.cid),
                    format!("state-{}", client.cid),
                    format!("listing-{}", client.cid),
                    format!("native-shell-{}-", client.cid),
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
                client.pane,
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
    h.case = "cold startup and idle lease";
    let first = h.launch(h.path("files"), false)?;
    h.idle(&first, Duration::from_secs(7))?;
    h.expect_ui(ROWS, "native block stays interactive after idle", |v| {
        !nodes(v).is_empty()
    })?;
    h.shot("01-cold-native")?;

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
        "idle command slot is blank",
        |v| nodes(v).len() == 1 && ui_text(v).trim().is_empty(),
    )?;
    let columns = h.tree("[data-role=\"yazi.column.current\"]")?;
    let idle_y = nodes(&columns)
        .first()
        .and_then(|n| n["rect"][1].as_f64())
        .ok_or("No visible current-column geometry")?;
    h.key(":")?;
    h.expect_ui(
        "[data-role=\"yazi.command-bubble.shell\"]",
        "shell editor occupies the reserved command row",
        |v| nodes(v).len() == 1 && theme_visible(&nodes(v)[0]),
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
        .ok_or("Native listing lacks alpha.txt")?;
    let next = entries
        .get(index + 1)
        .ok_or("Fixture has no next native row")?
        .clone();
    h.key("j")?;
    h.expect_state(&first, "j follows clicked native cursor", |s| {
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
    h.expect_state(&first, "H uses native directory history", |s| {
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
    h.paste(first.pane, "files")?;
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
    h.click_target("[data-role=\"yazi.path-edit\"]")?;
    h.paste(first.pane, "/does/not/exist/tern-yazi")?;
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
    h.paste(first.pane, &text(&long_dir))?;
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
        "native resize narrows the header before containment checks",
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
    h.paste(first.pane, &text(&first.cwd.join("alpha.txt")))?;
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
    h.open_path_editor(&breadcrumb_root)?;
    h.paste(first.pane, &text(&first.cwd))?;
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
    let bottom = text(&first.cwd.join(list.last().ok_or("Empty native listing")?));
    h.expect_state(&first, "G reaches native final row", |s| hover(s) == bottom)?;
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
    h.expect_state(&first, "second g reaches native first row", |s| {
        hover(s) == top
    })?;
    h.key("G")?;
    h.key("Home")?;
    h.expect_state(&first, "Home reaches native first row", |s| hover(s) == top)?;
    h.key("Space")?;
    let second_path = text(&first.cwd.join(&list[1]));
    h.expect_state(&first, "Space selects and advances native cursor", |s| {
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
    h.expect_state(&first, "v extends native visual range", |s| {
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
    h.expect_state(&first, "V creates native visual unset range", |s| {
        s["mode"] == "unset" && names(s, "marked_urls").len() >= 2
    })?;
    h.key("Escape")?;
    h.key("Escape")?;
    h.expect_state(
        &first,
        "Escape clears selection and keeps native panel",
        |s| s["mode"] == "normal" && selected(s).is_empty(),
    )?;
    h.expect_ui(ROWS, "Escape never closes live browser", |v| {
        !nodes(v).is_empty()
    })?;

    h.case = "smartcase filtering, Enter, visual Escape priority and stale bar";
    h.key("f")?;
    h.paste(first.pane, "alpha")?;
    h.expect_state(&first, "lowercase filter matches both cases", |s| {
        names(s, "files").into_iter().collect::<BTreeSet<_>>()
            == BTreeSet::from(["Alpha.txt".into(), "alpha.txt".into()])
    })?;
    h.key("Enter")?;
    h.expect_state(&first, "Enter retains native filter", |s| {
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
    h.expect_state(&first, "second Escape removes native filter", |s| {
        names(s, "files").len() > 2 && s["filter"].as_str().unwrap_or_default().is_empty()
    })?;
    h.key("f")?;
    h.paste(first.pane, "Alpha")?;
    h.key("Enter")?;
    h.expect_state(&first, "uppercase filter is case-sensitive", |s| {
        names(s, "files") == vec!["Alpha.txt"]
    })?;
    h.key("Escape")?;
    h.expect_state(&first, "uppercase filter clears", |s| {
        names(s, "files").len() > 2
    })?;

    h.case = "native slash find moves hover without filtering the listing";
    h.key("Escape")?;
    h.expect_state(&first, "find cases begin without retained selection", |s| {
        selected(s).is_empty()
    })?;
    h.key("Home")?;
    let full_listing = names(&h.snapshot(&first), "files");
    h.key("/")?;
    h.paste(first.pane, "alpha")?;
    h.key("Enter")?;
    h.expect_state(
        &first,
        "slash finds the first match without hiding entries",
        |s| hover(s) == text(&first.cwd.join("Alpha.txt")) && names(s, "files") == full_listing,
    )?;
    h.key("n")?;
    h.expect_state(&first, "n advances to the next native find match", |s| {
        hover(s) == text(&first.cwd.join("alpha.txt"))
    })?;
    h.key("N")?;
    h.expect_state(&first, "N returns to the previous native find match", |s| {
        hover(s) == text(&first.cwd.join("Alpha.txt"))
    })?;
    h.key("Escape")?;
    h.key("n")?;
    h.hover_stays(&first, &text(&first.cwd.join("Alpha.txt")))?;
    h.key("G")?;
    h.key("?")?;
    h.paste(first.pane, "alpha")?;
    h.key("Enter")?;
    h.expect_state(
        &first,
        "question mark searches backward without filtering",
        |s| hover(s) == text(&first.cwd.join("alpha.txt")) && names(s, "files") == full_listing,
    )?;
    h.key("Escape")?;
    h.key("f")?;
    h.paste(first.pane, "beta")?;
    h.key("Enter")?;
    h.expect_state(&first, "filter remains distinct from find", |s| {
        names(s, "files") == vec!["beta.txt"]
    })?;
    h.key("/")?;
    h.paste(first.pane, "beta")?;
    h.key("Enter")?;
    h.key("Escape")?;
    h.expect_state(
        &first,
        "Escape clears find before the retained filter",
        |s| names(s, "files") == vec!["beta.txt"] && s["filter"] == "beta",
    )?;
    h.key("Escape")?;
    h.expect_state(
        &first,
        "the following Escape clears the native filter",
        |s| names(s, "files") == full_listing,
    )?;

    h.case = "UTF-8, punctuation paste/editing and hidden native listing";
    h.key("f")?;
    h.paste(first.pane, "雪's 100%雪")?;
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
    h.expect_state(&first, "native hidden row initially absent", |s| {
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

    h.case = "Tern owns real text, Markdown and image previews and file opens";
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
                && ["runtime error", "stack traceback", "max_bytes", "host.luau"]
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
        h.expect_ui(PREVIEW, "actual file contents in native preview", |v| {
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
    h.shot("02-native-image-preview")?;
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
    h.focus(first.pane)?;
    h.key("Escape")?;
    h.expect_state(&first, "multiple-open selection clears", |s| {
        selected(s).is_empty()
    })?;
    h.click_node(ROWS, "notes.md", true)?;
    let file = h.opened(&first.cwd.join("notes.md"))?;
    h.shot("04-tern-markdown-open")?;
    h.close_file(&first, file)?;
    h.click_file(&first, "image.svg")?;
    h.click_node(
        "[data-role=\"yazi.preview-actions\"] button",
        "Open in Tern",
        false,
    )?;
    let file = h.opened(&first.cwd.join("image.svg"))?;
    h.shot("05-tern-image-open")?;
    h.close_file(&first, file)?;

    h.case = "typed colon ls uses real visible native terminal";
    h.key(":")?;
    h.key("l")?;
    h.key("s")?;
    h.expect_ui("", "typed l and s appear in command bar", |v| {
        ui_text(v).contains(":ls")
    })?;
    h.key("Enter")?;
    let terminal = h.wait("ls opens a real terminal", |h| {
        let state = h.ctl("state", &[])?;
        let id = state["focused"]["id"].as_u64().unwrap_or(0);
        Ok((id != 0 && id != first.pane && id != first.owner).then_some(id))
    })?;
    h.output(terminal, "alpha.txt")?;
    h.output(terminal, "[exit 0]")?;
    h.shot("06-real-colon-ls")?;
    h.dismiss_shell(&first, terminal)?;

    h.case = "native shell stdout stderr exit status and interactive stdin";
    let terminal = h
        .shell(
            &first,
            "printf 'REAL_STDOUT\\n'; printf 'REAL_STDERR\\n' >&2; exit 7",
            false,
        )?
        .ok_or("No foreground terminal")?;
    h.output(terminal, "REAL_STDOUT")?;
    h.output(terminal, "REAL_STDERR")?;
    h.output(terminal, "[exit 7]")?;
    h.dismiss_shell(&first, terminal)?;
    let terminal = h.shell(&first, "printf 'STDIN_READY\\n'; IFS= read -r answer; printf 'STDIN_ECHO=<%%s>\\n' \"$answer\"", false)?.ok_or("No stdin terminal")?;
    h.output(terminal, "STDIN_READY")?;
    h.paste(terminal, "雪's 100%")?;
    h.key("Enter")?;
    h.output(terminal, "STDIN_ECHO=<雪's 100%>")?;
    h.output(terminal, "[exit 0]")?;
    h.dismiss_shell(&first, terminal)?;

    h.case = "native selected/hover macro context preserves Unicode quotes percent and cwd";
    h.click_file(&first, "雪's 100%.txt")?;
    h.key("Space")?;
    h.expect_state(&first, "Unicode native selected path", |s| {
        selected(s).contains(&text(&first.cwd.join("雪's 100%.txt")))
    })?;
    h.click_file(&first, "beta.txt")?;
    let marker = h.path("native-args.txt");
    let cwd_marker = h.path("native-cwd.txt");
    let script = format!(
        "printf '%%s\\n' %h %s > {}; pwd > {}; printf 'MACRO_CONTEXT_DONE\\n'",
        quote(text(&marker)),
        quote(text(&cwd_marker))
    );
    let terminal = h
        .shell(&first, &script, false)?
        .ok_or("No macro terminal")?;
    h.output(terminal, "MACRO_CONTEXT_DONE")?;
    h.output(terminal, "[exit 0]")?;
    h.wait(
        "real shell expands hovered %h and selected %s paths exactly",
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
    h.dismiss_shell(&first, terminal)?;
    h.key("Escape")?;
    h.expect_state(&first, "macro selection cleared", |s| {
        selected(s).is_empty()
    })?;

    h.case = "semicolon background has real delayed effects without terminal";
    let marker = h.path("background.txt");
    let script = format!(
        "sleep 0.3; printf 'BACKGROUND_REAL_EFFECT\\n' > {}",
        quote(text(&marker))
    );
    let terminal_count = h.ctl("state", &[])?["panes"].as_array().map_or(0, Vec::len);
    h.shell(&first, &script, true)?;
    h.wait("real background command writes its delayed marker", |h| {
        let state = h.ctl("state", &[])?;
        if state["focused"]["id"].as_u64() != Some(first.pane) {
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

    h.case = "native yank copy and cut register downstream paste outcomes";
    for (name, key, dest, source_remains) in [
        ("copy.txt", "y", "copydest", true),
        ("cut.txt", "x", "cutdest", false),
    ] {
        h.click_file(&first, name)?;
        h.key(key)?;
        h.click_node(ROWS, dest, true)?;
        h.expect_state(
            &first,
            "UI navigation reaches isolated paste destination",
            |s| s["cwd"] == text(&first.cwd.join(dest)),
        )?;
        // The frontend deliberately has no p action. Only downstream paste is CLI-driven;
        // y/x and navigation above are genuine UI interactions, not emitted substitutes.
        h.execute("ya", &["emit-to".into(), first.cid.clone(), "paste".into()])?;
        let source = first.cwd.join(name);
        let destination = first.cwd.join(dest).join(name);
        h.wait("native yank register drives actual copy/cut filesystem task", |_| {
            let contents = fs::read_to_string(&destination).unwrap_or_default();
            let expected = if source_remains { "COPY_ORIGINAL\n" } else { "CUT_ORIGINAL\n" };
            if contents == expected && source.exists() == source_remains { Ok(Some(())) }
            else { Err(format!("destination {} contents {contents:?}; source exists {} expected {source_remains}", text(&destination), source.exists())) }
        })?;
        h.key("h")?;
        h.expect_state(&first, "return from paste destination", |s| {
            s["cwd"] == text(&first.cwd)
        })?;
    }

    h.case = "Trash exact native selection, Escape cancellation, confirmed disposable paths";
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
    h.expect_state(&first, "cancel leaves native selection unchanged", |s| {
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

    h.case =
        "plugin reload preserves pinned client token pane and never duplicates native terminal";
    h.click_file(&first, "alpha.txt")?;
    let terminal = h
        .shell(
            &first,
            "printf 'RELOAD_STDIN_READY\\n'; IFS= read -r reply; printf 'RELOAD_DONE\\n'",
            false,
        )?
        .ok_or("No reload terminal")?;
    h.output(terminal, "RELOAD_STDIN_READY")?;
    let before = h.ctl("state", &[])?;
    let terminal_count = before["panes"].as_array().map_or(0, Vec::len);
    h.cli(&["plugin", "reload"])?;
    h.wait("reload keeps exact native terminal focus without duplicate panes", |h| {
        let state = h.ctl("state", &[])?;
        let health = read_json(&h.inbox().join(format!("managed-{}.json", first.cid))).unwrap_or(Value::Null);
        let lease = fs::read_to_string(h.inbox().join(format!("managed-{}.lease", first.cid))).unwrap_or_default();
        let fields: Vec<_> = lease.split_whitespace().collect();
        if health["token"] == first.token && fields.first().copied() == Some(first.token.as_str())
            && fields.get(1).and_then(|s| s.parse::<u64>().ok()) == Some(first.pane)
            && fields.get(4).copied() == Some(first.nonce.as_str())
            && state["focused"]["id"].as_u64() == Some(terminal)
            && state["panes"].as_array().map_or(0, Vec::len) == terminal_count { Ok(Some(())) }
        else { Err(format!("reload focused {}, terminal count {} expected {terminal_count}; pinned pane {}", state["focused"]["id"], state["panes"].as_array().map_or(0, Vec::len), first.pane)) }
    })?;
    h.paste(terminal, "continue")?;
    h.key("Enter")?;
    h.output(terminal, "RELOAD_DONE")?;
    h.output(terminal, "[exit 0]")?;
    h.dismiss_shell(&first, terminal)?;
    h.idle(&first, Duration::from_secs(4))?;

    h.case = "two clients preserve independent cwd and hover";
    let second = h.launch(h.path("other"), true)?;
    if first.cid == second.cid || first.token == second.token || first.pane == second.pane {
        return Err("Two actual managed launches reused a pinned identity".into());
    }
    h.click_file(&second, "other.txt")?;
    let second_before = h.snapshot(&second);
    h.focus(first.pane)?;
    h.click_file(&first, "beta.txt")?;
    h.expect_state(&second, "other client cwd and hover do not change", |s| {
        s["cwd"] == second_before["cwd"] && hover(s) == hover(&second_before)
    })?;
    h.focus(second.pane)?;
    h.expect_ui(
        ROWS,
        "second client does not render first client files",
        |v| ui_text(v).contains("other.txt") && !ui_text(v).contains("beta.txt"),
    )?;

    h.case = "q cleans only the owning real backend process group";
    h.focus(first.pane)?;
    let watched = descendants(first.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&first, &watched)?;
    h.restored(&first, true, Some(first.owner))?;
    if !second.supervisor.alive() || !second.backend.alive() {
        return Err("Closing first client killed the unrelated second client".into());
    }
    h.focus(second.pane)?;
    h.expect_ui(ROWS, "second client survives first client q", |v| {
        ui_text(v).contains("other.txt")
    })?;

    h.case = "native pane close cleans its own backend";
    let watched = descendants(second.supervisor);
    h.owned.extend(watched.iter().copied());
    h.cli(&["close", &second.pane.to_string()])?;
    h.removed(&second, &watched)?;
    h.restored(&second, false, None)?;

    h.case = "owner close terminates live native command children and Yazi group";
    let third = h.launch(h.path("files"), true)?;
    let child_marker = h.path("native-child.pid");
    let script = format!("sleep 120 & child=$!; printf '%%s\\n' \"$child\" > {}; printf 'OWNED_CHILD_READY\\n'; wait", quote(text(&child_marker)));
    let terminal = h
        .shell(&third, &script, false)?
        .ok_or("No live child terminal")?;
    h.output(terminal, "OWNED_CHILD_READY")?;
    let child = h.wait("actual native command child PID", |_| {
        let pid = fs::read_to_string(&child_marker)
            .ok()
            .and_then(|s| s.trim().parse().ok());
        Ok(pid.and_then(process).map(|(p, _, _)| p))
    })?;
    let mut watched = descendants(third.supervisor);
    watched.insert(child);
    h.owned.extend(watched.iter().copied());
    h.cli(&["close", &third.owner.to_string()])?;
    h.removed(&third, &watched)?;
    h.wait("owner close also removes foreground native terminal", |h| {
        let state = h.ctl("state", &[])?;
        Ok((!state["panes"]
            .as_array()
            .is_some_and(|p| p.iter().any(|p| p["id"].as_u64() == Some(terminal))))
        .then_some(()))
    })?;

    h.case = "backend crash creates offline surface; q and reload never recreate controls";
    let fourth = h.launch(h.path("other"), true)?;
    let watched = descendants(fourth.supervisor);
    h.owned.extend(watched.iter().copied());
    fourth.backend.signal(libc::SIGKILL);
    h.expect_ui(
        ROWS,
        "crashed backend withdraws interactive file rows",
        |v| nodes(v).is_empty(),
    )?;
    for key in ["j", "Enter", "o"] {
        h.key(key)?;
    }
    h.wait(
        "offline actions neither open files nor leave the dead companion",
        |h| {
            let state = h.ctl("state", &[])?;
            Ok((state["focused"]["id"].as_u64() == Some(fourth.pane)
                && state["file_blocks"].as_array().is_some_and(Vec::is_empty))
            .then_some(()))
        },
    )?;
    h.shot("08-real-backend-offline")?;
    h.key("q")?;
    h.removed(&fourth, &watched)?;
    h.restored(&fourth, true, Some(fourth.owner))?;
    h.cli(&["plugin", "reload"])?;
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        for suffix in ["json", "lease", "stop", "lock"] {
            if h.inbox()
                .join(format!("managed-{}.{}", fourth.cid, suffix))
                .exists()
            {
                return Err("Offline q/reload recreated a private managed control file".into());
            }
        }
        if h.state_path(&fourth).exists() {
            return Err("Offline q/reload recreated the dead backend snapshot".into());
        }
        thread::sleep(POLL);
    }
    Ok(())
}

fn native_slot_flow(h: &mut Harness) -> TestResult<()> {
    for (path, content) in [
        ("slot/alpha.txt", "NATIVE_SLOT_ALPHA\n"),
        ("slot/beta.txt", "NATIVE_SLOT_BETA\n"),
    ] {
        h.write_fixture(path, content)?;
    }
    let cwd = h.path("slot");
    h.case = "already-split unzoomed owner keeps sibling, ratio and original native visual slot";
    let owner = h.fixture(json!({"op": "shell-tab", "cwd": text(&cwd)}))?["result"]
        .as_u64()
        .ok_or("Private slot fixture created no shell owner")?;
    h.shell_prompt(owner)?;
    let sibling = h
        .fixture(json!({"op": "split", "pane": owner, "dir": "right", "cwd": text(&cwd)}))?
        ["result"]
        .as_u64()
        .ok_or("Private slot fixture created no real sibling")?;
    h.shell_prompt(sibling)?;
    h.fixture(json!({"op": "resize", "pane": owner, "dir": "right", "cells": 11}))?;
    h.focus(owner)?;
    let before = h.layout_state()?;
    let tab = pane_info(&before, owner)
        .and_then(|p| p["tab"].as_u64())
        .ok_or("Split owner has no tab")?;
    let original = tab_layout(&before, tab).ok_or("Split owner has no real layout")?;
    if original["zoomed"] == true
        || !original["root"]["ratio"]
            .as_f64()
            .is_some_and(|ratio| (ratio - 0.5).abs() >= 0.01)
    {
        return Err(format!(
            "Real split fixture must start unzoomed with a non-equal ratio: {original}"
        ));
    }
    let split = h.launch(cwd.clone(), false)?;
    h.key("Escape")?;
    h.expect_ui(ROWS, "online Escape keeps the native browser open", |v| {
        ui_text(v).contains("alpha.txt")
    })?;
    h.idle(&split, Duration::from_secs(4))?;

    h.case = "reload and toggle reopen keep parked pair, lease nonce and no duplicate native pane";
    let pinned = h.layout_state()?;
    h.cli(&["plugin", "reload"])?;
    h.pinned_pair(&split, &pinned, split.pane)?;
    h.ctl("plugins", &["run", "plugin.tern-yazi.toggle"])?;
    h.pinned_pair(&split, &pinned, split.pane)?;
    h.case = "renderer close and reopen reconnect the same daemon-owned native pair";
    h.reopen_window()?;
    h.focus(split.pane)?;
    h.pinned_pair(&split, &pinned, split.pane)?;
    h.idle(&split, Duration::from_secs(4))?;
    h.case = "q restores exact split slot, sibling, ratio, tab and original shell ID";
    let watched = descendants(split.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&split, &watched)?;
    h.restored(&split, true, Some(owner))?;

    h.case = "raw native close with sibling restores owner in original tab without stealing sibling focus";
    h.focus(owner)?;
    let raw = h.launch(cwd.clone(), false)?;
    let watched = descendants(raw.supervisor);
    h.owned.extend(watched.iter().copied());
    h.focus(sibling)?;
    h.cli(&["close", &raw.pane.to_string()])?;
    h.removed(&raw, &watched)?;
    h.restored(&raw, false, Some(sibling))?;
    let state = h.layout_state()?;
    if pane_info(&state, owner).and_then(|p| p["tab"].as_u64()) != Some(tab)
        || pane_info(&state, sibling).and_then(|p| p["tab"].as_u64()) != Some(tab)
    {
        return Err(format!(
            "Raw close did not restore beside the surviving original-tab sibling: {state}"
        ));
    }

    h.case = "backend crash stays offline until Escape restores original owner and cleans controls";
    h.focus(owner)?;
    let offline = h.launch(cwd.clone(), false)?;
    let watched = descendants(offline.supervisor);
    h.owned.extend(watched.iter().copied());
    offline.backend.signal(libc::SIGKILL);
    h.expect_ui(ROWS, "real crashed backend withdraws file controls", |v| {
        nodes(v).is_empty()
    })?;
    for key in ["j", "Enter", "o"] {
        h.key(key)?;
    }
    h.wait(
        "offline native remains pinned with its actual owner parked",
        |h| {
            let state = h.layout_state()?;
            Ok((state["focused"].as_u64() == Some(offline.pane)
                && pane_info(&state, owner).is_some_and(|p| p["parked"] == true))
            .then_some(()))
        },
    )?;
    h.key("Escape")?;
    h.removed(&offline, &watched)?;
    h.restored(&offline, true, Some(owner))?;
    h.cli(&["plugin", "reload"])?;
    h.removed(&offline, &watched)?;

    h.case = "raw close of final native tab restores owner in a dedicated tab, not unrelated focused tab";
    let final_tab = h.launch(cwd.clone(), true)?;
    let watched = descendants(final_tab.supervisor);
    h.owned.extend(watched.iter().copied());
    h.focus(sibling)?;
    let unrelated = h.layout_state()?;
    h.cli(&["close", &final_tab.pane.to_string()])?;
    h.removed(&final_tab, &watched)?;
    h.restored(&final_tab, false, Some(sibling))?;
    let state = h.layout_state()?;
    let restored_tab = pane_info(&state, final_tab.owner)
        .and_then(|p| p["tab"].as_u64())
        .ok_or("Final-tab owner was not restored")?;
    if restored_tab == tab || tab_layout(&state, tab) != tab_layout(&unrelated, tab) {
        return Err(format!(
            "Final-tab restoration modified the unrelated focused tab: {state}"
        ));
    }

    h.case =
        "native moved to another tab restores the owner beside its surviving original-tab sibling";
    h.focus(owner)?;
    let moved = h.launch(cwd.clone(), false)?;
    h.fixture(json!({"op": "move-tab", "pane": moved.pane}))?;
    h.focus(moved.pane)?;
    let watched = descendants(moved.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&moved, &watched)?;
    h.restored(&moved, false, Some(owner))?;
    let state = h.layout_state()?;
    if pane_info(&state, owner).and_then(|p| p["tab"].as_u64()) != Some(tab)
        || pane_info(&state, sibling).and_then(|p| p["tab"].as_u64()) != Some(tab)
    {
        return Err(format!(
            "User-moved native returned its owner to an unrelated tab: {state}"
        ));
    }

    h.case =
        "two real owners close independently without stealing the unrelated native client focus";
    h.focus(owner)?;
    let first = h.launch(cwd.clone(), false)?;
    let second = h.launch(h.path("other"), true)?;
    if first.owner == second.owner
        || first.cid == second.cid
        || first.token == second.token
        || first.pane == second.pane
    {
        return Err("Independent actual owners reused a managed identity".into());
    }
    let second_before = h.layout_state()?;
    let watched = descendants(first.supervisor);
    h.owned.extend(watched.iter().copied());
    h.cli(&["close", &first.pane.to_string()])?;
    h.removed(&first, &watched)?;
    h.restored(&first, false, Some(second.pane))?;
    if !second.supervisor.alive()
        || !second.backend.alive()
        || tab_layout(
            &h.layout_state()?,
            second.original_tab.ok_or("Second owner lacks tab")?,
        ) != tab_layout(
            &second_before,
            second.original_tab.ok_or("Second owner lacks tab")?,
        )
    {
        return Err(
            "Closing first owner affected the other client's real processes or layout".into(),
        );
    }
    h.expect_ui(ROWS, "unrelated native client remains usable", |v| {
        ui_text(v).contains("other.txt")
    })?;
    let watched = descendants(second.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&second, &watched)?;
    h.restored(&second, true, Some(second.owner))?;

    h.case = "floating shell launches real Yazi in the same over/corner without changing the background tree";
    let background = h.fixture(json!({"op": "shell-tab", "cwd": text(&cwd)}))?["result"]
        .as_u64()
        .ok_or("Floating fixture created no real background shell")?;
    h.shell_prompt(background)?;
    let background_sibling = h
        .fixture(json!({"op": "split", "pane": background, "dir": "right", "cwd": text(&cwd)}))?
        ["result"]
        .as_u64()
        .ok_or("Floating fixture created no real background sibling")?;
    h.shell_prompt(background_sibling)?;
    h.fixture(json!({"op": "resize", "pane": background, "dir": "right", "cells": 9}))?;
    let floating_sibling = h.fixture(
        json!({"op": "split", "pane": background_sibling, "dir": "down", "cwd": text(&cwd)}),
    )?["result"]
        .as_u64()
        .ok_or("Floating fixture created no independent floating sibling shell")?;
    h.shell_prompt(floating_sibling)?;
    h.fixture(json!({"op": "float", "pane": floating_sibling, "over": background_sibling, "corner": "tl"}))?;
    let floating_owner = h
        .fixture(json!({"op": "split", "pane": background, "dir": "down", "cwd": text(&cwd)}))?
        ["result"]
        .as_u64()
        .ok_or("Floating fixture created no real owner shell")?;
    h.shell_prompt(floating_owner)?;
    h.fixture(json!({"op": "float", "pane": floating_owner, "over": background, "corner": "br"}))?;
    let floating = h.launch(cwd.clone(), false)?;
    let float_tab = floating.original_tab.ok_or("Floating owner has no tab")?;
    if floating.original_layout["zoomed"] != false
        || floating.original_layout["floats"]
            != json!([
                { "pane": floating_sibling, "over": background_sibling, "corner": "tl" },
                { "pane": floating_owner, "over": background, "corner": "br" }
            ])
    {
        return Err(format!(
            "Real floating fixture has unexpected presentation: {}",
            floating.original_layout
        ));
    }
    h.click_file(&floating, "beta.txt")?;
    h.key("k")?;
    h.expect_state(
        &floating,
        "floating native keyboard navigates the actual Yazi backend",
        |s| hover(s) == text(&cwd.join("alpha.txt")),
    )?;
    let floating_pinned = h.layout_state()?;
    h.cli(&["plugin", "reload"])?;
    h.pinned_pair(&floating, &floating_pinned, floating.pane)?;
    h.idle(&floating, Duration::from_secs(4))?;
    h.case = "floating q restores original shell PID and exact float/background presentation";
    let watched = descendants(floating.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&floating, &watched)?;
    h.restored(&floating, true, Some(floating_owner))?;

    h.case = "raw floating native close restores the original float without changing background focus or tree";
    h.focus(floating_owner)?;
    let floating_raw = h.launch(cwd.clone(), false)?;
    let watched = descendants(floating_raw.supervisor);
    h.owned.extend(watched.iter().copied());
    h.focus(background_sibling)?;
    h.cli(&["close", &floating_raw.pane.to_string()])?;
    h.removed(&floating_raw, &watched)?;
    h.restored(&floating_raw, true, Some(background_sibling))?;

    h.case = "floating backend crash and offline Escape restore the original float and Bash";
    h.focus(floating_owner)?;
    let floating_offline = h.launch(cwd.clone(), false)?;
    let watched = descendants(floating_offline.supervisor);
    h.owned.extend(watched.iter().copied());
    floating_offline.backend.signal(libc::SIGKILL);
    h.expect_ui(
        ROWS,
        "floating real backend crash withdraws file controls",
        |v| nodes(v).is_empty(),
    )?;
    h.key("Escape")?;
    h.removed(&floating_offline, &watched)?;
    h.restored(&floating_offline, true, Some(floating_owner))?;
    if tab_layout(&h.layout_state()?, float_tab).ok_or("Floating source tab disappeared")?["root"]
        != floating.original_layout["root"]
    {
        return Err("Floating lifecycle changed its actual background split tree".into());
    }
    h.case = "default native block discovers installed-prefix helper and original metadata without environment overrides";
    h.focus(owner)?;
    let before = h.layout_state()?;
    let pane = h.fixture(json!({"op": "new", "how": "tab"}))?["result"]
        .as_u64()
        .ok_or("No default native pane")?;
    let direct = h.standalone(pane, cwd.clone(), pane_ids(&before))?;
    let mut expected = pane_ids(&before);
    expected.insert(pane);
    if pane_ids(&h.layout_state()?) != expected {
        return Err("Default native block created an extra terminal or pane".into());
    }
    h.click_file(&direct, "beta.txt")?;
    h.key("k")?;
    h.expect_state(
        &direct,
        "default native keyboard controls the real backend",
        |s| hover(s) == text(&cwd.join("alpha.txt")),
    )?;
    let pinned = h.layout_state()?;
    h.cli(&["plugin", "reload"])?;
    h.pinned_pair(&direct, &pinned, pane)?;
    h.reopen_window()?;
    h.focus(pane)?;
    h.pinned_pair(&direct, &pinned, pane)?;
    h.idle(&direct, Duration::from_secs(4))?;
    let watched = descendants(direct.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&direct, &watched)?;
    if h.inbox().join(format!("native-{pane}.json")).exists() {
        return Err("Standalone native close left its live binding".into());
    }

    h.case = "default toggle starts a real backend with no extra terminal and closes cleanly after reload";
    h.focus(owner)?;
    let before = h.layout_state()?;
    h.ctl("plugins", &["run", "plugin.tern-yazi.toggle"])?;
    let pane = h.wait("toggle focuses a newly created real native block", |h| {
        let state = h.layout_state()?;
        let pane = state["focused"].as_u64().ok_or("Toggle has no focus")?;
        Ok((!pane_ids(&before).contains(&pane)
            && pane_info(&state, pane).is_some_and(|p| p["block"] == "tern-yazi.companion"))
        .then_some(pane))
    })?;
    let toggled = h.standalone(pane, cwd, pane_ids(&before))?;
    let mut expected = pane_ids(&before);
    expected.insert(pane);
    for _ in 0..2 {
        h.ctl("plugins", &["run", "plugin.tern-yazi.toggle"])?;
        if pane_ids(&h.layout_state()?) != expected {
            return Err("Toggle created a duplicate native block or extra terminal".into());
        }
    }
    h.focus(pane)?;
    let pinned = h.layout_state()?;
    h.cli(&["plugin", "reload"])?;
    h.pinned_pair(&toggled, &pinned, pane)?;
    let watched = descendants(toggled.supervisor);
    h.owned.extend(watched.iter().copied());
    h.cli(&["close", &pane.to_string()])?;
    h.removed(&toggled, &watched)?;
    if h.inbox().join(format!("native-{pane}.json")).exists() {
        return Err("Toggle native close left its live binding".into());
    }
    Ok(())
}

fn glyph_interaction_flow(h: &mut Harness) -> TestResult<()> {
    h.case = "isolated theme overrides export authoritative native glyphs";
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

    h.case = "hover conditions update both departed and newly hovered native rows";
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
    h.idle(&client, Duration::from_secs(1))?;
    for (mode, glyph) in [(0o600, ""), (0o700, "")] {
        let before = h.snapshot(&client);
        fs::set_permissions(
            client.cwd.join("exec-fixture"),
            fs::Permissions::from_mode(mode),
        )
        .map_err(|e| e.to_string())?;
        // No actor/ping/forced export: the native watcher must redraw and publish this
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

fn native_metadata_footer_headers(h: &mut Harness) -> TestResult<()> {
    h.case = "private native metadata fixtures";
    for dir in [
        "metadata",
        "metadata/many/nested",
        "metadata/contents/nested",
        "metadata/empty",
        "metadata/unreadable",
    ] {
        fs::create_dir_all(h.path(dir)).map_err(|e| e.to_string())?;
    }
    let large = "// NATIVE_METADATA_PREVIEW_LINE\n".repeat(2048);
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
            Ok(_) => return Err("Native unknown-count fixture requires an unprivileged runtime: mode 000 did not deny real directory loading".into()),
        }
        let client = h.launch(h.path("metadata"), true)?;
        h.expect_state(
            &client,
            "private current folder has seven authoritative immediate entries",
            |s| s["file_count"].as_u64() == Some(7) && names(s, "files").len() == 7,
        )?;

        h.case = "native file footer/header exact metadata and bounded preview";
        h.click_file(&client, "large.rs")?;
        h.expect_native_file_metadata(
            &client,
            "large.rs",
            (65536, "64.0 KB"),
            "-rw-r-----",
            "rust",
        )?;
        h.expect_ui(
            PREVIEW,
            "large real text fixture produces a preview rather than an unavailable fallback",
            |v| ui_text(v).contains("NATIVE_METADATA_PREVIEW_LINE"),
        )?;
        h.shot("09-native-file-metadata")?;

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
        h.expect_native_file_metadata(
            &client,
            "large.rs",
            (65536, "64.0 KB"),
            "-rwSr-----",
            "rust",
        )?;
        h.shot("09-native-special-permissions")?;
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
        h.expect_native_file_metadata(
            &client,
            "large.rs",
            (65536, "64.0 KB"),
            "-rw-r-----",
            "rust",
        )?;

        h.case = "file-to-file native selection keeps exact metadata without selection counters";
        h.key("Space")?;
        let selected_path = text(&client.cwd.join("large.rs"));
        let private_path = text(&client.cwd.join("private.txt"));
        h.expect_state(
            &client,
            "Space selects large file and advances the actual Yazi cursor",
            |s| selected(s) == BTreeSet::from([selected_path.clone()]) && hover(s) == private_path,
        )?;
        h.expect_native_file_metadata(&client, "private.txt", (8, "8 B"), "-rw-------", "text")?;
        h.key("Escape")?;
        h.expect_state(
            &client,
            "Escape clears the real selection before metadata navigation",
            |s| selected(s).is_empty(),
        )?;
        h.key("k")?;
        h.expect_native_file_metadata(
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
        h.expect_state(&client, "folder content bytes include descendants without directory inodes", |s| {
            hover(s) == many_path && s["hovered"]["size"].as_u64() == Some(314)
        })?;
        h.expect_metadata_text("[data-role=\"yazi.status-size\"]", "314 B")?;
        h.expect_metadata_text("[data-role=\"yazi.status-permissions\"]", "drwxr-x--x")?;
        h.expect_metadata_text("[data-role=\"yazi.status-name\"]", "many")?;
        h.expect_ui(
            CHILD_ROWS,
            "mounted child pool contains only authoritative capped immediate entries",
            |v| {
                !nodes(v).is_empty() && nodes(v).len() <= 30
                    && nodes(v).iter().all(|n| {
                        n["title"].as_str().is_some_and(|p| {
                            Path::new(p).parent() == Some(client.cwd.join("many").as_path())
                                && names(&many["preview"], "files").contains(&Path::new(p).file_name().unwrap().to_string_lossy().into_owned())
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
        h.expect_ui("[data-role=\"yazi.preview-metadata\"], [data-role=\"yazi.preview-size\"], [data-role=\"yazi.preview-permissions\"]", "directory preview removes stale file-only metadata", |v| nodes(v).is_empty())?;
        h.shot("10-native-folder-count-before-cap")?;

        h.case = "folder footer sums 137 plus nested 211 plus hidden 19 without changing Yazi state";
        h.click_file(&client, "regular.txt")?;
        h.expect_state(&client, "regular file exports its real 23 bytes", |s| s["hovered"]["size"].as_u64() == Some(23))?;
        h.expect_metadata_text("[data-role=\"yazi.status-size\"]", "23 B")?;
        h.key("Space")?;
        h.expect_state(&client, "folder usage starts with a real retained file selection", |s| {
            selected(s) == BTreeSet::from([text(&client.cwd.join("regular.txt"))])
        })?;
        h.click_file(&client, "contents")?;
        let calculation_state = h.expect_state(&client, "folder calculation starts from a coherent actor/listing pair", |s| {
            hover(s) == text(&client.cwd.join("contents")) && s["hovered"]["dir"] == true
        })?;
        let contents_path = text(&client.cwd.join("contents"));
        h.expect_state(&client, "recursive folder usage becomes 367 bytes with hidden files included", |s| {
            hover(s) == contents_path && s["hovered"]["size"].as_u64() == Some(367)
                && ["cwd", "files", "cursor", "filter", "finder", "mode", "selected_urls", "marked_urls"]
                    .iter().all(|key| s[*key] == calculation_state[*key])
        })?;
        h.expect_metadata_text("[data-role=\"yazi.status-size\"]", "367 B")?;
        h.expect_metadata_text("[data-role=\"yazi.status-name\"]", "contents")?;
        // Start repeated real directory calculations and immediately leave their URL.
        // No injected callbacks or forced snapshots substitute for async ownership.
        for _ in 0..8 {
            h.click_node(ROWS, "many", false)?;
            h.click_node(ROWS, "regular.txt", false)?;
        }
        h.expect_state(&client, "old folder completion cannot replace the current regular-file bytes", |s| {
            hover(s) == text(&client.cwd.join("regular.txt"))
                && s["hovered"]["size"].as_u64() == Some(23)
        })?;
        let stable = h.snapshot(&client);
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            let current = h.snapshot(&client);
            if !current.is_null() && (current["hovered"] != stable["hovered"]
                || ["files", "cursor", "filter", "finder", "selected_urls", "marked_urls"]
                    .iter().any(|key| current[*key] != stable[*key])) {
                return Err(format!("Stale directory computation changed current file: {current}"));
            }
            h.expect_metadata_text("[data-role=\"yazi.status-size\"]", "23 B")?;
            thread::sleep(POLL);
        }
        h.key("Escape")?;
        h.expect_state(&client, "native Escape clears the retained size-fixture selection", |s| selected(s).is_empty())?;

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
        // Revisit after a real native cursor hop; managed idle sessions do not poll snapshots.
        h.key("j")?;
        h.expect_state(
            &client,
            "native cursor leaves the failed directory for the real large file",
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
        h.shot("11-native-unreadable-folder-unknown")?;

        h.case = "folder-to-file restores only exact hovered metadata";
        h.click_file(&client, "private.txt")?;
        h.expect_native_file_metadata(&client, "private.txt", (8, "8 B"), "-rw-------", "text")?;
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

fn theme_columns(h: &Harness) -> TestResult<[[f64; 4]; 3]> {
    Ok([
        metadata_rect(&h.tree("[data-role=\"yazi.column.parent\"]")?)?,
        metadata_rect(&h.tree("[data-role=\"yazi.column.current\"]")?)?,
        metadata_rect(&h.tree("[data-role=\"yazi.column.preview\"]")?)?,
    ])
}

fn theme_wide_layout(h: &mut Harness, id: &str, scale: f64) -> TestResult<[[f64; 4]; 3]> {
    h.wait(&format!("{id} has its actual wide native geometry"), |h| {
        let columns = theme_columns(h)?;
        let browser = metadata_rect(&h.tree("[data-role=\"yazi.columns\"]")?)?;
        let [parent, current, preview] = columns;
        let gap = current[0] - parent[0] - parent[2];
        let preview_gap = preview[0] - current[0] - current[2];
        let expected = match id {
            "current" => {
                let unit = (browser[2] - 12.0 * scale) / 20.0;
                [unit * 4.0, unit * 9.0, 6.0 * scale]
            }
            "aurora" => [172.0 * scale, 264.0 * scale, 12.0 * scale],
            "editorial" => [150.0 * scale, 254.0 * scale, 0.0],
            "amber" => [186.0 * scale, 268.0 * scale, 12.0 * scale],
            _ => return Err(format!("Unknown theme test case: {id}")),
        };
        let bounds = columns.into_iter().all(|rect| metadata_contains(browser, rect))
            && metadata_before(parent, current)
            && metadata_before(current, preview)
            && (parent[1] - current[1]).abs() < 1.0
            && (current[1] - preview[1]).abs() < 1.0
            && (parent[3] - preview[3]).abs() < 1.0;
        if bounds
            && (parent[2] - expected[0]).abs() < 2.0
            && (current[2] - expected[1]).abs() < 2.0
            && (gap - expected[2]).abs() < 2.0
            && (preview_gap - expected[2]).abs() < 2.0
            && preview[2] > 220.0
        {
            Ok(Some(columns))
        } else {
            Err(format!("{id}: browser {browser:?}, columns {columns:?}, gaps {gap}/{preview_gap}, expected {expected:?}"))
        }
    })
}

fn theme_narrow_layout(h: &mut Harness, id: &str) -> TestResult<()> {
    h.wait(&format!("{id} narrow native pane retains bounded side-by-side browsing"), |h| {
        let current = metadata_rect(&h.tree("[data-role=\"yazi.column.current\"]")?)?;
        let preview = metadata_rect(&h.tree("[data-role=\"yazi.column.preview\"]")?)?;
        let browser = metadata_rect(&h.tree("[data-role=\"yazi.columns\"]")?)?;
        let parents = h.tree("[data-role=\"yazi.column.parent\"]")?;
        if id == "current" {
            let parent = metadata_rect(&parents)?;
            if metadata_before(parent, current) && metadata_before(current, preview)
                && metadata_contains(browser, parent) && metadata_contains(browser, preview)
            {
                return Ok(Some(()));
            }
        } else if nodes(&parents).iter().all(|n| !theme_visible(n))
            && metadata_contains(browser, current) && metadata_contains(browser, preview)
            && metadata_before(current, preview)
            && (current[1] - preview[1]).abs() < 1.0
            && (current[3] - preview[3]).abs() < 1.0
            && (preview[2] / current[2] - 1.15).abs() < 0.05
        {
            return Ok(Some(()));
        }
        Err(format!("{id} narrow bounds: browser {browser:?}, current {current:?}, preview {preview:?}, parent {parents}"))
    })
}

fn theme_identity(h: &Harness, client: &Client, before: &Value) -> TestResult<()> {
    let snapshot = h.snapshot(client);
    for key in [
        "client_id",
        "cwd",
        "hovered",
        "selected_urls",
        "marked_urls",
        "mode",
        "files",
        "file_icons",
    ] {
        if snapshot[key] != before[key] {
            return Err(format!(
                "Theme changed authoritative {key}: before {}, after {}",
                before[key], snapshot[key]
            ));
        }
    }
    let health = read_json(&h.inbox().join(format!("managed-{}.json", client.cid)))
        .ok_or("Theme switch lost managed health")?;
    let lease = fs::read_to_string(h.inbox().join(format!("managed-{}.lease", client.cid)))
        .map_err(|e| e.to_string())?;
    let lease: Vec<_> = lease.split_whitespace().collect();
    if health["token"] != client.token
        || lease.first().copied() != Some(client.token.as_str())
        || lease.get(1).and_then(|s| s.parse::<u64>().ok()) != Some(client.pane)
        || !client.supervisor.alive()
        || !client.backend.alive()
    {
        return Err(format!(
            "Theme replaced backend/client identity: health {health}, lease {lease:?}"
        ));
    }
    Ok(())
}

fn theme_preview_controls(h: &mut Harness, id: &str) -> TestResult<()> {
    h.expect_ui(
        "[data-role=\"yazi.preview-actions\"] button",
        "only Current retains three visible preview actions",
        |v| {
            let visible = nodes(v).iter().filter(|n| theme_visible(n)).count();
            visible == if id == "current" { 3 } else { 0 }
        },
    )?;
    Ok(())
}

fn theme_switch(h: &Harness, id: &str) -> TestResult<()> {
    h.ctl("plugins", &["run", &format!("plugin.tern-yazi.theme_{id}")])?;
    Ok(())
}

fn theme_preview_scroll(h: &Harness) -> TestResult<f64> {
    let preview = metadata_rect(&h.tree(PREVIEW)?)?;
    let code = metadata_rect(&h.tree(&format!("{PREVIEW} .sf-code"))?)?;
    Ok(preview[1] - code[1])
}

fn theme_unchanged_columns(h: &mut Harness, before: [[f64; 4]; 3]) -> TestResult<()> {
    h.wait("reserved command row prevents any native column jump", |h| {
        let after = theme_columns(h)?;
        if before.iter().zip(after).all(|(old, new)| old.iter().zip(new).all(|(a, b)| (a - b).abs() < 1.0)) {
            Ok(Some(()))
        } else { Err(format!("Reserved slot moved columns: {before:?} -> {after:?}")) }
    })
}

fn theme_pointer_hover(h: &mut Harness, selector: &str, name: &str, idle_alpha: f64, hover_alpha: f64) -> TestResult<()> {
    h.ctl("move", &["0", "0"])?;
    let idle = h.computed(selector, &format!("12-input-{name}-idle"))?;
    let rect = metadata_rect(&h.tree(selector)?)?;
    h.ctl("move", &[&(rect[0] + rect[2] / 2.0).to_string(), &(rect[1] + rect[3] / 2.0).to_string()])?;
    h.expect_ui(&format!("{selector}:hover"), "actual pointer reaches the role-bearing native control", |v| nodes(v).len() == 1)?;
    let alpha = |color: &Value| -> Option<f64> {
        let color = color.as_str()?;
        if color.starts_with("rgba(") { color.trim_end_matches(')').rsplit(',').next()?.trim().parse().ok() }
        else if color.starts_with("rgb(") { Some(1.0) } else { None }
    };
    h.wait("native hovered control visibly changes its own computed background", |h| {
        let hovered = h.computed(selector, &format!("12-input-{name}-hover"))?;
        if idle["backgroundColor"] != hovered["backgroundColor"]
            && alpha(&idle["backgroundColor"]).is_some_and(|value| (value - idle_alpha).abs() < 0.015)
            && alpha(&hovered["backgroundColor"]).is_some_and(|value| (value - hover_alpha).abs() < 0.015) {
            Ok(Some(()))
        } else { Err(format!("Root hover color did not visibly change: idle={idle}, hover={hovered}")) }
    })
}

fn theme_command_geometry(h: &mut Harness, id: &str, kind: &str, columns: [[f64; 4]; 3], scale: f64) -> TestResult<()> {
    let label_selector = "[data-role=\"yazi.command-label\"]";
    let bubble_selector = format!("[data-role=\"yazi.command-bubble.{kind}\"]");
    let cancel_selector = "[data-role=\"yazi.command-cancel\"]";
    h.expect_ui(&bubble_selector, "real typed query remains visible in its theme-specific command bubble", |v| {
        nodes(v).len() == 1 && theme_visible(nodes(v)[0]) && !ui_text(v).trim().is_empty()
    })?;
    theme_unchanged_columns(h, columns)?;
    h.wait("theme command label, query and independent cancel stay inside the reserved row", |h| {
        let row = metadata_rect(&h.tree("[data-role=\"yazi.input-row\"]")?)?;
        let label = metadata_rect(&h.tree(label_selector)?)?;
        let bubble = metadata_rect(&h.tree(&bubble_selector)?)?;
        let cancel = metadata_rect(&h.tree(cancel_selector)?)?;
        let expected_gap = match id { "aurora" => Some(10.0 * scale), "editorial" => Some(12.0 * scale), "amber" => Some(0.0), _ => None };
        let gaps = [bubble[0] - label[0] - label[2], cancel[0] - bubble[0] - bubble[2]];
        if [label, bubble, cancel].iter().all(|rect| metadata_contains(row, *rect))
            && gaps.iter().all(|gap| *gap >= -1.0)
            && expected_gap.map_or(true, |expected| gaps.iter().all(|gap| (gap - expected).abs() < 1.5))
            && bubble[2] > 80.0 * scale {
            Ok(Some(()))
        } else { Err(format!("{id}/{kind}: row={row:?}, label={label:?}, bubble={bubble:?}, cancel={cancel:?}, gap={expected_gap:?}")) }
    })?;
    let label = h.computed(label_selector, &format!("12-input-{id}-{kind}-label"))?;
    let bubble = h.computed(&bubble_selector, &format!("12-input-{id}-{kind}-shape"))?;
    let font_px = |value: &Value| value.as_str().and_then(|s| s.strip_suffix("px")).and_then(|s| s.parse::<f64>().ok());
    let label_family = label["fontFamily"].as_str().ok_or("No computed mode-label font")?;
    let bubble_family = bubble["fontFamily"].as_str().ok_or("No computed query font")?;
    let ratio = font_px(&label["fontSize"]).ok_or("No actual label font size")?
        / font_px(&bubble["fontSize"]).ok_or("No actual query font size")?;
    let font_correct = match id {
        "aurora" => label_family != bubble_family && (ratio - 11.0 / 12.5).abs() < 0.025,
        "editorial" => label_family == "serif" && (ratio - 14.0 / 13.0).abs() < 0.025
            && bubble["backgroundColor"] == "rgba(0, 0, 0, 0)",
        "amber" => label_family == bubble_family && bubble_family.to_lowercase().contains("mono")
            && (ratio - 11.0 / 12.0).abs() < 0.025,
        "current" => !label_family.is_empty() && !bubble_family.is_empty(),
        _ => false,
    };
    if !font_correct { return Err(format!("Wrong native theme command typography: label={label}, query={bubble}")); }
    Ok(())
}

fn theme_actual_inputs(h: &mut Harness, client: &Client, id: &str, scale: f64, identity: &Value) -> TestResult<()> {
    h.case = "four themes preserve real shell/find/filter input geometry and Cancel/submit behavior";
    theme_switch(h, id)?;
    let columns = theme_wide_layout(h, id, scale)?;
    let alternate_path = text(&h.path("files"));
    let effect = h.path(&format!("theme-command-{id}.txt"));
    let query = format!("printf REAL_THEME_{id} > {}", quote(text(&effect)));
    h.key(":")?;
    h.paste(client.pane, &query)?;
    theme_command_geometry(h, id, "shell", columns, scale)?;
    h.click_target("[data-role=\"yazi.command-cancel\"]")?;
    h.expect_ui("[data-role=\"yazi.input-row\"]", "pointer Cancel returns to the blank reserved command slot", |v| ui_text(v).trim().is_empty())?;
    if effect.exists() { return Err("Canceled shell query executed a real side effect".into()); }
    let panes = pane_ids(&h.ctl("state", &[])?);
    h.key(";")?;
    h.paste(client.pane, &query)?;
    h.key("Enter")?;
    h.wait("theme shell submission executes its real background command without opening a native terminal", |h| {
        let state = h.ctl("state", &[])?;
        Ok((fs::read_to_string(&effect).is_ok_and(|value| value == format!("REAL_THEME_{id}"))
            && pane_ids(&state) == panes && state["focused"]["id"].as_u64() == Some(client.pane)).then_some(()))
    })?;
    theme_unchanged_columns(h, columns)?;
    h.key("/")?;
    h.paste(client.pane, "row-063")?;
    theme_command_geometry(h, id, "find", columns, scale)?;
    h.click_target("[data-role=\"yazi.command-cancel\"]")?;
    h.expect_ui("[data-role=\"yazi.input-row\"]", "pointer Cancel dismisses the real find query without filtering", |v| ui_text(v).trim().is_empty())?;
    h.expect_state(client, "find cancellation keeps full native order and original selection", |s| {
        names(s, "files") == names(identity, "files") && selected(s) == selected(identity)
    })?;
    h.key("/")?;
    h.paste(client.pane, "row-063")?;
    h.key("Enter")?;
    h.expect_state(client, "submitted find retains full native listing and reaches the exact file", |s| {
        names(s, "files") == names(identity, "files") && hover(s) == text(&client.cwd.join("row-063.rs"))
    })?;
    h.key("Escape")?;
    h.key("f")?;
    h.paste(client.pane, "row-063")?;
    h.expect_state(client, "theme filter uses Yazi's real listing projection", |s| names(s, "files") == vec!["row-063.rs"])?;
    theme_command_geometry(h, id, "filter", columns, scale)?;
    h.click_target("[data-role=\"yazi.command-cancel\"]")?;
    h.expect_state(client, "pointer Clear restores full native filter order and frozen selection", |s| {
        names(s, "files") == names(identity, "files") && selected(s) == selected(identity) && s["filter"] == ""
    })?;
    h.key("G")?;
    theme_unchanged_columns(h, columns)?;
    h.expect_state(client, "input cancellation and final jump retain the original coherent native identity", |s| {
        ["cwd", "files", "hovered", "selected_urls", "marked_urls", "mode"]
            .iter().all(|key| s[*key] == identity[*key])
    })?;
    theme_identity(h, client, identity)?;

    h.case = "four themes render visible root Go/Cancel pointer hover and actual path actions";
    h.open_path_editor(&text(&client.cwd))?;
    let (idle, hovered) = match id { "current" => (0.04, 0.20), "aurora" => (0.05, 0.24), "editorial" => (0.0, 0.18), "amber" => (0.06, 0.26), _ => unreachable!() };
    for (role, name) in [("yazi.path-go", "go"), ("yazi.path-cancel", "cancel")] {
        theme_pointer_hover(h, &format!("[data-role=\"{role}\"]"), &format!("{id}-{name}"), idle, hovered)?;
    }
    h.paste(client.pane, &text(&h.path("files")))?;
    h.click_target("[data-role=\"yazi.path-cancel\"]")?;
    h.expect_ui("[data-role=\"yazi.path-input\"]", "pointer Cancel dismisses the real path editor", |v| nodes(v).is_empty())?;
    theme_identity(h, client, identity)?;
    h.open_path_editor(&text(&client.cwd))?;
    h.paste(client.pane, &text(&h.path("files")))?;
    h.click_target("[data-role=\"yazi.path-go\"]")?;
    h.expect_state(client, "pointer Go submits the edited path to real Yazi", |s| s["cwd"] == alternate_path)?;
    h.open_path_editor(&text(&h.path("files")))?;
    h.paste(client.pane, &text(&client.cwd))?;
    h.click_target("[data-role=\"yazi.path-go\"]")?;
    h.expect_state(client, "pointer Go returns to the original native directory", |s| s["cwd"] == text(&client.cwd))?;
    h.key("G")?;
    h.expect_state(client, "path actions retain original global selection and order", |s| {
        names(s, "files") == names(identity, "files") && selected(s) == selected(identity)
            && hover(s) == hover(identity)
    })?;
    theme_unchanged_columns(h, columns)
}

fn native_layout_themes(h: &mut Harness) -> TestResult<()> {
    h.case = "native layout theme fixture and untouched Current default";
    fs::create_dir_all(h.path("themes")).map_err(|e| e.to_string())?;
    for index in 0..64 {
        h.write_fixture(
            &format!("themes/row-{index:03}.rs"),
            "// NATIVE_THEME_SCROLL_LINE\n".repeat(1024),
        )?;
    }
    h.write_fixture("themes/image.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"40\" height=\"40\"><rect width=\"40\" height=\"40\" fill=\"red\"/></svg>")?;
    h.write_fixture("themes/Selected-Case.txt", "SELECTED_CASE_CONTENT\n")?;
    let first = h.launch(h.path("themes"), true)?;
    h.ctl("resize", &["1280", "900"])?;
    h.click_file(&first, "Selected-Case.txt")?;
    h.key("Space")?;
    h.expect_state(
        &first,
        "real selected case-sensitive path survives cursor advance",
        |s| selected(s) == BTreeSet::from([text(&first.cwd.join("Selected-Case.txt"))]),
    )?;
    h.case = "default Ctrl+Shift+P opens the real command palette with all four Yazi layout entries";
    h.key("ctrl+shift+p")?;
    for key in ["Y", "a", "z", "i", ":", "Space", "L", "a", "y", "o", "u", "t"] {
        h.key(key)?;
    }
    let titles = [
        "Yazi: Layout - Current",
        "Yazi: Layout - Aurora Glass",
        "Yazi: Layout - Editorial Paper",
        "Yazi: Layout - Amber Ledger",
    ];
    h.expect_ui("", "all four layout commands are visible in the actual keyboard-opened palette", |v| {
        titles.iter().all(|title| {
            nodes(v).iter().any(|node| node["text"].as_str() == Some(*title) && theme_visible(node))
        })
    })?;
    h.shot("12-native-theme-command-palette")?;
    h.key("Escape")?;
    h.expect_ui("", "Escape closes the actual palette without leaving the real Yazi pane", |v| {
        !titles.iter().any(|title| nodes(v).iter().any(|node| node["text"].as_str() == Some(*title) && theme_visible(node)))
    })?;
    h.case = "native layout geometry and scroll retain the untouched Current baseline";
    h.key("G")?;
    let before = h.expect_state(
        &first,
        "native G selects the final row without clearing persistent selection",
        |s| {
            hover(s) == text(&first.cwd.join("row-063.rs"))
                && selected(s) == BTreeSet::from([text(&first.cwd.join("Selected-Case.txt"))])
        },
    )?;
    h.expect_ui(
        PREVIEW,
        "real code preview renders before theme migration",
        |v| ui_text(v).contains("NATIVE_THEME_SCROLL_LINE"),
    )?;
    // Native regions scale logical pixels with their inherited font; calibrate from untouched Current.
    let initial = theme_columns(h)?;
    let scale = (initial[1][0] - initial[0][0] - initial[0][2]) / 6.0;
    if !(0.5..=3.0).contains(&scale) {
        return Err(format!(
            "Untouched Current has an invalid native pixel scale: {initial:?}"
        ));
    }
    let baseline = theme_wide_layout(h, "current", scale)?;
    theme_preview_controls(h, "current")?;
    let preview_rect = metadata_rect(&h.tree(PREVIEW)?)?;
    h.ctl(
        "move",
        &[
            &(preview_rect[0] + preview_rect[2] / 2.0).to_string(),
            &(preview_rect[1] + preview_rect[3] / 2.0).to_string(),
        ],
    )?;
    h.ctl("wheel", &["0", "160"])?;
    let baseline_scroll = h.wait(
        "real wheel scrolls code preview independently of the native list",
        |h| {
            let offset = theme_preview_scroll(h)?;
            Ok((offset > 100.0).then_some(offset))
        },
    )?;
    let baseline_row = metadata_rect(&h.tree(&format!("{ROWS}.sel"))?)?;

    h.case = "all four palette commands preserve native identity selection and scroll";
    for (id, row_height) in [
        ("aurora", 29.0 * scale),
        ("editorial", 28.0 * scale),
        ("amber", 26.0 * scale),
        ("current", baseline_row[3]),
    ] {
        theme_switch(h, id)?;
        theme_wide_layout(h, id, scale)?;
        theme_preview_controls(h, id)?;
        theme_identity(h, &first, &before)?;
        h.wait("theme rows retain native final cursor and bounded scrollers", |h| {
            let card = metadata_rect(&h.tree("[data-role=\"yazi.column.current\"]")?)?;
            let row = metadata_rect(&h.tree(&format!("{ROWS}.sel"))?)?;
            let scroller = metadata_rect(&h.tree("[data-role=\"yazi.column.current\"] .sf-list-scroll.max")?)?;
            let scroll = theme_preview_scroll(h)?;
            if (row[3] - row_height).abs() < 2.0 && metadata_contains(card, row)
                && metadata_contains(scroller, row) && row[1] + row[3] >= scroller[1] + scroller[3] - 40.0 * scale
                && (scroll - baseline_scroll).abs() < 1.0
            {
                Ok(Some(()))
            } else {
                Err(format!("{id}: cursor {row:?}, card {card:?}, scroller {scroller:?}, preview offset {scroll}, baseline {baseline_scroll}"))
            }
        })?;
        if id == "amber" {
            h.expect_ui(
                &format!("{ROWS}.sel"),
                "ledger ordinal is the real ordered row number, not the cursor index",
                |v| ui_text(v).starts_with("66 ") && ui_text(v).ends_with("row-063.rs"),
            )?;
            h.expect_ui(
                "[data-role=\"yazi.selection-chip\"]",
                "bounded tray contains actual selected filename and complete path tooltip",
                |v| {
                    nodes(v).len() == 1
                        && nodes(v)[0]["text"] == "Selected-Case.txt"
                        && nodes(v)[0]["title"] == text(&first.cwd.join("Selected-Case.txt"))
                        && theme_visible(nodes(v)[0])
                },
            )?;
        }
        h.shot(&format!("12-native-theme-{id}-wide"))?;
    }
    let restored = theme_wide_layout(h, "current", scale)?;
    if baseline
        .iter()
        .zip(restored)
        .any(|(old, new)| old.iter().zip(new).any(|(a, b)| (a - b).abs() > 1.0))
    {
        return Err(format!(
            "Current roundtrip changed original geometry: {baseline:?} -> {restored:?}"
        ));
    }
    let restored_row = metadata_rect(&h.tree(&format!("{ROWS}.sel"))?)?;
    if baseline_row
        .iter()
        .zip(restored_row)
        .any(|(a, b)| (a - b).abs() > 1.0)
    {
        return Err(format!("Current roundtrip lost native list cursor/scroll geometry: {baseline_row:?} -> {restored_row:?}"));
    }
    let restored_scroll = theme_preview_scroll(h)?;
    if (baseline_scroll - restored_scroll).abs() > 1.0 {
        return Err(format!("Current roundtrip lost native preview scroller position: {baseline_scroll} -> {restored_scroll}"));
    }
    for id in ["current", "aurora", "editorial", "amber"] {
        theme_actual_inputs(h, &first, id, scale, &before)?;
    }
    theme_switch(h, "current")?;

    h.case = "all existing companion panes consume layout commands and preserve their own state";
    let second = h.launch(h.path("themes"), true)?;
    h.click_file(&second, "row-000.rs")?;
    let second_before = h.snapshot(&second);
    for id in ["aurora", "editorial", "amber", "current"] {
        theme_switch(h, id)?;
        for (client, snapshot) in [(&first, &before), (&second, &second_before)] {
            h.focus(client.pane)?;
            theme_wide_layout(h, id, scale)?;
            theme_identity(h, client, snapshot)?;
        }
    }

    h.case = "theme preference survives native plugin reload and new companion pane";
    theme_switch(h, "amber")?;
    h.cli(&["plugin", "reload"])?;
    for (client, snapshot) in [(&first, &before), (&second, &second_before)] {
        h.focus(client.pane)?;
        theme_wide_layout(h, "amber", scale)?;
        theme_identity(h, client, snapshot)?;
    }
    let third = h.launch(h.path("themes"), true)?;
    theme_wide_layout(h, "amber", scale)?;
    h.case = "Amber selected-filename tray is bounded and authoritative";
    h.key("ctrl+a")?;
    let tray_snapshot = h.expect_state(
        &third,
        "native toggle_all selects all real fixture entries",
        |s| selected(s).len() == 66,
    )?;
    let selected_paths = selected(&tray_snapshot);
    h.expect_ui(
        "[data-role=\"yazi.selection-chip\"]",
        "tray clips to six real case-sensitive names with full-path titles",
        |v| {
            nodes(v).len() == 6
                && nodes(v).iter().all(|node| {
                    let Some(path) = node["title"].as_str() else {
                        return false;
                    };
                    selected_paths.contains(path)
                        && Path::new(path).file_name().and_then(|name| name.to_str())
                            == node["text"].as_str()
                        && theme_visible(node)
                })
        },
    )?;
    h.expect_metadata_text("[data-role=\"yazi.selection-overflow\"]", "+60")?;
    h.wait(
        "ledger tray stays a bounded single-line native dock row",
        |h| {
            let tray = metadata_rect(&h.tree("[data-role=\"yazi.selection-tray\"]")?)?;
            let chips = h.tree("[data-role=\"yazi.selection-chip\"]")?;
            if nodes(&chips).iter().all(|node| {
                let rect = &node["rect"];
                let x = rect[0].as_f64().unwrap_or(-1.0);
                let y = rect[1].as_f64().unwrap_or(-1.0);
                let width = rect[2].as_f64().unwrap_or(0.0);
                let height = rect[3].as_f64().unwrap_or(0.0);
                metadata_contains(tray, [x, y, width, height]) && (y - tray[1]).abs() <= 6.0
            }) {
                Ok(Some(()))
            } else {
                Err(format!("Tray {tray:?} overflowed with chips {chips}"))
            }
        },
    )?;
    h.expect_ui(
        ".sf",
        "ledger has no resurrected selection counter or source/info tabs",
        |v| {
            let text = ui_text(v);
            !text.contains(" selected")
                && !text.contains("Synced")
                && !text.contains("Source")
                && !text.contains("Info")
        },
    )?;
    h.key("Escape")?;
    h.expect_state(
        &third,
        "native Escape clears selection rather than a UI-only tray reset",
        |s| selected(s).is_empty(),
    )?;
    h.expect_ui(
        "[data-role=\"yazi.selection-tray\"]",
        "empty authoritative selection removes filename tray",
        |v| nodes(v).is_empty(),
    )?;
    h.click_file(&third, "image.svg")?;
    let image_before = h.snapshot(&third);

    h.case = "wide and narrow native themes preserve image aspect and pane bounds";
    for id in ["current", "aurora", "editorial", "amber"] {
        theme_switch(h, id)?;
        for (width, height, narrow) in [("1280", "900", false), ("680", "700", true)] {
            h.ctl("resize", &[width, height])?;
            if narrow {
                theme_narrow_layout(h, id)?;
            } else {
                theme_wide_layout(h, id, scale)?;
            }
            theme_preview_controls(h, id)?;
            theme_identity(h, &third, &image_before)?;
            h.wait(
                "actual SVG retains square aspect inside independent preview bounds",
                |h| {
                    let card = metadata_rect(&h.tree("[data-role=\"yazi.column.preview\"]")?)?;
                    let image = metadata_rect(
                        &h.tree("[data-role=\"yazi.image\"] svg[role=\"img\"] rect")?,
                    )?;
                    if image[2] >= 24.0
                        && (image[2] - image[3]).abs() < 1.0
                        && metadata_contains(card, image)
                    {
                        Ok(Some(()))
                    } else {
                        Err(format!(
                            "{id} image {image:?} outside/aspect-changed from card {card:?}"
                        ))
                    }
                },
            )?;
            h.shot(&format!(
                "13-native-theme-{id}-{}",
                if narrow { "narrow" } else { "image-wide" }
            ))?;
        }
    }
    h.ctl("resize", &["1280", "900"])?;
    theme_switch(h, "current")?;
    for client in [&third, &second, &first] {
        h.focus(client.pane)?;
        let watched = descendants(client.supervisor);
        h.owned.extend(watched.iter().copied());
        h.key("q")?;
        h.removed(client, &watched)?;
    }
    Ok(())
}

fn native_large_directory(h: &mut Harness) -> TestResult<()> {
    h.case = "5000 real Yazi entries preserve complete order beyond the dynamic snapshot cap";
    fs::create_dir_all(h.path("large-directory")).map_err(|e| e.to_string())?;
    let ordered: Vec<String> = (0..5000)
        .map(|index| format!("entry-{index:05}-native-global-navigation-fixture.txt"))
        .collect();
    for name in &ordered {
        h.write_fixture(&format!("large-directory/{name}"), b"REAL_LARGE_DIRECTORY\n")?;
    }
    let client = h.launch(h.path("large-directory"), true)?;
    let initial = h.expect_state(&client, "all 5000 names arrive in exact native alphabetical order", |s| {
        names(s, "files") == ordered && s["file_count"].as_u64() == Some(5000)
            && hover(s) == text(&client.cwd.join(&ordered[0]))
    })?;
    let listing_bytes = fs::metadata(h.inbox().join(format!("listing-{}.json", client.cid)))
        .map_err(|e| e.to_string())?.len();
    let state_bytes = fs::metadata(h.state_path(&client)).map_err(|e| e.to_string())?.len();
    if !(256 * 1024..16 * 1024 * 1024).contains(&listing_bytes) || state_bytes > 256 * 1024 {
        return Err(format!("Real large listing must cross the dynamic cap: listing={listing_bytes}, state={state_bytes}"));
    }
    let frozen_url = text(&client.cwd.join(&ordered[0]));
    h.key("Space")?;
    h.expect_state(&client, "Space selects the original global URL and advances", |s| {
        selected(s) == BTreeSet::from([frozen_url.clone()])
            && hover(s) == text(&client.cwd.join(&ordered[1]))
    })?;
    let mut actor_ms = Vec::new();
    let mut ui_ms = Vec::new();
    let mut index = 1usize;
    for key in ["j", "Down", "k", "Up"].into_iter().cycle().take(32) {
        index = if matches!(key, "j" | "Down") { index + 1 } else { index - 1 };
        let started = Instant::now();
        h.key(key)?;
        h.expect_state(&client, "real input reaches the matching actor cursor with full order intact", |s| {
            hover(s) == text(&client.cwd.join(&ordered[index])) && names(s, "files") == ordered
                && selected(s) == BTreeSet::from([frozen_url.clone()])
        })?;
        actor_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        h.expect_metadata_selection(&format!("{ROWS}.sel"), &client.cwd.join(&ordered[index]))?;
        h.expect_ui("[data-role=\"yazi.column.current\"] .sf-list.virtual", "distinct navigation frames retain the mounted middle native list", |v| {
            nodes(v).len() == 1 && theme_visible(nodes(v)[0])
        })?;
        ui_ms.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    h.case = "G reveals the global final item in a bounded virtual pool";
    h.key("G")?;
    h.expect_state(&client, "G reaches global index 4999 without truncation or selection loss", |s| {
        hover(s) == text(&client.cwd.join(&ordered[4999])) && s["cursor"].as_u64() == Some(5000)
            && names(s, "files") == ordered && selected(s) == BTreeSet::from([frozen_url.clone()])
    })?;
    h.expect_metadata_selection(&row_selector(ROWS, &ordered[4999]), &client.cwd.join(&ordered[4999]))?;
    let (mounted, capacity) = h.wait("global selected item is revealed inside a bounded native virtual pool", |h| {
        let scroller = metadata_rect(&h.tree("[data-role=\"yazi.column.current\"] .sf-list-scroll.max")?)?;
        let row = metadata_rect(&h.tree(&row_selector(ROWS, &ordered[4999]))?)?;
        let pool = h.tree("[data-role=\"yazi.column.current\"] .sf-item.sf-vrow[data-item]")?;
        let capacity = (scroller[3] / row[3]).ceil() as usize + 14;
        let mounted = nodes(&pool).len();
        let correct = nodes(&pool).iter().all(|node| {
            node["title"].as_str().is_some_and(|url| {
                ordered.iter().any(|name| url == text(&client.cwd.join(name)))
            })
        });
        if metadata_contains(scroller, row) && mounted > 0 && mounted <= capacity && correct {
            Ok(Some((mounted, capacity)))
        } else {
            Err(format!("Global reveal/pool mismatch: selected={row:?}, scroller={scroller:?}, mounted={mounted}, capacity={capacity}, pool={pool}"))
        }
    })?;
    for _ in 0..4 {
        h.open_path_editor(&text(&client.cwd))?;
        h.expect_ui("[data-role=\"yazi.column.current\"] .sf-list.virtual", "path-hover overlay render does not erase the middle column", |v| {
            nodes(v).len() == 1 && theme_visible(nodes(v)[0])
        })?;
        h.expect_metadata_selection(&format!("{ROWS}.sel"), &client.cwd.join(&ordered[4999]))?;
        h.click_target("[data-role=\"yazi.path-cancel\"]")?;
        h.expect_metadata_selection(&format!("{ROWS}.sel"), &client.cwd.join(&ordered[4999]))?;
    }
    h.click_file(&client, &ordered[4998])?;
    h.expect_state(&client, "pointer selects pooled row using its global item identity, not pool index", |s| {
        s["cursor"].as_u64() == Some(4999) && selected(s) == BTreeSet::from([frozen_url.clone()])
    })?;
    h.key("v")?;
    h.key("k")?;
    let range = BTreeSet::from([text(&client.cwd.join(&ordered[4997])), text(&client.cwd.join(&ordered[4998]))]);
    h.expect_state(&client, "high-index visual range marks authoritative global URLs", |s| {
        s["mode"] == "select" && names(s, "marked_urls").into_iter().collect::<BTreeSet<_>>() == range
    })?;
    h.key("Escape")?;
    h.key("ctrl+a")?;
    let all_urls: BTreeSet<String> = ordered.iter().map(|name| text(&client.cwd.join(name))).collect();
    h.expect_state(&client, "native toggle-all keeps every one of the 5000 global URLs", |s| selected(s) == all_urls)?;
    h.case = "large listing/filter pairs remain coherent while selection URLs stay global";
    h.key("f")?;
    h.paste(client.pane, "entry-04999")?;
    let filtered = h.expect_state(&client, "filtered coherent pair contains only the exact high-index filename", |s| {
        names(s, "files") == vec![ordered[4999].clone()] && selected(s) == all_urls
            && hover(s) == text(&client.cwd.join(&ordered[4999]))
    })?;
    h.expect_metadata_selection(&row_selector(ROWS, &ordered[4999]), &client.cwd.join(&ordered[4999]))?;
    h.click_target("[data-role=\"yazi.command-cancel\"]")?;
    let restored = h.expect_state(&client, "clearing the actual filter restores every entry in original order", |s| {
        names(s, "files") == ordered && selected(s) == all_urls && s["filter"] == ""
    })?;
    if initial["listing_epoch"] != filtered["listing_epoch"]
        || filtered["listing_epoch"] != restored["listing_epoch"]
        || filtered["listing_revision"] == restored["listing_revision"] {
        return Err(format!("Real filter listing revisions did not advance coherently: initial={initial}, filtered={filtered}, restored={restored}"));
    }
    actor_ms.sort_by(f64::total_cmp);
    ui_ms.sort_by(f64::total_cmp);
    let distribution = |samples: &[f64]| json!({
        "samples": samples.len(), "median_ms": samples[samples.len() / 2],
        "p95_ms": samples[(samples.len() * 95 + 99) / 100 - 1],
        "max_ms": samples[samples.len() - 1],
    });
    println!("Actual AFTER large-directory input latency (includes ctl process and 40ms polling): {}", json!({
        "entries": 5000, "listing_bytes": listing_bytes, "dynamic_bytes": state_bytes,
        "mounted": mounted, "pool_capacity": capacity,
        "input_to_actor": distribution(&actor_ms), "input_to_visible_ui": distribution(&ui_ms),
    }));
    h.shot("14-native-large-directory-global-selection")?;
    h.key("Escape")?;
    h.expect_state(&client, "native Escape clears all large-directory selection", |s| selected(s).is_empty())?;
    let watched = descendants(client.supervisor);
    h.owned.extend(watched.iter().copied());
    h.key("q")?;
    h.removed(&client, &watched)
}

#[test]
fn real_native_interactions() {
    let mut harness = Harness::new()
        .unwrap_or_else(|error| panic!("Real interaction prerequisites/setup failed: {error}"));
    match interaction_flow(&mut harness)
        .and_then(|()| glyph_interaction_flow(&mut harness))
        .and_then(|()| native_metadata_footer_headers(&mut harness))
        .and_then(|()| native_slot_flow(&mut harness))
        .and_then(|()| native_layout_themes(&mut harness))
        .and_then(|()| native_large_directory(&mut harness))
    {
        Ok(()) => {
            harness.success = true;
            println!("All real native interaction cases passed (owned GUI, isolated daemon).");
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
