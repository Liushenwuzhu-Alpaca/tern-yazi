//! The Yazi snapshot: the dynamic state record joined with the listing generation it names, and
//! the pure classification and lookup helpers the view and input reducer share.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

/// A filename's glyph from Yazi; `None` is Yazi's explicit "no icon".
pub(crate) type Icons = HashMap<String, Option<String>>;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Folder {
    pub(crate) cwd: String,
    pub(crate) files: Vec<String>,
    pub(crate) dirs: HashMap<String, bool>,
    pub(crate) icons: Icons,
    pub(crate) overrides: Icons,
    pub(crate) count: Option<u64>,
    /// The subject of the Yazi search this folder lists, if it is a search.
    pub(crate) search: Option<String>,
}

impl Folder {
    /// The glyph Yazi chose for `name`: a live override wins over the listing's icon.
    pub(crate) fn icon(&self, name: &str) -> Option<&str> {
        self.overrides
            .get(name)
            .or_else(|| self.icons.get(name))
            .and_then(|icon| icon.as_deref())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Hover {
    pub(crate) url: String,
    pub(crate) dir: Option<bool>,
    pub(crate) size: Option<u64>,
    pub(crate) mtime: Option<f64>,
    pub(crate) permissions: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Tab {
    pub(crate) name: String,
    pub(crate) active: bool,
}

/// One complete, consistent Yazi generation.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) seq: u64,
    pub(crate) current: Folder,
    pub(crate) parent: Option<Folder>,
    pub(crate) preview: Option<Folder>,
    pub(crate) preview_skip: usize,
    pub(crate) tabs: Vec<Tab>,
    /// `Some(None)`: Yazi reports no active filter; `None`: the record does not say.
    pub(crate) filter: Option<Option<String>>,
    pub(crate) selected_urls: Vec<String>,
    pub(crate) marked_urls: HashSet<String>,
    pub(crate) mode: String,
    pub(crate) hovered: Option<Hover>,
}

/// The dynamic record (`state-CID.json`) before it is joined with its listing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StateRecord {
    pub(crate) epoch: String,
    pub(crate) revision: u64,
    root: Map<String, Value>,
}

/// The listing generation (`listing-CID.json`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Listing {
    pub(crate) epoch: String,
    pub(crate) revision: u64,
    current: Folder,
    parent: Option<Folder>,
    preview: Option<Folder>,
    selected_urls: Vec<String>,
    marked_urls: HashSet<String>,
}

