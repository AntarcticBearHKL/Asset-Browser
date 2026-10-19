//! GLB loading + CPU linear-blend skinning for the rig viewer.
//!
//! Flattens a scene into one vertex/triangle list plus per-vertex
//! `JOINTS_0`/`WEIGHTS_0`; unskinned props get a synthetic single-bone joint on
//! their own node.
//!
//! Invariants:
//! * a skinned vertex is `Σ wᵢ·(jointGlobalᵢ·inverseBindᵢ)·p`, with `w` normalised
//!   at load and the skinned node's own transform ignored (glTF: the joints
//!   already place the vertices in world space);
//! * an unskinned vertex is baked into world space at load and rides a synthetic
//!   joint whose inverse bind is its own node's bind global, so it follows the
//!   bone it is parented to;
//! * clips are ordered like the asset's `.clips.json` (falling back to the GLB
//!   order), and `Idle` is the default selection.

mod anim;
mod math;

pub use math::{Mat4, Vec3};

use std::collections::HashMap;

use anim::{Channel, Clip, Interp, Path, Values};
use math::{identity, mat4_inverse, mat4_mul, normalize3, transform_dir, transform_point, Trs};

/// World-space poses of every visible vertex at one clip time.
pub struct Pose {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    /// Bounds of the deformed mesh — what the camera frames.
    pub min: Vec3,
    pub max: Vec3,
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
    /// Per-vertex joint weights (normalised).
    vweights: Vec<[f32; 4]>,
    /// Unified joint list: skin joints first, then one synthetic joint per static
    /// mesh node. Node index into `nodes`.
    joint_nodes: Vec<usize>,
    inverse_bind: Vec<Mat4>,
    nodes: Vec<Trs>,
    parents: Vec<Option<usize>>,
    clips: Vec<Clip>,
    /// Clip to open on (see `pick_default_clip`).
    default_clip: usize,
    aabb_min: Vec3,
    aabb_max: Vec3,
}

impl Rig {
    /// Display clips: `(name, duration)` in the asset's own order, with the
    /// `<slug>__` prefix stripped.
    pub fn clips(&self) -> Vec<(String, f32)> {
        self.clips
            .iter()
            .map(|c| (strip_clip_prefix(&c.name), c.duration))
            .collect()
    }

    pub fn clip_count(&self) -> usize {
        self.clips.len()
    }

    pub fn clip_duration(&self, clip: usize) -> f32 {
        self.clips.get(clip).map(|c| c.duration).unwrap_or(0.0)
    }

