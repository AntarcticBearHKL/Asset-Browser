//! CPU software renderer for the rig stage.
//!
//! A z-buffered, threaded, super-sampled triangle rasterizer with clay shading,
//! a floor grid with a soft contact shadow, and an optional bone overlay.
//! No GPU, no `wgpu`.
//!
//! Invariants: the caller passes an `w*h*4` RGBA8 buffer plus triangle indices
//! that address `pos`/`nrm`; triangles crossing the near plane are **clipped**
//! (not dropped), and the renderer owns all of its working buffers.

use std::thread;

/// Orbit camera: the eye sits on a sphere around `target`.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
    pub target: [f32; 3],
}

/// Default 3/4 front view, a little above the model.
pub const DEFAULT_YAW: f32 = 0.62;
pub const DEFAULT_PITCH: f32 = 0.20;

impl Camera {
    /// Frame an AABB: the default 3/4 view with the whole box in shot.
    pub fn framed(min: [f32; 3], max: [f32; 3]) -> Camera {
        let (target, dist) = Camera::fit(min, max, DEFAULT_YAW, DEFAULT_PITCH, 1.3);
        Camera {
            yaw: DEFAULT_YAW,
            pitch: DEFAULT_PITCH,
            dist,
            target,
        }
    }

    /// `(target, distance)` that keeps every corner of `min..max` inside the
    /// 45°-vertical-fov frustum for the given angles and aspect.
    ///
    /// Pure and cheap (8 corners), so the caller can re-fit every frame to keep
    /// the *deformed* model in shot — root-motion clips included.
    pub fn fit(min: [f32; 3], max: [f32; 3], yaw: f32, pitch: f32, aspect: f32) -> ([f32; 3], f32) {
        let target = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let back = dir_of(yaw, pitch);
        let fwd = [-back[0], -back[1], -back[2]];
        let (right, up) = basis(fwd);
        let tan_half = (FOV * 0.5).tan();
        let aspect = if aspect > 1e-3 { aspect } else { 1.0 };
        let mut need = 1e-3f32;
        for cx in [min[0], max[0]] {
            for cy in [min[1], max[1]] {
                for cz in [min[2], max[2]] {
                    let c = [cx - target[0], cy - target[1], cz - target[2]];
                    let x = dot(c, right);
                    let y = dot(c, up);
                    let z = dot(c, fwd);
                    let d_v = y.abs() / tan_half - z;
                    let d_h = x.abs() / (tan_half * aspect) - z;
                    need = need.max(d_v).max(d_h);
                }
            }
        }
        (target, need * 1.07)
    }

    /// World-space eye position.
    pub fn eye(&self) -> [f32; 3] {
        let d = dir_of(self.yaw, self.pitch);
        [
            self.target[0] + d[0] * self.dist,
            self.target[1] + d[1] * self.dist,
            self.target[2] + d[2] * self.dist,
        ]
    }

    /// Screen-space basis of the camera: `(right, up)` unit vectors in world
    /// space. Used to turn a drag in pixels into a world-space pan.
    pub fn basis(&self) -> ([f32; 3], [f32; 3]) {
        let back = dir_of(self.yaw, self.pitch);
        basis([-back[0], -back[1], -back[2]])
    }

    /// World units covered by one pixel at the target's depth.
    pub fn world_per_pixel(&self, viewport_height: f32) -> f32 {
        2.0 * self.dist * (FOV * 0.5).tan() / viewport_height.max(1.0)
    }
}

fn dir_of(yaw: f32, pitch: f32) -> [f32; 3] {
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    [cp * sy, sp, cp * cy]
}

/* ------------------------------ the scene ------------------------------- */

/// A ground plane with a grid and one soft contact shadow.
#[derive(Clone, Copy, Debug)]
pub struct Floor {
    /// World y of the plane (the bind pose's lowest point).
    pub y: f32,
    /// World-space spacing of the grid lines.
    pub step: f32,
    /// Distance at which the grid has faded out completely.
    pub reach: f32,
    pub shadow: Option<Shadow>,
}

