//! GLB loading + CPU linear-blend skinning for the rig viewer.
//!
//! Flattens a scene into one vertex/triangle list plus per-vertex
//! `JOINTS_0`/`WEIGHTS_0`; unskinned props get a synthetic single-bone joint on
//! their own node. Invariant: a skinned vertex is `Σ wᵢ·(jointGlobalᵢ·inverseBindᵢ)·p`
//! with the skinned node's own transform ignored (glTF) and `inverseBind` world-relative.

use std::collections::HashMap;

pub type Mat4 = [[f32; 4]; 4];
pub type Vec3 = [f32; 3];
pub type Quat = [f32; 4]; // x, y, z, w (matches glTF)

/// Default local transform of one node.
#[derive(Clone, Copy)]
struct NodeTrs {
    t: Vec3,
    r: Quat,
    s: Vec3,
}

impl Default for NodeTrs {
    fn default() -> Self {
        NodeTrs {
            t: [0.0, 0.0, 0.0],
            r: [0.0, 0.0, 0.0, 1.0],
            s: [1.0, 1.0, 1.0],
        }
    }
}

/// Which TRS property an animation channel drives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Path {
    Translation,
    Rotation,
    Scale,
}

/// The value stream of one channel.
enum Values {
    Vec(Vec<Vec3>),
    Quat(Vec<Quat>),
}

/// One animation channel: a node property plus its sampled keyframes.
struct Channel {
    node: usize,
    path: Path,
    times: Vec<f32>,
    values: Values,
    /// STEP holds the left key; everything else interpolates.
    step: bool,
}

impl Channel {
    fn sample_vec3(&self, t: f32) -> Option<Vec3> {
        let (v, step) = match &self.values {
            Values::Vec(v) => (v, self.step),
            Values::Quat(_) => return None,
        };
        let (i, j, a) = sample_at(&self.times, t)?;
        let v0 = *v.get(i)?;
        let v1 = *v.get(j)?;
        if step || i == j {
            Some(v0)
        } else {
            Some(lerp3(v0, v1, a))
        }
    }

    fn sample_quat(&self, t: f32) -> Option<Quat> {
        let v = match &self.values {
            Values::Quat(v) => v,
            Values::Vec(_) => return None,
        };
        let (i, j, a) = sample_at(&self.times, t)?;
        let q0 = *v.get(i)?;
        let q1 = *v.get(j)?;
        if self.step || i == j {
            Some(q0)
        } else {
            Some(slerp(q0, q1, a))
        }
    }
}

/// One animation clip.
pub struct Clip {
    pub name: String,
    pub duration: f32,
    channels: Vec<Channel>,
}

impl Clip {
    fn apply(&self, trs: &mut [NodeTrs], time: f32) {
        for ch in &self.channels {
            if ch.node >= trs.len() {
                continue;
            }
            match ch.path {
                Path::Translation => {
                    if let Some(v) = ch.sample_vec3(time) {
                        trs[ch.node].t = v;
                    }
                }
                Path::Rotation => {
                    if let Some(v) = ch.sample_quat(time) {
                        trs[ch.node].r = v;
                    }
                }
                Path::Scale => {
                    if let Some(v) = ch.sample_vec3(time) {
                        trs[ch.node].s = v;
                    }
                }
            }
        }
    }
}

/// A loaded, skinnable rig ready for the CPU rasterizer.
pub struct Rig {
    /// Bind-pose vertex positions in world space.
    pub rest_positions: Vec<Vec3>,
    /// Bind-pose vertex normals in world space.
    pub rest_normals: Vec<Vec3>,
    /// Triangle list (every primitive flattened into one buffer).
    pub indices: Vec<u32>,
    /// Per-vertex joint indices into the unified joint list.
    vjoints: Vec<[u16; 4]>,
    /// Per-vertex joint weights.
    vweights: Vec<[f32; 4]>,
    /// Unified joint list: skin joints first, then one synthetic joint per static
    /// mesh node. Node index into `nodes`.
    joint_nodes: Vec<usize>,
    inverse_bind: Vec<Mat4>,
    nodes: Vec<NodeTrs>,
    parents: Vec<Option<usize>>,
    clips: Vec<Clip>,
    aabb_min: Vec3,
    aabb_max: Vec3,
}

