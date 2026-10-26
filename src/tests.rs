//! Deterministic tests for the data layer (no GUI, no network).
//!
//! The real-library tests assert against `D:\3D Resource` and are skipped when
//! that manifest is absent, so the suite still passes on a machine without it.

use crate::export::{build_export, build_item_payload, now_iso};
use crate::library::Library;
use crate::model::{
    format_dims, format_int, format_resolution, parse_manifest, preview_badge, spec_pairs, Dims,
    Item, Kind,
};
use crate::state::{
    build_taxonomy, page_size_options, sort_options, style_options, AppState, SortKey, Taxonomy,
};

const REAL_ROOT: &str = r"D:\3D Resource";

fn item(code: &str, kind: &str, l1: &str, l2: &str, slug: &str, tags: &[&str]) -> Item {
    Item {
        code: code.into(),
        kind: kind.into(),
        l1: l1.into(),
        l2: l2.into(),
        slug: slug.into(),
        category: if l1.is_empty() {
            String::new()
        } else {
            format!("{l1}/{l2}")
        },
        tags: tags.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn sample() -> AppState {
    AppState::new(
        vec![
            item("AR-0001", "model", "Architecture", "Beam", "beam_a", &["wood", "stylized"]),
            item("AR-0002", "model", "Architecture", "Wall", "wall_a", &["stone"]),
            item("RO-0001", "model", "Rock", "Boulder", "rock_a", &["rock"]),
            item("MT-0001", "material", "Terrain", "Ground", "ground_a", &["realistic"]),
            item("RG-0001", "rig", "Humanoid", "Biped", "rig_a", &["ik"]),
        ],
        Vec::new(),
        Taxonomy::default(),
    )
}

/* ------------------------------ model.rs -------------------------------- */

#[test]
fn manifest_honours_camel_case_renames_and_default_kind() {
    let text = r#"{"version":1,"count":2,"items":[
      {"code":"AR-0001","slug":"a","l1":"Architecture","l2":"Beam",
       "preview":"Models/Architecture/Beam/a/a.png","tri_count":12,
       "hasPreview":true,"clipCount":3,"boneCount":9,"shapeHash":"dead","glbBytes":100},
      {"code":"RG-0001","slug":"r","kind":"rig","glb":"Rig/Humanoid/r/r.glb"}
    ]}"#;
    let items = parse_manifest(text).expect("parses");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].kind(), Kind::Model);
    assert!(items[0].has_preview);
    assert_eq!(items[0].clip_count, 3);
    assert_eq!(items[0].bone_count, 9);
    assert_eq!(items[0].shape_hash, "dead");
    assert_eq!(items[0].glb_bytes, 100);
    assert_eq!(items[1].kind(), Kind::Rig);
    assert_eq!(items[1].glb, "Rig/Humanoid/r/r.glb");
}

