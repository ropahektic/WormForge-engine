//! `wa.world.trace` and `wa.world.collides`: a circle against WA's terrain
//! and objects, what Project X's `TraceMaskEx` and `CheckCollisionEx` give
//! its rolling and walking weapons (the Bowling Ball, the Ferret).
//!
//! Terrain is WA's own collision bitmap, the one its physics walks on.
//! Objects (worms alive, mines, crates, oil drums, graves) are circles of the
//! sizes the electrical weapons use. A sweep steps at most a pixel at a time
//! in 16.16 integers; the normal at a contact is the sum of the offsets from
//! the solid pixels around it to the circle's centre (or from the object's
//! centre), normalised: integer sums and one square root, the same on every
//! machine.

use game::entity::read_i32;
use game::offsets::{CLASS_CRATE, CLASS_GRAVE, CLASS_MINE, CLASS_OIL, CLASS_WORM, ENT_CLASS_TYPE, ENT_POS_X, ENT_POS_Y};
use lua_host::{ShapeHit, ShapeQuery, SHAPE_CRATES, SHAPE_GRAVES, SHAPE_MINES, SHAPE_OIL, SHAPE_TERRAIN, SHAPE_WORMS};

const ONE: i64 = 0x1_0000;
/// The normal looks this many pixels beyond the circle for the surface.
const NORMAL_REACH: i64 = 2;

/// An object as a circle: handle, centre (16.16), radius in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Body {
    pub handle: u32,
    pub x: i32,
    pub y: i32,
    pub r: i32,
}

/// What a circle at a place overlaps: None nothing, Some(0) terrain,
/// Some(handle) an object (terrain first).
fn overlap(x: i32, y: i32, r: i32, terrain: Option<&dyn Fn(i32, i32) -> bool>, bodies: &[Body]) -> Option<u32> {
    let (x, y, r) = (x as i64, y as i64, r as i64);
    if let Some(solid) = terrain {
        // The pixel the centre is in always counts: radius 0 is a point.
        if solid((x >> 16) as i32, (y >> 16) as i32) {
            return Some(0);
        }
        let rr = (r * ONE) * (r * ONE);
        let (px0, px1) = ((x >> 16) - r - 1, (x >> 16) + r + 1);
        let (py0, py1) = ((y >> 16) - r - 1, (y >> 16) + r + 1);
        for py in py0..=py1 {
            for px in px0..=px1 {
                let (dx, dy) = (px * ONE + ONE / 2 - x, py * ONE + ONE / 2 - y);
                if dx * dx + dy * dy <= rr && solid(px as i32, py as i32) {
                    return Some(0);
                }
            }
        }
    }
    bodies
        .iter()
        .find(|b| {
            let (dx, dy) = (b.x as i64 - x, b.y as i64 - y);
            let reach = (r + b.r as i64) * ONE;
            dx * dx + dy * dy < reach * reach
        })
        .map(|b| b.handle)
}

/// The unit normal at a contact of a circle at (x, y): away from `touched`
/// (0 terrain, else that object), or against `dx, dy` if nothing tells.
fn normal_at(
    x: i32, y: i32, r: i32, touched: u32, terrain: Option<&dyn Fn(i32, i32) -> bool>, bodies: &[Body],
    dx: i64, dy: i64,
) -> (f64, f64) {
    let (x, y) = (x as i64, y as i64);
    let (mut sx, mut sy) = (0i64, 0i64);
    if touched != 0 {
        if let Some(b) = bodies.iter().find(|b| b.handle == touched) {
            sx = x - b.x as i64;
            sy = y - b.y as i64;
        }
    } else if let Some(solid) = terrain {
        let reach = r as i64 + NORMAL_REACH;
        let rr = (reach * ONE) * (reach * ONE);
        for py in (y >> 16) - reach - 1..=(y >> 16) + reach + 1 {
            for px in (x >> 16) - reach - 1..=(x >> 16) + reach + 1 {
                let (ox, oy) = (x - (px * ONE + ONE / 2), y - (py * ONE + ONE / 2));
                if ox * ox + oy * oy <= rr && solid(px as i32, py as i32) {
                    sx += ox >> 8;
                    sy += oy >> 8;
                }
            }
        }
    }
    if sx == 0 && sy == 0 {
        (sx, sy) = (-dx, -dy);
    }
    let (fx, fy) = (sx as f64, sy as f64);
    let len = (fx * fx + fy * fy).sqrt();
    if len == 0.0 {
        (0.0, -1.0)
    } else {
        (fx / len, fy / len)
    }
}