impl Rig {
    /// Display clips: `(name, duration)` with the `<slug>__` prefix stripped.
    pub fn clips(&self) -> Vec<(String, f32)> {
        self.clips
            .iter()
            .map(|c| (strip_clip_prefix(&c.name), c.duration))
            .collect()
    }

    /// Raw clip count (used by the tests / transport).
    pub fn clip_count(&self) -> usize {
        self.clips.len()
    }

    pub fn clip_duration(&self, clip: usize) -> f32 {
        self.clips.get(clip).map(|c| c.duration).unwrap_or(0.0)
    }

    /// Number of unified joints (skin joints + synthetic static-mesh joints).
    // Read only by the rig tests (`src/tests.rs`); the viewer never needs it.
    #[cfg(test)]
    pub fn joint_count(&self) -> usize {
        self.joint_nodes.len()
    }

    /// World-space AABB of the bind pose.
    pub fn aabb(&self) -> (Vec3, Vec3) {
        (self.aabb_min, self.aabb_max)
    }

    /// The world `4×4` matrix of every unified joint at `time` in `clip`.
    ///
    /// `clip` out of range or a clip shorter than `time` falls back to the bind pose.
    pub fn joint_matrices(&self, clip: usize, time: f32) -> Vec<Mat4> {
        let globals = match self.clips.get(clip) {
            Some(c) => {
                let mut trs = self.nodes.clone();
                c.apply(&mut trs, time);
                compute_globals(&trs, &self.parents)
            }
            None => compute_globals(&self.nodes, &self.parents),
        };
        let mut out = Vec::with_capacity(self.joint_nodes.len());
        for (j, node) in self.joint_nodes.iter().enumerate() {
            let g = globals.get(*node).copied().unwrap_or_else(identity);
            let ib = self.inverse_bind.get(j).copied().unwrap_or_else(identity);
            out.push(mat4_mul(&g, &ib));
        }
        out
    }

    /// Linear-blend skinning: deformed world positions + normals.
    pub fn skin(&self, joints: &[Mat4]) -> (Vec<Vec3>, Vec<Vec3>) {
        let n = self.rest_positions.len();
        let mut pos = vec![[0.0f32; 3]; n];
        let mut nrm = vec![[0.0f32; 3]; n];
        let id = identity();
        for v in 0..n {
            let p = self.rest_positions[v];
            let nn = self.rest_normals[v];
            let j = self.vjoints.get(v).copied().unwrap_or([0, 0, 0, 0]);
            let w = self.vweights.get(v).copied().unwrap_or([1.0, 0.0, 0.0, 0.0]);
            let mut acc = [0.0f32; 3];
            let mut nac = [0.0f32; 3];
            for k in 0..4 {
                let wk = w[k];
                if wk <= 0.0 {
                    continue;
                }
                let m = joints.get(j[k] as usize).copied().unwrap_or(id);
                let tp = transform_point(&m, p);
                acc[0] += tp[0] * wk;
                acc[1] += tp[1] * wk;
                acc[2] += tp[2] * wk;
                let tn = transform_dir(&m, nn);
                nac[0] += tn[0] * wk;
                nac[1] += tn[1] * wk;
                nac[2] += tn[2] * wk;
            }
            pos[v] = acc;
            let len2 = nac[0] * nac[0] + nac[1] * nac[1] + nac[2] * nac[2];
            nrm[v] = if len2 > 1e-12 {
                let inv = 1.0 / len2.sqrt();
                [nac[0] * inv, nac[1] * inv, nac[2] * inv]
            } else {
                nn
            };
        }
        (pos, nrm)
    }

    /// Skinning at the default (bind) pose.
    // Read only by the rig tests (`src/tests.rs`); the viewer never needs it.
    #[cfg(test)]
    pub fn bind_pose(&self) -> (Vec<Vec3>, Vec<Vec3>) {
        (self.rest_positions.clone(), self.rest_normals.clone())
    }
}

