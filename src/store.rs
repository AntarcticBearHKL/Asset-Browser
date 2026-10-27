//! Persistence to `%APPDATA%\asset-library-desktop\store.json` (cart + bridge
//! configuration). Missing or corrupt files degrade to `Default`.

use std::path::PathBuf;

fn dark_default() -> bool {
    true
}
/// The shipped look: a light tint (20% transparency) over a strong frost
/// (100% blur) in both themes; the value is still remembered per theme, so
/// switching Dark/Light never drags one theme's number into the other's.
/// `Reset` in the settings sheet restores these.
fn transparency_dark_default() -> f32 {
    20.0
}
fn transparency_light_default() -> f32 {
    20.0
}
fn blur_default() -> f32 {
    100.0
}
fn sections_open_default() -> bool {
    true
}

/// Everything that survives a restart.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Store {
    #[serde(default)]
    pub cart: Vec<String>,
    #[serde(default)]
    pub blender_base: String,
    #[serde(default)]
    pub blender_root: String,
    #[serde(default)]
    pub unity_base: String,
    #[serde(default)]
    pub painter_base: String,
    #[serde(default)]
    pub painter_bridge: String,
    #[serde(default)]
    pub painter_root: String,
    /// Settings sheet: theme + glass sliders (0..100).
    #[serde(default = "dark_default")]
    pub dark: bool,
    /// The tint strength is remembered per theme: switching Dark/Light must not
    /// drag one number across two palettes (both ship at 20%, but the user can
    /// set each theme independently).
    #[serde(default = "transparency_dark_default")]
    pub glass_transparency_dark: f32,
    #[serde(default = "transparency_light_default")]
    pub glass_transparency_light: f32,
    #[serde(default = "blur_default")]
    pub glass_blur: f32,
    /// AI tagging: DeepSeek key + endpoint. Empty key = feature off; the Blender
    /// add-on and the MCP server read the same file, so this is the one place a
    /// key is configured.
    #[serde(default)]
    pub deepseek_api_key: String,
    #[serde(default = "deepseek_model_default")]
    pub deepseek_model: String,
    #[serde(default = "deepseek_base_default")]
    pub deepseek_base: String,
    #[serde(default)]
    pub deepseek_auto_tag: bool,
    /// Detail pane sections, shared by every asset and remembered across runs.
    #[serde(default = "sections_open_default")]
    pub specs_open: bool,
    #[serde(default = "sections_open_default")]
    pub category_open: bool,
    #[serde(default = "sections_open_default")]
    pub tags_open: bool,
}

fn deepseek_model_default() -> String {
    "deepseek-flash".to_string()
}

fn deepseek_base_default() -> String {
    "https://api.deepseek.com".to_string()
}

impl Default for Store {
    fn default() -> Self {
        Store {
            cart: Vec::new(),
            blender_base: crate::bridge::BLENDER_ADDON.to_string(),
            blender_root: crate::ROOT.to_string(),
            unity_base: "http://localhost:26440".to_string(),
            painter_base: "http://localhost:60041".to_string(),
            painter_bridge: "http://localhost:60041".to_string(),
            painter_root: crate::ROOT.to_string(),
            dark: dark_default(),
            glass_transparency_dark: transparency_dark_default(),
            glass_transparency_light: transparency_light_default(),
            glass_blur: blur_default(),
            deepseek_api_key: String::new(),
            deepseek_model: deepseek_model_default(),
            deepseek_base: deepseek_base_default(),
            deepseek_auto_tag: false,
            specs_open: true,
            category_open: true,
            tags_open: true,
        }
    }
}

