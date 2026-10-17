# asset-library-desktop — Port Spec (historical plan)

> **HISTORICAL — this is not a description of the shipped app.** It is the porting *plan*
> written before implementation. Several parts were deliberately superseded:
> the grid is **6 cards per row and paged** (not a `ListView` over every row); the bridges
> run **in-process** (the `bridge/*.py` relays and ports 9877/9878/9879 no longer exist);
> edits are written to disk **directly** (there is no `server/serve.py` write API).
> For the actual behaviour see the top-level `README.md`.

Single source of truth for porting `D:\Dev\asset-browser` (web, Vite + vanilla JS) to
`D:\Dev\asset-library-desktop` (Rust + Slint 1.18, native Windows desktop).

Everything below is a **contract**. Implement to it exactly. Where a signature or a
Slint property name is given, use that name verbatim — other files depend on it.

---

## 0. Source of truth

* Original web app: `D:\Dev\asset-browser` (read it whenever behaviour is unclear).
  - `src/lib/library.js` — manifest load, edit/delete, path resolution
  - `src/lib/state.js` — filters, sort, cart, export payloads
  - `src/ui/grid.js` — card markup + per-card actions
  - `src/ui/detail.js` — detail dialog, edit/save/delete, filmstrip, rig spec
  - `src/ui/cart.js`, `src/ui/{blender,unity,painter}.js` — cart + bridge dialogs
  - `src/lib/{blender,unity,painter}.js` — bridge clients (exact protocol)
  - `DESIGN.md` — the design system (tokens already mirrored in `ui/app.slint` Theme)
* Asset library (read-only data): `D:\3D Resource`
* Manifest: `D:\3D Resource\_Catalog\manifest.json`
* Taxonomy: `D:\3D Resource\_Catalog\taxonomy.json`

---

## 1. Non-negotiable constraints

1. **It must build.** `cargo build` must exit 0 and produce `target\debug\asset-library-desktop.exe`.
2. **Never** suppress type errors (`as any`/`#[allow(...)]` to hide real errors/`unwrap()` on
   user input). No `panic!` on the normal data path — a missing/bad file is a soft state, not a crash.
3. The app must stay responsive: **no network or big file IO on the UI thread.**
   Use `std::thread::spawn` + `slint::invoke_from_event_loop` to come back.
4. Data source is **fixed**: root `D:\3D Resource`. Keep `ROOT`/`MANIFEST` as consts with the
   existing defaults, but route **all** file access through `library::Library`.
5. `ui/app.slint` is the **only** Slint file (`slint::include_modules!()` unchanged).
6. Rust edition 2021. Keep `#![deny(warnings)]` OUT (do not add it).

### Cargo.toml (final dependency set)

```toml
[dependencies]
slint = "1.18"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
arboard = "3"
ureq = "2"          # blocking HTTP for the local bridges (localhost only, no TLS needed)
chrono = "0.4"      # generatedAt ISO-8601 for the export payload

[build-dependencies]
slint-build = "1.18"

[profile.dev]
opt-level = 1

[profile.dev.package."*"]
opt-level = 2
```

---

## 2. Module layout (each file owns exactly one concern)

```
src/main.rs      entry, Slint wiring, all callbacks, background threads, 15s status timer
src/model.rs     Item + manifest parsing + Kind + formatting + spec rows
src/state.rs     AppState: items/tab/filters/sort/cart/taxonomy + filtered() + counts
src/library.rs   filesystem access: load manifest, read text, edit/delete asset, abs paths
src/store.rs     persistence to %APPDATA%\asset-library-desktop\store.json (cart + bridge cfg)
src/export.rs    export payload builder (version/generatedAt/count/codes/items) + now_iso()
src/bridge.rs    Blender/Unity/Painter HTTP clients + health probes (ureq, blocking)
```

`src/main.rs` declares them in this order:
```rust
mod model; mod state; mod library; mod store; mod export; mod bridge;
slint::include_modules!();
```

---

## 3. The manifest data model (ground truth)

