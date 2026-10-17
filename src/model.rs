//! Manifest data model, asset kinds and the formatting helpers that reproduce
//! the web app's spec rows / badges / slide captions.
//!
//! Every field carries `#[serde(default)]` because the manifest is produced by
//! several different tools and legacy records omit fields (a missing `kind`
//! means a model).

use serde::Deserialize;

/// Physical dimensions of a model, in metres.
#[derive(Clone, Default, Deserialize)]
pub struct Dims {
    #[serde(default)]
    pub width: f64,
    #[serde(default)]
    pub height: f64,
    #[serde(default)]
    pub depth: f64,
}

/// One library entry (see PORT_SPEC §3).
// manifest.json wire record: the fields flagged as unread (id, hasPreview, rig,
// clipsFile, humanoidCount, extraCount, retargetable, glbBytes) mirror external
// JSON keys, so they must stay to keep parse/write round-trips lossless.
#[allow(dead_code)]
#[derive(Clone, Default, Deserialize)]
pub struct Item {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub l1: String,
    #[serde(default)]
    pub l2: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub name_original: String,
    #[serde(default)]
    pub tri_count: i64,
    #[serde(default)]
    pub lods: Vec<String>,
    #[serde(default)]
    pub n_tex: i64,
    #[serde(default)]
    pub dimensions_m: Option<Dims>,
    #[serde(default)]
    pub textures: Vec<String>,
    #[serde(default)]
    pub resolution: Option<Vec<i64>>,
    #[serde(default)]
    pub n_channels: i64,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default, rename = "hasPreview")]
    pub has_preview: bool,
    #[serde(default)]
    pub rig: String,
    #[serde(default)]
    pub glb: String,
    #[serde(default, rename = "clipsFile")]
    pub clips_file: String,
    #[serde(default, rename = "clipCount")]
    pub clip_count: i64,
    #[serde(default, rename = "boneCount")]
    pub bone_count: i64,
    #[serde(default, rename = "humanoidCount")]
    pub humanoid_count: i64,
    #[serde(default, rename = "extraCount")]
    pub extra_count: i64,
    #[serde(default, rename = "shapeHash")]
    pub shape_hash: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub retargetable: bool,
    #[serde(default, rename = "glbBytes")]
    pub glb_bytes: i64,
    #[serde(default)]
    pub profiles: Vec<serde_json::Value>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub untextured: bool,
    /// Only merged on per-asset refresh (the manifest rarely carries it).
    #[serde(default)]
    pub description: Option<String>,
}

/// The three asset families. `kind` is absent ⇒ model (legacy records).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Model,
    Material,
    Rig,
}

impl Kind {
    pub fn num(self) -> i32 {
        match self {
            Kind::Model => 0,
            Kind::Material => 1,
            Kind::Rig => 2,
        }
    }

    pub fn from_num(n: i32) -> Kind {
        match n {
            1 => Kind::Material,
            2 => Kind::Rig,
            _ => Kind::Model,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Model => "model",
            Kind::Material => "material",
            Kind::Rig => "rig",
        }
    }

    /// Count-label noun (uppercase), see `AppState::count_label`.
    pub fn noun(self) -> &'static str {
        match self {
            Kind::Model => "ASSETS",
            Kind::Material => "TEXTURES",
            Kind::Rig => "RIGS",
        }
    }
}

impl Item {
    pub fn kind(&self) -> Kind {
        match self.kind.as_str() {
            "material" => Kind::Material,
            "rig" => Kind::Rig,
            _ => Kind::Model,
        }
    }

    /// Library-relative posix folder for this asset (mirrors `deriveAssetPath`).
    pub fn path(&self) -> String {
        if !self.path.trim().is_empty() {
            return self.path.clone();
        }
        if let Some(cut) = self.preview.rfind('/') {
            if cut > 0 {
                return self.preview[..cut].to_string();
            }
        }
        let mut parts: Vec<&str> = Vec::new();
        for p in [self.l1.as_str(), self.l2.as_str(), self.slug.as_str()] {
            if !p.is_empty() {
                parts.push(p);
            }
        }
        parts.join("/")
    }

    pub fn fbx_rel(&self) -> String {
        format!("{}/{}.fbx", self.path(), self.slug)
    }

    pub fn has_tag(&self, t: &str) -> bool {
        self.tags.iter().any(|x| x == t)
    }

    /// Lowercase search haystack: `code slug name_original tags…`.
    pub fn hay(&self) -> String {
        format!(
            "{} {} {} {}",
            self.code,
            self.slug,
            self.name_original,
            self.tags.join(" ")
        )
        .to_lowercase()
    }
}

/// Parse a manifest document (object with `items`, or a legacy bare array).
///
/// Individual records that are structurally wrong are skipped rather than
/// failing the whole library — one bad entry must not blank the browser.
pub fn parse_manifest(text: &str) -> Result<Vec<Item>, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("manifest.json: {e}"))?;
    let entries = if let Some(items) = value.get("items") {
        items
    } else if value.is_array() {
        &value
    } else {
        return Err("manifest.json: missing `items` array".to_string());
    };
    let entries = entries
        .as_array()
        .ok_or_else(|| "manifest.json: `items` is not an array".to_string())?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut entry = entry.clone();
        if let Some(obj) = entry.as_object_mut() {
            // `#[serde(default)]` covers a *missing* field but not an explicit
            // `null`. The real manifest has `"tri_count": null` on PR-0003 and
            // PR-0004; without this the whole record fails to deserialize and
            // silently disappears from the library.
            obj.retain(|_, v| !v.is_null());
        }
        if let Ok(item) = serde_json::from_value::<Item>(entry) {
            out.push(item);
        }
    }
    Ok(out)
}

