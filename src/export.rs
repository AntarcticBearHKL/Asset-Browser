//! Cart export payloads — byte-for-byte compatible with
//! `asset-browser/src/lib/state.js::buildExport` / `buildItemPayload`.

use crate::model::{Item, Kind};

/// ISO-8601 with milliseconds in UTC, e.g. `2026-10-05T13:52:00.000Z`.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The per-item record shared by both payloads. Materials omit model fields.
fn item_record(it: &Item) -> serde_json::Value {
    let kind = it.kind();
    let mut rec = serde_json::Map::new();
    rec.insert("kind".into(), kind.as_str().into());
    rec.insert("code".into(), it.code.clone().into());
    rec.insert("slug".into(), it.slug.clone().into());
    rec.insert("l1".into(), it.l1.clone().into());
    rec.insert("l2".into(), it.l2.clone().into());
    rec.insert("category".into(), it.category.clone().into());

    if kind == Kind::Material {
        if let Some(subtype) = &it.subtype {
            if !subtype.is_empty() {
                rec.insert("subtype".into(), subtype.clone().into());
            }
        }
        if let Some(res) = &it.resolution {
            rec.insert("resolution".into(), serde_json::json!(res));
        }
        rec.insert("n_channels".into(), it.n_channels.into());
        return serde_json::Value::Object(rec);
    }

    rec.insert("path".into(), it.path().into());
    rec.insert("fbx".into(), it.fbx_rel().into());
    rec.insert(
        "lods".into(),
        serde_json::to_value(&it.lods).unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
    );
    rec.insert(
        "textures".into(),
        serde_json::to_value(&it.textures).unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
    );
    serde_json::Value::Object(rec)
}

/// Full cart payload.
pub fn build_export(items: &[Item]) -> String {
    let codes: Vec<String> = items.iter().map(|it| it.code.clone()).collect();
    let records: Vec<serde_json::Value> = items.iter().map(item_record).collect();
    let payload = serde_json::json!({
        "version": 1,
        "generatedAt": now_iso(),
        "count": items.len(),
        "codes": codes,
        "items": records,
    });
    serde_json::to_string_pretty(&payload).unwrap_or_default()
}

/// Single-asset payload for the per-card copy action (count 1).
pub fn build_item_payload(it: &Item) -> String {
    let payload = serde_json::json!({
        "version": 1,
        "generatedAt": now_iso(),
        "count": 1,
        "codes": [it.code.clone()],
        "items": [item_record(it)],
    });
    serde_json::to_string_pretty(&payload).unwrap_or_default()
}
