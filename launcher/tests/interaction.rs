//! Real GUI interaction tests. No state or backend response is synthesized.
//! Run through tests/smoke_luau.sh on Wayland/X11; prerequisites fail rather than skip.
//! Tern 0.7 serve is standalone, so these tests explicitly use a daemon-backed window.

use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::env;
use std::fs::{self, DirBuilder, File};
use std::io::Read;
use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(40);
const ROWS: &str = "[data-role=\"yazi.column.current\"] .sf-item";
const PREVIEW: &str = "[data-role=\"yazi.preview\"]";

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

#[derive(Clone)]
struct Client {
    cid: String,
    token: String,
    owner: u64,
    pane: u64,
    supervisor: Process,
    backend: Process,
    cwd: PathBuf,
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
        read_json(&self.state_path(client)).unwrap_or(Value::Null)
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
            "shots",
            "files/child",
            "files/copydest",
            "files/cutdest",
            "other",
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
        self.write_fixture("bashrc", "PROMPT_COMMAND='status=$?; printf \"\\033]133;D;%s\\007\" \"$status\"'\nPS1='\\[\\e]133;A\\a\\]\\w\\$ \\[\\e]133;B\\a\\]'\ntrap 'printf \"\\033]133;C\\007\"' DEBUG\n")?;
        self.executable(
            "bin/shell",
            &format!(
                "#!/bin/sh\nexec /bin/bash --noprofile --rcfile {} -i \"$@\"\n",
                quote(text(&self.path("bashrc")))
            ),
        )?;
        self.executable("bin/yazi", &format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > {}/supervisor-$$.pid\nexec {} --real /usr/bin/yazi -- \"$@\"\n", quote(text(&self.root)), quote(env!("CARGO_BIN_EXE_tern-yazi-launch"))))?;
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
        let before = self.pid_markers();
        let line = format!(
            "cd {} && YAZI_CONFIG_HOME={} {} {}",
            quote(text(&cwd)),
            quote(text(&self.path("yazi"))),
            quote(text(&self.path("bin/yazi"))),
            quote(text(&cwd))
        );
        self.cli(&["run", &owner.to_string(), &line])?;
        let supervisor = self.wait("actual managed supervisor process", |h| {
            let pid = h
                .pid_markers()
                .difference(&before)
                .copied()
                .find(|pid| process(*pid).is_some());
            Ok(pid.and_then(process).map(|(p, _, _)| p))
        })?;
        self.owned.insert(supervisor);
        let client = self.wait(
            "cold native block with real snapshot and live lease (no artificial wake)",
            |h| {
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
                    let snapshot = read_json(&h.inbox().join(format!("state-{cid}.json")))
                        .unwrap_or(Value::Null);
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
        self.expect_ui(ROWS, "actual native file rows", |value| {
            !nodes(value).is_empty()
        })?;
        Ok(client)
    }

    fn click_node(&mut self, selector: &str, label: &str, double: bool) -> TestResult<()> {
        let node = self.wait(&format!("visible clickable {label}"), |h| {
            let value = h.tree(selector)?;
            Ok(nodes(&value)
                .into_iter()
                .find(|node| {
                    node["text"]
                        .as_str()
                        .is_some_and(|s| s == label || s.ends_with(&format!(" {label}")))
                        || node["title"]
                            .as_str()
                            .is_some_and(|s| s.ends_with(&format!("/{label}")))
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
        // Observe files only: no ctl, screenshot, event or render wake during this interval.
        let begin = Instant::now();
        let initial = read_json(&self.inbox().join(format!("managed-{}.json", client.cid)))
            .ok_or("Missing initial health")?["seq"]
            .as_u64()
            .unwrap_or(0);
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
            {
                return Err("Idle native lease lost its pinned client/pane identity".into());
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
    h.expect_ui(".sf", "selection appears only in the footer", |v| {
        let text = ui_text(v);
        !text.contains("Synced") && !text.contains(" selected") && text.contains("0 sel")
    })?;
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
        "[data-role=\"yazi.input-row\"]",
        "command prompt occupies the reserved row",
        |v| ui_text(v).contains("Shell [:]"),
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

#[test]
fn real_native_interactions() {
    let mut harness = Harness::new()
        .unwrap_or_else(|error| panic!("Real interaction prerequisites/setup failed: {error}"));
    match interaction_flow(&mut harness) {
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
