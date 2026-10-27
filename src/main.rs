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
mod glass;

#[cfg(test)]
mod tests;

slint::include_modules!();

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel, Weak};

use bridge::Dot;
use library::Library;
use model::{preview_badge, slide_label, spec_pairs, Dims, Item, Kind};
use state::{build_taxonomy, column_options, page_size_options, sort_options, AppState};

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

/// Internal resolution of the software-rendered rig frame. 2x super-sampled
/// internally (see `raster::render`), so this is the smallest of the three
/// numbers that matter.
const RIG_W: u32 = 760;
const RIG_H: u32 = 560;

/// Live rig state while the 3D stage is on screen.
struct RigSession {
    rig: Arc<rig::Rig>,
    camera: raster::Camera,
    /// User zoom multiplier over the auto-fitted distance.
    zoom: f32,
    /// User pan, in world space, added to the auto-fitted camera target.
    pan: [f32; 3],
    /// Bind-pose bounds: initial framing, reset, and the ground plane.
    aabb_min: [f32; 3],
    aabb_max: [f32; 3],
    /// Bounds of the last rendered pose, for the camera follow.
    last_bounds: Option<([f32; 3], [f32; 3])>,
    /// Monotonic playback clock.
    last_step: Instant,
    clip: usize,
    time: f32,
    duration: f32,
    /// Showing the skeleton's bind pose (the sidebar's `Preview` row) instead
    /// of one of its animations.
    base_pose: bool,
    playing: bool,
    bones: bool,
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
    /// Slugs present in the open Unity project's import ledger.
    unity_imported: std::collections::HashSet<String>,
    unity_syncing: bool,
    selected: Option<usize>,
    edit: Option<EditState>,
    delete_armed: bool,
    rig: Option<RigSession>,
    toast_timer: Timer,
    delete_timer: Timer,
    probe_timer: Timer,
    rig_timer: Timer,
    /// The capture the frosted backdrop was blurred from (whole desktop).
    glass_shot: Option<glass::Shot>,
    /// Set by the settings sheet's resample (and by a display change): take one
    /// more capture even when the window has not been idle long enough yet.
    glass_recapture: bool,
    /// The blur setting the current image was built with, so moving the slider
    /// re-blurs the stored shot instead of photographing the screen.
    glass_blur_at_render: Option<u8>,
    /// Where the image was last placed (logical px); a tick with no window
    /// move never touches the placement properties.
    glass_placed: Option<(f32, f32, f32, f32)>,
    /// A capture/blur job is running on a worker thread; never more than one.
    glass_pending: bool,
    /// When the window last moved: the backdrop only re-captures once it has
    /// been still for `GLASS_IDLE_REFRESH`.
    glass_still_since: std::time::Instant,
    /// When the last capture started, so the idle refresh runs at most once
    /// per `GLASS_IDLE_REFRESH`.
    glass_last_grab: std::time::Instant,
    /// Window origin at the previous tick, to detect movement.
    glass_last_origin: Option<(i32, i32)>,
    glass_timer: Timer,
    /// Set when the settings change; the store is written once a slider drag
    /// has settled instead of once per pointer move.
    store_pending_at: Option<std::time::Instant>,
    rig_zoom_ready: std::time::Instant,
}

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = RefCell::new(None);
    /// The Slint handle, so the Win32 window proc can dismiss panels on a
    /// right-click before the event reaches any Flickable/button.
    static UI_HANDLE: RefCell<Option<Weak<AppWindow>>> = RefCell::new(None);
    /// A right-down that closed a panel also swallows its matching right-up.
    static SWALLOW_RIGHT_UP: Cell<bool> = const { Cell::new(false) };
}

fn any_panel_open(ui: &AppWindow) -> bool {
    ui.get_detail_open()
        || ui.get_cart_open()
        || ui.get_blender_open()
        || ui.get_unity_open()
        || ui.get_painter_open()
        || ui.get_settings_open()
        || ui.get_pick_open() >= 0
        || ui.get_page_edit_open()
}

fn dismiss_top_panel(ui: &AppWindow) {
    if ui.get_detail_open() {
        ui.invoke_close_detail();
    } else if ui.get_cart_open() {
        ui.invoke_close_cart();
    } else if ui.get_blender_open() {
        ui.invoke_blender_close();
    } else if ui.get_unity_open() {
        ui.invoke_unity_close();
    } else if ui.get_painter_open() {
        ui.invoke_painter_close();
    } else if ui.get_settings_open() {
        ui.set_settings_open(false);
    } else if ui.get_pick_open() >= 0 {
        ui.set_pick_open(-1);
    } else if ui.get_page_edit_open() {
        ui.set_page_edit_open(false);
    }
}

/// Called from `WM_RBUTTONDOWN`; returns true when the message should be
/// swallowed. The actual close is queued so the Win32 proc never re-enters
/// Slint while it is dispatching a window message.
fn begin_right_click_dismiss() -> bool {
    let Some(ui) = UI_HANDLE.with(|cell| cell.borrow().as_ref().and_then(|w| w.upgrade())) else {
        return false;
    };
    if !any_panel_open(&ui) {
        return false;
    }
    SWALLOW_RIGHT_UP.with(|cell| cell.set(true));
    let weak = ui.as_weak();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            dismiss_top_panel(&ui);
        }
    });
    true
}

fn consume_right_click_up() -> bool {
    SWALLOW_RIGHT_UP.with(|cell| cell.replace(false))
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
        in_unity: false,
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

/// Chunk a page's global indices into `GridRow`s of at most `columns` cards.
/// `columns` is one of `column_options()` (3/4/5/6/8); the struct has eight
/// slots, and `n` tells the UI how many of them are real.
fn rows_from_slice(slice: &[usize], columns: usize) -> Vec<GridRow> {
    let columns = columns.clamp(1, 8);
    let mut rows: Vec<GridRow> = Vec::with_capacity(slice.len().div_ceil(columns));
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
            g: slot(6),
            h: slot(7),
            n: remaining.min(columns) as i32,
        });
        k += columns;
    }
    rows
}

fn rebuild_rows(ui: &AppWindow, a: &App) {
    let all = a.state.view_indices();
    let slice = page_slice(&all, a.state.first_item_on_page(), a.state.page_size);
    ui.set_rows(ModelRc::new(VecModel::from(rows_from_slice(
        slice,
        a.state.columns,
    ))));
}

/// Chunk the cart into rows of five preview cards.
fn cart_grid_rows(len: usize) -> Vec<CartGridRow> {
    let mut rows = Vec::with_capacity(len.div_ceil(5));
    let mut k = 0usize;
    while k < len {
        let n = (len - k).min(5);
        let slot = |offset: usize| -> i32 {
            if n > offset { (k + offset) as i32 } else { -1 }
        };
        rows.push(CartGridRow {
            a: slot(0),
            b: slot(1),
            c: slot(2),
            d: slot(3),
            e: slot(4),
            n: n as i32,
        });
        k += 5;
    }
    rows
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
    let grid = cart_grid_rows(rows.len());
    a.cart_model.set_vec(rows);
    ui.set_cart(ModelRc::new(a.cart_model.clone()));
    ui.set_cart_grid(ModelRc::new(VecModel::from(grid)));
    ui.set_cart_count(a.state.cart.len() as i32);
}

fn sync_view(ui: &AppWindow, a: &App) {
    ui.set_active_filters(to_string_model(&a.state.filter_labels()));
    ui.set_matching_label(a.state.matching_label().into());
    ui.set_total_label(a.state.total_label().into());
    ui.set_filters_active(a.state.filters_active());
    ui.set_page(a.state.page as i32);
    ui.set_page_count(a.state.page_count() as i32);
    ui.set_page_size(a.state.page_size as i32);
    ui.set_columns(a.state.columns as i32);
    ui.set_random_active(a.state.random_active());
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
    ui.set_page_sizes(to_int_model(&page_size_options()));
    ui.set_page_size_labels(to_string_model(
        &page_size_options().iter().map(|n| n.to_string()).collect::<Vec<_>>(),
    ));
    ui.set_column_options(to_int_model(&column_options()));
    ui.set_column_labels(to_string_model(
        &column_options().iter().map(|n| n.to_string()).collect::<Vec<_>>(),
    ));
    ui.set_sort_index(a.state.sort.index());

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
    ui.set_rig_base_pose(false);
    ui.set_detail_frame(slint::Image::default());
    ui.set_rig_seek_pos(0.0);
    ui.set_rig_duration(0.0);
}

/// A "nice" world-space grid spacing around `height / 5`, so the floor grid
/// reads at any model scale (1.9 m character -> 0.5 m cells).
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

