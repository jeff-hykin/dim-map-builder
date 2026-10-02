//! The editable global map: voxel centers on a grid of `voxel_size`, and the selections the cleanup tools make on it.
//! Every tool returns a selection (indices into `points`); the server turns a selection into an undoable delete.
use ahash::{AHashMap, AHashSet};
use kiddo::{ImmutableKdTree, SquaredEuclidean};
use nalgebra::{Matrix3, SymmetricEigen, Vector3};
use rayon::prelude::*;
use std::num::NonZero;

pub type Key = (i32, i32, i32);

pub fn key_of(p: [f32; 3], voxel: f32) -> Key {
    ((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32, (p[2] / voxel).floor() as i32)
}

/// An axis-aligned box, optionally yawed about its center (what users and the agent draw).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Box3 {
    pub center: [f32; 3],
    /// full extents along the box's own x, y, z
    pub size: [f32; 3],
    /// radians about +z
    #[serde(default)]
    pub yaw: f32,
}

impl Box3 {
    pub fn from_bounds(min: [f32; 3], max: [f32; 3]) -> Box3 {
        Box3 { center: std::array::from_fn(|i| (min[i] + max[i]) / 2.0), size: std::array::from_fn(|i| (max[i] - min[i]).abs()), yaw: 0.0 }
    }

    pub fn contains(&self, p: [f32; 3]) -> bool {
        let (s, c) = self.yaw.sin_cos();
        let dx = p[0] - self.center[0];
        let dy = p[1] - self.center[1];
        let local = [c * dx + s * dy, -s * dx + c * dy, p[2] - self.center[2]];
        (0..3).all(|i| local[i].abs() <= self.size[i] / 2.0)
    }

    pub fn volume(&self) -> f32 {
        self.size[0] * self.size[1] * self.size[2]
    }
}

/// Indices of the points inside (or outside) `region`.
pub fn select_box(points: &[[f32; 3]], region: &Box3, inside: bool) -> Vec<u32> {
    (0..points.len() as u32).into_par_iter().filter(|i| region.contains(points[*i as usize]) == inside).collect()
}

/// Points outside `z_min..=z_max` (a height crop).
pub fn select_outside_heights(points: &[[f32; 3]], z_min: f32, z_max: f32) -> Vec<u32> {
    (0..points.len() as u32).filter(|i| !(z_min..=z_max).contains(&points[*i as usize][2])).collect()
}

/// Connected groups (26-neighbour voxels) smaller than `min_voxels`: floating specks, dust, a passer-by's ghost.
/// With `region`, only groups entirely inside it are considered.
pub fn select_floating(points: &[[f32; 3]], voxel: f32, min_voxels: usize, region: Option<&Box3>) -> Vec<u32> {
    let mut by_key: AHashMap<Key, Vec<u32>> = AHashMap::new();
    for (index, p) in points.iter().enumerate() {
        by_key.entry(key_of(*p, voxel)).or_default().push(index as u32);
    }
    let mut seen: AHashSet<Key> = AHashSet::new();
    let mut selected = Vec::new();
    for start in by_key.keys() {
        if seen.contains(start) {
            continue;
        }
        let mut group = vec![*start];
        let mut stack = vec![*start];
        seen.insert(*start);
        while let Some(k) = stack.pop() {
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let n = (k.0 + dx, k.1 + dy, k.2 + dz);
                        if by_key.contains_key(&n) && seen.insert(n) {
                            stack.push(n);
                            group.push(n);
                        }
                    }
                }
            }
            if group.len() >= min_voxels {
                // big enough: finish the flood (so its voxels aren't revisited) but don't keep collecting
                while let Some(k) = stack.pop() {
                    for dx in -1..=1 {
                        for dy in -1..=1 {
                            for dz in -1..=1 {
                                let n = (k.0 + dx, k.1 + dy, k.2 + dz);
                                if by_key.contains_key(&n) && seen.insert(n) {
                                    stack.push(n);
                                }
                            }
                        }
                    }
                }
                break;
            }
        }
        if group.len() < min_voxels {
            let indices: Vec<u32> = group.iter().flat_map(|k| by_key[k].iter().copied()).collect();
            if region.is_none_or(|region| indices.iter().all(|i| region.contains(points[*i as usize]))) {
                selected.extend(indices);
            }
        }
    }
    selected.sort_unstable();
    selected
}

