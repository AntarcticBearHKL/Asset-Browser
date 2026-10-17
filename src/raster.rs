//! CPU software renderer for the rig stage: a z-buffered triangle rasterizer with
//! double-sided Lambert shading (grey albedo, one key light) — no GPU, no `wgpu`.
//!
//! Invariant: the caller passes a `w*h*4` RGBA8 buffer plus triangle indices that
//! address `pos`/`nrm`; the renderer owns its depth buffer and *drops* (does not
//! clip) triangles crossing the near plane.

/// Orbit camera: the eye sits on a sphere around `target`.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
    pub target: [f32; 3],
}

impl Camera {
    /// Frame an AABB the way the web's `frameModel` does: 45° vertical fov, the
    /// largest dimension sets the radius, and the eye starts on the (1,-0.4,0.9)
    /// ray (`dist = radius / tan(fov/2) * 1.9`).
    pub fn framed(min: [f32; 3], max: [f32; 3]) -> Camera {
        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let dx = (max[0] - min[0]).abs();
        let dy = (max[1] - min[1]).abs();
        let dz = (max[2] - min[2]).abs();
        let max_dim = dx.max(dy).max(dz).max(1e-4);
        let radius = max_dim * 0.55;
        let fov = 45f32.to_radians();
        let dist = radius / (fov * 0.5).tan() * 1.9;
        let dir = normalize([1.0, -0.4, 0.9]);
        let pitch = dir[1].clamp(-1.0, 1.0).asin();
        let yaw = dir[0].atan2(dir[2]);
        Camera {
            yaw,
            pitch,
            dist,
            target: center,
        }
    }

    /// World-space eye position.
    pub fn eye(&self) -> [f32; 3] {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        let dir = [cp * sy, sp, cp * cy];
        [
            self.target[0] + dir[0] * self.dist,
            self.target[1] + dir[1] * self.dist,
            self.target[2] + dir[2] * self.dist,
        ]
    }
}

const BG: u8 = 36; // #242424 — matches Theme.well (dark)
const AMBIENT: f32 = 0.30;
const DIFFUSE: f32 = 0.85;
const ALBEDO: f32 = 0.55;
const KEY_DIR: [f32; 3] = [1.0, 1.6, 0.8];

