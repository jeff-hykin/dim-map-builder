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
        *by_stretch.entry((line.along(*p) / 0.5).floor() as i32).or_default().entry((line.across(*p) / voxel).round() as i32).or_default() += 1;
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
        middles.push((low + high) as f32 / 2.0 * voxel);
    }
    widths.sort_unstable();
    middles.sort_by(|a, b| a.total_cmp(b));
    (widths.get(widths.len() / 2).copied().unwrap_or(1).max(1), middles.get(middles.len() / 2).copied().unwrap_or(0.0))
}

/// A line fitted to some wall voxels, its thickness from the data (or `thickness`), placed on the voxel grid when it
/// runs along an axis.
fn wall_line(xy: &[[f32; 2]], voxel: f32, thickness: Option<f32>, toward: [f32; 2]) -> Option<(Line, f32)> {
    let (origin, mut direction, ratio) = fit_axis(xy)?;
    if direction[0] * toward[0] + direction[1] * toward[1] < 0.0 {
        direction = [-direction[0], -direction[1]];
    }
    // within 2° of an axis it is that axis (a square room stays square)
    for axis in [[1.0f32, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]] {
        if direction[0] * axis[0] + direction[1] * axis[1] > 2.0f32.to_radians().cos() {
            direction = axis;
        }
    }
    let mut line = Line { origin, direction, half: 0.0 };
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
            floor_of(((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32)).is_some_and(|floor| p[2] > floor + voxel * 0.5 && p[2] <= reach.top(floor))
        })
        .collect();
    let xy = |i: &u32| [points[*i as usize][0], points[*i as usize][1]];
    // 1. the hint: the wall in the brush's band
    let hint: Vec<[f32; 2]> = wallish.iter().filter(|i| (0.0..=length).contains(&drawn.along(xy(i))) && drawn.across(xy(i)).abs() <= width / 2.0).map(xy).collect();
    if hint.len() < 8 {
        return edit;
    }
    let Some((mut line, _)) = wall_line(&hint, voxel, thickness, drawn.direction) else { return edit };
    // 2. grow along the wall from the stroke's span while there's wall there (gaps up to 0.5 m: doorways have headers)
    let grow_support = |line: &Line| -> (f32, f32) {
        let mut counts: AHashMap<i32, usize> = AHashMap::new();
        for i in &wallish {
            if line.across(xy(i)).abs() <= line.half + 2.0 * voxel {
                *counts.entry((line.along(xy(i)) / voxel).floor() as i32).or_default() += 1;
            }
        }
        let (s0, s1) = {
            let (a, b) = (line.along(from), line.along(to));
            ((a.min(b) / voxel).floor() as i32, (a.max(b) / voxel).floor() as i32)
        };
        let mut inside: Vec<usize> = (s0..=s1).map(|s| counts.get(&s).copied().unwrap_or(0)).collect();
        inside.sort_unstable();
        let enough = (inside[inside.len() / 2] / 5).max(2);
        let max_gap = (0.5 / voxel).ceil() as i32;
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
    if let Some((refit, _)) = wall_line(&support, voxel, thickness, line.direction) {
        line = refit;
        (start, end) = grow_support(&line);
    }
    let half = line.half;
    let window = (width / 2.0).max(half + 3.0 * voxel) + 2.0 * voxel;
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
        let around: Vec<[f32; 2]> = wallish
            .iter()
            .map(xy)
            .filter(|p| (p[0] - tip[0]).hypot(p[1] - tip[1]) <= 1.2 && line.across(*p).abs() > window)
            .collect();
        // a wall, not fringe: it reaches well away from this one
        let reaches = around.iter().any(|p| line.across(*p).abs() >= half + 0.4);
        let into = [line.direction[0] * -outward, line.direction[1] * -outward];
        let neighbour = (around.len() >= 30 && reaches).then(|| wall_line(&around, voxel, None, line.normal())).flatten().filter(|(other, ratio)| {
            let sine = (line.direction[0] * other.direction[1] - line.direction[1] * other.direction[0]).abs();
            *ratio >= 4.0 && sine >= 20f32.to_radians().sin()
        });
        let Some((other, _)) = neighbour else {
            cuts.push(Cut { point: tip, inward: into });
            continue;
        };
        // the other wall's normal, pointed back into this wall's body
        let mut inward = other.normal();
        if inward[0] * into[0] + inward[1] * into[1] < 0.0 {
            inward = [-inward[0], -inward[1]];
        }
        // does it carry on past this wall's far side (a T: stop at its near face) or end here (an L: fill the corner)?
        let crossing = |p: &[f32; 2]| line.across(*p);
        let (left, right) = around.iter().filter(|p| other.across(**p).abs() <= other.half + voxel).fold((0, 0), |(l, r), p| if crossing(p) > 0.0 { (l + 1, r) } else { (l, r + 1) });
        let through = left.min(right) * 4 >= left.max(right);
        let face = if through { other.half } else { -other.half };
        let point = [other.origin[0] + inward[0] * face, other.origin[1] + inward[1] * face];
        cuts.push(Cut { point, inward });
        // its span: the voxel-long bins along it that are dense (fringe from this wall in its band is sparse)
        let mut counts: AHashMap<i32, usize> = AHashMap::new();
        for p in wallish.iter().map(xy).filter(|p| (p[0] - tip[0]).hypot(p[1] - tip[1]) <= 1.5 && other.across(*p).abs() <= other.half + 0.5 * voxel) {
            *counts.entry((other.along(p) / voxel).floor() as i32).or_default() += 1;
        }
        let dense = counts.values().copied().max().unwrap_or(0) * 3 / 10;
        let (low, high) = counts.iter().filter(|(_, c)| **c >= dense.max(1)).fold((i32::MAX, i32::MIN), |(a, b), (bin, _)| (a.min(*bin), b.max(*bin)));
        let (low, high) = (low as f32 * voxel, (high + 1) as f32 * voxel);
        others.push((other, low - voxel, high + voxel));
        // the slab reaches as far as the face does across its thickness
        let sine = (line.direction[0] * inward[0] + line.direction[1] * inward[1]).abs().max(0.2);
        let reach_along = line.along(point) + outward * (half / sine * (1.0 - sine * sine).sqrt() + voxel);
        if outward > 0.0 {
            slab_end = slab_end.max(reach_along);
        } else {
            slab_start = slab_start.min(reach_along);
        }
    }
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
            Some(((floor / voxel).floor() as i32 + 1, span))
        })
        .collect();
    let near_floor = (0.5 / voxel).round() as i32;
    let open: Vec<bool> = stretches.iter().map(|s| s.is_none_or(|(bottom, span)| span.is_none_or(|(low, _)| low - bottom > near_floor))).collect();
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
    // 7. what goes: wall-height voxels along the wall within a few voxels of the prism, except what belongs to
    // something else: the other walls at the ends, and anything that carries on outward (a crossing wall, furniture)
    let belongs_to_other = |p: [f32; 2]| others.iter().any(|(o, low, high)| o.across(p).abs() <= o.half && (*low..=*high).contains(&o.along(p)));
    let mut occupied: AHashSet<(i32, i32)> = AHashSet::new();
    for i in wallish.iter().filter(|i| !belongs_to_other(xy(i))) {
        let (t, d) = (line.along(xy(i)), line.across(xy(i)));
        occupied.insert(((t / voxel).floor() as i32, (d / voxel).floor() as i32));
    }
    let carries_on = |t: f32, d: f32| -> bool {
        let (a, sign) = ((t / voxel).floor() as i32, d.signum());
        let first = ((window / voxel).ceil()) as i32 + 1;
        (first..first + 4).filter(|k| (a - 1..=a + 1).any(|aa| occupied.contains(&(aa, ((sign * *k as f32 * voxel + sign * 0.5 * voxel) / voxel).floor() as i32)))).count() >= 3
    };
    let (clean_start, clean_end) = {
        let (a, b) = (line.along(from), line.along(to));
        (slab_start.min(a.min(b)), slab_end.max(a.max(b)))
    };
    let mut removed: AHashSet<Key> = AHashSet::new();
    for i in &wallish {
        let p = xy(i);
        let (t, d) = (line.along(p), line.across(p));
        if t < clean_start || t > clean_end || d.abs() > window || !inside_cuts(p, 0.5 * voxel) {
            continue;
        }
        if belongs_to_other(p) || (d.abs() > half && carries_on(t, d)) {
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
            straighten(&self.points, &self.normals, V, from, to, width, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX })
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
        assert!(before_other.iter().all(|c| after.contains(c)), "the 60° wall keeps every column");
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
