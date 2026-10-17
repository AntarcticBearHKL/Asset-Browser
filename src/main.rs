//! Application entry point: Slint wiring, every `ui.on_*` callback, the worker
//! threads and the 15s bridge-status timer.
//!
//! Invariant: nothing blocking runs on the UI thread — workers post results back
//! with `slint::invoke_from_event_loop` (the shared `Rc<RefCell<App>>` is `!Send`,
//! so the `Send` `Weak<AppWindow>` is reached via a thread-local).

// GUI subsystem on Windows so launching the app never opens a console window.
// `not(test)` keeps the test harness's stdout working.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod model;
mod state;
mod library;
mod store;
mod export;
mod bridge;
mod rig;
mod raster;

#[cfg(test)]
mod tests;

slint::include_modules!();

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel, Weak};

use bridge::Dot;
use library::Library;
use model::{preview_badge, slide_label, spec_pairs, Dims, Item, Kind};
use state::{build_taxonomy, page_size_options, sort_options, style_options, AppState};

/// Asset library root (fixed data source, see PORT_SPEC §1.4).
pub const ROOT: &str = r"D:\3D Resource";
/// Manifest path kept for parity with the web app / spec.
pub const MANIFEST: &str = r"D:\3D Resource\_Catalog\manifest.json";

/* ----------------------------- app state -------------------------------- */

struct EditState {
    kind: Kind,
    l1: String,
    l2: String,
    tags: Vec<String>,
    /// Signature `[l1, l2, sorted tags]` captured when the dialog opened.
    base: String,
    l1_options: Vec<String>,
    l2_options: Vec<String>,
    busy: bool,
}

/// Fixed size of the software-rendered rig frame (see `RIG_VIEWER_PLAN.md`).
const RIG_W: u32 = 512;
const RIG_H: u32 = 448;

/// Live rig state while the 3D stage is on screen.
struct RigSession {
    rig: Arc<rig::Rig>,
    camera: raster::Camera,
    aabb_min: [f32; 3],
    aabb_max: [f32; 3],
    clip: usize,
    time: f32,
    duration: f32,
    playing: bool,
    dirty: bool,
    pending: bool,
    w: u32,
    h: u32,
}

struct App {
    lib: Library,
    state: AppState,
    store: store::Store,
    items: Rc<VecModel<AssetItem>>,
    cart_model: Rc<VecModel<CartRow>>,
    slides_model: Rc<VecModel<SlideItem>>,
    /// 0 = not decoded, 1 = queued, 2 = decoded/failed (per item index).
    thumb_state: Vec<u8>,
    selected: Option<usize>,
    edit: Option<EditState>,
    delete_armed: bool,
    rig: Option<RigSession>,
    toast_timer: Timer,
    delete_timer: Timer,
    probe_timer: Timer,
    rig_timer: Timer,
}

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = RefCell::new(None);
}

/// Run `f` with the shared app on the UI thread. Returns `None` when the app is
/// missing or already mutably borrowed (never panics).
fn app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        let borrowed = cell.borrow();
        match borrowed.as_ref() {
            Some(rc) => rc.try_borrow_mut().ok().map(|mut guard| f(&mut guard)),
            None => None,
        }
    })
}

/* ------------------------------ helpers --------------------------------- */

fn to_string_model(values: &[String]) -> ModelRc<slint::SharedString> {
    let v: Vec<slint::SharedString> = values
        .iter()
        .map(|s| slint::SharedString::from(s.as_str()))
        .collect();
    ModelRc::new(VecModel::from(v))
}

fn to_int_model(values: &[i32]) -> ModelRc<i32> {
    ModelRc::new(VecModel::from(values.to_vec()))
}

fn empty_string_model() -> ModelRc<slint::SharedString> {
    ModelRc::new(VecModel::from(Vec::<slint::SharedString>::new()))
}

fn index_in(list: &[String], value: &str) -> i32 {
    list.iter()
        .position(|x| x == value)
        .map(|i| i as i32)
        .unwrap_or(-1)
}

fn asset_row(it: &Item) -> AssetItem {
    AssetItem {
        code: it.code.clone().into(),
        slug: it.slug.clone().into(),
        thumb: slint::Image::default(),
        tag1: it.tags.first().cloned().unwrap_or_default().into(),
        tag2: it.tags.get(1).cloned().unwrap_or_default().into(),
        tag3: it.tags.get(2).cloned().unwrap_or_default().into(),
        in_cart: false,
        kind: it.kind().num(),
    }
}

/// Absolute path of an item's preview PNG (falls back to `{path}/{slug}.png`).
fn preview_abs(a: &App, it: &Item) -> String {
    let rel = if it.preview.trim().is_empty() {
        format!("{}/{}.png", it.path(), it.slug)
    } else {
        it.preview.trim().to_string()
    };
    a.lib.abs_path(&rel)
}

fn slides_of(it: &Item) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let preview = it.preview.trim();
    if !preview.is_empty() {
        out.push(("Preview".to_string(), preview.to_string()));
    }
    for tex in &it.textures {
        let rel = tex.trim();
        if !rel.is_empty() {
            out.push((slide_label(rel), rel.to_string()));
        }
    }
    out
}

fn detail_badge(it: &Item) -> String {
    match it.kind() {
        Kind::Rig => {
            if it.glb.is_empty() {
                "No preview bundle".to_string()
            } else {
                format!("{} clips", model::format_int(it.clip_count))
            }
        }
        _ => preview_badge(it),
    }
}

fn signature(l1: &str, l2: &str, tags: &[String]) -> String {
    let mut sorted: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    format!("{l1}\u{1}{l2}\u{1}{}", sorted.join("\u{1}"))
}

fn edit_dirty(a: &App) -> bool {
    match &a.edit {
        Some(e) => e.base != signature(&e.l1, &e.l2, &e.tags),
        None => false,
    }
}

fn cart_items(a: &App) -> Vec<Item> {
    a.state
        .cart_indices()
        .iter()
        .map(|&i| a.state.items[i].clone())
        .collect()
}

fn current_item(a: &App) -> Option<Item> {
    a.selected.and_then(|i| a.state.items.get(i).cloned())
}

fn persist(store: &store::Store, cart: &[String]) {
    let mut next = store.clone();
    next.cart = cart.to_vec();
    store::save(&next);
}

fn refresh_cart_flags(a: &App, indices: &[usize]) {
    for &i in indices {
        if i >= a.state.items.len() {
            continue;
        }
        let want = a.state.in_cart(a.state.items[i].code.as_str());
        if let Some(mut row) = a.items.row_data(i) {
            if row.in_cart != want {
                row.in_cart = want;
                a.items.set_row_data(i, row);
            }
        }
    }
}

fn decode_to_buffer(path: &str) -> Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> {
    let image = slint::Image::load_from_path(std::path::Path::new(path)).ok()?;
    image.to_rgba8_premultiplied()
}

/* --------------------------- UI synchronisation ------------------------- */

/// Up to `page_size` filtered indices starting at `start`. Empty past the end.
fn page_slice(all: &[usize], start: usize, page_size: usize) -> &[usize] {
    if start >= all.len() {
        return &[];
    }
    let end = (start + page_size).min(all.len());
    &all[start..end]
}

/// Chunk a page's global indices into `GridRow`s of at most six cards.
fn rows_from_slice(slice: &[usize]) -> Vec<GridRow> {
    let mut rows: Vec<GridRow> = Vec::with_capacity(slice.len() / 6 + 1);
    let mut k = 0usize;
    while k < slice.len() {
        let remaining = slice.len() - k;
        let slot = |offset: usize| -> i32 {
            if remaining > offset {
                slice[k + offset] as i32
            } else {
                -1
            }
        };
        rows.push(GridRow {
            a: slice[k] as i32,
            b: slot(1),
            c: slot(2),
            d: slot(3),
            e: slot(4),
            f: slot(5),
            n: remaining.min(6) as i32,
        });
        k += 6;
    }
    rows
}

fn rebuild_rows(ui: &AppWindow, a: &App) {
    let all = a.state.filtered();
    let slice = page_slice(&all, a.state.first_item_on_page(), a.state.page_size);
    ui.set_rows(ModelRc::new(VecModel::from(rows_from_slice(slice))));
}

fn rebuild_cart(ui: &AppWindow, a: &App) {
    let rows: Vec<CartRow> = a
        .state
        .cart_indices()
        .iter()
        .map(|&i| {
            let it = &a.state.items[i];
            CartRow {
                code: it.code.clone().into(),
                slug: it.slug.clone().into(),
                category: it.category.clone().into(),
                idx: i as i32,
                thumb: slint::Image::default(),
            }
        })
        .collect();
    a.cart_model.set_vec(rows);
    ui.set_cart(ModelRc::new(a.cart_model.clone()));
    ui.set_cart_count(a.state.cart.len() as i32);
}

fn sync_view(ui: &AppWindow, a: &App) {
    ui.set_count_label(a.state.count_label().into());
    ui.set_filters_active(a.state.filters_active());
    ui.set_page(a.state.page as i32);
    ui.set_page_count(a.state.page_count() as i32);
    ui.set_page_size(a.state.page_size as i32);
    rebuild_rows(ui, a);
}

fn sync_all(ui: &AppWindow, a: &App) {
    ui.set_tab(a.state.tab.num());
    ui.set_count_models(a.state.kind_count(Kind::Model) as i32);
    ui.set_count_materials(a.state.kind_count(Kind::Material) as i32);
    ui.set_count_rigs(a.state.kind_count(Kind::Rig) as i32);

    // "All" is an explicit option at index 0 (mirrors the web's
    // `<option value="">All categories</option>`). Using -1 for "all" is
    // unreliable: Slint's ComboBox clamps a negative `current-index` to 0 and
    // fires `changed`, which would silently apply the first category as a filter.
    let l1 = a.state.l1_options();
    let l2 = a.state.l2_options();

    let mut l1_disp: Vec<String> = Vec::with_capacity(l1.len() + 1);
    l1_disp.push("All categories".to_string());
    l1_disp.extend(l1.iter().cloned());
    ui.set_l1_options(to_string_model(&l1_disp));

    let l1_index = if a.state.l1.is_empty() {
        0
    } else {
        index_in(&l1, &a.state.l1) + 1
    };

    let (l2_model, l2_index) = if a.state.l1.is_empty() {
        (empty_string_model(), -1)
    } else {
        let mut l2_disp: Vec<String> = Vec::with_capacity(l2.len() + 1);
        l2_disp.push("All subcategories".to_string());
        l2_disp.extend(l2.iter().cloned());
        let idx = if a.state.l2.is_empty() {
            0
        } else {
            index_in(&l2, &a.state.l2) + 1
        };
        (to_string_model(&l2_disp), idx)
    };

    ui.set_l2_options(l2_model);
    ui.set_l1_index(l1_index);
    ui.set_l2_index(l2_index);
    ui.set_sort_options(to_string_model(&sort_options()));
    ui.set_style_options(to_string_model(&style_options()));
    ui.set_page_sizes(to_int_model(&page_size_options()));
    ui.set_page_size_labels(to_string_model(
        &page_size_options().iter().map(|n| n.to_string()).collect::<Vec<_>>(),
    ));
    ui.set_sort_index(a.state.sort.index());
    ui.set_style_index(if a.state.style_textured_only { 1 } else { 0 });

    sync_view(ui, a);
    rebuild_cart(ui, a);
}