fn object(bytes: &[u8]) -> Result<Map<String, Value>, String> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err("Expected a snapshot object".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

fn generation(map: &Map<String, Value>, epoch: &str, revision: &str) -> Option<(String, u64)> {
    let epoch = map.get(epoch)?.as_str()?.to_owned();
    let revision = map.get(revision)?.as_f64()?;
    (revision >= 0.0).then_some((epoch, revision as u64))
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

fn icons(value: Option<&Value>) -> Icons {
    let Some(Value::Object(map)) = value else {
        return Icons::new();
    };
    map.iter()
        .map(|(name, icon)| (name.clone(), icon.as_str().map(str::to_owned)))
        .collect()
}

fn folder(map: &Map<String, Value>) -> Option<Folder> {
    let cwd = map.get("cwd")?.as_str()?.to_owned();
    let dirs = match map.get("file_dirs") {
        Some(Value::Object(dirs)) => dirs
            .iter()
            .filter_map(|(name, dir)| dir.as_bool().map(|dir| (name.clone(), dir)))
            .collect(),
        _ => HashMap::new(),
    };
    Some(Folder {
        cwd,
        files: strings(map.get("files")),
        dirs,
        icons: icons(map.get("file_icons")),
        overrides: Icons::new(),
        count: map.get("file_count").and_then(Value::as_u64),
        search: map.get("search").and_then(Value::as_str).map(str::to_owned),
    })
}

impl StateRecord {
    /// Parses the dynamic record and checks it belongs to client `cid`.
    pub(crate) fn parse(bytes: &[u8], cid: &str) -> Result<Self, String> {
        let root = object(bytes)?;
        let client = match root.get("client_id") {
            Some(Value::String(id)) => id.clone(),
            Some(Value::Number(id)) => id.to_string(),
            _ => String::new(),
        };
        match generation(&root, "listing_epoch", "listing_revision") {
            Some((epoch, revision)) if client == cid => Ok(Self {
                epoch,
                revision,
                root,
            }),
            _ => Err("Yazi snapshot identity or listing generation is invalid".to_owned()),
        }
    }

    /// Joins the record with its listing; `None` until both name the same generation.
    pub(crate) fn join(&self, listing: &Listing) -> Option<Snapshot> {
        if listing.epoch != self.epoch || listing.revision != self.revision {
            return None;
        }
        let root = &self.root;
        let with_overrides = |folder: &Option<Folder>, key: &str| {
            folder.clone().map(|mut folder| {
                folder.overrides = icons(root.get(key));
                folder
            })
        };
        let mut current = listing.current.clone();
        current.overrides = icons(root.get("icon_overrides"));
        let hovered = match root.get("hovered") {
            Some(Value::Object(hover)) => hover.get("url").and_then(Value::as_str).map(|url| Hover {
                url: url.to_owned(),
                dir: hover.get("dir").and_then(Value::as_bool),
                size: hover.get("size").and_then(Value::as_u64),
                mtime: hover.get("mtime").and_then(Value::as_f64),
                permissions: hover
                    .get("permissions")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }),
            _ => None,
        };
        let tabs = root
            .get("tabs")
            .and_then(|tabs| tabs.get("items"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|tab| Tab {
                        name: tab.get("name").and_then(Value::as_str).unwrap_or("").to_owned(),
                        active: tab.get("active").and_then(Value::as_bool) == Some(true),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Snapshot {
            seq: root.get("seq").and_then(Value::as_u64).unwrap_or(0),
            current,
            parent: with_overrides(&listing.parent, "parent_icon_overrides"),
            preview: with_overrides(&listing.preview, "preview_icon_overrides"),
            preview_skip: root.get("preview_skip").and_then(Value::as_u64).unwrap_or(0) as usize,
            tabs,
            filter: match root.get("filter") {
                Some(Value::String(query)) => Some(Some(query.clone())),
                Some(Value::Bool(false)) => Some(None),
                _ => None,
            },
            selected_urls: listing.selected_urls.clone(),
            marked_urls: listing.marked_urls.clone(),
            mode: root.get("mode").and_then(Value::as_str).unwrap_or("normal").to_owned(),
            hovered,
        })
    }
}

impl Listing {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, String> {
        let root = object(bytes)?;
        let (epoch, revision) = generation(&root, "epoch", "revision")
            .ok_or_else(|| "Yazi listing generation is invalid".to_owned())?;
        let current = folder(&root).ok_or_else(|| "Yazi listing has no directory".to_owned())?;
        let nested = |key: &str| match root.get(key) {
            Some(Value::Object(map)) => folder(map),
            _ => None,
        };
        Ok(Self {
            epoch,
            revision,
            parent: nested("parent"),
            preview: nested("preview"),
            current,
            selected_urls: strings(root.get("selected_urls")),
            marked_urls: strings(root.get("marked_urls")).into_iter().collect(),
        })
    }
}

impl Snapshot {
    /// The folder this snapshot lists for `cwd`. Types are never inferred from the filesystem.
    pub(crate) fn directory(&self, cwd: &str) -> Option<&Folder> {
        [Some(&self.current), self.parent.as_ref(), self.preview.as_ref()]
            .into_iter()
            .flatten()
            .find(|folder| folder.cwd == cwd)
    }

    pub(crate) fn is_dir(&self, cwd: &str, name: &str) -> Option<bool> {
        if let Some(dir) = self.directory(cwd).and_then(|folder| folder.dirs.get(name)) {
            return Some(*dir);
        }
        self.hovered
            .as_ref()
            .filter(|hover| hover.url == join_path(cwd, name))
            .and_then(|hover| hover.dir)
    }
}

pub(crate) fn join_path(base: &str, name: &str) -> String {
    if base.is_empty() || name.is_empty() {
        return format!("{base}{name}");
    }
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

pub(crate) fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return path;
    }
    match trimmed.rfind('/') {
        Some(at) => &trimmed[at + 1..],
        None => trimmed,
    }
}

/// `path` as named in the folder `cwd`: a search lists nested paths, a directory bare names.
pub(crate) fn relative<'a>(cwd: &str, path: &'a str) -> &'a str {
    path.strip_prefix(cwd)
        .and_then(|rest| if cwd.ends_with('/') { Some(rest) } else { rest.strip_prefix('/') })
        .filter(|rest| !rest.is_empty())
        .unwrap_or_else(|| basename(path))
}

pub(crate) fn parent_dir(cwd: &str) -> Option<&str> {
    if cwd == "/" {
        return None;
    }
    match cwd.rfind('/') {
        Some(0) | None => Some("/"),
        Some(at) => Some(&cwd[..at]),
    }
}

/// The lowercase extension after the last dot of the last path component.
pub(crate) fn file_ext(path: &str) -> String {
    let name = basename(path);
    match name.rfind('.') {
        Some(at) if at + 1 < name.len() => name[at + 1..].to_lowercase(),
        _ => String::new(),
    }
}

pub(crate) fn language(ext: &str) -> &'static str {
    match ext {
        "lua" | "luau" => "lua",
        "py" => "python",
        "rs" => "rust",
        "js" => "javascript",
        "ts" => "typescript",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "sh" | "bash" | "zsh" => "bash",
        "c" | "h" => "c",
        "cpp" => "cpp",
        "html" => "html",
        "css" => "css",
        "xml" => "xml",
        "sql" => "sql",
        "mermaid" | "mmd" => "mermaid",
        _ => "text",
    }
}