impl RigSession {
    /// Ground plane + grid + contact shadow for the current pose.
    fn floor(&self) -> raster::Floor {
        let height = (self.aabb_max[1] - self.aabb_min[1]).max(1e-3);
        let (mn, mx) = self.last_bounds.unwrap_or((self.aabb_min, self.aabb_max));
        let dx = (mx[0] - mn[0]).max(0.0);
        let dz = (mx[2] - mn[2]).max(0.0);
        let radius = (0.55 * (dx * dx + dz * dz).sqrt()).max(height * 0.18);
        let lift = (mn[1] - self.aabb_min[1]).max(0.0);
        raster::Floor {
            y: self.aabb_min[1],
            step: grid_step(height),
            reach: (height * 7.0).max(1.5),
            shadow: Some(raster::Shadow {
                center: [(mn[0] + mx[0]) * 0.5, (mn[2] + mx[2]) * 0.5],
                radius,
                strength: 0.55 / (1.0 + lift * 3.0),
            }),
        }
    }
}

fn spawn_rig_render(
    ui: Weak<AppWindow>,
    rig: Arc<rig::Rig>,
    cam: raster::Camera,
    clip: Option<usize>,
    time: f32,
    w: u32,
    h: u32,
    floor: Option<raster::Floor>,
    bones: bool,
) {
    std::thread::spawn(move || {
        // `None` = the bind pose (the sidebar's `Preview` entry).
        let pose = match clip {
            Some(c) => rig.pose(c, time),
            None => rig.pose_bind(),
        };
        let bone_segments = if bones {
            match clip {
                Some(c) => rig.skeleton(c, time),
                None => rig.skeleton_bind(),
            }
        } else {
            Vec::new()
        };
        let overlay = raster::Overlay {
            bones: &bone_segments,
            ..Default::default()
        };
        let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
        raster::render(
            buffer.make_mut_bytes(),
            w,
            h,
            &pose.positions,
            &pose.normals,
            &rig.indices,
            &cam,
            floor,
            &overlay,
        );
        let bounds = (pose.min, pose.max);
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
                        s.last_bounds = Some(bounds);
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
                        let clip = rig.default_clip();
                        let duration = rig.clip_duration(clip);
                        let rows: Vec<ClipItem> = clips
                            .iter()
                            .map(|(name, dur)| ClipItem {
                                name: name.as_str().into(),
                                dur: format!("{dur:.1}s").into(),
                            })
                            .collect();
                        u.set_clips(ModelRc::new(VecModel::from(rows)));
                        u.set_rig_clip_index(clip as i32);
                        u.set_rig_has_3d(true);
                        u.set_rig_playing(true);
                        u.set_rig_duration(duration);
                        u.set_rig_seek_pos(0.0);
                        u.set_rig_time(rig_time_text(0.0, duration).into());
                        let bones = u.get_rig_bones();
                        let base_pose = rig.clip_count() == 0;
                        let session = RigSession {
                            rig: rig.clone(),
                            camera: cam,
                            zoom: 1.0,
                            pan: [0.0; 3],
                            aabb_min: mn,
                            aabb_max: mx,
                            last_bounds: None,
                            last_step: Instant::now(),
                            clip,
                            time: 0.0,
                            duration,
                            base_pose,
                            playing: true,
                            bones,
                            dirty: false,
                            pending: true,
                            w: RIG_W,
                            h: RIG_H,
                        };
                        u.set_rig_base_pose(base_pose);
                        let floor = Some(session.floor());
                        a.rig = Some(session);
                        let weak = u.as_weak();
                        a.rig_timer.start(
                            TimerMode::Repeated,
                            Duration::from_millis(33),
                            move || rig_tick(weak.clone()),
                        );
                        spawn_rig_render(
                            u.as_weak(),
                            rig,
                            cam,
                            if base_pose { None } else { Some(clip) },
                            0.0,
                            RIG_W,
                            RIG_H,
                            floor,
                            bones,
                        );
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

/// One playback tick: advance the clock by the time that actually passed,
/// ease the camera to frame the *deformed* model (so root-motion clips can
/// never walk out of shot), then render one frame if anything moved.
fn rig_tick(ui: Weak<AppWindow>) {
    let Some(u) = ui.upgrade() else {
        return;
    };
    app(|a| {
        let Some(s) = a.rig.as_mut() else {
            return;
        };
        let now = Instant::now();
        let dt = (now - s.last_step).as_secs_f32().clamp(0.0, 0.1);
        s.last_step = now;

        if s.playing && !s.base_pose && s.duration > 0.0 {
            s.time = (s.time + dt).rem_euclid(s.duration);
            s.dirty = true;
        }

        // Camera follow: frame the last pose's bounds, smoothly, plus the
        // user's pan.
        let aspect = s.w as f32 / s.h as f32;
        if let Some((mn, mx)) = s.last_bounds {
            let (target, fit) = raster::Camera::fit(mn, mx, s.camera.yaw, s.camera.pitch, aspect);
            let want = fit * s.zoom;
            let k = 1.0 - (-dt / 0.16).exp();
            let mut moved = false;
            for axis in 0..3 {
                let d = (target[axis] + s.pan[axis]) - s.camera.target[axis];
                moved |= d.abs() > 1e-5;
                s.camera.target[axis] += d * k;
            }
            let dd = want - s.camera.dist;
            moved |= dd.abs() > 1e-4;
            s.camera.dist += dd * k;
            s.dirty |= moved;
        }

        if !s.dirty || s.pending {
            return;
        }
        s.dirty = false;
        s.pending = true;
        let (cam, clip, time, w, h) = (s.camera, s.clip, s.time, s.w, s.h);
        let rig = s.rig.clone();
        let duration = s.duration;
        let floor = Some(s.floor());
        let bones = s.bones;
        let base_pose = s.base_pose;
        if base_pose {
            u.set_rig_time("bind pose".to_string().into());
        } else {
            u.set_rig_seek_pos(time);
            u.set_rig_time(rig_time_text(time, duration).into());
        }
        spawn_rig_render(
            u.as_weak(),
            rig,
            cam,
            if base_pose { None } else { Some(clip) },
            time,
            w,
            h,
            floor,
            bones,
        );
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
            if dot.state == "ok" {
                request_unity_sync(ui.as_weak(), false);
            } else {
                clear_unity_imports(ui);
            }
        }
        _ => {
            ui.set_painter_state(dot.state.as_str().into());
            ui.set_painter_status(dot.text.as_str().into());
        }
    }
}

fn update_unity_rows(a: &mut App) {
    for i in 0..a.items.row_count() {
        let Some(it) = a.state.items.get(i) else {
            continue;
        };
        let flag = a.unity_imported.contains(&it.slug);
        if let Some(mut row) = a.items.row_data(i) {
            if row.in_unity != flag {
                row.in_unity = flag;
                a.items.set_row_data(i, row);
            }
        }
    }
}

fn clear_unity_imports(ui: &AppWindow) {
    app(|a| {
        if !a.unity_imported.is_empty() {
            a.unity_imported.clear();
            update_unity_rows(a);
        }
    });
    ui.set_unity_sync_state("Unity offline".into());
}

/// Query Unity's import ledger and paint the green dots. Called from the 15 s
/// health probe and from the manual "Sync Unity" action.
fn request_unity_sync(ui: Weak<AppWindow>, toast: bool) {
    let Some((base, syncing)) = app(|a| (a.store.unity_base.clone(), a.unity_syncing)) else {
        return;
    };
    if syncing {
        return;
    }
    app(|a| a.unity_syncing = true);
    if let Some(u) = ui.upgrade() {
        u.set_unity_sync_state("syncing…".into());
    }
    std::thread::spawn(move || {
        let result = bridge::unity_imported_slugs(&base);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(u) = ui.upgrade() else {
                return;
            };
            app(|a| {
                a.unity_syncing = false;
                match result {
                    Ok(slugs) => {
                        let set: std::collections::HashSet<String> =
                            slugs.into_iter().collect();
                        let count = set.len();
                        a.unity_imported = set;
                        update_unity_rows(a);
                        u.set_unity_sync_state(format!("{count} imported").into());
                        if toast {
                            show_toast(
                                &u,
                                a,
                                &format!("Unity: {count} imported asset(s)"),
                                "ok",
                            );
                        }
                    }
                    Err(e) => {
                        u.set_unity_sync_state("sync failed".into());
                        if toast {
                            let detail: String = e.chars().take(140).collect();
                            show_toast(&u, a, &format!("Unity sync failed: {detail}"), "error");
                        }
                    }
                }
            });
        });
    });
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
    fn details(payload: &serde_json::Value, key: &str) -> (usize, Option<String>) {
        let Some(list) = payload.get(key).and_then(|v| v.as_array()) else {
            return (0, None);
        };
        let first = list.first().and_then(|entry| {
            if let Some(pair) = entry.as_array() {
                let who = pair.first().and_then(|v| v.as_str()).unwrap_or("");
                let what = pair.get(1).and_then(|v| v.as_str()).unwrap_or("");
                Some(if who.is_empty() {
                    what.to_string()
                } else {
                    format!("{who}: {what}")
                })
            } else {
                entry.as_str().map(str::to_string)
            }
        });
        (list.len(), first)
    }

    match res {
        Err(e) => (e.clone(), "error".to_string()),
        Ok(body) => {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
                return (
                    format!("Blender bridge replied ({fallback})"),
                    "ok".to_string(),
                );
            };

            let ok = v.get("status").and_then(|s| s.as_str()) == Some("ok")
                || v.get("ok").and_then(|b| b.as_bool()) == Some(true);
            if !ok {
                let message = v
                    .get("error")
                    .or_else(|| v.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Import failed");
                return (message.to_string(), "error".to_string());
            }

            if mode == "open" {
                let opened = v
                    .get("opened")
                    .and_then(|x| x.as_i64())
                    .unwrap_or(fallback as i64);
                return (format!("Opened {opened} in a new Blender window"), "ok".into());
            }

            // The addon replies `{"status":"ok","result":{...}}`; accept a flat
            // `{...}` too so an older bridge build still reports correctly.
            let payload = v.get("result").unwrap_or(&v);
            let imported = payload
                .get("count")
                .and_then(|c| c.as_i64())
                .or_else(|| {
                    payload
                        .get("imported")
                        .and_then(|a| a.as_array())
                        .map(|a| a.len() as i64)
                })
                .unwrap_or(fallback as i64);

            let (failed, first_error) = details(payload, "errors");
            let (bind_failed, first_bind_error) = details(payload, "bind_errors");
            if failed > 0 {
                let detail = first_error.unwrap_or_else(|| "unknown error".to_string());
                let detail: String = detail.chars().take(140).collect();
                let message = if imported > 0 {
                    format!("Imported {imported}, {failed} failed: {detail}")
                } else {
                    format!("Import failed: {detail}")
                };
                return (message, "error".to_string());
            }
            if bind_failed > 0 {
                let detail = first_bind_error.unwrap_or_else(|| "unknown error".to_string());
                let detail: String = detail.chars().take(140).collect();
                return (
                    format!("Imported {imported}; bridge binding failed: {detail}"),
                    "error".to_string(),
                );
            }
            (format!("Imported {imported} to Blender"), "ok".into())
        }
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

/* ------------------------------ deepseek -------------------------------- */

/// One OpenAI-compatible chat call to DeepSeek. `body` is sent as-is so callers
/// control image blocks / JSON mode / thinking.
fn deepseek_chat(base: &str, key: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let resp = ureq::post(&url)
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {key}"))
        .timeout(std::time::Duration::from_secs(90))
        .send_json(body.clone())
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => {
                let text = r.into_string().unwrap_or_default();
                format!("HTTP {code}: {}", text.chars().take(200).collect::<String>())
            }
            other => other.to_string(),
        })?;
    resp.into_json::<serde_json::Value>()
        .map_err(|e| format!("bad JSON reply: {e}"))
}

fn deepseek_ping(base: &str, key: &str, model: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": "ping" }],
        "max_tokens": 4,
        "thinking": { "type": "disabled" }
    });
    let doc = deepseek_chat(base, key, &body)?;
    let text = doc["choices"][0]["message"]["content"].as_str().unwrap_or("");
    Ok(if text.trim().is_empty() { "ok".to_string() } else { text.trim().chars().take(24).collect() })
}

