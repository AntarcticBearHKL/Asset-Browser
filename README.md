# Asset Browser — native desktop

A native Windows desktop browser for a **local 3D asset library**, built with
**Rust + Slint 1.18**. It replaces the earlier companion web app (Vite + vanilla JS)
with a single self-contained binary: no browser, no dev server, no Python relay
processes.

**Version 0.04** · repo: <https://github.com/AntarcticBearHKL/Asset-Browser>

## What it does

Browses a manifest-backed library of 3D models, materials and rigged characters, and hands
selected assets off to DCC tools.

- Data source: `<library>\_Catalog\manifest.json` + `_Catalog\taxonomy.json`
  (1,976 entries — 1,913 models / 52 materials / 11 rigs in the reference library).
- The reference library lives at `D:\3D Resource`. The root is currently fixed in code
  (see *Known limitations*).

## Install (pre-built)

1. Download `AssetBrowser-v0.04-win64.zip` from the
   [Releases](https://github.com/AntarcticBearHKL/Asset-Browser/releases) page.
2. Extract anywhere and run `asset-library-desktop.exe`.
3. Needs a 64-bit Windows 10/11 and the Microsoft Visual C++ 2015-2022 Redistributable
   (present on most systems).

The app expects the asset library at `D:\3D Resource`. With no library present it still
starts, but the grid will be empty.

## Build from source

```powershell
$env:PATH = "C:\Users\antar\.cargo\bin;" + $env:PATH
cargo run              # debug
cargo build --release  # -> target\release\asset-library-desktop.exe
```

`cargo test` runs 65 tests (plus 6 ignored); the ones that touch the real library skip
themselves when `D:\3D Resource\_Catalog\manifest.json` is absent.

## Design

- **Dark and light theme.** Tokens follow the bilibili-slint dark set
  (`bg #121212`, `surface #1E1E1E`, `text #E8E8E8`, `accent #3EA6FF`) in dark mode and a
  matched light palette otherwise; the brand in the header opens a **Settings sheet** with
  the theme switch, a transparency slider, and a Gaussian-blur slider (all persisted to
  `store.json`; the tint strength is remembered per theme, both defaulting to 20% with
  blur 100).
- **Frameless + frosted glass, blurred in-process.** `no-frame` plus the frameless
  treatment ported from bilibili-slint's `win.rs` (`WM_NCCALCSIZE`, `WM_NCACTIVATE`, the
  `WM_NCUAHDRAW*` suppressions, and a `WM_STYLECHANGING` filter because winit re-applies
  `WS_CAPTION` after `show()`). The glass is the algorithm of the sibling `studyapp`
  project (`glass/blur.rs`), ported as-is: `glass.rs` photographs the whole desktop —
  every monitor in one image — once with GDI (`WDA_EXCLUDEFROMCAPTURE` around the blit
  so the grab contains what is *behind* the window, not the window itself), downscales
  the shot by repeated 2×2 box halvings and runs a real separable Gaussian over the
  result, and hands the image to Slint, which draws it under a tint layer. Because the
  shot is the **whole desktop**, it is pinned to the desktop by placing it at
  capture-origin minus window-origin: moving or resizing the window only re-registers
  that placement. Capture + blur run on a worker thread (tens of milliseconds) and are
  **never started while the window is moving** — that was the drag lag — instead the
  shot is refreshed once the window has been still for 500 ms, at that cadence
  afterwards, so the frost keeps up with whatever moved behind the window; the blur
  slider re-blurs the stored pixels instead of re-capturing. The **Backdrop → Resample**
  button in Settings forces a capture at once, and a monitor hot-plug / resolution
  change (`WM_DISPLAYCHANGE`) takes one automatically.
  Why not the OS: measured on this machine, `DwmEnableBlurBehindWindow` gives transparency
  only (no blur), and the DWM backdrop paths composite opaquely and/or paint a caption
  band across the client. The transparency and blur sliders scale the tint alpha and the
  blur strength; at blur 0 the snapshot is skipped and the window falls back to the
  compositor's own see-through (sharp, always live).