/// Load a `.glb`/`.gltf` and flatten it into a skinnable rig.
pub fn load(path: &str) -> Result<Rig, String> {
    let (doc, buffers, _images) =
        gltf::import(path).map_err(|e| format!("glb import failed: {e}"))?;
    let get_buf =
        |buffer: gltf::Buffer| buffers.get(buffer.index()).map(|b| b.0.as_slice());

    // ---- node graph (default TRS + parents) ------------------------------
    let n_nodes = doc.nodes().count();
    let mut nodes = vec![NodeTrs::default(); n_nodes];
    let mut parents: Vec<Option<usize>> = vec![None; n_nodes];
    for node in doc.nodes() {
        let i = node.index();
        if i >= n_nodes {
            continue;
        }
        let (t, r, s) = node.transform().decomposed();
        nodes[i] = NodeTrs { t, r, s };
        for child in node.children() {
            let c = child.index();
            if c < n_nodes {
                parents[c] = Some(i);
            }
        }
    }
    let globals = compute_globals(&nodes, &parents);

    // ---- unified joint list ----------------------------------------------
    let mut joint_nodes: Vec<usize> = Vec::new();
    let mut inverse_bind: Vec<Mat4> = Vec::new();
    let mut skin_base: HashMap<usize, usize> = HashMap::new();
    for skin in doc.skins() {
        let base = joint_nodes.len();
        let joints: Vec<usize> = skin.joints().map(|j| j.index()).collect();
        let ibms: Vec<Mat4> = skin
            .reader(&get_buf)
            .read_inverse_bind_matrices()
            .map(|it| it.collect())
            .unwrap_or_default();
        for (k, jn) in joints.iter().enumerate() {
            joint_nodes.push(*jn);
            inverse_bind.push(ibms.get(k).copied().unwrap_or_else(identity));
        }
        skin_base.insert(skin.index(), base);
    }
    let mut static_joint: HashMap<usize, usize> = HashMap::new();

    // ---- geometry --------------------------------------------------------
    let mut rest_positions: Vec<Vec3> = Vec::new();
    let mut rest_normals: Vec<Vec3> = Vec::new();
    let mut vjoints: Vec<[u16; 4]> = Vec::new();
    let mut vweights: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for node in doc.nodes() {
        let Some(mesh) = node.mesh() else { continue };
        let node_idx = node.index();
        let node_global = globals.get(node_idx).copied().unwrap_or_else(identity);
        let skin_block = node
            .skin()
            .and_then(|s| skin_base.get(&s.index()).copied());
        // Meshes without a skin get a synthetic joint equal to their own node, so
        // they track the bone they are parented to.
        let static_idx = if skin_block.is_none() {
            Some(*static_joint.entry(node_idx).or_insert_with(|| {
                let j = joint_nodes.len();
                joint_nodes.push(node_idx);
                inverse_bind.push(mat4_inverse(&node_global));
                j
            }))
        } else {
            None
        };

        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let reader = prim.reader(&get_buf);
            let Some(pos_iter) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<Vec3> = pos_iter.collect();
            if positions.is_empty() {
                continue;
            }
            let normals: Vec<Vec3> = reader
                .read_normals()
                .map(|it| it.collect())
                .unwrap_or_default();
            let prim_joints: Vec<[u16; 4]> = reader
                .read_joints(0)
                .map(|it| it.into_u16().collect())
                .unwrap_or_default();
            let prim_weights: Vec<[f32; 4]> = reader
                .read_weights(0)
                .map(|it| it.into_f32().collect())
                .unwrap_or_default();
            let prim_indices: Vec<u32> = reader
                .read_indices()
                .map(|it| it.into_u32().collect())
                .unwrap_or_default();

            let base = rest_positions.len();
            let nv = positions.len();
            for k in 0..nv {
                let p = positions[k];
                let nn = normals.get(k).copied().unwrap_or([0.0, 0.0, 1.0]);
                match skin_block {
                    Some(sb) => {
                        // Skinned verts keep their mesh-local rest position; the
                        // node's own transform is ignored (glTF spec).
                        rest_positions.push(p);
                        rest_normals.push(normalize(nn));
                        let j = prim_joints.get(k).copied().unwrap_or([0, 0, 0, 0]);
                        let w = prim_weights.get(k).copied().unwrap_or([0.0, 0.0, 0.0, 0.0]);
                        let map = |x: u16| -> u16 {
                            (sb + x as usize).min(u16::MAX as usize) as u16
                        };
                        vjoints.push([map(j[0]), map(j[1]), map(j[2]), map(j[3])]);
                        vweights.push(w);
                    }
                    None => {
                        rest_positions.push(transform_point(&node_global, p));
                        rest_normals.push(normalize(transform_dir(&node_global, nn)));
                        let j = static_idx.unwrap_or(0).min(u16::MAX as usize) as u16;
                        vjoints.push([j, 0, 0, 0]);
                        vweights.push([1.0, 0.0, 0.0, 0.0]);
                    }
                }
            }
            for ii in prim_indices {
                let gi = ii as usize;
                if gi < nv {
                    if let Some(gi) = gi.checked_add(base) {
                        if gi <= u32::MAX as usize {
                            indices.push(gi as u32);
                        }
                    }
                }
            }
        }
    }

    if rest_positions.is_empty() {
        return Err("glb contains no triangle geometry".to_string());
    }

    // ---- AABB of the bind pose -------------------------------------------
    let mut aabb_min = [f32::INFINITY; 3];
    let mut aabb_max = [f32::NEG_INFINITY; 3];
    for p in &rest_positions {
        for a in 0..3 {
            if p[a] < aabb_min[a] {
                aabb_min[a] = p[a];
            }
            if p[a] > aabb_max[a] {
                aabb_max[a] = p[a];
            }
        }
    }
    if !aabb_min.iter().all(|v| v.is_finite()) || !aabb_max.iter().all(|v| v.is_finite()) {
        return Err("glb has a non-finite bounding box".to_string());
    }

    // ---- clips -----------------------------------------------------------
    let mut clips: Vec<Clip> = Vec::new();
    for anim in doc.animations() {
        let name = anim.name().unwrap_or("").to_string();
        let mut channels: Vec<Channel> = Vec::new();
        let mut duration = 0.0f32;
        for ch in anim.channels() {
            let target = ch.target();
            let path = match target.property() {
                gltf::animation::Property::Translation => Path::Translation,
                gltf::animation::Property::Rotation => Path::Rotation,
                gltf::animation::Property::Scale => Path::Scale,
                // Morph-target weights are not rendered; skip them.
                _ => continue,
            };
            let node = target.node().index();
            if node >= n_nodes {
                continue;
            }
            let sampler = ch.sampler();
            let cubic = matches!(
                sampler.interpolation(),
                gltf::animation::Interpolation::CubicSpline
            );
            let step = matches!(
                sampler.interpolation(),
                gltf::animation::Interpolation::Step
            );
            let reader = ch.reader(&get_buf);
            let times: Vec<f32> = reader.read_inputs().map(|it| it.collect()).unwrap_or_default();
            if times.is_empty() {
                continue;
            }
            let values = match reader.read_outputs() {
                Some(gltf::animation::util::ReadOutputs::Translations(it)) => {
                    let raw: Vec<Vec3> = it.collect();
                    Values::Vec(reduce_cubic(raw, cubic))
                }
                Some(gltf::animation::util::ReadOutputs::Rotations(it)) => {
                    let raw: Vec<Quat> = it.into_f32().collect();
                    Values::Quat(reduce_cubic(raw, cubic))
                }
                Some(gltf::animation::util::ReadOutputs::Scales(it)) => {
                    let raw: Vec<Vec3> = it.collect();
                    Values::Vec(reduce_cubic(raw, cubic))
                }
                _ => continue,
            };
            if let Some(last) = times.last() {
                if last.is_finite() && *last > duration {
                    duration = *last;
                }
            }
            channels.push(Channel {
                node,
                path,
                times,
                values,
                step,
            });
        }
        clips.push(Clip {
            name,
            duration,
            channels,
        });
    }

    Ok(Rig {
        rest_positions,
        rest_normals,
        indices,
        vjoints,
        vweights,
        joint_nodes,
        inverse_bind,
        nodes,
        parents,
        clips,
        aabb_min,
        aabb_max,
    })
}

