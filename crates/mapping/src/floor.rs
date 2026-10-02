//! The local floor: per `cell` column and storey, the height of the floor there, so sloped floors, ramps and stairs
//! are followed instead of one flat z per storey. A column's floor is its lowest dense up-facing layer within the
//! storey's height band, kept where it joins the storey's level in small steps (so a couch seat or a tabletop is dropped),
//! and the holes left are filled from the surrounding floor (up to `fill_distance` from anything measured).
use ahash::AHashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FloorOptions {
    pub cell: f32,
    /// up-facing voxels a column's layer (two voxel layers) needs to count as floor
    pub min_count: usize,
    /// floor columns within this of the storey's level start the floor
    pub seed_band: f32,
    /// the largest step between neighbouring columns that still joins them (a stair's riser, not a couch seat)
    pub max_step: f32,
    /// holes are filled this far from measured floor
    pub fill_distance: f32,
}

impl Default for FloorOptions {
    fn default() -> Self {
        FloorOptions { cell: 0.25, min_count: 3, seed_band: 0.1, max_step: 0.25, fill_distance: 2.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoreyFloor {
    /// the storey's nominal floor height
    pub level: f32,
    /// the band of heights searched for this storey's floor
    pub band: [f32; 2],
    /// row-major per cell: the floor's z, NaN where unknown
    pub heights: Vec<f32>,
    /// true where the height was measured (not filled from neighbours)
    pub measured: Vec<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FloorModel {
    pub cell: f32,
    /// world x, y of cell (0, 0)'s corner
    pub origin: [f32; 2],
    pub width: u32,
    pub height: u32,
    pub storeys: Vec<StoreyFloor>,
}

/// Each storey's search band: from a little under its level to just under the next storey's (whose ceiling then
/// stays out of it), the top storey up to 1.5 m above its level.
pub fn bands(levels: &[f32]) -> Vec<[f32; 2]> {
    levels
        .iter()
        .enumerate()
        .map(|(index, level)| {
            let low = if index == 0 { level - 0.6 } else { level - 0.25 };
            let high = levels.get(index + 1).map_or(level + 1.5, |next| next - 0.25);
            [low, high]
        })
        .collect()
}

pub fn build(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, levels: &[f32], options: &FloorOptions) -> FloorModel {
    let cell = options.cell;
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in points {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    if points.is_empty() {
        return FloorModel { cell, origin: [0.0, 0.0], width: 0, height: 0, storeys: Vec::new() };
    }
    let origin = [(x0 / cell).floor() * cell - cell, (y0 / cell).floor() * cell - cell];
    let width = ((x1 - origin[0]) / cell).ceil() as usize + 2;
    let height = ((y1 - origin[1]) / cell).ceil() as usize + 2;
    let storeys = bands(levels)
        .into_iter()
        .zip(levels)
        .map(|(band, level)| {
            let (heights, measured) = storey(points, normals, voxel, band, *level, origin, width, height, options);
            StoreyFloor { level: *level, band, heights, measured }
        })
        .collect();
    FloorModel { cell, origin, width: width as u32, height: height as u32, storeys }
}

#[allow(clippy::too_many_arguments)]
fn storey(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, band: [f32; 2], level: f32, origin: [f32; 2], width: usize, height: usize, options: &FloorOptions) -> (Vec<f32>, Vec<bool>) {
    let cell = options.cell;
    // up-facing voxels per (column, voxel layer): count and z sum
    let mut layers: AHashMap<(usize, i32), (usize, f32)> = AHashMap::new();
    for (p, n) in points.iter().zip(normals) {
        if n[2] < 0.8 || p[2] < band[0] || p[2] > band[1] {
            continue;
        }
        let column = ((p[1] - origin[1]) / cell) as usize * width + ((p[0] - origin[0]) / cell) as usize;
        let entry = layers.entry((column, (p[2] / voxel).floor() as i32)).or_default();
        entry.0 += 1;
        entry.1 += p[2];
    }
    let mut by_column: AHashMap<usize, Vec<(i32, usize, f32)>> = AHashMap::new();
    for ((column, layer), (count, sum)) in layers {
        by_column.entry(column).or_default().push((layer, count, sum));
    }
    let mut heights = vec![f32::NAN; width * height];
    for (column, mut list) in by_column {
        list.sort_by_key(|l| l.0);
        // the lowest layer that, with the one above it, is dense enough
        for (index, (layer, count, sum)) in list.iter().enumerate() {
            let next = list.get(index + 1).filter(|n| n.0 == layer + 1);
            let total = count + next.map_or(0, |n| n.1);
            if total >= options.min_count {
                heights[column] = (sum + next.map_or(0.0, |n| n.2)) / total as f32;
                break;
            }
        }
    }
    // furniture: floor is what connects to the storey's level in small steps (ramps, stairs); a couch seat or a
    // tabletop is a jump up from everything around it
    let mut reached = vec![false; width * height];
    let mut stack: Vec<usize> = (0..width * height).filter(|i| (heights[*i] - level).abs() <= options.seed_band).collect();
    for i in &stack {
        reached[*i] = true;
    }
    while let Some(i) = stack.pop() {
        let (row, column) = ((i / width) as isize, (i % width) as isize);
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            let (r, c) = (row + dy, column + dx);
            if r < 0 || c < 0 || r as usize >= height || c as usize >= width {
                continue;
            }
            let next = r as usize * width + c as usize;
            if !reached[next] && (heights[next] - heights[i]).abs() <= options.max_step {
                reached[next] = true;
                stack.push(next);
            }
        }
    }
    for (value, reached) in heights.iter_mut().zip(&reached) {
        if !reached {
            *value = f32::NAN;
        }
    }
    let measured: Vec<bool> = heights.iter().map(|h| !h.is_nan()).collect();
    // holes: grown in from the known neighbours, one ring per pass
    for _ in 0..(options.fill_distance / cell).ceil() as usize {
        let before = heights.clone();
        let mut changed = false;
        for row in 0..height {
            for column in 0..width {
                if !before[row * width + column].is_nan() {
                    continue;
                }
                let (mut sum, mut count) = (0.0, 0);
                for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)] {
                    let (r, c) = (row as isize + dy, column as isize + dx);
                    if r >= 0 && c >= 0 && (r as usize) < height && (c as usize) < width {
                        let value = before[r as usize * width + c as usize];
                        if !value.is_nan() {
                            sum += value;
                            count += 1;
                        }
                    }
                }
                if count > 0 {
                    heights[row * width + column] = sum / count as f32;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    (heights, measured)
}

impl FloorModel {
    /// Storey `index`'s floor at (x, y): bilinear between cell centers over the known ones.
    pub fn height_at(&self, index: usize, x: f32, y: f32) -> Option<f32> {
        let storey = self.storeys.get(index)?;
        let fx = (x - self.origin[0]) / self.cell - 0.5;
        let fy = (y - self.origin[1]) / self.cell - 0.5;
        let (c0, r0) = (fx.floor() as isize, fy.floor() as isize);
        let (tx, ty) = (fx - c0 as f32, fy - r0 as f32);
        let (mut sum, mut weights) = (0.0, 0.0);
        for (dc, dr, weight) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
            let (c, r) = (c0 + dc, r0 + dr);
            if c < 0 || r < 0 || c >= self.width as isize || r >= self.height as isize {
                continue;
            }
            let value = storey.heights[r as usize * self.width as usize + c as usize];
            if !value.is_nan() && weight > 0.0 {
                sum += value * weight;
                weights += weight;
            }
        }
        (weights > 1e-6).then(|| sum / weights)
    }

    /// The floor under a point: the highest storey floor at (x, y) that is at most `tolerance` above z.
    pub fn local(&self, p: [f32; 3], tolerance: f32) -> Option<(usize, f32)> {
        (0..self.storeys.len()).filter_map(|index| self.height_at(index, p[0], p[1]).map(|h| (index, h))).filter(|(_, h)| *h <= p[2] + tolerance).max_by(|a, b| a.1.total_cmp(&b.1))
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::voxels;

    /// voxel centers on a 5 cm grid
    pub fn at(k: i32) -> f32 {
        (k as f32 + 0.5) * 0.05
    }

    pub fn snap(z: f32) -> f32 {
        at((z / 0.05).floor() as i32)
    }

    /// 8 x 4 m: a ramp rising 0.15 m per meter along x with a wall at y = 0 and a couch-sized block on it
    fn ramp() -> (Vec<[f32; 3]>, impl Fn(f32, f32) -> f32) {
        let floor = |x: f32, _y: f32| 0.15 * x;
        let mut points = Vec::new();
        for i in 0..160 {
            for j in 0..80 {
                let (x, y) = (at(i), at(j));
                let couch = (3.0..5.0).contains(&x) && (1.5..2.4).contains(&y);
                if !couch {
                    points.push([x, y, snap(floor(x, y))]);
                }
            }
            for k in 1..40 {
                points.push([at(i), at(0), snap(floor(at(i), 0.0)) + k as f32 * 0.05]);
            }
        }
        // the couch: a seat 0.45 m up and its sides, with no floor seen under it
        for i in 60..100 {
            for j in 30..48 {
                let (x, y) = (at(i), at(j));
                points.push([x, y, snap(floor(x, y) + 0.45)]);
            }
        }
        (points, floor)
    }

    #[test]
    fn follows_a_ramp_under_furniture() {
        let (points, floor) = ramp();
        let normals = voxels::normals(&points, 0.18);
        let model = build(&points, &normals, 0.05, &[0.0], &FloorOptions::default());
        let mut worst = 0.0f32;
        for x in [0.5, 1.7, 3.5, 4.0, 4.6, 6.2, 7.5] {
            for y in [0.5, 1.9, 2.2, 3.4] {
                let error = (model.height_at(0, x, y).unwrap() - floor(x, y)).abs();
                worst = worst.max(error);
            }
        }
        assert!(worst < 0.05, "worst floor error {worst} m on the ramp (couch at x 3..5, y 1.5..2.4)");
    }

    /// two storeys 3 m apart (a slab with its ceiling 0.3 m under the upper floor) joined by a flight of stairs
    #[test]
    fn two_storeys_and_stairs() {
        let mut points = Vec::new();
        // treads 0.3 m deep rising 0.18 m along x from x = 2 to 7, in y 0..1; the upper floor from x = 7
        let tread = |x: f32| ((x - 2.0) / 0.3).floor() * 0.18;
        for i in 0..240 {
            for j in 0..80 {
                let (x, y) = (at(i), at(j));
                if !(j < 20 && (40..140).contains(&i)) {
                    points.push([x, y, at(0)]);
                }
                // the ceiling under the slab is seen in patches, the upper floor everywhere
                if i >= 140 && (i / 10 + j / 10) % 2 == 0 {
                    points.push([x, y, at(54)]);
                }
                if i >= 140 {
                    points.push([x, y, at(60)]);
                }
                if j < 20 && (40..140).contains(&i) {
                    points.push([x, y, snap(tread(x))]);
                }
            }
            // a wall along y = 4 on both storeys
            for k in 1..52 {
                points.push([at(i), at(80), at(k)]);
            }
            if i >= 140 {
                for k in 61..110 {
                    points.push([at(i), at(80), at(k)]);
                }
            }
        }
        let normals = voxels::normals(&points, 0.18);
        let levels = voxels::floor_levels(&points, &normals, 1.8);
        assert_eq!(levels.len(), 2, "{levels:?}");
        let model = build(&points, &normals, 0.05, &levels, &FloorOptions::default());
        for (x, y) in [(0.5, 2.0), (1.5, 3.5), (3.5, 3.0), (9.0, 2.0)] {
            let found = model.height_at(0, x, y).unwrap();
            assert!((found - at(0)).abs() < 0.05, "ground floor at ({x}, {y}): {found}");
        }
        for (x, y) in [(7.6, 2.0), (11.0, 0.5), (11.0, 3.5)] {
            let found = model.height_at(1, x, y).unwrap();
            assert!((found - at(60)).abs() < 0.05, "upper floor at ({x}, {y}): {found}");
        }
        // on the stairs, the floor under a point half a meter above a tread is that tread to within one riser: a 25 cm
        // cell that straddles a riser holds the lower tread (edits refine this per voxel column, edit.rs)
        for x in [2.15, 2.75, 3.35, 4.55, 5.75, 6.65] {
            let tread = snap(tread(x));
            let (_, found) = model.local([x, 0.5, tread + 0.5], 0.1).unwrap();
            assert!((found - tread).abs() <= 0.19, "stairs at x {x}: {found} vs {tread}");
        }
        assert_eq!(model.local([9.0, 2.0, 3.5], 0.2).unwrap().0, 1);
        assert_eq!(model.local([9.0, 2.0, 1.0], 0.2).unwrap().0, 0);
    }
}
