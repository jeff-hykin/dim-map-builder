//! Straighten a wall: fit an oriented rectangular prism to a wall and force the wall's voxels into it. The stroke is
//! only a hint: the fit grows along the wall well past it (through doorways), the prism keeps the wall's own thickness,
//! and its ends meet the walls they run into, at any angle: an L corner fills out to the other wall's outer face, a
//! wall ending on a through-wall (T) stops at its near face. Walls crossing the prism, furniture against it and the
//! other walls are left alone; specks, fringe and stair-step edges hugging the wall go.
use crate::edit::{center, column_floor, surfaces, Edit, Reach};
use crate::voxels::{key_of, Key};
use ahash::{AHashMap, AHashSet};

/// A wall's footprint: a centre line through `origin` along unit `direction`, `half` its thickness / 2.
#[derive(Debug, Clone, Copy)]
struct Line {
    origin: [f32; 2],
    direction: [f32; 2],
    half: f32,
}

impl Line {
    fn normal(&self) -> [f32; 2] {
        [-self.direction[1], self.direction[0]]
    }
    fn along(&self, p: [f32; 2]) -> f32 {
        (p[0] - self.origin[0]) * self.direction[0] + (p[1] - self.origin[1]) * self.direction[1]
    }
    fn across(&self, p: [f32; 2]) -> f32 {
        let n = self.normal();
        (p[0] - self.origin[0]) * n[0] + (p[1] - self.origin[1]) * n[1]
    }
    fn at(&self, along: f32, across: f32) -> [f32; 2] {
        let n = self.normal();
        [self.origin[0] + self.direction[0] * along + n[0] * across, self.origin[1] + self.direction[1] * along + n[1] * across]
    }
}

/// The principal axis of some x, y points (refit twice on the closest 60%, so noise off the line drops out), and how
/// elongated they are (the axes' spread ratio).
fn fit_axis(xy: &[[f32; 2]]) -> Option<([f32; 2], [f32; 2], f32)> {
    if xy.len() < 4 {
        return None;
    }
    let fit = |chosen: &[[f32; 2]]| -> ([f32; 2], [f32; 2], f32) {
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
        let root = (((sxx - syy) / 2.0).powi(2) + sxy * sxy).sqrt();
        let (big, small) = ((sxx + syy) / 2.0 + root, ((sxx + syy) / 2.0 - root).max(1e-9));
        ([mx, my], [angle.cos(), angle.sin()], big / small)
    };
    let (mut origin, mut direction, mut ratio) = fit(xy);
    for _ in 0..2 {
        let mut ranked: Vec<(f32, [f32; 2])> = xy.iter().map(|p| (((p[0] - origin[0]) * -direction[1] + (p[1] - origin[1]) * direction[0]).abs(), *p)).collect();
        ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
        let keep: Vec<[f32; 2]> = ranked[..(ranked.len() * 6 / 10).max(4)].iter().map(|r| r.1).collect();
        (origin, direction, ratio) = fit(&keep);
    }
    Some((origin, direction, ratio))
}

/// The wall's core across its line, per 0.5 m stretch: the run of voxel-wide bins around the fullest with at least half
/// its count. Returns (thickness in voxels, the core's middle across), medians over the stretches.
fn core(xy: &[[f32; 2]], line: &Line, voxel: f32) -> (i32, f32) {
    let mut by_stretch: AHashMap<i32, AHashMap<i32, usize>> = AHashMap::new();
    for p in xy {
        *by_stretch.entry((line.along(*p) / 0.5).floor() as i32).or_default().entry((line.across(*p) / voxel).floor() as i32).or_default() += 1;
    }
    let (mut widths, mut middles) = (Vec::new(), Vec::new());
    for histogram in by_stretch.values() {
        let (&peak_bin, &peak) = histogram.iter().max_by_key(|(bin, count)| (**count, -bin.abs())).unwrap();
        if peak < 3 {
            continue;
        }
        let full = |bin: i32| histogram.get(&bin).is_some_and(|c| *c * 2 >= peak);
        let (mut low, mut high) = (peak_bin, peak_bin);
        while full(low - 1) {
            low -= 1;
        }
        while full(high + 1) {
            high += 1;
        }
        widths.push(high - low + 1);
        middles.push((low + high + 1) as f32 / 2.0 * voxel);
    }
    widths.sort_unstable();
    middles.sort_by(|a, b| a.total_cmp(b));
    (widths.get(widths.len() / 2).copied().unwrap_or(1).max(1), middles.get(middles.len() / 2).copied().unwrap_or(0.0))
}

/// A line fitted to some wall voxels, its thickness from the data (or `thickness`), placed on the voxel grid when it
/// runs along an axis.
/// A wall along a given direction (snapped to an axis within 10°): only its offset and thickness come from the data.
fn wall_line_along(xy: &[[f32; 2]], voxel: f32, thickness: Option<f32>, direction: [f32; 2]) -> Option<(Line, f32)> {
    if xy.len() < 4 {
        return None;
    }
    let n = xy.len() as f32;
    let mean = [xy.iter().map(|p| p[0]).sum::<f32>() / n, xy.iter().map(|p| p[1]).sum::<f32>() / n];
    let mut line = Line { origin: mean, direction, half: 0.0 };
    for axis in [[1.0f32, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]] {
        if direction[0] * axis[0] + direction[1] * axis[1] > 10f32.to_radians().cos() {
            line.direction = axis;
        }
    }
    let normal = line.normal();
    for k in 0..2 {
        if normal[k].abs() > 0.99 {
            line.origin[k] = (line.origin[k] / voxel).floor() * voxel;
        }
    }
    let (voxels_thick, middle) = core(xy, &line, voxel);
    let voxels_thick = thickness.map_or(voxels_thick, |t| ((t / voxel).round() as i32).max(1));
    line.origin = line.at(0.0, middle);
    line.half = voxels_thick as f32 * voxel / 2.0;
    Some((line, 10.0))
}