fn tag_prompt(item: &Item, taxonomy: &crate::state::Taxonomy, vocab: &[String]) -> String {
    let kind_key = if item.kind == "material" { "material" } else { "model" };
    let tax = taxonomy
        .by_kind
        .get(kind_key)
        .map(|m| serde_json::to_string(m).unwrap_or_default())
        .unwrap_or_else(|| "{}".to_string());
    let dims = item
        .dimensions_m
        .as_ref()
        .map(|d| format!("{:.2}x{:.2}x{:.2} m", d.width, d.height, d.depth))
        .unwrap_or_else(|| "?".to_string());
    format!(
        "You classify 3D game assets for a local asset library. The attached image is a clay render \
of the asset (grey material, neutral background). Answer with JSON only - no prose.\n\n\
TAXONOMY (l1 -> allowed l2 values): {tax}\n\
EXISTING TAG VOCABULARY (prefer these when they fit): {vocab}\n\
ASSET: slug={slug}, current tags={tags}, triangles={tris}, size={dims}\n\n\
Rules: l1 must be one of the taxonomy keys; l2 one of that l1's values; tags 3-8 lowercase \
snake_case English, specific and useful for search; description one English sentence <=140 chars; \
confidence 0..1.\n\
Return exactly: {{\"l1\": \"...\", \"l2\": \"...\", \"tags\": [\"...\"], \"description\": \"...\", \"confidence\": 0.0}}",
        tax = tax,
        vocab = vocab.join(", "),
        slug = item.slug,
        tags = item.tags.join(", "),
        tris = item.tri_count,
        dims = dims,
    )
}

fn deepseek_tag_call(
    base: &str,
    key: &str,
    model: &str,
    prompt: &str,
    image: &std::path::Path,
) -> Result<serde_json::Value, String> {
    use base64::Engine;
    let bytes = std::fs::read(image).map_err(|e| format!("preview unreadable: {e}"))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let body = serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": {
                    "url": format!("data:image/png;base64,{b64}"),
                    "detail": "low"
                }}
            ]
        }],
        "response_format": { "type": "json_object" },
        "max_tokens": 500,
        "temperature": 0.2,
        "thinking": { "type": "disabled" }
    });
    let doc = deepseek_chat(base, key, &body)?;
    let text = doc["choices"][0]["message"]["content"].as_str().unwrap_or("");
    serde_json::from_str::<serde_json::Value>(text.trim())
        .map_err(|_| format!("model did not return JSON: {}", text.chars().take(120).collect::<String>()))
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

/* ----------------------- window drag (self-driven) ---------------------- */

/// Native `HWND` of a Slint window, or `None` on a non-Win32/unsupported
/// backend.
///
/// Shared by the frosted-glass setup and the self-driven window drag, so the
/// handle is resolved through Slint's `raw-window-handle` integration in one
/// place (never by window title, which would be ambiguous).
fn hwnd_of(window: &slint::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle();
    let raw = HasWindowHandle::window_handle(&handle).ok()?;
    match raw.as_raw() {
        RawWindowHandle::Win32(win32) => Some(win32.hwnd.get()),
        _ => None,
    }
}

/// Absolute window top-left for a cursor position, relative to a drag anchor.
///
/// Pure on purpose: this is the math the regression tests pin. The window
/// follows the cursor delta exactly once, independent of how many move events
/// have already been delivered, so repeating a move for the same cursor is a
/// no-op and the window can never accumulate motion.
fn drag_target(
    anchor_window: (i32, i32),
    anchor_cursor: (i32, i32),
    cursor: (i32, i32),
) -> (i32, i32) {
    (
        anchor_window.0 + (cursor.0 - anchor_cursor.0),
        anchor_window.1 + (cursor.1 - anchor_cursor.1),
    )
}

/// Self-driven window move state for the main Slint window.
///
/// Deliberately never hands the drag to the OS (no OS modal move loop, which
/// would re-evaluate the frame and restore the caption). The press records an
/// absolute anchor: the physical screen cursor and the window's physical outer
/// top-left. Every move then computes one absolute target from that anchor and
/// applies it with `SetWindowPos`, so repeating a move for the same cursor is a
/// no-op and the window can never accumulate motion.
pub struct WindowDrag {
    dragging: Cell<bool>,
    anchor_cursor: Cell<(i32, i32)>,
    anchor_window: Cell<(i32, i32)>,
}

impl WindowDrag {
    pub fn new() -> Self {
        Self {
            dragging: Cell::new(false),
            anchor_cursor: Cell::new((0, 0)),
            anchor_window: Cell::new((0, 0)),
        }
    }

    /// Record the absolute drag anchor. Never moves the window.
    ///
    /// Both the cursor and the native window rect must be readable; otherwise
    /// the drag is left inactive so a stale/default anchor is never used.
    pub fn start(&self, window: &slint::Window) {
        let Some(hwnd) = hwnd_of(window) else {
            self.dragging.set(false);
            return;
        };
        let Some(cursor) = cursor_position() else {
            self.dragging.set(false);
            return;
        };
        let Some(origin) = window_origin_hwnd(hwnd) else {
            self.dragging.set(false);
            return;
        };
        self.anchor_cursor.set(cursor);
        self.anchor_window.set(origin);
        self.dragging.set(true);
    }

    /// Treat the current cursor position as an absolute drag position.
    pub fn move_to(&self, window: &slint::Window) {
        if !self.dragging.get() {
            return;
        }
        let Some(cursor) = cursor_position() else {
            return;
        };
        self.move_to_cursor(window, cursor);
    }

