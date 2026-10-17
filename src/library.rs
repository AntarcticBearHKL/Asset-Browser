//! Filesystem access to the asset library.
//!
//! Every path is library-relative and posix-style on input; Windows separators
//! are produced only for actual I/O. `edit_asset` / `delete_asset` mirror the
//! web File System Access write API (folder move, `<slug>.json` rewrite and a
//! 1-space-indented manifest write).

use std::path::Path;

use crate::model::{parse_manifest, Item, Kind};

/// One library root. All file access is routed through this type.
pub struct Library {
    pub root: String,
}

impl Library {
    pub fn new(root: &str) -> Self {
        Library {
            root: root.to_string(),
        }
    }

    pub fn manifest_path(&self) -> String {
        self.abs_path("_Catalog/manifest.json")
    }

    /// Missing manifest ⇒ an empty library (the library simply has not been
    /// generated yet) — never an error.
    pub fn load_manifest(&self) -> Result<Vec<Item>, String> {
        let path = self.manifest_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => parse_manifest(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("{path}: {e}")),
        }
    }

    pub fn taxonomy_json(&self) -> Option<String> {
        self.read_text("_Catalog/taxonomy.json").ok()
    }

    pub fn read_text(&self, rel: &str) -> Result<String, String> {
        let path = self.abs_path(rel);
        std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))
    }

    /// `root` joined with a posix relative path, using Windows separators.
    pub fn abs_path(&self, rel: &str) -> String {
        let root = self.root.trim_end_matches(|c| c == '\\' || c == '/');
        let tail = rel
            .replace('/', "\\")
            .trim_start_matches('\\')
            .to_string();
        if tail.is_empty() {
            root.to_string()
        } else {
            format!("{root}\\{tail}")
        }
    }

    /// Persist a category/tags change. Returns the updated manifest entry.
    ///
    /// Divergence from the web FSA code, documented here because the spec
    /// freezes `base = match kind { Material => "Materials", Rig => "Rig", _ =>
    /// "Models" }`: the source folder is taken from `item.path()` (the real
    /// on-disk location) and the destination is `{base}/{l1}/{l2}/{slug}`. When
    /// the folder moves we also keep `path`/`preview` consistent, which the web
    /// code leaves stale.
    pub fn edit_asset(
        &self,
        code: &str,
        l1: &str,
        l2: &str,
        tags: &[String],
    ) -> Result<Item, String> {
        let manifest_path = self.manifest_path();
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("{manifest_path}: {e}"))?;
        let mut man: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("manifest.json: {e}"))?;

        let items = man
            .get_mut("items")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| "manifest.json: missing items array".to_string())?;
        let idx = items
            .iter()
            .position(|it| it.get("code").and_then(|c| c.as_str()) == Some(code))
            .ok_or_else(|| format!("asset not found: {code}"))?;
        let entry = items
            .get_mut(idx)
            .ok_or_else(|| format!("asset not found: {code}"))?;

        let slug = entry
            .get("slug")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        if slug.is_empty() {
            return Err(format!("asset {code} has no slug"));
        }
        let kind = entry
            .get("kind")
            .and_then(|s| s.as_str())
            .unwrap_or("model")
            .to_string();
        let kind_enum = match kind.as_str() {
            "material" => Kind::Material,
            "rig" => Kind::Rig,
            _ => Kind::Model,
        };
        let base = match kind_enum {
            Kind::Material => "Materials",
            Kind::Rig => "Rig",
            Kind::Model => "Models",
        };

        // Old folder: the real on-disk location (from the in-memory item view).
        let old_dir = serde_json::from_value::<Item>(entry.clone())
            .map(|it| it.path())
            .unwrap_or_else(|_| {
                let old_l1 = entry.get("l1").and_then(|s| s.as_str()).unwrap_or("");
                let old_l2 = entry.get("l2").and_then(|s| s.as_str()).unwrap_or("");
                join_dir(base, old_l1, old_l2, &slug)
            });
        let new_dir = join_dir(base, l1, l2, &slug);
        let moved = old_dir != new_dir;

        if moved {
            let src = self.abs_path(&old_dir);
            let dst = self.abs_path(&new_dir);
            if Path::new(&src).exists() {
                if let Some(parent) = Path::new(&dst).parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
                }
                copy_dir(&src, &dst)?;
                let _ = std::fs::remove_dir_all(&src);
            }
        }

        // Rewrite `<slug>.json` (merged over the existing doc, 2-space indent).
        let dir_abs = self.abs_path(&new_dir);
        let _ = std::fs::create_dir_all(&dir_abs);
        let json_path = format!("{dir_abs}\\{slug}.json");
        let mut doc: serde_json::Value = std::fs::read_to_string(&json_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if !doc.is_object() {
            doc = serde_json::json!({});
        }
        if let Some(obj) = doc.as_object_mut() {
            obj.insert("id".into(), serde_json::json!(slug));
            obj.insert("name".into(), serde_json::json!(slug));
            obj.insert("kind".into(), serde_json::json!(kind));
            obj.insert("category".into(), serde_json::json!(format!("{l1}/{l2}")));
            obj.insert("tags".into(), serde_json::json!(tags));
        }
        let _ = std::fs::write(
            &json_path,
            serde_json::to_string_pretty(&doc).unwrap_or_default(),
        );

        // Update the manifest entry.
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("l1".into(), serde_json::json!(l1));
            obj.insert("l2".into(), serde_json::json!(l2));
            obj.insert("category".into(), serde_json::json!(format!("{l1}/{l2}")));
            obj.insert("tags".into(), serde_json::json!(tags));
            if moved {
                obj.insert("path".into(), serde_json::json!(new_dir));
                if kind_enum != Kind::Rig {
                    obj.insert(
                        "preview".into(),
                        serde_json::json!(format!("{new_dir}/{slug}.png")),
                    );
                }
            } else if kind_enum == Kind::Model {
                // Keep the canonical preview path even when only tags changed.
                obj.insert(
                    "preview".into(),
                    serde_json::json!(format!("{new_dir}/{slug}.png")),
                );
            }
        }
        let entry_view = entry.clone();
        if let Some(root) = man.as_object_mut() {
            root.insert(
                "generatedAt".into(),
                serde_json::json!(crate::export::now_iso()),
            );
        }

        std::fs::write(&manifest_path, to_string_indent(&man, b" "))
            .map_err(|e| format!("{manifest_path}: {e}"))?;

        Ok(serde_json::from_value::<Item>(entry_view).unwrap_or_default())
    }

    /// Remove the asset folder and its manifest entry.
    pub fn delete_asset(&self, code: &str) -> Result<(), String> {
        let manifest_path = self.manifest_path();
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("{manifest_path}: {e}"))?;
        let mut man: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("manifest.json: {e}"))?;

        let items = man
            .get_mut("items")
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| "manifest.json: missing items array".to_string())?;
        let idx = items
            .iter()
            .position(|it| it.get("code").and_then(|c| c.as_str()) == Some(code))
            .ok_or_else(|| format!("asset not found: {code}"))?;
        let dir = serde_json::from_value::<Item>(items[idx].clone())
            .map(|it| it.path())
            .unwrap_or_default();

        if !dir.is_empty() {
            let abs = self.abs_path(&dir);
            if Path::new(&abs).exists() {
                let _ = std::fs::remove_dir_all(&abs);
            }
        }
        items.remove(idx);
        if let Some(root) = man.as_object_mut() {
            root.insert(
                "generatedAt".into(),
                serde_json::json!(crate::export::now_iso()),
            );
        }
        std::fs::write(&manifest_path, to_string_indent(&man, b" "))
            .map_err(|e| format!("{manifest_path}: {e}"))?;
        Ok(())
    }
}