- **Frameless, and it stays frameless.** `WM_NCCALCSIZE` reports a zero-size non-client area
  for **both** of its forms; `WM_NCACTIVATE` / `WM_NCPAINT` / `WM_NCUAHDRAWCAPTION` /
  `WM_NCUAHDRAWFRAME` are answered so nothing repaints a caption; and `WM_STYLECHANGING`
  filters the frame bits out of every incoming style change — necessary because winit
  re-applies `WS_CAPTION` after `show()`, so one-shot stripping is not enough.
  `WS_THICKFRAME` is kept: the DWM shadow and snap ride on it.
- **Dragging is app-driven, not the OS move loop.** `WindowMoveArea` is deliberately gone.
  The header carries a `TouchArea` whose `pointer-event` drives `window-drag-start/move/end`,
  and the Rust side records an absolute anchor (screen cursor + window origin) on press and
  applies one absolute `SetWindowPos` per move. The OS modal move loop is what re-evaluates
  the frame and makes a native border appear while dragging. Because the anchor is absolute,
  repeating a move for the same cursor is a no-op and the window cannot accumulate drift.
- **Resizing stays native.** `WM_NCHITTEST` answers a 6px edge/corner band, which is what
  keeps the native resize rim and Aero Snap; `WM_GETMINMAXINFO` + `WM_WINDOWPOSCHANGING`
  enforce a 1368×620 minimum. The Slint window deliberately does **not** pin `width`/`height`
  (a constant binding freezes the surface and the layout stops following an OS resize); the
  initial 1440×900 is set from Rust.
- **Bridges run in-process.** The Python relays existed only because a browser cannot open
  raw sockets; this binary talks to the targets directly — the Blender MCP addon over its TCP
  socket (`127.0.0.1:9876`, NUL-terminated JSON), Unity's MCP REST API
  (`localhost:26440`) and Substance Painter's remote scripting (`localhost:60041`).
  A health probe every 15 s drives the header badges.
- **Motion.** A single motion scale is used throughout — hover `110–150ms ease-out`, card
  hover-lift `180ms`, press `90ms`, toast `220ms ease-out-quart`.

## Features

- **Three tabs** — 3D Assets / Textures / Rigs, each with a live count. The brand area is a
  drag handle; minimise/close are custom buttons at the top right.
- **Paged grid** — the web's `minmax(212px,1fr)` wall as 6 cards per row, every card given
  the same explicit width/height derived from the window so they never differ in size and
  rows never overflow. The grid is **paged rather than endlessly scrolled**: the row model
  only ever holds the current page, and a floating pill at the bottom carries
  `« ‹ n / N › »`. The page number is **click-to-edit** (type a page, Enter or click away
  to jump). Page size (`12 / 24 / 48 / 96`, default 48) lives in the toolbar and opens a
  popup menu. Changing page scrolls to the top; changing page size keeps the first visible
  item; changing tab/filters/sort clamps the page so it can never point past the last one.
  Each card has a square thumbnail well, a code plate, path-drawn action icons, the slug and
  a strip of uppercase mono tag chips.
- **Filters** — L1 + L2 category dropdowns (`All …` per level), sort (code / slug /
  triangles ↑↓), tag filtering by clicking a card tag, and a second toolbar row
  with the active-filter chips, matching count and library total.
- **Card actions** — add to cart, copy the asset JSON, and hand off per kind: models/rigs →
  Blender + Unity, materials → Substance Painter + Unity.
- **Detail panel** — large preview with a texture filmstrip (centred tiles), full spec rows,
  editable category and tags with save, per-asset refresh (re-reads `<slug>.json`), delete
  with click-again confirmation, and the bridge hand-off buttons.
- **Rig 3D preview** — rigs play their `.glb` animation inside the detail panel. Rendering is
  CPU-only: linear-blend skinning (`src/rig/`) plus a threaded, 2x super-sampled software
  rasterizer (`src/raster.rs`) into a Slint image — no GPU dependency. The panel opens on
  the asset's own **first animated clip** (`Idle` when there is one), and the clip list
  follows the catalog's `<stem>.clips.json` order (with the `<slug>__` prefix stripped);
  playback runs on a wall-clock timeline with the spec's step / linear / cubic-spline
  samplers, and the camera follows the *deformed* model so root-motion clips stay in frame.
  A GLB that carries a skin **plus unskinned copies of the same body** (exported pose
  line-ups) loads as the single animated character: the frozen copies would read as "the
  animation is not applied". The stage draws a floor grid with a soft contact shadow.
  Controls: **right-drag orbits**, **middle-drag pans**, the **wheel zooms**, plus
  play/pause, restart, seek, reset view and **Bones** (a skeleton overlay over the mesh).
