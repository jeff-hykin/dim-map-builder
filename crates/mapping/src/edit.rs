//! The Modify tools, as voxel edits on top of the local floor (floor.rs): erase what stands in brushed columns (and
//! patch the floor under it), draw a block of voxels up from the floor, and replace a noisy wall with a straight one.
//! Each returns which voxels to remove and which to add; the server makes that one undoable edit.
use crate::voxels::{key_of, Key};
use ahash::{AHashMap, AHashSet};

/// A brush stroke (or a line drawn with a brush): the voxel columns within `radius` of the polyline.
#[derive(Debug, Clone)]
pub struct Stroke {
    pub path: Vec<[f32; 2]>,
    pub radius: f32,
}

impl Stroke {
    /// distance from (x, y) to the polyline, and the direction of the nearest segment
    fn nearest(&self, x: f32, y: f32) -> (f32, [f32; 2]) {
        let mut best = (f32::MAX, [1.0, 0.0]);
        if self.path.len() == 1 {
            let p = self.path[0];
            return ((x - p[0]).hypot(y - p[1]), [1.0, 0.0]);
        }
        for pair in self.path.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let length2 = dx * dx + dy * dy;
            let t = if length2 > 0.0 { (((x - a[0]) * dx + (y - a[1]) * dy) / length2).clamp(0.0, 1.0) } else { 0.0 };
            let distance = (x - a[0] - t * dx).hypot(y - a[1] - t * dy);
            if distance < best.0 {
                let length = length2.sqrt().max(1e-6);
                best = (distance, [dx / length, dy / length]);
            }
        }
        best
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        self.nearest(x, y).0 <= self.radius
    }

    /// the voxel columns (x, y keys) whose centers the stroke covers
    fn columns(&self, voxel: f32) -> Vec<(i32, i32)> {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in &self.path {
            x0 = x0.min(p[0] - self.radius);
            y0 = y0.min(p[1] - self.radius);
            x1 = x1.max(p[0] + self.radius);
            y1 = y1.max(p[1] + self.radius);
        }
        let mut found = Vec::new();
        for ix in (x0 / voxel).floor() as i32..=(x1 / voxel).floor() as i32 {
            for iy in (y0 / voxel).floor() as i32..=(y1 / voxel).floor() as i32 {
                if self.contains((ix as f32 + 0.5) * voxel, (iy as f32 + 0.5) * voxel) {
                    found.push((ix, iy));
                }
            }
        }
        found
    }
}

/// How high an edit reaches: `above_floor` meters over the local floor, capped at `top` (absolute z).
#[derive(Debug, Clone, Copy)]
pub struct Reach {
    pub above_floor: Option<f32>,
    pub top: f32,
}

