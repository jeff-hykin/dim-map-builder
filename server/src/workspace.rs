//! One open session's logic, independent of HTTP: the map in the map frame, every edit (undoable), the cleanup
//! tools, floors and plans. The API and the MCP tools both call these; nothing here knows who asked.
use crate::session::{Annotations, Area, BoxAnnotation, Floor, MapData, PlaneAnnotation, PlanPoint, PointAnnotation, Session, Transform, UndoEntry};
use anyhow::{bail, Context, Result};
use mapping::floorplan::{rasterize, FloorPlan, PlanOptions};
use mapping::tf::Iso;
use mapping::voxels::{self, Box3};
use nalgebra::{Point3, UnitQuaternion, Vector3};
use serde::{Deserialize, Serialize};

/// Where a tool acts: a box, the camera's view (its view-projection matrix, column-major, map frame), or everything.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Region {
    All,
    Box {
        #[serde(flatten)]
        region: crate::session::Box3Json,
    },
    View {
        matrix: [f32; 16],
    },
}

impl Region {
    pub fn contains(&self, p: [f32; 3]) -> bool {
        match self {
            Region::All => true,
            Region::Box { region } => Box3::from(*region).contains(p),
            Region::View { matrix: m } => {
                let x = m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12];
                let y = m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13];
                let z = m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14];
                let w = m[3] * p[0] + m[7] * p[1] + m[11] * p[2] + m[15];
                w > 0.0 && x.abs() <= w && y.abs() <= w && z.abs() <= w
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpResult {
    pub label: String,
    /// voxels this removed (or would remove, for a preview)
    pub changed: usize,
    pub remaining: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<Vec<[f32; 3]>>,
}

#[derive(Clone)]
pub struct Workspace {
    pub session: Session,
    pub map: Option<MapData>,
    /// map-frame copies of `map.points` / `map.normals` (transform applied), rebuilt when the transform changes
    points: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    /// bumps whenever what the page draws as the map changes (deletions, transform, a new build)
    pub map_version: u64,
}

fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{prefix}{:x}{:x}", now.as_millis() % 0xffffff, COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub fn now_seconds() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

fn apply(value: &Iso, p: [f32; 3]) -> [f32; 3] {
    let v = value * Point3::new(p[0] as f64, p[1] as f64, p[2] as f64);
    [v.x as f32, v.y as f32, v.z as f32]
}

fn rotate(value: &Iso, n: [f32; 3]) -> [f32; 3] {
    let v = value.rotation * Vector3::new(n[0] as f64, n[1] as f64, n[2] as f64);
    [v.x as f32, v.y as f32, v.z as f32]
}

impl Workspace {
    pub fn new(session: Session, map: Option<MapData>) -> Workspace {
        let mut workspace = Workspace { session, map, points: Vec::new(), normals: Vec::new(), map_version: (now_seconds() * 1000.0) as u64 };
        workspace.refresh_frame();
        workspace
    }

    fn refresh_frame(&mut self) {
        self.map_version += 1;
        let transform = self.session.transform.iso();
        if let Some(map) = &self.map {
            self.points = map.points.iter().map(|p| apply(&transform, *p)).collect();
            self.normals = map.normals.iter().map(|n| rotate(&transform, *n)).collect();
        } else {
            self.points.clear();
            self.normals.clear();
        }
    }

    pub fn set_map(&mut self, map: MapData) {
        self.map = Some(map);
        self.session.stage = "map".into();
        self.refresh_frame();
    }

    /// Indices of voxels still in the map.
    pub fn visible(&self) -> Vec<u32> {
        let Some(map) = &self.map else { return Vec::new() };
        (0..self.points.len() as u32).filter(|i| !map.removed[*i as usize]).collect()
    }

    pub fn visible_points(&self) -> (Vec<u32>, Vec<[f32; 3]>, Vec<[f32; 3]>) {
        let indices = self.visible();
        let points = indices.iter().map(|i| self.points[*i as usize]).collect();
        let normals = indices.iter().map(|i| self.normals[*i as usize]).collect();
        (indices, points, normals)
    }

    pub fn remaining(&self) -> usize {
        self.map.as_ref().map_or(0, |m| m.removed.iter().filter(|r| !**r).count())
    }