/// Statistical outlier removal (Open3D's): a point whose mean distance to its `k` nearest neighbours is more than
/// `std_ratio` standard deviations above the map's average.
pub fn select_outliers(points: &[[f32; 3]], k: usize, std_ratio: f32, region: Option<&Box3>) -> Vec<u32> {
    if points.len() <= k {
        return Vec::new();
    }
    let tree: ImmutableKdTree<f32, 3> = ImmutableKdTree::new_from_slice(points);
    let means: Vec<f32> = points
        .par_iter()
        .map(|p| {
            let near = tree.nearest_n::<SquaredEuclidean>(p, NonZero::new(k + 1).unwrap());
            let sum: f32 = near.iter().skip(1).map(|n| n.distance.sqrt()).sum();
            sum / k as f32
        })
        .collect();
    let n = means.len() as f64;
    let mean = means.iter().map(|m| *m as f64).sum::<f64>() / n;
    let variance = means.iter().map(|m| (*m as f64 - mean).powi(2)).sum::<f64>() / n;
    let limit = (mean + std_ratio as f64 * variance.sqrt()) as f32;
    (0..points.len() as u32)
        .filter(|i| means[*i as usize] > limit && region.is_none_or(|r| r.contains(points[*i as usize])))
        .collect()
}

/// Each point's surface normal (unit, +z-ish when ambiguous), from a PCA of the voxels within `radius`; zero where
/// there aren't enough neighbours to fit a plane.
pub fn normals(points: &[[f32; 3]], radius: f32) -> Vec<[f32; 3]> {
    let tree: ImmutableKdTree<f32, 3> = ImmutableKdTree::new_from_slice(points);
    points
        .par_iter()
        .map(|p| {
            let near = tree.nearest_n_within::<SquaredEuclidean>(p, radius * radius, NonZero::new(24).unwrap(), true);
            if near.len() < 5 {
                return [0.0; 3];
            }
            let mut mean = Vector3::<f64>::zeros();
            for n in &near {
                let q = points[n.item as usize];
                mean += Vector3::new(q[0] as f64, q[1] as f64, q[2] as f64);
            }
            mean /= near.len() as f64;
            let mut covariance = Matrix3::<f64>::zeros();
            for n in &near {
                let q = points[n.item as usize];
                let d = Vector3::new(q[0] as f64, q[1] as f64, q[2] as f64) - mean;
                covariance += d * d.transpose();
            }
            let eigen = SymmetricEigen::new(covariance);
            let (index, _) = eigen.eigenvalues.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).unwrap();
            let mut normal = eigen.eigenvectors.column(index).into_owned().normalize();
            if normal.z < 0.0 {
                normal = -normal;
            }
            [normal.x as f32, normal.y as f32, normal.z as f32]
        })
        .collect()
}

/// Horizontal surfaces: the z of each big flat layer (floors, also tabletops and ceilings), heaviest first.
/// A histogram of up-facing points' heights in `bin` slices; a peak needs `min_share` of those points.
pub fn horizontal_levels(points: &[[f32; 3]], normals: &[[f32; 3]], bin: f32, min_share: f32) -> Vec<(f32, usize)> {
    let mut histogram: AHashMap<i32, usize> = AHashMap::new();
    let mut total = 0usize;
    for (p, n) in points.iter().zip(normals) {
        if n[2].abs() > 0.9 {
            *histogram.entry((p[2] / bin).floor() as i32).or_default() += 1;
            total += 1;
        }
    }
    let mut bins: Vec<(i32, usize)> = histogram.into_iter().collect();
    bins.sort();
    let mut levels: Vec<(f32, usize)> = Vec::new();
    for (index, (slice, count)) in bins.iter().enumerate() {
        let left = if index > 0 && bins[index - 1].0 == slice - 1 { bins[index - 1].1 } else { 0 };
        let right = bins.get(index + 1).filter(|b| b.0 == slice + 1).map_or(0, |b| b.1);
        if *count >= left && *count > right && *count as f32 >= min_share * total as f32 {
            levels.push(((*slice as f32 + 0.5) * bin, *count));
        }
    }
    levels.sort_by(|a, b| b.1.cmp(&a.1));
    levels
}