fn show_toast(ui: &AppWindow, a: &mut App, message: &str, tone: &str) {
    ui.set_toast_text(message.into());
    ui.set_toast_tone(tone.into());
    ui.set_toast_open(true);
    let weak = ui.as_weak();
    a.toast_timer.start(
        TimerMode::SingleShot,
        Duration::from_millis(2600),
        move || {
            if let Some(u) = weak.upgrade() {
                u.set_toast_open(false);
            }
        },
    );
}

/* ------------------------ detail dialog (open / edit) ------------------- */

fn setup_edit(ui: &AppWindow, a: &mut App, it: &Item, kind: Kind) {
    let l1_options = a.state.taxonomy.l1_for(kind);
    let l1 = if l1_options.iter().any(|x| x == &it.l1) {
        it.l1.clone()
    } else {
        l1_options.first().cloned().unwrap_or_default()
    };
    let l2_options = a.state.taxonomy.l2_for(kind, &l1);
    let l2 = if l2_options.iter().any(|x| x == &it.l2) {
        it.l2.clone()
    } else {
        l2_options.first().cloned().unwrap_or_default()
    };
    let tags = it.tags.clone();
    let base = signature(&l1, &l2, &tags);

    ui.set_edit_l1_options(to_string_model(&l1_options));
    ui.set_edit_l2_options(to_string_model(&l2_options));
    ui.set_edit_l1_index(index_in(&l1_options, &l1));
    ui.set_edit_l2_index(index_in(&l2_options, &l2));
    ui.set_edit_tags(to_string_model(&tags));

    a.edit = Some(EditState {
        kind,
        l1,
        l2,
        tags,
        base,
        l1_options,
        l2_options,
        busy: false,
    });
}

fn open_detail(ui: &AppWindow, a: &mut App, idx: usize) {
    if idx >= a.state.items.len() {
        return;
    }
    a.selected = Some(idx);
    let it = a.state.items[idx].clone();
    let kind = it.kind();

    a.edit = None;
    a.delete_armed = false;
    ui.set_delete_armed(false);
    ui.set_edit_busy(false);
    ui.set_edit_dirty(false);
    ui.set_edit_tag_input("".into());
    ui.set_rig_clip_index(0);
    ui.set_rig_playing(false);
    ui.set_rig_time("0.00 / 0.00s".into());
    ui.set_clips(ModelRc::new(VecModel::from(Vec::<ClipItem>::new())));

    stop_rig(ui, a);
    if kind == Kind::Rig {
        let glb = it.glb.trim().to_string();
        if !glb.is_empty() {
            let path = a.lib.abs_path(&glb);
            spawn_rig_load(ui.as_weak(), idx, path);
        }
    }

    ui.set_detail_code(it.code.clone().into());
    ui.set_detail_slug(it.slug.clone().into());
    ui.set_detail_category(it.category.clone().into());
    ui.set_detail_kind(kind.as_str().into());
    ui.set_detail_badge(detail_badge(&it).into());

    let spec: Vec<SpecRow> = spec_pairs(&it)
        .into_iter()
        .map(|(k, v)| SpecRow {
            k: k.into(),
            v: v.into(),
        })
        .collect();
    ui.set_spec(ModelRc::new(VecModel::from(spec)));

    let slides = slides_of(&it);
    let mut slide_rows: Vec<SlideItem> = Vec::with_capacity(slides.len());
    for (i, (label, rel)) in slides.iter().enumerate() {
        slide_rows.push(SlideItem {
            label: label.as_str().into(),
            rel: rel.as_str().into(),
            thumb: slint::Image::default(),
            active: i == 0,
        });
    }
    a.slides_model.set_vec(slide_rows);
    ui.set_slides(ModelRc::new(a.slides_model.clone()));

    let existing = a.items.row_data(idx).map(|r| r.thumb).unwrap_or_default();
    if existing == slint::Image::default() {
        ui.set_detail_thumb(slint::Image::default());
        if a.thumb_state.get(idx).copied().unwrap_or(1) != 1 {
            a.thumb_state[idx] = 1;
            spawn_item_thumb(ui.as_weak(), idx, preview_abs(a, &it));
        }
    } else {
        ui.set_detail_thumb(existing);
    }

    if slides.len() > 1 {
        let abs: Vec<String> = slides.iter().map(|(_, rel)| a.lib.abs_path(rel)).collect();
        spawn_slide_thumbs(ui.as_weak(), abs);
    }

    setup_edit(ui, a, &it, kind);
    ui.set_detail_open(true);
}

/// Merge the fields the web app merges on per-asset refresh.
fn merge_refresh(it: &mut Item, json: &serde_json::Value) {
    if let Some(dims) = json.get("dimensions_m") {
        if let Ok(parsed) = serde_json::from_value::<Option<Dims>>(dims.clone()) {
            it.dimensions_m = parsed;
        }
    }
    if let Some(arr) = json.get("textures").and_then(|x| x.as_array()) {
        let textures: Vec<String> = arr
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        it.n_tex = textures.len() as i64;
        it.textures = textures;
    }
    if let Some(n) = json.get("tri_count").and_then(|x| x.as_i64()) {
        it.tri_count = n;
    }
    if let Some(s) = json.get("subtype").and_then(|x| x.as_str()) {
        it.subtype = Some(s.to_string());
    }
    if let Some(arr) = json.get("resolution").and_then(|x| x.as_array()) {
        it.resolution = Some(arr.iter().filter_map(|x| x.as_i64()).collect());
    }
    if let Some(n) = json.get("n_channels").and_then(|x| x.as_i64()) {
        it.n_channels = n;
    }
    it.untextured = json
        .get("untextured")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    if let Some(d) = json.get("description").and_then(|x| x.as_str()) {
        it.description = Some(d.to_string());
    }
    if let Some(arr) = json.get("tags").and_then(|x| x.as_array()) {
        it.tags = arr
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
    }
}

/* --------------------------- background work ---------------------------- */

fn spawn_item_thumb(ui: Weak<AppWindow>, idx: usize, path: String) {
    std::thread::spawn(move || {
        let buffer = decode_to_buffer(&path);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(u) = ui.upgrade() else {
                return;
            };
            match buffer {
                Some(buffer) => {
                    let image = slint::Image::from_rgba8_premultiplied(buffer);
                    app(|a| {
                        if let Some(mut row) = a.items.row_data(idx) {
                            row.thumb = image.clone();
                            a.items.set_row_data(idx, row);
                        }
                        if idx < a.thumb_state.len() {
                            a.thumb_state[idx] = 2;
                        }
                        if a.selected == Some(idx) {
                            u.set_detail_thumb(image.clone());
                        }
                    });
                }
                None => {
                    app(|a| {
                        if idx < a.thumb_state.len() {
                            a.thumb_state[idx] = 2;
                        }
                    });
                }
            }
        });
    });
}

fn spawn_slide_thumbs(ui: Weak<AppWindow>, paths: Vec<String>) {
    std::thread::spawn(move || {
        for (index, path) in paths.into_iter().enumerate() {
            let buffer = decode_to_buffer(&path);
            let ui = ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(u) = ui.upgrade() else {
                    return;
                };
                if let Some(buffer) = buffer {
                    let image = slint::Image::from_rgba8_premultiplied(buffer);
                    app(|a| {
                        if let Some(mut row) = a.slides_model.row_data(index) {
                            row.thumb = image.clone();
                            a.slides_model.set_row_data(index, row);
                        }
                    });
                }
                let _ = u;
            });
        }
    });
}

fn spawn_cart_thumbs(ui: Weak<AppWindow>, entries: Vec<(usize, String)>) {
    std::thread::spawn(move || {
        for (row, path) in entries {
            let buffer = decode_to_buffer(&path);
            let ui = ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(_u) = ui.upgrade() else {
                    return;
                };
                if let Some(buffer) = buffer {
                    let image = slint::Image::from_rgba8_premultiplied(buffer);
                    app(|a| {
                        if let Some(mut r) = a.cart_model.row_data(row) {
                            r.thumb = image.clone();
                            a.cart_model.set_row_data(row, r);
                        }
                    });
                }
            });
        }
    });
}

/* ------------------------------- rig stage ------------------------------ */

fn rig_time_text(time: f32, duration: f32) -> String {
    format!("{time:.2} / {duration:.2}s")
}

fn stop_rig(ui: &AppWindow, a: &mut App) {
    a.rig = None;
    a.rig_timer.stop();
    ui.set_rig_has_3d(false);
    ui.set_rig_playing(false);
    ui.set_detail_frame(slint::Image::default());
    ui.set_rig_seek_pos(0.0);
    ui.set_rig_duration(0.0);
}

fn spawn_rig_render(
    ui: Weak<AppWindow>,
    rig: Arc<rig::Rig>,
    cam: raster::Camera,
    clip: usize,
    time: f32,
    w: u32,
    h: u32,
) {
    std::thread::spawn(move || {
        let joints = rig.joint_matrices(clip, time);
        let (pos, nrm) = rig.skin(&joints);
        let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
        raster::render(buffer.make_mut_bytes(), w, h, &pos, &nrm, &rig.indices, &cam);
        let done = rig.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(u) = ui.upgrade() else {
                return;
            };
            u.set_detail_frame(slint::Image::from_rgba8(buffer));
            app(|a| {
                if let Some(s) = a.rig.as_mut() {
                    if Arc::ptr_eq(&s.rig, &done) {
                        s.pending = false;
                    }
                }
            });
        });
    });
}