impl Reach {
    pub(crate) fn top(&self, floor: f32) -> f32 {
        self.above_floor.map_or(self.top, |above| (floor + above).min(self.top))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Edit {
    pub remove: Vec<u32>,
    pub add: Vec<[f32; 3]>,
    pub add_normals: Vec<[f32; 3]>,
}

impl Edit {
    pub(crate) fn push(&mut self, p: [f32; 3], normal: [f32; 3]) {
        self.add.push(p);
        self.add_normals.push(normal);
    }
}

pub(crate) fn center(key: Key, voxel: f32) -> [f32; 3] {
    [(key.0 as f32 + 0.5) * voxel, (key.1 as f32 + 0.5) * voxel, (key.2 as f32 + 0.5) * voxel]
}

/// The voxels in some columns, by column.
fn by_column(points: &[[f32; 3]], voxel: f32, columns: &AHashSet<(i32, i32)>) -> AHashMap<(i32, i32), Vec<u32>> {
    let mut found: AHashMap<(i32, i32), Vec<u32>> = AHashMap::new();
    for (index, p) in points.iter().enumerate() {
        let column = ((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32);
        if columns.contains(&column) {
            found.entry(column).or_default().push(index as u32);
        }
    }
    found
}

/// The up-facing voxels around some columns (one column of margin), for `column_floor`.
pub(crate) fn surfaces(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, columns: &AHashSet<(i32, i32)>) -> AHashSet<Key> {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for c in columns {
        x0 = x0.min(c.0 - 1);
        y0 = y0.min(c.1 - 1);
        x1 = x1.max(c.0 + 1);
        y1 = y1.max(c.1 + 1);
    }
    points
        .iter()
        .zip(normals)
        .filter(|(_, n)| n[2] > 0.8)
        .map(|(p, _)| key_of(*p, voxel))
        .filter(|k| (x0..=x1).contains(&k.0) && (y0..=y1).contains(&k.1))
        .collect()
}

/// A column's floor: the floor model's estimate, raised to the highest up-facing *surface* just above it (the model's
/// 25 cm cells blur stair treads; a column knows its own tread). A surface means most of the 3 x 3 columns around have
/// an up-facing voxel at that height, so a wall's ragged foot or a couch seat (too high) doesn't count.
pub(crate) fn column_floor(surfaces: &AHashSet<Key>, column: (i32, i32), voxel: f32, estimate: f32) -> f32 {
    let (low, high) = (((estimate - 0.1) / voxel).floor() as i32, ((estimate + 0.22) / voxel).floor() as i32);
    for layer in (low..=high).rev() {
        let around = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dy| (dx, dy)))
            .filter(|(dx, dy)| surfaces.contains(&(column.0 + dx, column.1 + dy, layer)))
            .count();
        if around >= 5 {
            return (layer as f32 + 0.5) * voxel;
        }
    }
    estimate
}

/// Erase: every voxel in the stroke's columns from one voxel over the local floor up to `reach`, then a floor voxel
/// in each stroked column that has none, at the local floor's height (so an erased couch leaves floor, not a hole).
pub fn erase(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, stroke: &Stroke, floor_at: impl Fn(f32, f32) -> Option<f32>, reach: Reach) -> Edit {
    let columns: AHashSet<(i32, i32)> = stroke.columns(voxel).into_iter().collect();
    let members = by_column(points, voxel, &columns);
    let surfaces = surfaces(points, normals, voxel, &columns);
    let mut edit = Edit::default();
    for column in &columns {
        let (x, y) = ((column.0 as f32 + 0.5) * voxel, (column.1 as f32 + 0.5) * voxel);
        let Some(estimate) = floor_at(x, y) else { continue };
        let empty = Vec::new();
        let here = members.get(column).unwrap_or(&empty);
        let floor = column_floor(&surfaces, *column, voxel, estimate);
        let top = reach.top(floor);
        let mut has_floor = false;
        for index in here {
            let z = points[*index as usize][2];
            if z > floor + voxel * 0.5 && z <= top {
                edit.remove.push(*index);
            } else if (z - floor).abs() <= voxel * 1.5 {
                has_floor = true;
            }
        }
        if !has_floor {
            edit.push(center((column.0, column.1, (floor / voxel).floor() as i32), voxel), [0.0, 0.0, 1.0]);
        }
    }
    edit.remove.sort_unstable();
    edit
}

/// Draw: voxels from one over the local floor up to `height` above it in every stroked column (what's already
/// there stays). Their normals face away from the stroke's line, like a wall's.
pub fn draw(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, stroke: &Stroke, floor_at: impl Fn(f32, f32) -> Option<f32>, height: f32) -> Edit {
    let columns: AHashSet<(i32, i32)> = stroke.columns(voxel).into_iter().collect();
    let members = by_column(points, voxel, &columns);
    let occupied: AHashSet<Key> = members.values().flatten().map(|i| key_of(points[*i as usize], voxel)).collect();
    let surfaces = surfaces(points, normals, voxel, &columns);
    let mut edit = Edit::default();
    for column in &columns {
        let (x, y) = ((column.0 as f32 + 0.5) * voxel, (column.1 as f32 + 0.5) * voxel);
        let Some(estimate) = floor_at(x, y) else { continue };
        let floor = column_floor(&surfaces, *column, voxel, estimate);
        let (_, direction) = stroke.nearest(x, y);
        let normal = [-direction[1], direction[0], 0.0];
        let bottom = (floor / voxel).floor() as i32 + 1;
        let mut layer = bottom;
        while (layer as f32 + 0.5) * voxel <= floor + height {
            let key = (column.0, column.1, layer);
            if !occupied.contains(&key) {
                edit.push(center(key, voxel), normal);
            }
            layer += 1;
        }
    }
    edit
}

/// Straighten a wall along the stroke from `from` to `to` (a band `width` wide): wall.rs.
#[allow(clippy::too_many_arguments)]
pub fn straighten(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, from: [f32; 2], to: [f32; 2], width: f32, thickness: Option<f32>, floor_at: impl Fn(f32, f32) -> Option<f32>, reach: Reach) -> Edit {
    crate::wall::straighten(points, normals, voxel, from, to, width, thickness, floor_at, reach)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floor::tests::at;

    /// a flat floor at z = at(0) and a wall along x = 2 that wobbles ±10 cm, 2 m tall, with a person-sized blob
    fn room() -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
        let (mut points, mut normals) = (Vec::new(), Vec::new());
        for i in 0..80 {
            for j in 0..80 {
                // no floor was seen under the blob at (1, 1)
                if !((17..23).contains(&i) && (17..23).contains(&j)) {
                    points.push([at(i), at(j), at(0)]);
                    normals.push([0.0, 0.0, 1.0]);
                }
            }
        }
        for j in 0..80 {
            let wobble = ((j as f32 * 0.7).sin() * 2.0).round() as i32;
            for k in 1..41 {
                points.push([at(40 + wobble), at(j), at(k)]);
                normals.push([1.0, 0.0, 0.0]);
            }
        }
        for i in 17..23 {
            for j in 17..23 {
                for k in 1..34 {
                    points.push([at(i), at(j), at(k)]);
                    normals.push([1.0, 0.0, 0.0]);
                }
            }
        }
        (points, normals)
    }