    /// Diagnostic entry point: identical absolute path with an explicit cursor.
    ///
    /// Reads no window position and sends no synthetic input, so a debug hook
    /// exercises exactly the production math.
    pub fn move_to_cursor(&self, window: &slint::Window, cursor: (i32, i32)) {
        if !self.dragging.get() {
            return;
        }
        let Some(hwnd) = hwnd_of(window) else {
            return;
        };
        let target = drag_target(
            self.anchor_window.get(),
            self.anchor_cursor.get(),
            cursor,
        );
        set_origin_hwnd(hwnd, target);
    }

    pub fn end(&self) {
        self.dragging.set(false);
    }

    /// The captured `(cursor, window)` anchor, for diagnostics only.
    #[allow(dead_code)]
    pub fn anchor(&self) -> ((i32, i32), (i32, i32)) {
        (self.anchor_cursor.get(), self.anchor_window.get())
    }
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinPoint {
    x: i32,
    y: i32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn GetCursorPos(point: *mut WinPoint) -> i32;
    fn GetWindowRect(hwnd: isize, rect: *mut WinRect) -> i32;
    fn SetWindowPos(
        hwnd: isize,
        insert_after: isize,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
}

/// Physical screen cursor position, or `None` when `GetCursorPos` fails.
#[cfg(windows)]
fn cursor_position() -> Option<(i32, i32)> {
    let mut point = WinPoint::default();
    (unsafe { GetCursorPos(&mut point) } != 0).then_some((point.x, point.y))
}

#[cfg(not(windows))]
fn cursor_position() -> Option<(i32, i32)> {
    None
}

/// Physical top-left of a raw window rect, or `None` when the read fails.
#[cfg(windows)]
fn window_origin_hwnd(hwnd: isize) -> Option<(i32, i32)> {
    let mut rect = WinRect::default();
    (unsafe { GetWindowRect(hwnd, &mut rect) } != 0).then_some((rect.left, rect.top))
}

#[cfg(not(windows))]
fn window_origin_hwnd(_hwnd: isize) -> Option<(i32, i32)> {
    None
}

/// Move a window to an absolute physical top-left. Never relative, never
/// resized, never re-activated, never re-ordered in the Z axis.
#[cfg(windows)]
fn set_origin_hwnd(hwnd: isize, (x, y): (i32, i32)) {
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    unsafe {
        SetWindowPos(
            hwnd,
            0,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

#[cfg(not(windows))]
fn set_origin_hwnd(_hwnd: isize, _target: (i32, i32)) {}

/// Window chrome for the frameless window: rounded corners, dark DWM chrome, no
/// border, and the frameless style/message handling that keeps it that way.
///
/// Deliberately no system backdrop. The DWM acrylic would require
/// `DwmExtendFrameIntoClientArea`, and extending the frame makes DWM paint its
/// caption band across the top ~30px of the client — a hard horizontal edge that
/// cannot be recoloured (`DWMWA_CAPTION_COLOR`) or disabled
/// (`DWMWA_NCRENDERING_POLICY`); both are rejected with `E_HANDLE` on the build
/// this was measured against. The window instead paints its own translucent
/// background (`Theme.window-bg`) over the `glass.rs` in-process blur.
/// Everything here is best-effort.
#[cfg(windows)]
fn apply_backdrop(ui: &AppWindow) -> bool {
    const GWL_STYLE: i32 = -16;
    const WS_CAPTION: isize = 0x00C0_0000;
    const WS_THICKFRAME: isize = 0x0004_0000;

    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: u32 = 2;

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

    /// `DWM_BLURBEHIND`. With `DWM_BB_ENABLE` set and a null `region`, DWM blurs
    /// the whole window rather than an (empty) region.
    #[repr(C)]
    struct BlurBehind {
        flags: u32,
        enable: i32,
        region: isize,
        transition_on_maximized: i32,
    }
    const DWM_BB_ENABLE: u32 = 0x0000_0001;

    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const u8, size: u32) -> i32;
        fn DwmEnableBlurBehindWindow(hwnd: isize, data: *const BlurBehind) -> i32;
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
    const WM_RBUTTONDOWN: u32 = 0x0204;
    const WM_RBUTTONUP: u32 = 0x0205;
    const WM_NCCALCSIZE: u32 = 0x0083;
    const WM_NCHITTEST: u32 = 0x0084;
    const WM_GETMINMAXINFO: u32 = 0x0024;
    const WM_WINDOWPOSCHANGING: u32 = 0x0046;
    const SWP_NOSIZE: u32 = 0x0001;
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;

    // Frameless window styles + the messages that keep it frameless. Ported from
    // bilibili-slint's `win.rs`, which solves the same problem on this stack.
    const GWL_EXSTYLE: i32 = -20;
    const WS_DLGFRAME: isize = 0x0040_0000;
    const WS_BORDER: isize = 0x0080_0000;
    const WS_EX_WINDOWEDGE: isize = 0x0000_0100;
    const WS_EX_CLIENTEDGE: isize = 0x0000_0200;
    const WS_EX_STATICEDGE: isize = 0x0002_0000;
    const WS_EX_DLGMODALFRAME: isize = 0x0000_0001;
    const WM_STYLECHANGING: u32 = 0x007C;
    const WM_STYLECHANGED: u32 = 0x007D;
    const WM_NCACTIVATE: u32 = 0x0086;
    const WM_NCPAINT: u32 = 0x0085;
    const WM_NCUAHDRAWCAPTION: u32 = 0x00AE;
    const WM_NCUAHDRAWFRAME: u32 = 0x00AF;
    const WM_DISPLAYCHANGE: u32 = 0x007E;
    const SWP_FRAMECHANGED: u32 = 0x0020;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;

    /// `STYLESTRUCT` — the payload of `WM_STYLECHANGING` / `-CHANGED`.
    #[repr(C)]
    struct StyleStruct {
        style_old: u32,
        style_new: u32,
    }

    /// Re-assert the frameless style words.
    ///
    /// `WS_CAPTION`/`WS_DLGFRAME`/`WS_BORDER` are cleared and `WS_THICKFRAME` is
    /// kept (re-added if something dropped it) because the DWM shadow and snap
    /// ride on it. On the extended side the four non-client edge styles are
    /// cleared; `WS_EX_WINDOWEDGE` is deliberately excluded from the change test
    /// because USER32 re-adds it to every top-level window by itself, and
    /// treating that as a change would make this non-idempotent.
    ///
    /// Idempotent and cheap: when the words already have the wanted value this
    /// issues no `SetWindowLongPtrW`/`SetWindowPos` at all, so the glass retry
    /// chain can call it unconditionally as a watchdog.
    unsafe fn normalise_frameless(hwnd: isize) {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let want_style = (style & !(WS_CAPTION | WS_DLGFRAME | WS_BORDER)) | WS_THICKFRAME;
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want_ex = ex_style
            & !(WS_EX_WINDOWEDGE | WS_EX_CLIENTEDGE | WS_EX_STATICEDGE | WS_EX_DLGMODALFRAME);
        let ex_needs_change =
            ex_style & (WS_EX_CLIENTEDGE | WS_EX_STATICEDGE | WS_EX_DLGMODALFRAME) != 0;
        if want_style == style && !ex_needs_change {
            return;
        }
        if want_style != style {
            SetWindowLongPtrW(hwnd, GWL_STYLE, want_style);
        }
        if want_ex != ex_style {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want_ex);
        }
        SetWindowPos(
            hwnd,
            0,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }


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
        // Right-click dismisses the top-most panel before it reaches a
        // Flickable or button, which would otherwise swallow it.
        if msg == WM_RBUTTONDOWN && crate::begin_right_click_dismiss() {
            return 0;
        }
        if msg == WM_RBUTTONUP && crate::consume_right_click_up() {
            return 0;
        }

        // Client area == whole window for BOTH the "proposed parameters" form
        // (wparam != 0) and the plain window-rect form (wparam == 0). Falling
        // through to the default proc here is what computes a standard caption +
        // border frame — that is where the native title bar comes from.
        if msg == WM_NCCALCSIZE {
            return 0;
        }

        // Claim every activation as handled without calling the default proc: it
        // repaints the caption/frame on activate AND deactivate, so the native
        // title bar flashes in whenever another window takes focus.
        if msg == WM_NCACTIVATE {
            normalise_frameless(hwnd);
            return 1;
        }

        // There is no non-client area to paint (see WM_NCCALCSIZE). The two
        // undocumented theme messages draw a fake caption/frame over the client.
        if msg == WM_NCPAINT || msg == WM_NCUAHDRAWCAPTION || msg == WM_NCUAHDRAWFRAME {
            return 0;
        }

        // Filter the frame bits out of any incoming style change: winit re-applies
        // WS_CAPTION after show(), and a stale instance could too. Do NOT return —
        // the filtered change still has to be applied by the default proc below.
        if msg == WM_STYLECHANGING && lparam != 0 {
            let change = lparam as *mut StyleStruct;
            if wparam as i32 == GWL_STYLE {
                (*change).style_new &= !((WS_CAPTION | WS_DLGFRAME | WS_BORDER) as u32);
            } else if wparam as i32 == GWL_EXSTYLE {
                (*change).style_new &= !((WS_EX_WINDOWEDGE
                    | WS_EX_CLIENTEDGE
                    | WS_EX_STATICEDGE
                    | WS_EX_DLGMODALFRAME) as u32);
            }
        }

        // Re-assert after the change landed, in case it bypassed the filter above.
        if msg == WM_STYLECHANGED {
            let result = CallWindowProcW(
                OLD_WNDPROC.load(std::sync::atomic::Ordering::Relaxed),
                hwnd,
                msg,
                wparam,
                lparam,
            );
            normalise_frameless(hwnd);
            return result;
        }

        // A monitor was added / removed / rearranged: the stored capture no
        // longer describes the desktop, so ask for one more (see glass_step).
        if msg == WM_DISPLAYCHANGE {
            GLASS_DISPLAY_CHANGED.store(true, std::sync::atomic::Ordering::Relaxed);
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
    let Some(hwnd) = hwnd_of(ui.window()) else {
        glass_log("glass: FAILED no Win32 handle");
        return false;
    };
    glass_log(&format!("glass: hwnd={hwnd}"));
    if hwnd == 0 {
        return false;
    }

    let set_u32 = |attr: u32, value: u32| -> i32 {
        unsafe { DwmSetWindowAttribute(hwnd, attr, (&value as *const u32).cast(), 4) }
    };

    unsafe {
        // See-through via the LEGACY blur-behind call, not the modern DWM
        // backdrop. Measured on this machine (and independently in the sibling
        // StudyApp project): DwmExtendFrameIntoClientArea + DWMWA_SYSTEMBACKDROP_TYPE
        // move the window onto DWM's backdrop composition path, which then
        // composites OPAQUELY - and DWM also paints its caption band across the
        // top ~30px of the client, a hard horizontal edge that cannot be
        // recoloured or switched off. DwmEnableBlurBehindWindow with a null blur
        // region instead makes the whole window show what is behind it (alpha
        // honoured by the compositor), so the desktop genuinely shows through
        // (measured 68/336 sampled pixels change when the window moves over
        // different desktop content). The Gaussian frost is NOT this call's
        // doing - `glass.rs` draws the blurred snapshot in-process.
        let blur = BlurBehind {
            flags: DWM_BB_ENABLE,
            enable: 1,
            region: 0,
            transition_on_maximized: 0,
        };
        let _ = DwmEnableBlurBehindWindow(hwnd, &blur);

        // 1. rounded corners + dark DWM chrome, and no border
        let _ = set_u32(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
        let _ = set_u32(DWMWA_USE_IMMERSIVE_DARK_MODE, 1);
        let _ = set_u32(DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE);

        // 2. enforce frameless while keeping native resize, then install the
        //    window procedure that keeps it that way
        normalise_frameless(hwnd);
        if OLD_WNDPROC.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            let proc = frameless_proc
                as unsafe extern "system" fn(isize, u32, usize, isize) -> isize;
            let old = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc as usize as isize);
            if old != 0 {
                OLD_WNDPROC.store(old, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // 3. never keep an always-on-top bit (a leftover from an external tool
        //    would pin the window above every other app)
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

/* ------------------------------ glass loop ------------------------------ */

/// Set by `WM_DISPLAYCHANGE` (monitor hot-plug, removal, rearrangement or a
/// resolution change): the stored capture no longer describes the desktop, so
/// the loop takes one more. Cleared when it does.
#[cfg(windows)]
static GLASS_DISPLAY_CHANGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// What the frosted backdrop has to do this tick.
#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GlassWork {
    /// Nothing: the image is current.
    None,
    /// Photograph the desktop once more (`glass::capture`).
    Capture,
    /// Re-run downscale + Gaussian + the image upload from the stored shot.
    /// No screen access at all.
    Reblur,
}

/// The window must have been still this long before the backdrop re-captures,
/// and it re-captures at most this often while it stays still: "stop moving,
/// refresh" — a drag never touches the screen, so it can never lag.
#[cfg(windows)]
const GLASS_IDLE_REFRESH: Duration = Duration::from_millis(500);

/// Backdrop policy, in one pure place so the tests can pin it down:
/// * **never** touch the screen while the window is moving — that is what made
///   dragging lag when every tick re-photographed the desktop;
/// * once it has been still for `GLASS_IDLE_REFRESH` (and at most that often
///   afterwards) capture again, so the frost follows what is behind the window
///   instead of freezing at the startup shot;
/// * a blur change re-blurs the stored shot — no screen access;
/// * the settings' resample (or a display change) captures even when the window
///   has not been idle long enough yet.
#[cfg(windows)]
fn glass_work(
    has_shot: bool,
    moving: bool,
    refresh_due: bool,
    blur_changed: bool,
    recapture: bool,
) -> GlassWork {
    if moving {
        return GlassWork::None;
    }
    if recapture || !has_shot {
        return GlassWork::Capture;
    }
    if blur_changed {
        return GlassWork::Reblur;
    }
    if refresh_due {
        return GlassWork::Capture;
    }
    GlassWork::None
}

/// One iteration of the backdrop refresh loop (see the 33ms timer in `main`).
///
/// The loop does three jobs on every tick, each of them cheap when nothing
/// changed:
/// - writes a settled settings change to the store (slider drags fire every
///   frame, so the write is debounced through `App::store_pending_at`);
/// - runs the capture rule above; the capture + blur themselves run on a worker
///   thread, so a 0.5s refresh cadence cannot stall the UI;
/// - keeps the whole-desktop image registered with the desktop (see
///   [`glass_place`]), so a move / resize only changes that placement.
fn glass_step(weak: &Weak<AppWindow>, app_rc: &Rc<RefCell<App>>) {
    let Some(ui) = weak.upgrade() else {
        return;
    };
    let theme = ui.global::<Theme>();
    let blur = theme.get_glass_blur();
    let mut transparency = theme.get_glass_transparency();
    let dark = theme.get_dark_mode();
    let now = Instant::now();

    let Ok(mut a) = app_rc.try_borrow_mut() else {
        return;
    };

    // The Dark/Light tab flipped. Both themes carry their own tint strength, so
    // remember the outgoing slider value and hand the incoming mode its stored
    // one instead of dragging the old number across.
    if dark != a.store.dark {
        let previous_dark = a.store.dark;
        a.store.set_transparency(previous_dark, transparency);
        a.store.dark = dark;
        transparency = a.store.transparency(dark);
        theme.set_glass_transparency(transparency);
        a.store_pending_at = Some(now + Duration::from_millis(650));
    }

    if let Some(at) = a.store_pending_at {
        if now >= at {
            a.store.dark = dark;
            a.store.set_transparency(dark, transparency);
            a.store.glass_blur = blur;
            store::save(&a.store);
            a.store_pending_at = None;
        }
    }

    if blur <= 0.5 {
        // Slider at zero: there is no blur layer at all. The tint alone sits
        // over the compositor's transparent window — a sharp, always-live
        // desktop — and the loop stays idle. The next non-zero value takes a
        // fresh capture rather than reusing a shot that may be minutes old.
        if ui.get_glass_backdrop_valid() {
            ui.set_glass_backdrop(slint::Image::default());
            ui.set_glass_backdrop_valid(false);
        }
        a.glass_shot = None;
        a.glass_blur_at_render = None;
        a.glass_last_origin = None;
        return;
    }

    #[cfg(windows)]
    {
        let Some(hwnd) = hwnd_of(ui.window()) else {
            return;
        };
        if GLASS_DISPLAY_CHANGED.swap(false, std::sync::atomic::Ordering::Relaxed) {
            a.glass_recapture = true;
        }
        let Some(origin) = glass::window_origin(hwnd) else {
            return;
        };
        // No origin yet (first tick) counts as "just moved": let the window
        // settle before the first photograph.
        let moving = a.glass_last_origin != Some(origin);
        if moving {
            a.glass_still_since = now;
        }
        a.glass_last_origin = Some(origin);
        glass_place(&ui, &mut a);

        if a.glass_pending {
            return;
        }
        let blur = blur.round().clamp(0.0, 100.0) as u8;
        let refresh_due = now.saturating_duration_since(a.glass_still_since) >= GLASS_IDLE_REFRESH
            && now.saturating_duration_since(a.glass_last_grab) >= GLASS_IDLE_REFRESH;
        let work = glass_work(
            a.glass_shot.is_some(),
            moving,
            refresh_due,
            a.glass_blur_at_render != Some(blur),
            a.glass_recapture,
        );
        let capture = matches!(work, GlassWork::Capture);
        if capture || matches!(work, GlassWork::Reblur) {
            if capture {
                a.glass_recapture = false;
                a.glass_last_grab = now;
            }
            a.glass_pending = true;
            let shot = a.glass_shot.take();
            spawn_glass_grab(ui.as_weak(), hwnd, blur, capture, shot);
        }
    }
}

/// Capture (and/or blur) off the UI thread. A whole-desktop BitBlt plus the
/// Gaussian costs tens of milliseconds: doing that on the UI thread twice a
/// second would hitch, and doing it while the window moves is what made the
/// old backdrop lag. One job at a time (`App::glass_pending`).
#[cfg(windows)]
fn spawn_glass_grab(
    ui: Weak<AppWindow>,
    hwnd: isize,
    blur: u8,
    capture: bool,
    previous: Option<glass::Shot>,
) {
    std::thread::spawn(move || {
        let shot = if capture {
            match glass::capture(hwnd) {
                Some((shot, hide)) => {
                    glass_log(&format!("glass: capture ok (hide={})", hide.id()));
                    Some(shot)
                }
                None => {
                    glass_log("glass: capture failed, keeping the previous shot");
                    previous
                }
            }
        } else {
            previous
        };
        let Some(shot) = shot else {
            let _ = slint::invoke_from_event_loop(move || {
                app(|a| a.glass_pending = false);
            });
            return;
        };
        let pixels = glass::render_pixels(&shot, blur);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(u) = ui.upgrade() else {
                return;
            };
            u.set_glass_backdrop(slint::Image::from_rgba8(pixels));
            u.set_glass_backdrop_valid(true);
            app(|a| {
                a.glass_pending = false;
                a.glass_shot = Some(shot);
                a.glass_blur_at_render = Some(blur);
                glass_place(&u, a);
            });
        });
    });
}

/// Placement of the whole-desktop capture inside the window, in logical
/// pixels: the capture origin relative to the window origin, then the capture
/// size.
///
/// Keeping the image at the capture origin minus the current window origin is
/// what pins it to the desktop — a move only changes this offset, never the
/// capture (see `glass_work`). `scale` converts the physical coordinates to
/// the logical units Slint lays out in, and a degenerate 0 clamps to 1 so the
/// image can never collapse.
#[cfg(windows)]
fn backdrop_placement(
    shot_origin: (i32, i32),
    window_origin: (i32, i32),
    shot: (usize, usize),
    scale: f32,
) -> (f32, f32, f32, f32) {
    let scale = if scale <= 0.0 { 1.0 } else { scale };
    (
        (shot_origin.0 - window_origin.0) as f32 / scale,
        (shot_origin.1 - window_origin.1) as f32 / scale,
        shot.0 as f32 / scale,
        shot.1 as f32 / scale,
    )
}

/// Keep the backdrop registered with the desktop: place the whole-desktop
/// image at the capture origin minus the window origin, in the same tick (and
/// in the same pointer event, see the drag wiring) that moved the window.
/// Does nothing until a shot exists or when the window has not moved, so the
/// 33ms loop can call it freely.
#[cfg(windows)]
fn glass_place(ui: &AppWindow, a: &mut App) {
    let Some((shot_origin, sw, sh)) = a
        .glass_shot
        .as_ref()
        .map(|s| (s.origin, s.width, s.height))
    else {
        return;
    };
    let Some(hwnd) = hwnd_of(ui.window()) else {
        return;
    };
    let Some(window_origin) = glass::window_origin(hwnd) else {
        return;
    };
    let placed = backdrop_placement(shot_origin, window_origin, (sw, sh), ui.window().scale_factor());
    if a.glass_placed == Some(placed) {
        return;
    }
    a.glass_placed = Some(placed);
    ui.set_glass_x(placed.0);
    ui.set_glass_y(placed.1);
    ui.set_glass_w(placed.2);
    ui.set_glass_h(placed.3);
}

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
        unity_imported: std::collections::HashSet::new(),
        unity_syncing: false,
        selected: None,
        edit: None,
        delete_armed: false,
        rig: None,
        toast_timer: Timer::default(),
        delete_timer: Timer::default(),
        probe_timer: Timer::default(),
        rig_timer: Timer::default(),
        glass_shot: None,
        glass_recapture: false,
        glass_blur_at_render: None,
        glass_placed: None,
        glass_pending: false,
        glass_still_since: std::time::Instant::now(),
        glass_last_grab: std::time::Instant::now(),
        glass_last_origin: None,
        glass_timer: Timer::default(),
        store_pending_at: None,
        rig_zoom_ready: std::time::Instant::now(),
    }));
    APP.with(|cell| {
        *cell.borrow_mut() = Some(app_rc.clone());
    });

    let ui = AppWindow::new()?;
    UI_HANDLE.with(|cell| *cell.borrow_mut() = Some(ui.as_weak()));

    // The .slint window no longer pins width/height (a constant binding froze the
    // surface size); set the initial size here, before the first show().
    ui.window().set_size(slint::PhysicalSize::new(1440u32, 900u32));

    // Restore the persisted appearance before the first frame: these are the
    // values the Settings sheet edits live (see `Store`).
    {
        let theme = ui.global::<Theme>();
        if let Ok(a) = app_rc.try_borrow() {
            theme.set_dark_mode(a.store.dark);
            theme.set_glass_transparency(a.store.transparency(a.store.dark));
            theme.set_glass_blur(a.store.glass_blur);
        }
    }


    // Initial data models.
    if let Ok(a) = app_rc.try_borrow() {
        ui.set_items(ModelRc::new(a.items.clone()));
        ui.set_cart(ModelRc::new(a.cart_model.clone()));
        ui.set_slides(ModelRc::new(a.slides_model.clone()));
    }
    ui.set_clips(ModelRc::new(VecModel::from(Vec::<ClipItem>::new())));
    ui.set_spec(ModelRc::new(VecModel::from(Vec::<SpecRow>::new())));
    ui.set_sort_options(to_string_model(&sort_options()));
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
    ui.set_unity_sync_state("checking…".into());
    ui.set_painter_status("Checking…".into());
    if let Ok(a) = app_rc.try_borrow() {
        ui.set_blender_root(a.store.blender_root.clone().into());
        ui.set_blender_base(a.store.blender_base.clone().into());
        ui.set_unity_base(a.store.unity_base.clone().into());
        ui.set_painter_base(a.store.painter_base.clone().into());
        ui.set_painter_bridge(a.store.painter_bridge.clone().into());
        ui.set_painter_root(a.store.painter_root.clone().into());
        ui.set_specs_open(a.store.specs_open);
        ui.set_category_open(a.store.category_open);
        ui.set_tags_open(a.store.tags_open);
        ui.set_deepseek_key(a.store.deepseek_api_key.clone().into());
        ui.set_deepseek_model(a.store.deepseek_model.clone().into());
        ui.set_deepseek_state(
            if a.store.deepseek_api_key.trim().is_empty() {
                "no key".into()
            } else {
                format!("key set · {}", a.store.deepseek_model).into()
            },
        );
        sync_all(&ui, &a);
    }

    if !std::path::Path::new(MANIFEST).exists() {
        ui.set_status_text(format!("Manifest not found: {MANIFEST}").into());
    }

    // Self-driven window drag state, shared with the callbacks (like the toast
    // timer) so the press/move/release sequence sees one anchor.
    let drag = Rc::new(WindowDrag::new());
    wire_callbacks(&ui, &app_rc, &drag);

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
            "rig" | "rigbones" => {
                if debug_view == "rigbones" {
                    ui.set_rig_bones(true);
                }
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
            // Settings sheet (same as clicking the brand), for screenshots.
            "settings" => ui.set_settings_open(true),
            _ => {}
        }
    }

    ui.show()?;
    apply_backdrop(&ui);

    ui.show()?;
    apply_backdrop(&ui);

    ui.show()?;
    schedule_glass(ui.as_weak(), 25);

    // Backdrop refresh loop. The tick itself is cheap; `glass_step` decides
    // whether it needs to capture and/or re-blur (the first capture, a
    // resample, or a moved blur slider) and keeps the placement registered,
    // so this can run at the animation cadence without work.
    {
        let weak = ui.as_weak();
        let tick_app = app_rc.clone();
        if let Ok(a) = app_rc.try_borrow() {
            a.glass_timer.start(TimerMode::Repeated, Duration::from_millis(33), move || {
                glass_step(&weak, &tick_app);
            });
        }
    }

    ui.run()
}

/* ------------------------------ callbacks ------------------------------- */

fn wire_callbacks(ui: &AppWindow, app_rc: &Rc<RefCell<App>>, drag: &Rc<WindowDrag>) {
    // --- toolbar / filters ------------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_select_tab(move |tab| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.set_tab(Kind::from_num(tab));
                if let Some(u) = weak.upgrade() {
                    sync_all(&u, &a);
                    u.set_grid_scroll(0.0);
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
                a.state.clear_random();
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
        ui.on_clear_query(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.q.clear();
                a.state.clear_random();
                a.state.clamp_page();
                if let Some(u) = weak.upgrade() {
                    u.set_query("".into());
                    sync_view(&u, &a);
                    u.set_grid_scroll(0.0);
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
                a.state.clear_random();
                a.state.ensure_integrity();
                a.state.clamp_page();
                if let Some(u) = weak.upgrade() {
                    sync_all(&u, &a);
                    u.set_grid_scroll(0.0);
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
                a.state.clear_random();
                a.state.ensure_integrity();
                a.state.clamp_page();
                if let Some(u) = weak.upgrade() {
                    sync_all(&u, &a);
                    u.set_grid_scroll(0.0);
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
                    u.set_grid_scroll(0.0);
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
                    u.set_grid_scroll(0.0);
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
        ui.on_set_columns(move |columns| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.set_columns(columns.max(0) as usize);
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
        ui.on_shuffle(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.randomize();
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
                        a.state.clear_random();
                        a.state.taxonomy =
                            build_taxonomy(&a.state.items, taxonomy_json.as_deref());
                        let valid: std::collections::HashSet<String> =
                            a.state.items.iter().map(|it| it.code.clone()).collect();
                        a.state.cart.retain(|c| valid.contains(c));
                        a.items.set_vec(
                            a.state.items.iter().map(asset_row).collect::<Vec<_>>(),
                        );
                        update_unity_rows(a);
                        a.thumb_state = vec![0u8; a.state.items.len()];
                        a.selected = None;
                        a.edit = None;
                        stop_rig(&u, a);
                        persist(&a.store, &a.state.cart);
                        a.state.clamp_page();
                        sync_all(&u, a);
                        u.set_grid_scroll(0.0);
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
        ui.on_card_tag(move |tag| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                a.state.tag = tag.to_string();
                a.state.clear_random();
                a.state.clamp_page();
                if let Some(u) = weak.upgrade() {
                    sync_view(&u, &a);
                    u.set_grid_scroll(0.0);
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
        ui.on_detail_ai_tag(move || {
            let payload = {
                let Ok(a) = app_rc.try_borrow() else {
                    return;
                };
                let Some(idx) = a.selected else {
                    return;
                };
                let it = a.state.items[idx].clone();
                let key = a.store.deepseek_api_key.trim().to_string();
                let model = a.store.deepseek_model.trim().to_string();
                let base = a.store.deepseek_base.trim().to_string();
                let root = a.lib.root.clone();

                // Tag vocabulary from the library (keeps AI names consistent).
                let mut counts: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for item in &a.state.items {
                    for t in &item.tags {
                        *counts.entry(t.clone()).or_insert(0) += 1;
                    }
                }
                let mut vocab: Vec<(String, usize)> = counts.into_iter().collect();
                vocab.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                let vocab: Vec<String> = vocab.into_iter().take(30).map(|(t, _)| t).collect();

                let prompt = tag_prompt(&it, &a.state.taxonomy, &vocab);
                let image = std::path::PathBuf::from(format!(
                    "{}\\{}",
                    root.trim_end_matches('\\'),
                    it.preview.replace('/', "\\")
                ));
                (key, model, base, prompt, image, it.slug.clone(), it.code.clone())
            };

            let (key, model, base, prompt, image, slug, code) = payload;
            let Some(u) = weak.upgrade() else {
                return;
            };
            if key.is_empty() {
                u.set_deepseek_state("no key".into());
                app(|a| show_toast(&u, a, "Set a DeepSeek API key in Settings first", "error"));
                return;
            }
            if !image.exists() {
                app(|a| show_toast(&u, a, &format!("No preview image for {slug}"), "error"));
                return;
            }

            u.set_ai_busy(true);
            let ui = weak.clone();
            std::thread::spawn(move || {
                let result = deepseek_tag_call(&base, &key, &model, &prompt, &image);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(u) = ui.upgrade() else {
                        return;
                    };
                    u.set_ai_busy(false);
                    let result = match result {
                        Ok(v) => v,
                        Err(e) => {
                            app(|a| show_toast(&u, a, &format!("AI tagging failed: {e}"), "error"));
                            return;
                        }
                    };

                    let mut tags: Vec<String> = result["tags"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|t| t.as_str())
                                .map(|t| t.trim().to_lowercase())
                                .filter(|t| !t.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();
                    tags.dedup();
                    let l1 = result["l1"].as_str().unwrap_or("").to_string();
                    let l2 = result["l2"].as_str().unwrap_or("").to_string();

                    app(|a| {
                        let mut added = 0usize;
                        if let Some(e) = a.edit.as_mut() {
                            for t in tags.iter() {
                                if !e.tags.iter().any(|x| x == t) {
                                    e.tags.push(t.clone());
                                    added += 1;
                                }
                            }
                        }
                        let all = a.edit.as_ref().map(|e| e.tags.clone()).unwrap_or_default();
                        let dirty = edit_dirty(&a);
                        u.set_edit_tags(to_string_model(&all));
                        u.set_edit_dirty(dirty);
                        show_toast(
                            &u,
                            a,
                            &format!(
                                "{code}: +{added} tags · {l1}{}  (review, then Save)",
                                if l2.is_empty() { String::new() } else { format!("/{l2}") }
                            ),
                            "ok",
                        );
                    });
                });
            });
        });
    }
    // --- cart: batch AI tagging -------------------------------------------
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_cart_ai_tag(move || {
            let (jobs, key, model, base, root) = {
                let Ok(a) = app_rc.try_borrow() else {
                    return;
                };
                let key = a.store.deepseek_api_key.trim().to_string();
                let model = a.store.deepseek_model.trim().to_string();
                let base = a.store.deepseek_base.trim().to_string();
                let root = a.lib.root.clone();

                let mut counts: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for item in &a.state.items {
                    for t in &item.tags {
                        *counts.entry(t.clone()).or_insert(0) += 1;
                    }
                }
                let mut vocab: Vec<(String, usize)> = counts.into_iter().collect();
                vocab.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                let vocab: Vec<String> = vocab.into_iter().take(30).map(|(t, _)| t).collect();

                let mut jobs = Vec::new();
                for code in a.state.cart.clone() {
                    let Some(idx) = a.state.index_of(&code) else {
                        continue;
                    };
                    let item = a.state.items[idx].clone();
                    if item.preview.is_empty() {
                        continue;
                    }
                    let prompt = tag_prompt(&item, &a.state.taxonomy, &vocab);
                    let image = std::path::PathBuf::from(format!(
                        "{}\\{}",
                        root.trim_end_matches('\\'),
                        item.preview.replace('/', "\\")
                    ));
                    jobs.push((code, prompt, image, item.l1.clone(), item.l2.clone(), item.tags.clone()));
                }
                (jobs, key, model, base, root)
            };

            let Some(u) = weak.upgrade() else {
                return;
            };
            if jobs.is_empty() {
                app(|a| show_toast(&u, a, "Cart is empty (or its items have no preview)", "error"));
                return;
            }
            if key.is_empty() {
                app(|a| show_toast(&u, a, "Set a DeepSeek API key in Settings first", "error"));
                return;
            }

            u.set_ai_busy(true);
            let ui = weak.clone();
            std::thread::spawn(move || {
                let lib = Library::new(&root);
                let total = jobs.len();
                let mut ok = 0usize;
                let mut failed = 0usize;
                let mut last_error = String::new();

                for (n, (code, prompt, image, l1, l2, current)) in jobs.into_iter().enumerate() {
                    let progress = ui.clone();
                    let label = code.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(u) = progress.upgrade() {
                            u.set_status_text(format!("AI tags {}/{} · {label}", n + 1, total).into());
                        }
                    });

                    match deepseek_tag_call(&base, &key, &model, &prompt, &image) {
                        Ok(doc) => {
                            let mut tags = current.clone();
                            if let Some(arr) = doc["tags"].as_array() {
                                for t in arr {
                                    if let Some(t) = t.as_str() {
                                        let tag = t.trim().to_lowercase();
                                        if !tag.is_empty() && !tags.contains(&tag) {
                                            tags.push(tag);
                                        }
                                    }
                                }
                            }
                            match lib.edit_asset(&code, &l1, &l2, &tags) {
                                Ok(updated) => {
                                    ok += 1;
                                    let ui = ui.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        let Some(u) = ui.upgrade() else {
                                            return;
                                        };
                                        app(|a| {
                                            if let Some(pos) = a.state.index_of(&code) {
                                                a.state.items[pos] = updated.clone();
                                                let mut row = asset_row(&updated);
                                                if let Some(old) = a.items.row_data(pos) {
                                                    row.thumb = old.thumb;
                                                    row.in_cart = old.in_cart;
                                                    row.in_unity = old.in_unity;
                                                }
                                                a.items.set_row_data(pos, row);
                                            }
                                            rebuild_rows(&u, a);
                                        });
                                    });
                                }
                                Err(e) => {
                                    failed += 1;
                                    if last_error.is_empty() {
                                        last_error = format!("{code}: {e}");
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            failed += 1;
                            if last_error.is_empty() {
                                last_error = format!("{code}: {e}");
                            }
                        }
                    }
                }

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(u) = ui.upgrade() else {
                        return;
                    };
                    u.set_ai_busy(false);
                    u.set_status_text(format!("AI tags: {ok}/{total} tagged").into());
                    app(|a| {
                        let message = if failed == 0 {
                            format!("AI tagged {ok} asset(s)")
                        } else {
                            format!("AI tagged {ok}, {failed} failed: {last_error}")
                        };
                        show_toast(&u, a, &message, if failed == 0 { "ok" } else { "error" });
                    });
                });
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_deepseek_test(move || {
            let store = app(|a| a.store.clone()).unwrap_or_default();
            let Some(u) = weak.upgrade() else {
                return;
            };
            let key = u.get_deepseek_key().trim().to_string();
            let model = {
                let m = u.get_deepseek_model().trim().to_string();
                if m.is_empty() { "deepseek-flash".to_string() } else { m }
            };
            let base = if store.deepseek_base.trim().is_empty() {
                "https://api.deepseek.com".to_string()
            } else {
                store.deepseek_base.clone()
            };
            if key.is_empty() {
                u.set_deepseek_state("no key".into());
                return;
            }
            u.set_deepseek_state("testing…".into());
            let ui = weak.clone();
            std::thread::spawn(move || {
                let result = deepseek_ping(&base, &key, &model);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(u) = ui.upgrade() else {
                        return;
                    };
                    match result {
                        Ok(reply) => u.set_deepseek_state(format!("ok · {reply}").into()),
                        Err(e) => u.set_deepseek_state(format!("failed: {}", e.chars().take(40).collect::<String>()).into()),
                    }
                });
            });
        });
    }
    {
        let app_rc = app_rc.clone();
        ui.on_section_toggled(move |which, open| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                match which {
                    0 => a.store.specs_open = open,
                    1 => a.store.category_open = open,
                    2 => a.store.tags_open = open,
                    _ => return,
                }
                store::save(&a.store);
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
                                        row.in_unity = old.in_unity;
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
                                a.state.clear_random();
                                a.state.remove_cart(&code);
                                a.items.set_vec(
                                    a.state.items.iter().map(asset_row).collect::<Vec<_>>(),
                                );
                                update_unity_rows(a);
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
                                    row.in_unity = old.in_unity;
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
        let weak = ui.as_weak();
        ui.on_unity_sync(move || {
            request_unity_sync(weak.clone(), true);
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
                        // Keep cart-open set: the cart is hidden by the detail
                        // sheet and reappears when that sheet closes.
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
                if s.base_pose {
                    return; // the bind pose has no timeline
                }
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
                if s.base_pose {
                    return;
                }
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
                    s.zoom = 1.0;
                    s.pan = [0.0; 3];
                    s.dirty = true;
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_rig_toggle_bones(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                if let Some(s) = a.rig.as_mut() {
                    s.bones = !s.bones;
                    s.dirty = true;
                    let bones = s.bones;
                    if let Some(u) = weak.upgrade() {
                        u.set_rig_bones(bones);
                    }
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
                s.base_pose = false;
                s.time = 0.0;
                s.duration = s.rig.clip_duration(idx);
                s.playing = s.duration > 0.0;
                s.dirty = true;
                let (playing, duration) = (s.playing, s.duration);
                if let Some(u) = weak.upgrade() {
                    u.set_rig_base_pose(false);
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
        // The sidebar's top entry: show the skeleton in its bind pose.
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_rig_select_base(move || {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                let Some(s) = a.rig.as_mut() else {
                    return;
                };
                if s.base_pose {
                    return;
                }
                s.base_pose = true;
                s.dirty = true;
                if let Some(u) = weak.upgrade() {
                    u.set_rig_base_pose(true);
                    u.set_rig_time("bind pose".to_string().into());
                }
            }
        });
    }
    {
        // Placeholder for the animation import workflow: it lands here next.
        let app_rc = app_rc.clone();
        let weak = ui.as_weak();
        ui.on_rig_import(move || {
            let Some(u) = weak.upgrade() else {
                return;
            };
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                show_toast(&u, &mut a, "Animation import is not wired up yet", "info");
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
                    // Drag right -> the model turns right (the camera orbits the
                    // other way), which is what the hand expects.
                    s.camera.yaw -= dx * 0.01;
                    s.camera.pitch = (s.camera.pitch + dy * 0.01).clamp(-1.4, 1.4);
                    s.dirty = true;
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        ui.on_rig_pan(move |dx, dy| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                if let Some(s) = a.rig.as_mut() {
                    let (right, up) = s.camera.basis();
                    let wpp = s.camera.world_per_pixel(s.h as f32);
                    // The scene follows the cursor: the camera target moves the
                    // opposite way, inside the camera's own screen plane.
                    let sx = -dx * wpp;
                    let sy = dy * wpp;
                    for k in 0..3 {
                        let d = right[k] * sx + up[k] * sy;
                        s.pan[k] += d;
                        s.camera.target[k] += d;
                    }
                    s.dirty = true;
                }
            }
        });
    }
    {
        let app_rc = app_rc.clone();
        ui.on_rig_zoom(move |factor| {
            if let Ok(mut a) = app_rc.try_borrow_mut() {
                // Wheel bursts collapse to one zoom step per gesture; button
                // clicks (one event each) pass straight through.
                let now = Instant::now();
                if now < a.rig_zoom_ready {
                    return;
                }
                a.rig_zoom_ready = now + Duration::from_millis(140);
                if let Some(s) = a.rig.as_mut() {
                    s.zoom = (s.zoom * factor).clamp(0.06, 8.0);
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
        let ui_close = ui.as_weak();
        ui.on_win_close(move || {
            // A settings change may still be inside its debounce window: copy
            // the live values and flush the store once before quitting.
            app(|a| {
                if a.store_pending_at.take().is_some() {
                    if let Some(u) = ui_close.upgrade() {
                        let theme = u.global::<Theme>();
                        let dark = theme.get_dark_mode();
                        a.store.dark = dark;
                        a.store.set_transparency(dark, theme.get_glass_transparency());
                        a.store.glass_blur = theme.get_glass_blur();
                    }
                    store::save(&a.store);
                }
            });
            let _ = slint::quit_event_loop();
        });
    }

    // --- settings sheet ----------------------------------------------------
    {
        let weak = ui.as_weak();
        ui.on_settings_changed(move || {
            // Marks the store dirty with a short debounce: a slider drag fires
            // this on every pointer move, and glass_step writes the JSON once
            // the value settles (and re-blurs the stored shot on its next tick).
            let ui = weak.clone();
            app(move |a| {
                if let Some(u) = ui.upgrade() {
                    a.store.deepseek_api_key = u.get_deepseek_key().to_string();
                    a.store.deepseek_model = u.get_deepseek_model().to_string();
                }
                a.store_pending_at = Some(Instant::now() + Duration::from_millis(650));
            });
        });
    }
    {
        // The settings sheet's resample button: the one deliberate capture
        // after startup (see glass_work). The backdrop is otherwise never
        // re-photographed.
        ui.on_glass_resample(move || {
            app(|a| a.glass_recapture = true);
        });
    }

    // --- self-driven window drag (never the OS modal move loop) -----------
    {
        let weak = ui.as_weak();
        let drag = drag.clone();
        ui.on_window_drag_start(move || {
            if let Some(u) = weak.upgrade() {
                drag.start(&u.window());
            }
        });
    }
    {
        let weak = ui.as_weak();
        let drag = drag.clone();
        let app_rc = app_rc.clone();
        ui.on_window_drag_move(move || {
            if let Some(u) = weak.upgrade() {
                drag.move_to(&u.window());
                // Re-register the backdrop in the same pointer event that
                // moved the window, so the blur never trails the drag.
                #[cfg(windows)]
                if let Ok(mut a) = app_rc.try_borrow_mut() {
                    glass_place(&u, &mut a);
                }
                #[cfg(not(windows))]
                let _ = &app_rc;
            }
        });
    }
    {
        let drag = drag.clone();
        ui.on_window_drag_end(move || drag.end());
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