`manifest.json` = `{ "version", "generatedAt", "count", "items": [ ... ] }`.
All fields observed across the 1976 items (**use `#[serde(default)]` on every field** so legacy
items without a field still parse):

| field | type | notes |
|---|---|---|
| `kind` | string | `"model"` \| `"material"` \| `"rig"`; **absent ⇒ model** |
| `code` | string | e.g. `AR-0103` |
| `slug` | string | e.g. `balk_1_1` |
| `id` | string | usually == slug |
| `l1`, `l2`, `category` | string | `category` = `"{l1}/{l2}"` |
| `tags` | string[] | |
| `preview` | string | library-relative **posix** path, e.g. `Models/Architecture/Beam/balk_1_1/balk_1_1.png` |
| `name_original` | string | search haystack includes this |
| `tri_count` | int | |
| `lods` | string[] | |
| `n_tex` | int | |
| `dimensions_m` | `{width,height,depth}` \| null | models only |
| `textures` | string[] | library-relative posix paths (may be under `_textures/...`) |
| `resolution` | int[2] \| null | materials |
| `n_channels` | int | materials |
| `subtype` | string? | possible |
| `hasPreview` | bool | **serde rename** `hasPreview` |
| `rig` | string | rig signature json path, e.g. `Rig/Humanoid/<slug>/<slug>.rig.json` |
| `glb` | string | rig preview bundle path (used by the 3D viewer / fallback) |
| `clipsFile` | string | **rename** `clipsFile` |
| `clipCount` | int | **rename** `clipCount` |
| `boneCount` | int | **rename** `boneCount` |
| `humanoidCount` | int | **rename** `humanoidCount` |
| `extraCount` | int | **rename** `extraCount` |
| `shapeHash` | string | **rename** `shapeHash` |
| `vendor` | string | |
| `retargetable` | bool | |
| `glbBytes` | int | **rename** `glbBytes` |
| `profiles` | any[]? | rig export profiles (may be absent) |
| `base` | string? | rig derived base |
| `untextured` | bool? | model has no textures |

`Item::path()` — the asset folder (posix, library-relative). Use `path` when present; otherwise
derive: strip the last `/segment` from `preview`; if that yields nothing fall back to
`[l1, l2, slug].join("/")`. (Mirrors `deriveAssetPath` in `library.js`.)

---

## 4. Frozen Slint contract — `ui/app.slint`

Keep the existing `Theme` global (tokens already correct) and the existing component names
(`Plate`, `Chip`, `Btn`, `Tab`, `Card`). **These public names must exist verbatim** — `main.rs`
binds to them.

### 4.1 Structs

```slint
export struct AssetItem { code: string, slug: string, thumb: image,
                          tag1: string, tag2: string, tag3: string,
                          in-cart: bool, kind: int }          // kind: 0 model, 1 material, 2 rig
export struct GridRow   { a: int, b: int, c: int, d: int, e: int, f: int, n: int }
export struct CartRow   { code: string, slug: string, category: string, idx: int, thumb: image }
export struct SpecRow   { k: string, v: string }
export struct SlideItem { label: string, rel: string, thumb: image, active: bool }
export struct ClipItem  { name: string, dur: string }
```

### 4.2 `AppWindow` properties (in `in-out`, or `in` where noted)