    /// The clip to open on: an animated `Idle` when the asset has one, else
    /// the first clip that actually moves the skeleton (a pose line-up often
    /// starts with a frozen `00_T-pose` take).
    pub fn default_clip(&self) -> usize {
        self.default_clip
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

    /// Node TRS at `time` in `clip` (defaults for anything the clip omits).
    fn posed_nodes(&self, clip: usize, time: f32) -> Vec<Trs> {
        let mut trs = self.nodes.clone();
        if let Some(c) = self.clips.get(clip) {
            c.apply(&mut trs, time);
        }
        trs
    }

    /// The world `4×4` matrix of every unified joint at `time` in `clip`.
    ///
    /// `clip` out of range falls back to the bind pose.
    pub fn joint_matrices(&self, clip: usize, time: f32) -> Vec<Mat4> {
        self.palette(&compute_globals(&self.posed_nodes(clip, time), &self.parents))
    }

    /// Joint palette at the file's own bind pose — no clip applied. This is
    /// what the viewer shows for the skeleton's `Preview` entry.
    pub fn bind_matrices(&self) -> Vec<Mat4> {
        self.palette(&compute_globals(&self.nodes, &self.parents))
    }

    fn palette(&self, globals: &[Mat4]) -> Vec<Mat4> {
        let mut out = Vec::with_capacity(self.joint_nodes.len());
        for (j, node) in self.joint_nodes.iter().enumerate() {
            let g = globals.get(*node).copied().unwrap_or_else(identity);
            let ib = self.inverse_bind.get(j).copied().unwrap_or_else(identity);
            out.push(mat4_mul(&g, &ib));
        }
        out
    }

    /// Deformed world positions and normals for a joint palette.
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
            let mut total = 0.0f32;
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
                total += wk;
            }
            if total <= 1e-8 {
                // A vertex with no usable influence stays where it was rather
                // than collapsing onto the origin.
                pos[v] = p;
                nrm[v] = nn;
                continue;
            }
            let inv = 1.0 / total;
            pos[v] = [acc[0] * inv, acc[1] * inv, acc[2] * inv];
            let len2 = nac[0] * nac[0] + nac[1] * nac[1] + nac[2] * nac[2];
            nrm[v] = if len2 > 1e-12 {
                let s = 1.0 / len2.sqrt();
                [nac[0] * s, nac[1] * s, nac[2] * s]
            } else {
                nn
            };
        }
        (pos, nrm)
    }

    /// Skinning at one clip time, plus the deformed bounds the camera frames.
    pub fn pose(&self, clip: usize, time: f32) -> Pose {
        let joints = self.joint_matrices(clip, time);
        let (positions, normals) = self.skin(&joints);
        self.finish_pose(positions, normals)
    }

    /// The bind pose (no clip): what the `Preview` view shows. Skinned with
    /// the bind palette, which reproduces the rest geometry exactly.
    pub fn pose_bind(&self) -> Pose {
        let (positions, normals) = self.skin(&self.bind_matrices());
        self.finish_pose(positions, normals)
    }

    fn finish_pose(&self, positions: Vec<Vec3>, normals: Vec<Vec3>) -> Pose {
        let (mut min, mut max) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for p in &positions {
            for a in 0..3 {
                min[a] = min[a].min(p[a]);
                max[a] = max[a].max(p[a]);
            }
        }
        if !min.iter().chain(max.iter()).all(|v| v.is_finite()) {
            (min, max) = (self.aabb_min, self.aabb_max);
        }
        Pose {
            positions,
            normals,
            min,
            max,
        }
    }

    /// Skeleton segments at `time` in `clip`, as `[parent, child]` world
    /// positions — one per joint that has another joint as its parent.
    pub fn skeleton(&self, clip: usize, time: f32) -> Vec<[[f32; 3]; 2]> {
        self.skeleton_of(&compute_globals(&self.posed_nodes(clip, time), &self.parents))
    }

    /// Skeleton segments at the bind pose (no clip).
    pub fn skeleton_bind(&self) -> Vec<[[f32; 3]; 2]> {
        self.skeleton_of(&compute_globals(&self.nodes, &self.parents))
    }

    fn skeleton_of(&self, globals: &[Mat4]) -> Vec<[[f32; 3]; 2]> {
        let mut is_joint = vec![false; self.nodes.len()];
        for &n in &self.joint_nodes {
            if n < is_joint.len() {
                is_joint[n] = true;
            }
        }
        let at = |n: usize| -> Vec3 {
            let m = &globals[n];
            [m[3][0], m[3][1], m[3][2]]
        };
        let mut out = Vec::new();
        for (i, parent) in self.parents.iter().enumerate() {
            if !is_joint.get(i).copied().unwrap_or(false) {
                continue;
            }
            let Some(p) = parent else { continue };
            if *p == i || !is_joint.get(*p).copied().unwrap_or(false) {
                continue;
            }
            out.push([at(*p), at(i)]);
        }
        out
    }

    /// Test-only: one line per channel — `(node, path, keys, t0, t1, spread)`
    /// where `spread` is the largest component range across the channel's
    /// values. Used by `diag_rig_animation_coverage` to explain clips that do
    /// not move anything.
    #[cfg(test)]
    pub fn channel_summary(&self, clip: usize) -> Vec<(usize, &'static str, usize, f32, f32, f32)> {
        let Some(c) = self.clips.get(clip) else {
            return Vec::new();
        };
        c.channels
            .iter()
            .map(|ch| {
                let (t0, t1) = match (ch.times.first(), ch.times.last()) {
                    (Some(a), Some(b)) => (*a, *b),
                    _ => (0.0, 0.0),
                };
                let spread = match &ch.values {
                    anim::Values::Vecs(v) => spread3(v),
                    anim::Values::Quats(v) => spread4(v),
                };
                let path = match ch.path {
                    Path::Translation => "T",
                    Path::Rotation => "R",
                    Path::Scale => "S",
                };
                (ch.node, path, ch.times.len(), t0, t1, spread)
            })
            .collect()
    }

    /// Test-only: whether a node is one of the unified joints.
    #[cfg(test)]
    pub fn node_is_joint(&self, node: usize) -> bool {
        self.joint_nodes.contains(&node)
    }

    /// Skinning at the default (bind) pose.
    // Read only by the rig tests (`src/tests.rs`); the viewer never needs it.
    #[cfg(test)]
    pub fn bind_pose(&self) -> (Vec<Vec3>, Vec<Vec3>) {
        (self.rest_positions.clone(), self.rest_normals.clone())
    }
}

