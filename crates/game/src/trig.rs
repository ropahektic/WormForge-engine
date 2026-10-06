//! Trigonometry that gives the same bits on every machine, for code whose
//! result reaches the simulation. A C runtime's `sin`/`cos`/`atan2` need
//! not round exactly, and differ in the last bit between Windows and Wine,
//! and between CPUs (the UCRT takes another path with FMA3): in lockstep, one
//! such bit that survives into a position is a desync.
//!
//! `sin`/`cos` are WA's own: its sine table (0x6A1860, 16.16, 1024 steps a
//! turn) read as its aim code reads it (0x51CA90): an angle of 0x10000 a
//! turn, its top 10 bits the step, its low 6 the linear interpolation to the
//! next. Cosine is the same table a quarter turn on (0x6A1C60). The table is
//! a sine wave's: a quarter of it (WA's values, 0 to a quarter turn) gives
//! the rest by symmetry.

const QUARTER: [i32; 257] = [
    0, 402, 804, 1206, 1608, 2010, 2412, 2814, 3215, 3617, 4018, 4420,
    4821, 5222, 5622, 6023, 6423, 6823, 7223, 7623, 8022, 8421, 8819, 9218,
    9616, 10013, 10410, 10807, 11204, 11600, 11995, 12390, 12785, 13179, 13573, 13966,
    14359, 14751, 15142, 15533, 15923, 16313, 16702, 17091, 17479, 17866, 18253, 18638,
    19024, 19408, 19792, 20175, 20557, 20938, 21319, 21699, 22078, 22456, 22833, 23210,
    23586, 23960, 24334, 24707, 25079, 25450, 25820, 26189, 26557, 26925, 27291, 27656,
    28020, 28383, 28745, 29105, 29465, 29824, 30181, 30538, 30893, 31247, 31600, 31952,
    32302, 32651, 32999, 33346, 33692, 34036, 34379, 34721, 35061, 35400, 35738, 36074,
    36409, 36743, 37075, 37406, 37736, 38064, 38390, 38716, 39039, 39362, 39682, 40002,
    40319, 40636, 40950, 41263, 41575, 41885, 42194, 42501, 42806, 43110, 43412, 43712,
    44011, 44308, 44603, 44897, 45189, 45480, 45768, 46055, 46340, 46624, 46906, 47186,
    47464, 47740, 48015, 48288, 48558, 48828, 49095, 49360, 49624, 49886, 50146, 50403,
    50660, 50914, 51166, 51416, 51665, 51911, 52155, 52398, 52639, 52877, 53114, 53348,
    53581, 53811, 54040, 54266, 54491, 54713, 54933, 55152, 55368, 55582, 55794, 56004,
    56212, 56417, 56621, 56822, 57022, 57219, 57414, 57606, 57797, 57986, 58172, 58356,
    58538, 58718, 58895, 59070, 59243, 59414, 59583, 59749, 59913, 60075, 60235, 60392,
    60547, 60700, 60850, 60998, 61144, 61288, 61429, 61568, 61705, 61839, 61971, 62100,
    62228, 62353, 62475, 62596, 62714, 62829, 62942, 63053, 63162, 63268, 63371, 63473,
    63571, 63668, 63762, 63854, 63943, 64030, 64115, 64197, 64276, 64353, 64428, 64501,
    64571, 64638, 64703, 64766, 64826, 64884, 64939, 64992, 65043, 65091, 65136, 65179,
    65220, 65258, 65294, 65327, 65358, 65386, 65412, 65436, 65457, 65475, 65491, 65505,
    65516, 65524, 65531, 65534, 65536,
];

/// WA's table at step `i`, 0 to 1024 (a whole turn).
fn step(i: usize) -> i32 {
    match i {
        0..=256 => QUARTER[i],
        257..=512 => QUARTER[512 - i],
        513..=768 => -QUARTER[i - 512],
        _ => -QUARTER[1024 - i],
    }
}

/// WA's sine of `angle` (0x10000 a turn, any value: it wraps), 16.16.
pub fn sin(angle: i32) -> i32 {
    let a = angle as u32;
    let i = ((a >> 6) & 0x3FF) as usize;
    let frac = ((a & 0x3F) << 10) as i64;
    let (lo, hi) = (step(i), step(i + 1));
    lo + (((hi - lo) as i64 * frac) >> 16) as i32
}

/// WA's cosine of `angle` (0x10000 a turn), 16.16.
pub fn cos(angle: i32) -> i32 {
    sin(angle.wrapping_add(0x4000))
}