```slint
// data
in property <[AssetItem]> items;
in property <[GridRow]>   rows;
in property <[CartRow]>   cart;
in property <[SpecRow]>   spec;
in property <[SlideItem]> slides;
in property <[ClipItem]>  clips;

// header / toolbar
in-out property <int>    tab;                  // 0 model, 1 material, 2 rig
in property <int>        count-models;
in property <int>        count-materials;
in property <int>        count-rigs;
in-out property <string> count-label;
in-out property <string> query;
in property <[string]>   l1-options;
in property <[string]>   l2-options;
in property <[string]>   sort-options;         // ["Code","Slug","Triangles ↑","Triangles ↓"]
in property <[string]>   style-options;        // ["All","Textured only"]
in-out property <int>    l1-index;             // -1 = all
in-out property <int>    l2-index;             // -1 = all
in-out property <int>    sort-index;
in-out property <int>    style-index;
in property <bool>       filters-active;
in-out property <string> status-text;
in-out property <bool>   busy;

// bridge dots: "ok" | "warn" | "down" | "checking"
in-out property <string> blender-state;
in-out property <string> unity-state;
in-out property <string> painter-state;

// detail dialog
in-out property <bool>           detail-open;
in-out property <string>         detail-code;
in-out property <string>         detail-slug;
in-out property <string>         detail-category;
in-out property <string>         detail-badge;
in-out property <string>         detail-kind;        // "model" | "material" | "rig"
in-out property <image>          detail-thumb;
in-out property <[string]>       edit-l1-options;
in-out property <[string]>       edit-l2-options;
in-out property <int>            edit-l1-index;
in-out property <int>            edit-l2-index;
in-out property <[string]>       edit-tags;
in-out property <bool>           edit-dirty;
in-out property <bool>           edit-busy;
in-out property <bool>           delete-armed;
in-out property <string>         edit-tag-input;
// rig 3D preview (stage 2 — implemented)
in-out property <image>          detail-frame;
in property <bool>               rig-has-3d: false;
in property <int>                rig-clip-index: 0;
in property <bool>               rig-playing: false;
in-out property <string>         rig-time;           // "0.00 / 0.00s"
in-out property <float>          rig-seek-pos: 0;
in property <float>              rig-duration: 0;

// cart
in-out property <bool>   cart-open;
in property <int>        cart-count;

// bridge dialogs + their config (in-out so TextInput round-trips)
in-out property <bool>   blender-open;
in-out property <string> blender-status;
in-out property <string> blender-root;
in-out property <string> blender-base;
in-out property <bool>   unity-open;
in-out property <string> unity-status;
in-out property <string> unity-base;
in-out property <bool>   painter-open;
in-out property <string> painter-status;
in-out property <string> painter-base;    // remote-scripting endpoint
in-out property <string> painter-bridge;  // local bridge url
in-out property <string> painter-root;

// toast (single, auto-hide ~2600ms)
in-out property <string> toast-text;
in-out property <string> toast-tone;      // "ok" | "info" | "error"
```

### 4.3 `AppWindow` callbacks (exact names + arity)

```slint
callback select-tab(int);
callback set-query(string);
callback clear-query();
callback select-l1(int);            // -1 = all
callback select-l2(int);
callback select-sort(int);
callback select-style(int);
callback clear-filters();
callback refresh-all();
callback ensure-image(int);         // grid row index (into items) to decode lazily
callback card-cart(int);            // index into items
callback card-copy(int);
callback card-blender(int);
callback card-unity(int);
callback card-painter(int);
callback card-tag(string);          // tag text -> filter by tag
callback open-detail(int);
callback close-detail();
callback detail-set-l1(int);        // index into edit-l1-options
callback detail-set-l2(int);
callback detail-add-tag(string);
callback detail-remove-tag(string);
callback detail-save();
callback detail-delete();           // arms on first click, deletes on second (handled in Rust)
callback detail-refresh();          // re-read <slug>.json + re-decode preview
callback detail-blender();          // import current asset
callback detail-unity();
callback detail-painter();
callback check-bridge(string);      // "blender" | "unity" | "painter"
callback blender-open-dialog();
callback blender-close();
callback blender-config(string, string);   // root, base
callback blender-send(string);             // "import" | "open"  (cart items)
callback unity-open-dialog();
callback unity-close();
callback unity-config(string);             // base
callback unity-place-cart();
callback unity-action(string);             // "sync" | "export"
callback painter-open-dialog();
callback painter-close();
callback painter-config(string, string, string);  // base, bridge, root
callback painter-send(string);             // "import" | "open"  (cart items)
callback toast(string, string);            // message, tone
callback open-cart();
callback close-cart();
callback cart-remove(string);
callback cart-clear();
callback cart-select-all-filtered();
callback cart-export();
callback cart-open-detail(string);

// rig 3D preview (stage 2 — implemented in src/rig.rs + src/raster.rs)
callback rig-toggle-play();
callback rig-restart();
callback rig-reset-view();
callback rig-select-clip(int);      // index into clips
callback rig-seek(float);           // seconds into the clip
callback rig-orbit(float, float);   // drag delta in px
callback rig-zoom(float);           // distance multiplier (0.8 = closer, 1.25 = further)
callback win-minimize();            // borderless window: custom title-bar controls
callback win-close();
```

