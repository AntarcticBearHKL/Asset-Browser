//! Column-major 4x4 / quaternion math for the rig viewer.
//!
//! Matrices are `m[column][row]`, the layout glTF accessors use, so values can
//! be copied into a `Mat4` without transposing. Composition is `a * b`, i.e.
//! `a` applied after `b` (the usual graphics order).

pub type Mat4 = [[f32; 4]; 4];
pub type Vec3 = [f32; 3];
/// `x, y, z, w` — glTF's component order.
pub type Quat = [f32; 4];

pub fn identity() -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// One node's local translation / rotation / scale.
#[derive(Clone, Copy)]
pub struct Trs {
    pub t: Vec3,
    pub r: Quat,
    pub s: Vec3,
}

impl Default for Trs {
    fn default() -> Self {
        Trs {
            t: [0.0, 0.0, 0.0],
            r: [0.0, 0.0, 0.0, 1.0],
            s: [1.0, 1.0, 1.0],
        }
    }
}

impl Trs {
    /// `T * R * S` — scale first, then rotate, then translate.
    pub fn matrix(&self) -> Mat4 {
        let mut m = quat_mat4(self.r);
        // Scaling each basis column by its axis is `R * diag(s)`.
        for col in 0..3 {
            for row in 0..3 {
                m[col][row] *= self.s[col];
            }
        }
        m[3][0] = self.t[0];
        m[3][1] = self.t[1];
        m[3][2] = self.t[2];
        m
    }
}

pub fn quat_mat4(q: Quat) -> Mat4 {
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

pub fn transform_point(m: &Mat4, p: Vec3) -> Vec3 {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
    ]
}

pub fn transform_dir(m: &Mat4, v: Vec3) -> Vec3 {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
    ]
}

/// General 4x4 inverse (column-major, gl-matrix algorithm). A singular or
/// non-finite matrix falls back to the identity so a bad asset can never emit
/// NaN vertices.
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

pub fn lerp3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Spherical interpolation of unit quaternions, shortest arc.
pub fn slerp(mut a: Quat, mut b: Quat, t: f32) -> Quat {
    let mut d = dot4(a, b);
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
        a = normalize4(a);
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

#[inline]
pub fn dot4(a: Quat, b: Quat) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}

pub fn normalize3(v: Vec3) -> Vec3 {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-12 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 0.0, 1.0]
    }
}

pub fn normalize4(v: Quat) -> Quat {
    let l = dot4(v, v).sqrt();
    if l > 1e-12 {
        [v[0] / l, v[1] / l, v[2] / l, v[3] / l]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn trs_scales_rotates_then_translates() {
        // 90° about +Y (x, y, z, w), uniform scale 2, translate (1, 2, 3).
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let trs = Trs {
            t: [1.0, 2.0, 3.0],
            r: [0.0, s, 0.0, s],
            s: [2.0, 2.0, 2.0],
        };
        let m = trs.matrix();
        // (1,0,0) -> rotate 90° about Y -> (0,0,-1) -> scale 2 -> (0,0,-2).
        let p = transform_point(&m, [1.0, 0.0, 0.0]);
        assert!(close(p[0], 1.0) && close(p[1], 2.0) && close(p[2], 1.0), "{p:?}");
        // Direction ignores translation.
        let d = transform_dir(&m, [0.0, 1.0, 0.0]);
        assert!(close(d[0], 0.0) && close(d[1], 2.0) && close(d[2], 0.0), "{d:?}");
    }

    #[test]
    fn inverse_round_trips_a_compound_matrix() {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let a = Trs {
            t: [3.0, -2.0, 0.5],
            r: [s, 0.0, 0.0, s],
            s: [1.0, 2.0, 3.0],
        }
        .matrix();
        let ia = mat4_inverse(&a);
        let p = [0.7, -1.3, 2.1];
        let round = transform_point(&ia, transform_point(&a, p));
        assert!(
            close(round[0], p[0]) && close(round[1], p[1]) && close(round[2], p[2]),
            "{round:?}"
        );
        // A singular matrix must not produce NaN/inf.
        let zero = [[0.0f32; 4]; 4];
        let inv = mat4_inverse(&zero);
        assert!(inv.iter().flatten().all(|v| v.is_finite()));
    }

    #[test]
    fn slerp_halfway_between_identity_and_180_degrees_is_90() {
        let q = slerp([0.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 0.0], 0.5);
        let s = std::f32::consts::FRAC_1_SQRT_2;
        assert!(close(q[0], 0.0) && close(q[1], s) && close(q[2], 0.0) && close(q[3], s), "{q:?}");

        // Quaternions on the far side of the hypersphere take the short arc.
        let q2 = slerp([0.0, 0.0, 0.0, 1.0], [0.0, -1.0, 0.0, 0.0], 0.5);
        assert!((q2[1].abs() - s).abs() < 1e-5, "{q2:?}");
    }

    #[test]
    fn quat_matrix_is_orthonormal() {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let m = quat_mat4([s, s, 0.0, 0.0]); // 90° about the (1,1,0)/√2 axis
        let x = transform_dir(&m, [1.0, 0.0, 0.0]);
        let y = transform_dir(&m, [0.0, 1.0, 0.0]);
        let z = transform_dir(&m, [0.0, 0.0, 1.0]);
        let dot = |a: Vec3, b: Vec3| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        assert!(close(dot(x, y), 0.0) && close(dot(y, z), 0.0) && close(dot(x, z), 0.0));
        assert!(close(dot(x, x), 1.0) && close(dot(y, y), 1.0) && close(dot(z, z), 1.0));
    }
}