/// Thousands separators, e.g. `1913` → `"1,913"`.
pub fn format_int(n: i64) -> String {
    let neg = n < 0;
    let digits = (n as i128).unsigned_abs().to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

fn fmt_unit(v: f64) -> String {
    if !v.is_finite() {
        return "-".to_string();
    }
    // Mirror JS: toFixed(2) then strip trailing zeros (and a trailing dot).
    let mut s = format!("{v:.2}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s.is_empty() {
        "0".to_string()
    } else {
        s
    }
}

/// `"2 × 0.19 × 0.18 m"` or `"-"`.
pub fn format_dims(d: &Option<Dims>) -> String {
    match d {
        Some(d) => format!(
            "{} × {} × {} m",
            fmt_unit(d.width),
            fmt_unit(d.height),
            fmt_unit(d.depth)
        ),
        None => "-".to_string(),
    }
}

/// `"2048 × 2048"` or `"-"`.
pub fn format_resolution(r: &Option<Vec<i64>>) -> String {
    match r {
        Some(v) if v.len() >= 2 => format!("{} × {}", v[0], v[1]),
        _ => "-".to_string(),
    }
}

/// Short badge shown over the preview (materials show their resolution).
pub fn preview_badge(it: &Item) -> String {
    if it.kind() == Kind::Material {
        return format_resolution(&it.resolution);
    }
    if it.untextured {
        return "No textures".to_string();
    }
    let n = it.n_tex;
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} texture{plural}")
}

/// Key/value rows for the detail panel, in the web app's spec order.
pub fn spec_pairs(it: &Item) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |k: &str, v: String| out.push((k.to_string(), v));

    push("CODE", it.code.clone());
    push("SLUG", it.slug.clone());
    push("CATEGORY", it.category.clone());
    push("KIND", it.kind().as_str().to_string());

    match it.kind() {
        Kind::Material => {
            push("RESOLUTION", format_resolution(&it.resolution));
            push("CHANNELS", it.n_channels.to_string());
            push("TEXTURES", it.n_tex.to_string());
        }
        Kind::Rig => {
            push("BONES", it.bone_count.to_string());
            push("CLIPS", it.clip_count.to_string());
            push(
                "SHAPE HASH",
                if it.shape_hash.is_empty() {
                    "-".to_string()
                } else {
                    it.shape_hash.clone()
                },
            );
            push(
                "VENDOR",
                if it.vendor.is_empty() {
                    "-".to_string()
                } else {
                    it.vendor.clone()
                },
            );
            for (label, tag) in [
                ("SPINE", "spine"),
                ("IK", "ik"),
                ("TWIST", "twist"),
                ("WEAPON", "weapon"),
            ] {
                push(label, if it.has_tag(tag) { "yes" } else { "-" }.to_string());
            }
            if !it.glb.is_empty() {
                push("GLB", it.glb.clone());
            }
        }
        Kind::Model => {
            push("TRIANGLES", it.tri_count.to_string());
            push("TEXTURES", it.n_tex.to_string());
            if it.dimensions_m.is_some() {
                push("DIMENSIONS", format_dims(&it.dimensions_m));
            }
        }
    }

    if !it.tags.is_empty() {
        push("TAGS", it.tags.join(" · "));
    }

    if it.kind() == Kind::Rig {
        if let Some(base) = &it.base {
            if !base.is_empty() {
                push("SOURCE PACK", base.clone());
            }
        }
        if !it.profiles.is_empty() {
            let labels: Vec<String> = it.profiles.iter().map(profile_label).collect();
            push("PROFILES", labels.join(" · "));
        }
    }

    out
}

fn profile_label(v: &serde_json::Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    if let Some(obj) = v.as_object() {
        for key in ["name", "id", "slug"] {
            if let Some(s) = obj.get(key).and_then(|x| x.as_str()) {
                return s.to_string();
            }
        }
    }
    v.to_string()
}

const SLIDE_LABELS: &[(&str, &str)] = &[
    ("preview", "Preview"),
    ("basecolor", "Base Color"),
    ("normal", "Normal"),
    ("mask", "Mask (ARM)"),
    ("ao", "AO"),
    ("height", "Height"),
];

/// Filename → friendly caption, e.g. `BaseColor.png` → `"Base Color"`.
pub fn slide_label(rel: &str) -> String {
    let file = rel.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = match file.rsplit_once('.') {
        Some((head, _)) if !head.is_empty() => head,
        _ => file,
    };
    let stem = stem.to_lowercase();
    if let Some((_, label)) = SLIDE_LABELS.iter().find(|(k, _)| *k == stem) {
        return (*label).to_string();
    }
    let parts: Vec<String> = stem
        .split(['_', '-', ' '])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut chars = p.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if parts.is_empty() {
        "Texture".to_string()
    } else {
        parts.join(" ")
    }
}