/// Drop everything up to and including the first `__` (mirrors `stripClipPrefix`).
fn strip_clip_prefix(name: &str) -> String {
    match name.find("__") {
        Some(i) => name[i + 2..].to_string(),
        None => name.to_string(),
    }
}

/// Keep the mid (value) key of CUBICSPLINE's `[in-tangent, value, out-tangent]`
/// triplets; the tangents are ignored and the channel is interpolated linearly.
fn reduce_cubic<T: Copy>(v: Vec<T>, cubic: bool) -> Vec<T> {
    if !cubic {
        return v;
    }
    let mut out = Vec::with_capacity(v.len() / 3 + 1);
    let mut i = 1;
    while i < v.len() {
        out.push(v[i]);
        i += 3;
    }
    out
}

/// Resolve the global matrix of each node, memoising along the way.
fn compute_globals(nodes: &[NodeTrs], parents: &[Option<usize>]) -> Vec<Mat4> {
    let n = nodes.len();
    let mut out = vec![identity(); n];
    let mut state = vec![0u8; n]; // 0 = unseen, 1 = in progress, 2 = done
    for i in 0..n {
        resolve_global(i, nodes, parents, &mut out, &mut state);
    }
    out
}

fn resolve_global(
    i: usize,
    nodes: &[NodeTrs],
    parents: &[Option<usize>],
    out: &mut [Mat4],
    state: &mut [u8],
) {
    if i >= nodes.len() || state[i] == 2 {
        return;
    }
    if state[i] == 1 {
        // Cycle guard: fall back to the local transform.
        out[i] = local_matrix(&nodes[i]);
        state[i] = 2;
        return;
    }
    state[i] = 1;
    let local = local_matrix(&nodes[i]);
    let parent = parents.get(i).copied().flatten();
    match parent {
        Some(p) if p < nodes.len() && p != i => {
            resolve_global(p, nodes, parents, out, state);
            out[i] = mat4_mul(&out[p], &local);
        }
        _ => out[i] = local,
    }
    state[i] = 2;
}