fn join_dir(base: &str, l1: &str, l2: &str, slug: &str) -> String {
    let mut parts: Vec<&str> = vec![base];
    if !l1.is_empty() {
        parts.push(l1);
    }
    if !l2.is_empty() {
        parts.push(l2);
    }
    if !slug.is_empty() {
        parts.push(slug);
    }
    parts.join("/")
}

/// Recursive copy (directories created as needed). Non-UTF-8 names degrade via
/// lossy conversion; failures bubble up so the caller can report them.
fn copy_dir(src: &str, dst: &str) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {dst}: {e}"))?;
    let reader = std::fs::read_dir(src).map_err(|e| format!("read_dir {src}: {e}"))?;
    for entry in reader {
        let entry = entry.map_err(|e| e.to_string())?;
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        let to = format!("{dst}\\{name}");
        if file_type.is_dir() {
            copy_dir(&entry.path().to_string_lossy(), &to)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &to).map_err(|e| format!("copy {name}: {e}"))?;
        }
    }
    Ok(())
}

/// Serialise JSON with a caller-chosen indent (the web writes the manifest with
/// `JSON.stringify(man, null, 1)`).
fn to_string_indent(value: &serde_json::Value, indent: &[u8]) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent);
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    match serde::Serialize::serialize(value, &mut ser) {
        Ok(()) => String::from_utf8(buf).unwrap_or_default(),
        Err(_) => String::new(),
    }
}
