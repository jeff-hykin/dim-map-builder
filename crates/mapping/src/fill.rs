//! Fill floor: close a hole in a storey's floor. The floor's top surface around the brushed area (one sample per voxel
//! column within `MARGIN` of the stroke: its highest up-facing voxel within `SEARCH` of the floor model's estimate) gets
//! a robust plane fit (RANSAC, then least squares on the inliers), so a tilted floor or a ramp is followed and a box
//! top, a cable or a wall's foot doesn't pull it. Then every brushed column with no floor near that plane and nothing
//! standing on it gets the surrounding floor's own thickness of voxels, on the plane.
use crate::edit::{center, Edit, Stroke};
use crate::voxels::{key_of, Key};
use ahash::{AHashMap, AHashSet};
use anyhow::{bail, Result};

/// how far around the stroke the floor is sampled, m
const MARGIN: f32 = 1.5;
/// how far from the floor model's estimate a column's top surface may be to count as a floor sample, m
const SEARCH: f32 = 0.3;
/// steeper than this isn't a floor (a ramp is ~5°)
const MAX_TILT_DEGREES: f32 = 15.0;
/// a column with this much (2+ voxels) standing within this of the plane is under a wall or furniture: not filled, m
const CLEARANCE: f32 = 0.3;
/// floor samples (and plane inliers) needed for a fit
const MIN_SAMPLES: usize = 30;

/// A fill and the plane it was made on.
#[derive(Debug, Clone, Default)]
pub struct FillFloor {
    pub edit: Edit,
    /// filled columns' area, m²
    pub area: f32,
    /// the fitted plane's tilt from level, degrees
    pub tilt_degrees: f32,
    /// the plane's z under the stroke's middle point
    pub height: f32,
    /// the floor's thickness the fill copies, m
    pub thickness: f32,
}

/// z = a x + b y + c
#[derive(Debug, Clone, Copy)]
struct Plane {
    a: f32,
    b: f32,
    c: f32,
}

impl Plane {
    fn z(&self, x: f32, y: f32) -> f32 {
        self.a * x + self.b * y + self.c
    }

    fn through(p: [f32; 3], q: [f32; 3], r: [f32; 3]) -> Option<Plane> {
        let (u, v) = ([q[0] - p[0], q[1] - p[1], q[2] - p[2]], [r[0] - p[0], r[1] - p[1], r[2] - p[2]]);
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if length < 1e-9 || n[2].abs() / length < MAX_TILT_DEGREES.to_radians().cos() {
            return None;
        }
        let (a, b) = (-n[0] / n[2], -n[1] / n[2]);
        Some(Plane { a, b, c: p[2] - a * p[0] - b * p[1] })
    }

    /// least squares over `samples` (about their centroid, for conditioning)
    fn fit(samples: &[[f32; 3]]) -> Option<Plane> {
        let count = samples.len() as f64;
        let mean = samples.iter().fold([0.0f64; 3], |m, p| [m[0] + p[0] as f64 / count, m[1] + p[1] as f64 / count, m[2] + p[2] as f64 / count]);
        let (mut xx, mut xy, mut yy, mut xz, mut yz) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for p in samples {
            let (x, y, z) = (p[0] as f64 - mean[0], p[1] as f64 - mean[1], p[2] as f64 - mean[2]);
            xx += x * x;
            xy += x * y;
            yy += y * y;
            xz += x * z;
            yz += y * z;
        }
        let determinant = xx * yy - xy * xy;
        if determinant.abs() < 1e-12 {
            return None;
        }
        let a = (xz * yy - yz * xy) / determinant;
        let b = (yz * xx - xz * xy) / determinant;
        Some(Plane { a: a as f32, b: b as f32, c: (mean[2] - a * mean[0] - b * mean[1]) as f32 })
    }

    fn tilt_degrees(&self) -> f32 {
        self.a.hypot(self.b).atan().to_degrees()
    }
}