/// A soft elliptical darkening on the floor plane, centred under the model.
#[derive(Clone, Copy, Debug)]
pub struct Shadow {
    pub center: [f32; 2],
    pub radius: f32,
    /// 0..1; falls off as the model lifts off the ground.
    pub strength: f32,
}

/// Optional overlay drawn on top of the shaded mesh.
pub struct Overlay<'a> {
    /// Skeleton segments `[parent, child]` in world space.
    pub bones: &'a [[[f32; 3]; 2]],
    pub bone_color: [u8; 3],
}

impl Default for Overlay<'_> {
    fn default() -> Self {
        Overlay {
            bones: &[],
            bone_color: [62, 166, 255],
        }
    }
}

/* ------------------------------ rendering ------------------------------- */

const FOV: f32 = std::f32::consts::FRAC_PI_4;
/// #242424 — matches Theme.well so the panel's letterbox blends in.
const BG: u8 = 36;
/// Direction *toward* the key light.
const KEY_DIR: [f32; 3] = [0.42, 0.72, 0.55];
/// Supersampling factor: the panel is several times the internal buffer, so 2x
/// here buys most of the anti-aliasing back.
const SS: usize = 2;

/// Rasterize `idx` (a triangle list) from `pos`/`nrm` into `pixels` (RGBA8, `w×h`).
pub fn render(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    pos: &[[f32; 3]],
    nrm: &[[f32; 3]],
    idx: &[u32],
    cam: &Camera,
    floor: Option<Floor>,
    overlay: &Overlay,
) {
    let ww = w as usize;
    let hh = h as usize;
    if ww == 0 || hh == 0 {
        return;
    }
    let need = ww * hh * 4;
    if pixels.len() < need {
        return;
    }

    let bw = ww * SS;
    let bh = hh * SS;
    let mut color = vec![0u8; bw * bh * 4];
    let mut depth = vec![f32::INFINITY; bw * bh];

    let eye = cam.eye();
    let fwd = normalize(sub(cam.target, eye));
    let (right, up) = basis(fwd);
    let tan_half = (FOV * 0.5).tan();
    let aspect = bw as f32 / bh as f32;
    let near = (cam.dist * 1e-4).max(1e-6);

    // View space for every vertex: clipping and the depth test both live there.
    let mut view = Vec::with_capacity(pos.len());
    for p in pos {
        let d = sub(*p, eye);
        view.push([dot(d, right), dot(d, up), dot(d, fwd)]);
    }

    // Bones, also in view space.
    let bones: Vec<[[f32; 3]; 2]> = overlay
        .bones
        .iter()
        .map(|[a, b]| [to_view(*a, eye, right, up, fwd), to_view(*b, eye, right, up, fwd)])
        .collect();

    // Rows above the horizon can never meet the ground plane: fill them with
    // the flat background instead of running the floor shading per pixel.
    // `right.y` is 0 by construction, so the horizon is a row, not a line.
    let floor_row = match floor {
        Some(f) if up[1] > 1e-6 && eye[1] > f.y => {
            let dy_horizon = -fwd[1] / up[1];
            let row = (1.0 - dy_horizon / tan_half) * 0.5 * bh as f32;
            row.clamp(0.0, bh as f32) as usize
        }
        Some(_) => 0,
        None => bh,
    };

    let band_count = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 12);
    let band_count = band_count.min(bh.max(1));

    // One prepass over the triangles: project the vertices once and keep the
    // screen bbox, so every row band only pays for the triangles it overlaps
    // (the old shape projected all 45k triangles once *per band*).
    let tris = prep_tris(idx, &view, near, bw, bh, tan_half, aspect);

    let bands = split_bands(&mut color, &mut depth, bh, bw, band_count);
    let shared = BandShared {
        bw,
        bh,
        eye,
        fwd,
        right,
        up,
        tan_half,
        aspect,
        near,
        target: cam.target,
        view: &view,
        nrm,
        tris: &tris,
        floor,
        floor_row,
        bones: &bones,
        bone_color: overlay.bone_color,
    };
    thread::scope(|scope| {
        for (y0, band_color, band_depth) in bands {
            let shared = &shared;
            scope.spawn(move || {
                raster_band(y0, band_color, band_depth, shared);
            });
        }
    });

    // Box-downsample the supersampled internal buffer into the caller's.
    let inv = 1.0 / (SS * SS) as f32;
    let chunk_rows = hh.div_ceil(band_count).max(1);
    thread::scope(|scope| {
        for (ci, chunk) in pixels[..need].chunks_mut(chunk_rows * ww * 4).enumerate() {
            let y_base = ci * chunk_rows;
            let color = &color;
            scope.spawn(move || {
                for (ri, orow) in chunk.chunks_mut(ww * 4).enumerate() {
                    let oy = y_base + ri;
                    for ox in 0..ww {
                        let mut acc = [0.0f32; 3];
                        for sy in 0..SS {
                            let row = (oy * SS + sy) * bw;
                            for sx in 0..SS {
                                let i = (row + ox * SS + sx) * 4;
                                acc[0] += color[i] as f32;
                                acc[1] += color[i + 1] as f32;
                                acc[2] += color[i + 2] as f32;
                            }
                        }
                        orow[ox * 4] = (acc[0] * inv + 0.5) as u8;
                        orow[ox * 4 + 1] = (acc[1] * inv + 0.5) as u8;
                        orow[ox * 4 + 2] = (acc[2] * inv + 0.5) as u8;
                        orow[ox * 4 + 3] = 255;
                    }
                }
            });
        }
    });
}