pub(crate) fn image_mime(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// Text 3D formats Tern's Markdown renders as live model figures, keyed to their fence tag.
/// XML X3D and binary formats (glb, fbx, 3ds, usdc/usdz) have no fence form.
pub(crate) fn model_fence(ext: &str) -> Option<&'static str> {
    match ext {
        "wrl" | "vrml" => Some("vrml"),
        "x3dv" => Some("x3dv"),
        "obj" => Some("obj"),
        "stl" => Some("stl"),
        "ply" => Some("ply"),
        "gltf" => Some("gltf"),
        "usda" => Some("usda"),
        _ => None,
    }
}

pub(crate) fn is_archive(ext: &str) -> bool {
    matches!(ext, "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state(revision: u64) -> StateRecord {
        let bytes = json!({
            "client_id": 12345, "seq": 4, "listing_epoch": "e1", "listing_revision": revision,
            "icon_overrides": {"a.txt": "X"}, "parent_icon_overrides": {"files": false},
            "filter": false, "mode": "select",
            "hovered": {"url": "/tmp/files/a.txt", "dir": false, "size": 12, "permissions": "-rw-r--r--"},
            "tabs": {"idx": 1, "total": 2, "items": [{"name": "files", "active": true}, {"name": "tmp"}]},
        });
        StateRecord::parse(bytes.to_string().as_bytes(), "12345").unwrap()
    }

    fn listing() -> Listing {
        let bytes = json!({
            "epoch": "e1", "revision": 2, "cwd": "/tmp/files", "files": ["a.txt", "child"],
            "file_dirs": {"a.txt": false, "child": true}, "file_icons": {"a.txt": "T", "child": false},
            "parent": {"cwd": "/tmp", "files": ["files"], "file_dirs": {"files": true}, "file_icons": {"files": "D"}},
            "selected_urls": ["/tmp/files/a.txt"], "marked_urls": [],
        });
        Listing::parse(bytes.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn joins_only_matching_generations_and_layers_icon_overrides() {
        assert!(state(1).join(&listing()).is_none());
        let snapshot = state(2).join(&listing()).unwrap();
        assert_eq!(snapshot.seq, 4);
        assert_eq!(snapshot.current.icon("a.txt"), Some("X"));
        assert_eq!(snapshot.current.icon("child"), None);
        assert_eq!(snapshot.parent.as_ref().unwrap().icon("files"), None);
        assert_eq!(snapshot.filter, Some(None));
        assert_eq!(snapshot.mode, "select");
        assert_eq!(snapshot.tabs.len(), 2);
        assert!(snapshot.tabs[0].active && !snapshot.tabs[1].active);
        assert_eq!(snapshot.hovered.as_ref().unwrap().size, Some(12));
        assert_eq!(snapshot.selected_urls, ["/tmp/files/a.txt"]);
    }

    #[test]
    fn rejects_foreign_clients_and_missing_generations() {
        let foreign = json!({"client_id": "999", "listing_epoch": "e", "listing_revision": 1});
        assert!(StateRecord::parse(foreign.to_string().as_bytes(), "12345").is_err());
        let missing = json!({"client_id": "12345", "listing_epoch": "e"});
        assert!(StateRecord::parse(missing.to_string().as_bytes(), "12345").is_err());
        assert!(StateRecord::parse(b"[1]", "12345").is_err());
    }

    #[test]
    fn directory_lookup_never_guesses_types() {
        let snapshot = state(2).join(&listing()).unwrap();
        assert_eq!(snapshot.is_dir("/tmp/files", "child"), Some(true));
        assert_eq!(snapshot.is_dir("/tmp", "files"), Some(true));
        assert_eq!(snapshot.is_dir("/elsewhere", "x"), None);
        assert!(snapshot.directory("/tmp").is_some());
    }

    #[test]
    fn classifies_paths() {
        assert_eq!(file_ext("/a/b.TAR.GZ"), "gz");
        assert_eq!(file_ext("/a/.hidden"), "hidden");
        assert_eq!(file_ext("/a/plain"), "");
        assert_eq!(language("rs"), "rust");
        assert_eq!(language("zzz"), "text");
        assert_eq!(image_mime("svg"), Some("image/svg+xml"));
        assert_eq!(model_fence("wrl"), Some("vrml"));
        assert!(is_archive("zip") && !is_archive("txt"));
        assert_eq!(parent_dir("/tmp/files"), Some("/tmp"));
        assert_eq!(parent_dir("/tmp"), Some("/"));
        assert_eq!(parent_dir("/"), None);
        assert_eq!(join_path("/", "tmp"), "/tmp");
        assert_eq!(basename("/tmp/files"), "files");
    }
}