### 4.4 Behaviour requirements for the UI

* **Header**: brand mark + `3D Asset Browser` (Haettenschweiler, uppercase-ish), 3 tabs
  (`3D Assets` / `Textures` / `Rigs`) each with a count plate, then three small status dots
  for Blender/Unity/Painter (click → opens that dialog), then `Cart` (shows `cart-count`).
* **Toolbar**: search field + clear ✕, `L1` ComboBox, `L2` ComboBox (disabled until L1 chosen),
  `Sort` ComboBox, `Style` ComboBox, `Refresh` button, right-aligned `count-label`.
* **Active-filter chips** row (visible only when `filters-active`): one chip per active
  q / l1 / l2 / tag / style each with an ✕ that clears just that filter, plus a `Clear filters`.
* **Grid**: `ListView` over `rows`; each row = up to 4 `Card`s (12px gap, 16px side padding).
  Card = thumb (contain, 116px) + code plate + action buttons + slug + up to 3 tag chips.
  Card actions per kind (indices are indices into `items`):
  - model/rig: `card-cart(idx)`, `card-copy(idx)`, `card-blender(idx)`, `card-unity(idx)`
  - material: `card-cart(idx)`, `card-copy(idx)`, `card-painter(idx)`, `card-unity(idx)`
  Card body click → `open-detail(idx)`. Tag chip click → `card-tag(tag_text)`.
  `init`/`ensure-image` hook must call `root.ensure-image(idx)` to lazily decode the thumb.
* **Empty state**: shown when `rows` is empty — title + hint vary by tab and `filters-active`.
* **Detail dialog** (modal overlay): left = preview image + badge; below it the **filmstrip**
  (`slides`) when `slides.length > 1`; right = plate/close, slug, category, scrollable spec
  rows (`SpecRow` k/v), **Category** editors (L1/L2 ComboBoxes), **Tags** editor (chips with ✕ +
  input + Add), `Save` (disabled unless `edit-dirty`), then the footer actions:
  `Add/Remove cart`, `Refresh`, `Delete asset` (shows `delete-armed` → "Click again to confirm"),
  and the primary `Open in Blender` / `Place in Unity` / `Send to Painter` (per kind).
  For `detail-kind == "rig"`: show the rig stats/tree spec; render the GLB stage when
  `rig-*` properties are populated, else fall back to `detail-thumb` with a `No preview bundle` note.
* **Cart drawer** (right overlay + scrim): header `CART` + count plate + close; scrollable list
  of `CartRow` (thumb, plate, slug, category, ✕ remove, row click → `cart-open-detail`);
  footer: `Export codes` (primary, disabled when empty), `Clear` (danger),
  `Add all N filtered` (select-all).
* **Bridge dialogs**: modal overlay per bridge — status line + dot, config `TextInput`s,
  `Recheck`, and the bulk cart actions (Blender: Import cart / Open cart; Unity: Place cart /
  Sync library / Export scene; Painter: Import textures / Open project).
* **Toast**: bottom-left ink pill, shown when `toast-text != ""`, tone colours from Theme
  (`ok` green, `info` ink, `error` red).

---

## 5. Frozen Rust module API

Other modules compile against these signatures. Do not rename.