/// The circle swept from (x0, y0) to (x1, y1), a pixel at a time at most.
pub(crate) fn sweep(q: &ShapeQuery, terrain: Option<&dyn Fn(i32, i32) -> bool>, bodies: &[Body]) -> ShapeHit {
    let (x0, y0) = (q.x0 as i64, q.y0 as i64);
    let (dx, dy) = (q.x1 as i64 - x0, q.y1 as i64 - y0);
    let n = (dx.abs().max(dy.abs()) / ONE + 1).clamp(1, 20_000);
    let at = |i: i64| ((x0 + dx * i / n) as i32, (y0 + dy * i / n) as i32);
    let mut out = ShapeHit::default();
    let mut from = 0;
    let (sx, sy) = at(0);
    if overlap(sx, sy, q.radius, terrain, bodies).is_some() {
        out.starts_solid = true;
        match (1..=n).find(|&i| {
            let (x, y) = at(i);
            overlap(x, y, q.radius, terrain, bodies).is_none()
        }) {
            Some(i) => {
                out.left = Some(at(i));
                from = i;
            }
            None => return out,
        }
    }
    for i in from + 1..=n {
        let (x, y) = at(i);
        if let Some(touched) = overlap(x, y, q.radius, terrain, bodies) {
            // The last free centre, to 1/256 of the step: halve the step
            // between the free position and the touching one.
            let (mut lo, mut hi) = (0i64, 256i64);
            let at_part = |k: i64| {
                let t = (i - 1) * 256 + k;
                ((x0 + dx * t / (n * 256)) as i32, (y0 + dy * t / (n * 256)) as i32)
            };
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                let (mx, my) = at_part(mid);
                if overlap(mx, my, q.radius, terrain, bodies).is_some() {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let (px, py) = at_part(lo);
            let (nx, ny) = normal_at(x, y, q.radius, touched, terrain, bodies, dx, dy);
            out.hit = true;
            out.x = px;
            out.y = py;
            out.nx = nx;
            out.ny = ny;
            out.obj = touched;
            return out;
        }
    }
    out
}

/// The objects a query names, as circles, without the ones it ignores.
fn bodies(q: &ShapeQuery) -> Vec<Body> {
    let wanted = |class: i32| match class {
        CLASS_WORM => q.with & SHAPE_WORMS != 0,
        CLASS_MINE => q.with & SHAPE_MINES != 0,
        CLASS_CRATE => q.with & SHAPE_CRATES != 0,
        CLASS_OIL => q.with & SHAPE_OIL != 0,
        CLASS_GRAVE => q.with & SHAPE_GRAVES != 0,
        _ => false,
    };
    if q.with & (SHAPE_WORMS | SHAPE_MINES | SHAPE_CRATES | SHAPE_OIL | SHAPE_GRAVES) == 0 {
        return Vec::new();
    }
    crate::world::electrical_targets(q.with & SHAPE_GRAVES != 0)
        .into_iter()
        .filter(|h| !q.ignore.contains(h))
        .filter_map(|h| unsafe {
            let e = h as *mut u8;
            let class = read_i32(e, ENT_CLASS_TYPE);
            wanted(class).then(|| Body {
                handle: h,
                x: read_i32(e, ENT_POS_X),
                y: read_i32(e, ENT_POS_Y),
                r: match class {
                    CLASS_WORM => 6,
                    CLASS_GRAVE => 8,
                    CLASS_MINE => 5,
                    _ => 9,
                },
            })
        })
        .collect()
}

pub fn trace(q: &ShapeQuery) -> ShapeHit {
    let grid = (q.with & SHAPE_TERRAIN != 0).then(crate::world::live_grid).flatten();
    let solid = grid.as_ref().map(|g| move |x: i32, y: i32| g.solid_px(x, y));
    let terrain = solid.as_ref().map(|f| f as &dyn Fn(i32, i32) -> bool);
    sweep(q, terrain, &bodies(q))
}

pub fn collides(q: &ShapeQuery) -> Option<u32> {
    let grid = (q.with & SHAPE_TERRAIN != 0).then(crate::world::live_grid).flatten();
    let solid = grid.as_ref().map(|g| move |x: i32, y: i32| g.solid_px(x, y));
    let terrain = solid.as_ref().map(|f| f as &dyn Fn(i32, i32) -> bool);
    overlap(q.x0, q.y0, q.radius, terrain, &bodies(q))
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: i32 = 0x1_0000;

    fn q(x0: i32, y0: i32, x1: i32, y1: i32, r: i32) -> ShapeQuery {
        ShapeQuery { x0: x0 * P, y0: y0 * P, x1: x1 * P, y1: y1 * P, radius: r, with: SHAPE_TERRAIN, ignore: vec![] }
    }

    #[test]
    fn a_ball_dropped_on_a_floor_stops_above_it_with_the_normal_up() {
        let floor = |_: i32, y: i32| y >= 100;
        let hit = sweep(&q(50, 50, 50, 150, 8), Some(&floor), &[]);
        assert!(hit.hit && !hit.starts_solid && hit.obj == 0);
        // The last free centre, to a fraction of a pixel. The floor's nearest
        // pixel centre is (50.5, 100.5), half a pixel aside, so the circle
        // touches it at a centre of 100.5 - sqrt(64 - 0.25).
        let y = hit.y as f64 / P as f64;
        let touch = 100.5 - (64.0f64 - 0.25).sqrt();
        assert!(y <= touch && y > touch - 1.0 / 64.0, "{y} vs {touch}");
        assert!(hit.nx.abs() < 1e-9 && (hit.ny + 1.0).abs() < 1e-9, "{} {}", hit.nx, hit.ny);
    }

    #[test]
    fn a_slope_gives_its_normal_not_a_45_degree_snap() {
        // Solid below the line y = 100 + x / 2: normal (1, -2) / sqrt(5).
        let slope = |x: i32, y: i32| 2 * y >= 200 + x;
        let hit = sweep(&q(40, 40, 40, 200, 8), Some(&slope), &[]);
        assert!(hit.hit);
        let want = (1.0 / 5f64.sqrt(), -2.0 / 5f64.sqrt());
        assert!((hit.nx - want.0).abs() < 0.08 && (hit.ny - want.1).abs() < 0.08, "{} {}", hit.nx, hit.ny);
    }

    #[test]
    fn a_wall_gives_a_sideways_normal() {
        let wall = |x: i32, _: i32| x >= 100;
        let hit = sweep(&q(50, 50, 150, 50, 8), Some(&wall), &[]);
        assert!(hit.hit && (hit.nx + 1.0).abs() < 1e-9 && hit.ny.abs() < 1e-9);
    }

    #[test]
    fn starting_inside_reports_where_it_comes_free() {
        let floor = |_: i32, y: i32| y >= 100;
        let hit = sweep(&q(50, 105, 50, 60, 8), Some(&floor), &[]);
        assert!(hit.starts_solid && !hit.hit);
        let (_, ly) = hit.left.expect("comes free on the way up");
        // Free once the floor's first row (centres at y 100.5) is more than
        // 8 away: at a centre of 92 or above.
        assert_eq!(ly >> 16, 92);
        let stuck = sweep(&q(50, 150, 50, 140, 8), Some(&floor), &[]);
        assert!(stuck.starts_solid && stuck.left.is_none() && !stuck.hit);
    }

    #[test]
    fn an_object_is_hit_with_the_normal_from_its_centre() {
        let worm = Body { handle: 7, x: 100 * P, y: 50 * P, r: 6 };
        let mut query = q(50, 50, 150, 50, 8);
        query.with = SHAPE_WORMS;
        let hit = sweep(&query, None, &[worm]);
        assert!(hit.hit && hit.obj == 7);
        assert!((hit.x >> 16) <= 86 && (hit.nx + 1.0).abs() < 1e-9);
        assert_eq!(overlap(100 * P, 60 * P, 8, None, &[worm]), Some(7));
        assert_eq!(overlap(100 * P, 70 * P, 8, None, &[worm]), None);
    }

    #[test]
    fn a_clear_path_hits_nothing_and_radius_zero_is_a_point() {
        let floor = |_: i32, y: i32| y >= 100;
        assert_eq!(sweep(&q(50, 10, 90, 60, 8), Some(&floor), &[]), ShapeHit::default());
        assert_eq!(overlap(50 * P, 99 * P, 0, Some(&floor), &[]), None);
        assert_eq!(overlap(50 * P, 100 * P, 0, Some(&floor), &[]), Some(0));
    }
}