struct BandShared<'a> {
    bw: usize,
    bh: usize,
    eye: [f32; 3],
    fwd: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    tan_half: f32,
    aspect: f32,
    near: f32,
    target: [f32; 3],
    view: &'a [[f32; 3]],
    nrm: &'a [[f32; 3]],
    tris: &'a [TriPrep],
    floor: Option<Floor>,
    floor_row: usize,
    bones: &'a [[[f32; 3]; 2]],
    bone_color: [u8; 3],
}

/// One triangle after the prepass: its vertex indices and the screen-space
/// bounding box at the internal resolution.
#[derive(Clone, Copy)]
struct TriPrep {
    i: [u32; 3],
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
}

fn prep_tris(
    idx: &[u32],
    view: &[[f32; 3]],
    near: f32,
    bw: usize,
    bh: usize,
    tan_half: f32,
    aspect: f32,
) -> Vec<TriPrep> {
    let project = |v: [f32; 3]| -> [f32; 3] {
        let inv = 1.0 / (v[2] * tan_half);
        [
            (v[0] * inv / aspect) * 0.5 * bw as f32 + 0.5 * bw as f32,
            0.5 * bh as f32 - (v[1] * inv) * 0.5 * bh as f32,
            v[2],
        ]
    };
    let mut out = Vec::with_capacity(idx.len() / 3);
    for tri in idx.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let (Some(v0), Some(v1), Some(v2)) = (
            view.get(i0).copied(),
            view.get(i1).copied(),
            view.get(i2).copied(),
        ) else {
            continue;
        };
        let behind = (v0[2] <= near) as usize + (v1[2] <= near) as usize + (v2[2] <= near) as usize;
        if behind == 3 {
            continue;
        }
        let (min_x, max_x, min_y, max_y) = if behind > 0 {
            // The clipped extent is only known during clipping; give the band
            // the whole viewport (near-plane triangles are rare).
            (0, bw as i32 - 1, 0, bh as i32 - 1)
        } else {
            let p0 = project(v0);
            let p1 = project(v1);
            let p2 = project(v2);
            let clamp_x = |x: f32| x.floor().max(0.0).min((bw - 1) as f32) as i32;
            let clamp_y = |y: f32| y.floor().max(0.0).min((bh - 1) as f32) as i32;
            let ceil_x = |x: f32| x.ceil().max(0.0).min((bw - 1) as f32) as i32;
            let ceil_y = |y: f32| y.ceil().max(0.0).min((bh - 1) as f32) as i32;
            (
                clamp_x(p0[0].min(p1[0]).min(p2[0])),
                ceil_x(p0[0].max(p1[0]).max(p2[0])),
                clamp_y(p0[1].min(p1[1]).min(p2[1])),
                ceil_y(p0[1].max(p1[1]).max(p2[1])),
            )
        };
        out.push(TriPrep {
            i: [tri[0], tri[1], tri[2]],
            min_x,
            max_x,
            min_y,
            max_y,
        });
    }
    out
}

