//! Straighten a wall: fit one straight slab to the wall under the brush and force the wall's voxels into it. The fit may
//! read half a metre around the brush (a wall's angle and thickness show better over more of it), but every voxel
//! removed or added is inside the brush's band. A noisy band, or a wall scanned twice a few cm apart, becomes ONE slab
//! of the wall's own thickness (whole voxels, measured per 10 cm along it, so jitter doesn't widen it) at the median
//! position; everything else wall-high in the band goes, except what carries on outward past the band's side (a
//! crossing wall, a cabinet against the wall). A free end stops at the wall's last voxel; doorways keep their header.
use crate::edit::{center, Edit, Reach};
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

/// The wall's core across its line, per 10 cm stretch (so a jittering wall doesn't read wider than it is): the run of voxel-wide bins around the fullest with at least half
/// its count. Returns (thickness in voxels, the core's middle across), medians over the stretches.
fn core(xy: &[[f32; 2]], line: &Line, voxel: f32) -> (i32, f32) {
    let mut by_stretch: AHashMap<i32, AHashMap<i32, usize>> = AHashMap::new();
    for p in xy {
        *by_stretch.entry((line.along(*p) / 0.1).floor() as i32).or_default().entry((line.across(*p) / voxel).floor() as i32).or_default() += 1;
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

/// See the module docs. The selection is the brush's band: `width` wide along from → to. `reach` caps how high the
/// wall is rebuilt and cleaned.
#[allow(clippy::too_many_arguments)]
pub fn straighten(points: &[[f32; 3]], _normals: &[[f32; 3]], voxel: f32, from: [f32; 2], to: [f32; 2], width: f32, thickness: Option<f32>, floor_at: impl Fn(f32, f32) -> Option<f32>, reach: Reach) -> Edit {
    let mut edit = Edit::default();
    let length = (to[0] - from[0]).hypot(to[1] - from[1]);
    if length < voxel {
        return edit;
    }
    let drawn = Line { origin: from, direction: [(to[0] - from[0]) / length, (to[1] - from[1]) / length], half: width / 2.0 };
    // the selection, by a voxel column's centre: nothing outside it is ever removed or added
    let selected = |column: (i32, i32)| {
        let p = [(column.0 as f32 + 0.5) * voxel, (column.1 as f32 + 0.5) * voxel];
        (0.0..=length).contains(&drawn.along(p)) && drawn.across(p).abs() <= drawn.half
    };
    let column_of = |p: [f32; 3]| ((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32);
    // what's read: the selection and `margin` around it (to fit the wall, find floors and see what carries on outward)
    let margin = 0.5;
    let near: Vec<u32> = (0..points.len() as u32)
        .filter(|i| {
            let p = points[*i as usize];
            let (t, d) = (drawn.along([p[0], p[1]]), drawn.across([p[0], p[1]]));
            t >= -margin && t <= length + margin && d.abs() <= drawn.half + margin
        })
        .collect();
    let occupied: AHashSet<Key> = near.iter().map(|i| key_of(points[*i as usize], voxel)).collect();
    // for finding floors, the wall under the brush doesn't count (it's about to be rebuilt; its foot isn't floor)
    let around_floor: AHashSet<Key> = near.iter().filter(|i| !selected(column_of(points[**i as usize]))).map(|i| key_of(points[*i as usize], voxel)).collect();
    let mut floors: AHashMap<(i32, i32), Option<f32>> = AHashMap::new();
    let mut floor_of = |column: (i32, i32)| -> Option<f32> {
        *floors.entry(column).or_insert_with(|| {
            let (x, y) = ((column.0 as f32 + 0.5) * voxel, (column.1 as f32 + 0.5) * voxel);
            floor_at(x, y).map(|estimate| {
                // the column's own walkable surface: the highest layer within 0.3 m of the model's floor where most of
                // the 3 x 3 columns around are occupied and little just above it (a landing or tread the 25 cm model sits a few cm off)
                let (low, high) = (((estimate - 0.3) / voxel).floor() as i32, ((estimate + 0.3) / voxel).floor() as i32);
                (low..=high)
                    .rev()
                    .find(|layer| {
                        let count = |l: i32| (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))).filter(|(dx, dy)| around_floor.contains(&(column.0 + dx, column.1 + dy, l))).count();
                        count(*layer) >= 7 && count(layer + 1) <= 3
                    })
                    .map_or(estimate, |layer| (layer as f32 + 0.5) * voxel)
            })
        })
    };
    // the wall-height voxels: above their column's floor and under the reach
    let wallish: Vec<u32> = near
        .iter()
        .copied()
        .filter(|i| {
            let p = points[*i as usize];
            p[2] >= reach.bottom && floor_of(column_of(p)).is_some_and(|floor| p[2] > floor + voxel * 0.5 && p[2] <= reach.top(floor))
        })
        .collect();
    let xy = |i: &u32| [points[*i as usize][0], points[*i as usize][1]];
    // 1. the line: fitted to the wall in the selection (refit below on the wall around it too)
    let hint: Vec<[f32; 2]> = wallish.iter().filter(|i| selected(column_of(points[**i as usize]))).map(xy).collect();
    if hint.len() < 8 {
        return edit;
    }
    let snap = if length < 1.5 { 10.0 } else { 2.0 };
    let Some((mut line, _)) = wall_line_snapped(&hint, voxel, thickness, drawn.direction, snap) else { return edit };
    // the stroke says which way the wall runs: a fit more than 20° off it (a short stroke over mixed data) is overruled
    // (a stroke under a metre is all over a short wall: its direction is the stroke's)
    let forced = length < 1.0 || (line.direction[0] * drawn.direction[0] + line.direction[1] * drawn.direction[1]).abs() < 20f32.to_radians().cos();
    if forced {
        let Some((along_stroke, _)) = wall_line_along(&hint, voxel, thickness, drawn.direction) else { return edit };
        line = along_stroke;
    }
    // the stroke's span on the line
    let (start, end) = {
        let (a, b) = (line.along(from), line.along(to));
        (a.min(b), a.max(b))
    };
    // refit on the wall's band through the whole read area (a few metres say more about its angle than the stroke)
    let support: Vec<[f32; 2]> = wallish.iter().map(xy).filter(|p| (start - margin..=end + margin).contains(&line.along(*p)) && line.across(*p).abs() <= line.half + 2.0 * voxel).collect();
    let refit = if forced { wall_line_along(&support, voxel, thickness, line.direction) } else { wall_line_snapped(&support, voxel, thickness, line.direction, snap) };
    if let Some((refit, _)) = refit.filter(|(r, _)| (r.direction[0] * line.direction[0] + r.direction[1] * line.direction[1]).abs() >= 20f32.to_radians().cos()) {
        line = refit;
    }
    // never thicker than the selection
    line.half = line.half.min((drawn.half / voxel).floor().max(0.5) * voxel);
    let half = line.half;
    let (start, end) = {
        let (a, b) = (line.along(from), line.along(to));
        (a.min(b), a.max(b))
    };
    // 2. a free end stops at the wall's last voxel in the selection (its core, not fringe)
    let core_along: Vec<f32> = wallish.iter().map(xy).filter(|p| line.across(*p).abs() <= half && (start..=end).contains(&line.along(*p))).map(|p| line.along(p)).collect();
    if core_along.is_empty() {
        return edit;
    }
    let (data_start, data_end) = core_along.iter().fold((f32::MAX, f32::MIN), |(a, b), t| (a.min(*t), b.max(*t)));
    let (slab_start, slab_end) = ((data_start - voxel * 0.5).max(start), (data_end + voxel * 0.5).min(end));
    // 3. heights along the wall, per voxel-long bin of the span: floor to a level top, doorways under a header
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
    let extent_at = |t: f32| -> Option<(i32, i32)> {
        let b = bin_of(t);
        extent[b].or_else(|| (0..bins).flat_map(|k| [b.saturating_sub(k), (b + k).min(bins - 1)]).find_map(|k| extent[k]))
    };
    // 4. the slab, sampled every half voxel along and across so a slanted wall has no holes; only in the selection
    let mut slab: AHashSet<Key> = AHashSet::new();
    let steps_along = ((slab_end - slab_start) / (voxel * 0.5)).ceil() as usize;
    let steps_across = ((2.0 * half / voxel).round() as usize * 2).saturating_sub(1).max(1);
    for step in 0..=steps_along {
        let t = (slab_start + step as f32 * voxel * 0.5).min(slab_end - 0.01 * voxel).max(slab_start + 0.01 * voxel);
        let Some((bottom, top)) = extent_at(t) else { continue };
        for k in 0..steps_across {
            let p = line.at(t, -half + voxel * 0.25 + k as f32 * voxel * 0.5);
            let column = ((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32);
            if !selected(column) {
                continue;
            }
            for layer in bottom..=top {
                slab.insert((column.0, column.1, layer));
            }
        }
    }
    // 5. what stays beside the slab: something that carries on outward past the selection's side (a crossing wall at
    // any angle down to 45°, a cabinet against the wall): the voxel's column is a full one, and a ray from it outward
    // (straight out or up to 45° either way) finds full columns in nearly all of the 15 cm past the band's side. Fringe, specks and a second
    // scan of the same wall a few cm off run along it inside the selection, so they go.
    let mut column_counts: AHashMap<(i32, i32), usize> = AHashMap::new();
    for i in &wallish {
        *column_counts.entry(column_of(points[*i as usize])).or_default() += 1;
    }
    // a structure's column is a full one (a speck is a voxel or two): a quarter of the wall's typical column
    let mut typical: Vec<usize> = column_counts.iter().filter(|(c, _)| selected(**c) && line.across([(c.0 as f32 + 0.5) * voxel, (c.1 as f32 + 0.5) * voxel]).abs() <= half).map(|(_, n)| *n).collect();
    typical.sort_unstable();
    let full = (typical.get(typical.len() / 2).copied().unwrap_or(8) / 4).max(3);
    let is_full = |q: [f32; 2]| column_counts.get(&((q[0] / voxel).floor() as i32, (q[1] / voxel).floor() as i32)).copied().unwrap_or(0) >= full;
    let mut carries_on: AHashMap<(i32, i32), bool> = AHashMap::new();
    let mut carries_on_at = |column: (i32, i32)| -> bool {
        *carries_on.entry(column).or_insert_with(|| {
            let p = [(column.0 as f32 + 0.5) * voxel, (column.1 as f32 + 0.5) * voxel];
            if !is_full(p) {
                return false;
            }
            let side = if drawn.across(p) >= 0.0 { 1.0 } else { -1.0 };
            let out = [drawn.normal()[0] * side, drawn.normal()[1] * side];
            (-3..=3).any(|step| {
                let angle = step as f32 * 15f32.to_radians();
                let (c, s) = (angle.cos(), angle.sin());
                let ray = [out[0] * c - out[1] * s, out[0] * s + out[1] * c];
                let (mut samples, mut hits, mut k) = (0, 0, 1);
                loop {
                    let q = [p[0] + ray[0] * k as f32 * voxel * 0.5, p[1] + ray[1] * k as f32 * voxel * 0.5];
                    let past = drawn.across(q) * side - drawn.half;
                    if past > 0.15 || k > 400 {
                        break;
                    }
                    if past > 0.0 {
                        samples += 1;
                        hits += usize::from(is_full(q));
                    }
                    k += 1;
                }
                samples > 0 && hits * 5 >= samples * 4
            })
        })
    };
    // the voxel just over a column's floor, in a layer spread around it, is the floor's own thickness: it stays
    let floor_layer = |i: &u32| {
        let p = points[*i as usize];
        let k = key_of(p, voxel);
        let spread = |layer: i32| (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))).filter(|(dx, dy)| occupied.contains(&(k.0 + dx, k.1 + dy, layer))).count();
        spread(k.2) >= 5 && spread(k.2 + 1) <= 3 && floors.get(&(k.0, k.1)).copied().flatten().is_some_and(|floor| p[2] <= floor + 1.5 * voxel)
    };
    let mut removed: AHashSet<Key> = AHashSet::new();
    for i in &wallish {
        let p = points[*i as usize];
        let key = key_of(p, voxel);
        if !selected((key.0, key.1)) || slab.contains(&key) {
            continue;
        }
        if line.across(xy(i)).abs() > half && (carries_on_at((key.0, key.1)) || floor_layer(i)) {
            continue;
        }
        edit.remove.push(*i);
        removed.insert(key);
    }
    let kept: AHashSet<Key> = wallish.iter().map(|i| key_of(points[*i as usize], voxel)).filter(|k| !removed.contains(k)).collect();
    let normal = [line.normal()[0], line.normal()[1], 0.0];
    let mut added: Vec<Key> = slab.into_iter().filter(|k| !kept.contains(k) && !occupied.contains(k)).collect();
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

    /// is a voxel column in the brush's band (by its centre, as `straighten` decides)
    fn in_band(c: (i32, i32), from: [f32; 2], to: [f32; 2], width: f32) -> bool {
        let length = (to[0] - from[0]).hypot(to[1] - from[1]);
        let drawn = Line { origin: from, direction: [(to[0] - from[0]) / length, (to[1] - from[1]) / length], half: width / 2.0 };
        let p = [(c.0 as f32 + 0.5) * V, (c.1 as f32 + 0.5) * V];
        (0.0..=length).contains(&drawn.along(p)) && drawn.across(p).abs() <= drawn.half
    }

    /// every voxel the edit removes or adds is in the brush's band
    fn assert_clipped(points: &[[f32; 3]], edit: &Edit, from: [f32; 2], to: [f32; 2], width: f32) {
        let inside = |p: [f32; 3]| in_band(((p[0] / V).floor() as i32, (p[1] / V).floor() as i32), from, to, width);
        let outside: Vec<[f32; 3]> = edit.remove.iter().map(|i| points[*i as usize]).chain(edit.add.iter().copied()).filter(|p| !inside(*p)).collect();
        assert!(outside.is_empty(), "{} voxels changed outside the selection: {:?}", outside.len(), &outside[..outside.len().min(5)]);
    }

    /// a deterministic 0..1 hash of a cell, for jitter
    fn noise(i: i32, j: i32, k: i32) -> f32 {
        let h = (i as u32).wrapping_mul(73856093) ^ (j as u32).wrapping_mul(19349663) ^ (k as u32).wrapping_mul(83492791);
        (h.wrapping_mul(2654435761) >> 8) as f32 / (1u32 << 24) as f32
    }

    /// for a wall along x: per voxel column x of the selection (0.1 m in from its ends), the runs of wall-high rows in
    /// the band, and how many rows those are
    fn runs_across(scene: &Scene, edit: &Edit, from: [f32; 2], to: [f32; 2], width: f32) -> Vec<(usize, usize)> {
        let cells = scene.after(edit);
        let (x0, x1) = (((from[0].min(to[0]) + 0.1) / V).floor() as i32, ((from[0].max(to[0]) - 0.1) / V).floor() as i32);
        (x0..=x1)
            .map(|x| {
                let rows: Vec<i32> = (-200..200).filter(|r| in_band((x, *r), from, to, width) && cells.contains(&(x, *r))).collect();
                (rows.windows(2).filter(|w| w[1] != w[0] + 1).count() + usize::from(!rows.is_empty()), rows.len())
            })
            .collect()
    }

    /// Jeff's case: a lidar wall scanned twice a few cm apart (a jittering 2-voxel band, a ghost row 3 voxels off it,
    /// specks), with a perpendicular wall at its left end
    fn double_scanned_wall() -> Scene {
        let mut scene = Scene::new();
        scene.wall([1.025, 1.0], [1.025, 5.0], 0.1, false);
        for i in 22..95 {
            let jitter = (noise(i / 4, 1, 1) * 2.0) as i32;
            for k in 1..41 {
                for j in [60, 61] {
                    if noise(i, j, k) < 0.85 {
                        scene.points.push([at(i), at(j + jitter), at(k)]);
                        scene.normals.push([0.0, 1.0, 0.0]);
                    }
                }
                if noise(i, 64, k) < 0.6 {
                    scene.points.push([at(i), at(64), at(k)]);
                    scene.normals.push([0.0, 1.0, 0.0]);
                }
                if noise(i, 7, k) < 0.08 {
                    let off = 1 + (noise(k, i, 3) * 5.0) as i32;
                    let j = if noise(i, k, 9) < 0.5 { 59 - off } else { 65 + off };
                    scene.points.push([at(i), at(j), at(k)]);
                    scene.normals.push([0.0, 1.0, 0.0]);
                }
            }
        }
        scene
    }

    #[test]
    fn a_double_scanned_wall_becomes_one_wall_inside_the_selection() {
        let scene = double_scanned_wall();
        // over part of it, slightly slanted against it
        let (from, to, width) = ([1.6, 3.02], [3.9, 3.14], 0.4);
        let edit = scene.run(from, to, width);
        assert!(!edit.add.is_empty() && !edit.remove.is_empty());
        assert_clipped(&scene.points, &edit, from, to, width);
        let runs = runs_across(&scene, &edit, from, to, width);
        assert!(runs.iter().all(|(n, _)| *n == 1), "one wall in every slice: {runs:?}");
        assert!(runs.iter().all(|(_, cells)| (1..=3).contains(cells)), "a thin wall, 1–3 voxels across: {runs:?}");
        // one straight wall: the same rows from one end to the other
        let rows: AHashSet<i32> = scene.after(&edit).iter().filter(|c| in_band(**c, from, to, width)).map(|c| c.1).collect();
        assert!(rows.len() <= 3, "a straight wall: {rows:?}");
    }

    /// the regression: the stroke runs from inside the perpendicular wall to past the wall's far end
    #[test]
    fn a_stroke_into_a_corner_and_past_the_end_makes_one_wall_and_no_more() {
        let scene = double_scanned_wall();
        let perpendicular = ideal([1.025, 1.0], [1.025, 5.0], 0.1);
        let (from, to, width) = ([0.9, 3.05], [5.2, 3.08], 0.45);
        let edit = scene.run(from, to, width);
        assert_clipped(&scene.points, &edit, from, to, width);
        let after = scene.after(&edit);
        let lost: Vec<_> = perpendicular.difference(&after).collect();
        assert!(lost.is_empty(), "the perpendicular wall stays: lost {lost:?}");
        let runs = runs_across(&scene, &edit, from, to, width);
        let rows_by_col: std::collections::BTreeMap<i32, Vec<i32>> = after.iter().filter(|c| (30..40).contains(&c.0) && (52..72).contains(&c.1)).fold(Default::default(), |mut m: std::collections::BTreeMap<i32, Vec<i32>>, c| { m.entry(c.0).or_default().push(c.1); m });
        eprintln!("{rows_by_col:?} removed {} added {}", edit.remove.len(), edit.add.len());
        assert!(runs.iter().all(|(n, _)| *n <= 1), "never two or three walls: {runs:?}");
        // past the wall's end (x 4.75) nothing is added: the free end stops at its last voxel
        assert!(edit.add.iter().all(|p| p[0] < 4.8), "nothing added past the wall's end");
        let rows: AHashSet<i32> = after.iter().filter(|c| (25..94).contains(&c.0) && in_band(**c, from, to, width)).map(|c| c.1).collect();
        assert!(rows.len() <= 3, "one straight wall: {rows:?}");
    }

    #[test]
    fn noisy_thick_wall_becomes_a_clean_slab_within_the_brush() {
        let mut scene = Scene::new();
        scene.wall([1.0, 3.025], [5.0, 3.025], 0.15, true);
        // the brush only covers the middle 1.5 m: only that is cleaned
        let (from, to) = ([2.3, 3.025], [3.8, 3.025]);
        let edit = scene.run(from, to, 0.4);
        assert_clipped(&scene.points, &edit, from, to, 0.4);
        let before = scene.after(&Edit::default());
        let after = scene.after(&edit);
        let inside = |c: &(i32, i32)| in_band(*c, from, to, 0.4);
        let rows: AHashSet<i32> = after.iter().filter(|c| inside(c)).map(|c| c.1).collect();
        assert_eq!(rows.len(), 3, "15 cm = 3 voxels thick, fringe gone under the brush: {rows:?}");
        let want: AHashSet<(i32, i32)> = ideal([1.0, 3.025], [5.0, 3.025], 0.15).into_iter().filter(inside).collect();
        let got: AHashSet<(i32, i32)> = after.iter().copied().filter(inside).collect();
        assert_eq!(got, want, "a clean slab under the brush");
        let outside_before: AHashSet<(i32, i32)> = before.iter().copied().filter(|c| !inside(c)).collect();
        let outside_after: AHashSet<(i32, i32)> = after.iter().copied().filter(|c| !inside(c)).collect();
        assert_eq!(outside_before, outside_after, "past the brush the wall is as it was");
    }

    #[test]
    fn l_corner_at_90_degrees() {
        let mut scene = Scene::new();
        scene.wall([1.0, 2.0], [4.0, 2.0], 0.1, true);
        scene.wall([4.0, 2.0], [4.0, 5.0], 0.1, false);
        let other = ideal([4.0, 2.0], [4.0, 5.0], 0.1);
        let (from, to) = ([1.2, 2.0], [4.3, 2.0]);
        let edit = scene.run(from, to, 0.4);
        assert_clipped(&scene.points, &edit, from, to, 0.4);
        let after = scene.after(&edit);
        assert!(other.iter().all(|c| after.contains(c)), "the other wall stays");
        let band = |c: &(i32, i32)| (24..86).contains(&c.0) && (36..44).contains(&c.1);
        let want: AHashSet<(i32, i32)> = ideal([1.0, 2.0], [4.0, 2.0], 0.1).union(&other).copied().filter(band).collect();
        let got: AHashSet<(i32, i32)> = after.iter().copied().filter(band).collect();
        let (missing, extra): (Vec<_>, Vec<_>) = (want.difference(&got).collect(), got.difference(&want).collect());
        assert!(missing.len() <= 2 && extra.len() <= 2, "clean L: extra {extra:?}, missing {missing:?}");
    }

    #[test]
    fn l_corner_at_60_degrees() {
        let mut scene = Scene::new();
        let corner = [3.5, 1.5];
        let other = [3.5 - 2.5 * 60f32.to_radians().cos(), 1.5 + 2.5 * 60f32.to_radians().sin()];
        scene.wall([0.8, 1.5], corner, 0.1, true);
        scene.wall(corner, other, 0.1, false);
        let before_other = ideal(corner, other, 0.1);
        let (from, to) = ([1.0, 1.5], [3.6, 1.5]);
        let edit = scene.run(from, to, 0.4);
        assert_clipped(&scene.points, &edit, from, to, 0.4);
        let after = scene.after(&edit);
        let lost: Vec<_> = before_other.difference(&after).collect();
        assert!(lost.is_empty(), "the 60° wall keeps every column: lost {lost:?}");
        let band = |c: &(i32, i32)| (20..72).contains(&c.0) && (26..34).contains(&c.1);
        let want: AHashSet<(i32, i32)> = ideal([0.8, 1.5], corner, 0.1).union(&before_other).copied().filter(band).collect();
        let got: AHashSet<(i32, i32)> = after.iter().copied().filter(band).collect();
        let (missing, extra): (Vec<_>, Vec<_>) = (want.difference(&got).collect(), got.difference(&want).collect());
        // a slanted wall rasterizes a voxel thicker here and there: allow a few single-voxel differences
        assert!(missing.len() <= 2 && extra.len() <= 3, "clean corner: missing {missing:?}, extra {extra:?}");
    }

    #[test]
    fn t_join_keeps_the_through_wall() {
        let mut scene = Scene::new();
        scene.wall([1.025, 1.0], [1.025, 5.0], 0.15, false);
        scene.wall([1.025, 3.0], [4.5, 3.0], 0.1, true);
        let through = ideal([1.025, 1.0], [1.025, 5.0], 0.15);
        let (from, to) = ([0.9, 3.0], [4.4, 3.0]);
        let edit = scene.run(from, to, 0.4);
        assert_clipped(&scene.points, &edit, from, to, 0.4);
        let after = scene.after(&edit);
        let lost: Vec<_> = through.difference(&after).collect();
        assert!(lost.is_empty(), "the through wall is intact: lost {lost:?}");
        let band = |c: &(i32, i32)| (18..88).contains(&c.0) && (56..64).contains(&c.1);
        let want: AHashSet<(i32, i32)> = ideal([1.025, 3.0], [4.5, 3.0], 0.1).union(&through).copied().filter(band).collect();
        let got: AHashSet<(i32, i32)> = after.iter().copied().filter(band).collect();
        let (missing, extra): (Vec<_>, Vec<_>) = (want.difference(&got).collect(), got.difference(&want).collect());
        assert!(missing.is_empty() && extra.len() <= 2, "a clean T: missing {missing:?}, extra {extra:?}");
    }

    /// specks in the band go, specks outside it stay (never written); a crossing wall and a cabinet against the wall stay
    #[test]
    fn clears_the_band_but_keeps_real_things() {
        let mut scene = Scene::new();
        scene.wall([0.5, 3.025], [5.5, 3.025], 0.05, false);
        scene.wall([3.025, 3.1], [3.025, 5.5], 0.1, false);
        let specks = [(20, 2), (24, -3), (44, 2), (50, -7), (70, 9), (80, -9), (90, 3)];
        for (i, off) in specks {
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
        let (from, to) = ([0.8, 3.025], [5.2, 3.025]);
        let edit = scene.run(from, to, 0.4);
        assert_clipped(&scene.points, &edit, from, to, 0.4);
        let after = scene.after(&edit);
        let lost: Vec<_> = crossing.difference(&after).collect();
        assert!(lost.is_empty(), "the crossing wall stays: lost {lost:?}");
        assert!((30..42).all(|i| (52..60).all(|j| after.contains(&(i, j)))), "the cabinet stays");
        for (i, off) in specks {
            // the band is rows 56..64 (0.2 m either side of row 60's centre)
            assert_eq!(after.contains(&(i, 60 + off)), off.abs() > 4, "speck at column {i}, {off} voxels off");
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
        for (from, to) in [([2.025, 0.4], [2.025, 2.6]), ([2.1, 2.525], [1.55, 2.525]), ([1.625, 2.45], [1.625, 4.6])] {
            let edit = straighten(&points, &normals, V, from, to, 0.3, None, |_, _| Some(at(0)), Reach { above_floor: None, top: f32::MAX, bottom: f32::MIN });
            assert_clipped(&points, &edit, from, to, 0.3);
            let removed: AHashSet<u32> = edit.remove.iter().copied().collect();
            let kept: Vec<usize> = (0..points.len()).filter(|i| !removed.contains(&(*i as u32))).collect();
            normals = kept.iter().map(|i| normals[*i]).chain(edit.add_normals.iter().copied()).collect();
            points = kept.iter().map(|i| points[*i]).chain(edit.add.iter().copied()).collect();
        }
        let after: AHashSet<(i32, i32)> = points.iter().filter(|p| p[2] > at(0) + 0.01).map(|p| ((p[0] / V).floor() as i32, (p[1] / V).floor() as i32)).collect();
        let want: AHashSet<(i32, i32)> = [ideal(a0, a1, 0.05), ideal(a1, b0, 0.05), ideal(b0, b1, 0.05)].into_iter().flatten().collect();
        let (missing, extra): (Vec<_>, Vec<_>) = (want.difference(&after).collect(), after.difference(&want).collect());
        assert!(missing.is_empty() && extra.len() <= 4, "a clean step: missing {missing:?}, extra {extra:?}");
    }

    #[test]
    fn a_crossing_wall_and_a_doorway_survive() {
        let mut scene = Scene::new();
        scene.wall([0.5, 3.0], [5.5, 3.0], 0.1, true);
        scene.wall([3.0, 0.5], [3.0, 5.5], 0.1, false);
        let crossing = ideal([3.0, 0.5], [3.0, 5.5], 0.1);
        let (from, to) = ([1.0, 3.0], [5.0, 3.0]);
        let edit = scene.run(from, to, 0.4);
        assert_clipped(&scene.points, &edit, from, to, 0.4);
        let after = scene.after(&edit);
        assert!(crossing.iter().all(|c| after.contains(c)), "the crossing wall keeps every column");
    }

    /// The real case (mid360_athens_stairs, upper storey): the noisy wall Jeff straightened into three parallel lines.
    /// The voxels around it before that edit and the storey's floor model are in tests/fixtures/athens_noisy_wall.*;
    /// the stroke is the one he drew (3.1 m along the wall, the default 0.4 m band). Set STRAIGHTEN_FIXTURE_OUT=<file>
    /// to write the map after the edit (f32 xyz) for a look.
    #[test]
    fn the_real_noisy_wall_becomes_one_wall_inside_the_selection() {
        let points: Vec<[f32; 3]> = include_bytes!("../tests/fixtures/athens_noisy_wall.bin")
            .chunks_exact(12)
            .map(|c| std::array::from_fn(|k| f32::from_le_bytes(c[k * 4..k * 4 + 4].try_into().unwrap())))
            .collect();
        let model: serde_json::Value = serde_json::from_str(include_str!("../tests/fixtures/athens_noisy_wall.json")).unwrap();
        let number = |v: &serde_json::Value| v.as_f64().unwrap() as f32;
        let (cell, origin) = (number(&model["cell"]), [number(&model["origin"][0]), number(&model["origin"][1])]);
        let heights: Vec<Vec<Option<f32>>> = model["heights"].as_array().unwrap().iter().map(|row| row.as_array().unwrap().iter().map(|h| h.as_f64().map(|h| h as f32)).collect()).collect();
        // the floor model's bilinear height (floor.rs's height_at)
        let floor_at = |x: f32, y: f32| -> Option<f32> {
            let (fx, fy) = ((x - origin[0]) / cell - 0.5, (y - origin[1]) / cell - 0.5);
            let (c0, r0) = (fx.floor() as i32, fy.floor() as i32);
            let (tx, ty) = (fx - c0 as f32, fy - r0 as f32);
            let (mut sum, mut weights) = (0.0, 0.0);
            for (dc, dr, weight) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
                if let Some(Some(h)) = heights.get((r0 + dr) as usize).and_then(|row| row.get((c0 + dc) as usize)) {
                    if weight > 0.0 && r0 + dr >= 0 && c0 + dc >= 0 {
                        sum += h * weight;
                        weights += weight;
                    }
                }
            }
            (weights > 1e-6).then(|| sum / weights)
        };
        let band = [number(&model["band"][0]), number(&model["band"][1])];
        // the server's reach for a straighten on the top storey: its floor to 3.25 m over it
        let reach = Reach { above_floor: None, top: band[0] + 3.25, bottom: band[0] };
        let (from, to, width) = ([-1.3, -2.33], [1.6, -2.33], 0.4);
        let normals = vec![[0.0, 0.0, 1.0]; points.len()];
        let edit = straighten(&points, &normals, V, from, to, width, None, floor_at, reach);
        assert!(!edit.add.is_empty() && !edit.remove.is_empty(), "it does something");
        assert_clipped(&points, &edit, from, to, width);
        let removed: AHashSet<u32> = edit.remove.iter().copied().collect();
        let after: Vec<[f32; 3]> = points.iter().enumerate().filter(|(i, _)| !removed.contains(&(*i as u32))).map(|(_, p)| *p).chain(edit.add.iter().copied()).collect();
        if let Ok(path) = std::env::var("STRAIGHTEN_FIXTURE_OUT") {
            std::fs::write(path, after.iter().flat_map(|p| p.iter().flat_map(|v| v.to_le_bytes())).collect::<Vec<u8>>()).unwrap();
        }
        // what the 2D view draws as wall: 0.1–1.8 m over the floor model
        let wall: AHashSet<(i32, i32)> = after
            .iter()
            .filter(|p| floor_at(p[0], p[1]).is_some_and(|f| (0.1..=1.8).contains(&(p[2] - f))))
            .map(|p| ((p[0] / V).floor() as i32, (p[1] / V).floor() as i32))
            .collect();
        let (x0, x1) = (((from[0] + 0.1) / V).floor() as i32, ((to[0] - 0.1) / V).floor() as i32);
        for x in x0..=x1 {
            let rows: Vec<i32> = (-80..-20).filter(|r| in_band((x, *r), from, to, width) && wall.contains(&(x, *r))).collect();
            let runs = rows.windows(2).filter(|w| w[1] != w[0] + 1).count() + usize::from(!rows.is_empty());
            assert!(runs <= 1 && rows.len() <= 3, "column {x}: one wall at most 3 voxels thick, got rows {rows:?}");
        }
    }
}