/// Rasterize `idx` (a triangle list) from `pos`/`nrm` into `pixels` (RGBA8, `w×h`).
pub fn render(
    pixels: &mut [u8],
    w: u32,
    h: u32,
    pos: &[[f32; 3]],
    nrm: &[[f32; 3]],
    idx: &[u32],
    cam: &Camera,
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

    // Background.
    for px in pixels[..need].chunks_exact_mut(4) {
        px[0] = BG;
        px[1] = BG;
        px[2] = BG;
        px[3] = 255;
    }

    let mut depth = vec![f32::INFINITY; ww * hh];

    let eye = cam.eye();
    let fwd = normalize(sub(cam.target, eye)); // into the screen
    let mut up = [0.0f32, 1.0, 0.0];
    let mut right = cross(fwd, up);
    if dot(right, right) < 1e-12 {
        up = [0.0, 0.0, 1.0];
        right = cross(fwd, up);
    }
    let right = normalize(right);
    let up = cross(right, fwd);

    let fov = 45f32.to_radians();
    let tan_half = (fov * 0.5).tan();
    let aspect = ww as f32 / hh as f32;
    let near = (cam.dist * 1e-4).max(1e-6);

    let light = normalize(KEY_DIR);
    let scale_x = 0.5 * ww as f32;
    let scale_y = 0.5 * hh as f32;

    // Project a world point to (screen_x, screen_y, view_z). Returns None when
    // the point is at/behind the near plane.
    let project = |p: [f32; 3]| -> Option<[f32; 3]> {
        let d = sub(p, eye);
        let z = dot(d, fwd);
        if z <= near {
            return None;
        }
        let xv = dot(d, right);
        let yv = dot(d, up);
        let inv = 1.0 / (z * tan_half);
        let sx = (xv * inv / aspect) * scale_x + scale_x;
        let sy = scale_y - (yv * inv) * scale_y;
        Some([sx, sy, z])
    };

    for tri in idx.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let (Some(p0), Some(p1), Some(p2)) = (
            pos.get(i0).copied(),
            pos.get(i1).copied(),
            pos.get(i2).copied(),
        ) else {
            continue;
        };
        let (Some(s0), Some(s1), Some(s2)) = (project(p0), project(p1), project(p2)) else {
            // Crude near-plane clipping: drop triangles that cross the plane.
            continue;
        };

        let n0 = nrm.get(i0).copied().unwrap_or([0.0, 0.0, 1.0]);
        let n1 = nrm.get(i1).copied().unwrap_or([0.0, 0.0, 1.0]);
        let n2 = nrm.get(i2).copied().unwrap_or([0.0, 0.0, 1.0]);

        let (x0, y0, z0) = (s0[0], s0[1], s0[2]);
        let (x1, y1, z1) = (s1[0], s1[1], s1[2]);
        let (x2, y2, z2) = (s2[0], s2[1], s2[2]);

        let area = edge(x0, y0, x1, y1, x2, y2);
        if area.abs() < 1e-9 {
            continue;
        }

        let minx = x0.min(x1).min(x2).floor().max(0.0) as usize;
        let maxx = (x0.max(x1).max(x2).ceil().min((ww - 1) as f32)).max(0.0) as usize;
        let miny = y0.min(y1).min(y2).floor().max(0.0) as usize;
        let maxy = (y0.max(y1).max(y2).ceil().min((hh - 1) as f32)).max(0.0) as usize;
        if minx > maxx || miny > maxy {
            continue;
        }

        let inv_area = 1.0 / area;
        // Face centre (world) for the double-sided normal flip.
        let face_center = [
            (p0[0] + p1[0] + p2[0]) / 3.0,
            (p0[1] + p1[1] + p2[1]) / 3.0,
            (p0[2] + p1[2] + p2[2]) / 3.0,
        ];
        let view_dir = normalize(sub(eye, face_center));

        for py in miny..=maxy {
            let cy = py as f32 + 0.5;
            for px in minx..=maxx {
                let cx = px as f32 + 0.5;
                let w0 = edge(x1, y1, x2, y2, cx, cy);
                let w1 = edge(x2, y2, x0, y0, cx, cy);
                let w2 = edge(x0, y0, x1, y1, cx, cy);
                let inside = (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0)
                    || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0);
                if !inside {
                    continue;
                }
                let b0 = w0 * inv_area;
                let b1 = w1 * inv_area;
                let b2 = w2 * inv_area;
                // Perspective-correct depth.
                let denom = b0 / z0 + b1 / z1 + b2 / z2;
                if denom.abs() < 1e-12 {
                    continue;
                }
                let z = 1.0 / denom;
                let di = py * ww + px;
                if z >= depth[di] {
                    continue;
                }

                let mut n = [
                    n0[0] * b0 + n1[0] * b1 + n2[0] * b2,
                    n0[1] * b0 + n1[1] * b1 + n2[1] * b2,
                    n0[2] * b0 + n1[2] * b1 + n2[2] * b2,
                ];
                n = normalize(n);
                if dot(n, view_dir) < 0.0 {
                    n = [-n[0], -n[1], -n[2]];
                }
                let ndl = dot(n, light).max(0.0);
                let shade = (ALBEDO * (AMBIENT + DIFFUSE * ndl)).clamp(0.0, 1.0);
                let c = (shade * 255.0 + 0.5) as u8;

                depth[di] = z;
                let o = di * 4;
                pixels[o] = c;
                pixels[o + 1] = c;
                pixels[o + 2] = c;
                pixels[o + 3] = 255;
            }
        }
    }
}

#[inline]
fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
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