fn split_bands<'a>(
    color: &'a mut [u8],
    depth: &'a mut [f32],
    bh: usize,
    bw: usize,
    n: usize,
) -> Vec<(usize, &'a mut [u8], &'a mut [f32])> {
    let rows = bh.div_ceil(n.max(1));
    let mut out = Vec::new();
    let mut colors = color.chunks_mut(rows * bw * 4);
    let mut depths = depth.chunks_mut(rows * bw);
    let mut y = 0usize;
    while y < bh {
        let (Some(c), Some(d)) = (colors.next(), depths.next()) else {
            break;
        };
        out.push((y, c, d));
        y += rows;
    }
    out
}

fn raster_band(y0: usize, color: &mut [u8], depth: &mut [f32], s: &BandShared) {
    let y1 = (y0 + depth.len() / s.bw).min(s.bh);

    // Background: flat well colour, plus the floor / grid / contact shadow
    // wherever a view ray meets the ground plane.
    for py in y0..y1 {
        let dy = (1.0 - (py as f32 + 0.5) / s.bh as f32 * 2.0) * s.tan_half;
        let row = (py - y0) * s.bw;
        let sky = py < s.floor_row;
        for px in 0..s.bw {
            let c = if sky {
                BG
            } else {
                let dx = ((px as f32 + 0.5) / s.bw as f32 * 2.0 - 1.0) * s.tan_half * s.aspect;
                background(dx, dy, s)
            };
            let o = (row + px) * 4;
            color[o] = c;
            color[o + 1] = c;
            color[o + 2] = c;
            color[o + 3] = 255;
            depth[row + px] = f32::INFINITY;
        }
    }

    // Triangles (the prepass already rejected the fully-behind ones and gave
    // each survivor its screen bbox, so a band only touches what it covers).
    let mut poly = [VVert::ZERO; 4];
    for t in s.tris {
        if (t.min_y as usize) >= y1 || (t.max_y as usize) < y0 {
            continue;
        }
        if t.max_x < 0 || t.min_x >= s.bw as i32 {
            continue;
        }
        let (i0, i1, i2) = (t.i[0] as usize, t.i[1] as usize, t.i[2] as usize);
        let (Some(v0), Some(v1), Some(v2)) = (
            s.view.get(i0).copied(),
            s.view.get(i1).copied(),
            s.view.get(i2).copied(),
        ) else {
            continue;
        };
        let a = VVert {
            v: v0,
            n: s.nrm.get(i0).copied().unwrap_or([0.0, 0.0, 1.0]),
        };
        let b = VVert {
            v: v1,
            n: s.nrm.get(i1).copied().unwrap_or([0.0, 0.0, 1.0]),
        };
        let c = VVert {
            v: v2,
            n: s.nrm.get(i2).copied().unwrap_or([0.0, 0.0, 1.0]),
        };

        if a.v[2] <= s.near && b.v[2] <= s.near && c.v[2] <= s.near {
            continue;
        }
        if a.v[2] > s.near && b.v[2] > s.near && c.v[2] > s.near {
            raster_tri(y0, y1, color, depth, [a, b, c], s);
        } else {
            let n = clip_poly([a, b, c], s.near, &mut poly);
            for k in 1..n.saturating_sub(1) {
                raster_tri(y0, y1, color, depth, [poly[0], poly[k], poly[k + 1]], s);
            }
        }
    }

    // Bone overlay: drawn last, always on top (it is an inspection aid, and the
    // bones live inside the mesh volume).
    for seg in s.bones {
        let Some((a, b)) = clip_segment(seg[0], seg[1], s.near) else {
            continue;
        };
        let pa = project(a, s);
        let pb = project(b, s);
        draw_line(y0, y1, s.bw, color, pa, pb, s.bone_color);
    }
}