fn spawn_rig_load(ui: Weak<AppWindow>, idx: usize, path: String) {
    std::thread::spawn(move || {
        let result = rig::load(&path);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(u) = ui.upgrade() else {
                return;
            };
            app(|a| {
                if a.selected != Some(idx) {
                    return;
                }
                match result {
                    Ok(loaded) => {
                        let (mn, mx) = loaded.aabb();
                        let cam = raster::Camera::framed(mn, mx);
                        let rig = Arc::new(loaded);
                        let clips = rig.clips();
                        let duration = clips.first().map(|c| c.1).unwrap_or(0.0);
                        let rows: Vec<ClipItem> = clips
                            .iter()
                            .map(|(name, dur)| ClipItem {
                                name: name.as_str().into(),
                                dur: format!("{dur:.1}s").into(),
                            })
                            .collect();
                        u.set_clips(ModelRc::new(VecModel::from(rows)));
                        u.set_rig_clip_index(0);
                        u.set_rig_has_3d(true);
                        u.set_rig_playing(true);
                        u.set_rig_duration(duration);
                        u.set_rig_seek_pos(0.0);
                        u.set_rig_time(rig_time_text(0.0, duration).into());
                        a.rig = Some(RigSession {
                            rig: rig.clone(),
                            camera: cam,
                            aabb_min: mn,
                            aabb_max: mx,
                            clip: 0,
                            time: 0.0,
                            duration,
                            playing: true,
                            dirty: false,
                            pending: true,
                            w: RIG_W,
                            h: RIG_H,
                        });
                        let weak = u.as_weak();
                        a.rig_timer.start(
                            TimerMode::Repeated,
                            Duration::from_millis(33),
                            move || rig_tick(weak.clone()),
                        );
                        spawn_rig_render(u.as_weak(), rig, cam, 0, 0.0, RIG_W, RIG_H);
                    }
                    Err(error) => {
                        a.rig = None;
                        a.rig_timer.stop();
                        u.set_rig_has_3d(false);
                        u.set_rig_playing(false);
                        u.set_detail_badge(format!("Animation error: {error}").into());
                    }
                }
            });
        });
    });
}

fn rig_tick(ui: Weak<AppWindow>) {
    let Some(u) = ui.upgrade() else {
        return;
    };
    app(|a| {
        let Some(s) = a.rig.as_mut() else {
            return;
        };
        if s.playing && s.duration > 0.0 {
            s.time += 0.033;
            while s.time >= s.duration {
                s.time -= s.duration;
            }
            s.dirty = true;
        }
        if !s.dirty || s.pending {
            return;
        }
        s.dirty = false;
        s.pending = true;
        let (cam, clip, time, w, h) = (s.camera, s.clip, s.time, s.w, s.h);
        let rig = s.rig.clone();
        let duration = s.duration;
        u.set_rig_seek_pos(time);
        u.set_rig_time(rig_time_text(time, duration).into());
        spawn_rig_render(u.as_weak(), rig, cam, clip, time, w, h);
    });
}

fn spawn_probe(ui: Weak<AppWindow>, which: String, base: String, remote: String) {
    std::thread::spawn(move || {
        let dot = match which.as_str() {
            "blender" => bridge::blender_health(&base),
            "unity" => bridge::unity_health(&base),
            "painter" => bridge::painter_health(&base, &remote),
            _ => Dot::default(),
        };
        let _ = slint::invoke_from_event_loop(move || {
            let Some(u) = ui.upgrade() else {
                return;
            };
            apply_dot(&u, &which, &dot);
        });
    });
}

fn apply_dot(ui: &AppWindow, which: &str, dot: &Dot) {
    match which {
        "blender" => {
            ui.set_blender_state(dot.state.as_str().into());
            ui.set_blender_status(dot.text.as_str().into());
        }
        "unity" => {
            ui.set_unity_state(dot.state.as_str().into());
            ui.set_unity_status(dot.text.as_str().into());
        }
        _ => {
            ui.set_painter_state(dot.state.as_str().into());
            ui.set_painter_status(dot.text.as_str().into());
        }
    }
}

fn probe_all(ui: Weak<AppWindow>) {
    let config = app(|a| {
        (
            a.store.blender_base.clone(),
            a.store.unity_base.clone(),
            a.store.painter_bridge.clone(),
            a.store.painter_base.clone(),
        )
    });
    let Some((blender_base, unity_base, painter_bridge, painter_remote)) = config else {
        return;
    };
    if let Some(u) = ui.upgrade() {
        u.set_blender_state("checking".into());
        u.set_unity_state("checking".into());
        u.set_painter_state("checking".into());
        u.set_blender_status("Checking…".into());
        u.set_unity_status("Checking…".into());
        u.set_painter_status("Checking…".into());
    }
    spawn_probe(ui.clone(), "blender".to_string(), blender_base, String::new());
    spawn_probe(ui.clone(), "unity".to_string(), unity_base, String::new());
    spawn_probe(ui, "painter".to_string(), painter_bridge, painter_remote);
}

fn blender_result(res: &Result<String, String>, mode: &str, fallback: usize) -> (String, String) {
    match res {
        Err(e) => (e.clone(), "error".to_string()),
        Ok(body) => match serde_json::from_str::<serde_json::Value>(body) {
            Err(_) => (
                format!("Blender bridge replied ({fallback})"),
                "ok".to_string(),
            ),
            Ok(v) => {
                let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
                if mode == "open" {
                    if ok {
                        let opened = v
                            .get("opened")
                            .and_then(|x| x.as_i64())
                            .unwrap_or(fallback as i64);
                        (format!("Opened {opened} in a new Blender window"), "ok".into())
                    } else {
                        (
                            v.get("error")
                                .and_then(|x| x.as_str())
                                .unwrap_or("Failed to launch")
                                .to_string(),
                            "error".into(),
                        )
                    }
                } else if ok {
                    let imported = v
                        .get("result")
                        .and_then(|r| r.get("result"))
                        .and_then(|r| r.get("count"))
                        .and_then(|c| c.as_i64())
                        .unwrap_or(fallback as i64);
                    (format!("Imported {imported} to Blender"), "ok".into())
                } else {
                    (
                        v.get("error")
                            .and_then(|x| x.as_str())
                            .unwrap_or("Import failed")
                            .to_string(),
                        "error".into(),
                    )
                }
            }
        },
    }
}

fn spawn_blender_send(
    ui: Weak<AppWindow>,
    base: String,
    root: String,
    mode: String,
    items: Vec<Item>,
) {
    if items.is_empty() {
        return;
    }
    let count = items.len();
    std::thread::spawn(move || {
        let res = bridge::blender_send(&base, &root, &mode, &items);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(u) = ui.upgrade() else {
                return;
            };
            let (message, tone) = blender_result(&res, &mode, count);
            app(|a| show_toast(&u, a, &message, &tone));
        });
    });
}

fn spawn_unity_place(ui: Weak<AppWindow>, base: String, items: Vec<Item>, label: String) {
    let mut unique: Vec<Item> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for it in items {
        if it.slug.is_empty() || !seen.insert(it.code.clone()) {
            continue;
        }
        unique.push(it);
    }
    if unique.is_empty() {
        return;
    }
    let total = unique.len();
    std::thread::spawn(move || {
        let mut placed = 0usize;
        let mut first_error = String::new();
        for (n, it) in unique.iter().enumerate() {
            let request_id = format!("rq-{}-{}", chrono::Utc::now().timestamp_millis(), n);
            match bridge::unity_place_item(&base, it, &request_id) {
                Ok(body) => {
                    let value = serde_json::from_str::<serde_json::Value>(&body).ok();
                    let ok = value
                        .as_ref()
                        .and_then(|v| v.get("ok"))
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                    if ok {
                        placed += 1;
                    } else if first_error.is_empty() {
                        first_error = value
                            .as_ref()
                            .and_then(|v| v.get("error"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("Unknown error")
                            .to_string();
                    }
                }
                Err(e) => {
                    if first_error.is_empty() {
                        first_error = e;
                    }
                }
            }
        }
        let _ = slint::invoke_from_event_loop(move || {
            let Some(u) = ui.upgrade() else {
                return;
            };
            let (message, tone) = if placed == total {
                if total == 1 {
                    (format!("Placed {label} in Unity"), "ok".to_string())
                } else {
                    (format!("Placed {placed} {label}s in Unity"), "ok".to_string())
                }
            } else if placed > 0 {
                (
                    format!("Placed {placed}, {} failed: {first_error}", total - placed),
                    "error".to_string(),
                )
            } else if first_error.is_empty() {
                ("Place failed".to_string(), "error".to_string())
            } else {
                (first_error, "error".to_string())
            };
            app(|a| show_toast(&u, a, &message, &tone));
        });
    });
}

fn spawn_unity_action(ui: Weak<AppWindow>, base: String, route: String) {
    std::thread::spawn(move || {
        let res = bridge::unity_action(&base, &route);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(u) = ui.upgrade() else {
                return;
            };
            let is_sync = route == "sync";
            let (message, tone) = match &res {
                Ok(body) => {
                    let ok = serde_json::from_str::<serde_json::Value>(body)
                        .ok()
                        .and_then(|v| v.get("ok").and_then(|x| x.as_bool()))
                        .unwrap_or(false);
                    if ok {
                        (
                            if is_sync {
                                "Library synced".to_string()
                            } else {
                                "Scene exported".to_string()
                            },
                            "ok".to_string(),
                        )
                    } else {
                        let err = serde_json::from_str::<serde_json::Value>(body)
                            .ok()
                            .and_then(|v| v.get("error").and_then(|x| x.as_str()).map(str::to_string))
                            .unwrap_or_else(|| if is_sync { "Sync failed" } else { "Export failed" }.to_string());
                        (err, "error".to_string())
                    }
                }
                Err(e) => (e.clone(), "error".to_string()),
            };
            app(|a| show_toast(&u, a, &message, &tone));
        });
    });
}