/// `(i, j, alpha)` such that `times[i] <= t < times[j]` (or the clamped ends).
fn sample_at(times: &[f32], t: f32) -> Option<(usize, usize, f32)> {
    let n = times.len();
    if n == 0 || !t.is_finite() {
        return None;
    }
    if n == 1 || t <= times[0] {
        return Some((0, (n - 1).max(0), 0.0));
    }
    let last = times[n - 1];
    if t >= last {
        return Some((n - 1, n - 1, 0.0));
    }
    let (mut lo, mut hi) = (0usize, n - 1);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if times[mid] <= t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (t0, t1) = (times[lo], times[hi]);
    let dt = t1 - t0;
    let a = if dt > 0.0 { (t - t0) / dt } else { 0.0 };
    Some((lo, hi, a.clamp(0.0, 1.0)))
}

/* ------------------------------ math ------------------------------------ */

pub fn identity() -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn quat_mat4(q: Quat) -> Mat4 {
    let [x, y, z, w] = q;
    let (xx, yy, zz) = (x * x, y * y, z * z);
    let (xy, xz, yz) = (x * y, x * z, y * z);
    let (wx, wy, wz) = (w * x, w * y, w * z);
    [
        [1.0 - 2.0 * (yy + zz), 2.0 * (xy + wz), 2.0 * (xz - wy), 0.0],
        [2.0 * (xy - wz), 1.0 - 2.0 * (xx + zz), 2.0 * (yz + wx), 0.0],
        [2.0 * (xz + wy), 2.0 * (yz - wx), 1.0 - 2.0 * (xx + yy), 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn local_matrix(n: &NodeTrs) -> Mat4 {
    let mut m = quat_mat4(n.r);
    for col in 0..3 {
        for row in 0..3 {
            m[col][row] *= n.s[col];
        }
    }
    m[3][0] = n.t[0];
    m[3][1] = n.t[1];
    m[3][2] = n.t[2];
    m[3][3] = 1.0;
    m
}

/// Column-major `a * b`.
pub fn mat4_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut c = [[0.0f32; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k][row] * b[col][k];
            }
            c[col][row] = s;
        }
    }
    c
}

fn transform_point(m: &Mat4, p: Vec3) -> Vec3 {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
    ]
}

fn transform_dir(m: &Mat4, v: Vec3) -> Vec3 {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
    ]
}