/// Flat well colour, or the floor with its grid and shadow where the ray hits.
fn background(dx: f32, dy: f32, s: &BandShared) -> u8 {
    let Some(f) = s.floor else {
        return BG;
    };
    let dir = [
        s.fwd[0] + s.right[0] * dx + s.up[0] * dy,
        s.fwd[1] + s.right[1] * dx + s.up[1] * dy,
        s.fwd[2] + s.right[2] * dx + s.up[2] * dy,
    ];
    if dir[1] >= -1e-5 {
        return BG;
    }
    let t = (f.y - s.eye[1]) / dir[1];
    if t <= 0.0 {
        return BG;
    }
    let wx = s.eye[0] + dir[0] * t;
    let wz = s.eye[2] + dir[2] * t;
    let ddx = wx - s.target[0];
    let ddz = wz - s.target[2];
    let d2 = ddx * ddx + ddz * ddz;
    // Outside the grid's reach there is nothing to draw on the floor, and this
    // early-out is what keeps the per-pixel floor cost off the far field.
    if d2 >= f.reach * f.reach {
        return BG;
    }
    let dist = d2.sqrt();
    let fade = 1.0 - smoothstep(f.reach * 0.45, f.reach, dist);
    if fade <= 0.0 {
        return BG;
    }

    let mut shade = BG as f32 - 3.0 * fade;
    // Grid lines, anti-aliased by the pixel's footprint on the plane.
    let dir_len2 = dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2];
    let world_per_px = (t * dir_len2.sqrt() * 2.0 * s.tan_half / s.bh as f32).max(1e-5);
    let gx = (wx / f.step - (wx / f.step).round()).abs() * f.step;
    let gz = (wz / f.step - (wz / f.step).round()).abs() * f.step;
    let line = gx.min(gz);
    let lit = 1.0 - smoothstep(world_per_px * 0.7, world_per_px * 2.0, line);
    shade += 9.0 * lit * fade;
    // Contact shadow (a soft disc; squared distance keeps the sqrt out).
    if let Some(sh) = f.shadow {
        if sh.strength > 0.0 {
            let rx = wx - sh.center[0];
            let rz = wz - sh.center[1];
            let r2 = (rx * rx + rz * rz) / (sh.radius * sh.radius).max(1e-8);
            let soft = 1.0 - smoothstep(0.03, 1.0, r2);
            shade *= 1.0 - 0.55 * soft * sh.strength;
        }
    }
    shade.clamp(0.0, 255.0) as u8
}

#[derive(Clone, Copy)]
struct VVert {
    /// View-space position (z is the depth along the view axis).
    v: [f32; 3],
    /// World-space normal.
    n: [f32; 3],
}

impl VVert {
    const ZERO: VVert = VVert {
        v: [0.0; 3],
        n: [0.0, 0.0, 1.0],
    };
}