### `model.rs`
```rust
#[derive(Clone, Default, serde::Deserialize)]
pub struct Dims { pub width: f64, pub height: f64, pub depth: f64 }

#[derive(Clone, Default, serde::Deserialize)]
pub struct Item { /* every §3 field, #[serde(default)] on all; renamed camelCase ones */ }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind { Model, Material, Rig }

impl Kind {
    pub fn num(self) -> i32;              // Model=0, Material=1, Rig=2
    pub fn from_num(n: i32) -> Kind;
    pub fn as_str(self) -> &'static str;  // "model"|"material"|"rig"
    pub fn noun(self) -> &'static str;    // "ASSETS"|"TEXTURES"|"RIGS" (count label)
}

impl Item {
    pub fn kind(&self) -> Kind;
    pub fn path(&self) -> String;         // library-relative posix folder
    pub fn fbx_rel(&self) -> String;      // "{path}/{slug}.fbx"
    pub fn has_tag(&self, t: &str) -> bool;
    pub fn hay(&self) -> String;          // lowercase "code slug name_original tags..."
}

pub fn parse_manifest(text: &str) -> Result<Vec<Item>, String>;
pub fn format_int(n: i64) -> String;                 // thousands separators
pub fn format_dims(d: &Option<Dims>) -> String;      // "2 × 0.19 × 0.18 m" or "-"
pub fn format_resolution(r: &Option<Vec<i64>>) -> String; // "2048 × 2048" or "-"
pub fn preview_badge(it: &Item) -> String;
pub fn spec_pairs(it: &Item) -> Vec<(String, String)>;  // key/value rows for the detail panel
pub fn slide_label(rel: &str) -> String;                // filename → "Base Color", "Normal", ...
```

`spec_pairs` must reproduce the web spec ordering:
common `CODE, SLUG, CATEGORY, KIND`; then per kind:
* material: `RESOLUTION, CHANNELS, TEXTURES`
* rig: `BONES, CLIPS, SHAPE HASH, VENDOR, SPINE, IK, TWIST, WEAPON` (+ `GLB` when non-empty)
* model: `TRIANGLES, TEXTURES` (+ `DIMENSIONS` from `dimensions_m`)
then `TAGS` (joined `" · "`) and, for rigs, `SOURCE PACK`/`PROFILES` from `base`/`profiles` if present.

### `state.rs`
```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey { Code, Slug, TriAsc, TriDesc }
impl SortKey { pub fn from_index(i: i32) -> SortKey; pub fn label(self) -> &'static str; }

#[derive(Clone, Default)]
pub struct Taxonomy { pub by_kind: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Vec<String>>> }

pub struct AppState {
    pub items: Vec<Item>,
    pub tab: Kind,
    pub q: String, pub l1: String, pub l2: String, pub tag: String,
    pub style_textured_only: bool,
    pub sort: SortKey,
    pub cart: Vec<String>,                // codes, insertion order
    pub taxonomy: Taxonomy,
}

impl AppState {
    pub fn new(items: Vec<Item>, cart: Vec<String>, taxonomy: Taxonomy) -> Self;
    pub fn kind_count(&self, k: Kind) -> i64;
    pub fn l1_options(&self) -> Vec<String>;
    pub fn l2_options(&self) -> Vec<String>;     // for current l1 within current tab
    pub fn tag_count(&self) -> usize;
    pub fn filtered(&self) -> Vec<usize>;        // indices into self.items, sorted
    pub fn in_cart(&self, code: &str) -> bool;
    pub fn toggle_cart(&mut self, code: &str) -> bool;   // returns true if now in cart
    pub fn add_all_filtered(&mut self) -> usize;         // returns how many were added
    pub fn remove_cart(&mut self, code: &str) -> bool;
    pub fn clear_cart(&mut self);
    pub fn cart_indices(&self) -> Vec<usize>;
    pub fn index_of(&self, code: &str) -> Option<usize>;
    pub fn set_tab(&mut self, k: Kind);                  // resets l1/l2, integrity-checks filters
    pub fn reset_filters(&mut self);
    pub fn filters_active(&self) -> bool;
    pub fn count_label(&self) -> String;                 // "1,913 ASSETS" or "… · N matching"
}

pub fn build_taxonomy(items: &[Item], taxonomy_json: Option<&str>) -> Taxonomy;
pub fn sort_options() -> Vec<String>;   // ["Code","Slug","Triangles ↑","Triangles ↓"]
pub fn style_options() -> Vec<String>;  // ["All","Textured only"]
```
Taxonomy rules (mirror `state.js`): use `taxonomy.json[kind]` when that key exists and is
non-empty, else derive from items (sorted, `en-US` numeric collator). Rigs are **not** in
taxonomy.json → always derived. Filter integrity: drop `l1`/`l2` the active tab cannot satisfy;
`TriAsc`/`TriDesc` are invalid for material/rig → fall back to `Code`.

