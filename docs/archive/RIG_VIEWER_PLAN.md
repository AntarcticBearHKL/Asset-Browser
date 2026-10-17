# Stage 2 — Rig GLB animation viewer (historical plan)

> **HISTORICAL — kept for the rationale behind the chosen approach.** The CPU
> software-renderer path described here **is implemented** (`src/rig.rs`, `src/raster.rs`);
> the GPU options at the end were not taken. For the shipped behaviour and its measured
> cost, see the "Known limitations" section of the top-level `README.md`.

## STATUS: IMPLEMENTED (CPU software-renderer path)

Shipped as `src/rig.rs` (GLB parse + skeleton + linear-blend skinning) and `src/raster.rs`
(z-buffered software rasterizer, Lambert clay shading) rendered into a `slint::Image` and
shown in the rig detail panel, with orbit-by-drag, play/pause/restart/seek and a clip list.

**Why not the wgpu Option A below:** the wgpu route requires enabling Slint's
`unstable-wgpu-30` feature and calling `BackendSelector::require_wgpu_30(...)`, which *forces*
the renderer backend — if GPU init fails the whole (already working) app stops launching. The
CPU path adds no backend requirement, no unstable feature and no wgpu version-lockstep, and it
is verifiable without a GPU (geometry/renderer unit tests + offscreen frame dumps). Measured:
**~10 ms/frame at 512×448** for the 45,190-triangle `rig_9cg_archer`, well inside a 30 fps
budget.

The wgpu design below is retained as the upgrade path if 60 fps or higher image quality is
ever required. Everything below this line is the original research/plan.

---

Target: replace the rig detail panel's static preview with a live, interactive, skinned
GLB animation player, inside the Slint UI.

Source: librarian research, 2026-10. Sample asset:
`D:\3D Resource\Rig\Humanoid\rig_9cg_archer\rig_9cg_archer.glb` (17 MB, 94 bones, 40 clips).

## Verdict

* **`three-d` is disqualified** — it has **no linear-blend skinning** (no joints/weights/skins
  anywhere in `three-d` 0.19 or `three-d-asset` 0.10). It would render the archer frozen at
  bind pose and animate only node/morph tracks.
* **No turnkey crate exists** (`wgpu-skinned-mesh` does not exist). The skinning draw is ours.
* **Recommended: Option A** — Slint's own **wgpu backend** + `gltf` parser; render the skinned
  rig into a `wgpu::Texture` and hand it to Slint **zero-copy** via `slint::Image::try_from`.
  This is the only path that is at once interactive, not a whole engine, and zero-copy.

## Cargo changes

```toml
slint = { version = "1.18", features = ["unstable-wgpu-30"] }
wgpu  = "30"     # MUST match the slint unstable-wgpu-NN feature major version
gltf  = "1.4"    # parser only
```

**Critical:** the `wgpu` major must equal the Slint feature major (`unstable-wgpu-30` ↔
`wgpu 30`), otherwise two incompatible `wgpu::Device` types appear and `Image::try_from`
won't type-check.

## Integration shape

1. `BackendSelector::new().require_wgpu_30(WGPUConfiguration::Automatic(Default::default())).select()`
2. In `window().set_rendering_notifier`, on `RenderingState::RenderingSetup` grab
   `GraphicsAPI::WGPU30 { device, queue, .. }` and build a `SkinnedRig` from the GLB bytes.
3. On `RenderingState::BeforeRendering`: read playback state from Slint properties
   (`rig-playing`, `rig-clip-index`, `rig-time`), render one frame into an
   `Rgba8UnormSrgb` texture (`TEXTURE_BINDING | RENDER_ATTACHMENT`), then
   `app.set_frame(slint::Image::try_from(tex))`; call `window().request_redraw()` while playing.
4. On `RenderingState::RenderingTeardown`, drop the rig.
5. All GPU submission uses **Slint's** device/queue (one device, one queue). No second
   `wgpu::Instance`.

## The skinning work (ours to write, ~300–500 lines)

1. `gltf::import(path)` → document, buffers, images.
2. Read `skin.joints()`, `skin.reader().read_inverse_bind_matrices()` (feature `utils`),
   per-primitive `Semantic::Joints(0)/Weights(0)` + `Positions/Normals/TexCoords(0)/Indices`.
3. Per frame: evaluate animated node TRS from the clip samplers (step/linear/cubicspline),
   walk the joint hierarchy → `globalJointTransform[j]`, then
   `jointMatrix[j] = inverse(globalTransformOfMeshNode) * globalJointTransform[j] * inverseBind[j]`.
4. Upload the ~94 `mat4` palette (storage/uniform) + `JOINTS_0`/`WEIGHTS_0` as vertex attrs;
   vertex shader does `Σ w[k] * jointMatrix[joint[k]] * vec4(pos,1)`.
5. Fragment shader = monochrome clay: `base * (0.2 + 0.8 * max(dot(N,L),0))`, no textures.
   Keep the ink colour from `Theme.ink` (#111). Ground grid + axes optional. Orbit
   controls: mouse drag → yaw/pitch, wheel → zoom (implement over the Slint stage `TouchArea`).

## Transport (map to the existing Slint contract)

`clips` (list of `ClipItem{name,dur}`), `rig-clip-index`, `rig-playing`,
`rig-time` ("0.00 / 0.00s"), plus restart / reset-view. Playback advances in the render loop
using a monotonic clock; loop the clip (`LoopRepeat`). Clip names ship as
`<slug>__<ClipName>` → display the half after `__`.

## Pitfalls to respect

* Pin Slint minor: `unstable-wgpu-30` may change/be removed in future minors.
* Texture size = panel **logical** size × `window().scale_factor()`; recreate the texture when
  it changes.
* Do not run Slint's loop and a second winit loop.
* Do not add Bevy — the official `slint-hosts-bevy` path works but drags in a full engine and
  hits a Windows Skia/ICU duplicate-symbol linker issue (`slint-ui/slint#10587`).
* Keep a **graceful fallback**: if the GLB fails to load or wgpu init fails, show
  `detail-thumb` + a "No preview bundle" / "Could not load the animation bundle" note. Never
  hang or panic. This fallback must exist even before Option A lands.

## Options if Option A becomes a schedule risk

* **Option B** — CPU linear-blend skinning + three-d offscreen `RenderTarget` +
  `read_color::<[u8;4]>()` → `Image::from_rgba8` (worker thread + `invoke_from_event_loop`).
  Render-on-demand (still frame on seek/clip change), not 60 fps.
* **Fallback** — static bind-pose preview only.