fn spawn_painter_send(
    ui: Weak<AppWindow>,
    bridge_base: String,
    root: String,
    mode: String,
    items: Vec<Item>,
    label: String,
) {
    if items.is_empty() {
        return;
    }
    let count = items.len();
    std::thread::spawn(move || {
        let res = bridge::painter_send_with_root(&bridge_base, &root, &mode, &items);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(u) = ui.upgrade() else {
                return;
            };
            let (message, tone) = match &res {
                Err(e) => (e.clone(), "error".to_string()),
                Ok(body) => {
                    let value = serde_json::from_str::<serde_json::Value>(body).ok();
                    let ok = value
                        .as_ref()
                        .and_then(|v| v.get("ok"))
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                    if mode == "open" {
                        if ok {
                            (format!("Opened {label} in a new Painter project"), "ok".into())
                        } else {
                            (
                                value
                                    .as_ref()
                                    .and_then(|v| v.get("error"))
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("Failed to open")
                                    .to_string(),
                                "error".into(),
                            )
                        }
                    } else if ok {
                        let imported = value
                            .as_ref()
                            .and_then(|v| v.get("imported"))
                            .and_then(|x| x.as_array())
                            .map(|a| a.len())
                            .unwrap_or(count);
                        (format!("Imported {imported} to Painter"), "ok".into())
                    } else {
                        (
                            value
                                .as_ref()
                                .and_then(|v| v.get("error"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("Import failed")
                                .to_string(),
                            "error".into(),
                        )
                    }
                }
            };
            app(|a| show_toast(&u, a, &message, &tone));
        });
    });
}

/* --------------------------------- main --------------------------------- */

#[cfg(windows)]
fn glass_log(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("ald-glass.txt");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

#[cfg(not(windows))]
fn glass_log(_msg: &str) {}

/// Frosted-glass setup, ported from bilibili-slint's `win.rs`.
///
/// `DwmExtendFrameIntoClientArea` is the step that actually makes a frameless
/// window show a system backdrop; without it the DWM attributes below have no
/// visible effect. Win11 22621+ gets a live Acrylic backdrop, older builds fall
/// back to the undocumented Win10 accent policy. Everything is best-effort.
#[cfg(windows)]
fn apply_backdrop(ui: &AppWindow) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    const GWL_STYLE: i32 = -16;
    const WS_CAPTION: isize = 0x00C0_0000;
    const WS_THICKFRAME: isize = 0x0004_0000;
    const SWP_FLAGS: u32 = 0x0001 | 0x0002 | 0x0004 | 0x0010 | 0x0020;

    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
    const DWMWCP_ROUND: u32 = 2;
    const DWMSBT_TRANSIENTWINDOW: u32 = 3;

    const WCA_ACCENT_POLICY: i32 = 19;
    const ACCENT_ENABLE_ACRYLICBLURBEHIND: i32 = 4;

    #[repr(C)]
    struct Margins {
        l: i32,
        r: i32,
        t: i32,
        b: i32,
    }
    #[repr(C)]
    struct AccentPolicy {
        state: i32,
        flags: i32,
        gradient: u32,
        animation: i32,
    }
    #[repr(C)]
    struct WcaData {
        attribute: i32,
        data: *mut std::os::raw::c_void,
        size: usize,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    /// `MINMAXINFO` — five `POINT`s (all i32 pairs), see MSDN.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct MinMaxInfo {
        reserved: Point,
        max_size: Point,
        max_position: Point,
        min_track: Point,
        max_track: Point,
    }
    /// `WINDOWPOS` — size/position about to be applied (`WM_WINDOWPOSCHANGING`).
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct WindowPos {
        hwnd: isize,
        hwnd_insert_after: isize,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    }

    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const u8, size: u32) -> i32;
        fn DwmExtendFrameIntoClientArea(hwnd: isize, margins: *const Margins) -> i32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
        fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
        fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
        fn CallWindowProcW(prev: isize, hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn GetWindowRect(hwnd: isize, rect: *mut Rect) -> i32;
        fn GetDpiForWindow(hwnd: isize) -> u32;
    }

    const GWLP_WNDPROC: i32 = -4;
    const WM_NCCALCSIZE: u32 = 0x0083;
    const WM_NCHITTEST: u32 = 0x0084;
    const WM_GETMINMAXINFO: u32 = 0x0024;
    const WM_WINDOWPOSCHANGING: u32 = 0x0046;
    const SWP_NOSIZE: u32 = 0x0001;
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;

    static OLD_WNDPROC: std::sync::atomic::AtomicIsize =
        std::sync::atomic::AtomicIsize::new(0);

    /// Keep `WS_THICKFRAME` (so the window still resizes) while removing the
    /// non-client area, which is what stops Windows drawing its own frame while
    /// the window is dragged.
    ///
    /// `WM_NCCALCSIZE` removes the whole frame, so the default hit test has no
    /// border left to report; `WM_NCHITTEST` is therefore answered here with a
    /// 6px band to restore native edge/corner resize, `WM_GETMINMAXINFO` and
    /// `WM_WINDOWPOSCHANGING` restore the minimum size that the frame removal
    /// also broke (drag and programmatic paths respectively).
    unsafe extern "system" fn frameless_proc(
        hwnd: isize,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        if msg == WM_NCCALCSIZE && wparam != 0 {
            return 0;
        }

        if msg == WM_NCHITTEST {
            const BAND: i32 = 6;
            const HTCLIENT: isize = 1;
            const HTLEFT: isize = 10;
            const HTRIGHT: isize = 11;
            const HTTOP: isize = 12;
            const HTTOPLEFT: isize = 13;
            const HTTOPRIGHT: isize = 14;
            const HTBOTTOM: isize = 15;
            const HTBOTTOMLEFT: isize = 16;
            const HTBOTTOMRIGHT: isize = 17;

            // lParam packs two SIGNED 16-bit screen coordinates (low word x).
            let x = (lparam & 0xFFFF) as u16 as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32;
            let mut r = Rect::default();
            GetWindowRect(hwnd, &mut r);

            let on_left = x < r.left + BAND;
            let on_right = x >= r.right - BAND;
            let on_top = y < r.top + BAND;
            let on_bottom = y >= r.bottom - BAND;

            // Corners must be tested before single edges, else the edge wins.
            if on_top && on_left {
                return HTTOPLEFT;
            }
            if on_top && on_right {
                return HTTOPRIGHT;
            }
            if on_bottom && on_left {
                return HTBOTTOMLEFT;
            }
            if on_bottom && on_right {
                return HTBOTTOMRIGHT;
            }
            if on_left {
                return HTLEFT;
            }
            if on_right {
                return HTRIGHT;
            }
            if on_top {
                return HTTOP;
            }
            if on_bottom {
                return HTBOTTOM;
            }
            return HTCLIENT;
        }

        if msg == WM_GETMINMAXINFO {
            // Let the default proc fill in the (unreliable) defaults first, then
            // only clamp the minimum tracking size; leave max tracking as-is.
            let prev = CallWindowProcW(
                OLD_WNDPROC.load(std::sync::atomic::Ordering::Relaxed),
                hwnd,
                msg,
                wparam,
                lparam,
            );
            if lparam != 0 {
                let mmi = lparam as *mut MinMaxInfo;
                let dpi = GetDpiForWindow(hwnd);
                let scale = if dpi == 0 { 1.0f64 } else { dpi as f64 / 96.0 };
                (*mmi).min_track.x = (1368.0 * scale) as i32;
                (*mmi).min_track.y = (620.0 * scale) as i32;
            }
            return prev;
        }

        if msg == WM_WINDOWPOSCHANGING {
            // WM_GETMINMAXINFO only constrains user drags; a programmatic
            // SetWindowPos would otherwise slip below the floor. Clamp the
            // pending size in the WINDOWPOS struct as well.
            let prev = CallWindowProcW(
                OLD_WNDPROC.load(std::sync::atomic::Ordering::Relaxed),
                hwnd,
                msg,
                wparam,
                lparam,
            );
            if lparam != 0 {
                let wp = lparam as *mut WindowPos;
                if (*wp).flags & SWP_NOSIZE == 0 {
                    let dpi = GetDpiForWindow(hwnd);
                    let scale = if dpi == 0 { 1.0f64 } else { dpi as f64 / 96.0 };
                    let min_w = (1368.0 * scale) as i32;
                    let min_h = (620.0 * scale) as i32;
                    if (*wp).cx < min_w {
                        (*wp).cx = min_w;
                    }
                    if (*wp).cy < min_h {
                        (*wp).cy = min_h;
                    }
                }
            }
            return prev;
        }

        CallWindowProcW(
            OLD_WNDPROC.load(std::sync::atomic::Ordering::Relaxed),
            hwnd,
            msg,
            wparam,
            lparam,
        )
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> isize;
        fn GetProcAddress(module: isize, name: *const u8) -> *const std::os::raw::c_void;
    }

    let handle = ui.window().window_handle();
    glass_log("glass: got slint window_handle");
    let Ok(raw) = HasWindowHandle::window_handle(&handle) else {
        return false;
    };
    let RawWindowHandle::Win32(win32) = raw.as_raw() else {
        glass_log("glass: FAILED not a Win32 handle");
        return false;
    };
    let hwnd = win32.hwnd.get();
    glass_log(&format!("glass: hwnd={hwnd}"));
    if hwnd == 0 {
        return false;
    }

    let set_u32 = |attr: u32, value: u32| -> i32 {
        unsafe { DwmSetWindowAttribute(hwnd, attr, (&value as *const u32).cast(), 4) }
    };

    unsafe {
        // 1. extend the DWM frame over the whole client area (mandatory for the
        //    backdrop of a frameless window)
        let margins = Margins { l: -1, r: -1, t: -1, b: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);

        // 2. live Acrylic; fall back to the Win10 accent policy
        let backdrop = set_u32(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW);
        if backdrop != 0 {
            type SetAccent = unsafe extern "system" fn(isize, *mut WcaData) -> i32;
            let module: Vec<u16> = "user32.dll\0".encode_utf16().collect();
            let lib = LoadLibraryW(module.as_ptr());
            if lib != 0 {
                let entry = GetProcAddress(lib, c"SetWindowCompositionAttribute".as_ptr().cast());
                if !entry.is_null() {
                    let set_accent: SetAccent = std::mem::transmute(entry);
                    let policy = AccentPolicy {
                        state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
                        flags: 2,
                        gradient: 0x0100_0000,
                        animation: 0,
                    };
                    let mut data = WcaData {
                        attribute: WCA_ACCENT_POLICY,
                        data: (&policy as *const AccentPolicy as *mut AccentPolicy).cast(),
                        size: std::mem::size_of::<AccentPolicy>(),
                    };
                    let _ = set_accent(hwnd, &mut data);
                }
            }
        }

        // 3. rounded corners + dark DWM chrome
        let _ = set_u32(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
        let _ = set_u32(DWMWA_USE_IMMERSIVE_DARK_MODE, 1);

        // 4. enforce frameless while keeping native resize, then kill the frame
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let frameless = (style & !WS_CAPTION) | WS_THICKFRAME;
        if frameless != style {
            SetWindowLongPtrW(hwnd, GWL_STYLE, frameless);
            SetWindowPos(hwnd, 0, 0, 0, 0, 0, SWP_FLAGS);
        }
        let _ = set_u32(DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE);
        if OLD_WNDPROC.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            let proc = frameless_proc
                as unsafe extern "system" fn(isize, u32, usize, isize) -> isize;
            let old = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc as usize as isize);
            if old != 0 {
                OLD_WNDPROC.store(old, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // 5. never keep an always-on-top bit (a leftover from an external tool
        //    would pin the window above every other app)
        const GWL_EXSTYLE: i32 = -20;
        const WS_EX_TOPMOST: isize = 0x0000_0008;
        const HWND_NOTOPMOST: isize = -2;
        const SWP_PLAIN: u32 = 0x0001 | 0x0002 | 0x0010;
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if ex & WS_EX_TOPMOST != 0 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !WS_EX_TOPMOST);
            SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_PLAIN);
        }
    }

    true
}