#[cfg(test)]
fn spread3(values: &[[f32; 3]]) -> f32 {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for v in values {
        for k in 0..3 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    (0..3).map(|k| hi[k] - lo[k]).fold(0.0f32, f32::max)
}

#[cfg(test)]
fn spread4(values: &[[f32; 4]]) -> f32 {
    let mut lo = [f32::INFINITY; 4];
    let mut hi = [f32::NEG_INFINITY; 4];
    for v in values {
        for k in 0..4 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    (0..4).map(|k| hi[k] - lo[k]).fold(0.0f32, f32::max)
}

/// Load a `.glb`/`.gltf` and flatten it into a skinnable rig.
pub fn load(path: &str) -> Result<Rig, String> {
    let (doc, buffers, _images) =
        gltf::import(path).map_err(|e| format!("glb import failed: {e}"))?;
    let get_buf =
        |buffer: gltf::Buffer| buffers.get(buffer.index()).map(|b| b.0.as_slice());

    // ---- node graph (default TRS + parents) ------------------------------
    let n_nodes = doc.nodes().count();
    let mut nodes = vec![Trs::default(); n_nodes];
    let mut parents: Vec<Option<usize>> = vec![None; n_nodes];
    for node in doc.nodes() {
        let i = node.index();
        if i >= n_nodes {
            continue;
        }
        let (t, r, s) = node.transform().decomposed();
        nodes[i] = Trs { t, r, s };
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

    // A rig that has a skin may also carry *unskinned copies of the same body*
    // (exported pose line-ups: 30–40 frozen duplicates, one skinned). Those
    // copies can never follow the animation, so only the live character is
    // kept; bone-attached props stay because their node sits under a joint.
    let skinned_rig = doc
        .nodes()
        .any(|n| n.mesh().is_some() && n.skin().is_some());
    let skin_joints: std::collections::HashSet<usize> = joint_nodes.iter().copied().collect();
    let under_joint = |mut i: usize| -> bool {
        for _ in 0..64 {
            match parents.get(i).copied().flatten() {
                Some(p) => {
                    if skin_joints.contains(&p) {
                        return true;
                    }
                    i = p;
                }
                None => return false,
            }
        }
        false
    };

    // ---- geometry --------------------------------------------------------
    let mut rest_positions: Vec<Vec3> = Vec::new();
    let mut rest_normals: Vec<Vec3> = Vec::new();
    let mut vjoints: Vec<[u16; 4]> = Vec::new();
    let mut vweights: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for node in doc.nodes() {
        let Some(mesh) = node.mesh() else { continue };
        if skinned_rig && node.skin().is_none() && !under_joint(node.index()) {
            continue;
        }
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
                        rest_normals.push(normalize3(nn));
                        let j = prim_joints.get(k).copied().unwrap_or([0, 0, 0, 0]);
                        let mut w = prim_weights
                            .get(k)
                            .copied()
                            .unwrap_or([0.0, 0.0, 0.0, 0.0]);
                        let sum: f32 = w.iter().sum();
                        if sum > 1e-6 {
                            for x in &mut w {
                                *x /= sum;
                            }
                        } else {
                            w = [1.0, 0.0, 0.0, 0.0];
                        }
                        let map = |x: u16| -> u16 {
                            (sb + x as usize).min(u16::MAX as usize) as u16
                        };
                        vjoints.push([map(j[0]), map(j[1]), map(j[2]), map(j[3])]);
                        vweights.push(w);
                    }
                    None => {
                        rest_positions.push(transform_point(&node_global, p));
                        rest_normals.push(normalize3(transform_dir(&node_global, nn)));
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
    for animation in doc.animations() {
        let name = animation.name().unwrap_or("").to_string();
        let mut channels: Vec<Channel> = Vec::new();
        let mut duration = 0.0f32;
        for ch in animation.channels() {
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
            let interp = match sampler.interpolation() {
                gltf::animation::Interpolation::Step => Interp::Step,
                gltf::animation::Interpolation::CubicSpline => Interp::Cubic,
                // Linear is the default and the format's other value.
                _ => Interp::Linear,
            };
            let reader = ch.reader(&get_buf);
            let times: Vec<f32> = reader.read_inputs().map(|it| it.collect()).unwrap_or_default();
            if times.is_empty() {
                continue;
            }
            let values = match reader.read_outputs() {
                Some(gltf::animation::util::ReadOutputs::Translations(it)) => {
                    Values::Vecs(it.collect())
                }
                Some(gltf::animation::util::ReadOutputs::Rotations(it)) => {
                    Values::Quats(it.into_f32().collect())
                }
                Some(gltf::animation::util::ReadOutputs::Scales(it)) => {
                    Values::Vecs(it.collect())
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
                interp,
                values,
            });
        }
        clips.push(Clip {
            name,
            duration,
            channels,
        });
    }
    reorder_clips(path, &mut clips);
    let default_clip = pick_default_clip(&nodes, &parents, &clips);

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
        default_clip,
        aabb_min,
        aabb_max,
    })
}

/// Pick the clip to open on: an animated `Idle` if there is one, else the first
/// clip whose skeleton actually moves. Pose line-ups often start with a frozen
/// `00_T-pose` take, which would look like "the animation is not applied".
fn pick_default_clip(nodes: &[Trs], parents: &[Option<usize>], clips: &[Clip]) -> usize {
    let animated: Vec<bool> = clips
        .iter()
        .map(|c| clip_moves(nodes, parents, c))
        .collect();
    let idle = clips
        .iter()
        .position(|c| strip_clip_prefix(&c.name).eq_ignore_ascii_case("idle"));
    match idle {
        Some(i) if animated.get(i).copied().unwrap_or(false) => i,
        _ => animated.iter().position(|a| *a).unwrap_or(0),
    }
}

/// Whether `clip` changes any node's global transform over its duration.
fn clip_moves(nodes: &[Trs], parents: &[Option<usize>], clip: &Clip) -> bool {
    if clip.duration <= 0.0 || clip.channels.is_empty() {
        return false;
    }
    let global_at = |t: f32| {
        let mut trs = nodes.to_vec();
        clip.apply(&mut trs, t);
        compute_globals(&trs, parents)
    };
    let base = global_at(0.0);
    for k in 1..=3 {
        let m = global_at(clip.duration * k as f32 / 3.0);
        let moved = m.iter().zip(&base).any(|(a, b)| {
            (0..4).any(|c| (0..4).any(|r| (a[c][r] - b[c][r]).abs() > 1e-5))
        });
        if moved {
            return true;
        }
    }
    false
}

/// Read `<stem>.clips.json` next to the GLB and put the clips in the asset's
/// catalog order, so a rig opens on `Idle` instead of whatever the exporter
/// wrote first. Clips the JSON does not mention keep their GLB order at the
/// end; a missing file leaves the GLB order untouched.
fn reorder_clips(glb_path: &str, clips: &mut Vec<Clip>) {
    let sidecar = std::path::Path::new(glb_path).with_extension("clips.json");
    let Ok(text) = std::fs::read_to_string(&sidecar) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let Some(list) = json.get("clips").and_then(|v| v.as_array()) else {
        return;
    };
    let mut slots: Vec<Option<Clip>> = clips.drain(..).map(Some).collect();
    let mut ordered: Vec<Clip> = Vec::with_capacity(slots.len());
    for name in list.iter().filter_map(|c| c.get("name")?.as_str()) {
        let want = strip_clip_prefix(name);
        let found = slots.iter().position(|slot| {
            slot.as_ref()
                .map(|c| strip_clip_prefix(&c.name) == want)
                .unwrap_or(false)
        });
        if let Some(i) = found {
            if let Some(clip) = slots[i].take() {
                ordered.push(clip);
            }
        }
    }
    ordered.extend(slots.into_iter().flatten());
    *clips = ordered;
}

/// Drop everything up to and including the first `__` (mirrors `stripClipPrefix`).
fn strip_clip_prefix(name: &str) -> String {
    match name.find("__") {
        Some(i) => name[i + 2..].to_string(),
        None => name.to_string(),
    }
}

/// Resolve the global matrix of each node, memoising along the way.
fn compute_globals(nodes: &[Trs], parents: &[Option<usize>]) -> Vec<Mat4> {
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
    nodes: &[Trs],
    parents: &[Option<usize>],
    out: &mut [Mat4],
    state: &mut [u8],
) {
    if i >= nodes.len() || state[i] == 2 {
        return;
    }
    if state[i] == 1 {
        // Cycle guard: fall back to the local transform.
        out[i] = nodes[i].matrix();
        state[i] = 2;
        return;
    }
    state[i] = 1;
    let local = nodes[i].matrix();
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