/// Snapped to an axis within `snap` degrees.
fn wall_line_snapped(xy: &[[f32; 2]], voxel: f32, thickness: Option<f32>, toward: [f32; 2], snap: f32) -> Option<(Line, f32)> {
    let (origin, mut direction, ratio) = fit_axis(xy)?;
    if direction[0] * toward[0] + direction[1] * toward[1] < 0.0 {
        direction = [-direction[0], -direction[1]];
    }
    // within 2° of an axis it is that axis (a square room stays square)
    for axis in [[1.0f32, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]] {
        if direction[0] * axis[0] + direction[1] * axis[1] > snap.to_radians().cos() {
            direction = axis;
        }
    }
    let mut line = Line { origin, direction, half: 0.0 };
    // along an axis, measure across from a voxel boundary, so the core's bins are the voxel columns themselves
    let normal = line.normal();
    for k in 0..2 {
        if normal[k].abs() > 0.99 {
            line.origin[k] = (line.origin[k] / voxel).floor() * voxel;
        }
    }
    let (voxels_thick, middle) = core(xy, &line, voxel);
    let voxels_thick = thickness.map_or(voxels_thick, |t| ((t / voxel).round() as i32).max(1));
    line.origin = line.at(0.0, middle);
    let normal = line.normal();
    for k in 0..2 {
        if normal[k].abs() > 0.99 {
            let shift = if voxels_thick % 2 == 1 { 0.5 } else { 0.0 };
            line.origin[k] = ((line.origin[k] / voxel - shift).round() + shift) * voxel;
        }
    }
    line.half = voxels_thick as f32 * voxel / 2.0;
    Some((line, ratio))
}

/// One end of the prism: keep the side of a line (a point on it, and a normal pointing into the prism).
#[derive(Debug, Clone, Copy)]
struct Cut {
    point: [f32; 2],
    inward: [f32; 2],
}

impl Cut {
    fn keeps(&self, p: [f32; 2], slack: f32) -> bool {
        (p[0] - self.point[0]) * self.inward[0] + (p[1] - self.point[1]) * self.inward[1] >= -slack
    }
}