    #[test]
    fn erase_leaves_floor() {
        let (points, normals) = room();
        let floor = |_x: f32, _y: f32| Some(at(0));
        let edit = erase(&points, &normals, 0.05, &Stroke { path: vec![[1.0, 1.0]], radius: 0.3 }, floor, Reach { above_floor: Some(2.0), top: f32::MAX });
        assert_eq!(edit.remove.len(), 6 * 6 * 33, "the whole blob");
        assert!(edit.add.len() >= 36 && edit.add.iter().all(|p| (p[2] - at(0)).abs() < 1e-4), "floor patched under it: {}", edit.add.len());
        // capped at 1 m above the floor, the blob's top stays
        let capped = erase(&points, &normals, 0.05, &Stroke { path: vec![[1.0, 1.0]], radius: 0.3 }, floor, Reach { above_floor: Some(1.0), top: f32::MAX });
        assert_eq!(capped.remove.len(), 6 * 6 * 20);
    }

    /// on stairs the floor model can be a riser low; erasing must still keep every tread
    #[test]
    fn erase_on_stairs_keeps_the_treads() {
        let (mut points, mut normals) = (Vec::new(), Vec::new());
        let tread = |i: i32| (i / 6) as f32 * 0.18;
        for i in 0..60 {
            for j in 0..20 {
                points.push([at(i), at(j), crate::floor::tests::snap(tread(i))]);
                normals.push([0.0, 0.0, 1.0]);
            }
        }
        let treads = points.len();
        // a person standing on the stairs
        for k in 1..30 {
            points.push([at(30), at(10), crate::floor::tests::snap(tread(30)) + k as f32 * 0.05]);
            normals.push([1.0, 0.0, 0.0]);
        }
        let model_low = |x: f32, _y: f32| Some(tread((x / 0.05) as i32) - 0.15);
        let edit = erase(&points, &normals, 0.05, &Stroke { path: vec![[0.2, 0.5], [2.8, 0.5]], radius: 0.4 }, model_low, Reach { above_floor: Some(1.8), top: f32::MAX });
        assert!(edit.remove.iter().all(|i| *i as usize >= treads), "a tread was erased");
        assert_eq!(edit.remove.len(), 29, "the person goes");
    }