/// The walkable floors: horizontal levels that are big, low within their surroundings, and at least `min_gap`
/// apart (a storey); sorted bottom to top.
pub fn floor_levels(points: &[[f32; 3]], normals: &[[f32; 3]], min_gap: f32) -> Vec<f32> {
    let levels = horizontal_levels(points, normals, 0.05, 0.03);
    let mut floors: Vec<f32> = Vec::new();
    let biggest = levels.first().map_or(0, |l| l.1);
    // biggest first: a level joins unless a bigger one is within a storey of it
    for (z, count) in &levels {
        if (*count as f32) < biggest as f32 * 0.15 {
            continue;
        }
        if floors.iter().all(|f| (f - z).abs() >= min_gap) {
            floors.push(*z);
        }
    }
    floors.sort_by(|a, b| a.total_cmp(b));
    // a floor sees most up-facing points above it; a ceiling-only level (nothing above) is dropped unless alone
    if floors.len() > 1 {
        let top = *floors.last().unwrap();
        let above_top = points.iter().zip(normals).filter(|(p, n)| n[2].abs() < 0.5 && p[2] > top + 0.3).count();
        if above_top < 50 {
            floors.pop();
        }
    }
    floors
}

/// The walls' main direction, folded into [-45°, 45°): from the horizontal normals of the vertical surfaces, a 1°
/// histogram of their angle mod 90° (smoothed over ±2°), its peak refined by the mean around it. Turning the map by
/// minus this lines its walls up with x and y.
pub fn dominant_wall_angle(normals: &[[f32; 3]]) -> Option<f32> {
    let mut histogram = [0.0f32; 90];
    let mut total = 0;
    for n in normals {
        let flat = n[0].hypot(n[1]);
        if n[2].abs() < 0.3 && flat > 0.5 {
            let degrees = n[1].atan2(n[0]).to_degrees().rem_euclid(90.0);
            histogram[(degrees as usize).min(89)] += flat;
            total += 1;
        }
    }
    if total < 50 {
        return None;
    }
    let smooth = |bin: i32| -> f32 { (-2..=2).map(|k| histogram[(bin + k).rem_euclid(90) as usize]).sum() };
    let peak = (0..90).max_by(|a, b| smooth(*a).total_cmp(&smooth(*b)))?;
    let (mut sum, mut weight) = (0.0f32, 0.0f32);
    for k in -3..=3 {
        let w = histogram[(peak + k).rem_euclid(90) as usize];
        sum += (peak + k) as f32 * w + 0.5 * w;
        weight += w;
    }
    let degrees = sum / weight.max(1e-6);
    let folded = if degrees >= 45.0 { degrees - 90.0 } else if degrees < -45.0 { degrees + 90.0 } else { degrees };
    Some(folded.to_radians())
}

/// Up-facing points within `thickness` of a floor level.
pub fn select_floor(points: &[[f32; 3]], normals: &[[f32; 3]], floors: &[f32], thickness: f32, region: Option<&Box3>) -> Vec<u32> {
    (0..points.len() as u32)
        .filter(|i| {
            let (p, n) = (points[*i as usize], normals[*i as usize]);
            n[2].abs() > 0.7 && floors.iter().any(|f| (p[2] - f).abs() <= thickness) && region.is_none_or(|r| r.contains(p))
        })
        .collect()
}

/// Points on vertical surfaces (|normal.z| < 0.3) at least `min_height` above the floor below them.
pub fn select_walls(points: &[[f32; 3]], normals: &[[f32; 3]], floors: &[f32], min_height: f32, region: Option<&Box3>) -> Vec<u32> {
    (0..points.len() as u32)
        .filter(|i| {
            let (p, n) = (points[*i as usize], normals[*i as usize]);
            let floor = floors.iter().copied().filter(|f| *f <= p[2] + 0.1).fold(f32::NEG_INFINITY, f32::max);
            let floor = if floor.is_finite() { floor } else { floors.first().copied().unwrap_or(f32::NEG_INFINITY) };
            (n[0] != 0.0 || n[1] != 0.0 || n[2] != 0.0) && n[2].abs() < 0.3 && p[2] - floor >= min_height && region.is_none_or(|r| r.contains(p))
        })
        .collect()
}