/// Slint only exposes the native window handle once the window really exists, so
/// the glass setup is retried on a short timer chain until it takes.
fn schedule_glass(ui: Weak<AppWindow>, tries: u32) {
    if tries == 0 {
        glass_log("glass: gave up");
        return;
    }
    let timer = Timer::default();
    let weak = ui.clone();
    timer.start(
        TimerMode::SingleShot,
        Duration::from_millis(200),
        move || {
            let done = weak.upgrade().map(|u| apply_backdrop(&u)).unwrap_or(false);
            if done {
                glass_log("glass: applied");
            } else {
                schedule_glass(weak.clone(), tries - 1);
            }
        },
    );
    std::mem::forget(timer);
}

#[cfg(not(windows))]
fn apply_backdrop(_ui: &AppWindow) -> bool {
    false
}

#[cfg(not(windows))]
fn schedule_glass(_ui: Weak<AppWindow>, _tries: u32) {}

fn main() -> Result<(), slint::PlatformError> {
    let lib = Library::new(ROOT);
    let mut store = store::load();

    let items = lib.load_manifest().unwrap_or_default();
    let taxonomy = build_taxonomy(&items, lib.taxonomy_json().as_deref());
    let state = AppState::new(items, store.cart.clone(), taxonomy);
    // The store only keeps codes that still exist.
    store.cart = state.cart.clone();

    let items_model = Rc::new(VecModel::from(
        state.items.iter().map(asset_row).collect::<Vec<_>>(),
    ));
    let thumb_state = vec![0u8; state.items.len()];
    let cart_model = Rc::new(VecModel::from(Vec::<CartRow>::new()));
    let slides_model = Rc::new(VecModel::from(Vec::<SlideItem>::new()));

    let app_rc = Rc::new(RefCell::new(App {
        lib,
        state,
        store,
        items: items_model,
        cart_model,
        slides_model,
        thumb_state,
        selected: None,
        edit: None,
        delete_armed: false,
        rig: None,
        toast_timer: Timer::default(),
        delete_timer: Timer::default(),
        probe_timer: Timer::default(),
        rig_timer: Timer::default(),
    }));
    APP.with(|cell| {
        *cell.borrow_mut() = Some(app_rc.clone());
    });

    let ui = AppWindow::new()?;

    // The .slint window no longer pins width/height (a constant binding froze the
    // surface size); set the initial size here, before the first show().
    ui.window().set_size(slint::PhysicalSize::new(1440u32, 900u32));


    // Initial data models.
    if let Ok(a) = app_rc.try_borrow() {
        ui.set_items(ModelRc::new(a.items.clone()));
        ui.set_cart(ModelRc::new(a.cart_model.clone()));
        ui.set_slides(ModelRc::new(a.slides_model.clone()));
    }
    ui.set_clips(ModelRc::new(VecModel::from(Vec::<ClipItem>::new())));
    ui.set_spec(ModelRc::new(VecModel::from(Vec::<SpecRow>::new())));
    ui.set_sort_options(to_string_model(&sort_options()));
    ui.set_style_options(to_string_model(&style_options()));
    ui.set_l1_options(empty_string_model());
    ui.set_l2_options(empty_string_model());
    ui.set_edit_l1_options(empty_string_model());
    ui.set_edit_l2_options(empty_string_model());
    ui.set_edit_tags(empty_string_model());
    ui.set_query("".into());
    ui.set_status_text("Ready".into());
    ui.set_toast_text("".into());
    ui.set_blender_state("checking".into());
    ui.set_unity_state("checking".into());
    ui.set_painter_state("checking".into());
    ui.set_blender_status("Checking…".into());
    ui.set_unity_status("Checking…".into());
    ui.set_painter_status("Checking…".into());
    if let Ok(a) = app_rc.try_borrow() {
        ui.set_blender_root(a.store.blender_root.clone().into());
        ui.set_blender_base(a.store.blender_base.clone().into());
        ui.set_unity_base(a.store.unity_base.clone().into());
        ui.set_painter_base(a.store.painter_base.clone().into());
        ui.set_painter_bridge(a.store.painter_bridge.clone().into());
        ui.set_painter_root(a.store.painter_root.clone().into());
        sync_all(&ui, &a);
    }

    if !std::path::Path::new(MANIFEST).exists() {
        ui.set_status_text(format!("Manifest not found: {MANIFEST}").into());
    }

    wire_callbacks(&ui, &app_rc);

    // Initial health probe + 15s re-probe timer.
    probe_all(ui.as_weak());
    {
        let weak = ui.as_weak();
        if let Ok(a) = app_rc.try_borrow() {
            a.probe_timer.start(TimerMode::Repeated, Duration::from_secs(15), move || {
                probe_all(weak.clone());
            });
        }
    }

    if let Ok(debug_view) = std::env::var("ALD_DEBUG_VIEW") {
        match debug_view.as_str() {
            "cart" => ui.invoke_open_cart(),
            "rig" => {
                ui.invoke_select_tab(2);
                let first_rig = app_rc
                    .try_borrow()
                    .ok()
                    .and_then(|a| a.state.items.iter().position(|it| it.kind() == Kind::Rig));
                if let Some(i) = first_rig {
                    ui.invoke_open_detail(i as i32);
                }
            }
            "cat" => {
                let first_model = app_rc
                    .try_borrow()
                    .ok()
                    .and_then(|a| a.state.items.iter().position(|it| it.kind() == Kind::Model));
                if let Some(i) = first_model {
                    ui.invoke_open_detail(i as i32);
                    ui.set_pick_open(4);
                }
            }
            "model" => {
                let first_model = app_rc
                    .try_borrow()
                    .ok()
                    .and_then(|a| a.state.items.iter().position(|it| it.kind() == Kind::Model));
                if let Some(i) = first_model {
                    ui.invoke_open_detail(i as i32);
                }
            }
            "blender" => ui.invoke_blender_open_dialog(),
            "unity" => ui.invoke_unity_open_dialog(),
            "painter" => ui.invoke_painter_open_dialog(),
            _ => {}
        }
    }

    ui.show()?;
    apply_backdrop(&ui);

    ui.show()?;
    apply_backdrop(&ui);

    ui.show()?;
    schedule_glass(ui.as_weak(), 25);

    ui.run()
}

/* ------------------------------ callbacks ------------------------------- */