/// General 4×4 inverse (column-major, gl-matrix algorithm). Falls back to the
/// identity for a singular matrix.
pub fn mat4_inverse(m: &Mat4) -> Mat4 {
    let a = |c: usize, r: usize| m[c][r];
    let a00 = a(0, 0);
    let a01 = a(0, 1);
    let a02 = a(0, 2);
    let a03 = a(0, 3);
    let a10 = a(1, 0);
    let a11 = a(1, 1);
    let a12 = a(1, 2);
    let a13 = a(1, 3);
    let a20 = a(2, 0);
    let a21 = a(2, 1);
    let a22 = a(2, 2);
    let a23 = a(2, 3);
    let a30 = a(3, 0);
    let a31 = a(3, 1);
    let a32 = a(3, 2);
    let a33 = a(3, 3);

    let b00 = a00 * a11 - a01 * a10;
    let b01 = a00 * a12 - a02 * a10;
    let b02 = a00 * a13 - a03 * a10;
    let b03 = a01 * a12 - a02 * a11;
    let b04 = a01 * a13 - a03 * a11;
    let b05 = a02 * a13 - a03 * a12;
    let b06 = a20 * a31 - a21 * a30;
    let b07 = a20 * a32 - a22 * a30;
    let b08 = a20 * a33 - a23 * a30;
    let b09 = a21 * a32 - a22 * a31;
    let b10 = a21 * a33 - a23 * a31;
    let b11 = a22 * a33 - a23 * a32;

    let det = b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
    if det.abs() < 1e-20 || !det.is_finite() {
        return identity();
    }
    let inv = 1.0 / det;

    let mut o = [[0.0f32; 4]; 4];
    o[0][0] = (a11 * b11 - a12 * b10 + a13 * b09) * inv;
    o[0][1] = (a02 * b10 - a01 * b11 - a03 * b09) * inv;
    o[0][2] = (a31 * b05 - a32 * b04 + a33 * b03) * inv;
    o[0][3] = (a22 * b04 - a21 * b05 - a23 * b03) * inv;
    o[1][0] = (a12 * b08 - a10 * b11 - a13 * b07) * inv;
    o[1][1] = (a00 * b11 - a02 * b08 + a03 * b07) * inv;
    o[1][2] = (a32 * b02 - a30 * b05 - a33 * b01) * inv;
    o[1][3] = (a20 * b05 - a22 * b02 + a23 * b01) * inv;
    o[2][0] = (a10 * b10 - a11 * b08 + a13 * b06) * inv;
    o[2][1] = (a01 * b08 - a00 * b10 - a03 * b06) * inv;
    o[2][2] = (a30 * b04 - a31 * b02 + a33 * b00) * inv;
    o[2][3] = (a21 * b02 - a20 * b04 - a23 * b00) * inv;
    o[3][0] = (a11 * b07 - a10 * b09 - a12 * b06) * inv;
    o[3][1] = (a00 * b09 - a01 * b07 + a02 * b06) * inv;
    o[3][2] = (a31 * b01 - a30 * b03 - a32 * b00) * inv;
    o[3][3] = (a20 * b03 - a21 * b01 + a22 * b00) * inv;
    o
}

fn lerp3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn slerp(a: Quat, b: Quat, t: f32) -> Quat {
    let mut b = b;
    let mut d = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    if d < 0.0 {
        b = [-b[0], -b[1], -b[2], -b[3]];
        d = -d;
    }
    if d > 0.9995 {
        return normalize4([
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
            a[3] + (b[3] - a[3]) * t,
        ]);
    }
    let theta0 = d.clamp(-1.0, 1.0).acos();
    let sin0 = theta0.sin();
    if sin0.abs() < 1e-8 {
        return a;
    }
    let s0 = ((1.0 - t) * theta0).sin() / sin0;
    let s1 = (t * theta0).sin() / sin0;
    [
        a[0] * s0 + b[0] * s1,
        a[1] * s0 + b[1] * s1,
        a[2] * s0 + b[2] * s1,
        a[3] * s0 + b[3] * s1,
    ]
}

fn normalize4(v: Quat) -> Quat {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2] + v[3] * v[3]).sqrt();
    if l > 1e-12 {
        [v[0] / l, v[1] / l, v[2] / l, v[3] / l]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

fn normalize(v: Vec3) -> Vec3 {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-12 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 0.0, 1.0]
    }
}
