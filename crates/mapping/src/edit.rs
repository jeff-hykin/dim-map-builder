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
    /// nothing under this (absolute z) is touched: the storey's own band, not the one below
    pub bottom: f32,
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

/// How far under the local floor an erase reaches (a speck sunk into the floor goes, what's under the floor stays).
const FLOOR_DEPTH: f32 = 0.15;
/// How far from the floor model's estimate a column's own floor may be (the page's 2D view searches as far).
const FLOOR_SEARCH: f32 = 0.3;

/// Erase: one stroke removes every voxel in the columns the brush touches (a column's center within `radius` plus
/// half a voxel of the stroke, and the column under each stroke point, so a click always takes the voxel under it)
/// from just under the local floor (`FLOOR_DEPTH`, never under `reach.bottom`, the storey's band) up to `reach.top`
/// over the local floor (the slice's z-end), except the column's floor: its floor voxel and what is under it stay.
/// A column's floor is what the page's 2D view takes as its floor (frontend `columnFloors`: the highest voxel within
/// `FLOOR_SEARCH` of the floor model with 7 of its 3 x 3 columns occupied at that height and at most 3 just above), else
/// the up-facing voxel nearest the model's estimate that is part of a horizontal sheet (4 of its 8 neighbouring columns
/// occupied within a voxel of its height). Where neither is found, a voxel less than a voxel over the model's floor
/// with company (3 of its 8 neighbouring columns occupied within a voxel of its height) stays: the floor itself.
/// Anything else goes: a lone speck at floor height, clutter where no floor was seen. Where `floor_at` has no estimate
/// the column is cleared from `reach.bottom` to `reach.top` (the server passes the storey's level as the estimate
/// there). What counts as a floor depends on the neighbours, so this repeats until nothing more goes: a second stroke
/// over the same spot finds (almost) nothing left. Erase never adds a voxel.
pub fn erase(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, stroke: &Stroke, floor_at: impl Fn(f32, f32) -> Option<f32>, reach: Reach) -> Edit {
    // what counts as a column's floor depends on its neighbours: once clutter next to it is gone, a voxel that looked
    // like a floor (a top surface) may stop being one, and the page would show it. Repeat until nothing more goes, so
    // one stroke leaves what a second one would.
    let mut removed: AHashSet<u32> = AHashSet::new();
    let original: AHashSet<Key> = points.iter().map(|p| key_of(*p, voxel)).collect();
    let company = |key: Key| (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))).filter(|(dx, dy)| (*dx, *dy) != (0, 0) && (-1..=1).any(|dz| original.contains(&(key.0 + dx, key.1 + dy, key.2 + dz)))).count();
    for _ in 0..8 {
        let keep: Vec<u32> = (0..points.len() as u32).filter(|i| !removed.contains(i)).collect();
        let kept_points: Vec<[f32; 3]> = keep.iter().map(|i| points[*i as usize]).collect();
        let kept_normals: Vec<[f32; 3]> = keep.iter().map(|i| normals[*i as usize]).collect();
        let pass = erase_once(&kept_points, &kept_normals, voxel, stroke, &floor_at, reach, &company);
        if pass.remove.is_empty() {
            break;
        }
        removed.extend(pass.remove.iter().map(|i| keep[*i as usize]));
    }
    let mut edit = Edit { remove: removed.into_iter().collect(), ..Default::default() };
    edit.remove.sort_unstable();
    edit
}