/// Sutherland-Hodgman against `z >= near`; the callers know at least one vertex
/// is behind the plane. Returns the number of valid entries in `out`.
fn clip_poly(v: [VVert; 3], near: f32, out: &mut [VVert; 4]) -> usize {
    let mut n = 0usize;
    for i in 0..3 {
        let a = v[i];
        let b = v[(i + 1) % 3];
        let da = a.v[2] - near;
        let db = b.v[2] - near;
        if da >= 0.0 {
            out[n] = a;
            n += 1;
        }
        if (da >= 0.0) != (db >= 0.0) && n < 4 {
            let t = da / (da - db);
            out[n] = VVert {
                v: lerp3(a.v, b.v, t),
                n: normalize(lerp3(a.n, b.n, t)),
            };
            n += 1;
        }
    }
    n.min(4)
}

fn raster_tri(y0: usize, y1: usize, color: &mut [u8], depth: &mut [f32], t: [VVert; 3], s: &BandShared) {
    let p0 = project(t[0].v, s);
    let p1 = project(t[1].v, s);
    let p2 = project(t[2].v, s);
    let (x0, y0s, z0) = (p0[0], p0[1], p0[2]);
    let (x1, y1s, z1) = (p1[0], p1[1], p1[2]);
    let (x2, y2s, z2) = (p2[0], p2[1], p2[2]);

    let top = y0s.min(y1s).min(y2s).floor().max(y0 as f32);
    let bottom = y0s.max(y1s).max(y2s).ceil().min((y1 - 1) as f32);
    if bottom < top {
        return;
    }
    let left = x0.min(x1).min(x2).floor().max(0.0) as usize;
    let right = (x0.max(x1).max(x2).ceil().min((s.bw - 1) as f32)).max(0.0) as usize;
    if left > right {
        return;
    }

    let area = edge(x0, y0s, x1, y1s, x2, y2s);
    if area.abs() < 1e-9 {
        return;
    }
    let inv_area = 1.0 / area;

    for py in top as usize..=bottom as usize {
        let cy = py as f32 + 0.5;
        let row = (py - y0) * s.bw;
        for px in left..=right {
            let cx = px as f32 + 0.5;
            let b0 = edge(x1, y1s, x2, y2s, cx, cy) * inv_area;
            let b1 = edge(x2, y2s, x0, y0s, cx, cy) * inv_area;
            let b2 = edge(x0, y0s, x1, y1s, cx, cy) * inv_area;
            let inside =
                (b0 >= 0.0 && b1 >= 0.0 && b2 >= 0.0) || (b0 <= 0.0 && b1 <= 0.0 && b2 <= 0.0);
            if !inside {
                continue;
            }
            let iz = b0 / z0 + b1 / z1 + b2 / z2;
            if iz <= 0.0 || !iz.is_finite() {
                continue;
            }
            let z = 1.0 / iz;
            let di = row + px;
            if z >= depth[di] {
                continue;
            }

            // Perspective-correct barycentrics.
            let w0 = b0 / z0 / iz;
            let w1 = b1 / z1 / iz;
            let w2 = b2 / z2 / iz;
            let world = [
                t[0].v[0] * w0 + t[1].v[0] * w1 + t[2].v[0] * w2 + s.eye[0],
                t[0].v[1] * w0 + t[1].v[1] * w1 + t[2].v[1] * w2 + s.eye[1],
                t[0].v[2] * w0 + t[1].v[2] * w1 + t[2].v[2] * w2 + s.eye[2],
            ];
            let mut n = normalize([
                t[0].n[0] * w0 + t[1].n[0] * w1 + t[2].n[0] * w2,
                t[0].n[1] * w0 + t[1].n[1] * w1 + t[2].n[1] * w2,
                t[0].n[2] * w0 + t[1].n[2] * w1 + t[2].n[2] * w2,
            ]);
            let view_dir = normalize(sub(s.eye, world));
            if dot(n, view_dir) < 0.0 {
                n = [-n[0], -n[1], -n[2]];
            }
            let nl = dot(n, KEY_DIR).max(0.0);
            let hemi = 0.5 + 0.5 * n[1];
            let rim = (1.0 - dot(n, view_dir).abs()).powi(3) * 0.10;
            let shade = ((0.18 + 0.34 * hemi + 0.60 * nl) * 0.78 + rim).clamp(0.0, 1.0);
            let c = (shade * 255.0 + 0.5) as u8;

            depth[di] = z;
            let o = di * 4;
            color[o] = c;
            color[o + 1] = c;
            color[o + 2] = c;
            color[o + 3] = 255;
        }
    }
}

