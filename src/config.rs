//! Layered settings. Each layer overrides the one before:
//!
//! 1. built-in defaults
//! 2. the global file, `~/.config/jev-cc/config.toml`
//! 3. the project file, `.jev-cc.toml` at the repository root
//! 4. `JEV_CC_*` environment variables
//!
//! The project file comes with whatever repository you clone, so it can't set anything
//! that changes where your diffs and API key are sent (`base_url`), and its `exclude`
//! patterns add to the global ones rather than replacing them.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::jev::DEFAULT_BASE_URL;

pub const PROJECT_FILE: &str = ".jev-cc.toml";
const GLOBAL_FILE: &str = "config.toml";

/// `$XDG_CONFIG_HOME/jev-cc`, `~/.config/jev-cc`, or `%APPDATA%\jev-cc` on Windows.
pub fn dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))
    };
    Some(base?.join("jev-cc"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Default,
    Global,
    Project,
    Env(&'static str),
}

impl Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Default => write!(f, "default"),
            Source::Global => write!(f, "global"),
            Source::Project => write!(f, "project"),
            Source::Env(name) => write!(f, "{name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Setting<T> {
    pub value: T,
    pub source: Source,
}

#[derive(Debug)]
pub struct Config {
    pub disable: Setting<bool>,
    pub base_url: Setting<String>,
    pub timeout_ms: Setting<u64>,
    pub deadline_ms: Setting<u64>,
    pub min_confidence: Setting<f64>,
    pub breaking_threshold: Setting<f64>,
    /// Git glob pathspecs, relative to the repository root. Global and project patterns
    /// combined.
    pub exclude: Vec<Setting<String>>,
    pub global_path: Option<PathBuf>,
    pub project_path: Option<PathBuf>,
    /// Problems found while loading. Loading never fails; bad values are skipped.
    pub warnings: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct File {
    disable: Option<bool>,
    base_url: Option<String>,
    timeout_ms: Option<u64>,
    deadline_ms: Option<u64>,
    min_confidence: Option<f64>,
    breaking_threshold: Option<f64>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(flatten)]
    unknown: BTreeMap<String, IgnoredAny>,
}

/// Loads every layer. `project_root` is the repository's top-level directory, if any.
pub fn load(project_root: Option<&Path>) -> Config {
    let global_path = dir().map(|d| d.join(GLOBAL_FILE));
    let project_path = project_root.map(|r| r.join(PROJECT_FILE));
    let mut warnings = Vec::new();
    let global = global_path.as_deref().and_then(|p| read(p, &mut warnings));
    let project = project_path.as_deref().and_then(|p| read(p, &mut warnings));
    let mut config = resolve(global, project, &|name| std::env::var(name).ok(), warnings);
    config.global_path = global_path;
    config.project_path = project_path;
    config
}

/// A missing file is fine. A file that can't be read or parsed is skipped with a warning.
fn read(path: &Path, warnings: &mut Vec<String>) -> Option<File> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warnings.push(format!("ignoring {}: {e}", path.display()));
            return None;
        }
    };
    match toml::from_str::<File>(&text) {
        Ok(file) => {
            for key in file.unknown.keys() {
                warnings.push(format!("unknown setting `{key}` in {}", path.display()));
            }
            Some(file)
        }
        Err(e) => {
            let reason = e.message().trim().to_string();
            warnings.push(format!("ignoring {}: {reason}", path.display()));
            None
        }
    }
}

fn resolve(
    global: Option<File>,
    project: Option<File>,
    env: &dyn Fn(&str) -> Option<String>,
    mut warnings: Vec<String>,
) -> Config {
    let global = global.unwrap_or_default();
    let mut project = project.unwrap_or_default();

    if project.base_url.take().is_some() {
        warnings.push(format!(
            "ignoring `base_url` in {PROJECT_FILE}: it can only be set in the global config"
        ));
    }

    let mut w = |msg: String| warnings.push(msg);
    let disable = layer(
        false,
        global.disable,
        project.disable,
        "JEV_CC_DISABLE",
        env,
        &mut w,
    );
    let base_url = layer(
        DEFAULT_BASE_URL.to_string(),
        global.base_url,
        None,
        "JEV_CC_BASE_URL",
        env,
        &mut w,
    );
    let timeout_ms = layer(
        1000,
        global.timeout_ms,
        project.timeout_ms,
        "JEV_CC_TIMEOUT_MS",
        env,
        &mut w,
    );
    let deadline_ms = layer(
        2000,
        global.deadline_ms,
        project.deadline_ms,
        "JEV_CC_DEADLINE_MS",
        env,
        &mut w,
    );
    let min_confidence = layer(
        0.6,
        global.min_confidence,
        project.min_confidence,
        "JEV_CC_MIN_CONFIDENCE",
        env,
        &mut w,
    );
    let breaking_threshold = layer(
        0.85,
        global.breaking_threshold,
        project.breaking_threshold,
        "JEV_CC_BREAKING_THRESHOLD",
        env,
        &mut w,
    );

    let exclude = global
        .exclude
        .into_iter()
        .map(|value| Setting {
            value,
            source: Source::Global,
        })
        .chain(project.exclude.into_iter().map(|value| Setting {
            value,
            source: Source::Project,
        }))
        .collect();

    Config {
        disable,
        base_url,
        timeout_ms,
        deadline_ms,
        min_confidence,
        breaking_threshold,
        exclude,
        global_path: None,
        project_path: None,
        warnings,
    }
}