fn erase_once(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, stroke: &Stroke, floor_at: &impl Fn(f32, f32) -> Option<f32>, reach: Reach, company: &impl Fn(Key) -> usize) -> Edit {
    let touching = Stroke { path: stroke.path.clone(), radius: stroke.radius + voxel * 0.5 };
    let mut columns: AHashSet<(i32, i32)> = touching.columns(voxel).into_iter().collect();
    columns.extend(stroke.path.iter().map(|p| ((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32)));
    let members = by_column(points, voxel, &columns);
    // every voxel in and one column around the brushed ones, to tell a floor from a speck
    let around: AHashSet<(i32, i32)> = columns.iter().flat_map(|c| (-1..=1).flat_map(move |dx| (-1..=1).map(move |dy| (c.0 + dx, c.1 + dy)))).collect();
    let occupied: AHashSet<Key> = by_column(points, voxel, &around).values().flatten().map(|i| key_of(points[*i as usize], voxel)).collect();
    let count = |column: (i32, i32), layer: i32| (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))).filter(|(dx, dy)| occupied.contains(&(column.0 + dx, column.1 + dy, layer))).count();
    let sheet = |key: Key| (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))).filter(|(dx, dy)| (*dx, *dy) != (0, 0) && (-1..=1).any(|dz| occupied.contains(&(key.0 + dx, key.1 + dy, key.2 + dz)))).count() >= 4;
    let mut edit = Edit::default();
    for column in &columns {
        let Some(here) = members.get(column) else { continue };
        let (x, y) = ((column.0 as f32 + 0.5) * voxel, (column.1 as f32 + 0.5) * voxel);
        let estimate = floor_at(x, y);
        let floor = estimate.and_then(|estimate| {
            let near = || here.iter().map(|i| (*i as usize, points[*i as usize])).filter(move |(_, p)| (p[2] - estimate).abs() <= FLOOR_SEARCH);
            // the page's own floor: a top surface
            let top_surface = near()
                .filter(|(_, p)| {
                    let layer = (p[2] / voxel).floor() as i32;
                    count(*column, layer) >= 7 && count(*column, layer + 1) <= 3
                })
                .map(|(_, p)| p[2])
                .max_by(f32::total_cmp);
            // else a piece of floor: up-facing, in a horizontal sheet, nearest the estimate
            top_surface.or_else(|| {
                near()
                    .filter(|(i, p)| normals[*i][2] > 0.8 && sheet(key_of(*p, voxel)))
                    .map(|(_, p)| p[2])
                    .min_by(|a, b| (a - estimate).abs().total_cmp(&(b - estimate).abs()))
            })
        });
        let base = floor.or(estimate);
        // the slice's top over whichever is higher, the column's floor or the model's (as the page's slice reaches)
        let top = match (floor, estimate) {
            (Some(floor), Some(estimate)) => reach.top(floor.max(estimate)),
            (_, Some(estimate)) => reach.top(estimate),
            _ => reach.top,
        };
        // no deeper than just under the local floor: a staircase's underside and the room under it aren't in this slice
        let bottom = base.map_or(reach.bottom, |base| reach.bottom.max(base - FLOOR_DEPTH));
        for index in here {
            let p = points[*index as usize];
            let z = p[2];
            // the floor and what's under it; where no floor was found, a voxel less than a voxel over the model's floor
            // that had company before this stroke (3 of the 8 neighbouring columns occupied within a voxel of its
            // height): floor, not a speck
            let is_floor = match floor {
                Some(floor) => z <= floor + voxel * 0.5,
                None => base.is_some_and(|base| z < base + voxel && company(key_of(p, voxel)) >= 3),
            };
            if !is_floor && z >= bottom && z <= top {
                edit.remove.push(*index);
            }
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
    fn erase_takes_the_blob_and_leaves_the_floor() {
        let (points, normals) = room();
        let floor = |_x: f32, _y: f32| Some(at(0));
        let edit = erase(&points, &normals, 0.05, &Stroke { path: vec![[1.0, 1.0]], radius: 0.3 }, floor, Reach { above_floor: Some(2.0), top: f32::MAX, bottom: f32::MIN });
        assert_eq!(edit.remove.len(), 6 * 6 * 33, "the whole blob");
        assert!(edit.add.is_empty(), "erase never adds (no floor is invented under the blob)");
        // capped at 1 m above the floor, the blob's top stays
        let capped = erase(&points, &normals, 0.05, &Stroke { path: vec![[1.0, 1.0]], radius: 0.3 }, floor, Reach { above_floor: Some(1.0), top: f32::MAX, bottom: f32::MIN });
        assert!(capped.remove.len() >= 6 * 6 * 19 && capped.remove.iter().all(|i| points[*i as usize][2] <= at(0) + 1.05), "{}", capped.remove.len());
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
        let edit = erase(&points, &normals, 0.05, &Stroke { path: vec![[0.2, 0.5], [2.8, 0.5]], radius: 0.4 }, model_low, Reach { above_floor: Some(1.8), top: f32::MAX, bottom: f32::MIN });
        assert!(edit.remove.iter().all(|i| *i as usize >= treads), "a tread was erased");
        assert_eq!(edit.remove.len(), 29, "the person goes");
    }

    // ---- erase: one stroke takes everything in the brush volume, never adds, and gets a lone speck in one click ----

    /// a 2 x 2 m floor at z = at(0) (x, y in 0..2 m), clutter on it, and specks: at floor height in the open, one off
    /// the floor model's edge, one just over the floor
    fn cluttered() -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
        let (mut points, mut normals) = (Vec::new(), Vec::new());
        for i in 0..40 {
            for j in 0..40 {
                points.push([at(i), at(j), at(0)]);
                normals.push([0.0, 0.0, 1.0]);
            }
        }
        // a box 30 cm square, 10 to 60 cm tall, with a flat top (a surface), and a cable lying on the floor (1 voxel up)
        for i in 10..16 {
            for j in 10..16 {
                for k in 2..13 {
                    points.push([at(i), at(j), at(k)]);
                    normals.push([0.0, 0.0, if k == 12 { 1.0 } else { 0.0 }]);
                }
            }
        }
        for i in 20..30 {
            points.push([at(i), at(25), at(1)]);
            normals.push([0.0, 0.0, 1.0]);
        }
        // a lamp up to 2.5 m: above the 1.8 m slice top its head stays
        for k in 1..50 {
            points.push([at(30), at(10), at(k)]);
            normals.push([1.0, 0.0, 0.0]);
        }
        (points, normals)
    }

    const SLICE: Reach = Reach { above_floor: Some(1.8), top: 3.0, bottom: -0.5 };
    fn on_the_floor(x: f32, y: f32) -> Option<f32> {
        ((0.0..2.0).contains(&x) && (0.0..2.0).contains(&y)).then_some(at(0))
    }

    #[test]
    fn erase_takes_everything_in_the_brush_volume() {
        let (points, normals) = cluttered();
        let stroke = Stroke { path: vec![[0.4, 0.4], [1.6, 1.4]], radius: 0.35 };
        let edit = erase(&points, &normals, 0.05, &stroke, on_the_floor, SLICE);
        assert!(edit.add.is_empty(), "erase never adds");
        let removed: AHashSet<u32> = edit.remove.iter().copied().collect();
        for (index, p) in points.iter().enumerate() {
            let column = ((p[0] / 0.05).floor() as i32, (p[1] / 0.05).floor() as i32);
            let brushed = Stroke { path: stroke.path.clone(), radius: stroke.radius + 0.025 }.columns(0.05).contains(&column);
            let in_volume = brushed && p[2] > at(0) + 0.01 && p[2] <= at(0) + 1.8;
            assert_eq!(removed.contains(&(index as u32)), in_volume, "{p:?} (brushed {brushed})");
        }
        // the box (its flat top is a surface, but not the floor) and the brushed part of the cable go in one stroke
        assert!(edit.remove.len() >= 6 * 6 * 11, "{}", edit.remove.len());
    }

    #[test]
    fn erase_never_adds_where_there_was_no_floor() {
        let (points, normals) = room();
        let edit = erase(&points, &normals, 0.05, &Stroke { path: vec![[1.0, 1.0], [3.5, 3.5]], radius: 0.4 }, |_, _| Some(at(0)), SLICE);
        assert!(edit.add.is_empty());
        assert!(!edit.remove.is_empty());
    }

    #[test]
    fn erase_a_lone_voxel_in_one_click() {
        let (mut points, mut normals) = cluttered();
        // off the floor model's edge (no floor estimate there), at floor height in the open (no floor around it), and
        // one voxel over the floor; the click lands off each voxel's center, with a brush smaller than a voxel
        let specks = [[at(80), at(80), at(0)], [at(60), at(5), at(0)], [at(5), at(35), at(1)]];
        let first = points.len() as u32;
        for p in specks {
            points.push(p);
            normals.push([0.0, 0.0, 1.0]);
        }
        let floor = |x: f32, y: f32| ((0.0..3.5).contains(&x) && (0.0..2.0).contains(&y)).then_some(at(0));
        for (n, p) in specks.iter().enumerate() {
            let click = Stroke { path: vec![[p[0] + 0.02, p[1] - 0.015]], radius: 0.01 };
            let edit = erase(&points, &normals, 0.05, &click, floor, SLICE);
            assert_eq!(edit.remove, vec![first + n as u32], "speck {n} at {p:?}");
            assert!(edit.add.is_empty());
        }
        // a click on the open floor takes nothing: it is floor
        let edit = erase(&points, &normals, 0.05, &Stroke { path: vec![[1.02, 0.4]], radius: 0.01 }, floor, SLICE);
        assert!(edit.remove.is_empty() && edit.add.is_empty());
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
        let edit = straighten(&points, &normals, 0.05, [2.0, 0.1], [2.0, 3.9], 0.3, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX, bottom: f32::MIN });
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

    /// the x columns with something above the floor after the edit, between y0 and y1
    fn columns_x_in(points: &[[f32; 3]], edit: &Edit, y0: f32, y1: f32) -> Vec<i32> {
        let mut xs: Vec<i32> = result(points, edit).iter().filter(|p| p[2] > 0.03 && (y0..y1).contains(&p[1])).map(|p| (p[0] / 0.05).floor() as i32).collect();
        xs.sort_unstable();
        xs.dedup();
        xs
    }

    #[test]
    fn straighten_a_rough_thin_wall() {
        let (points, normals) = rough_wall(1);
        // the brush covers only part of it: only that part changes
        let edit = straighten(&points, &normals, 0.05, [2.0, 1.2], [2.0, 2.8], 0.4, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX, bottom: f32::MIN });
        let brushed = |p: &[f32; 3]| (1.2..=2.8).contains(&(((p[1] / 0.05).floor() + 0.5) * 0.05)) && (((p[0] / 0.05).floor() + 0.5) * 0.05 - 2.0).abs() <= 0.2;
        assert!(edit.remove.iter().map(|i| &points[*i as usize]).chain(&edit.add).all(brushed), "nothing changes outside the brush");
        assert_eq!(columns_x_in(&points, &edit, 1.25, 2.75), vec![40], "one voxel thick, on the wall, fringe gone under the brush");
        let after = result(&points, &edit);
        assert_eq!(after.iter().filter(|p| p[2] > 0.03 && (1.25..2.75).contains(&p[1])).count(), 30 * 40, "every voxel of the wall there, nothing else");
    }

    #[test]
    fn straighten_keeps_a_thick_wall_thick() {
        let (points, normals) = rough_wall(3);
        let edit = straighten(&points, &normals, 0.05, [2.05, 0.2], [2.05, 3.8], 0.5, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX, bottom: f32::MIN });
        assert_eq!(columns_x_in(&points, &edit, 0.25, 3.75), vec![40, 41, 42], "three voxels thick, where the wall was");
        let overridden = straighten(&points, &normals, 0.05, [2.05, 0.2], [2.05, 3.8], 0.5, Some(0.1), |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX, bottom: f32::MIN });
        assert_eq!(columns_x_in(&points, &overridden, 0.25, 3.75).len(), 2, "a 10 cm override");
        // flat faces: every row along the wall (under the brush) has the same columns
        let mut rows: AHashMap<i32, AHashSet<i32>> = AHashMap::new();
        for p in result(&points, &edit).iter().filter(|p| p[2] > 0.03 && (0.25..3.75).contains(&p[1])) {
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
        let edit = straighten(&points, &normals, 0.05, [1.0, 1.0], [1.0 + c * 3.5, 1.0 + s * 3.5], 0.4, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX, bottom: f32::MIN });
        // no holes: every layer of every column the wall covers is filled
        let added: AHashSet<Key> = result(&points, &edit).iter().map(|p| key_of(*p, 0.05)).collect();
        let columns: AHashSet<(i32, i32)> = added.iter().map(|k| (k.0, k.1)).collect();
        assert!(columns.iter().all(|c| (1..29).all(|k| added.contains(&(c.0, c.1, k)))), "solid columns");
    }
}
