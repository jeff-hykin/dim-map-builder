//! Probing the map's geometry for the agent and the page: regions (a box, "view", "all"), boxes, and the
//! furniture-like clusters standing on a floor.
use crate::workspace::{Region, Workspace};
use anyhow::{bail, Context, Result};
use mapping::voxels::Box3;
use serde_json::{json, Value};

pub fn parse_box(value: &Value) -> Result<Box3> {
    let b: crate::session::Box3Json = serde_json::from_value(value.clone()).context("box needs center [x,y,z] and size [dx,dy,dz]")?;
    if b.size.iter().any(|s| *s <= 0.0) {
        bail!("box sizes must be > 0");
    }
    Ok(b.into())
}

/// A region as the page sends it (`{kind: "all" | "view" | "box", ...}`) or as the agent does: "view" (the user's
/// camera frustum, from the page's last report), "all", or a box `{center, size, yaw}`.
pub fn parse_region(workspace: &Workspace, value: &Value) -> Result<Region> {
    match value {
        Value::Null => Ok(Region::All),
        Value::String(kind) if kind == "all" => Ok(Region::All),
        Value::String(kind) if kind == "view" => view_region(workspace),
        Value::Object(fields) if fields.get("kind").and_then(Value::as_str) == Some("view") && !fields.contains_key("matrix") => view_region(workspace),
        Value::Object(fields) if fields.contains_key("kind") => {
            let region: Region = serde_json::from_value(value.clone()).context("region")?;
            if let Region::Box { region: b } = &region {
                parse_box(&serde_json::to_value(b)?)?;
            }
            Ok(region)
        }
        other => Ok(Region::Box { region: parse_box(other)?.into() }),
    }
}

fn view_region(workspace: &Workspace) -> Result<Region> {
    let matrix = &workspace.session.view["viewProjection"];
    let matrix: [f32; 16] = serde_json::from_value(matrix.clone()).context("the page hasn't reported its view yet (is the Map Builder open?)")?;
    Ok(Region::View { matrix })
}

/// Things standing on a floor: voxels above the floor (not floor, not walls taller than `max_height`, not ceiling),
/// grouped by 2D connectivity of their footprint, each with its fitted box.
pub fn find_objects(ws: &Workspace, region: &Region, min_voxels: usize, max_height: f32) -> Vec<Value> {
    let (_, points, normals) = ws.visible_points();
    let voxel = ws.map.as_ref().map_or(0.05, |m| m.voxel_size);
    let floors: Vec<f32> = if ws.session.annotations.floors.is_empty() {
        mapping::voxels::floor_levels(&points, &normals, 1.8)
    } else {
        ws.session.annotations.floors.iter().map(|f| f.z).collect()
    };
    let floor_below = |z: f32| floors.iter().copied().filter(|f| *f <= z + 0.05).fold(f32::NEG_INFINITY, f32::max);
    let mut cells: std::collections::HashMap<(i32, i32, i32), Vec<usize>> = std::collections::HashMap::new();
    let cell = (voxel * 2.0).max(0.08);
    for (index, p) in points.iter().enumerate() {
        if !region.contains(*p) {
            continue;
        }
        let floor = floor_below(p[2]);
        if !floor.is_finite() {
            continue;
        }
        let above = p[2] - floor;
        if above < 0.06 || above > max_height {
            continue;
        }
        let storey = floors.iter().position(|f| *f == floor).unwrap_or(0) as i32;
        cells.entry(((p[0] / cell).floor() as i32, (p[1] / cell).floor() as i32, storey)).or_default().push(index);
    }
    // footprints connected in 2D (8-neighbour) per storey; tall groups that run long are walls, skipped
    let mut seen = std::collections::HashSet::new();
    let mut objects = Vec::new();
    let keys: Vec<_> = cells.keys().copied().collect();
    for start in keys {
        if !seen.insert(start) {
            continue;
        }
        let mut stack = vec![start];
        let mut members = Vec::new();
        while let Some(k) = stack.pop() {
            members.extend(cells[&k].iter().copied());
            for dx in -1..=1 {
                for dy in -1..=1 {
                    let n = (k.0 + dx, k.1 + dy, k.2);
                    if cells.contains_key(&n) && seen.insert(n) {
                        stack.push(n);
                    }
                }
            }
        }
        if members.len() < min_voxels {
            continue;
        }
        let group: Vec<[f32; 3]> = members.iter().map(|i| points[*i]).collect();
        let bounds = Box3::from_bounds(
            std::array::from_fn(|axis| group.iter().map(|p| p[axis]).fold(f32::MAX, f32::min) - 0.01),
            std::array::from_fn(|axis| group.iter().map(|p| p[axis]).fold(f32::MIN, f32::max) + 0.01),
        );
        let Some((fit, used)) = mapping::voxels::fit_box(&group, None, &bounds, &[]) else { continue };
        let long = fit.size[0].max(fit.size[1]);
        if long > 4.0 && fit.size[2] > 1.2 {
            continue; // a wall run, not an object
        }
        let floor = floor_below(fit.center[2]);
        objects.push(json!({
            "box": crate::session::Box3Json::from(fit),
            "voxels": used,
            "heightAboveFloor": ((fit.center[2] + fit.size[2] / 2.0 - floor) * 100.0).round() / 100.0,
            "footprint": [(fit.size[0] * 100.0).round() / 100.0, (fit.size[1] * 100.0).round() / 100.0],
        }));
    }
    objects.sort_by(|a, b| b["voxels"].as_u64().cmp(&a["voxels"].as_u64()));
    objects.truncate(60);
    objects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_from_the_page_and_the_agent() {
        let workspace = Workspace::new(Default::default(), None);
        assert!(matches!(parse_region(&workspace, &json!("all")).unwrap(), Region::All));
        assert!(matches!(parse_region(&workspace, &json!({ "kind": "all" })).unwrap(), Region::All));
        assert!(matches!(parse_region(&workspace, &json!({ "center": [0, 0, 0], "size": [1, 1, 1] })).unwrap(), Region::Box { .. }));
        assert!(matches!(parse_region(&workspace, &json!({ "kind": "box", "center": [0, 0, 0], "size": [1, 1, 1], "yaw": 0 })).unwrap(), Region::Box { .. }));
        assert!(parse_region(&workspace, &json!("view")).is_err(), "no page has reported a view");
        assert!(parse_region(&workspace, &json!({ "center": [0, 0, 0], "size": [0, 1, 1] })).is_err());
    }
}