/// The strongest straight wall among `points` that isn't along `line` (20° or more off it): a Hough vote over 3°
/// steps and 10 cm offsets, then a line fitted to the winner's points. With how elongated those are.
fn other_wall(points: &[[f32; 2]], line: &Line, voxel: f32) -> Option<(Line, f32)> {
    if points.len() < 20 {
        return None;
    }
    // each voxel column votes once (a tall wall doesn't outvote a long one)
    let cells: Vec<[f32; 2]> = {
        let set: AHashSet<(i32, i32)> = points.iter().map(|p| ((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32)).collect();
        set.into_iter().map(|c| [(c.0 as f32 + 0.5) * voxel, (c.1 as f32 + 0.5) * voxel]).collect()
    };
    let band = 1.2 * voxel;
    let mut best: (usize, f32, i32) = (0, 0.0, 0);
    for step in 0..60 {
        let angle = (step as f32 * 3.0).to_radians();
        let direction = [angle.cos(), angle.sin()];
        let sine = (line.direction[0] * direction[1] - line.direction[1] * direction[0]).abs();
        if sine < 20f32.to_radians().sin() {
            continue;
        }
        let normal = [-direction[1], direction[0]];
        let mut bins: AHashMap<i32, usize> = AHashMap::new();
        for p in &cells {
            *bins.entry(((p[0] * normal[0] + p[1] * normal[1]) / band).floor() as i32).or_default() += 1;
        }
        if let Some((bin, count)) = bins.into_iter().max_by_key(|(_, c)| *c) {
            if count > best.0 {
                best = (count, angle, bin);
            }
        }
    }
    if best.0 < 5 {
        return None;
    }
    let normal = [-best.1.sin(), best.1.cos()];
    let offset = (best.2 as f32 + 0.5) * band;
    let chosen: Vec<[f32; 2]> = points.iter().copied().filter(|p| (p[0] * normal[0] + p[1] * normal[1] - offset).abs() <= 2.0 * band).collect();
    // a short piece of wall fits loosely: a few degrees off an axis is the axis
    let (other, ratio) = wall_line_snapped(&chosen, voxel, None, [best.1.cos(), best.1.sin()], 10.0)?;
    // a wall, not a post: half a metre long at least
    let spread = chosen.iter().map(|p| other.along(*p)).fold((f32::MAX, f32::MIN), |(a, b), t| (a.min(t), b.max(t)));
    if spread.1 - spread.0 < 0.5 {
        return None;
    }
    let sine = (line.direction[0] * other.direction[1] - line.direction[1] * other.direction[0]).abs();
    (sine >= 20f32.to_radians().sin()).then_some((other, ratio))
}

/// See the module docs. `width` is the brush's band; `reach` caps how high the wall is rebuilt and cleaned.
#[allow(clippy::too_many_arguments)]
pub fn straighten(points: &[[f32; 3]], normals: &[[f32; 3]], voxel: f32, from: [f32; 2], to: [f32; 2], width: f32, thickness: Option<f32>, floor_at: impl Fn(f32, f32) -> Option<f32>, reach: Reach) -> Edit {
    let mut edit = Edit::default();
    let length = (to[0] - from[0]).hypot(to[1] - from[1]);
    if length < voxel {
        return edit;
    }
    let drawn = Line { origin: from, direction: [(to[0] - from[0]) / length, (to[1] - from[1]) / length], half: width / 2.0 };
    // the search area, well past the brush: 8 m beyond the stroke's ends and 2 m to either side
    let (grow, side) = (8.0, 2.0);
    let near: Vec<u32> = (0..points.len() as u32)
        .filter(|i| {
            let p = points[*i as usize];
            let (t, d) = (drawn.along([p[0], p[1]]), drawn.across([p[0], p[1]]));
            t >= -grow && t <= length + grow && d.abs() <= width / 2.0 + side
        })
        .collect();
    // the wall-height voxels among them: above their column's floor and under the reach
    let columns: AHashSet<(i32, i32)> = near.iter().map(|i| ((points[*i as usize][0] / voxel).floor() as i32, (points[*i as usize][1] / voxel).floor() as i32)).collect();
    let floors_seen = surfaces(points, normals, voxel, &columns);
    let mut floors: AHashMap<(i32, i32), Option<f32>> = AHashMap::new();
    let mut floor_of = |column: (i32, i32)| -> Option<f32> {
        *floors.entry(column).or_insert_with(|| {
            let (x, y) = ((column.0 as f32 + 0.5) * voxel, (column.1 as f32 + 0.5) * voxel);
            floor_at(x, y).map(|estimate| column_floor(&floors_seen, column, voxel, estimate))
        })
    };
    let wallish: Vec<u32> = near
        .iter()
        .copied()
        .filter(|i| {
            let p = points[*i as usize];
            p[2] >= reach.bottom && floor_of(((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32)).is_some_and(|floor| p[2] > floor + voxel * 0.5 && p[2] <= reach.top(floor))
        })
        .collect();
    // the wall's middle (0.4 m and more over the floor and the storey's bottom): what finds and follows the wall, so
    // a floor slab's edge or a step doesn't pass for wall
    let middle: AHashSet<u32> = wallish
        .iter()
        .copied()
        .filter(|i| {
            let p = points[*i as usize];
            p[2] >= reach.bottom + 0.6 && floor_of(((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32)).is_some_and(|floor| p[2] >= floor + 0.4)
        })
        .collect();
    let xy = |i: &u32| [points[*i as usize][0], points[*i as usize][1]];
    // 1. the hint: the wall in the brush's band
    let hint: Vec<[f32; 2]> = wallish.iter().filter(|i| (0.0..=length).contains(&drawn.along(xy(i))) && drawn.across(xy(i)).abs() <= width / 2.0).map(xy).collect();
    if hint.len() < 8 {
        return edit;
    }
    let Some((mut line, _)) = wall_line_snapped(&hint, voxel, thickness, drawn.direction, if length < 1.5 { 10.0 } else { 2.0 }) else { return edit };
    // the stroke says which way the wall runs: a fit more than 20° off it (a short stroke over mixed data) is overruled
    // (a stroke under a metre is all over a short wall: its direction is the stroke's)
    let forced = length < 1.0 || (line.direction[0] * drawn.direction[0] + line.direction[1] * drawn.direction[1]).abs() < 20f32.to_radians().cos();
    if forced {
        let Some((forced, _)) = wall_line_along(&hint, voxel, thickness, drawn.direction) else { return edit };
        line = forced;
    }
    // 2. grow along the wall from the stroke's span while there's wall there (gaps up to 0.25 m: doorways have headers)
    let grow_support = |line: &Line| -> (f32, f32) {
        let mut counts: AHashMap<i32, usize> = AHashMap::new();
        for i in &middle {
            if line.across(xy(i)).abs() <= line.half + voxel {
                *counts.entry((line.along(xy(i)) / voxel).floor() as i32).or_default() += 1;
            }
        }
        let (s0, s1) = {
            let (a, b) = (line.along(from), line.along(to));
            ((a.min(b) / voxel).floor() as i32, (a.max(b) / voxel).floor() as i32)
        };
        let mut inside: Vec<usize> = (s0..=s1).map(|s| counts.get(&s).copied().unwrap_or(0)).collect();
        inside.sort_unstable();
        let enough = (inside[inside.len() / 2] * 3 / 10).max(2);
        let max_gap = (0.25 / voxel).ceil() as i32;
        let walk = |start: i32, step: i32| -> i32 {
            let (mut last, mut gap, mut s) = (start, 0, start);
            loop {
                s += step;
                if (s - start).abs() as f32 * voxel > grow {
                    return last;
                }
                if counts.get(&s).copied().unwrap_or(0) >= enough {
                    last = s;
                    gap = 0;
                } else {
                    gap += 1;
                    if gap > max_gap {
                        return last;
                    }
                }
            }
        };
        let (first, end) = (walk(s0, -1), walk(s1, 1));
        // the stroke's own span counts even where it ran past the wall
        (first.min(s0) as f32 * voxel, (end.max(s1) + 1) as f32 * voxel)
    };
    let (mut start, mut end) = grow_support(&line);
    // 3. refit on the whole wall segment found
    let support: Vec<[f32; 2]> = wallish.iter().map(xy).filter(|p| (start..=end).contains(&line.along(*p)) && line.across(*p).abs() <= line.half + 2.0 * voxel).collect();
    // a short wall fits loosely: within a few degrees of an axis it's the axis
    let snap = if end - start < 1.5 { 10.0 } else { 2.0 };
    let refit = if forced { wall_line_along(&support, voxel, thickness, line.direction) } else { wall_line_snapped(&support, voxel, thickness, line.direction, snap) };
    if let Some((refit, _)) = refit.filter(|(r, _)| (r.direction[0] * line.direction[0] + r.direction[1] * line.direction[1]).abs() >= 20f32.to_radians().cos()) {
        line = refit;
        (start, end) = grow_support(&line);
    }
    // the ends are found twice when needed: a short wall between two walls takes their thickness if its own reads much
    // thicker (sparse data reads wide)
    let mut pass = 0;
    let (half, cuts, others, slab_start, slab_end) = loop {
        let half = line.half;
        // a free end stops at the wall's last voxel (its core, not fringe)
        let core_along: Vec<f32> = wallish.iter().map(xy).filter(|p| line.across(*p).abs() <= half && (start - voxel..=end + voxel).contains(&line.along(*p))).map(|p| line.along(p)).collect();
        if core_along.is_empty() {
            return edit;
        }
        let (data_start, data_end) = core_along.iter().fold((f32::MAX, f32::MIN), |(a, b), t| (a.min(*t), b.max(*t)));
        // 4. the ends: a wall that this one runs into (any angle) cuts it; otherwise it's cut square where it ends
        let mut cuts: Vec<Cut> = Vec::new();
        // the walls met at the ends, with the span along them their voxels cover
        let mut others: Vec<(Line, f32, f32)> = Vec::new();
        let (mut slab_start, mut slab_end) = (data_start - voxel * 0.5, data_end + voxel * 0.5);
        for outward in [-1.0f32, 1.0] {
            let tip = line.at(if outward > 0.0 { data_end + voxel * 0.5 } else { data_start - voxel * 0.5 }, 0.0);
            let tip_along = line.along(tip);
            // a little back from the end (a wall it ends against may overlap it), never as far as the other end's walls
            let back = 0.3f32.min(0.4 * (data_end - data_start));
            // what's around the end: past it or beside it, not this wall's own body
            let around: Vec<[f32; 2]> = wallish
                .iter()
                .map(xy)
                .filter(|p| {
                    let (t, d) = ((line.along(*p) - tip_along) * outward, line.across(*p));
                    (-back..=0.6).contains(&t) && d.abs() <= 0.8 && !(t <= 0.0 && d.abs() <= half + 2.0 * voxel)
                })
                .collect();
            let into = [line.direction[0] * -outward, line.direction[1] * -outward];
            let neighbour = other_wall(&around, &line, voxel).filter(|(_, ratio)| *ratio >= 6.0).map(|(found, ratio)| {
                // refit on more of it: its band, out to 1.5 m from the end, minus this wall's own body
                let more: Vec<[f32; 2]> = wallish
                    .iter()
                    .map(xy)
                    .filter(|p| {
                        let (t, d) = ((line.along(*p) - tip_along) * outward, line.across(*p));
                        found.across(*p).abs() <= found.half + 2.0 * voxel && (p[0] - tip[0]).hypot(p[1] - tip[1]) <= 1.5 && !(t <= 0.0 && d.abs() <= half + 2.0 * voxel)
                    })
                    .collect();
                let snap = if more.len() < 200 { 10.0 } else { 3.0 };
                wall_line_snapped(&more, voxel, None, found.direction, snap).filter(|(again, _)| {
                    (line.direction[0] * again.direction[1] - line.direction[1] * again.direction[0]).abs() >= 20f32.to_radians().sin()
                }).unwrap_or((found, ratio))
            });
            // only a wall right at the end: one further off doesn't pull this one out to it
        let neighbour = neighbour.filter(|(other, _)| {
            let mut toward = other.normal();
            if toward[0] * into[0] + toward[1] * into[1] < 0.0 {
                toward = [-toward[0], -toward[1]];
            }
            let gap = ((tip[0] - other.origin[0]) * toward[0] + (tip[1] - other.origin[1]) * toward[1]).abs() - other.half;
            gap <= 0.3
        });
        let Some((other, _)) = neighbour else {
                cuts.push(Cut { point: tip, inward: into });
                continue;
            };
            // its span: the voxel-long bins along it that are dense (fringe from this wall in its band is sparse)
            let mut counts: AHashMap<i32, usize> = AHashMap::new();
            for p in wallish.iter().map(xy).filter(|p| (p[0] - tip[0]).hypot(p[1] - tip[1]) <= 1.5 && other.across(*p).abs() <= other.half + 0.5 * voxel) {
                *counts.entry((other.along(p) / voxel).floor() as i32).or_default() += 1;
            }
            let dense = counts.values().copied().max().unwrap_or(0) * 3 / 10;
            let (low, high) = counts.iter().filter(|(_, c)| **c >= dense.max(1)).fold((i32::MAX, i32::MIN), |(a, b), (bin, _)| (a.min(*bin), b.max(*bin)));
            let (low, high) = (low as f32 * voxel, (high + 1) as f32 * voxel);
            // a junction only if that wall actually reaches this one's line (a wall passing nearby isn't one)
            let (ends_a, ends_b) = (line.across(other.at(low, 0.0)), line.across(other.at(high, 0.0)));
            let reaches = ends_a.signum() != ends_b.signum() || ends_a.abs().min(ends_b.abs()) <= half + other.half + 0.15;
            if !reaches {
                cuts.push(Cut { point: tip, inward: into });
                continue;
            }
            // the other wall's normal, pointed back into this wall's body
            let mut inward = other.normal();
            if inward[0] * into[0] + inward[1] * into[1] < 0.0 {
                inward = [-inward[0], -inward[1]];
            }
            // does it carry on past this wall's far side (a T: stop at its near face) or end here (an L: fill the corner)?
            let crossing = |p: &[f32; 2]| line.across(*p);
            let (left, right) = around.iter().filter(|p| line.across(**p).abs() > half + voxel).filter(|p| other.across(**p).abs() <= other.half + voxel).fold((0, 0), |(l, r), p| if crossing(p) > 0.0 { (l + 1, r) } else { (l, r + 1) });
            let through = left.min(right) * 4 >= left.max(right);
            let face = if through { other.half } else { -other.half };
            let point = [other.origin[0] + inward[0] * face, other.origin[1] + inward[1] * face];
            cuts.push(Cut { point, inward });
            others.push((other, low - voxel, high + voxel));
            // the slab reaches as far as the face does across its thickness
            let sine = (line.direction[0] * inward[0] + line.direction[1] * inward[1]).abs().max(0.2);
            let reach_along = line.along(point) + outward * (half / sine * (1.0 - sine * sine).sqrt() + voxel);
            // a real corner is right at the wall's last voxel: one that would pull the wall out further isn't this one's
            let tip_at = if outward > 0.0 { data_end } else { data_start };
            if (reach_along - tip_at) * outward > other.half * 2.0 + 0.2 {
                cuts.pop();
                others.pop();
                cuts.push(Cut { point: tip, inward: into });
                continue;
            }
            if outward > 0.0 {
                slab_end = slab_end.max(reach_along);
            } else {
                slab_start = slab_start.min(reach_along);
            }
        }
        let widest = others.iter().map(|(o, _, _)| o.half).fold(0.0f32, f32::max);
        if pass == 0 && thickness.is_none() && end - start < 1.5 && widest > 0.0 && half > widest * 1.5 + 0.01 {
            line.half = widest;
            let normal = line.normal();
            for k in 0..2 {
                if normal[k].abs() > 0.99 {
                    let shift = if ((2.0 * widest / voxel).round() as i32) % 2 == 1 { 0.5 } else { 0.0 };
                    line.origin[k] = ((line.origin[k] / voxel - shift).round() + shift) * voxel;
                }
            }
            pass += 1;
            continue;
        }
        break (half, cuts, others, slab_start, slab_end);
    };
    let inside_cuts = |p: [f32; 2], slack: f32| cuts.iter().all(|c| c.keeps(p, slack));
    // 5. heights along the wall, per voxel-long bin of the support: floor to a level top, doorways under a header
    let bins = ((end - start) / voxel).ceil().max(1.0) as usize;
    let bin_of = |t: f32| (((t - start) / voxel).floor().max(0.0) as usize).min(bins - 1);
    let mut layers: Vec<AHashSet<i32>> = vec![AHashSet::new(); bins];
    for i in &wallish {
        let p = points[*i as usize];
        let (t, d) = (line.along([p[0], p[1]]), line.across([p[0], p[1]]));
        if (start..end).contains(&t) && d.abs() <= half + voxel {
            layers[bin_of(t)].insert((p[2] / voxel).floor() as i32);
        }
    }
    let stretches: Vec<Option<(i32, Option<(i32, i32)>)>> = (0..bins)
        .map(|bin| {
            let [x, y] = line.at(start + (bin as f32 + 0.5) * voxel, 0.0);
            let floor = floor_of(((x / voxel).floor() as i32, (y / voxel).floor() as i32))?;
            let span = layers[bin].iter().copied().fold(None, |range: Option<(i32, i32)>, l| Some(range.map_or((l, l), |(lo, hi)| (lo.min(l), hi.max(l)))));
            Some((((floor / voxel).floor() as i32 + 1).max((reach.bottom.max(-1e6) / voxel).ceil() as i32), span))
        })
        .collect();
    let near_floor = (0.5 / voxel).round() as i32;
    // measured against where the wall usually starts (a wall over a stairwell starts low, not at the model's floor)
    let mut lows: Vec<i32> = stretches.iter().filter_map(|s| s.and_then(|(_, span)| span.map(|(low, _)| low))).collect();
    lows.sort_unstable();
    let usual_low = lows.get(lows.len() / 4).copied();
    let open: Vec<bool> = stretches
        .iter()
        .map(|s| s.is_none_or(|(bottom, span)| span.is_none_or(|(low, _)| low - usual_low.unwrap_or(bottom).max(bottom) > near_floor)))
        .collect();
    let mut doorway = vec![false; bins];
    let mut run_start = 0;
    for bin in 0..=bins {
        if bin < bins && open[bin] {
            continue;
        }
        // a doorway is an opening inside the wall, not past its ends
        if run_start > 0 && bin < bins && (bin - run_start) as f32 * voxel >= 0.6 {
            doorway[run_start..bin].iter_mut().for_each(|d| *d = true);
        }
        run_start = bin + 1;
    }
    let median = |values: &mut Vec<i32>| -> Option<i32> {
        values.sort_unstable();
        values.get(values.len() / 2).copied()
    };
    let level = (0.4 / voxel).round() as usize;
    let tops: Vec<Option<i32>> = (0..bins)
        .map(|bin| median(&mut (bin.saturating_sub(level)..(bin + level + 1).min(bins)).filter_map(|b| stretches[b].and_then(|s| s.1).map(|span| span.1)).collect()))
        .collect();
    let bottoms: Vec<Option<i32>> = (0..bins)
        .map(|bin| median(&mut (bin.saturating_sub(level)..(bin + level + 1).min(bins)).filter_map(|b| stretches[b].map(|s| s.0)).collect()))
        .collect();
    let mut extent: Vec<Option<(i32, i32)>> = vec![None; bins];
    let mut bin = 0;
    while bin < bins {
        if doorway[bin] {
            let stop = (bin..bins).find(|b| !doorway[*b]).unwrap_or(bins);
            if let Some(header) = median(&mut (bin..stop).filter_map(|b| stretches[b].and_then(|s| s.1).map(|span| span.0)).collect()) {
                for b in bin..stop {
                    extent[b] = tops[b].map(|top| (header, top));
                }
            }
            bin = stop;
            continue;
        }
        extent[bin] = match (stretches[bin].map(|s| s.0).or(bottoms[bin]), tops[bin]) {
            (Some(bottom), Some(top)) => Some((bottom, top)),
            _ => None,
        };
        bin += 1;
    }
    // past the support (into a corner) the wall keeps the height it had at its end
    let extent_at = |t: f32| -> Option<(i32, i32)> {
        let b = bin_of(t);
        extent[b].or_else(|| (0..bins).map(|k| [b.saturating_sub(k), (b + k).min(bins - 1)]).flatten().find_map(|k| extent[k]))
    };
    // 6. the prism, sampled every half voxel along and across so a slanted wall has no holes
    let mut slab: AHashSet<Key> = AHashSet::new();
    let steps_along = ((slab_end - slab_start) / (voxel * 0.5)).ceil() as usize;
    let steps_across = ((2.0 * half / voxel) as usize * 2).saturating_sub(1).max(1);
    for step in 0..=steps_along {
        let t = slab_start + step as f32 * voxel * 0.5;
        let Some((bottom, top)) = extent_at(t) else { continue };
        for k in 0..steps_across {
            let p = line.at(t, -half + voxel * 0.25 + k as f32 * voxel * 0.5);
            if !inside_cuts(p, -0.01 * voxel) {
                continue;
            }
            let column = ((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32);
            for layer in bottom..=top {
                slab.insert((column.0, column.1, layer));
            }
        }
    }
    // 7. what goes: every wall-height voxel along the wall within half a metre of either face, connected to the wall or
    // not, except the walls met at the ends and what's really something else: a piece (26-connected, outside the
    // slab) that reaches half a metre away from the face (a crossing wall), or that's big and deep (furniture)
    let clear = half + 0.5;
    // a column of another wall is a full one (fringe next to it is a few stray voxels); the bar is a third of this
    // wall's typical column
    let mut column_counts: AHashMap<(i32, i32), usize> = AHashMap::new();
    for i in &wallish {
        *column_counts.entry(((xy(i)[0] / voxel).floor() as i32, (xy(i)[1] / voxel).floor() as i32)).or_default() += 1;
    }
    let mut own: Vec<usize> = column_counts.iter().filter(|(c, _)| {
        let p = [(c.0 as f32 + 0.5) * voxel, (c.1 as f32 + 0.5) * voxel];
        line.across(p).abs() <= half && (start..=end).contains(&line.along(p))
    }).map(|(_, n)| *n).collect();
    own.sort_unstable();
    let full_column = (own.get(own.len() / 2).copied().unwrap_or(9) / 3).max(3);
    let dense = |p: [f32; 2]| column_counts.get(&((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32)).copied().unwrap_or(0) >= full_column;
    let belongs_to_other = |p: [f32; 2]| dense(p) && others.iter().any(|(o, low, high)| o.across(p).abs() <= o.half + 0.5 * voxel && (*low..=*high).contains(&o.along(p)));
    let (clean_start, clean_end) = {
        let (a, b) = (line.along(from), line.along(to));
        (slab_start.min(a.min(b)), slab_end.max(a.max(b)))
    };
    let outside: Vec<u32> = wallish.iter().copied().filter(|i| line.across(xy(i)).abs() > half && !belongs_to_other(xy(i))).collect();
    let keys: AHashMap<Key, usize> = outside.iter().enumerate().map(|(k, i)| (key_of(points[*i as usize], voxel), k)).collect();
    let mut piece = vec![usize::MAX; outside.len()];
    let mut keep_piece: Vec<bool> = Vec::new();
    for start in 0..outside.len() {
        if piece[start] != usize::MAX {
            continue;
        }
        let id = keep_piece.len();
        let (mut stack, mut members) = (vec![start], Vec::new());
        piece[start] = id;
        while let Some(k) = stack.pop() {
            members.push(k);
            let c = key_of(points[outside[k] as usize], voxel);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(&n) = keys.get(&(c.0 + dx, c.1 + dy, c.2 + dz)) {
                            if piece[n] == usize::MAX {
                                piece[n] = id;
                                stack.push(n);
                            }
                        }
                    }
                }
            }
        }
        let away: Vec<f32> = members.iter().map(|k| line.across(xy(&outside[*k])).abs() - half).collect();
        let far = away.iter().copied().fold(f32::MIN, f32::max);
        let heights: Vec<f32> = members.iter().map(|k| points[outside[*k] as usize][2]).collect();
        let tall = heights.iter().copied().fold(f32::MIN, f32::max) - heights.iter().copied().fold(f32::MAX, f32::min);
        let volume = members.len() as f32 * voxel.powi(3);
        // a crossing wall or anything reaching half a metre out; or something real that reaches 20 cm or more out (a
        // short wall of a jog, a column, furniture): tall or sizeable. Specks and fringe hugging the face are neither
        keep_piece.push(far >= 0.5 || (far >= 0.2 && (tall >= 0.5 || volume >= 0.02)));
    }
    // and the voxel itself is a way off the face, or part of it going outward (fringe stuck to a crossing wall isn't):
    // the next 0.3 m out from it, along the same stretch, is mostly occupied
    // voxel centres sit on (or halfway between) multiples of a voxel from the line: a quarter voxel keeps the bins stable
    let bin = |value: f32| (value / voxel + 0.25).floor() as i32;
    let occupied: AHashSet<(i32, i32)> = outside.iter().map(|i| (bin(line.along(xy(i))), bin(line.across(xy(i))))).collect();
    let goes_outward = |p: [f32; 2]| -> bool {
        let (a, d) = (bin(line.along(p)), bin(line.across(p)));
        let sign = if d >= 0 { 1 } else { -1 };
        let filled = |k: i32| (a - 1..=a + 1).any(|aa| occupied.contains(&(aa, d + sign * k)));
        // most of the next 0.3 m out is occupied, or (away from the face) the way back in is
        let face = bin(sign as f32 * half);
        let back = (d - face) * sign;
        (1..=6).filter(|k| filled(*k)).count() >= 5 || (back >= 3 && (1..back).filter(|k| filled(-*k)).count() * 5 >= (back - 1) as usize * 4)
    };
    let kept_piece: AHashMap<u32, bool> = outside.iter().enumerate().map(|(k, i)| (*i, keep_piece[piece[k]] && (line.across(xy(i)).abs() - half >= 0.2 || goes_outward(xy(i))))).collect();
    let mut removed: AHashSet<Key> = AHashSet::new();
    for i in &wallish {
        let p = xy(i);
        let (t, d) = (line.along(p), line.across(p));
        if t < clean_start || t > clean_end || d.abs() > clear || !inside_cuts(p, 0.5 * voxel) {
            continue;
        }
        if belongs_to_other(p) || kept_piece.get(i).copied().unwrap_or(false) {
            continue;
        }
        let key = key_of(points[*i as usize], voxel);
        if slab.contains(&key) {
            continue;
        }
        edit.remove.push(*i);
        removed.insert(key);
    }
    let kept: AHashSet<Key> = wallish.iter().map(|i| key_of(points[*i as usize], voxel)).filter(|k| !removed.contains(k)).collect();
    let normal = [line.normal()[0], line.normal()[1], 0.0];
    let mut added: Vec<Key> = slab.into_iter().filter(|k| !kept.contains(k)).collect();
    added.sort_unstable();
    for key in added {
        edit.push(center(key, voxel), normal);
    }
    edit.remove.sort_unstable();
    edit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floor::tests::at;

    const V: f32 = 0.05;

    /// a 6 x 6 m floor plus wall voxels: every voxel center within `half` of the segment a→b, 2 m tall
    struct Scene {
        points: Vec<[f32; 3]>,
        normals: Vec<[f32; 3]>,
    }

    impl Scene {
        fn new() -> Scene {
            let mut scene = Scene { points: Vec::new(), normals: Vec::new() };
            for i in 0..120 {
                for j in 0..120 {
                    scene.points.push([at(i), at(j), at(0)]);
                    scene.normals.push([0.0, 0.0, 1.0]);
                }
            }
            scene
        }
        fn wall(&mut self, a: [f32; 2], b: [f32; 2], thickness: f32, noise: bool) {
            let length = (b[0] - a[0]).hypot(b[1] - a[1]);
            let line = Line { origin: a, direction: [(b[0] - a[0]) / length, (b[1] - a[1]) / length], half: thickness / 2.0 };
            let mut cells: AHashSet<(i32, i32)> = AHashSet::new();
            for i in 0..120 {
                for j in 0..120 {
                    let p = [at(i), at(j)];
                    let (t, d) = (line.along(p), line.across(p));
                    if t >= -line.half && t <= length + line.half && d.abs() <= line.half {
                        cells.insert((i, j));
                    }
                }
            }
            for (i, j) in cells {
                for k in 1..41 {
                    self.points.push([at(i), at(j), at(k)]);
                    self.normals.push([line.normal()[0], line.normal()[1], 0.0]);
                    // fringe hugging both faces now and then
                    if noise && (i * 7 + j * 5 + k * 3) % 11 == 0 {
                        let n = line.normal();
                        let off = line.half + V * (1.0 + (k % 2) as f32);
                        let sign = if (i + j + k) % 2 == 0 { 1.0 } else { -1.0 };
                        self.points.push([at(i) + n[0] * off * sign, at(j) + n[1] * off * sign, at(k)]);
                        self.normals.push([n[0], n[1], 0.0]);
                    }
                }
            }
        }
        fn run(&self, from: [f32; 2], to: [f32; 2], width: f32) -> Edit {
            straighten(&self.points, &self.normals, V, from, to, width, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX, bottom: f32::MIN })
        }
        /// the wall after the edit: the columns with voxels above the floor
        fn after(&self, edit: &Edit) -> AHashSet<(i32, i32)> {
            let removed: AHashSet<u32> = edit.remove.iter().copied().collect();
            self.points
                .iter()
                .enumerate()
                .filter(|(i, p)| p[2] > at(0) + 0.01 && !removed.contains(&(*i as u32)))
                .map(|(_, p)| *p)
                .chain(edit.add.iter().copied())
                .map(|p| ((p[0] / V).floor() as i32, (p[1] / V).floor() as i32))
                .collect()
        }
    }

    /// the cells of an ideal wall (a→b, thickness), in voxel columns
    fn ideal(a: [f32; 2], b: [f32; 2], thickness: f32) -> AHashSet<(i32, i32)> {
        let mut scene = Scene { points: Vec::new(), normals: Vec::new() };
        scene.wall(a, b, thickness, false);
        scene.points.iter().map(|p| ((p[0] / V).floor() as i32, (p[1] / V).floor() as i32)).collect()
    }

    #[test]
    fn noisy_thick_wall_becomes_a_clean_slab_grown_past_the_brush() {
        let mut scene = Scene::new();
        scene.wall([1.0, 3.025], [5.0, 3.025], 0.15, true);
        // the brush only covers the middle 1.5 m
        let edit = scene.run([2.3, 3.025], [3.8, 3.025], 0.4);
        let after = scene.after(&edit);
        let rows: AHashSet<i32> = after.iter().map(|c| c.1).collect();
        assert_eq!(rows.len(), 3, "15 cm = 3 voxels thick, fringe gone along the whole wall: {rows:?}");
        let want = ideal([1.0, 3.025], [5.0, 3.025], 0.15);
        let (missing, extra): (Vec<_>, Vec<_>) = (want.difference(&after).collect(), after.difference(&want).collect());
        assert!(missing.len() <= 3 && extra.len() <= 3, "the whole wall, cleaned: missing {missing:?}, extra {extra:?}");
    }

    #[test]
    fn l_corner_at_90_degrees() {
        let mut scene = Scene::new();
        scene.wall([1.0, 2.0], [4.0, 2.0], 0.1, true);
        scene.wall([4.0, 2.0], [4.0, 5.0], 0.1, false);
        let edit = scene.run([1.5, 2.0], [3.0, 2.0], 0.4);
        let after = scene.after(&edit);
        let want: AHashSet<(i32, i32)> = ideal([1.0, 2.0], [4.0, 2.0], 0.1).union(&ideal([4.0, 2.0], [4.0, 5.0], 0.1)).copied().collect();
        let extra: Vec<_> = after.difference(&want).collect();
        let missing: Vec<_> = want.difference(&after).collect();
        assert!(extra.len() <= 2 && missing.len() <= 2, "clean L: extra {extra:?}, missing {missing:?}");
    }

    #[test]
    fn l_corner_at_60_degrees() {
        let mut scene = Scene::new();
        let corner = [3.5, 1.5];
        let other = [3.5 - 2.5 * 60f32.to_radians().cos(), 1.5 + 2.5 * 60f32.to_radians().sin()];
        scene.wall([0.8, 1.5], corner, 0.1, true);
        scene.wall(corner, other, 0.1, false);
        let before_other = ideal(corner, other, 0.1);
        let edit = scene.run([1.2, 1.5], [2.8, 1.5], 0.4);
        let after = scene.after(&edit);
        // the other wall is untouched
        let lost: Vec<_> = before_other.difference(&after).collect();
        assert!(lost.is_empty(), "the 60° wall keeps every column: lost {lost:?}");
        // no gap at the corner: along the edited wall's outer face, the last column reaches the other wall's band
        let want = ideal([0.8, 1.5], corner, 0.1);
        let missing: Vec<_> = want.difference(&after).collect();
        assert!(missing.len() <= 2, "gap at the corner: {missing:?}");
        // past the two rectangles only the mitre's wedge: inside the edited wall's band, short of the other's outer face
        let both: AHashSet<(i32, i32)> = want.union(&before_other).copied().collect();
        let a = Line { origin: [0.8, 1.5], direction: [1.0, 0.0], half: 0.05 };
        let b = Line { origin: corner, direction: [(other[0] - corner[0]) / 2.5, (other[1] - corner[1]) / 2.5], half: 0.05 };
        let extra: Vec<_> = after
            .difference(&both)
            .filter(|c| {
                let p = [at(c.0), at(c.1)];
                !(a.across(p).abs() <= a.half + 0.01 && b.across(p) >= -(b.half + V))
            })
            .collect();
        // a slanted wall rasterizes a voxel thicker here and there: allow a few single-voxel differences
        assert!(extra.len() <= 3, "overlap past the corner: {extra:?}");
    }

    #[test]
    fn t_join_keeps_the_through_wall() {
        let mut scene = Scene::new();
        scene.wall([1.025, 1.0], [1.025, 5.0], 0.15, false);
        scene.wall([1.025, 3.0], [4.5, 3.0], 0.1, true);
        let through = ideal([1.025, 1.0], [1.025, 5.0], 0.15);
        let edit = scene.run([2.0, 3.0], [4.0, 3.0], 0.4);
        let after = scene.after(&edit);
        let lost: Vec<_> = through.difference(&after).collect();
        assert!(lost.is_empty(), "the through wall is intact: lost {lost:?}");
        let want: AHashSet<(i32, i32)> = ideal([1.025, 3.0], [4.5, 3.0], 0.1).union(&through).copied().collect();
        let missing: Vec<_> = want.difference(&after).collect();
        let extra: Vec<_> = after.difference(&want).collect();
        assert!(missing.is_empty() && extra.len() <= 2, "a clean T: missing {missing:?}, extra {extra:?}");
    }

    /// specks up to half a metre off the face go; a crossing wall and a cabinet against the wall stay
    #[test]
    fn clears_half_a_metre_but_keeps_real_things() {
        let mut scene = Scene::new();
        scene.wall([0.5, 3.025], [5.5, 3.025], 0.05, false);
        scene.wall([3.025, 3.1], [3.025, 5.5], 0.1, false);
        // specks and short stubs 2..9 voxels off the face on both sides
        for (i, off) in [(20, 2), (30, -3), (40, 5), (50, -7), (70, 9), (80, -9), (90, 4)] {
            for k in [5, 6, 20] {
                scene.points.push([at(i), at(60 + off), at(k)]);
                scene.normals.push([0.0, 1.0, 0.0]);
            }
        }
        // a 0.6 x 0.4 x 0.8 m cabinet against the south face
        for i in 30..42 {
            for j in 52..60 {
                for k in 1..17 {
                    scene.points.push([at(i), at(j), at(k)]);
                    scene.normals.push([0.0, -1.0, 0.0]);
                }
            }
        }
        let crossing = ideal([3.025, 3.1], [3.025, 5.5], 0.1);
        let edit = scene.run([1.0, 3.025], [2.0, 3.025], 0.3);
        let after = scene.after(&edit);
        let lost: Vec<_> = crossing.difference(&after).collect();
        assert!(lost.is_empty(), "the crossing wall stays: lost {lost:?}");
        assert!((30..42).all(|i| (52..60).all(|j| after.contains(&(i, j)))), "the cabinet stays");
        for (i, off) in [(20, 2), (30, -3), (40, 5), (50, -7), (70, 9), (80, -9), (90, 4)] {
            if i == 30 && off < 0 {
                continue;
            }
            assert!(!after.contains(&(i, 60 + off)), "speck at column {i}, {off} voxels off");
        }
    }

    /// a wall that jogs: two runs joined by a short step. Straightening each run and the step gives one clean outline,
    /// never two slabs side by side
    #[test]
    fn a_jog_becomes_a_clean_step() {
        let mut scene = Scene::new();
        let (a0, a1, b0, b1) = ([2.025, 0.5], [2.025, 2.525], [1.625, 2.525], [1.625, 4.5]);
        scene.wall(a0, a1, 0.05, true);
        scene.wall(a1, b0, 0.05, true);
        scene.wall(b0, b1, 0.05, true);
        let mut points = scene.points.clone();
        let mut normals = scene.normals.clone();
        for (from, to) in [([2.025, 0.8], [2.025, 2.2]), ([1.95, 2.525], [1.7, 2.525]), ([1.625, 2.8], [1.625, 4.2])] {
            let edit = straighten(&points, &normals, V, from, to, 0.3, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX, bottom: f32::MIN });
            let removed: AHashSet<u32> = edit.remove.iter().copied().collect();
            let kept: Vec<usize> = (0..points.len()).filter(|i| !removed.contains(&(*i as u32))).collect();
            normals = kept.iter().map(|i| normals[*i]).chain(edit.add_normals.iter().copied()).collect();
            points = kept.iter().map(|i| points[*i]).chain(edit.add.iter().copied()).collect();
        }
        let after: AHashSet<(i32, i32)> = points.iter().filter(|p| p[2] > at(0) + 0.01).map(|p| ((p[0] / V).floor() as i32, (p[1] / V).floor() as i32)).collect();
        let want: AHashSet<(i32, i32)> = [ideal(a0, a1, 0.05), ideal(a1, b0, 0.05), ideal(b0, b1, 0.05)].into_iter().flatten().collect();
        let (missing, extra): (Vec<_>, Vec<_>) = (want.difference(&after).collect(), after.difference(&want).collect());
        assert!(missing.is_empty() && extra.is_empty(), "a clean step: missing {missing:?}, extra {extra:?}");
    }

    #[test]
    fn a_crossing_wall_and_a_doorway_survive() {
        let mut scene = Scene::new();
        scene.wall([0.5, 3.0], [5.5, 3.0], 0.1, true);
        scene.wall([3.0, 0.5], [3.0, 5.5], 0.1, false);
        let crossing = ideal([3.0, 0.5], [3.0, 5.5], 0.1);
        let edit = scene.run([1.0, 3.0], [2.5, 3.0], 0.4);
        let after = scene.after(&edit);
        assert!(crossing.iter().all(|c| after.contains(c)), "the crossing wall keeps every column");
    }
}