fn layer<T: EnvValue>(
    default: T,
    global: Option<T>,
    project: Option<T>,
    env_name: &'static str,
    env: &dyn Fn(&str) -> Option<String>,
    warn: &mut dyn FnMut(String),
) -> Setting<T> {
    let mut setting = Setting {
        value: default,
        source: Source::Default,
    };
    if let Some(value) = global {
        setting = Setting {
            value,
            source: Source::Global,
        };
    }
    if let Some(value) = project {
        setting = Setting {
            value,
            source: Source::Project,
        };
    }
    if let Some(raw) = env(env_name).filter(|v| !v.is_empty()) {
        match T::parse_env(&raw) {
            Some(value) => {
                setting = Setting {
                    value,
                    source: Source::Env(env_name),
                }
            }
            None => warn(format!("ignoring {env_name}={raw}: not a valid value")),
        }
    }
    setting
}

trait EnvValue: Sized {
    fn parse_env(raw: &str) -> Option<Self>;
}

impl EnvValue for bool {
    fn parse_env(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        }
    }
}

macro_rules! env_value_from_str {
    ($($t:ty),*) => {$(
        impl EnvValue for $t {
            fn parse_env(raw: &str) -> Option<Self> {
                <$t>::from_str(raw).ok()
            }
        }
    )*};
}
env_value_from_str!(u64, f64, String);

#[cfg(test)]
mod tests {
    use super::*;

    fn file(toml: &str) -> File {
        toml::from_str(toml).unwrap()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn defaults_when_nothing_is_set() {
        let c = resolve(None, None, &no_env, vec![]);
        assert_eq!(
            c.timeout_ms,
            Setting {
                value: 1000,
                source: Source::Default
            }
        );
        assert_eq!(c.base_url.value, DEFAULT_BASE_URL);
        assert!(!c.disable.value);
        assert!(c.exclude.is_empty());
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn project_overrides_global_and_env_overrides_both() {
        let global = file("timeout_ms = 500\nmin_confidence = 0.5\ndeadline_ms = 3000");
        let project = file("timeout_ms = 800\nmin_confidence = 0.75");
        let env = |name: &str| (name == "JEV_CC_MIN_CONFIDENCE").then(|| "0.9".to_string());
        let c = resolve(Some(global), Some(project), &env, vec![]);
        assert_eq!(
            c.deadline_ms,
            Setting {
                value: 3000,
                source: Source::Global
            }
        );
        assert_eq!(
            c.timeout_ms,
            Setting {
                value: 800,
                source: Source::Project
            }
        );
        assert_eq!(
            c.min_confidence,
            Setting {
                value: 0.9,
                source: Source::Env("JEV_CC_MIN_CONFIDENCE")
            }
        );
    }

    #[test]
    fn project_cannot_set_base_url() {
        let project = file("base_url = \"https://attacker.example\"");
        let c = resolve(None, Some(project), &no_env, vec![]);
        assert_eq!(c.base_url.value, DEFAULT_BASE_URL);
        assert!(c.warnings[0].contains("base_url"));
    }

    #[test]
    fn global_and_env_can_set_base_url() {
        let global = file("base_url = \"https://proxy.internal\"");
        let c = resolve(Some(global), None, &no_env, vec![]);
        assert_eq!(c.base_url.value, "https://proxy.internal");
        let env = |name: &str| (name == "JEV_CC_BASE_URL").then(|| "http://localhost:1".into());
        let c = resolve(None, None, &env, vec![]);
        assert_eq!(c.base_url.source, Source::Env("JEV_CC_BASE_URL"));
    }

    #[test]
    fn exclude_patterns_combine() {
        let global = file("exclude = [\"**/*.pem\"]");
        let project = file("exclude = [\"secrets/**\"]");
        let c = resolve(Some(global), Some(project), &no_env, vec![]);
        let patterns: Vec<_> = c
            .exclude
            .iter()
            .map(|s| (s.value.as_str(), s.source))
            .collect();
        assert_eq!(
            patterns,
            vec![
                ("**/*.pem", Source::Global),
                ("secrets/**", Source::Project)
            ]
        );
    }

    #[test]
    fn invalid_env_value_is_ignored_with_warning() {
        let env = |name: &str| (name == "JEV_CC_TIMEOUT_MS").then(|| "soon".to_string());
        let c = resolve(None, None, &env, vec![]);
        assert_eq!(c.timeout_ms.source, Source::Default);
        assert!(c.warnings[0].contains("JEV_CC_TIMEOUT_MS=soon"));
    }

    #[test]
    fn env_disable_accepts_common_spellings() {
        for (raw, expected) in [("1", true), ("TRUE", true), ("0", false), ("false", false)] {
            let env = move |name: &str| (name == "JEV_CC_DISABLE").then(|| raw.to_string());
            assert_eq!(resolve(None, None, &env, vec![]).disable.value, expected);
        }
    }

    #[test]
    fn unknown_keys_and_bad_files_warn() {
        let dir = std::env::temp_dir().join(format!("jev-cc-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let unknown = dir.join("unknown.toml");
        std::fs::write(&unknown, "timeout_ms = 5\ntypo_setting = 1\n").unwrap();
        let broken = dir.join("broken.toml");
        std::fs::write(&broken, "timeout_ms = \"not a number\"\n").unwrap();

        let mut warnings = Vec::new();
        assert_eq!(read(&unknown, &mut warnings).unwrap().timeout_ms, Some(5));
        assert!(read(&broken, &mut warnings).is_none());
        assert!(read(&dir.join("missing.toml"), &mut warnings).is_none());
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("typo_setting"));
        assert!(warnings[1].contains("broken.toml"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
