//! Persistence to `%APPDATA%\asset-library-desktop\store.json` (cart + bridge
//! configuration). Missing or corrupt files degrade to `Default`.

use std::path::PathBuf;

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
    }
}

/// `%APPDATA%\asset-library-desktop\store.json` (temp dir when APPDATA is unset).
pub fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("asset-library-desktop").join("store.json")
}

/// Missing/corrupt store ⇒ `Default`.
pub fn load() -> Store {
    let mut store = std::fs::read_to_string(path())
        .ok()
        .and_then(|t| serde_json::from_str::<Store>(&t).ok())
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