fn project(v: [f32; 3], s: &BandShared) -> [f32; 3] {
    let inv = 1.0 / (v[2] * s.tan_half);
    [
        (v[0] * inv / s.aspect) * 0.5 * s.bw as f32 + 0.5 * s.bw as f32,
        0.5 * s.bh as f32 - (v[1] * inv) * 0.5 * s.bh as f32,
        v[2],
    ]
}

fn clip_segment(a: [f32; 3], b: [f32; 3], near: f32) -> Option<([f32; 3], [f32; 3])> {
    let da = a[2] - near;
    let db = b[2] - near;
    if da < 0.0 && db < 0.0 {
        return None;
    }
    if da >= 0.0 && db >= 0.0 {
        return Some((a, b));
    }
    let t = da / (da - db);
    let m = lerp3(a, b, t);
    if da < 0.0 {
        Some((m, b))
    } else {
        Some((a, m))
    }
}

/// A 2-pixel-wide line in screen space, blended over what is already there.
fn draw_line(
    y0: usize,
    y1: usize,
    bw: usize,
    color: &mut [u8],
    a: [f32; 3],
    b: [f32; 3],
    rgb: [u8; 3],
) {
    let (x0, y0f) = (a[0], a[1]);
    let (x1, y1f) = (b[0], b[1]);
    let steps = (x1 - x0).abs().max((y1f - y0f).abs()).ceil().max(1.0) as usize;
    if steps > 4096 {
        return;
    }
    let dx = (x1 - x0) / steps as f32;
    let dy = (y1f - y0f) / steps as f32;
    let mut x = x0;
    let mut y = y0f;
    for _ in 0..=steps {
        let px = x.round() as i32;
        let py = y.round() as i32;
        for (ox, oy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            blend_px(y0, y1, bw, color, px + ox, py + oy, rgb);
        }
        x += dx;
        y += dy;
    }
}

fn blend_px(
    y0: usize,
    y1: usize,
    bw: usize,
    color: &mut [u8],
    px: i32,
    py: i32,
    rgb: [u8; 3],
) {
    if px < 0 || px >= bw as i32 || py < y0 as i32 || py >= y1 as i32 {
        return;
    }
    let o = (py as usize - y0) * bw + px as usize;
    let Some(p) = color.get_mut(o * 4..o * 4 + 3) else {
        return;
    };
    for k in 0..3 {
        p[k] = ((p[k] as f32) * 0.25 + rgb[k] as f32 * 0.75).clamp(0.0, 255.0) as u8;
    }
}

fn to_view(p: [f32; 3], eye: [f32; 3], right: [f32; 3], up: [f32; 3], fwd: [f32; 3]) -> [f32; 3] {
    let d = sub(p, eye);
    [dot(d, right), dot(d, up), dot(d, fwd)]
}

#[inline]
fn basis(fwd: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let mut up = [0.0f32, 1.0, 0.0];
    let mut right = cross(fwd, up);
    if dot(right, right) < 1e-12 {
        up = [0.0, 0.0, 1.0];
        right = cross(fwd, up);
    }
    let right = normalize(right);
    let up = cross(right, fwd);
    (right, up)
}

#[inline]
fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

#[inline]
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = dot(v, v).sqrt();
    if l > 1e-12 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 0.0, 1.0]
    }
}