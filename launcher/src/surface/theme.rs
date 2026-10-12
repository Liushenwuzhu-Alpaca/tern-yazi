//! Layout themes: the registry with its structural flags, the embedded stylesheets, and the
//! persisted choice (`$XDG_CONFIG_HOME/tern-yazi/settings.json`; `YAZI_THEME` overrides it).

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

pub(crate) struct Theme {
    pub(crate) id: &'static str,
    pub(crate) title: &'static str,
    pub(crate) detail: &'static str,
    /// The role gains `.narrow` below 120 columns.
    pub(crate) narrow: bool,
    /// The parent column drops icons.
    pub(crate) plain_parent: bool,
    /// Rows lead with their listing ordinal.
    pub(crate) ordinals: bool,
    /// The dock carries the selection tray.
    pub(crate) tray: bool,
}

const fn theme(id: &'static str, title: &'static str, detail: &'static str) -> Theme {
    Theme {
        id,
        title,
        detail,
        narrow: true,
        plain_parent: false,
        ordinals: false,
        tray: false,
    }
}

pub(crate) const THEMES: [Theme; 8] = [
    Theme {
        narrow: false,
        ..theme("current", "Current", "Yazi capsules, compact 4:9:7 grid")
    },
    theme("aurora", "Aurora Glass", "Frosted rounded panels"),
    Theme {
        plain_parent: true,
        ..theme("editorial", "Editorial Paper", "Serif headings, hairline rules")
    },
    Theme {
        ordinals: true,
        tray: true,
        ..theme("amber", "Amber Ledger", "Mono ledger, ordinals, selection tray")
    },
    theme("blueprint", "Blueprint Draft", "Drafting grid, dashed cursor, title-block footer"),
    theme("brutalist", "Brutalist Block", "Thick borders, hard shadows, inverted cursor"),
    Theme {
        plain_parent: true,
        ..theme("zen", "Zen Minimal", "No chrome, whitespace and an accent bar")
    },
    Theme {
        ordinals: true,
        ..theme("swiss", "Swiss Grid", "Bold flush-left type, red ordinals")
    },
];

/// Every sheet the surface loads once; each theme's rules are scoped to its own role.
pub(crate) const SHEETS: [(&str, &str); 9] = [
    ("companion", include_str!("../../../companion.css")),
    ("theme-current", include_str!("../../../themes/current.css")),
    ("theme-aurora", include_str!("../../../themes/aurora.css")),
    ("theme-editorial", include_str!("../../../themes/editorial.css")),
    ("theme-amber", include_str!("../../../themes/amber.css")),
    ("theme-blueprint", include_str!("../../../themes/blueprint.css")),
    ("theme-brutalist", include_str!("../../../themes/brutalist.css")),
    ("theme-zen", include_str!("../../../themes/zen.css")),
    ("theme-swiss", include_str!("../../../themes/swiss.css")),
];

pub(crate) fn index(id: &str) -> Option<usize> {
    THEMES.iter().position(|theme| theme.id == id)
}

/// The theme wrapper's role, e.g. `yazi.theme.amber.narrow`.
pub(crate) fn role(theme: &Theme, cols: u32) -> String {
    if theme.narrow && cols < 120 {
        format!("yazi.theme.{}.narrow", theme.id)
    } else {
        format!("yazi.theme.{}", theme.id)
    }
}

fn settings_path() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").filter(|v| !v.is_empty()).map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("tern-yazi").join("settings.json"))
}

fn saved(bytes: &[u8]) -> Option<usize> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    index(value.get("theme")?.as_str()?)
}

/// A valid `YAZI_THEME` wins over the saved choice; anything unknown falls back to Current.
fn choose(env: Option<&str>, settings: Option<&[u8]>) -> usize {
    env.and_then(index).or_else(|| settings.and_then(saved)).unwrap_or(0)
}

/// The starting theme from the environment and the settings file.
pub(crate) fn initial() -> usize {
    let settings = settings_path().and_then(|path| fs::read(path).ok());
    choose(env::var("YAZI_THEME").ok().as_deref(), settings.as_deref())
}

/// Persists the choice atomically, keeping any other settings in the file.
pub(crate) fn save(at: usize) -> io::Result<()> {
    let path = settings_path().ok_or_else(|| io::Error::other("no XDG_CONFIG_HOME or HOME"))?;
    let dir = path.parent().ok_or_else(|| io::Error::other("invalid settings path"))?;
    fs::create_dir_all(dir)?;
    let mut settings = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    settings["theme"] = THEMES[at].id.into();
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp)?;
    serde_json::to_writer_pretty(&mut file, &settings).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_flags_and_roles() {
        assert_eq!(THEMES.len(), SHEETS.len() - 1);
        for (theme, (name, _)) in THEMES.iter().zip(&SHEETS[1..]) {
            assert_eq!(*name, format!("theme-{}", theme.id));
        }
        let amber = &THEMES[index("amber").unwrap()];
        assert!(amber.ordinals && amber.tray && amber.narrow && !amber.plain_parent);
        assert_eq!(role(amber, 100), "yazi.theme.amber.narrow");
        assert_eq!(role(amber, 160), "yazi.theme.amber");
        assert_eq!(role(&THEMES[0], 80), "yazi.theme.current");
    }

    #[test]
    fn saved_choice_needs_a_known_theme() {
        assert_eq!(saved(br#"{"theme":"zen"}"#), index("zen"));
        assert_eq!(saved(br#"{"theme":"nope"}"#), None);
        assert_eq!(saved(b"not json"), None);
    }

    #[test]
    fn environment_overrides_the_saved_choice() {
        let zen = br#"{"theme":"zen"}"#.as_slice();
        assert_eq!(choose(Some("swiss"), Some(zen)), index("swiss").unwrap());
        assert_eq!(choose(Some("unknown"), Some(zen)), index("zen").unwrap());
        assert_eq!(choose(None, Some(zen)), index("zen").unwrap());
        assert_eq!(choose(None, None), 0);
    }
}