    #[test]
    fn draw_a_wall() {
        let (points, normals) = room();
        let edit = draw(&points, &normals, 0.05, &Stroke { path: vec![[0.2, 3.0], [1.2, 3.0]], radius: 0.025 }, |_, _| Some(at(0)), 1.0);
        // 20 columns along the line, 20 layers (5 cm .. 1 m)
        assert_eq!(edit.add.len(), 20 * 20, "{}", edit.add.len());
        assert!(edit.add.iter().all(|p| p[2] > at(0) && p[2] <= at(0) + 1.0));
        assert!(edit.add_normals.iter().all(|n| n[1].abs() > 0.99), "faces away from the line");
    }

    /// a straight wall with a ragged top, a 1 m doorway (a header above 2 m), and a 20 cm hole that should close
    #[test]
    fn straighten_keeps_doorways_and_levels_the_top() {
        let (mut points, mut normals) = (Vec::new(), Vec::new());
        for i in 0..80 {
            for j in 0..80 {
                points.push([at(i), at(j), at(0)]);
                normals.push([0.0, 0.0, 1.0]);
            }
        }
        for j in 0..80 {
            let door = (20..40).contains(&j);
            let hole = (60..64).contains(&j);
            // the top wobbles between 2.2 and 2.45 m
            let top = 44 + (j * 7 % 5);
            for k in 1..=top {
                if (door && k < 40) || (hole && k < 30) {
                    continue;
                }
                points.push([at(40), at(j), at(k)]);
                normals.push([1.0, 0.0, 0.0]);
            }
        }
        let edit = straighten(&points, &normals, 0.05, [2.0, 0.1], [2.0, 3.9], 0.3, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX });
        let after = result(&points, &edit);
        let at_y = |y: f32| -> Vec<f32> {
            let mut zs: Vec<f32> = after.iter().filter(|p| (p[1] - y).abs() < 0.03 && p[2] > 0.03).map(|p| p[2]).collect();
            zs.sort_by(|a, b| a.total_cmp(b));
            zs
        };
        let door = at_y(at(30));
        assert!(!door.is_empty() && door[0] > 1.9, "the doorway stays open under its header: {:?}", door.first());
        let hole = at_y(at(62));
        assert!((hole[0] - at(1)).abs() < 1e-4, "the small hole is filled down to the floor");
        let tops: AHashSet<i32> = [at(5), at(12), at(50), at(70)].iter().map(|y| (at_y(*y).last().unwrap() / 0.05) as i32).collect();
        assert!(tops.len() <= 2, "a level top, not a comb: {tops:?}");
    }

    /// the map after an edit: what wasn't removed, and what was added
    fn result(points: &[[f32; 3]], edit: &Edit) -> Vec<[f32; 3]> {
        let removed: AHashSet<u32> = edit.remove.iter().copied().collect();
        points.iter().enumerate().filter(|(i, _)| !removed.contains(&(*i as u32))).map(|(_, p)| *p).chain(edit.add.iter().copied()).collect()
    }

    /// a wall along x = 2 of `thickness` voxels with specks hugging it (±1–2 voxels) and a 2 voxel thick room floor
    fn rough_wall(thickness: i32) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
        let (mut points, mut normals) = (Vec::new(), Vec::new());
        for i in 0..80 {
            for j in 0..80 {
                points.push([at(i), at(j), at(0)]);
                normals.push([0.0, 0.0, 1.0]);
            }
        }
        for j in 0..80 {
            for k in 1..41 {
                for w in 0..thickness {
                    points.push([at(40 + w), at(j), at(k)]);
                    normals.push([1.0, 0.0, 0.0]);
                }
                // fringe: every few voxels a speck one or two voxels off either face
                if (j * 7 + k * 3) % 5 == 0 {
                    let off = if (j + k) % 2 == 0 { -1 - (k % 2) } else { thickness + (k % 2) };
                    points.push([at(40 + off), at(j), at(k)]);
                    normals.push([1.0, 0.0, 0.0]);
                }
            }
        }
        (points, normals)
    }

    /// the x columns with something above the floor after the edit
    fn columns_x(points: &[[f32; 3]], edit: &Edit) -> Vec<i32> {
        let mut xs: Vec<i32> = result(points, edit).iter().filter(|p| p[2] > 0.03).map(|p| (p[0] / 0.05).floor() as i32).collect();
        xs.sort_unstable();
        xs.dedup();
        xs
    }

    #[test]
    fn straighten_a_rough_thin_wall() {
        let (points, normals) = rough_wall(1);
        // the brush covers only part of it: the tool finds the rest
        let edit = straighten(&points, &normals, 0.05, [2.0, 1.2], [2.0, 2.8], 0.4, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX });
        assert_eq!(columns_x(&points, &edit), vec![40], "one voxel thick, on the wall, fringe gone end to end");
        let after = result(&points, &edit);
        assert_eq!(after.iter().filter(|p| p[2] > 0.03).count(), 80 * 40, "every voxel of the wall, nothing else");
    }

    #[test]
    fn straighten_keeps_a_thick_wall_thick() {
        let (points, normals) = rough_wall(3);
        let edit = straighten(&points, &normals, 0.05, [2.05, 0.2], [2.05, 3.8], 0.5, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX });
        assert_eq!(columns_x(&points, &edit), vec![40, 41, 42], "three voxels thick, where the wall was");
        let overridden = straighten(&points, &normals, 0.05, [2.05, 0.2], [2.05, 3.8], 0.5, Some(0.1), |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX });
        assert_eq!(columns_x(&points, &overridden).len(), 2, "a 10 cm override");
        // flat faces: every row along the wall has the same columns
        let mut rows: AHashMap<i32, AHashSet<i32>> = AHashMap::new();
        for p in result(&points, &edit).iter().filter(|p| p[2] > 0.03) {
            rows.entry((p[1] / 0.05).floor() as i32).or_default().insert((p[0] / 0.05).floor() as i32);
        }
        assert!(rows.values().all(|r| r.len() == 3), "flat faces");
    }

    /// a slanted (30°) wall still becomes one straight slab with no holes
    #[test]
    fn straighten_a_slanted_wall() {
        let (c, s) = (30f32.to_radians().cos(), 30f32.to_radians().sin());
        let mut cloud: AHashSet<Key> = AHashSet::new();
        for step in 0..120 {
            let t = step as f32 * 0.03;
            for k in 1..30 {
                cloud.insert(key_of([1.0 + c * t, 1.0 + s * t, at(k)], 0.05));
            }
        }
        let points: Vec<[f32; 3]> = cloud.iter().map(|k| center(*k, 0.05)).collect();
        let normals = vec![[-s, c, 0.0]; points.len()];
        let edit = straighten(&points, &normals, 0.05, [1.0, 1.0], [1.0 + c * 3.5, 1.0 + s * 3.5], 0.4, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX });
        // no holes: every layer of every column the wall covers is filled
        let added: AHashSet<Key> = result(&points, &edit).iter().map(|p| key_of(*p, 0.05)).collect();
        let columns: AHashSet<(i32, i32)> = added.iter().map(|k| (k.0, k.1)).collect();
        assert!(columns.iter().all(|c| (1..29).all(|k| added.contains(&(c.0, c.1, k)))), "solid columns");
    }
}
