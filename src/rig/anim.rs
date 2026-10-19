//! glTF animation clips: one channel per (node, path) with its keyframe
//! times, and sampling that honours all three glTF interpolations.
//!
//! CUBICSPLINE output is stored in full (`in-tangent, value, out-tangent` per
//! key, as it sits in the buffer) and evaluated as the cubic Hermite spline
//! the spec defines, instead of throwing the tangents away.

use super::math::{lerp3, normalize4, slerp, Quat, Trs, Vec3};

/// Which node property a channel drives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Path {
    Translation,
    Rotation,
    Scale,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Interp {
    Step,
    Linear,
    Cubic,
}

/// The value stream of one channel. Cubic streams carry three entries per
/// keyframe (in-tangent, value, out-tangent); step/linear carry one.
pub enum Values {
    Vecs(Vec<Vec3>),
    Quats(Vec<Quat>),
}

pub struct Channel {
    pub node: usize,
    pub path: Path,
    pub times: Vec<f32>,
    pub interp: Interp,
    pub values: Values,
}

pub struct Clip {
    pub name: String,
    pub duration: f32,
    pub channels: Vec<Channel>,
}

impl Clip {
    /// Overwrite `trs` for every node this clip animates, at `time` (clamped
    /// to the clip's own range).
    pub fn apply(&self, trs: &mut [Trs], time: f32) {
        for ch in &self.channels {
            let Some(node) = trs.get_mut(ch.node) else {
                continue;
            };
            match ch.path {
                Path::Translation => {
                    if let Some(v) = ch.vec_at(time) {
                        node.t = v;
                    }
                }
                Path::Rotation => {
                    if let Some(v) = ch.quat_at(time) {
                        node.r = v;
                    }
                }
                Path::Scale => {
                    if let Some(v) = ch.vec_at(time) {
                        node.s = v;
                    }
                }
            }
        }
    }
}

impl Channel {
    fn vec_at(&self, t: f32) -> Option<Vec3> {
        let Values::Vecs(v) = &self.values else {
            return None;
        };
        let (i, j, a) = segment(&self.times, t)?;
        match self.interp {
            Interp::Step => v.get(i).copied(),
            Interp::Linear => {
                let v0 = *v.get(i)?;
                let v1 = *v.get(j)?;
                Some(if i == j { v0 } else { lerp3(v0, v1, a) })
            }
            Interp::Cubic => {
                let v0 = *v.get(i * 3 + 1)?;
                let out0 = *v.get(i * 3 + 2)?;
                let in1 = *v.get(j * 3)?;
                let v1 = *v.get(j * 3 + 1)?;
                Some(if i == j {
                    v0
                } else {
                    hermite3(v0, out0, in1, v1, self.times[j] - self.times[i], a)
                })
            }
        }
    }

    fn quat_at(&self, t: f32) -> Option<Quat> {
        let Values::Quats(v) = &self.values else {
            return None;
        };
        let (i, j, a) = segment(&self.times, t)?;
        match self.interp {
            Interp::Step => v.get(i).copied().map(normalize4),
            Interp::Linear => {
                let q0 = *v.get(i)?;
                let q1 = *v.get(j)?;
                Some(if i == j {
                    normalize4(q0)
                } else {
                    slerp(normalize4(q0), normalize4(q1), a)
                })
            }
            Interp::Cubic => {
                let q0 = *v.get(i * 3 + 1)?;
                let out0 = *v.get(i * 3 + 2)?;
                let in1 = *v.get(j * 3)?;
                let q1 = *v.get(j * 3 + 1)?;
                Some(if i == j {
                    normalize4(q0)
                } else {
                    let dt = self.times[j] - self.times[i];
                    normalize4([
                        hermite_scalar(q0[0], out0[0], in1[0], q1[0], dt, a),
                        hermite_scalar(q0[1], out0[1], in1[1], q1[1], dt, a),
                        hermite_scalar(q0[2], out0[2], in1[2], q1[2], dt, a),
                        hermite_scalar(q0[3], out0[3], in1[3], q1[3], dt, a),
                    ])
                })
            }
        }
    }
}