    /// The map in the map frame, for the page: little-endian f32 xyz of every visible voxel.
    pub fn points_bytes(&self) -> Vec<u8> {
        let Some(map) = &self.map else { return Vec::new() };
        let mut out = Vec::with_capacity(self.points.len() * 12);
        for (index, p) in self.points.iter().enumerate() {
            if !map.removed[index] {
                for v in p {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        out
    }

    pub fn paths(&self) -> serde_json::Value {
        let transform = self.session.transform.iso();
        let Some(map) = &self.map else { return serde_json::json!({}) };
        let moved = |path: &[[f32; 3]]| path.iter().map(|p| apply(&transform, *p)).collect::<Vec<_>>();
        serde_json::json!({
            "raw": moved(&map.raw_path),
            "corrected": moved(&map.corrected_path),
            "loops": map.loops.iter().map(|(a, b)| [apply(&transform, *a), apply(&transform, *b)]).collect::<Vec<_>>(),
        })
    }

    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let (_, points, _) = self.visible_points();
        if points.is_empty() {
            return None;
        }
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for p in &points {
            for axis in 0..3 {
                min[axis] = min[axis].min(p[axis]);
                max[axis] = max[axis].max(p[axis]);
            }
        }
        Some((min, max))
    }

    fn record(&mut self, entry: UndoEntry) {
        self.session.history.push(entry.label.clone());
        if self.session.history.len() > 200 {
            self.session.history.remove(0);
        }
        self.session.undo.push(entry);
        if self.session.undo.len() > 500 {
            self.session.undo.remove(0);
        }
        self.session.redo.clear();
        self.session.revision += 1;
    }

    fn require_map(&self) -> Result<&MapData> {
        self.map.as_ref().context("no map yet: build the global map first")
    }

    /// Removes `indices` (of currently visible voxels) as one undoable step.
    pub fn delete(&mut self, label: &str, indices: Vec<u32>) -> Result<OpResult> {
        let map = self.map.as_mut().context("no map yet: build the global map first")?;
        let flipped: Vec<u32> = indices.into_iter().filter(|i| !map.removed[*i as usize]).collect();
        for i in &flipped {
            map.removed[*i as usize] = true;
        }
        let changed = flipped.len();
        if changed > 0 {
            self.map_version += 1;
            self.record(UndoEntry { label: format!("{label} ({changed} voxels)"), flipped, ..Default::default() });
        }
        Ok(OpResult { label: label.into(), changed, remaining: self.remaining(), preview: None })
    }

    /// A cleanup / crop tool: compute its selection among visible voxels; delete it unless `preview`.
    pub fn op(&mut self, op: &str, params: &serde_json::Value, region: &Region, preview: bool) -> Result<OpResult> {
        self.require_map()?;
        let (indices, points, normals) = self.visible_points();
        let voxel = self.map.as_ref().unwrap().voxel_size;
        let number = |key: &str, default: f64| params.get(key).and_then(|v| v.as_f64()).unwrap_or(default);
        let in_region: Vec<bool> = points.iter().map(|p| region.contains(*p)).collect();
        let restrict = |selection: Vec<u32>| -> Vec<u32> { selection.into_iter().filter(|i| in_region[*i as usize]).collect() };
        let floors: Vec<f32> = if self.session.annotations.floors.is_empty() {
            voxels::floor_levels(&points, &normals, 1.8)
        } else {
            self.session.annotations.floors.iter().map(|f| f.z).collect()
        };
        let (label, local): (String, Vec<u32>) = match op {
            "floating" => {
                let min = number("minVoxels", 30.0) as usize;
                // groups judged on the whole map (a speck in view stays a speck), but only those in the region go
                let selection = voxels::select_floating(&points, voxel, min, None);
                let selection = selection.into_iter().filter(|i| in_region[*i as usize]).collect();
                (format!("Remove floating clusters < {min} voxels"), selection)
            }
            "outliers" => {
                let k = number("neighbors", 20.0) as usize;
                let ratio = number("stdRatio", 2.0) as f32;
                (format!("Remove statistical outliers (k={k}, σ×{ratio})"), restrict(voxels::select_outliers(&points, k, ratio, None)))
            }
            "floor" => {
                let thickness = number("thickness", (voxel * 1.6) as f64) as f32;
                (format!("Remove floor ({} level{})", floors.len(), if floors.len() == 1 { "" } else { "s" }), restrict(voxels::select_floor(&points, &normals, &floors, thickness, None)))
            }
            "walls" => {
                let min_height = number("minHeight", 0.3) as f32;
                (String::from("Remove walls"), restrict(voxels::select_walls(&points, &normals, &floors, min_height, None)))
            }
            "keepWalls" => {
                let min_height = number("minHeight", 0.3) as f32;
                let walls: std::collections::HashSet<u32> = voxels::select_walls(&points, &normals, &floors, min_height, None).into_iter().collect();
                (String::from("Keep only walls"), (0..points.len() as u32).filter(|i| !walls.contains(i) && in_region[*i as usize]).collect())
            }
            "cropOutside" => {
                let Region::Box { region } = region else { bail!("cropOutside needs a box region") };
                (String::from("Crop to box"), voxels::select_box(&points, &Box3::from(*region), false))
            }
            "deleteInside" | "delete" => (String::from("Delete selection"), (0..points.len() as u32).filter(|i| in_region[*i as usize]).collect()),
            "cropHeight" => {
                let (low, high) = (number("zMin", f64::NEG_INFINITY) as f32, number("zMax", f64::INFINITY) as f32);
                (format!("Crop heights to {low:.2}..{high:.2} m"), voxels::select_outside_heights(&points, low, high))
            }
            other => bail!("unknown op {other} (floating, outliers, floor, walls, keepWalls, cropOutside, deleteInside, cropHeight)"),
        };
        let global: Vec<u32> = local.iter().map(|i| indices[*i as usize]).collect();
        if preview {
            let sample: Vec<[f32; 3]> = local.iter().take(200_000).map(|i| points[*i as usize]).collect();
            return Ok(OpResult { label, changed: global.len(), remaining: self.remaining(), preview: Some(sample) });
        }
        self.delete(&label, global)
    }

    /// Replace the map-frame transform; annotations move with the map so they stay on what they marked.
    pub fn set_transform(&mut self, label: &str, transform: Transform) -> Result<()> {
        let before = self.session.transform;
        let delta = transform.iso() * before.iso().inverse();
        let annotations_before = self.session.annotations.clone();
        let mut annotations = annotations_before.clone();
        move_annotations(&mut annotations, &delta);
        let plans_before = self.session.plans.clone();
        self.session.transform = transform;
        self.session.annotations = annotations.clone();
        // plans are rasters of the old frame: they go stale
        self.session.plans.clear();
        self.refresh_frame();
        self.record(UndoEntry {
            label: label.into(),
            transform: Some((before, transform)),
            annotations: Some((annotations_before, annotations)),
            plans: Some((plans_before, Vec::new())),
            ..Default::default()
        });
        Ok(())
    }

    /// Spin the map about the vertical axis through the map's center.
    pub fn rotate_yaw(&mut self, degrees: f64) -> Result<()> {
        let center = self.bounds().map(|(a, b)| Vector3::new(((a[0] + b[0]) / 2.0) as f64, ((a[1] + b[1]) / 2.0) as f64, 0.0)).unwrap_or_default();
        let spin = Iso::rotation_wrt_point(UnitQuaternion::from_axis_angle(&Vector3::z_axis(), degrees.to_radians()), Point3::from(center));
        let transform = Transform::from_iso(&(spin * self.session.transform.iso()));
        self.set_transform(&format!("Rotate {degrees:.1}°"), transform)
    }

    /// Tilt the map so its main floor is level (its up-facing voxels' mean normal becomes +z) and sits at z = 0.
    pub fn level(&mut self) -> Result<Vector3<f64>> {
        self.require_map()?;
        let (_, points, normals) = self.visible_points();
        let floors = voxels::floor_levels(&points, &normals, 1.8);
        let lowest = floors.first().copied().context("no floor found to level against")?;
        let mut up = Vector3::zeros();
        for (p, n) in points.iter().zip(&normals) {
            if n[2] > 0.85 && (p[2] - lowest).abs() < 0.15 {
                up += Vector3::new(n[0] as f64, n[1] as f64, n[2] as f64);
            }
        }
        let up = up.try_normalize(1e-9).context("no floor normals")?;
        let tilt = UnitQuaternion::rotation_between(&up, &Vector3::z()).unwrap_or_default();
        let lift = Iso::from_parts(nalgebra::Translation3::new(0.0, 0.0, -lowest as f64), UnitQuaternion::identity());
        let transform = Transform::from_iso(&(lift * Iso::from_parts(nalgebra::Translation3::identity(), tilt) * self.session.transform.iso()));
        self.set_transform(&format!("Level the floor ({:.2}° tilt)", tilt.angle().to_degrees()), transform)?;
        Ok(up)
    }

    pub fn undo(&mut self) -> Option<String> {
        let entry = self.session.undo.pop()?;
        self.unapply(&entry, true);
        let label = entry.label.clone();
        self.session.redo.push(entry);
        self.session.revision += 1;
        self.session.history.push(format!("Undo: {label}"));
        Some(label)
    }

    pub fn redo(&mut self) -> Option<String> {
        let entry = self.session.redo.pop()?;
        self.unapply(&entry, false);
        let label = entry.label.clone();
        self.session.undo.push(entry);
        self.session.revision += 1;
        self.session.history.push(format!("Redo: {label}"));
        Some(label)
    }

    fn unapply(&mut self, entry: &UndoEntry, undo: bool) {
        if let Some(map) = self.map.as_mut() {
            for i in &entry.flipped {
                map.removed[*i as usize] = !undo;
            }
        }
        if !entry.flipped.is_empty() {
            self.map_version += 1;
        }
        if let Some((before, after)) = &entry.annotations {
            self.session.annotations = if undo { before.clone() } else { after.clone() };
        }
        if let Some((before, after)) = &entry.plans {
            self.session.plans = if undo { before.clone() } else { after.clone() };
        }
        if let Some((before, after)) = &entry.transform {
            self.session.transform = if undo { *before } else { *after };
            self.refresh_frame();
        }
    }

    /// Any annotation change, as one undoable step.
    pub fn edit_annotations(&mut self, label: &str, change: impl FnOnce(&mut Annotations) -> Result<()>) -> Result<()> {
        let before = self.session.annotations.clone();
        let mut after = before.clone();
        change(&mut after)?;
        if after == before {
            return Ok(());
        }
        self.session.annotations = after.clone();
        self.record(UndoEntry { label: label.into(), annotations: Some((before, after)), ..Default::default() });
        Ok(())
    }

    pub fn add_box(&mut self, label: &str, region: Box3, source: &str) -> Result<String> {
        let id = new_id("box-");
        let annotation = BoxAnnotation { id: id.clone(), label: label.into(), region: region.into(), color: None, source: source.into() };
        self.edit_annotations(&format!("Add box \"{label}\""), |a| {
            a.boxes.push(annotation);
            Ok(())
        })?;
        Ok(id)
    }

    pub fn add_plane(&mut self, label: &str, center: [f32; 3], normal: [f32; 3], size: [f32; 2], source: &str) -> Result<String> {
        let id = new_id("plane-");
        let n = Vector3::new(normal[0], normal[1], normal[2]).try_normalize(1e-6).unwrap_or(Vector3::z());
        let annotation = PlaneAnnotation { id: id.clone(), label: label.into(), center, normal: [n.x, n.y, n.z], size, source: source.into() };
        self.edit_annotations(&format!("Add plane \"{label}\""), |a| {
            a.planes.push(annotation);
            Ok(())
        })?;
        Ok(id)
    }

    pub fn add_point(&mut self, label: &str, position: [f32; 3], source: &str) -> Result<String> {
        let id = new_id("point-");
        let annotation = PointAnnotation { id: id.clone(), label: label.into(), position, source: source.into() };
        self.edit_annotations(&format!("Add point \"{label}\""), |a| {
            a.points.push(annotation);
            Ok(())
        })?;
        Ok(id)
    }

    /// Patch any annotation by id with the fields given (JSON merge), or delete it.
    pub fn update_annotation(&mut self, id: &str, patch: Option<&serde_json::Value>) -> Result<()> {
        let label = match patch {
            Some(_) => format!("Edit {id}"),
            None => format!("Delete {id}"),
        };
        self.edit_annotations(&label, |a| {
            fn patch_list<T: Serialize + for<'de> Deserialize<'de>>(list: &mut Vec<T>, id: &str, patch: Option<&serde_json::Value>, get_id: impl Fn(&T) -> &str) -> Result<bool> {
                let Some(index) = list.iter().position(|item| get_id(item) == id) else { return Ok(false) };
                match patch {
                    None => {
                        list.remove(index);
                    }
                    Some(patch) => {
                        let mut value = serde_json::to_value(&list[index])?;
                        merge(&mut value, patch);
                        value["id"] = serde_json::Value::String(id.to_string());
                        list[index] = serde_json::from_value(value).context("invalid annotation fields")?;
                    }
                }
                Ok(true)
            }
            let found = patch_list(&mut a.boxes, id, patch, |x| &x.id)?
                || patch_list(&mut a.planes, id, patch, |x| &x.id)?
                || patch_list(&mut a.points, id, patch, |x| &x.id)?
                || patch_list(&mut a.plan_points, id, patch, |x| &x.id)?
                || patch_list(&mut a.areas, id, patch, |x| &x.id)?;
            if !found {
                bail!("no annotation {id}");
            }
            Ok(())
        })
    }

    pub fn add_plan_point(&mut self, floor: usize, name: &str, position: [f32; 2]) -> Result<String> {
        self.require_floor(floor)?;
        let id = new_id("spot-");
        let point = PlanPoint { id: id.clone(), floor, name: name.into(), position };
        self.edit_annotations(&format!("Name point \"{name}\""), |a| {
            a.plan_points.push(point);
            Ok(())
        })?;
        Ok(id)
    }

    pub fn add_area(&mut self, floor: usize, name: &str, kind: &str, polygon: Vec<[f32; 2]>) -> Result<String> {
        self.require_floor(floor)?;
        if polygon.len() < 3 {
            bail!("an area needs at least 3 corners");
        }
        let id = new_id("area-");
        let area = Area { id: id.clone(), floor, name: name.into(), kind: kind.into(), polygon };
        self.edit_annotations(&format!("Add {kind} area \"{name}\""), |a| {
            a.areas.push(area);
            Ok(())
        })?;
        Ok(id)
    }

    fn require_floor(&self, floor: usize) -> Result<()> {
        if self.session.annotations.floors.iter().any(|f| f.index == floor) {
            Ok(())
        } else {
            bail!("no floor {floor}: generate floor plans first (floors: {:?})", self.session.annotations.floors.iter().map(|f| f.index).collect::<Vec<_>>())
        }
    }

    /// Detect floors (or use `levels`), rasterize a plan per floor, as one undoable step.
    pub fn generate_plans(&mut self, multi_floor: bool, levels: Option<Vec<f32>>, options: PlanOptions) -> Result<Vec<Floor>> {
        self.require_map()?;
        let (_, points, normals) = self.visible_points();
        let mut found = match levels {
            Some(levels) if !levels.is_empty() => levels,
            _ => voxels::floor_levels(&points, &normals, 1.8),
        };
        found.sort_by(|a, b| a.total_cmp(b));
        if found.is_empty() {
            bail!("no floor found (no large horizontal surface)");
        }
        if !multi_floor {
            found.truncate(1);
        }
        let floors: Vec<Floor> = found
            .iter()
            .enumerate()
            .map(|(index, z)| Floor { index, name: if found.len() == 1 { "Floor".into() } else { format!("Floor {}", index + 1) }, z: *z })
            .collect();
        let plans: Vec<FloorPlan> = found.iter().enumerate().map(|(index, z)| rasterize(&points, *z, found.get(index + 1).copied(), &options)).collect();
        let annotations_before = self.session.annotations.clone();
        let mut annotations = annotations_before.clone();
        // keep plan annotations of floors that still exist (matched by index)
        let names: Vec<String> = annotations.floors.iter().map(|f| f.name.clone()).collect();
        annotations.floors = floors
            .iter()
            .map(|f| Floor { name: names.get(f.index).cloned().filter(|n| !n.is_empty() && found.len() > 1 || found.len() == 1).unwrap_or(f.name.clone()), ..f.clone() })
            .collect();
        annotations.plan_points.retain(|p| p.floor < floors.len());
        annotations.areas.retain(|a| a.floor < floors.len());
        let plans_before = std::mem::replace(&mut self.session.plans, plans.clone());
        self.session.annotations = annotations.clone();
        self.record(UndoEntry {
            label: format!("Generate {} floor plan{}", floors.len(), if floors.len() == 1 { "" } else { "s" }),
            annotations: Some((annotations_before, annotations.clone())),
            plans: Some((plans_before, plans)),
            ..Default::default()
        });
        Ok(annotations.floors)
    }

    /// The tightest box around the non-floor voxels in `region` (the agent's "fit a box to that").
    pub fn fit_box(&self, region: &Box3) -> Result<(Box3, usize)> {
        self.require_map()?;
        let (_, points, normals) = self.visible_points();
        let floors: Vec<f32> = self.session.annotations.floors.iter().map(|f| f.z).collect();
        let floors = if floors.is_empty() { voxels::floor_levels(&points, &normals, 1.8) } else { floors };
        voxels::fit_box(&points, Some(&normals), region, &floors).context("fewer than 4 voxels (above the floor) in that region")
    }

    /// Voxels near a point / within a box, summarized (count, bounds, height range): the agent's way to probe.
    pub fn query(&self, region: &Region, limit: usize) -> serde_json::Value {
        let (_, points, normals) = self.visible_points();
        let mut found: Vec<([f32; 3], [f32; 3])> = points.iter().zip(&normals).filter(|(p, _)| region.contains(**p)).map(|(p, n)| (*p, *n)).collect();
        let count = found.len();
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        let (mut horizontal, mut vertical) = (0, 0);
        for (p, n) in &found {
            for axis in 0..3 {
                min[axis] = min[axis].min(p[axis]);
                max[axis] = max[axis].max(p[axis]);
            }
            if n[2].abs() > 0.8 {
                horizontal += 1;
            } else if n[2].abs() < 0.3 && (n[0] != 0.0 || n[1] != 0.0) {
                vertical += 1;
            }
        }
        let step = (found.len() / limit.max(1)).max(1);
        found = found.into_iter().step_by(step).take(limit).collect();
        serde_json::json!({
            "count": count,
            "min": if count > 0 { Some(min) } else { None },
            "max": if count > 0 { Some(max) } else { None },
            "horizontalSurfaceVoxels": horizontal,
            "verticalSurfaceVoxels": vertical,
            "sample": found.iter().map(|(p, _)| p.map(|v| (v * 100.0).round() / 100.0)).collect::<Vec<_>>(),
        })
    }
}

fn merge(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(target), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                match target.get_mut(key) {
                    Some(existing) if existing.is_object() && value.is_object() => merge(existing, value),
                    _ => {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (target, patch) => *target = patch.clone(),
    }
}

fn move_annotations(annotations: &mut Annotations, delta: &Iso) {
    let yaw_delta = delta.rotation.euler_angles().2 as f32;
    for b in &mut annotations.boxes {
        b.region.center = apply(delta, b.region.center);
        b.region.yaw += yaw_delta;
    }
    for p in &mut annotations.planes {
        p.center = apply(delta, p.center);
        p.normal = rotate(delta, p.normal);
    }
    for p in &mut annotations.points {
        p.position = apply(delta, p.position);
    }
    for f in &mut annotations.floors {
        f.z = apply(delta, [0.0, 0.0, f.z])[2];
    }
    for p in &mut annotations.plan_points {
        let moved = apply(delta, [p.position[0], p.position[1], 0.0]);
        p.position = [moved[0], moved[1]];
    }
    for a in &mut annotations.areas {
        for corner in &mut a.polygon {
            let moved = apply(delta, [corner[0], corner[1], 0.0]);
            *corner = [moved[0], moved[1]];
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// a 6 x 6 m room: floor, two walls, a table, and some floating specks
    pub fn room_map() -> MapData {
        // voxel centers sit mid-cell, like the ray tracer's: (k + 0.5) * 0.05
        let at = |k: i32| (k as f32 + 0.5) * 0.05;
        let mut points = Vec::new();
        for i in 0..120 {
            for j in 0..120 {
                points.push([at(i), at(j), at(0)]);
            }
            for k in 1..51 {
                points.push([at(i), at(0), at(k)]);
                points.push([at(0), at(i), at(k)]);
            }
        }
        for i in 0..24 {
            for j in 0..16 {
                points.push([at(40 + i), at(60 + j), at(15)]);
            }
        }
        points.extend([[at(60), at(20), at(32)], [at(61), at(20), at(32)], [at(100), at(100), at(42)]]);
        let normals = voxels::normals(&points, 0.18);
        let n = points.len();
        MapData { voxel_size: 0.05, points, normals, removed: vec![false; n], ..Default::default() }
    }

    #[test]
    fn edits_undo_and_redo() {
        let mut ws = Workspace::new(Session { stage: "map".into(), ..Default::default() }, Some(room_map()));
        let total = ws.remaining();
        let floating = ws.op("floating", &serde_json::json!({ "minVoxels": 10 }), &Region::All, false).unwrap();
        assert_eq!(floating.changed, 3);
        let floor = ws.op("floor", &serde_json::json!({}), &Region::All, false).unwrap();
        assert!(floor.changed > 10_000, "{}", floor.changed);
        assert_eq!(ws.remaining(), total - 3 - floor.changed);
        ws.undo();
        ws.undo();
        assert_eq!(ws.remaining(), total);
        ws.redo();
        assert_eq!(ws.remaining(), total - 3);
        // a preview changes nothing
        let preview = ws.op("walls", &serde_json::json!({}), &Region::All, true).unwrap();
        assert!(preview.changed > 5000 && preview.preview.is_some());
        assert_eq!(ws.remaining(), total - 3);
        // view-frustum region: an orthographic box x,y in [-1,1] → only points with |x|,|y|,|z| <= 1
        let identity = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        assert!(Region::View { matrix: identity }.contains([0.5, 0.5, 0.5]));
        assert!(!Region::View { matrix: identity }.contains([1.5, 0.5, 0.5]));
    }

    #[test]
    fn annotations_follow_the_map_and_undo() {
        let mut ws = Workspace::new(Session { stage: "map".into(), ..Default::default() }, Some(room_map()));
        let id = ws.add_box("table", Box3::from_bounds([2.0, 3.0, 0.7], [3.2, 3.8, 0.8]), "user").unwrap();
        assert_eq!(ws.bounds().unwrap(), ([0.025, 0.025, 0.025], [5.975, 5.975, 2.525]));
        ws.add_point("door", [6.0, 1.0, 0.0], "agent").unwrap();
        ws.update_annotation(&id, Some(&serde_json::json!({ "label": "big table", "box": { "size": [1.3, 0.9, 0.2] } }))).unwrap();
        assert_eq!(ws.session.annotations.boxes[0].label, "big table");
        assert_eq!(ws.session.annotations.boxes[0].region.size, [1.3, 0.9, 0.2]);
        assert_eq!(ws.session.annotations.boxes[0].region.center, [2.6, 3.4, 0.75]);
        ws.rotate_yaw(90.0).unwrap();
        // the box rotated about the map's center (3, 3) with everything else
        let moved = ws.session.annotations.boxes[0].region;
        assert!((moved.center[0] - 2.6).abs() < 0.02 && (moved.center[1] - 2.6).abs() < 0.02, "{moved:?}");
        ws.undo();
        assert_eq!(ws.session.annotations.boxes[0].region.center, [2.6, 3.4, 0.75]);
        let (fit, used) = ws.fit_box(&Box3::from_bounds([1.5, 2.5, 0.1], [3.8, 4.2, 1.2])).unwrap();
        assert_eq!(used, 24 * 16);
        assert!((fit.center[2] - 0.75).abs() < 0.05);
        ws.update_annotation(&id, None).unwrap();
        assert!(ws.session.annotations.boxes.is_empty());
        assert!(ws.update_annotation("nope", None).is_err());
    }

    #[test]
    fn plans_and_plan_annotations() {
        let mut ws = Workspace::new(Session { stage: "map".into(), ..Default::default() }, Some(room_map()));
        assert!(ws.add_area(0, "x", "no-go", vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]).is_err(), "no floors yet");
        let floors = ws.generate_plans(true, None, PlanOptions::default()).unwrap();
        assert_eq!(floors.len(), 1);
        assert_eq!(ws.session.plans.len(), 1);
        let (free, occupied, _) = ws.session.plans[0].counts();
        assert!(free > 5000 && occupied > 100, "{free} {occupied}");
        ws.add_area(0, "Stairs", "no-go", vec![[4.0, 4.0], [5.0, 4.0], [5.0, 5.0], [4.0, 5.0]]).unwrap();
        ws.add_plan_point(0, "Dock", [1.0, 1.0]).unwrap();
        assert_eq!(ws.session.annotations.areas[0].kind, "no-go");
        // already level with the floor at z = 0: levelling barely moves it
        ws.level().unwrap();
        let t = ws.session.transform;
        assert!(t.translation[2].abs() < 0.03 && t.rotation[3] > 0.9999, "{t:?}");
    }
}