- **Cart** — a centred preview-only modal (five square cards per row). Clicking a
  preview opens the same detail panel as the grid; hover a card for its remove button.
  Add / remove / clear / "add all filtered" and **Export codes** live in the four
  flat actions at the bottom. The cart persists to
  `%APPDATA%\asset-library-desktop\store.json` and is restored on launch.
- **Native file writes** — category/tag edits and deletes write straight to disk
  (folder move + `<slug>.json` + `manifest.json`); no helper server is involved.

## Layout

```
src/main.rs      entry point: Slint wiring, every ui.on_* callback, worker threads, timers
src/model.rs     manifest record + kinds, formatting, spec rows
src/state.rs     filtered/sorted/paged view state, tab, filters, cart, taxonomy
src/library.rs   filesystem: load manifest, read, edit, delete
src/store.rs     persistence of cart + bridge config
src/export.rs    cart export payloads (web-compatible)
src/bridge.rs    in-process Blender / Unity / Painter clients + health probes
src/glass.rs     in-process backdrop: GDI screen grab + separable Gaussian (studyapp port)
src/rig/         GLB loading, node graph + clips, CPU linear-blend skinning
src/raster.rs    threaded software rasterizer (SSAA, floor grid, bone overlay)
src/tests.rs     data-layer + rig/renderer tests
ui/theme.slint       design tokens (Theme) + the Slint/Rust contract structs
ui/components.slint  every reusable component (Card, Btn, Pick, dialogs, …)
ui/app.slint         AppWindow: properties, callbacks, and the page composition
tools/screenshot.ps1 dev tool: render any surface to a PNG for visual QA
mcp/                 agent-facing MCP server (Python/uv; same library + cart)
docs/archive/        historical planning documents (see the banner in each)
```

Nothing that touches the network or does heavy IO runs on the UI thread: work happens on
`std::thread` workers and is posted back with `slint::invoke_from_event_loop`.

## Visual QA

`tools/screenshot.ps1` captures the running window and can open any surface
deterministically, without synthesising mouse input:

```powershell
powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -Out grid.png
powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -View cart -Out cart.png
powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -View rig  -Out rig.png
```

`-View` drives the `ALD_DEBUG_VIEW` startup hook in `main.rs`
(`grid`, `cart`, `rig`, `rigbones`, `model`, `cat`, `blender`, `unity`, `painter`, `settings`). It finds
the window by enumerating visible top-level windows for the process, forces it to the
foreground, and grabs the screen, so the result matches what a user actually sees
(including the blurred backdrop).

## Known limitations

- **The rig preview renders on the CPU.** That keeps the app dependency-light and avoids
  forcing a GPU backend. Measured on the reference library at 256×224 (2x super-sampled
  internally, frame time includes skinning, floor and shadow, **debug build**: a loaded
  dev machine can skew these): the seven 11k–59k triangle rigs land in **10–50 ms/frame**;
  the three 1.0–1.6 M triangle meshes (`rig_insane_aircombo`, `rig_massive_greatsword`,
  `rig_oriental_spear`) take **~0.4–0.6 s/frame**, so their playback is choppy there —
  as before, the frames render on a worker thread with one frame in flight, so the UI
  stays responsive regardless, and the release build is several times faster.
- Thumbnails are decoded lazily, one per card on first reveal; nothing is decoded up front.
- The library root is fixed at `D:\3D Resource` (the desktop equivalent of picking a
  directory in the browser); it is not yet user-configurable from the UI.
- Dialog/drawer **open-close transitions are not animated.** They are gated by Slint `if`
  expressions, and an `if`-created element is rendered straight at its target value — there
  is no "from" value to animate, and the element is destroyed before an exit animation could
  run. Animating them would mean keeping every overlay alive behind `opacity`, which risks an
  invisible overlay swallowing clicks.
- **Dragging the window to a screen edge no longer Aero-Snaps it.** That gesture is performed
  by the OS move loop, which this app deliberately no longer enters (it is what drew the
  native border during a drag). `Win`+arrow keys still work — the shell handles those.
- No licence file is included yet.