/// RANSAC (deterministic: a fixed-seed generator) for the plane with the most samples within `threshold`, refined by
/// least squares on its inliers (twice, re-picking them). None when fewer than `MIN_SAMPLES` agree.
fn robust_plane(samples: &[[f32; 3]], threshold: f32) -> Option<(Plane, Vec<[f32; 3]>)> {
    let inliers = |plane: &Plane| -> Vec<[f32; 3]> { samples.iter().filter(|p| (p[2] - plane.z(p[0], p[1])).abs() <= threshold).copied().collect() };
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |n: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n
    };
    let mut best: Option<(Plane, usize)> = None;
    for _ in 0..300 {
        let (i, j, k) = (next(samples.len()), next(samples.len()), next(samples.len()));
        let Some(plane) = Plane::through(samples[i], samples[j], samples[k]) else { continue };
        let count = samples.iter().filter(|p| (p[2] - plane.z(p[0], p[1])).abs() <= threshold).count();
        if best.is_none_or(|(_, most)| count > most) {
            best = Some((plane, count));
        }
    }
    let (mut plane, _) = best?;
    for _ in 0..2 {
        let chosen = inliers(&plane);
        plane = Plane::fit(&chosen).filter(|p| p.tilt_degrees() <= MAX_TILT_DEGREES).unwrap_or(plane);
    }
    let chosen = inliers(&plane);
    (chosen.len() >= MIN_SAMPLES).then_some((plane, chosen))
}

