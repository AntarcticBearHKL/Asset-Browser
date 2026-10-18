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
    crate::raster::render(&mut buf, w, h, &pos, &nrm, &idx, &cam);

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
    let (w, h) = (512u32, 448u32);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let frame = |buf: &mut Vec<u8>, t: f32| {
        let jm = rig.joint_matrices(0, t);
        let (p, n) = rig.skin(&jm);
        crate::raster::render(buf, w, h, &p, &n, &rig.indices, &cam);
    };
    frame(&mut buf, 0.1);
    frame(&mut buf, 0.2);
    let iters = 20usize;
    let t0 = std::time::Instant::now();
    for k in 0..iters {
        frame(&mut buf, 0.01 * k as f32);
    }
    let per = t0.elapsed().as_secs_f64() / iters as f64;
    println!("RIG FRAME TIME: {:.2} ms/frame at {w}x{h}", per * 1000.0);
    assert!(per < 0.5, "frame time regressed: {:.1} ms", per * 1000.0);
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
        let (mn, mx) = rig.aabb();
        let cam = crate::raster::Camera::framed(mn, mx);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let jm = rig.joint_matrices(0, 0.0);
        let (p, n) = rig.skin(&jm);
        crate::raster::render(&mut buf, w, h, &p, &n, &rig.indices, &cam);
        let out = dir.join(format!("ald_rig_{}.bmp", it.code));
        write_bmp(&out, &buf, w, h);
        println!("DUMPED {} {} -> {}", it.code, it.slug, out.display());
    }
}

#[test]
#[ignore]
fn dump_rig_frame_bmp() {
    let Some(rig) = real_rig() else { return; };
    let (mn, mx) = rig.aabb();
    let cam = crate::raster::Camera::framed(mn, mx);
    let (w, h) = (512u32, 448u32);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let jm = rig.joint_matrices(0, 0.0);
    let (p, n) = rig.skin(&jm);
    crate::raster::render(&mut buf, w, h, &p, &n, &rig.indices, &cam);
    let out = std::env::temp_dir().join("ald_rig_frame.bmp");
    write_bmp(&out, &buf, w, h);
    println!("DUMPED {}", out.display());
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
        assert!(rig.clip_count() > 0, "{} has no clips", it.code);
        assert!(rig.joint_count() > 0, "{} has no joints", it.code);

        let (mn, mx) = rig.aabb();
        assert!(
            mn.iter().chain(mx.iter()).all(|v| v.is_finite()),
            "{} has a non-finite AABB {mn:?}..{mx:?}",
            it.code
        );
        let cam = crate::raster::Camera::framed(mn, mx);

        let frame_start = std::time::Instant::now();
        let jm = rig.joint_matrices(0, 0.0);
        let (p, n) = rig.skin(&jm);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        crate::raster::render(&mut buf, w, h, &p, &n, &rig.indices, &cam);
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
        // A regression tripwire, not a benchmark: the three 1M+ triangle rigs
        // legitimately cost ~200-242ms/frame, and a loaded machine can roughly
        // double that. The bound is loose on purpose so it only fires on a real
        // blow-up (e.g. re-decoding per frame); `--nocapture` prints the timing.
        assert!(frame_ms < 2000.0, "{} took {frame_ms:.0}ms for one frame", it.code);
    }

    assert_eq!(with_glb, 10, "10 of the 11 rigs should carry a .glb");
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