#[test]
fn one_broken_record_does_not_blank_the_library() {
    let items = parse_manifest(r#"{"items":[{"code":"A"}, 42, "nope", {"code":"B"}]}"#).unwrap();
    assert_eq!(items.len(), 2);
    let legacy = parse_manifest(r#"[{"code":"A"}]"#).unwrap();
    assert_eq!(legacy.len(), 1);
}

#[test]
fn path_prefers_field_then_preview_then_category() {
    let a = Item { path: "Models/X/y".into(), preview: "p/q/z.png".into(), ..Default::default() };
    assert_eq!(a.path(), "Models/X/y");
    let b = Item {
        preview: "Models/Architecture/Beam/balk_1_1/balk_1_1.png".into(),
        ..Default::default()
    };
    assert_eq!(b.path(), "Models/Architecture/Beam/balk_1_1");
    let c = Item { l1: "Rock".into(), l2: "Boulder".into(), slug: "rock_a".into(), ..Default::default() };
    assert_eq!(c.path(), "Rock/Boulder/rock_a");
    let d = Item { slug: "solo".into(), ..Default::default() };
    assert_eq!(d.path(), "solo");
}

#[test]
fn fbx_path_and_search_haystack() {
    let it = Item {
        code: "RO-0001".into(),
        slug: "rock_a".into(),
        l1: "Rock".into(),
        l2: "Boulder".into(),
        name_original: "Rock_A.fbx".into(),
        tags: vec!["stone".into()],
        ..Default::default()
    };
    assert_eq!(it.fbx_rel(), "Rock/Boulder/rock_a/rock_a.fbx");
    let hay = it.hay();
    assert!(hay.contains("ro-0001"));
    assert!(hay.contains("rock_a"));
    assert!(hay.contains("rock_a.fbx"));
    assert!(hay.contains("stone"));
}

#[test]
fn number_and_dimension_formatting_matches_the_web() {
    assert_eq!(format_int(0), "0");
    assert_eq!(format_int(52), "52");
    assert_eq!(format_int(1913), "1,913");
    assert_eq!(format_int(-1_234_567), "-1,234,567");

    assert_eq!(format_dims(&None), "-");
    let d = Some(Dims { width: 2.002, height: 0.188, depth: 0.184 });
    assert_eq!(format_dims(&d), "2 × 0.19 × 0.18 m");

    assert_eq!(format_resolution(&None), "-");
    assert_eq!(format_resolution(&Some(vec![2048, 2048])), "2048 × 2048");
    assert_eq!(format_resolution(&Some(vec![1])), "-");
}

#[test]
fn preview_badge_per_kind() {
    let mat = Item { kind: "material".into(), resolution: Some(vec![2048, 2048]), ..Default::default() };
    assert_eq!(preview_badge(&mat), "2048 × 2048");
    assert_eq!(preview_badge(&Item { n_tex: 1, ..Default::default() }), "1 texture");
    assert_eq!(preview_badge(&Item { n_tex: 4, ..Default::default() }), "4 textures");
    assert_eq!(preview_badge(&Item { untextured: true, ..Default::default() }), "No textures");
}

#[test]
fn spec_rows_use_the_web_order() {
    let model = Item {
        code: "AR-0001".into(),
        slug: "beam".into(),
        l1: "Architecture".into(),
        l2: "Beam".into(),
        category: "Architecture/Beam".into(),
        tri_count: 12,
        n_tex: 2,
        tags: vec!["t1".into(), "t2".into()],
        ..Default::default()
    };
    let rows = spec_pairs(&model);
    let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(&keys[..4], &["CODE", "SLUG", "CATEGORY", "KIND"]);
    assert!(keys.contains(&"TRIANGLES"));
    assert_eq!(rows.iter().find(|(k, _)| k == "KIND").unwrap().1, "model");
    assert_eq!(rows.iter().find(|(k, _)| k == "TAGS").unwrap().1, "t1 · t2");
}

/* ------------------------------ state.rs -------------------------------- */

#[test]
fn kind_counts_and_default_view() {
    let s = sample();
    assert_eq!(s.kind_count(Kind::Model), 3);
    assert_eq!(s.kind_count(Kind::Material), 1);
    assert_eq!(s.kind_count(Kind::Rig), 1);
    assert!(!s.filters_active());
    assert_eq!(s.filtered().len(), 3);
    assert_eq!(s.count_label(), "3 ASSETS");
}

#[test]
fn switching_tab_resets_filters_and_reports_its_own_counts() {
    let mut s = sample();
    s.l1 = "Architecture".into();
    s.l2 = "Beam".into();
    s.set_tab(Kind::Rig);
    assert!(s.l1.is_empty() && s.l2.is_empty());
    assert_eq!(s.count_label(), "1 RIGS");
    s.set_tab(Kind::Material);
    assert_eq!(s.count_label(), "1 TEXTURES");
}

#[test]
fn category_options_are_derived_and_sorted() {
    let mut s = sample();
    s.set_tab(Kind::Model);
    assert_eq!(s.l1_options(), vec!["Architecture", "Rock"]);
    assert!(s.l2_options().is_empty());
    s.l1 = "Architecture".into();
    assert_eq!(s.l2_options(), vec!["Beam", "Wall"]);
}

#[test]
fn search_covers_code_slug_original_name_and_tags() {
    let mut s = sample();
    s.q = "ar-0002".into();
    assert_eq!(s.filtered().len(), 1);
    s.q = "stylized".into();
    assert_eq!(s.filtered().len(), 1);
    s.q = "rock_a".into();
    assert_eq!(s.filtered().len(), 1);
    s.q = "nothing-here".into();
    assert_eq!(s.filtered().len(), 0);
    assert!(s.filters_active());
}

#[test]
fn filters_compose_and_count_label_shows_matches() {
    let mut s = sample();
    s.l1 = "Architecture".into();
    assert_eq!(s.filtered().len(), 2);
    s.l2 = "Beam".into();
    assert_eq!(s.filtered().len(), 1);
    assert_eq!(s.count_label(), "3 ASSETS · 1 matching");
    s.l2.clear();
    s.tag = "stone".into();
    assert_eq!(s.filtered().len(), 1);
    s.reset_filters();
    assert!(!s.filters_active());
    assert_eq!(s.filtered().len(), 3);
}

#[test]
fn sort_keys_order_the_view() {
    let mut s = sample();
    s.items[0].tri_count = 30;
    s.items[1].tri_count = 10;
    s.items[2].tri_count = 20;
    let codes = |s: &AppState| -> Vec<String> {
        s.filtered().iter().map(|&i| s.items[i].code.clone()).collect()
    };
    s.sort = SortKey::Code;
    assert_eq!(codes(&s), vec!["AR-0001", "AR-0002", "RO-0001"]);
    s.sort = SortKey::TriAsc;
    assert_eq!(codes(&s), vec!["AR-0002", "RO-0001", "AR-0001"]);
    s.sort = SortKey::TriDesc;
    assert_eq!(codes(&s), vec!["AR-0001", "RO-0001", "AR-0002"]);
    s.sort = SortKey::Slug;
    assert_eq!(codes(&s), vec!["AR-0001", "RO-0001", "AR-0002"]);
}

#[test]
fn triangle_sort_is_unavailable_on_material_and_rig_tabs() {
    let mut s = sample();
    s.set_tab(Kind::Material);
    s.sort = SortKey::TriDesc;
    s.ensure_integrity();
    assert_eq!(s.sort, SortKey::Code);
}

#[test]
fn cart_toggle_add_all_filtered_and_prune() {
    let mut s = sample();
    assert!(s.toggle_cart("AR-0001"));
    assert!(s.in_cart("AR-0001"));
    assert!(!s.toggle_cart("AR-0001"));
    assert!(!s.in_cart("AR-0001"));

    s.q = "rock".into();
    assert_eq!(s.add_all_filtered(), 1);
    assert_eq!(s.cart, vec!["RO-0001"]);
    assert_eq!(s.add_all_filtered(), 0);
    assert!(s.remove_cart("RO-0001"));
    s.clear_cart();
    assert!(s.cart.is_empty());
}

#[test]
fn cart_codes_that_no_longer_resolve_are_dropped_on_load() {
    let s = AppState::new(
        vec![item("AR-0001", "model", "Architecture", "Beam", "beam_a", &[])],
        vec!["AR-0001".into(), "GONE-9999".into()],
        Taxonomy::default(),
    );
    assert_eq!(s.cart, vec!["AR-0001"]);
}

#[test]
fn taxonomy_prefers_json_then_derives_missing_kinds() {
    let json = r#"{"model":{"Architecture":["Wall","Beam","Beam"]}}"#;
    let rigs = vec![
        item("RG-1", "rig", "Humanoid", "UE-Style", "s1", &[]),
        item("RG-2", "rig", "Humanoid", "Biped", "s2", &[]),
    ];
    let tax = build_taxonomy(&rigs, Some(json));
    assert_eq!(tax.l1_for(Kind::Model), vec!["Architecture"]);
    assert_eq!(tax.l2_for(Kind::Model, "Architecture"), vec!["Beam", "Wall"]);
    assert_eq!(tax.l1_for(Kind::Rig), vec!["Humanoid"]);
    assert_eq!(tax.l2_for(Kind::Rig, "Humanoid"), vec!["Biped", "UE-Style"]);
}

#[test]
fn dropdown_models_match_the_web() {
    assert_eq!(sort_options(), vec!["Code", "Slug", "Triangles ↑", "Triangles ↓"]);
    assert_eq!(style_options(), vec!["All", "Textured only"]);
}

/* -------------------------- state.rs: pagination ------------------------ */

fn models(n: usize) -> AppState {
    let items: Vec<Item> = (0..n)
        .map(|i| {
            item(
                &format!("AR-{i:04}"),
                "model",
                "Architecture",
                "Beam",
                &format!("b{i}"),
                &[],
            )
        })
        .collect();
    AppState::new(items, Vec::new(), Taxonomy::default())
}

#[test]
fn page_size_options_are_the_documented_set() {
    assert_eq!(page_size_options(), vec![12, 24, 48, 96]);
}

#[test]
fn page_count_rounds_up_and_never_hits_zero() {
    assert_eq!(models(0).page_count(), 1, "empty list still has one page");
    assert_eq!(models(12).page_count(), 1, "exact multiple");
    assert_eq!(models(13).page_count(), 2, "remainder adds a page");
    assert_eq!(models(24).page_count(), 2);
}

#[test]
fn page_count_matches_the_ceiling_of_filtered_over_page_size() {
    let s = models(37);
    let filtered = s.filtered().len();
    println!(
        "PAGINATION filtered={} page_size={} page_count={}",
        filtered,
        s.page_size,
        s.page_count()
    );
    assert_eq!(s.page_count(), (filtered + s.page_size - 1) / s.page_size);
    assert_eq!(s.page_count(), 4, "37 items at 12/page is 12,12,12,1");
}

#[test]
fn clamp_page_pulls_the_index_back_after_the_list_shrinks() {
    let mut s = models(30);
    s.page = 2;
    assert_eq!(s.page_count(), 3);
    s.items = models(5).items;
    s.clamp_page();
    assert_eq!(s.page_count(), 1);
    assert_eq!(s.page, 0);
    assert!(s.page < s.page_count());
}

#[test]
fn set_page_size_keeps_the_first_visible_item() {
    let mut s = models(200);

    s.page_size = 12;
    s.page = 3;
    assert_eq!(s.first_item_on_page(), 36);
    s.set_page_size(48);
    assert_eq!(s.page_size, 48);
    assert_eq!(s.page, 0, "item 36 lands on the page starting at 0");
    assert!(s.first_item_on_page() <= 36 && 36 < s.first_item_on_page() + s.page_size);

    s.page_size = 12;
    s.page = 10;
    assert_eq!(s.first_item_on_page(), 120);
    s.set_page_size(96);
    assert_eq!(s.page, 1, "120 / 96 == 1");
    assert!(s.first_item_on_page() <= 120 && 120 < s.first_item_on_page() + s.page_size);

    s.set_page_size(7);
    assert_eq!(s.page_size, 96, "an unlisted size is ignored");
}

#[test]
fn page_slice_and_rows_chunk_thirteen_items_as_6_6_1() {
    let all: Vec<usize> = (0..13).collect();

    assert_eq!(crate::page_slice(&all, 0, 12), &all[0..12]);
    assert_eq!(crate::page_slice(&all, 12, 12), &all[12..13]);
    assert!(crate::page_slice(&all, 24, 12).is_empty(), "past the last page");

    let page0 = crate::rows_from_slice(crate::page_slice(&all, 0, 12));
    let page1 = crate::rows_from_slice(crate::page_slice(&all, 12, 12));
    let ns: Vec<i32> = page0.iter().chain(page1.iter()).map(|r| r.n).collect();
    assert_eq!(ns, vec![6, 6, 1]);
    assert_eq!(page0.len(), 2);
    assert_eq!(page1.len(), 1);

    // Rows carry GLOBAL indices; the last slot is the -1 filler.
    assert_eq!((page0[0].a, page0[0].f, page0[0].n), (0, 5, 6));
    assert_eq!((page1[0].a, page1[0].b, page1[0].n), (12, -1, 1));
}

/* ------------------------------ export.rs ------------------------------- */

#[test]
fn export_payload_matches_the_web_shape() {
    let model = Item {
        code: "AR-0001".into(),
        slug: "beam".into(),
        l1: "Architecture".into(),
        l2: "Beam".into(),
        category: "Architecture/Beam".into(),
        preview: "Models/Architecture/Beam/beam/beam.png".into(),
        lods: vec!["beam.fbx".into()],
        textures: vec!["_textures/Model/Wood/w.png".into()],
        ..Default::default()
    };
    let mat = Item {
        kind: "material".into(),
        code: "MT-0001".into(),
        slug: "ground".into(),
        l1: "Terrain".into(),
        l2: "Ground".into(),
        category: "Terrain/Ground".into(),
        resolution: Some(vec![2048, 2048]),
        n_channels: 4,
        ..Default::default()
    };

    let v: serde_json::Value = serde_json::from_str(&build_export(&[model, mat])).unwrap();
    assert_eq!(v["version"], 1);
    assert_eq!(v["count"], 2);
    assert_eq!(v["codes"], serde_json::json!(["AR-0001", "MT-0001"]));
    assert_eq!(v["items"][0]["fbx"], "Models/Architecture/Beam/beam/beam.fbx");
    assert_eq!(v["items"][0]["textures"][0], "_textures/Model/Wood/w.png");
    assert_eq!(v["items"][1]["resolution"], serde_json::json!([2048, 2048]));
    assert_eq!(v["items"][1]["n_channels"], 4);
    assert!(v["items"][1].get("fbx").is_none());
    assert!(v["generatedAt"].as_str().unwrap().ends_with('Z'));
    assert!(now_iso().ends_with('Z'));
}

#[test]
fn single_item_payload_is_count_one() {
    let it = Item { code: "AR-0002".into(), slug: "x".into(), ..Default::default() };
    let v: serde_json::Value = serde_json::from_str(&build_item_payload(&it)).unwrap();
    assert_eq!(v["count"], 1);
    assert_eq!(v["codes"][0], "AR-0002");
}

#[test]
fn empty_cart_exports_zero_codes() {
    let v: serde_json::Value = serde_json::from_str(&build_export(&[])).unwrap();
    assert_eq!(v["count"], 0);
    assert_eq!(v["codes"], serde_json::json!([]));
}

/* ------------------------ real library (disk) --------------------------- */

fn real_library() -> Option<Library> {
    let lib = Library::new(REAL_ROOT);
    if std::path::Path::new(&lib.manifest_path()).exists() {
        Some(lib)
    } else {
        None
    }
}

#[test]
fn real_manifest_matches_ground_truth() {
    let Some(lib) = real_library() else {
        return;
    };
    let items = lib.load_manifest().expect("manifest loads");
    assert_eq!(items.len(), 1976);
    assert_eq!(items.iter().filter(|i| i.kind == "model").count(), 1913);
    assert_eq!(items.iter().filter(|i| i.kind == "material").count(), 52);
    assert_eq!(items.iter().filter(|i| i.kind == "rig").count(), 11);
    assert!(items.iter().filter(|i| i.kind == "rig").all(|i| !i.slug.is_empty()));
}

/* ------------------------- rig viewer (GLB) ----------------------------- */

const REAL_GLB: &str = r"D:\3D Resource\Rig\Humanoid\rig_9cg_archer\rig_9cg_archer.glb";

/// Load the real rig, or `None` when the library is absent. If the file exists it
/// MUST load — a failure is a test failure, not a silent skip.
fn real_rig() -> Option<crate::rig::Rig> {
    if !std::path::Path::new(REAL_GLB).exists() {
        return None;
    }
    Some(crate::rig::load(REAL_GLB).expect("the real GLB must load"))
}

/// One rig frame through the public pipeline (pose -> render), with the floor
/// and contact shadow the app uses. `cam = None` re-fits the camera to this
/// pose, previewing the app's follow behaviour.
fn render_rig_frame(
    rig: &crate::rig::Rig,
    clip: usize,
    t: f32,
    cam: Option<&crate::raster::Camera>,
    w: u32,
    h: u32,
    buf: &mut [u8],
    floor: bool,
) {
    let pose = rig.pose(clip, t);
    let cam = match cam {
        Some(c) => *c,
        None => {
            let (target, dist) = crate::raster::Camera::fit(
                pose.min,
                pose.max,
                crate::raster::DEFAULT_YAW,
                crate::raster::DEFAULT_PITCH,
                w as f32 / h as f32,
            );
            crate::raster::Camera {
                yaw: crate::raster::DEFAULT_YAW,
                pitch: crate::raster::DEFAULT_PITCH,
                dist,
                target,
            }
        }
    };
    let (bmn, bmx) = rig.aabb();
    let height = (bmx[1] - bmn[1]).max(1e-3);
    let dx = (pose.max[0] - pose.min[0]).max(0.0);
    let dz = (pose.max[2] - pose.min[2]).max(0.0);
    let shadow = crate::raster::Shadow {
        center: [
            (pose.min[0] + pose.max[0]) * 0.5,
            (pose.min[2] + pose.max[2]) * 0.5,
        ],
        radius: (0.55 * (dx * dx + dz * dz).sqrt()).max(height * 0.18),
        strength: 0.9 / (1.0 + (pose.min[1] - bmn[1]).max(0.0) * 3.0),
    };
    let floor = floor.then_some(crate::raster::Floor {
        y: bmn[1],
        step: grid_step(height),
        reach: (height * 7.0).max(1.5),
        shadow: Some(shadow),
    });
    crate::raster::render(
        buf,
        w,
        h,
        &pose.positions,
        &pose.normals,
        &rig.indices,
        &cam,
        floor,
        &crate::raster::Overlay::default(),
    );
}

fn grid_step(height: f32) -> f32 {
    let raw = (height / 5.0).max(1e-3);
    let base = 10f32.powf(raw.log10().floor());
    let n = raw / base;
    let nice = if n < 1.5 {
        1.0
    } else if n < 3.5 {
        2.0
    } else if n < 7.5 {
        5.0
    } else {
        10.0
    };
    nice * base
}

#[test]
fn real_glb_loads_with_the_expected_geometry() {
    let Some(rig) = real_rig() else {
        return;
    };
    assert_eq!(rig.rest_positions.len(), 26_804, "vertex count");
    assert_eq!(rig.indices.len() / 3, 45_190, "triangle count");
    // The manifest records 40 clips, but the GLB itself carries 41 animations
    // (the extra one is the `root|Take 001|BaseLayer` NLA take).
    assert_eq!(rig.clip_count(), 41, "clip count");
    assert!(rig.joint_count() > 60, "joint count {}", rig.joint_count());
    let (min, max) = rig.aabb();
    assert!(min.iter().all(|v| v.is_finite()));
    assert!(max.iter().all(|v| v.is_finite()));
    assert!(
        max[0] > min[0] && max[1] > min[1] && max[2] > min[2],
        "AABB must be non-empty: {min:?} .. {max:?}"
    );
}

/// The viewer opens on the asset's own first clip (Idle), not on whatever the
/// exporter wrote first, and keeps the GLB's extra NLA take at the end.
#[test]
fn rig_clips_open_on_idle_in_catalog_order() {
    let Some(rig) = real_rig() else { return; };
    let clips = rig.clips();
    assert_eq!(clips[0].0, "Idle", "catalog order starts with Idle");
    assert_eq!(rig.default_clip(), 0);
    assert!(
        clips.last().unwrap().0.to_lowercase().contains("take"),
        "the extra NLA take goes last, got {:?}",
        clips.last()
    );
}

/// A pose line-up GLB (one skinned body plus dozens of unskinned copies of it)
/// must load as the single animated character: the frozen copies can never
/// follow the animation and read as "the animation is not applied".
#[test]
fn pose_lineup_rigs_keep_only_the_animated_character() {
    let Some(lib) = real_library() else { return; };
    let items = lib.load_manifest().expect("manifest loads");
    for code in ["RG-0006", "RG-0008", "RG-0010"] {
        let Some(it) = items.iter().find(|i| i.code == code) else {
            continue;
        };
        if it.glb.trim().is_empty() {
            continue;
        }
        let rig = crate::rig::load(&lib.abs_path(&it.glb)).expect("loads");
        assert!(
            rig.rest_positions.len() < 300_000,
            "{code}: {} vertices left after dropping the copies",
            rig.rest_positions.len()
        );
        assert!(
            rig.indices.len() / 3 < 300_000,
            "{code}: {} triangles left",
            rig.indices.len() / 3
        );
        let (mn, mx) = rig.aabb();
        let span = (mx[0] - mn[0]).max(mx[1] - mn[1]).max(mx[2] - mn[2]);
        assert!(span < 500.0, "{code}: AABB spans {span}, copies still loaded");
    }
}

/// Whatever clip a rig opens on must actually move the skeleton, whenever the
/// file has any animated clip at all (a frozen `00_T-pose` take must not win).
#[test]
fn default_clip_animates_the_skeleton() {
    let Some(lib) = real_library() else { return; };
    let items = lib.load_manifest().expect("manifest loads");
    for it in items
        .iter()
        .filter(|i| i.kind == "rig" && !i.glb.trim().is_empty())
    {
        let rig = match crate::rig::load(&lib.abs_path(&it.glb)) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let animated = |clip: usize| -> bool {
            let dur = rig.clip_duration(clip);
            if dur <= 0.0 {
                return false;
            }
            let a = rig.joint_matrices(clip, 0.0);
            (1..=4).any(|k| {
                let m = rig.joint_matrices(clip, dur * k as f32 / 4.0);
                a.iter().zip(&m).any(|(x, y)| {
                    (0..4).any(|c| (0..4).any(|r| (x[c][r] - y[c][r]).abs() > 1e-4))
                })
            })
        };
        if (0..rig.clip_count()).any(animated) {
            let default = rig.default_clip();
            let name = rig
                .clips()
                .get(default)
                .map(|c| c.0.clone())
                .unwrap_or_default();
            assert!(
                animated(default),
                "{} {}: default clip {default} '{name}' does not move the skeleton",
                it.code,
                it.slug
            );
        }
    }
}

/// The loader ignores a skinned mesh node's own transform (the joints already
/// place the vertices in world space, per glTF). Every library asset must keep
/// that transform identity; a future asset that does not should fail here
/// instead of rendering somewhere unexpected.
#[test]
fn skinned_mesh_nodes_have_an_identity_transform() {
    let Some(lib) = real_library() else { return; };
    let items = lib.load_manifest().expect("manifest loads");
    let mut checked = 0usize;
    for it in items
        .iter()
        .filter(|i| i.kind == "rig" && !i.glb.trim().is_empty())
    {
        let (doc, _buffers, _images) =
            gltf::import(lib.abs_path(&it.glb)).unwrap_or_else(|e| panic!("{}: {e}", it.code));
        for node in doc.nodes() {
            if node.mesh().is_none() || node.skin().is_none() {
                continue;
            }
            let (t, r, s) = node.transform().decomposed();
            let identity = t.iter().all(|v| v.abs() < 1e-6)
                && r[0].abs() + r[1].abs() + r[2].abs() < 1e-6
                && (r[3] - 1.0).abs() < 1e-6
                && s.iter().all(|v| (v - 1.0).abs() < 1e-6);
            assert!(
                identity,
                "{}: skinned mesh node {} has a non-identity transform",
                it.code,
                node.index()
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "expected at least one skinned mesh in the library");
}

/// The camera follow exists so a root-motion clip cannot walk out of frame.
/// For every rig, every sampled pose's bounds must project inside the viewport
/// the clip-invariant `Camera::fit` computes for them.
#[test]
fn every_rig_pose_stays_inside_the_camera_fit() {
    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn norm(v: [f32; 3]) -> [f32; 3] {
        let l = dot(v, v).sqrt().max(1e-12);
        [v[0] / l, v[1] / l, v[2] / l]
    }

    let Some(lib) = real_library() else { return; };
    let items = lib.load_manifest().expect("manifest loads");
    let rigs: Vec<&Item> = items
        .iter()
        .filter(|i| i.kind == "rig" && !i.glb.trim().is_empty())
        .collect();
    assert!(!rigs.is_empty());

    let aspect = 820.0f32 / 620.0;
    let tan = (45f32.to_radians() * 0.5).tan();
    for it in rigs {
        let rig = crate::rig::load(&lib.abs_path(&it.glb)).expect("loads");
        let n = rig.clip_count();
        for &clip in &[0usize, n / 2, n.saturating_sub(1)] {
            let dur = rig.clip_duration(clip);
            for k in 0..4 {
                let t = dur * 0.37 * k as f32;
                let pose = rig.pose(clip, t);
                let (target, dist) =
                    crate::raster::Camera::fit(pose.min, pose.max, 0.62, 0.20, aspect);
                let cam = crate::raster::Camera {
                    yaw: 0.62,
                    pitch: 0.20,
                    dist,
                    target,
                };
                let eye = cam.eye();
                let fwd = norm(sub(target, eye));
                let right = norm(cross(fwd, [0.0, 1.0, 0.0]));
                let up = cross(right, fwd);
                for cx in [pose.min[0], pose.max[0]] {
                    for cy in [pose.min[1], pose.max[1]] {
                        for cz in [pose.min[2], pose.max[2]] {
                            let d = sub([cx, cy, cz], eye);
                            let z = dot(d, fwd);
                            assert!(
                                z > 0.0,
                                "{} clip {clip} t={t:.2}: a corner is behind the camera",
                                it.code
                            );
                            let x = dot(d, right) / (z * tan * aspect);
                            let y = dot(d, up) / (z * tan);
                            assert!(
                                x.abs() <= 1.03 && y.abs() <= 1.03,
                                "{} clip {clip} t={t:.2}: corner ({x:.2}, {y:.2}) outside the frame",
                                it.code
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn rig_skinning_moves_vertices_between_frames() {
    let Some(rig) = real_rig() else {
        return;
    };
    let a = rig.joint_matrices(0, 0.0);
    let b = rig.joint_matrices(0, 0.5);
    assert_eq!(a.len(), b.len());
    assert!(a.len() > 60);
    let differ = a.iter().zip(b.iter()).any(|(x, y)| {
        (0..4).any(|c| (0..4).any(|r| (x[c][r] - y[c][r]).abs() > 1e-6))
    });
    assert!(differ, "clip 0 must differ between t=0.0 and t=0.5");

    let (p0, _) = rig.skin(&a);
    let (p1, _) = rig.skin(&b);
    assert_eq!(p0.len(), rig.rest_positions.len());
    let moved = p0.iter().zip(p1.iter()).any(|(x, y)| {
        let d = [x[0] - y[0], x[1] - y[1], x[2] - y[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() > 1e-4
    });
    assert!(moved, "at least one vertex must move by > 1e-4");

    let (bp, _) = rig.bind_pose();
    assert_eq!(bp.len(), rig.rest_positions.len());
}

#[test]
fn rig_clip_names_have_the_slug_prefix_stripped() {
    let Some(rig) = real_rig() else {
        return;
    };
    let clips = rig.clips();
    assert!(clips.iter().any(|(name, _)| name == "Idle"));
    assert!(clips
        .iter()
        .all(|(name, _)| !name.contains("rig_9cg_archer__")));
}

#[test]
fn rasterizer_draws_near_the_centre() {
    let pos: Vec<[f32; 3]> = vec![[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]];
    let nrm: Vec<[f32; 3]> = vec![[0.0, 0.0, 1.0]; 3];
    let idx: Vec<u32> = vec![0, 1, 2];
    let cam = crate::raster::Camera::framed([-1.0, -1.0, -0.5], [1.0, 1.0, 0.5]);
    let (w, h) = (128u32, 128u32);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    crate::raster::render(
        &mut buf,
        w,
        h,
        &pos,
        &nrm,
        &idx,
        &cam,
        None,
        &crate::raster::Overlay::default(),
    );

    let is_bg = |b: &[u8]| b[0] == 36 && b[1] == 36 && b[2] == 36;
    let drawn = buf.chunks_exact(4).filter(|p| !is_bg(p)).count();
    let pct = drawn as f64 / (w * h) as f64;
    assert!((0.02..0.98).contains(&pct), "coverage was {pct}");
    let centre = ((h / 2) * w + w / 2) as usize * 4;
    assert!(!is_bg(&buf[centre..centre + 4]), "centre pixel not drawn");
}

#[test]
fn bench_rig_frame() {
    let Some(rig) = real_rig() else { return; };
    let (mn, mx) = rig.aabb();
    let cam = crate::raster::Camera::framed(mn, mx);
    let (w, h) = (820u32, 620u32);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let frame = |buf: &mut Vec<u8>, t: f32| {
        render_rig_frame(&rig, 0, t, Some(&cam), w, h, buf, true);
    };
    frame(&mut buf, 0.1);
    frame(&mut buf, 0.2);
    let iters = 12usize;
    let t0 = std::time::Instant::now();
    for k in 0..iters {
        frame(&mut buf, 0.03 * k as f32);
    }
    let per = t0.elapsed().as_secs_f64() / iters as f64;
    println!("RIG FRAME TIME: {:.2} ms/frame at {w}x{h}", per * 1000.0);
    // A tripwire, not a benchmark: the suite runs its tests in parallel, and
    // Unity/Blender work on the same machine can starve this one completely
    // (measured: 60 ms idle vs 2.2 s at 100% load). Only a catastrophic
    // regression — e.g. re-decoding the GLB per frame — trips it.
    assert!(per < 5.0, "frame time regressed: {:.1} ms", per * 1000.0);
}

fn write_bmp(path: &std::path::Path, buf: &[u8], w: u32, h: u32) {
    let row: u32 = (w * 3 + 3) & !3u32;
    let data: u32 = row * h;
    let file_size: u32 = 54 + data;
    let mut bmp: Vec<u8> = Vec::with_capacity(file_size as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&(w as i32).to_le_bytes());
    bmp.extend_from_slice(&(h as i32).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&24u16.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&data.to_le_bytes());
    bmp.extend_from_slice(&2835i32.to_le_bytes());
    bmp.extend_from_slice(&2835i32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..h).rev() {
        let mut written: u32 = 0;
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            bmp.push(buf[i + 2]);
            bmp.push(buf[i + 1]);
            bmp.push(buf[i]);
            written += 3;
        }
        while written < row {
            bmp.push(0);
            written += 1;
        }
    }
    std::fs::write(path, &bmp).expect("write bmp");
}

#[test]
#[ignore]
fn dump_all_rig_frames_bmp() {
    let Some(lib) = real_library() else { return; };
    let items = lib.load_manifest().expect("manifest loads");
    let dir = std::env::temp_dir();
    let (w, h) = (400u32, 352u32);
    for it in items
        .iter()
        .filter(|i| i.kind == "rig" && !i.glb.trim().is_empty())
    {
        let rig = match crate::rig::load(&lib.abs_path(&it.glb)) {
            Ok(r) => r,
            Err(e) => {
                println!("SKIP {} {}: {e}", it.code, it.slug);
                continue;
            }
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let clip = rig.default_clip();
        render_rig_frame(&rig, clip, 0.0, None, w, h, &mut buf, true);
        let out = dir.join(format!("ald_rig_{}.bmp", it.code));
        write_bmp(&out, &buf, w, h);
        println!("DUMPED {} {} -> {}", it.code, it.slug, out.display());
    }
}

#[test]
#[ignore]
fn dump_rig_frame_bmp() {
    let Some(rig) = real_rig() else { return; };
    let (w, h) = (820u32, 620u32);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let clip = rig.default_clip();
    render_rig_frame(&rig, clip, 0.0, None, w, h, &mut buf, true);
    let out = std::env::temp_dir().join("ald_rig_frame.bmp");
    write_bmp(&out, &buf, w, h);
    println!("DUMPED {}", out.display());
}

/// Where the frame time actually goes: skinning vs background/floor vs mesh
/// rasterization. Timings at 820x620 (2x super-sampled internally).
#[test]
#[ignore]
fn bench_rig_parts() {
    let Some(rig) = real_rig() else { return; };
    let (mn, mx) = rig.aabb();
    let cam = crate::raster::Camera::framed(mn, mx);
    let (w, h) = (820u32, 620u32);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let iters = 8usize;

    let t0 = std::time::Instant::now();
    let mut pose = rig.pose(0, 0.0);
    for k in 0..iters {
        pose = rig.pose(0, 0.05 * k as f32);
    }
    let pose_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;

    let t1 = std::time::Instant::now();
    for _ in 0..iters {
        crate::raster::render(
            &mut buf, w, h, &pose.positions, &pose.normals, &rig.indices, &cam, None,
            &crate::raster::Overlay::default(),
        );
    }
    let flat_ms = t1.elapsed().as_secs_f64() * 1000.0 / iters as f64;

    let t2 = std::time::Instant::now();
    for k in 0..iters {
        render_rig_frame(&rig, 0, 0.05 * k as f32, Some(&cam), w, h, &mut buf, true);
    }
    let full_ms = t2.elapsed().as_secs_f64() * 1000.0 / iters as f64;
    println!("PARTS: pose={pose_ms:.1}ms  render(no floor)={flat_ms:.1}ms  full={full_ms:.1}ms");
}

/// Dump clips 0/3/17 across their duration: a limb that flies off or a clip
/// that does not deform at all shows up immediately.
#[test]
#[ignore]
fn dump_rig_motion_bmp() {
    let Some(rig) = real_rig() else { return; };
    let (w, h) = (820u32, 620u32);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let clips = rig.clips();
    for &clip in &[0usize, 3, 17] {
        let dur = rig.clip_duration(clip);
        println!(
            "CLIP {clip}: {} dur={dur:.2}s",
            clips.get(clip).map(|c| c.0.as_str()).unwrap_or("?")
        );
        for k in 0..6 {
            let t = dur * (k as f32 / 6.0);
            let pose = rig.pose(clip, t);
            let far = pose.positions.iter().fold(0.0f32, |m, v| {
                m.max(v[0].abs().max(v[1].abs()).max(v[2].abs()))
            });
            render_rig_frame(&rig, clip, t, None, w, h, &mut buf, true);
            let out = std::env::temp_dir().join(format!("ald_rig_c{clip}_f{k}.bmp"));
            write_bmp(&out, &buf, w, h);
            println!("DUMPED clip={clip} t={t:.2} max|p|={far:.2} -> {}", out.display());
        }
    }
}

#[test]
fn every_real_rig_loads_and_renders() {
    let Some(lib) = real_library() else { return; };
    let items = lib.load_manifest().expect("manifest loads");
    let rigs: Vec<&Item> = items.iter().filter(|i| i.kind == "rig").collect();
    assert_eq!(rigs.len(), 11, "11 rigs in the manifest");

    let (w, h) = (256u32, 224u32);
    let is_bg = |b: &[u8]| b[0] == 36 && b[1] == 36 && b[2] == 36;
    let mut with_glb = 0usize;

    for it in &rigs {
        if it.glb.trim().is_empty() {
            println!("rig {} {}: no .glb (fallback path)", it.code, it.slug);
            continue;
        }
        with_glb += 1;
        let path = lib.abs_path(&it.glb);
        let rig = crate::rig::load(&path)
            .unwrap_or_else(|e| panic!("{} ({}) failed to load: {e}", it.code, it.slug));
        assert!(!rig.rest_positions.is_empty(), "{} has no vertices", it.code);
        assert!(!rig.indices.is_empty(), "{} has no indices", it.code);
        if rig.clip_count() == 0 {
            println!(
                "rig {} {}: bones-only (no animations) — Preview shows the bind pose",
                it.code, it.slug
            );
        }
        assert!(rig.joint_count() > 0, "{} has no joints", it.code);

        let (mn, mx) = rig.aabb();
        assert!(
            mn.iter().chain(mx.iter()).all(|v| v.is_finite()),
            "{} has a non-finite AABB {mn:?}..{mx:?}",
            it.code
        );
        let frame_start = std::time::Instant::now();
        let clip = rig.default_clip();
        let mut buf = vec![0u8; (w * h * 4) as usize];
        // Frame the actual pose (the app's follow camera does the same): a
        // default clip may start airborne well above the bind pose.
        render_rig_frame(&rig, clip, 0.0, None, w, h, &mut buf, false);
        let frame_ms = frame_start.elapsed().as_secs_f64() * 1000.0;

        let drawn = buf.chunks_exact(4).filter(|px| !is_bg(px)).count();
        let pct = 100.0 * drawn as f64 / (w * h) as f64;
        println!(
            "rig {} {:<28} verts={:<6} tris={:<6} clips={:<3} joints={:<4} cover={:>5.1}% frame={:.1}ms",
            it.code,
            it.slug,
            rig.rest_positions.len(),
            rig.indices.len() / 3,
            rig.clip_count(),
            rig.joint_count(),
            pct,
            frame_ms
        );
        assert!(pct > 1.0, "{} rendered almost nothing ({pct:.2}%)", it.code);
        assert!(pct < 99.0, "{} filled the whole frame ({pct:.2}%)", it.code);
        // A regression tripwire, not a benchmark: at 100% machine load even a
        // 20 ms frame can measure seconds, so it only fires on a blow-up.
        assert!(frame_ms < 8000.0, "{} took {frame_ms:.0}ms for one frame", it.code);
    }

    assert_eq!(with_glb, 11, "11 rigs should carry a .glb now (RG-0009 converted)");
}

/// The sidebar's `Preview` entry renders the bind pose: with the bind palette
/// the skinning must reproduce the rest geometry exactly, for every rig.
#[test]
fn bind_palette_reproduces_the_rest_pose() {
    let Some(lib) = real_library() else { return; };
    let items = lib.load_manifest().expect("manifest loads");
    let mut checked = 0usize;
    for it in items
        .iter()
        .filter(|i| i.kind == "rig" && !i.glb.trim().is_empty())
    {
        let rig = match crate::rig::load(&lib.abs_path(&it.glb)) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let (p, _) = rig.skin(&rig.bind_matrices());
        let worst = rig
            .rest_positions
            .iter()
            .zip(&p)
            .fold(0.0f32, |m, (a, b)| {
                m.max(
                    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt(),
                )
            });
        assert!(worst < 1e-3, "{}: bind skinning drifts by {worst}", it.code);
        let pose = rig.pose_bind();
        assert_eq!(pose.positions.len(), rig.rest_positions.len());
        assert!(pose.min.iter().chain(pose.max.iter()).all(|v| v.is_finite()));
        checked += 1;
    }
    assert!(checked >= 10, "checked {checked} rigs");
}

/// Every rig's animation coverage: do the clips move the joints **and** the
/// skinned vertices? A rig whose joints move but whose vertices do not is
/// exactly "the animation is not applied to the model".
#[test]
#[ignore]
fn diag_rig_animation_coverage() {
    fn sample_indices(n: usize) -> Vec<usize> {
        let mut v = vec![0usize];
        if n > 1 {
            v.push(n / 2);
        }
        if n > 2 {
            v.push(n - 1);
        }
        v.sort_unstable();
        v.dedup();
        v
    }

    let Some(lib) = real_library() else { return; };
    let items = lib.load_manifest().expect("manifest loads");
    for it in items
        .iter()
        .filter(|i| i.kind == "rig" && !i.glb.trim().is_empty())
    {
        let path = lib.abs_path(&it.glb);
        let rig = match crate::rig::load(&path) {
            Ok(r) => r,
            Err(e) => {
                println!("{} {}: LOAD FAILED: {e}", it.code, it.slug);
                continue;
            }
        };
        let (mn, mx) = rig.aabb();

        // The GLB's own skinning attributes: a skinned mesh with no
        // JOINTS_0/WEIGHTS_0 renders frozen no matter what the clips do.
        let mut prims = 0usize;
        let mut prims_with_joints = 0usize;
        let mut prims_with_weights = 0usize;
        let mut skinned_mesh_nodes = 0usize;
        let mut wsum = (f32::INFINITY, 0.0f32);
        if let Ok((doc, buffers, _)) = gltf::import(&path) {
            let get_buf = |b: gltf::Buffer| buffers.get(b.index()).map(|x| x.0.as_slice());
            for mesh in doc.meshes() {
                for prim in mesh.primitives() {
                    prims += 1;
                    let reader = prim.reader(&get_buf);
                    if reader.read_joints(0).is_some() {
                        prims_with_joints += 1;
                    }
                    if let Some(w) = reader.read_weights(0) {
                        prims_with_weights += 1;
                        for ww in w.into_f32() {
                            let s: f32 = ww.iter().sum();
                            wsum.0 = wsum.0.min(s);
                            wsum.1 = wsum.1.max(s);
                        }
                    }
                }
            }
            for node in doc.nodes() {
                if node.mesh().is_some() && node.skin().is_some() {
                    skinned_mesh_nodes += 1;
                }
            }
        }

        println!(
            "{} {:<27} verts={:<7} tris={:<7} clips={:<3} joints={:<3} prims={prims} skinned_nodes={skinned_mesh_nodes} j={prims_with_joints} w={prims_with_weights} wsum=[{:.3},{:.3}] bind=[{:.2},{:.2},{:.2}..{:.2},{:.2},{:.2}]",
            it.code,
            it.slug,
            rig.rest_positions.len(),
            rig.indices.len() / 3,
            rig.clip_count(),
            rig.joint_count(),
            wsum.0,
            wsum.1,
            mn[0], mn[1], mn[2], mx[0], mx[1], mx[2]
        );

        let names: Vec<String> = gltf::import(&path)
            .map(|(doc, _, _)| {
                doc.nodes()
                    .map(|n| n.name().unwrap_or("<unnamed>").to_string())
                    .collect()
            })
            .unwrap_or_default();

        let clips = rig.clips();
        let dc = rig.default_clip();
        let pose0 = rig.pose(dc, 0.0);
        println!(
            "    default clip {dc} '{}' pose=[{:.1},{:.1},{:.1}..{:.1},{:.1},{:.1}] bind=[{:.1},{:.1},{:.1}..{:.1},{:.1},{:.1}]",
            clips.get(dc).map(|c| c.0.as_str()).unwrap_or("?"),
            pose0.min[0], pose0.min[1], pose0.min[2], pose0.max[0], pose0.max[1], pose0.max[2],
            mn[0], mn[1], mn[2], mx[0], mx[1], mx[2],
        );
        let mut static_clips = 0usize;
        for clip in 0..clips.len() {
            let dur = rig.clip_duration(clip);
            // Joint motion over the whole clip (cheap) — enough to tell a no-op
            // clip apart from a hold.
            let mut peak = 0.0f32;
            let base = rig.joint_matrices(clip, 0.0);
            for k in 1..=8 {
                let t = dur * k as f32 / 8.0;
                let m = rig.joint_matrices(clip, t);
                for (x, y) in base.iter().zip(&m) {
                    for c in 0..4 {
                        for r in 0..4 {
                            peak = peak.max((x[c][r] - y[c][r]).abs());
                        }
                    }
                }
            }
            if peak < 1e-5 {
                static_clips += 1;
                println!(
                    "    clip {clip:2} {:<29} dur={dur:5.2}s  STATIC (jointΔ<1e-5)",
                    clips.get(clip).map(|c| c.0.as_str()).unwrap_or("?")
                );
                for (node, path, keys, t0, t1, spread) in rig.channel_summary(clip).iter().take(8) {
                    println!(
                        "         {path} node {node:3} '{}' joint={} keys={keys} t=[{t0:.2},{t1:.2}] spread={spread:.4}",
                        names.get(*node).map(|s| s.as_str()).unwrap_or("?"),
                        rig.node_is_joint(*node)
                    );
                }
                let total = rig.channel_summary(clip).len();
                if total > 8 {
                    println!("         ... {} more channels", total - 8);
                }
            }
        }

        for &clip in &sample_indices(clips.len()) {
            let dur = rig.clip_duration(clip);
            let mid = dur * 0.4;
            let a = rig.joint_matrices(clip, 0.0);
            let b = rig.joint_matrices(clip, mid);
            let mut joint_delta = 0.0f32;
            for (x, y) in a.iter().zip(&b) {
                for c in 0..4 {
                    for r in 0..4 {
                        joint_delta = joint_delta.max((x[c][r] - y[c][r]).abs());
                    }
                }
            }
            let (pa, _) = rig.skin(&a);
            let (pb, _) = rig.skin(&b);
            let vert_delta = pa
                .iter()
                .zip(&pb)
                .fold(0.0f32, |m, (x, y)| {
                    m.max(
                        ((x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2))
                            .sqrt(),
                    )
                });
            println!(
                "    clip {clip:2} {:<30} dur={dur:5.2}s jointΔ={joint_delta:9.4} vertΔ={vert_delta:9.4}",
                clips.get(clip).map(|c| c.0.as_str()).unwrap_or("?")
            );
        }
        if static_clips > 0 {
            println!(
                "    -> {static_clips}/{} clips have no joint motion at all",
                clips.len()
            );
        }
    }
}

/// Structural check of every rig's mesh nodes: is each primitive of a skinned
/// mesh actually skinned, and is an unskinned mesh a bone attachment (a prop)
/// or a static duplicate of the body?
#[test]
#[ignore]
fn diag_rig_mesh_structure() {
    use std::collections::HashSet;
    let Some(lib) = real_library() else { return; };
    let items = lib.load_manifest().expect("manifest loads");
    for it in items
        .iter()
        .filter(|i| i.kind == "rig" && !i.glb.trim().is_empty())
    {
        let path = lib.abs_path(&it.glb);
        let Ok((doc, buffers, _)) = gltf::import(&path) else {
            continue;
        };
        let n = doc.nodes().count();
        let mut parents: Vec<Option<usize>> = vec![None; n];
        for node in doc.nodes() {
            for child in node.children() {
                parents[child.index()] = Some(node.index());
            }
        }
        let joints: HashSet<usize> = doc
            .skins()
            .flat_map(|s| s.joints().map(|j| j.index()).collect::<Vec<_>>())
            .collect();
        let under_joint = |mut idx: usize| -> bool {
            for _ in 0..64 {
                match parents[idx] {
                    Some(p) => {
                        if joints.contains(&p) {
                            return true;
                        }
                        idx = p;
                    }
                    None => return false,
                }
            }
            false
        };
        println!("{} {}", it.code, it.slug);
        for node in doc.nodes() {
            let Some(mesh) = node.mesh() else { continue };
            let get_buf = |b: gltf::Buffer| buffers.get(b.index()).map(|x| x.0.as_slice());
            let mut verts = 0usize;
            let mut prims = 0usize;
            let mut with_j = 0usize;
            for prim in mesh.primitives() {
                prims += 1;
                let reader = prim.reader(&get_buf);
                verts += reader.read_positions().map(|p| p.count()).unwrap_or(0);
                if reader.read_joints(0).is_some() {
                    with_j += 1;
                }
            }
            let (t, _, _) = node.transform().decomposed();
            println!(
                "   node {:3} '{}' skin={:?} prims={prims} verts={verts} skinned_prims={with_j} under_joint={} t=({:.1},{:.1},{:.1})",
                node.index(),
                node.name().unwrap_or("<unnamed>"),
                node.skin().map(|s| s.index()),
                under_joint(node.index()),
                t[0], t[1], t[2],
            );
        }
    }
}

#[test]
fn real_library_filters_and_reads_files() {
    let Some(lib) = real_library() else {
        return;
    };
    let items = lib.load_manifest().expect("manifest loads");

    let tax = lib.taxonomy_json().expect("taxonomy.json readable");
    let taxonomy = build_taxonomy(&items, Some(&tax));
    let mut s = AppState::new(items, Vec::new(), taxonomy);
    assert_eq!(s.kind_count(Kind::Model), 1913);

    s.l1 = "Architecture".into();
    s.l2 = "Beam".into();
    assert_eq!(s.filtered().len(), 132);

    s.reset_filters();
    s.q = "rig_9cg_archer".into();
    s.set_tab(Kind::Rig);
    assert_eq!(s.filtered().len(), 1);

    assert!(lib.read_text("_Catalog/taxonomy.json").is_ok());
    assert!(lib.read_text("_Catalog/nope-missing.json").is_err());
}

#[test]
fn drag_target_shifts_both_axes_by_the_cursor_delta() {
    assert_eq!(
        crate::drag_target((100, 100), (500, 400), (620, 480)),
        (220, 180)
    );
}

#[test]
fn drag_target_is_idempotent_for_the_same_cursor() {
    let anchor_window = (100, 100);
    let anchor_cursor = (500, 400);
    let cursor = (620, 480);
    let first = crate::drag_target(anchor_window, anchor_cursor, cursor);
    for _ in 0..5 {
        assert_eq!(crate::drag_target(anchor_window, anchor_cursor, cursor), first);
    }
}

#[test]
fn drag_target_zero_delta_is_a_no_op() {
    assert_eq!(
        crate::drag_target((100, 100), (500, 400), (500, 400)),
        (100, 100)
    );
}

/// The backdrop policy, in one place: the screen is **never** photographed
/// while the window moves (that was the lag); it is photographed once the
/// window has been still for a moment and refreshed at that cadence
/// afterwards; a blur change re-blurs the stored shot; a resample (or a
/// display change) forces one capture.
#[cfg(windows)]
#[test]
fn glass_refreshes_while_idle_and_never_while_moving() {
    use crate::GlassWork::{Capture, Reblur};
    // Startup: no shot yet, window already still -> capture.
    assert_eq!(crate::glass_work(false, false, true, false, false), Capture);
    // Moving: never, not even for a resample.
    assert_eq!(
        crate::glass_work(true, true, true, false, false),
        crate::GlassWork::None
    );
    assert_eq!(
        crate::glass_work(true, true, false, false, true),
        crate::GlassWork::None
    );
    // Still and due: refresh.
    assert_eq!(crate::glass_work(true, false, true, false, false), Capture);
    // Still but not due yet: nothing (the loop stays idle).
    assert_eq!(
        crate::glass_work(true, false, false, false, false),
        crate::GlassWork::None
    );
    // Blur changed: re-blur the stored pixels, no screen access.
    assert_eq!(crate::glass_work(true, false, false, true, false), Reblur);
    // Blur changed with no shot (e.g. straight after blur 0 came back): capture.
    assert_eq!(crate::glass_work(false, false, false, true, false), Capture);
    // Resample: a real capture, and it wins over a pending re-blur.
    assert_eq!(crate::glass_work(true, false, false, false, true), Capture);
    assert_eq!(crate::glass_work(true, false, false, true, true), Capture);
}

/// Moving the window re-registers the stored capture instead of replacing
/// it: the same image is sampled from a different offset, so the blurred
/// desktop stays glued to the desktop and the image does not slide along with
/// the window. Multi-monitor works because the capture is the whole virtual
/// desktop and its origin can be negative.
#[cfg(windows)]
#[test]
fn the_desktop_capture_follows_the_window_without_a_new_capture() {
    let shot = (1920usize, 1080usize);
    let a = crate::backdrop_placement((0, 0), (300, 60), shot, 1.0);
    let b = crate::backdrop_placement((0, 0), (500, 60), shot, 1.0);
    assert!(
        (a.0 - -300.0).abs() < 1e-6 && (a.1 - -60.0).abs() < 1e-6,
        "the image sits at minus the window origin"
    );
    assert!(
        (b.0 - a.0 - (-200.0)).abs() < 1e-6,
        "moving 200px right samples 200px further left in the image"
    );
    assert_eq!(
        (a.2, a.3),
        (b.2, b.3),
        "a move does not change the image size"
    );
    assert!(
        (a.2 - 1920.0).abs() < 1e-6 && (a.3 - 1080.0).abs() < 1e-6,
        "the image is the whole capture in logical pixels"
    );

    // A capture not taken at the virtual-screen origin shifts the image with
    // it: only the difference between the two origins matters.
    let offset = crate::backdrop_placement((10, 20), (300, 60), shot, 1.0);
    assert!((offset.0 - -290.0).abs() < 1e-6 && (offset.1 - -40.0).abs() < 1e-6);

    // Monitors above / left of the primary push the virtual-screen origin
    // negative (this desk: second screen at y = -71). Placement is a
    // difference, so negative origins stay correct.
    let above = crate::backdrop_placement((0, -71), (380, 120), shot, 1.0);
    assert!(
        (above.0 - -380.0).abs() < 1e-6 && (above.1 - -191.0).abs() < 1e-6,
        "a capture origin above the primary shifts the image down"
    );
    let left = crate::backdrop_placement((-1920, 0), (-1700, 100), shot, 1.0);
    assert!(
        (left.0 - -220.0).abs() < 1e-6 && (left.1 - -100.0).abs() < 1e-6,
        "a window on a screen left of the primary lands inside the image"
    );

    // High-DPI: the origins are physical, the placement logical.
    let hidpi = crate::backdrop_placement((0, 0), (300, 60), shot, 2.0);
    assert!((hidpi.0 - -150.0).abs() < 1e-6);

    // A degenerate scale must not produce NaN or a collapsed image.
    let degenerate = crate::backdrop_placement((0, 0), (300, 60), shot, 0.0);
    assert!(degenerate.0.is_finite() && degenerate.2.is_finite() && degenerate.2 > 0.0);
}

#[test]
fn deepseek_tag_prompt_carries_taxonomy_and_asset() {
    // The prompt is what the model sees; it must name the taxonomy, the asset and
    // the exact JSON contract, otherwise JSON mode tends to drift.
    let item = crate::model::Item {
        kind: "model".into(),
        code: "AU-0001".into(),
        slug: "old_well".into(),
        tri_count: 1234,
        tags: vec!["stone".into()],
        dimensions_m: Some(crate::model::Dims {
            width: 1.4,
            height: 1.1,
            depth: 1.5,
        }),
        ..Default::default()
    };
    let mut by_kind = std::collections::BTreeMap::new();
    let mut prop = std::collections::BTreeMap::new();
    prop.insert("Container".to_string(), vec!["Barrel".to_string()]);
    by_kind.insert("model".to_string(), prop);
    let taxonomy = crate::state::Taxonomy { by_kind };

    let prompt = crate::tag_prompt(&item, &taxonomy, &["stone".to_string()]);
    assert!(prompt.contains("Container"));
    assert!(prompt.contains("old_well"));
    assert!(prompt.contains("1.40x1.10x1.50 m"));
    assert!(prompt.contains("\"confidence\""));
}