/// Fill the floor's holes under `stroke` on the storey whose floor `floor_at` estimates (searched within `band`).
pub fn fill_floor(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, stroke: &Stroke, floor_at: impl Fn(f32, f32) -> Option<f32>, band: [f32; 2]) -> Result<FillFloor> {
    if stroke.path.is_empty() {
        bail!("fill_floor needs a path");
    }
    let mut columns: AHashSet<(i32, i32)> = stroke.columns(voxel).into_iter().collect();
    columns.extend(stroke.path.iter().map(|p| ((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32)));
    let around = Stroke { path: stroke.path.clone(), radius: stroke.radius + MARGIN };
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in &around.path {
        x0 = x0.min(p[0] - around.radius);
        y0 = y0.min(p[1] - around.radius);
        x1 = x1.max(p[0] + around.radius);
        y1 = y1.max(p[1] + around.radius);
    }
    let column_center = |c: (i32, i32)| ((c.0 as f32 + 0.5) * voxel, (c.1 as f32 + 0.5) * voxel);
    // every voxel near the stroke in the storey (to find a floor's thickness and what stands on it), and per column
    // its top floor surface
    let mut occupied: AHashSet<Key> = AHashSet::new();
    let mut tops: AHashMap<(i32, i32), i32> = AHashMap::new();
    for (p, n) in points.iter().zip(normals) {
        if p[0] < x0 || p[0] > x1 || p[1] < y0 || p[1] > y1 || p[2] < band[0] - SEARCH || p[2] > band[1] {
            continue;
        }
        let key = key_of(*p, voxel);
        occupied.insert(key);
        if n[2] < 0.8 {
            continue;
        }
        let (x, y) = column_center((key.0, key.1));
        if !around.contains(x, y) || floor_at(x, y).is_none_or(|estimate| (p[2] - estimate).abs() > SEARCH) {
            continue;
        }
        let top = tops.entry((key.0, key.1)).or_insert(key.2);
        *top = (*top).max(key.2);
    }
    let samples: Vec<[f32; 3]> = tops
        .iter()
        .map(|(column, layer)| {
            let (x, y) = column_center(*column);
            [x, y, (*layer as f32 + 0.5) * voxel]
        })
        .collect();
    let no_floor = || anyhow::anyhow!("no floor around the stroke to fit a plane to ({} floor columns within {MARGIN} m; at least {MIN_SAMPLES} needed): brush over a hole that has floor around it", samples.len());
    if samples.len() < MIN_SAMPLES {
        return Err(no_floor());
    }
    let (plane, inliers) = robust_plane(&samples, voxel * 0.6).ok_or_else(no_floor)?;
    // the floor's thickness: the median run of occupied voxels down from an inlier's top
    let mut runs: Vec<i32> = inliers
        .iter()
        .map(|p| {
            let top = key_of(*p, voxel);
            (0..10).take_while(|d| occupied.contains(&(top.0, top.1, top.2 - d))).count() as i32
        })
        .collect();
    runs.sort_unstable();
    let thickness = runs[runs.len() / 2].max(1);
    let up = {
        let length = (plane.a * plane.a + plane.b * plane.b + 1.0).sqrt();
        [-plane.a / length, -plane.b / length, 1.0 / length]
    };
    let standing_layers = (CLEARANCE / voxel).ceil() as i32;
    let mut edit = Edit::default();
    let mut filled = 0;
    let mut sorted: Vec<(i32, i32)> = columns.into_iter().collect();
    sorted.sort_unstable();
    for column in sorted {
        let (x, y) = column_center(column);
        let z = plane.z(x, y);
        if z < band[0] || z > band[1] {
            continue;
        }
        let top = (z / voxel).floor() as i32;
        // floor already there (a voxel of it within one layer), or something standing on the plane: left alone
        let has_floor = (top - thickness - 1..=top + 1).any(|layer| occupied.contains(&(column.0, column.1, layer)));
        let standing = (top + 2..=top + standing_layers).filter(|layer| occupied.contains(&(column.0, column.1, *layer))).count() >= 2;
        if has_floor || standing {
            continue;
        }
        for layer in top - thickness + 1..=top {
            edit.push(center((column.0, column.1, layer), voxel), up);
        }
        filled += 1;
    }
    let middle = stroke.path[stroke.path.len() / 2];
    Ok(FillFloor { edit, area: filled as f32 * voxel * voxel, tilt_degrees: plane.tilt_degrees(), height: plane.z(middle[0], middle[1]), thickness: thickness as f32 * voxel })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floor::tests::{at, snap};

    const VOXEL: f32 = 0.05;
    const BAND: [f32; 2] = [-0.6, 1.5];

    /// a 4 x 4 m floor `layers` voxels thick whose top follows `floor`, with a hole (x, y in 1.6..2.4 m) where no floor
    /// was seen
    fn holed_floor(floor: impl Fn(f32, f32) -> f32, layers: i32) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, AHashSet<(i32, i32)>) {
        let (mut points, mut normals, mut hole) = (Vec::new(), Vec::new(), AHashSet::new());
        for i in 0..80 {
            for j in 0..80 {
                if (32..48).contains(&i) && (32..48).contains(&j) {
                    hole.insert((i, j));
                    continue;
                }
                let top = (floor(at(i), at(j)) / VOXEL).floor() as i32;
                for k in 0..layers {
                    points.push([at(i), at(j), at(top - k)]);
                    normals.push(if k == 0 { [0.0, 0.0, 1.0] } else { [0.0, 0.0, -1.0] });
                }
            }
        }
        (points, normals, hole)
    }

    fn keys(edit: &Edit) -> AHashSet<Key> {
        edit.add.iter().map(|p| key_of(*p, VOXEL)).collect()
    }

    #[test]
    fn fills_a_flat_hole_exactly() {
        let (points, normals, hole) = holed_floor(|_, _| at(0), 2);
        // the brush covers the hole and floor around it: only the hole is filled, both layers, no duplicates
        let fill = fill_floor(&points, &normals, VOXEL, &Stroke { path: vec![[1.8, 2.0], [2.2, 2.0]], radius: 0.6 }, |_, _| Some(at(0)), BAND).unwrap();
        let want: AHashSet<Key> = hole.iter().flat_map(|c| [(c.0, c.1, 0), (c.0, c.1, -1)]).collect();
        assert_eq!(fill.edit.add.len(), want.len(), "no duplicates");
        assert_eq!(keys(&fill.edit), want);
        assert!((fill.area - 16.0 * 16.0 * VOXEL * VOXEL).abs() < 1e-4, "{}", fill.area);
        assert!(fill.tilt_degrees < 0.1 && (fill.height - at(0)).abs() < 0.01 && (fill.thickness - 0.1).abs() < 1e-4, "{fill:?}");
        assert!(fill.edit.add_normals.iter().all(|n| n[2] > 0.999));
        // a second stroke over it (the hole now filled) adds nothing
        let mut after_points = points.clone();
        after_points.extend(&fill.edit.add);
        let mut after_normals = normals.clone();
        after_normals.extend(&fill.edit.add_normals);
        let again = fill_floor(&after_points, &after_normals, VOXEL, &Stroke { path: vec![[2.0, 2.0]], radius: 0.8 }, |_, _| Some(at(0)), BAND).unwrap();
        assert!(again.edit.add.is_empty(), "{}", again.edit.add.len());
    }

    #[test]
    fn fills_a_tilted_floor_on_the_tilt() {
        let slope = 2f32.to_radians().tan();
        let floor = move |x: f32, y: f32| 0.1 + slope * x + 0.3 * slope * y;
        let (points, normals, hole) = holed_floor(floor, 1);
        // the floor model blurs a tilt: a flat estimate is enough to pick the samples
        let fill = fill_floor(&points, &normals, VOXEL, &Stroke { path: vec![[2.0, 2.0]], radius: 0.6 }, |_, _| Some(floor(2.0, 2.0)), BAND).unwrap();
        assert!((fill.tilt_degrees - (slope.hypot(0.3 * slope)).atan().to_degrees()).abs() < 0.2, "{}", fill.tilt_degrees);
        assert_eq!(fill.edit.add.len(), hole.len());
        for p in &fill.edit.add {
            let column = ((p[0] / VOXEL).floor() as i32, (p[1] / VOXEL).floor() as i32);
            assert!(hole.contains(&column));
            assert!((p[2] - floor(p[0], p[1])).abs() <= VOXEL * 0.5 + 0.005, "{p:?} vs the floor's {}", floor(p[0], p[1]));
        }
        // a snapped hole voxel sits where the floor's own snapped voxel would be (mostly: a fit, not the formula)
        let exact = fill.edit.add.iter().filter(|p| (p[2] - snap(floor(p[0], p[1]))).abs() < 1e-4).count();
        assert!(exact as f32 >= 0.9 * hole.len() as f32, "{exact} of {}", hole.len());
    }

    #[test]
    fn furniture_and_walls_are_not_fit_or_filled() {
        let (mut points, mut normals, hole) = holed_floor(|_, _| at(0), 1);
        let mut solid: AHashSet<(i32, i32)> = AHashSet::new();
        // a wall along x = 2.3 m (crossing the hole's edge and the brush), 2 m tall, 
        for j in 0..80 {
            solid.insert((46, j));
            for k in 1..41 {
                points.push([at(46), at(j), at(k)]);
                normals.push([1.0, 0.0, 0.0]);
            }
        }
        // a 20 cm tall box beside the hole (its top a flat up-facing surface inside the samples' search band),
        // partly under the brush
        for i in 20..34 {
            for j in 36..60 {
                solid.insert((i, j));
                for k in 1..=4 {
                    points.push([at(i), at(j), at(k)]);
                    normals.push([0.0, 0.0, if k == 4 { 1.0 } else { 0.0 }]);
                }
            }
        }
        // no floor seen under the wall or the box
        let seen: Vec<bool> = points.iter().map(|p| !((p[2] - at(0)).abs() < 1e-4 && solid.contains(&((p[0] / VOXEL).floor() as i32, (p[1] / VOXEL).floor() as i32)))).collect();
        let points: Vec<[f32; 3]> = points.iter().zip(&seen).filter(|(_, s)| **s).map(|(p, _)| *p).collect();
        let normals: Vec<[f32; 3]> = normals.iter().zip(&seen).filter(|(_, s)| **s).map(|(n, _)| *n).collect();
        let fill = fill_floor(&points, &normals, VOXEL, &Stroke { path: vec![[1.6, 2.0], [2.3, 2.0]], radius: 0.6 }, |_, _| Some(at(0)), BAND).unwrap();
        assert!(fill.tilt_degrees < 0.1 && (fill.height - at(0)).abs() < 0.01, "the box top didn't pull the fit: {fill:?}");
        let added = keys(&fill.edit);
        assert!(added.iter().all(|k| k.2 == 0), "all on the floor");
        assert!(added.iter().all(|k| !solid.contains(&(k.0, k.1))), "nothing under the wall or the box");
        assert!(added.iter().all(|k| hole.contains(&(k.0, k.1))), "only the hole");
        let open_hole = hole.iter().filter(|c| !solid.contains(c)).count();
        assert_eq!(added.len(), open_hole, "the rest of the hole is filled");
    }

    #[test]
    fn no_floor_around_is_an_error() {
        // a wall in the open, and a scrap of floor too small to fit
        let (mut points, mut normals) = (Vec::new(), Vec::new());
        for j in 0..40 {
            for k in 0..40 {
                points.push([at(20), at(j), at(k)]);
                normals.push([1.0, 0.0, 0.0]);
            }
        }
        for i in 0..4 {
            for j in 0..4 {
                points.push([at(i), at(j), at(0)]);
                normals.push([0.0, 0.0, 1.0]);
            }
        }
        let error = fill_floor(&points, &normals, VOXEL, &Stroke { path: vec![[1.0, 1.0]], radius: 0.5 }, |_, _| Some(at(0)), BAND).unwrap_err();
        assert!(error.to_string().contains("no floor around the stroke"), "{error}");
    }
}
