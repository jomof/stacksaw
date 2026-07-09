//! Layered configuration (§11).
//!
//! Layers, later wins: built-in defaults → `/etc/stacksaw/config.toml` →
//! `~/.config/stacksaw/config.toml` (+ drop-ins) → repo `.stacksaw.toml` →
//! `.git/stacksaw/config.toml` → environment (`STACKSAW_*`) → flags.
//!
//! The merge is bespoke (no config framework) and tracks provenance so
//! `stacksaw config show --origin` can print where each value came from.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::recent::DEFAULT_MARKERS;

/// The fully-merged configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub ui: UiConfig,
    pub rainbox: RainboxConfig,
    pub upstream: UpstreamConfig,
    pub lint: LintConfig,
    pub watch: WatchConfig,
    pub core: CoreConfig,
    pub monorepo: MonorepoConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: String,
    pub background: String,
    pub date_style: String,
    /// Glyph set for status/git marks: `nerd` (default; requires a patched Nerd
    /// Font terminal font) or `unicode` (legible on any terminal). Set
    /// `STACKSAW_GLYPHS=unicode` (or empty) to opt out of the Nerd glyphs.
    pub glyphs: String,
}
impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            theme: "default".into(),
            background: "auto".into(),
            date_style: "relative".into(),
            glyphs: "nerd".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RainboxConfig {
    /// `[h0, span]` in degrees.
    pub staircase_arc: [f32; 2],
    pub half_life: String,
    pub contrast_floor: f32,
}
impl Default for RainboxConfig {
    fn default() -> Self {
        RainboxConfig {
            staircase_arc: [250.0, -190.0],
            half_life: "14d".into(),
            contrast_floor: 0.18,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpstreamConfig {
    pub default: String,
}
impl Default for UpstreamConfig {
    fn default() -> Self {
        UpstreamConfig {
            default: "origin/main".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LintConfig {
    pub profile: String,
}
impl Default for LintConfig {
    fn default() -> Self {
        LintConfig {
            profile: "local".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchConfig {
    pub reconcile_interval: String,
}
impl Default for WatchConfig {
    fn default() -> Self {
        WatchConfig {
            reconcile_interval: "30s".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoreConfig {
    pub idle_shutdown: String,
}
impl Default for CoreConfig {
    fn default() -> Self {
        CoreConfig {
            idle_shutdown: "10m".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MonorepoConfig {
    /// Marker files/dirs that identify a monorepo root; the nearest ancestor of
    /// a repo containing any of these anchors the recents view's grouping
    /// (§8.1). Extend this to teach stacksaw about your workspace tool.
    pub markers: Vec<String>,
}
impl Default for MonorepoConfig {
    fn default() -> Self {
        MonorepoConfig {
            markers: DEFAULT_MARKERS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

/// Records which layer supplied a given key (§11 `--origin`).
#[derive(Debug, Clone, Default)]
pub struct Provenance {
    pub origins: BTreeMap<String, String>,
}

/// Load and merge all layers for a repo rooted at `repo_root` with common git
/// dir `git_dir`. Environment overrides (`STACKSAW_*`) are applied last.
pub fn load(repo_root: &Path, git_dir: &Path) -> (Config, Provenance) {
    let mut merged = toml::Value::Table(Default::default());
    let mut prov = Provenance::default();

    let mut layers: Vec<(String, PathBuf)> = Vec::new();
    layers.push(("system".into(), PathBuf::from("/etc/stacksaw/config.toml")));
    if let Some(dirs) = directories::ProjectDirs::from("", "", "stacksaw") {
        layers.push(("user".into(), dirs.config_dir().join("config.toml")));
    }
    layers.push(("repo".into(), repo_root.join(".stacksaw.toml")));
    layers.push(("local".into(), git_dir.join("stacksaw").join("config.toml")));

    for (name, path) in layers {
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(value) = toml::from_str::<toml::Value>(&text) {
                record_origins(&value, &name, "", &mut prov);
                merge(&mut merged, value);
            }
        }
    }

    let mut config: Config = merged.try_into().unwrap_or_default();
    apply_env(&mut config, &mut prov);
    (config, prov)
}

/// Recursively merge `overlay` into `base` (tables merge, scalars overwrite).
fn merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => merge(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

fn record_origins(value: &toml::Value, layer: &str, prefix: &str, prov: &mut Provenance) {
    if let toml::Value::Table(t) = value {
        for (k, v) in t {
            let key = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            if matches!(v, toml::Value::Table(_)) {
                record_origins(v, layer, &key, prov);
            } else {
                prov.origins.insert(key, layer.to_string());
            }
        }
    }
}

fn apply_env(config: &mut Config, prov: &mut Provenance) {
    if let Ok(v) = env::var("STACKSAW_UPSTREAM") {
        config.upstream.default = v;
        prov.origins.insert("upstream.default".into(), "env".into());
    }
    if let Ok(v) = env::var("STACKSAW_PROFILE") {
        config.lint.profile = v;
        prov.origins.insert("lint.profile".into(), "env".into());
    }
    if let Ok(v) = env::var("STACKSAW_BACKGROUND") {
        config.ui.background = v;
        prov.origins.insert("ui.background".into(), "env".into());
    }
    if let Ok(v) = env::var("STACKSAW_GLYPHS") {
        config.ui.glyphs = v;
        prov.origins.insert("ui.glyphs".into(), "env".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_overlays_scalars_and_tables() {
        let mut base: toml::Value = toml::from_str("[a]\nx = 1\ny = 2\n").unwrap();
        let overlay: toml::Value = toml::from_str("[a]\ny = 9\nz = 3\n").unwrap();
        merge(&mut base, overlay);
        let a = base.get("a").unwrap();
        assert_eq!(a.get("x").unwrap().as_integer(), Some(1));
        assert_eq!(a.get("y").unwrap().as_integer(), Some(9));
        assert_eq!(a.get("z").unwrap().as_integer(), Some(3));
    }

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.upstream.default, "origin/main");
        assert_eq!(c.rainbox.contrast_floor, 0.18);
        assert_eq!(c.core.idle_shutdown, "10m");
    }
}
