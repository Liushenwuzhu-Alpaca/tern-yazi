//! Presentation-only labels: sizes, permissions, semantic tones and breadcrumb segments.

use super::model::{file_ext, is_archive, join_path};

pub(crate) fn size(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else {
        return "-".to_owned();
    };
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1 << 20 {
        format!("{:.1} KB", value / 1024.0)
    } else if bytes < 1 << 30 {
        format!("{:.1} MB", value / 1_048_576.0)
    } else {
        format!("{:.1} GB", value / 1_073_741_824.0)
    }
}

/// Yazi's exact permission text (uppercase S/T encode missing execute bits).
pub(crate) fn permissions(text: Option<&str>) -> &str {
    text.unwrap_or("-")
}

/// Modification time in local time as Yazi's status shows it, e.g. `2026-10-12 13:04`.
pub(crate) fn mtime(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds.filter(|seconds| seconds.is_finite()) else {
        return "-".to_owned();
    };
    let time = seconds.floor() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return "-".to_owned();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

/// Yazi's status coloring: type green, r yellow, w red, x/s/S/t/T cyan, -/? gray.
pub(crate) fn permission_tone(c: char) -> &'static str {
    match c {
        '-' | '?' => "muted",
        'r' => "warning",
        'w' => "error",
        'x' | 's' | 'S' | 't' | 'T' => "info",
        _ => "success",
    }
}

/// The semantic color of a filename; Yazi owns the glyph itself.
pub(crate) fn file_tone(name: &str, is_dir: Option<bool>) -> &'static str {
    match is_dir {
        None => return "muted",
        Some(true) => return if name.starts_with('.') { "muted" } else { "accent" },
        Some(false) => {}
    }
    let ext = file_ext(name);
    if is_archive(&ext) {
        return "warn";
    }
    match ext.as_str() {
        "lua" | "luau" | "c" | "cpp" | "h" | "hpp" | "go" | "md" | "markdown" | "png" | "jpg"
        | "jpeg" | "svg" | "gif" | "webp" | "mp4" | "mkv" | "webm" | "mp3" | "wav" => "accent",
        "py" | "js" | "ts" | "jsx" | "tsx" => "warn",
        "rs" | "pdf" => "error",
        "sh" | "bash" | "zsh" => "success",
        _ => "muted",
    }
}

/// The row glyph span: Yazi's icon plus a space, or nothing when Yazi chose none.
pub(crate) fn file_icon(icon: Option<&str>, name: &str, is_dir: Option<bool>) -> (String, &'static str) {
    let tone = file_tone(name, is_dir);
    match icon {
        Some(icon) if !icon.is_empty() => (format!("{icon} "), tone),
        _ => (String::new(), tone),
    }
}

/// Archive members have no live Yazi file, so they get generic glyphs only.
pub(crate) fn virtual_icon(name: &str, is_dir: bool) -> (&'static str, &'static str) {
    let glyph = if is_dir { " " } else { " " };
    (glyph, file_tone(name, Some(is_dir)))
}

pub(crate) struct Segment {
    pub(crate) label: String,
    pub(crate) path: String,
}

/// Breadcrumbs for `cwd`, rooted at `~` inside `home`.
pub(crate) fn path_segments(cwd: &str, home: &str) -> Vec<Segment> {
    let at_home = !home.is_empty()
        && (cwd == home || cwd.strip_prefix(home).is_some_and(|rest| rest.starts_with('/')));
    let root = if at_home { home } else { "/" };
    let mut segments = vec![Segment {
        label: if at_home { "~" } else { "/" }.to_owned(),
        path: root.to_owned(),
    }];
    let tail = if at_home { &cwd[home.len()..] } else { cwd };
    let mut path = root.to_owned();
    for name in tail.split('/').filter(|name| !name.is_empty()) {
        path = join_path(&path, name);
        segments.push(Segment {
            label: name.to_owned(),
            path: path.clone(),
        });
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_yazi_units() {
        assert_eq!(size(None), "-");
        assert_eq!(size(Some(12)), "12 B");
        assert_eq!(size(Some(1536)), "1.5 KB");
        assert_eq!(size(Some(3 << 20)), "3.0 MB");
    }

    #[test]
    fn tones_and_icons_follow_yazi_metadata_only() {
        assert_eq!(file_tone("x.rs", None), "muted");
        assert_eq!(file_tone(".git", Some(true)), "muted");
        assert_eq!(file_tone("src", Some(true)), "accent");
        assert_eq!(file_tone("a.tgz", Some(false)), "warn");
        assert_eq!(file_icon(Some("R"), "x.rs", Some(false)), ("R ".to_owned(), "error"));
        assert_eq!(file_icon(None, "x.rs", Some(false)).0, "");
        assert_eq!(virtual_icon("d/", true).0, " ");
    }

    #[test]
    fn breadcrumbs_root_at_home() {
        let labels = |cwd| {
            path_segments(cwd, "/home/u")
                .into_iter()
                .map(|s| (s.label, s.path))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels("/home/u/a"),
            [("~".into(), "/home/u".into()), ("a".into(), "/home/u/a".into())]
        );
        assert_eq!(labels("/home/user2"), [("/".into(), "/".into()), ("home".into(), "/home".into()), ("user2".into(), "/home/user2".into())]);
        assert_eq!(labels("/"), [("/".to_owned(), "/".to_owned())]);
    }
}