### `library.rs`
```rust
pub struct Library { pub root: String }
impl Library {
    pub fn new(root: &str) -> Self;
    pub fn manifest_path(&self) -> String;
    pub fn load_manifest(&self) -> Result<Vec<Item>, String>;   // missing file => Ok(vec![]) NOT Err
    pub fn taxonomy_json(&self) -> Option<String>;              // read, None on failure
    pub fn read_text(&self, rel: &str) -> Result<String, String>;
    pub fn abs_path(&self, rel: &str) -> String;                // root + "\" + rel(posix→backslash)
    pub fn edit_asset(&self, code: &str, l1: &str, l2: &str, tags: &[String]) -> Result<Item, String>;
    pub fn delete_asset(&self, code: &str) -> Result<(), String>;
}
```
`edit_asset` / `delete_asset` must mirror the web FSA path:
* find the item in `manifest.json` by code;
* `base = material ? "Materials" : "Models"` (rigs: `Rig` — see note);
* if `l1`/`l2` changed → move the asset folder (recursive copy then remove original),
  create parents as needed;
* rewrite `<slug>.json`: `{ id, name: slug, kind, category: "l1/l2", tags }` merged over the
  existing doc (2-space indent);
* for models update `item.preview = "{base}/{l1}/{l2}/{slug}/{slug}.png"`;
* update the manifest entry, set `generatedAt`, write manifest back (**1-space indent**, the web
  writes `JSON.stringify(man, null, 1)`);
* `delete_asset`: remove the asset folder recursively, drop the manifest entry, rewrite manifest.
**Rigs note:** web only handles model/material here and would treat a rig as `Models`. For the
desktop port use `base = match kind { Material => "Materials", Rig => "Rig", Model => "Models" }`
which is correct for this library (`D:\3D Resource\Rig\...`). Document the divergence in a comment.

### `store.rs`
```rust
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Store {
    pub cart: Vec<String>,
    pub blender_base: String, pub blender_root: String,
    pub unity_base: String,
    pub painter_base: String, pub painter_bridge: String, pub painter_root: String,
}
impl Default for Store { /* the web defaults:
    blender_base "http://127.0.0.1:9877", blender_root ROOT,
    unity_base  "http://127.0.0.1:9878",
    painter_base "http://localhost:60041", painter_bridge "http://127.0.0.1:9879", painter_root ROOT */ }
pub fn path() -> std::path::PathBuf;   // %APPDATA%\asset-library-desktop\store.json
pub fn load() -> Store;                // missing/corrupt => Default
pub fn save(s: &Store);                // best-effort, create parent dirs, ignore errors
```

### `export.rs`
```rust
pub fn now_iso() -> String;                       // RFC3339 / ISO-8601 millis UTC
pub fn build_export(items: &[Item]) -> String;    // pretty JSON payload (below)
pub fn build_item_payload(it: &Item) -> String;   // single-item variant (count 1)
```
Payload shape (identical to `state.js::buildExport`):
```json
{ "version": 1, "generatedAt": "<iso>", "count": N,
  "codes": ["..."],
  "items": [ { "kind","code","slug","l1","l2","category",
               /* material adds: subtype?,resolution?,n_channels? */
               /* model/rig add: path, fbx, lods, textures */ } ] }
```

