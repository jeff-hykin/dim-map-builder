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
    fn top(&self, floor: f32) -> f32 {
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
    fn push(&mut self, p: [f32; 3], normal: [f32; 3]) {
        self.add.push(p);
        self.add_normals.push(normal);
    }
}

fn center(key: Key, voxel: f32) -> [f32; 3] {
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
fn surfaces(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, columns: &AHashSet<(i32, i32)>) -> AHashSet<Key> {
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
fn column_floor(surfaces: &AHashSet<Key>, column: (i32, i32), voxel: f32, estimate: f32) -> f32 {
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

/// Straighten a wall: the voxels within `width / 2` of the line from `from` to `to` (above the local floor, up to
/// `reach`) are replaced by a one-voxel-thick wall on the line fitted through them, from the floor to a level top (the
/// running median of the observed tops). Gaps and notches are filled, except a doorway: at least 0.6 m along the wall
/// with nothing within 0.5 m of the floor, which stays open under a level header.
pub fn straighten(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, from: [f32; 2], to: [f32; 2], width: f32, floor_at: impl Fn(f32, f32) -> Option<f32>, reach: Reach) -> Edit {
    let stroke = Stroke { path: vec![from, to], radius: width / 2.0 };
    let columns: AHashSet<(i32, i32)> = stroke.columns(voxel).into_iter().collect();
    let members = by_column(points, voxel, &columns);
    let surfaces = surfaces(points, normals, voxel, &columns);
    let mut floors: AHashMap<(i32, i32), f32> = AHashMap::new();
    let mut wall: Vec<u32> = Vec::new();
    for (column, here) in &members {
        let (x, y) = ((column.0 as f32 + 0.5) * voxel, (column.1 as f32 + 0.5) * voxel);
        let Some(estimate) = floor_at(x, y) else { continue };
        let floor = column_floor(&surfaces, *column, voxel, estimate);
        floors.insert(*column, floor);
        let top = reach.top(floor);
        wall.extend(here.iter().filter(|i| {
            let z = points[**i as usize][2];
            z > floor + voxel * 0.5 && z <= top
        }));
    }
    let mut edit = Edit::default();
    if wall.len() < 8 {
        return edit;
    }
    // the line: principal axis of the wall voxels' x, y, refit twice on the closest 60% (noise off the wall drops out)
    let xy: Vec<[f32; 2]> = wall.iter().map(|i| [points[*i as usize][0], points[*i as usize][1]]).collect();
    let fit = |chosen: &[[f32; 2]]| -> ([f32; 2], [f32; 2]) {
        let n = chosen.len() as f32;
        let (mx, my) = (chosen.iter().map(|p| p[0]).sum::<f32>() / n, chosen.iter().map(|p| p[1]).sum::<f32>() / n);
        let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
        for p in chosen {
            let (dx, dy) = (p[0] - mx, p[1] - my);
            sxx += dx * dx;
            sxy += dx * dy;
            syy += dy * dy;
        }
        let angle = 0.5 * (2.0 * sxy).atan2(sxx - syy);
        ([mx, my], [angle.cos(), angle.sin()])
    };
    let (mut origin, mut direction) = fit(&xy);
    for _ in 0..2 {
        let mut ranked: Vec<(f32, [f32; 2])> = xy.iter().map(|p| (((p[0] - origin[0]) * -direction[1] + (p[1] - origin[1]) * direction[0]).abs(), *p)).collect();
        ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
        let keep: Vec<[f32; 2]> = ranked[..(ranked.len() * 6 / 10).max(4)].iter().map(|r| r.1).collect();
        (origin, direction) = fit(&keep);
    }
    // which heights each voxel-long stretch of the wall had
    let along = |p: [f32; 2]| (p[0] - origin[0]) * direction[0] + (p[1] - origin[1]) * direction[1];
    let mut spans: Vec<f32> = xy.iter().map(|p| along(*p)).collect();
    spans.sort_by(|a, b| a.total_cmp(b));
    let (start, end) = (spans[spans.len() / 50], spans[spans.len() - 1 - spans.len() / 50]);
    let bins = ((end - start) / voxel).ceil().max(1.0) as usize;
    let mut layers: Vec<AHashSet<i32>> = vec![AHashSet::new(); bins];
    for i in &wall {
        let p = points[*i as usize];
        let bin = ((along([p[0], p[1]]) - start) / voxel).floor();
        if bin >= 0.0 && (bin as usize) < bins {
            layers[bin as usize].insert((p[2] / voxel).floor() as i32);
        }
    }
    let normal = [-direction[1], direction[0], 0.0];
    // per stretch: where it is, its floor layer, and the lowest / highest wall voxel seen there
    let stretches: Vec<Option<([f32; 2], (i32, i32), i32, Option<(i32, i32)>)>> = (0..bins)
        .map(|bin| {
            let t = start + (bin as f32 + 0.5) * voxel;
            let (x, y) = (origin[0] + direction[0] * t, origin[1] + direction[1] * t);
            let column = ((x / voxel).floor() as i32, (y / voxel).floor() as i32);
            let floor = floors.get(&column).copied().or_else(|| floor_at(x, y))?;
            let span = layers[bin].iter().copied().fold(None, |range: Option<(i32, i32)>, l| Some(range.map_or((l, l), |(lo, hi)| (lo.min(l), hi.max(l)))));
            Some(([x, y], column, (floor / voxel).floor() as i32 + 1, span))
        })
        .collect();
    // a doorway: at least 0.6 m along the wall where nothing comes down to within 0.5 m of the floor
    let near_floor = (0.5 / voxel).round() as i32;
    let open: Vec<bool> = stretches.iter().map(|s| s.is_none_or(|(_, _, bottom, span)| span.is_none_or(|(low, _)| low - bottom > near_floor))).collect();
    let mut doorway = vec![false; bins];
    let mut run_start = 0;
    for bin in 0..=bins {
        if bin < bins && open[bin] {
            continue;
        }
        if (bin - run_start) as f32 * voxel >= 0.6 {
            doorway[run_start..bin].iter_mut().for_each(|d| *d = true);
        }
        run_start = bin + 1;
    }
    // the wall's top: the median of the stretch tops within 0.4 m, so a ragged top becomes a level one
    let window = (0.4 / voxel).round() as usize;
    let median = |values: &mut Vec<i32>| -> Option<i32> {
        values.sort_unstable();
        values.get(values.len() / 2).copied()
    };
    let tops: Vec<Option<i32>> = (0..bins)
        .map(|bin| median(&mut (bin.saturating_sub(window)..(bin + window + 1).min(bins)).filter_map(|b| stretches[b].and_then(|s| s.3).map(|span| span.1)).collect()))
        .collect();
    let mut added: AHashSet<Key> = AHashSet::new();
    let mut bin = 0;
    while bin < bins {
        // a doorway keeps its opening and gets a level header if there was wall above it
        if doorway[bin] {
            let end = (bin..bins).find(|b| !doorway[*b]).unwrap_or(bins);
            let header = median(&mut (bin..end).filter_map(|b| stretches[b].and_then(|s| s.3).map(|span| span.0)).collect());
            if let Some(header) = header {
                for b in bin..end {
                    if let (Some((_, column, _, _)), Some(top)) = (stretches[b], tops[b]) {
                        for layer in header..=top {
                            let key = (column.0, column.1, layer);
                            if added.insert(key) {
                                edit.push(center(key, voxel), normal);
                            }
                        }
                    }
                }
            }
            bin = end;
            continue;
        }
        // wall: from one over the floor up to the level top (small gaps and notches filled)
        if let (Some((_, column, bottom, _)), Some(top)) = (stretches[bin], tops[bin]) {
            for layer in bottom..=top {
                let key = (column.0, column.1, layer);
                if added.insert(key) {
                    edit.push(center(key, voxel), normal);
                }
            }
        }
        bin += 1;
    }
    edit.remove = wall;
    edit.remove.sort_unstable();
    edit
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
        let edit = straighten(&points, &normals, 0.05, [2.0, 0.1], [2.0, 3.9], 0.3, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX });
        let at_y = |y: f32| -> Vec<f32> {
            let mut zs: Vec<f32> = edit.add.iter().filter(|p| (p[1] - y).abs() < 0.03).map(|p| p[2]).collect();
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

    #[test]
    fn straighten_a_wobbly_wall() {
        let (points, normals) = room();
        let edit = straighten(&points, &normals, 0.05, [2.0, 0.2], [2.0, 3.8], 0.4, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX });
        let xs: AHashSet<i32> = edit.add.iter().map(|p| (p[0] / 0.05).floor() as i32).collect();
        // the wobble spanned 5 columns; the new wall is one column thick, two where the fitted line crosses a boundary
        assert!(xs.len() <= 2 && xs.iter().max().unwrap() - xs.iter().min().unwrap() <= 1, "a straight line of columns: {xs:?}");
        let x = edit.add.iter().map(|p| p[0]).sum::<f32>() / edit.add.len() as f32;
        assert!((x - 2.0).abs() < 0.1, "on the wall's mean line: {x}");
        let top = edit.add.iter().map(|p| p[2]).fold(0.0, f32::max);
        assert!((top - at(40)).abs() < 1e-4, "keeps the wall's height");
        assert!(edit.remove.len() > 3000, "the old wall goes: {}", edit.remove.len());
        assert!(edit.add.iter().all(|p| p[2] > at(0)), "the floor is untouched");
    }
}