/// The tightest box around the object in `region` (what "fit a box to this" means): the non-floor points inside it,
/// split into connected pieces (12 cm cells: a one-voxel gap still joins); the piece nearest the region's center is the object, so a rough region
/// that also clips a neighbour (the table next to a chair) still fits the chair. Yaw: the principal horizontal axis,
/// unless an axis-aligned box is about as tight (within 10% footprint), since most furniture sits square to the room.
/// Extents are the 2nd–98th percentiles (stray voxels don't inflate it).
pub fn fit_box(points: &[[f32; 3]], normals: Option<&[[f32; 3]]>, region: &Box3, floors: &[f32]) -> Option<(Box3, usize)> {
    let inside: Vec<[f32; 3]> = points
        .iter()
        .enumerate()
        .filter(|(i, p)| {
            region.contains(**p)
                && !normals.is_some_and(|n| n[*i][2].abs() > 0.7 && floors.iter().any(|f| (p[2] - f).abs() < 0.08))
        })
        .map(|(_, p)| *p)
        .collect();
    if inside.len() < 4 {
        return None;
    }
    let inside = nearest_piece(&inside, [region.center[0], region.center[1]], 0.12);
    if inside.len() < 4 {
        return None;
    }
    let percentile = |mut values: Vec<f32>, q: f32| -> f32 {
        values.sort_by(|a, b| a.total_cmp(b));
        values[((values.len() - 1) as f32 * q).round() as usize]
    };
    let extents = |yaw: f32| {
        let (s, c) = yaw.sin_cos();
        let u: Vec<f32> = inside.iter().map(|p| c * p[0] + s * p[1]).collect();
        let v: Vec<f32> = inside.iter().map(|p| -s * p[0] + c * p[1]).collect();
        ((percentile(u.clone(), 0.02), percentile(u, 0.98)), (percentile(v.clone(), 0.02), percentile(v, 0.98)))
    };
    let n = inside.len() as f32;
    let (mx, my) = (inside.iter().map(|p| p[0]).sum::<f32>() / n, inside.iter().map(|p| p[1]).sum::<f32>() / n);
    let (mut sxx, mut sxy, mut syy) = (0.0f32, 0.0f32, 0.0f32);
    for p in &inside {
        let (dx, dy) = (p[0] - mx, p[1] - my);
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    let principal = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let area = |e: &((f32, f32), (f32, f32))| (e.0 .1 - e.0 .0) * (e.1 .1 - e.1 .0);
    let (aligned, turned) = (extents(0.0), extents(principal));
    let (yaw, ((u0, u1), (v0, v1))) = if area(&aligned) <= area(&turned) * 1.1 { (0.0, aligned) } else { (principal, turned) };
    let (s, c) = yaw.sin_cos();
    let z: Vec<f32> = inside.iter().map(|p| p[2]).collect();
    let (z0, z1) = (percentile(z.clone(), 0.0), percentile(z, 0.98));
    let (cu, cv) = ((u0 + u1) / 2.0, (v0 + v1) / 2.0);
    let center = [c * cu - s * cv, s * cu + c * cv, (z0 + z1) / 2.0];
    Some((Box3 { center, size: [(u1 - u0).max(0.05), (v1 - v0).max(0.05), (z1 - z0).max(0.05)], yaw }, inside.len()))
}

/// The connected piece (26-neighbour `cell` cells) with a point nearest `center` (x, y).
fn nearest_piece(points: &[[f32; 3]], center: [f32; 2], cell: f32) -> Vec<[f32; 3]> {
    let mut by_key: AHashMap<Key, Vec<usize>> = AHashMap::new();
    for (index, p) in points.iter().enumerate() {
        by_key.entry(key_of(*p, cell)).or_default().push(index);
    }
    let mut seen: AHashSet<Key> = AHashSet::new();
    let mut best: (f32, Vec<usize>) = (f32::MAX, Vec::new());
    for start in by_key.keys() {
        if !seen.insert(*start) {
            continue;
        }
        let mut stack = vec![*start];
        let mut members = Vec::new();
        while let Some(k) = stack.pop() {
            members.extend(by_key[&k].iter().copied());
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let n = (k.0 + dx, k.1 + dy, k.2 + dz);
                        if by_key.contains_key(&n) && seen.insert(n) {
                            stack.push(n);
                        }
                    }
                }
            }
        }
        // tiny specks don't win by being near the center
        if members.len() < 4 {
            continue;
        }
        let distance = members.iter().map(|i| (points[*i][0] - center[0]).hypot(points[*i][1] - center[1])).fold(f32::MAX, f32::min);
        if distance < best.0 || (distance == best.0 && members.len() > best.1.len()) {
            best = (distance, members);
        }
    }
    if best.1.is_empty() {
        return points.to_vec();
    }
    best.1.iter().map(|i| points[*i]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room_with_specks() -> Vec<[f32; 3]> {
        let mut points = Vec::new();
        for i in 0..60 {
            for j in 0..60 {
                points.push([i as f32 * 0.1, j as f32 * 0.1, 0.05]); // floor
            }
            for k in 0..25 {
                points.push([i as f32 * 0.1, 0.0, 0.1 + k as f32 * 0.1]); // wall
            }
        }
        points.extend([[3.0, 3.0, 1.5], [3.05, 3.0, 1.5], [1.0, 4.0, 2.2]]); // floating specks
        points
    }

    #[test]
    fn floating_specks_and_outliers() {
        let points = room_with_specks();
        let floating = select_floating(&points, 0.1, 10, None);
        assert_eq!(floating.len(), 3);
        let only_in = select_floating(&points, 0.1, 10, Some(&Box3::from_bounds([2.5, 2.5, 1.0], [3.5, 3.5, 2.0])));
        assert_eq!(only_in.len(), 2);
        let outliers = select_outliers(&points, 8, 2.0, None);
        assert!(outliers.contains(&(points.len() as u32 - 1)), "the lone speck is an outlier");
    }

    #[test]
    fn dominant_wall_angle_of_a_turned_room() {
        for degrees in [0.0f32, 17.0, -30.0, 44.0] {
            let a = degrees.to_radians();
            // two walls' normals (and some floor) of a room turned by `degrees`
            let mut normals = vec![[0.0, 0.0, 1.0]; 500];
            for k in 0..400 {
                let wobble = ((k % 7) as f32 - 3.0) * 0.4f32.to_radians();
                let b = a + wobble + if k % 2 == 0 { 0.0 } else { std::f32::consts::FRAC_PI_2 };
                normals.push([b.cos(), b.sin(), 0.05]);
            }
            let found = dominant_wall_angle(&normals).unwrap().to_degrees();
            let folded = ((degrees + 45.0).rem_euclid(90.0)) - 45.0;
            assert!((found - folded).abs() < 1.0, "{degrees}: found {found}");
        }
    }

    #[test]
    fn floors_walls_and_box_fit() {
        let mut points = room_with_specks();
        // a second storey 3 m up
        for i in 0..60 {
            for j in 0..60 {
                points.push([i as f32 * 0.1, j as f32 * 0.1, 3.05]);
            }
            for k in 0..20 {
                points.push([i as f32 * 0.1, 0.0, 3.1 + k as f32 * 0.1]);
            }
        }
        // a table-sized block on the ground floor
        let mut table = Vec::new();
        for i in 0..10 {
            for j in 0..6 {
                table.push([2.0 + i as f32 * 0.1, 2.0 + j as f32 * 0.1, 0.75]);
            }
        }
        points.extend(&table);
        let normals = normals(&points, 0.25);
        let floors = floor_levels(&points, &normals, 1.8);
        assert_eq!(floors.len(), 2, "{floors:?}");
        assert!((floors[0] - 0.05).abs() < 0.06 && (floors[1] - 3.05).abs() < 0.06);
        let floor = select_floor(&points, &normals, &floors, 0.08, None);
        assert!(floor.len() >= 7000, "{}", floor.len());
        let walls = select_walls(&points, &normals, &floors, 0.2, None);
        assert!(walls.len() > 1000);
        let (fit, used) = fit_box(&points, Some(&normals), &Box3::from_bounds([1.6, 1.6, 0.0], [3.4, 3.0, 1.2]), &floors).unwrap();
        assert_eq!(used, table.len());
        assert!((fit.center[0] - 2.45).abs() < 0.1 && (fit.center[1] - 2.25).abs() < 0.1, "{fit:?}");
    }
}