impl Store {
    /// Fill blanks left by a partial/older store with the web defaults.
    pub fn fill_defaults(&mut self) {
        let d = Store::default();
        if self.blender_base.trim().is_empty() {
            self.blender_base = d.blender_base;
        }
        if self.blender_root.trim().is_empty() {
            self.blender_root = d.blender_root;
        }
        if self.unity_base.trim().is_empty() {
            self.unity_base = d.unity_base;
        }
        if self.painter_base.trim().is_empty() {
            self.painter_base = d.painter_base;
        }
        if self.painter_bridge.trim().is_empty() {
            self.painter_bridge = d.painter_bridge;
        }
        if self.painter_root.trim().is_empty() {
            self.painter_root = d.painter_root;
        }
        // Slider values arrive from JSON, where a hand-edited or old file can
        // hold anything; out-of-range or non-finite values fall back.
        if !(0.0..=100.0).contains(&self.glass_transparency_dark) {
            self.glass_transparency_dark = d.glass_transparency_dark;
        }
        if !(0.0..=100.0).contains(&self.glass_transparency_light) {
            self.glass_transparency_light = d.glass_transparency_light;
        }
        if !(0.0..=100.0).contains(&self.glass_blur) {
            self.glass_blur = d.glass_blur;
        }
        if self.deepseek_model.trim().is_empty() {
            self.deepseek_model = d.deepseek_model;
        }
        if self.deepseek_base.trim().is_empty() {
            self.deepseek_base = d.deepseek_base;
        }
    }

    /// The tint strength the given theme last used.
    pub fn transparency(&self, dark: bool) -> f32 {
        if dark {
            self.glass_transparency_dark
        } else {
            self.glass_transparency_light
        }
    }

    /// Remember the tint strength for the given theme.
    pub fn set_transparency(&mut self, dark: bool, value: f32) {
        if dark {
            self.glass_transparency_dark = value;
        } else {
            self.glass_transparency_light = value;
        }
    }
}

/// `%APPDATA%\asset-library-desktop\store.json` (temp dir when APPDATA is unset).
pub fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("asset-library-desktop").join("store.json")
}

/// Parse a store document. A leading UTF-8 BOM is tolerated because
/// PowerShell's `Set-Content -Encoding UTF8` (and some editors) write one, and
/// `serde_json` rejects it — a hand-edited store must not silently reset.
fn parse(text: &str) -> Option<Store> {
    serde_json::from_str::<Store>(text.strip_prefix('\u{feff}').unwrap_or(text)).ok()
}

/// Missing/corrupt store ⇒ `Default`.
pub fn load() -> Store {
    let mut store = std::fs::read_to_string(path())
        .ok()
        .and_then(|t| parse(&t))
        .unwrap_or_default();
    store.fill_defaults();
    migrate(&mut store);
    store
}

/// Older builds stored the local Python relay ports (9877/9878/9879). Those relays
/// no longer exist: the bridge logic runs in-process now, so repoint the same
/// fields at the real targets.
fn migrate(s: &mut Store) {
    if s.blender_base.contains(":9877") {
        s.blender_base = crate::bridge::BLENDER_ADDON.to_string();
    }
    if s.unity_base.contains(":9878") {
        s.unity_base = "http://localhost:26440".to_string();
    }
    if s.painter_bridge.contains(":9879") {
        s.painter_bridge = "http://localhost:60041".to_string();
    }
    if s.painter_base.is_empty() || s.painter_base.contains(":9879") {
        s.painter_base = "http://localhost:60041".to_string();
    }
}

/// Best-effort save: create parent dirs, ignore every error.
pub fn save(s: &Store) {
    let p = path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(&p, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tolerates_a_utf8_bom() {
        // BOM + a distinctive setting, so "parsed" and "fell back to Default"
        // cannot be confused.
        let text = "\u{feff}{ \"dark\": false, \"glass_transparency_light\": 12.5, \"glass_blur\": 34.0 }";
        let s = parse(text).expect("a BOM-prefixed store must parse");
        assert!(!s.dark);
        assert_eq!(s.glass_transparency_light, 12.5);
        assert_eq!(s.glass_blur, 34.0);
        // Absent keys take the shipped defaults, not zero.
        assert_eq!(s.glass_transparency_dark, 20.0);
    }

    #[test]
    fn transparency_defaults_are_20_in_both_themes() {
        let s = Store::default();
        assert_eq!(s.transparency(true), 20.0, "dark default tint");
        assert_eq!(s.transparency(false), 20.0, "light default tint");
        assert_eq!(s.glass_blur, 100.0);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse("not json").is_none());
    }
}