/// `(i, j, alpha)` with `times[i] <= t < times[j]`, clamped at both ends and
/// degenerating to `(i, i, 0)` for single-key or zero-length clips.
fn segment(times: &[f32], t: f32) -> Option<(usize, usize, f32)> {
    let n = times.len();
    if n == 0 || !t.is_finite() {
        return None;
    }
    if n == 1 || t <= times[0] {
        return Some((0, 0, 0.0));
    }
    if t >= times[n - 1] {
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

fn hermite_scalar(v0: f32, out0: f32, in1: f32, v1: f32, dt: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    (2.0 * t3 - 3.0 * t2 + 1.0) * v0
        + (t3 - 2.0 * t2 + t) * dt * out0
        + (-2.0 * t3 + 3.0 * t2) * v1
        + (t3 - t2) * dt * in1
}

fn hermite3(v0: Vec3, out0: Vec3, in1: Vec3, v1: Vec3, dt: f32, t: f32) -> Vec3 {
    [
        hermite_scalar(v0[0], out0[0], in1[0], v1[0], dt, t),
        hermite_scalar(v0[1], out0[1], in1[1], v1[1], dt, t),
        hermite_scalar(v0[2], out0[2], in1[2], v1[2], dt, t),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(interp: Interp, times: Vec<f32>, values: Values) -> Channel {
        Channel {
            node: 0,
            path: Path::Translation,
            times,
            interp,
            values,
        }
    }

    #[test]
    fn step_holds_the_left_key() {
        let ch = channel(
            Interp::Step,
            vec![0.0, 1.0],
            Values::Vecs(vec![[0.0; 3], [10.0; 3]]),
        );
        assert_eq!(ch.vec_at(0.0), Some([0.0; 3]));
        assert_eq!(ch.vec_at(0.999), Some([0.0; 3]));
        assert_eq!(ch.vec_at(1.0), Some([10.0; 3]));
        assert_eq!(ch.vec_at(5.0), Some([10.0; 3]), "clamped past the last key");
    }

    #[test]
    fn linear_interpolates_between_keys() {
        let ch = channel(
            Interp::Linear,
            vec![0.0, 2.0],
            Values::Vecs(vec![[0.0, 0.0, 0.0], [4.0, 8.0, 0.0]]),
        );
        assert_eq!(ch.vec_at(1.0), Some([2.0, 4.0, 0.0]));
    }

    #[test]
    fn cubic_uses_the_tangents() {
        // v0=0, out-tangent 0, v1=2, in-tangent 0 -> classic smoothstep.
        let ch = channel(
            Interp::Cubic,
            vec![0.0, 1.0],
            Values::Vecs(vec![[0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3], [2.0; 3], [0.0; 3]]),
        );
        assert_eq!(ch.vec_at(0.0), Some([0.0; 3]));
        assert_eq!(ch.vec_at(1.0), Some([2.0; 3]));
        let mid = ch.vec_at(0.5).unwrap();
        assert!((mid[0] - 1.0).abs() < 1e-5, "smoothstep midpoint = 1, got {mid:?}");

        // A non-zero out-tangent must bend the curve away from the straight
        // line: at t=0.25 the tangent term pulls ahead of linear.
        let ch2 = channel(
            Interp::Cubic,
            vec![0.0, 1.0],
            Values::Vecs(vec![[0.0; 3], [0.0; 3], [4.0; 3], [0.0; 3], [2.0; 3], [0.0; 3]]),
        );
        let q = ch2.vec_at(0.25).unwrap();
        assert!(q[0] > 0.5, "tangent must speed the start up, got {q:?}");
    }

    #[test]
    fn cubic_quaternions_stay_unit() {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let ch = Channel {
            node: 0,
            path: Path::Rotation,
            times: vec![0.0, 1.0],
            interp: Interp::Cubic,
            values: Values::Quats(vec![
                [0.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0, 0.0],
                [0.0, s, 0.0, s],
                [0.0, 0.0, 0.0, 0.0],
            ]),
        };
        for k in 0..=10 {
            let q = ch.quat_at(k as f32 / 10.0).unwrap();
            let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "unit quaternion, got {len}");
        }
    }

    #[test]
    fn degenerate_streams_do_not_panic() {
        let empty = channel(Interp::Linear, vec![], Values::Vecs(vec![]));
        assert_eq!(empty.vec_at(1.0), None);
        let single = channel(Interp::Linear, vec![0.5], Values::Vecs(vec![[1.0; 3]]));
        assert_eq!(single.vec_at(0.0), Some([1.0; 3]));
        assert_eq!(single.vec_at(9.0), Some([1.0; 3]));
        // A quat channel never answers vec_at and vice versa.
        let q = Channel {
            node: 0,
            path: Path::Rotation,
            times: vec![0.0, 1.0],
            interp: Interp::Linear,
            values: Values::Quats(vec![[0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]]),
        };
        assert_eq!(q.vec_at(0.5), None);
        assert!(q.quat_at(0.5).is_some());
    }

    #[test]
    fn clip_apply_ignores_out_of_range_nodes() {
        let clip = Clip {
            name: "t".into(),
            duration: 1.0,
            channels: vec![channel(
                Interp::Linear,
                vec![0.0, 1.0],
                Values::Vecs(vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]),
            )],
        };
        let mut trs = vec![Trs::default(); 0];
        clip.apply(&mut trs, 0.5); // must not panic
        let mut trs2 = vec![Trs::default(); 1];
        clip.apply(&mut trs2, 0.5);
        assert!((trs2[0].t[0] - 2.5).abs() < 1e-6);
    }
}