/// Radians as WA's angle (0x10000 a turn), rounded to the nearest: one
/// multiply, which IEEE rounds exactly, so every machine agrees.
pub fn angle_from_radians(rad: f64) -> i32 {
    ((rad * (65536.0 / std::f64::consts::TAU)).round() as i64 & 0xFFFF) as i32
}

/// atan2(y, x) in radians, -pi to pi, from + - * / alone, each of which IEEE
/// rounds exactly. The ratio of the smaller to the larger side is moved into
/// |z| <= tan(pi/12) (atan t = pi/6 + atan((t*sqrt3 - 1) / (t + sqrt3))),
/// where the series to z^29 is good to well below a double's last bit.
pub fn atan2(y: f64, x: f64) -> f64 {
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_6, PI};
    const TAN_PI_12: f64 = 0.267_949_192_431_122_7;
    const SQRT_3: f64 = 1.732_050_807_568_877_2;
    if y == 0.0 && x == 0.0 {
        return 0.0;
    }
    let (ax, ay) = (x.abs(), y.abs());
    let steep = ay > ax;
    let t = if steep { ax / ay } else { ay / ax };
    let (z, base) = if t > TAN_PI_12 {
        ((t * SQRT_3 - 1.0) / (t + SQRT_3), FRAC_PI_6)
    } else {
        (t, 0.0)
    };
    let z2 = z * z;
    let mut s = 0.0;
    for k in (0..15).rev() {
        s = 1.0 / (2 * k + 1) as f64 - z2 * s;
    }
    let mut r = base + z * s;
    if steep {
        r = FRAC_PI_2 - r;
    }
    if x < 0.0 {
        r = PI - r;
    }
    if y < 0.0 {
        r = -r;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_and_cos_are_was_table() {
        // Values read from WA 3.8.1's table at 0x6A1860.
        for (i, v) in [(0, 0), (1, 402), (64, 25079), (128, 46340), (255, 65534), (256, 65536),
            (257, 65534), (512, 0), (768, -65536), (1024, 0)]
        {
            assert_eq!(sin(i << 6), v, "step {i}");
        }
        assert_eq!(sin(32), 201, "half way to the next step");
        assert_eq!(sin(0x10000 + 0x4000), 0x10000, "a whole turn wraps");
        assert_eq!(sin(-0x4000), -0x10000);
        for a in (0..0x10000).step_by(97) {
            assert_eq!(cos(a), sin(a + 0x4000));
            // WA's interpolation rounds down, so between steps it is odd and
            // symmetric only to within one; on steps exactly.
            assert!((sin(-a) + sin(a)).abs() <= 1, "odd");
            let on_step = a & !0x3F;
            assert_eq!(sin(-on_step), -sin(on_step));
            assert_eq!(sin(0x8000 - on_step), sin(on_step), "symmetric about a quarter turn");
            let exact = (a as f64 / 65536.0 * std::f64::consts::TAU).sin() * 65536.0;
            assert!((sin(a) as f64 - exact).abs() < 4.0, "{a}: {} vs {exact}", sin(a));
        }
    }

    #[test]
    fn radians_round_to_was_angle() {
        use std::f64::consts::{FRAC_PI_2, PI};
        assert_eq!(angle_from_radians(0.0), 0);
        assert_eq!(angle_from_radians(FRAC_PI_2), 0x4000);
        assert_eq!(angle_from_radians(PI), 0x8000);
        assert_eq!(angle_from_radians(-FRAC_PI_2), 0xC000);
        assert_eq!(angle_from_radians(5.0 * PI), 0x8000, "wraps");
    }

    #[test]
    fn atan2_matches_the_c_runtime_closely() {
        use std::f64::consts::{FRAC_PI_2, PI};
        assert_eq!(atan2(0.0, 0.0), 0.0);
        assert_eq!(atan2(0.0, 1.0), 0.0);
        assert_eq!(atan2(1.0, 0.0), FRAC_PI_2);
        assert_eq!(atan2(0.0, -1.0), PI);
        assert_eq!(atan2(-1.0, 0.0), -FRAC_PI_2);
        for i in -40..=40 {
            for j in -40..=40 {
                let (y, x) = (i as f64 * 0.37, j as f64 * 1.13);
                if i == 0 && j == 0 {
                    continue;
                }
                assert!((atan2(y, x) - y.atan2(x)).abs() < 1e-15, "atan2({y}, {x})");
            }
        }
    }
}