fn wire_callbacks(ui: &AppWindow, app_rc: &Rc<RefCell<App>>) {
    // --- toolbar / filters ------------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_select_tab(move |tab| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.set_tab(Kind::from_num(tab));
                if let Some(u) = weak.upgrade() {
                    sync_all(&u, &a);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_set_query(move |text| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.q = text.to_string();
                a.state.clamp_page();
                if let Some(u) = weak.upgrade() {
                    sync_view(&u, &a);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_clear_query(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.q.clear();
                a.state.clamp_page();
                if let Some(u) = weak.upgrade() {
                    u.set_query("".into());
                    sync_view(&u, &a);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_select_l1(move |index| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let options = a.state.l1_options();
                let next = if index <= 0 {
                    String::new()
                } else {
                    options.get((index - 1) as usize).cloned().unwrap_or_default()
                };
                if next == a.state.l1 {
                    // Idempotent: also swallows the spurious index-0 that Slint's
                    // ComboBox emits while we push the options model.
                    return;
                }
                a.state.l1 = next;
                a.state.l2.clear();
                a.state.ensure_integrity();
                a.state.clamp_page();
                if let Some(u) = weak.upgrade() {
                    sync_all(&u, &a);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_select_l2(move |index| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let options = a.state.l2_options();
                let next = if index <= 0 {
                    String::new()
                } else {
                    options.get((index - 1) as usize).cloned().unwrap_or_default()
                };
                if next == a.state.l2 {
                    return;
                }
                a.state.l2 = next;
                a.state.ensure_integrity();
                a.state.clamp_page();
                if let Some(u) = weak.upgrade() {
                    sync_all(&u, &a);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_select_sort(move |index| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.set_sort_index(index);
                if let Some(u) = weak.upgrade() {
                    u.set_sort_index(a.state.sort.index());
                    sync_view(&u, &a);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_select_style(move |index| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.style_textured_only = index == 1;
                a.state.clamp_page();
                if let Some(u) = weak.upgrade() {
                    u.set_style_index(index);
                    sync_view(&u, &a);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_clear_filters(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.reset_filters();
                if let Some(u) = weak.upgrade() {
                    u.set_query("".into());
                    sync_all(&u, &a);
                }
            }
        });
    }

    // --- pagination -------------------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_page_first(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.page = 0;
                if let Some(u) = weak.upgrade() {
                    sync_view(&u, &a);
                    u.set_grid_scroll(0.0);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_page_prev(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.page = a.state.page.saturating_sub(1);
                if let Some(u) = weak.upgrade() {
                    sync_view(&u, &a);
                    u.set_grid_scroll(0.0);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_page_next(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let last = a.state.page_count() - 1;
                a.state.page = (a.state.page + 1).min(last);
                if let Some(u) = weak.upgrade() {
                    sync_view(&u, &a);
                    u.set_grid_scroll(0.0);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_page_last(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.page = a.state.page_count() - 1;
                if let Some(u) = weak.upgrade() {
                    sync_view(&u, &a);
                    u.set_grid_scroll(0.0);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_page_goto(move |index| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.page = index.max(0) as usize;
                a.state.clamp_page();
                if let Some(u) = weak.upgrade() {
                    sync_view(&u, &a);
                    u.set_grid_scroll(0.0);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_set_page_size(move |size| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.set_page_size(size.max(0) as usize);
                if let Some(u) = weak.upgrade() {
                    sync_view(&u, &a);
                    u.set_grid_scroll(0.0);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_page_goto_text(move |text| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                if let Ok(parsed) = text.trim().parse::<usize>() {
                    a.state.page = parsed.saturating_sub(1);
                    a.state.clamp_page();
                    if let Some(u) = weak.upgrade() {
                        sync_view(&u, &a);
                        u.set_grid_scroll(0.0);
                    }
                }
            }
        });
    }

    // --- refresh all ------------------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_refresh_all(move || {
            let root = match app_rc.try_borrow() {
                Ok(a) => a.lib.root.clone(),
                Err(_) => return,
            };
            if let Some(u) = weak.upgrade() {
                u.set_busy(true);
                u.set_status_text("Refreshing…".into());
            }
            let ui = weak.clone();
            std::thread::spawn(move || {
                let lib = Library::new(&root);
                let items = lib.load_manifest().unwrap_or_default();
                let taxonomy_json = lib.taxonomy_json();
                let count = items.len();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(u) = ui.upgrade() else {
                        return;
                    };
                    app(|a| {
                        a.state.items = items;
                        a.state.taxonomy =
                            build_taxonomy(&a.state.items, taxonomy_json.as_deref());
                        let valid: std::collections::HashSet<String> =
                            a.state.items.iter().map(|it| it.code.clone()).collect();
                        a.state.cart.retain(|c| valid.contains(c));
                        a.items.set_vec(
                            a.state.items.iter().map(asset_row).collect::<Vec<_>>(),
                        );
                        a.thumb_state = vec![0u8; a.state.items.len()];
                        a.selected = None;
                        a.edit = None;
                        stop_rig(&u, a);
                        persist(&a.store, &a.state.cart);
                        a.state.clamp_page();
                        sync_all(&u, a);
                        show_toast(&u, a, &format!("Refreshed ({count} assets)"), "ok");
                    });
                    u.set_busy(false);
                    u.set_status_text("Ready".into());
                });
            });
        });
    }

    // --- lazy thumbnails --------------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_ensure_image(move |index| {
            if index < 0 {
                return;
            }
            let idx = index as usize;
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                if idx >= a.state.items.len() {
                    return;
                }
                if a.thumb_state.get(idx).copied().unwrap_or(1) != 0 {
                    return;
                }
                a.thumb_state[idx] = 1;
                let path = preview_abs(&a, &a.state.items[idx]);
                if let Some(u) = weak.upgrade() {
                    spawn_item_thumb(u.as_weak(), idx, path);
                }
            }
        });
    }

    // --- per-card actions -------------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_card_cart(move |index| {
            if index < 0 {
                return;
            }
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let idx = index as usize;
                if idx >= a.state.items.len() {
                    return;
                }
                let code = a.state.items[idx].code.clone();
                let added = a.state.toggle_cart(&code);
                refresh_cart_flags(&a, &[idx]);
                persist(&a.store, &a.state.cart);
                if let Some(u) = weak.upgrade() {
                    rebuild_cart(&u, &a);
                    let (message, tone) = if added {
                        (format!("Added {code}"), "ok")
                    } else {
                        (format!("Removed {code}"), "info")
                    };
                    show_toast(&u, &mut a, &message, tone);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_card_copy(move |index| {
            if index < 0 {
                return;
            }
            let payload = match app_rc.try_borrow() {
                Ok(a) => {
                    let idx = index as usize;
                    if idx >= a.state.items.len() {
                        return;
                    }
                    Some((
                        a.state.items[idx].code.clone(),
                        export::build_item_payload(&a.state.items[idx]),
                    ))
                }
                Err(_) => None,
            };
            let Some((code, payload)) = payload else {
                return;
            };
            let copied = arboard::Clipboard::new()
                .map(|mut clip| clip.set_text(payload).is_ok())
                .unwrap_or(false);
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                if let Some(u) = weak.upgrade() {
                    if copied {
                        show_toast(&u, &mut a, &format!("Copied asset data for {code}"), "ok");
                    } else {
                        show_toast(&u, &mut a, &format!("Failed to copy {code}"), "error");
                    }
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_card_blender(move |index| {
            if index < 0 {
                return;
            }
            if let Ok(a) = app_rc.try_borrow() {
                let idx = index as usize;
                if idx >= a.state.items.len() {
                    return;
                }
                let targets = bridge::blender_targets(std::slice::from_ref(&a.state.items[idx]));
                if let Some(u) = weak.upgrade() {
                    spawn_blender_send(
                        u.as_weak(),
                        a.store.blender_base.clone(),
                        a.store.blender_root.clone(),
                        "import".to_string(),
                        targets,
                    );
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_card_unity(move |index| {
            if index < 0 {
                return;
            }
            if let Ok(a) = app_rc.try_borrow() {
                let idx = index as usize;
                if idx >= a.state.items.len() {
                    return;
                }
                let item = a.state.items[idx].clone();
                if let Some(u) = weak.upgrade() {
                    spawn_unity_place(
                        u.as_weak(),
                        a.store.unity_base.clone(),
                        vec![item],
                        "asset".to_string(),
                    );
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_card_painter(move |index| {
            if index < 0 {
                return;
            }
            if let Ok(a) = app_rc.try_borrow() {
                let idx = index as usize;
                if idx >= a.state.items.len() {
                    return;
                }
                let targets = bridge::painter_targets(std::slice::from_ref(&a.state.items[idx]));
                if let Some(u) = weak.upgrade() {
                    spawn_painter_send(
                        u.as_weak(),
                        a.store.painter_bridge.clone(),
                        a.store.painter_root.clone(),
                        "import".to_string(),
                        targets,
                        "asset".to_string(),
                    );
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_card_tag(move |tag| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.tag = tag.to_string();
                a.state.clamp_page();
                if let Some(u) = weak.upgrade() {
                    sync_view(&u, &a);
                }
            }
        });
    }

    // --- detail dialog ----------------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_open_detail(move |index| {
            if index < 0 {
                return;
            }
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                if let Some(u) = weak.upgrade() {
                    open_detail(&u, &mut a, index as usize);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_close_detail(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.selected = None;
                a.edit = None;
                a.delete_armed = false;
                if let Some(u) = weak.upgrade() {
                    stop_rig(&u, &mut a);
                    u.set_detail_open(false);
                    u.set_delete_armed(false);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_detail_set_l1(move |index| {
            if index < 0 {
                return;
            }
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let kind = match a.edit.as_ref() {
                    Some(e) => e.kind,
                    None => return,
                };
                let l1_value = {
                    let Some(e) = a.edit.as_mut() else {
                        return;
                    };
                    let Some(value) = e.l1_options.get(index as usize).cloned() else {
                        return;
                    };
                    e.l1 = value.clone();
                    value
                };
                let l2_options = a.state.taxonomy.l2_for(kind, &l1_value);
                if let Some(e) = a.edit.as_mut() {
                    e.l2 = if l2_options.iter().any(|x| x == &e.l2) {
                        e.l2.clone()
                    } else {
                        l2_options.first().cloned().unwrap_or_default()
                    };
                    e.l2_options = l2_options;
                }
                let dirty = edit_dirty(&a);
                let (l1_index, l2_index, l2_model) = match a.edit.as_ref() {
                    Some(e) => (
                        index_in(&e.l1_options, &e.l1),
                        index_in(&e.l2_options, &e.l2),
                        e.l2_options.clone(),
                    ),
                    None => return,
                };
                if let Some(u) = weak.upgrade() {
                    u.set_edit_l2_options(to_string_model(&l2_model));
                    u.set_edit_l1_index(l1_index);
                    u.set_edit_l2_index(l2_index);
                    u.set_edit_dirty(dirty);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_detail_set_l2(move |index| {
            if index < 0 {
                return;
            }
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                {
                    let Some(e) = a.edit.as_mut() else {
                        return;
                    };
                    let Some(value) = e.l2_options.get(index as usize).cloned() else {
                        return;
                    };
                    e.l2 = value;
                }
                let dirty = edit_dirty(&a);
                if let Some(u) = weak.upgrade() {
                    u.set_edit_l2_index(index);
                    u.set_edit_dirty(dirty);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_detail_add_tag(move |tag| {
            let value = tag.trim().to_lowercase();
            if value.is_empty() {
                return;
            }
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                {
                    let Some(e) = a.edit.as_mut() else {
                        return;
                    };
                    if !e.tags.iter().any(|x| x == &value) {
                        e.tags.push(value);
                    }
                }
                let dirty = edit_dirty(&a);
                let tags = a.edit.as_ref().map(|e| e.tags.clone()).unwrap_or_default();
                if let Some(u) = weak.upgrade() {
                    u.set_edit_tags(to_string_model(&tags));
                    u.set_edit_dirty(dirty);
                    u.set_edit_tag_input("".into());
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_detail_remove_tag(move |tag| {
            let value = tag.to_string();
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                {
                    let Some(e) = a.edit.as_mut() else {
                        return;
                    };
                    e.tags.retain(|x| x != &value);
                }
                let dirty = edit_dirty(&a);
                let tags = a.edit.as_ref().map(|e| e.tags.clone()).unwrap_or_default();
                if let Some(u) = weak.upgrade() {
                    u.set_edit_tags(to_string_model(&tags));
                    u.set_edit_dirty(dirty);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_detail_save(move || {
            let (code, l1, l2, tags, root) = {
                let Ok(mut a) = app_rc.try_borrow_mut() else {
                    return;
                };
                let Some(idx) = a.selected else {
                    return;
                };
                if !edit_dirty(&a) {
                    return;
                }
                let Some(e) = a.edit.as_ref() else {
                    return;
                };
                if e.busy {
                    return;
                }
                let (code, l1, l2, tags) = (
                    a.state.items[idx].code.clone(),
                    e.l1.clone(),
                    e.l2.clone(),
                    e.tags.clone(),
                );
                if let Some(e) = a.edit.as_mut() {
                    e.busy = true;
                }
                if let Some(u) = weak.upgrade() {
                    u.set_edit_busy(true);
                    u.set_edit_dirty(false);
                }
                (code, l1, l2, tags, a.lib.root.clone())
            };

            let ui = weak.clone();
            std::thread::spawn(move || {
                let lib = Library::new(&root);
                let result = lib.edit_asset(&code, &l1, &l2, &tags);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(u) = ui.upgrade() else {
                        return;
                    };
                    app(|a| {
                        if let Some(e) = a.edit.as_mut() {
                            e.busy = false;
                        }
                        match result {
                            Ok(updated) => {
                                if let Some(pos) = a.state.index_of(&code) {
                                    a.state.items[pos] = updated.clone();
                                    let mut row = asset_row(&updated);
                                    if let Some(old) = a.items.row_data(pos) {
                                        row.thumb = old.thumb;
                                        row.in_cart = old.in_cart;
                                    }
                                    a.items.set_row_data(pos, row);
                                    if a.selected == Some(pos) {
                                        open_detail(&u, a, pos);
                                    }
                                }
                                rebuild_rows(&u, a);
                                show_toast(
                                    &u,
                                    a,
                                    &format!("Updated {code} → {}", updated.category),
                                    "ok",
                                );
                            }
                            Err(e) => {
                                show_toast(&u, a, &format!("Update failed: {e}"), "error");
                            }
                        }
                    });
                    u.set_edit_busy(false);
                });
            });
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_detail_delete(move || {
            let (armed, code, root) = {
                let Ok(mut a) = app_rc.try_borrow_mut() else {
                    return;
                };
                let Some(idx) = a.selected else {
                    return;
                };
                if a.delete_armed {
                    a.delete_armed = false;
                    if let Some(u) = weak.upgrade() {
                        u.set_delete_armed(false);
                    }
                    (true, a.state.items[idx].code.clone(), a.lib.root.clone())
                } else {
                    a.delete_armed = true;
                    if let Some(u) = weak.upgrade() {
                        u.set_delete_armed(true);
                        let armed_weak = u.as_weak();
                        a.delete_timer.start(
                            TimerMode::SingleShot,
                            Duration::from_millis(8000),
                            move || {
                                if let Some(u) = armed_weak.upgrade() {
                                    u.set_delete_armed(false);
                                }
                                app(|a| a.delete_armed = false);
                            },
                        );
                    }
                    (false, String::new(), String::new())
                }
            };
            if !armed {
                return;
            }

            let ui = weak.clone();
            std::thread::spawn(move || {
                let lib = Library::new(&root);
                let result = lib.delete_asset(&code);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(u) = ui.upgrade() else {
                        return;
                    };
                    app(|a| match result {
                        Ok(()) => {
                            if let Some(pos) = a.state.index_of(&code) {
                                a.state.items.remove(pos);
                                a.state.remove_cart(&code);
                                a.items.set_vec(
                                    a.state.items.iter().map(asset_row).collect::<Vec<_>>(),
                                );
                                a.thumb_state = vec![0u8; a.state.items.len()];
                            }
                            a.selected = None;
                            a.edit = None;
                            stop_rig(&u, a);
                            persist(&a.store, &a.state.cart);
                            u.set_detail_open(false);
                            a.state.clamp_page();
                            sync_all(&u, a);
                            show_toast(&u, a, &format!("Deleted {code}"), "ok");
                        }
                        Err(e) => {
                            show_toast(&u, a, &format!("Delete failed: {e}"), "error");
                        }
                    });
                });
            });
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_detail_select_slide(move |index| {
            let Some(u) = weak.upgrade() else { return };
            let (abs, count) = {
                let Ok(a) = app_rc.try_borrow() else { return };
                let Some(row) = a.slides_model.row_data(index.max(0) as usize) else {
                    return;
                };
                (a.lib.abs_path(&row.rel), a.slides_model.row_count())
            };
            if let Ok(img) = slint::Image::load_from_path(std::path::Path::new(&abs)) {
                u.set_detail_thumb(img);
            }
            let Ok(a) = app_rc.try_borrow_mut() else {
                return;
            };
            for i in 0..count {
                if let Some(mut r) = a.slides_model.row_data(i) {
                    let want = i == index.max(0) as usize;
                    if r.active != want {
                        r.active = want;
                        a.slides_model.set_row_data(i, r);
                    }
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_detail_refresh(move || {
            let (item, root) = {
                let Ok(a) = app_rc.try_borrow() else {
                    return;
                };
                let Some(idx) = a.selected else {
                    return;
                };
                (
                    a.state.items[idx].clone(),
                    a.lib.root.clone(),
                )
            };
            let rel = format!("{}/{}.json", item.path(), item.slug);
            let ui = weak.clone();
            std::thread::spawn(move || {
                let lib = Library::new(&root);
                let result = lib
                    .read_text(&rel)
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(|e| e.to_string()));
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(u) = ui.upgrade() else {
                        return;
                    };
                    app(|a| match result {
                        Ok(json) => {
                            if let Some(pos) = a.state.index_of(&item.code) {
                                merge_refresh(&mut a.state.items[pos], &json);
                                let refreshed = a.state.items[pos].clone();
                                let mut row = asset_row(&refreshed);
                                if let Some(old) = a.items.row_data(pos) {
                                    row.thumb = old.thumb;
                                    row.in_cart = old.in_cart;
                                }
                                a.items.set_row_data(pos, row);
                                a.thumb_state[pos] = 0;
                                if a.selected == Some(pos) {
                                    open_detail(&u, a, pos);
                                } else {
                                    a.thumb_state[pos] = 1;
                                    let path = preview_abs(a, &refreshed);
                                    spawn_item_thumb(u.as_weak(), pos, path);
                                }
                            }
                            rebuild_rows(&u, a);
                            show_toast(&u, a, &format!("Refreshed {}", item.code), "ok");
                        }
                        Err(e) => {
                            show_toast(
                                &u,
                                a,
                                &format!("Failed to refresh {}: {e}", item.code),
                                "error",
                            );
                        }
                    });
                });
            });
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_detail_blender(move || {
            if let Ok(a) = app_rc.try_borrow() {
                let Some(item) = current_item(&a) else {
                    return;
                };
                let targets = bridge::blender_targets(std::slice::from_ref(&item));
                if let Some(u) = weak.upgrade() {
                    spawn_blender_send(
                        u.as_weak(),
                        a.store.blender_base.clone(),
                        a.store.blender_root.clone(),
                        "import".to_string(),
                        targets,
                    );
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_detail_unity(move || {
            if let Ok(a) = app_rc.try_borrow() {
                let Some(item) = current_item(&a) else {
                    return;
                };
                if let Some(u) = weak.upgrade() {
                    spawn_unity_place(
                        u.as_weak(),
                        a.store.unity_base.clone(),
                        vec![item],
                        "current asset".to_string(),
                    );
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_detail_painter(move || {
            if let Ok(a) = app_rc.try_borrow() {
                let Some(item) = current_item(&a) else {
                    return;
                };
                let targets = bridge::painter_targets(std::slice::from_ref(&item));
                if let Some(u) = weak.upgrade() {
                    spawn_painter_send(
                        u.as_weak(),
                        a.store.painter_bridge.clone(),
                        a.store.painter_root.clone(),
                        "import".to_string(),
                        targets,
                        "current asset".to_string(),
                    );
                }
            }
        });
    }

    // --- bridge dialogs ---------------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_check_bridge(move |which| {
            if let Ok(a) = app_rc.try_borrow() {
                if let Some(u) = weak.upgrade() {
                    match which.as_str() {
                        "blender" => {
                            u.set_blender_state("checking".into());
                            spawn_probe(
                                u.as_weak(),
                                "blender".to_string(),
                                a.store.blender_base.clone(),
                                String::new(),
                            );
                        }
                        "unity" => {
                            u.set_unity_state("checking".into());
                            spawn_probe(
                                u.as_weak(),
                                "unity".to_string(),
                                a.store.unity_base.clone(),
                                String::new(),
                            );
                        }
                        _ => {
                            u.set_painter_state("checking".into());
                            spawn_probe(
                                u.as_weak(),
                                "painter".to_string(),
                                a.store.painter_bridge.clone(),
                                a.store.painter_base.clone(),
                            );
                        }
                    }
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_blender_open_dialog(move || {
            if let Ok(a) = app_rc.try_borrow() {
                if let Some(u) = weak.upgrade() {
                    u.set_blender_root(a.store.blender_root.clone().into());
                    u.set_blender_base(a.store.blender_base.clone().into());
                    u.set_blender_open(true);
                    u.set_blender_state("checking".into());
                    spawn_probe(
                        u.as_weak(),
                        "blender".to_string(),
                        a.store.blender_base.clone(),
                        String::new(),
                    );
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_blender_close(move || {
            if let Some(u) = weak.upgrade() {
                u.set_blender_open(false);
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_blender_config(move |root, base| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.store.blender_root = root.to_string();
                a.store.blender_base = base.to_string();
                store::save(&a.store);
                if let Some(u) = weak.upgrade() {
                    u.set_blender_state("checking".into());
                    spawn_probe(
                        u.as_weak(),
                        "blender".to_string(),
                        base.to_string(),
                        String::new(),
                    );
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_blender_send(move |mode| {
            let Ok(mut a) = app_rc.try_borrow_mut() else {
                return;
            };
            let Some(u) = weak.upgrade() else {
                return;
            };
            let items = cart_items(&a);
            let targets = bridge::blender_targets(&items);
            if targets.is_empty() {
                let message = if items.is_empty() {
                    "Cart is empty"
                } else {
                    "Only models can be sent to Blender"
                };
                show_toast(&u, &mut a, message, "info");
            } else {
                spawn_blender_send(
                    u.as_weak(),
                    a.store.blender_base.clone(),
                    a.store.blender_root.clone(),
                    mode.to_string(),
                    targets,
                );
                show_toast(
                    &u,
                    &mut a,
                    &format!(
                        "{} {} to Blender…",
                        if mode == "open" { "Opening" } else { "Importing" },
                        items.len()
                    ),
                    "info",
                );
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_unity_open_dialog(move || {
            if let Ok(a) = app_rc.try_borrow() {
                if let Some(u) = weak.upgrade() {
                    u.set_unity_base(a.store.unity_base.clone().into());
                    u.set_unity_open(true);
                    u.set_unity_state("checking".into());
                    spawn_probe(
                        u.as_weak(),
                        "unity".to_string(),
                        a.store.unity_base.clone(),
                        String::new(),
                    );
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_unity_close(move || {
            if let Some(u) = weak.upgrade() {
                u.set_unity_open(false);
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_unity_config(move |base| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.store.unity_base = base.to_string();
                store::save(&a.store);
                if let Some(u) = weak.upgrade() {
                    u.set_unity_state("checking".into());
                    spawn_probe(
                        u.as_weak(),
                        "unity".to_string(),
                        base.to_string(),
                        String::new(),
                    );
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_unity_place_cart(move || {
            let Ok(mut a) = app_rc.try_borrow_mut() else {
                return;
            };
            let Some(u) = weak.upgrade() else {
                return;
            };
            let items = cart_items(&a);
            if items.is_empty() {
                show_toast(&u, &mut a, "Cart is empty", "info");
            } else {
                show_toast(&u, &mut a, &format!("Placing {} in Unity…", items.len()), "info");
                spawn_unity_place(
                    u.as_weak(),
                    a.store.unity_base.clone(),
                    items,
                    "cart asset".to_string(),
                );
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_unity_action(move |route| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let Some(u) = weak.upgrade() else {
                    return;
                };
                let is_sync = route.as_str() == "sync";
                show_toast(
                    &u,
                    &mut a,
                    if is_sync { "Syncing library…" } else { "Exporting scene…" },
                    "info",
                );
                spawn_unity_action(
                    u.as_weak(),
                    a.store.unity_base.clone(),
                    route.to_string(),
                );
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_painter_open_dialog(move || {
            if let Ok(a) = app_rc.try_borrow() {
                if let Some(u) = weak.upgrade() {
                    u.set_painter_base(a.store.painter_base.clone().into());
                    u.set_painter_bridge(a.store.painter_bridge.clone().into());
                    u.set_painter_root(a.store.painter_root.clone().into());
                    u.set_painter_open(true);
                    u.set_painter_state("checking".into());
                    spawn_probe(
                        u.as_weak(),
                        "painter".to_string(),
                        a.store.painter_bridge.clone(),
                        a.store.painter_base.clone(),
                    );
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_painter_close(move || {
            if let Some(u) = weak.upgrade() {
                u.set_painter_open(false);
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_painter_config(move |base, bridge, root| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.store.painter_base = base.to_string();
                a.store.painter_bridge = bridge.to_string();
                a.store.painter_root = root.to_string();
                store::save(&a.store);
                if let Some(u) = weak.upgrade() {
                    u.set_painter_state("checking".into());
                    spawn_probe(
                        u.as_weak(),
                        "painter".to_string(),
                        bridge.to_string(),
                        base.to_string(),
                    );
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_painter_send(move |mode| {
            let Ok(mut a) = app_rc.try_borrow_mut() else {
                return;
            };
            let Some(u) = weak.upgrade() else {
                return;
            };
            let items = cart_items(&a);
            let targets = bridge::painter_targets(&items);
            if targets.is_empty() {
                let message = if items.is_empty() {
                    "Cart is empty"
                } else {
                    "Only texture (material) assets can be sent to Substance Painter"
                };
                show_toast(&u, &mut a, message, "info");
            } else {
                spawn_painter_send(
                    u.as_weak(),
                    a.store.painter_bridge.clone(),
                    a.store.painter_root.clone(),
                    mode.to_string(),
                    targets,
                    "cart item".to_string(),
                );
                show_toast(
                    &u,
                    &mut a,
                    &format!(
                        "{} {} to Painter…",
                        if mode == "open" { "Opening" } else { "Importing" },
                        items.len()
                    ),
                    "info",
                );
            }
        });
    }

    // --- cart -------------------------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_open_cart(move || {
            if let Ok(a) = app_rc.try_borrow() {
                if let Some(u) = weak.upgrade() {
                    rebuild_cart(&u, &a);
                    u.set_cart_open(true);
                    let entries: Vec<(usize, String)> = a
                        .state
                        .cart_indices()
                        .iter()
                        .enumerate()
                        .map(|(row, &i)| (row, preview_abs(&a, &a.state.items[i])))
                        .collect();
                    spawn_cart_thumbs(u.as_weak(), entries);
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_close_cart(move || {
            if let Some(u) = weak.upgrade() {
                u.set_cart_open(false);
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_cart_remove(move |code| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let code = code.to_string();
                let idx = a.state.index_of(&code);
                a.state.remove_cart(&code);
                if let Some(i) = idx {
                    refresh_cart_flags(&a, &[i]);
                }
                persist(&a.store, &a.state.cart);
                if let Some(u) = weak.upgrade() {
                    rebuild_cart(&u, &a);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_cart_clear(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let previous = a.state.cart_indices();
                let count = previous.len();
                a.state.clear_cart();
                refresh_cart_flags(&a, &previous);
                persist(&a.store, &a.state.cart);
                if let Some(u) = weak.upgrade() {
                    rebuild_cart(&u, &a);
                    if count > 0 {
                        show_toast(&u, &mut a, &format!("Cart cleared ({count})"), "info");
                    }
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_cart_select_all_filtered(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let filtered = a.state.filtered();
                let added = a.state.add_all_filtered();
                refresh_cart_flags(&a, &filtered);
                persist(&a.store, &a.state.cart);
                if let Some(u) = weak.upgrade() {
                    rebuild_cart(&u, &a);
                    show_toast(
                        &u,
                        &mut a,
                        &format!("Added {added} to the cart"),
                        if added > 0 { "ok" } else { "info" },
                    );
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_cart_export(move || {
            let payload = match app_rc.try_borrow() {
                Ok(a) => {
                    let items: Vec<Item> = a
                        .state
                        .cart_indices()
                        .iter()
                        .map(|&i| a.state.items[i].clone())
                        .collect();
                    if items.is_empty() {
                        None
                    } else {
                        Some((export::build_export(&items), items.len()))
                    }
                }
                Err(_) => None,
            };
            let Some((payload, count)) = payload else {
                if let Ok(mut a) = app_rc.try_borrow_mut() {
                    if let Some(u) = weak.upgrade() {
                        show_toast(&u, &mut a, "Cart is empty", "info");
                    }
                }
                return;
            };
            let copied = arboard::Clipboard::new()
                .map(|mut clip| clip.set_text(payload).is_ok())
                .unwrap_or(false);

            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let Some(u) = weak.upgrade() else {
                    return;
                };
                if copied {
                    let previous = a.state.cart_indices();
                    a.state.clear_cart();
                    refresh_cart_flags(&a, &previous);
                    persist(&a.store, &a.state.cart);
                    rebuild_cart(&u, &a);
                    u.set_cart_open(false);
                    show_toast(
                        &u,
                        &mut a,
                        &format!(
                            "Copied {count} code{} to the clipboard (cart cleared)",
                            if count == 1 { "" } else { "s" }
                        ),
                        "ok",
                    );
                } else {
                    show_toast(&u, &mut a, "Copy failed", "error");
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_cart_open_detail(move |code| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                if let Some(pos) = a.state.index_of(&code) {
                    if let Some(u) = weak.upgrade() {
                        u.set_cart_open(false);
                        open_detail(&u, &mut a, pos);
                    }
                }
            }
        });
    }

    // --- rig transport ----------------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_rig_toggle_play(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let Some(s) = a.rig.as_mut() else {
                    return;
                };
                if s.playing {
                    s.playing = false;
                } else {
                    if s.duration > 0.0 && s.time >= s.duration {
                        s.time = 0.0;
                    }
                    s.playing = true;
                }
                s.dirty = true;
                let playing = s.playing;
                if let Some(u) = weak.upgrade() {
                    u.set_rig_playing(playing);
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_rig_restart(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let Some(s) = a.rig.as_mut() else {
                    return;
                };
                s.time = 0.0;
                s.playing = s.duration > 0.0;
                s.dirty = true;
                let (playing, duration) = (s.playing, s.duration);
                if let Some(u) = weak.upgrade() {
                    u.set_rig_playing(playing);
                    u.set_rig_seek_pos(0.0);
                    u.set_rig_time(rig_time_text(0.0, duration).into());
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        ui.on_rig_reset_view(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                if let Some(s) = a.rig.as_mut() {
                    s.camera = raster::Camera::framed(s.aabb_min, s.aabb_max);
                    s.dirty = true;
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_rig_select_clip(move |index| {
            if index < 0 {
                return;
            }
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let Some(s) = a.rig.as_mut() else {
                    return;
                };
                let idx = index as usize;
                if idx >= s.rig.clip_count() {
                    return;
                }
                s.clip = idx;
                s.time = 0.0;
                s.duration = s.rig.clip_duration(idx);
                s.playing = s.duration > 0.0;
                s.dirty = true;
                let (playing, duration) = (s.playing, s.duration);
                if let Some(u) = weak.upgrade() {
                    u.set_rig_clip_index(index);
                    u.set_rig_duration(duration);
                    u.set_rig_seek_pos(0.0);
                    u.set_rig_playing(playing);
                    u.set_rig_time(rig_time_text(0.0, duration).into());
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        ui.on_rig_seek(move |value| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let Some(s) = a.rig.as_mut() else {
                    return;
                };
                let clamped = value.clamp(0.0, s.duration.max(0.0));
                if (clamped - s.time).abs() < 0.02 {
                    return;
                }
                s.time = clamped;
                s.dirty = true;
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        ui.on_rig_orbit(move |dx, dy| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                if let Some(s) = a.rig.as_mut() {
                    s.camera.yaw += dx * 0.01;
                    s.camera.pitch = (s.camera.pitch + dy * 0.01).clamp(-1.4, 1.4);
                    s.dirty = true;
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        ui.on_rig_zoom(move |factor| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                if let Some(s) = a.rig.as_mut() {
                    let d = [
                        s.aabb_max[0] - s.aabb_min[0],
                        s.aabb_max[1] - s.aabb_min[1],
                        s.aabb_max[2] - s.aabb_min[2],
                    ];
                    let span = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-3);
                    s.camera.dist = (s.camera.dist * factor).clamp(span * 0.12, span * 12.0);
                    s.dirty = true;
                }
            }
        });
    }

    // --- window controls (borderless window) ------------------------------
    {
        let weak = ui.as_weak();
        ui.on_win_minimize(move || {
            if let Some(u) = weak.upgrade() {
                u.window().set_minimized(true);
            }
        });
    }
    {
        ui.on_win_close(move || {
            let _ = slint::quit_event_loop();
        });
    }

    // --- toast ------------------------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_toast(move |message, tone| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                if let Some(u) = weak.upgrade() {
                    show_toast(&u, &mut a, &message, &tone);
                }
            }
        });
    }
}