### `bridge.rs`
```rust
#[derive(Clone, Default)]
pub struct Dot { pub state: String, pub text: String }   // state: "ok"|"warn"|"down"

pub fn blender_health(base: &str) -> Dot;                 // GET /health -> {bridge,blender}
pub fn blender_send(base: &str, root: &str, mode: &str,
                    items: &[Item]) -> Result<String, String>;   // mode: "import"|"open"
pub fn unity_health(base: &str) -> Dot;                   // GET /health -> {ok,unity}
pub fn unity_place_item(base: &str, it: &Item, request_id: &str) -> Result<String, String>;
pub fn unity_action(base: &str, route: &str) -> Result<String, String>;   // "sync"|"export"
pub fn painter_health(bridge: &str, remote: &str) -> Dot;  // GET bridge /health; remote probe
pub fn painter_send_with_root(bridge: &str, root: &str, mode: &str, items: &[Item]) -> Result<String, String>;
```
All blocking, ~2.5s health timeout / 120s send timeout (painter open 180s). Never panic on
network failure — return `Err(message)` or a `"down"` Dot.

**Bridge HTTP contract** (from `bridge/*.py`; do not invent fields):

*Blender bridge* (`base`, default `http://127.0.0.1:9877`)
- `GET /health` → `{"bridge":bool,"blender":bool,"addon":str,"blender_exe":str}`
- `POST /import` body `{"paths":[fbx...],"items":[{fbx,library_root,asset_rel,slug,code}...]}`
  → `{"ok":bool,"blender":bool,"result":{...}}`
- `POST /open` same body → `{"ok":bool,"opened":int,"blender_exe":str}`

*Unity bridge* (`base`, default `http://127.0.0.1:9878`)
- `GET /health` → `{"ok":bool,"unity":bool,"host":str}`
- `POST /place` body `{"slug":str,"code":str,"l1":str,"l2":str,"category":str,"path":str,"requestId":str}`
  → `{"ok":bool,"result":str}` (or `{"ok":false,"error":str}`)
- `POST /sync`, `POST /export` body `{}` → same shape

*Painter bridge* (`bridge`, default `http://127.0.0.1:9879`; `remote`, default `http://localhost:60041`)
- `GET /health` → `{"bridge":bool,"painter":bool,"version":str|null,"endpoint":str,"project_open":bool}`
- `POST /import` body `{"items":[{code,name,files:[abs...],usage:"TEXTURE",group:"asset-browser"}...]}`
  → `{"ok":bool,"painter":bool,"scope":str,"imported":[],"errors":[]}`
- `POST /open` body `{"mesh":abs_fbx}` → `{"ok":bool,"painter":bool,"project":{...}}`
- Direct remote probe: `GET {remote}/` (a plain TCP success counts as alive).

Blender absolute FBX = `{book_root}\{item.path}\{slug}.fbx` with `/`→`\`.
Painter texture abs paths = `{painter_root}\{texture}` with `/`→`\`.

---

## 6. main.rs behaviour checklist

Loaded once at startup (off-thread where it matters):
1. `Store::load()` → cart + bridge config.
2. `Library::new(ROOT)`; `load_manifest()`; `taxonomy_json()`.
3. Build `AppState`; set `l1-options`, `l2-options`, `sort-options`, `style-options`.
4. Wire every callback in §4.3. **Never block the event loop** — anything taking > ~16ms
   (manifest reload, image decode, bridge HTTP, edit/delete) runs on a thread and posts back
   with `slint::invoke_from_event_loop`.
5. `slint::Timer` every **15s** → re-probe all three bridges (off-thread) → update `*-state`.
6. Persist to `Store` whenever cart or a bridge config changes.

Specific behaviours that must match the web app:
* **Search** matches code/slug/`name_original`/tags, case-insensitive.
* **Cart**: add/remove/clear; **select all filtered** adds every currently filtered item;
  **export** copies the JSON to the clipboard (`arboard`), then **clears the cart** and closes
  the drawer; empty cart export → toast "Cart is empty" and no clear.
* **Copy (per card)** → clipboard gets the single-item payload; toast on success/failure.
* **Refresh all** → re-read the manifest **and** re-decode previews, keep scroll position.
* **Detail refresh** → re-read `{path}/{slug}.json`, merge the fields the web merges
  (`dimensions_m, textures(+n_tex), tri_count, subtype, resolution, n_channels, untextured,
  description, tags`), re-decode the preview, update the card + the open dialog.
* **Detail save** → `edit_asset`, then update in-memory item, refresh the open dialog, toast.
* **Detail delete** → first click arms (label "Click again to confirm", auto-reset ~8s), second
  click deletes, closes the dialog, toasts.
* **Filter change** → scroll grid back to top. **Refresh** → keep scroll.
* **Keyboard**: `/` focuses search; `Esc` clears search / closes the topmost overlay.
* **Toasts** auto-clear after ~2.6s.

---

## 7. Design tokens (in `ui/app.slint::Theme`)

`bg #e4e4e4`, `ink #111`, ink-inv `#e4e4e4`, text ramp `.75/.62/.40`, hairlines `.15/.28`,
tint `.05/.08`, ok `#2f8f5b`, err `#b42318`, scrim `.55`. 12px card gap, 32px page gutter,
**6 cards per row** at 1440px (web track = `minmax(212px,1fr)`).

**Colour literal format (critical):** Slint's 8-digit hex is **`#RRGGBBAA`** — alpha
**last** (`i-slint-common/color_parsing.rs`; its own test asserts `#01234567 → 0x67012345`).
So `#111111` is opaque ink, but `#BF111111` is *not* 75% ink — it parses as R=BF G=11 B=11
A=11 (≈7% opaque), which makes every "faint" token invisible. Correct ramp values:
`#111111BF` (75%), `#1111119E` (62%), `#11111166` (40%), `#11111126` (line), `#11111147`
(line2), `#1111110D` (tint), `#11111114` (tint2), `#1111118C` (scrim).

**Fonts:** `Inter` is **not installed**, so the web's stack
(`"Inter","Helvetica Neue",Arial,…`) resolves to **Arial** — the desktop app uses `Arial`
for UI text, `Cascadia Mono` for mono, `Haettenschweiler` for display, matching what the
reference actually renders. Card/UI icons are drawn as `Path` geometry (Slint renders no SVG).

**Window:** frameless — `no-frame: true` on the root `Window`, with a `WindowMoveArea`
drag zone over the brand block in the header and custom `win-minimize` / `win-close`
buttons at the top-right (the OS title bar is gone).

---

## 8. Definition of done

* `cargo build` exit 0; `target\debug\asset-library-desktop.exe` exists.
* App launches, shows `1,913 ASSETS / 52 TEXTURES / 11 RIGS` counts from the real manifest.
* Tab switch, search, L1/L2/Sort/Style filters, tag filter, active-filter chips, clear-filters,
  empty state, lazy thumbnails, card cart/copy actions, cart drawer with persistence across
  restart, export-codes (clipboard + clear), detail open with filmstrip + spec, category/tag
  edit + save, delete-with-confirm, per-asset refresh, and the three bridge dialogs with live
  health dots all work.
* Stage 2 (rig GLB animation) is tracked separately; if the renderer is not wired yet the rig
  detail must degrade gracefully (static preview + "No preview bundle" note), never hang or panic.

---

## 9. Explicit scope decisions (audited — these are intentional, not omissions)

Verified by a full export-inventory audit of `asset-browser/src/**`:

* **`src/lib/locations.js` + the welcome/source picker** (`index.html#welcome`, FSA directory
  handles in IndexedDB, `?base=` HTTP mode): browser-only multi-source plumbing. The desktop app
  uses the fixed `D:\3D Resource` root, so this is intentionally not ported. (`DEFAULT_LOCATION`
  is just the `lib/` HTTP default of that same picker.)
* **`src/lib/rig-orient.js`** (bone FK / canonical basis / octahedron helper): has **zero
  importers** in the web app — dead code. Not ported.
* **`#detail-retarget`** ("Retarget to ARP"): a no-op stub in the web app (`toast('…not wired
  yet')`). Not ported.
* **`server/serve.py`** (`/api/asset/edit`, `/api/asset/delete`, `/api/taxonomy`): the write API
  that exists only because a browser cannot write files. The desktop app writes to disk directly
  through `library.rs`, so no server is needed.
* **`src/ui/rig-preview.js`** (three.js GLB skinned-animation player): becomes Stage 2 — see
  `docs/RIG_VIEWER_PLAN.md`. Until then the rig detail uses the static preview fallback.

