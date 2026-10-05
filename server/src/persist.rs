//! "Save into recording": the map and everything drawn on it, written into the opened file as new streams (.db) /
//! channels (.mcap, message_encoding "lcm") under `map/`, next to the recording's own data, as dimos message types
//! where one fits (schemas: docs/schema.md):
//!   map/voxels       sensor_msgs.PointCloud2  the edited voxel map, frame "map"
//!   map/path         nav_msgs.Path            the loop-closed sensor path, frame "map"
//!   map/annotations  std_msgs.String          JSON: map ← world transform, boxes, planes, points, polygons, floors,
//!                                             named points, areas (no-go zones), saved views, build summary
//!   map/views        std_msgs.String          JSON: the saved 2D views (each a storey and a height band)
//!   map/slice        std_msgs.String          JSON: the slicer's view {zMin, zMax, yaw, xMin, xMax, yMin, yMax} or null
//!   map/floor_<n>    nav_msgs.OccupancyGrid   storey n's occupancy grid, when exported; origin z = the floor
//! Saving again replaces them (and drops the `map_builder_*` streams older versions wrote). Opening a recording that
//! has them (and no working session) restores the session from them; older `map_builder` saves still open.
use crate::session::{Annotations, BuildSummary, MapData, Session, Transform};
use crate::workspace::Workspace;
use anyhow::{bail, Context, Result};
use dimos_recording::lcm::{self, Header, OccupancyGrid, Pose, PoseStamped};
use dimos_recording::{db, mcap_io, Format, Recording};
use mapping::floorplan::FloorPlan;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SCHEMA: &str = "dimos.map_builder.v1";
pub const MAP_FRAME: &str = "map";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedState {
    pub schema: String,
    pub saved_at: f64,
    pub frame: String,
    /// the frame the recording's data is in; `transform` maps it into "map"
    pub world_frame: String,
    pub transform: Transform,
    pub voxel_size: f32,
    pub annotations: Annotations,
    /// floor index → stream / topic holding its OccupancyGrid
    pub floor_streams: Vec<String>,
    pub build: Option<BuildSummary>,
    /// what the erase tool removed (recording frame): a rebuild after reopening keeps it erased
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub erased: Vec<[f32; 3]>,
}

pub const PREFIX: &str = "map/";

/// map, path, annotations, views, floor n
fn slice_name() -> String {
    format!("{PREFIX}slice")
}

fn names() -> (String, String, String, String, impl Fn(usize) -> String) {
    (format!("{PREFIX}voxels"), format!("{PREFIX}path"), format!("{PREFIX}annotations"), format!("{PREFIX}views"), |n: usize| format!("{PREFIX}floor_{n}"))
}

/// what saves before the `map/` prefix were called: map, path, annotations, and the prefix of them all
fn legacy_names(format: Format) -> (String, String, String, &'static str) {
    let prefix = match format {
        Format::Db => "map_builder_",
        Format::Mcap => "/map_builder/",
    };
    (format!("{prefix}global_map"), format!("{prefix}path"), format!("{prefix}annotations"), prefix)
}

/// Writes the session into its recording. `progress(done, total)` for the mcap rewrite.
pub fn save(workspace: &Workspace, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let session = &workspace.session;
    let path = Path::new(&session.recording_path);
    let format = dimos_recording::format_of(path)?;
    let (map_name, path_name, annotations_name, views_name, floor_name) = names();
    let legacy_prefix = legacy_names(format).3;
    let now = crate::workspace::now_seconds();
    let header = Header::at(now, MAP_FRAME);
    let (_, points, _) = workspace.visible_points();
    let map_payload = lcm::encode_xyz_cloud(&header, &points, None);
    let corrected: Vec<[f32; 3]> = workspace.paths()["corrected"].as_array().map(|list| list.iter().filter_map(|p| serde_json::from_value(p.clone()).ok()).collect()).unwrap_or_default();
    let poses: Vec<PoseStamped> = corrected
        .iter()
        .map(|p| PoseStamped { header: header.clone(), pose: Pose { position: [p[0] as f64, p[1] as f64, p[2] as f64], orientation: [0.0, 0.0, 0.0, 1.0] } })
        .collect();
    let path_payload = lcm::encode_path(&header, &poses);
    let floors: Vec<(String, Vec<u8>)> = session.plans.iter().enumerate().map(|(index, plan)| (floor_name(index), lcm::encode_occupancy_grid(&grid(plan, &header)))).collect();
    let state = SavedState {
        schema: SCHEMA.into(),
        saved_at: now,
        frame: MAP_FRAME.into(),
        world_frame: session.build.as_ref().map(|b| b.world_frame.clone()).unwrap_or_default(),
        transform: session.transform,
        voxel_size: workspace.map.as_ref().map_or(0.05, |m| m.voxel_size),
        annotations: session.annotations.clone(),
        floor_streams: floors.iter().map(|(name, _)| name.clone()).collect(),
        build: session.build.clone(),
        erased: session.erased.clone(),
    };
    let annotations_payload = lcm::encode_string(&serde_json::to_string(&state)?);
    let mut writes: Vec<(String, String, Vec<u8>)> = vec![
        (map_name, lcm::POINT_CLOUD2_TYPE.into(), map_payload),
        (path_name, lcm::PATH_TYPE.into(), path_payload),
        (annotations_name, lcm::STRING_TYPE.into(), annotations_payload),
        (views_name, lcm::STRING_TYPE.into(), lcm::encode_string(&serde_json::to_string(&session.annotations.views)?)),
        (slice_name(), lcm::STRING_TYPE.into(), lcm::encode_string(&serde_json::to_string(&session.annotations.slice)?)),
    ];
    writes.extend(floors.into_iter().map(|(name, payload)| (name, lcm::OCCUPANCY_GRID_TYPE.into(), payload)));
    // earlier saves' floors beyond today's count go too
    let stale: Vec<String> = Recording::open(path)?
        .streams()?
        .into_iter()
        .map(|s| s.name)
        .filter(|name| (name.starts_with(&floor_name(0)[..floor_name(0).len() - 1]) || name.starts_with(legacy_prefix)) && !writes.iter().any(|(w, _, _)| w == name))
        .collect();
    match format {
        Format::Db => {
            let mut connection = db::Connection::open(path).with_context(|| format!("opening {} to write", path.display()))?;
            connection.busy_timeout(std::time::Duration::from_secs(10))?;
            let total = writes.len() as u64 + 1;
            for (index, (name, kind, payload)) in writes.iter().enumerate() {
                db::write_stream(&mut connection, name, kind, &[(now, payload.clone())])?;
                progress(index as u64 + 1, total);
            }
            for name in stale {
                let quoted = |n: &str| format!("\"{}\"", n.replace('"', "\"\""));
                connection.execute_batch(&format!(
                    "DROP TABLE IF EXISTS {}; DROP TABLE IF EXISTS {}; DROP TABLE IF EXISTS {};",
                    quoted(&name),
                    quoted(&format!("{name}_blob")),
                    quoted(&format!("{name}_rtree"))
                ))?;
                connection.execute("DELETE FROM _streams WHERE name = ?1", [&name])?;
            }
            progress(total, total);
        }
        Format::Mcap => {
            let mut replace: Vec<String> = writes.iter().map(|(name, _, _)| name.clone()).collect();
            replace.extend(stale);
            let channels: Vec<mcap_io::NewChannel> = writes.into_iter().map(|(topic, kind, payload)| mcap_io::NewChannel { topic, kind, messages: vec![(now, payload)] }).collect();
            mcap_io::rewrite_with(path, &replace, &channels, progress)?;
        }
    }
    Ok(())
}

fn grid(plan: &FloorPlan, header: &Header) -> OccupancyGrid {
    OccupancyGrid {
        header: header.clone(),
        resolution: plan.resolution,
        width: plan.width,
        height: plan.height,
        origin: Pose { position: [plan.origin[0] as f64, plan.origin[1] as f64, plan.z as f64], orientation: [0.0, 0.0, 0.0, 1.0] },
        data: plan.cells.clone(),
    }
}

/// A session rebuilt from an earlier save inside the recording, if it has one.
pub fn load(recording: &Recording, mut session: Session) -> Result<Option<(Session, MapData)>> {
    let (map_name, path_name, annotations_name, _, _) = names();
    let (legacy_map, legacy_path, legacy_annotations, _) = legacy_names(recording.format());
    let (map_name, path_name, annotations_name, payload) = match recording.latest(&annotations_name)? {
        Some((_, payload)) => (map_name, path_name, annotations_name, payload),
        None => match recording.latest(&legacy_annotations)? {
            Some((_, payload)) => (legacy_map, legacy_path, legacy_annotations, payload),
            None => return Ok(None),
        },
    };
    let state: SavedState = serde_json::from_str(&lcm::decode_string(&payload)?).with_context(|| format!("reading {annotations_name}"))?;
    if state.schema != SCHEMA {
        bail!("unknown map_builder schema {}", state.schema);
    }
    let Some((_, map_payload)) = recording.latest(&map_name)? else { bail!("{annotations_name} without {map_name}") };
    let cloud = dimos_recording::points::extract(&lcm::decode_point_cloud2(&map_payload)?)?;
    // the session keeps the map in the recording's frame and `transform` on top
    let back = state.transform.iso().inverse();
    let to_world = |p: [f32; 3]| mapping::tf::transform_point(&back, p);
    let points: Vec<[f32; 3]> = cloud.points.iter().map(|p| to_world(*p)).collect();
    let normals_map = mapping::voxels::normals(&cloud.points, state.voxel_size * 3.5);
    let normals: Vec<[f32; 3]> = normals_map
        .iter()
        .map(|n| {
            let v = back.rotation * nalgebra::Vector3::new(n[0] as f64, n[1] as f64, n[2] as f64);
            [v.x as f32, v.y as f32, v.z as f32]
        })
        .collect();
    let corrected_path = match recording.latest(&path_name)? {
        Some((_, payload)) => lcm::decode_path(&payload)?.iter().map(|p| to_world([p.pose.position[0] as f32, p.pose.position[1] as f32, p.pose.position[2] as f32])).collect(),
        None => Vec::new(),
    };
    let mut plans = Vec::new();
    for (index, name) in state.floor_streams.iter().enumerate() {
        if let Some((_, payload)) = recording.latest(name)? {
            let grid = lcm::decode_occupancy_grid(&payload)?;
            let z = state.annotations.floors.get(index).map_or(grid.origin.position[2] as f32, |f| f.z);
            plans.push(FloorPlan { z, resolution: grid.resolution, origin: [grid.origin.position[0] as f32, grid.origin.position[1] as f32], width: grid.width, height: grid.height, cells: grid.data });
        }
    }
    let n = points.len();
    session.stage = "map".into();
    session.transform = state.transform;
    session.annotations = state.annotations;
    session.plans = plans;
    session.build = state.build;
    session.erased = state.erased;
    session.saved_at = Some(state.saved_at);
    session.history.push(format!("Restored from the recording's saved map ({} voxels)", n));
    let map = MapData { voxel_size: state.voxel_size, points, normals, removed: vec![false; n], raw_path: Vec::new(), corrected_path, loops: Vec::new() };
    Ok(Some((session, map)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::tests::room_map;
    use mapping::voxels::Box3;

    fn round_trip(format: &str) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("run.{format}"));
        // a recording with a lidar stream of its own
        let cloud = lcm::encode_xyz_cloud(&Header::at(1.0, "lidar"), &[[1.0, 0.0, 0.0]], None);
        match format {
            "db" => {
                let mut connection = db::Connection::open(&path).unwrap();
                db::write_stream(&mut connection, "lidar", lcm::POINT_CLOUD2_TYPE, &[(1.0, cloud)]).unwrap();
            }
            _ => mcap_io::write_fixture(&path, &[("/lidar", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0, cloud)]),
        }
        let session = Session { id: "s".into(), recording_path: path.display().to_string(), stage: "map".into(), ..Default::default() };
        let mut ws = Workspace::new(session, Some(room_map()));
        ws.op("floating", &serde_json::json!({ "minVoxels": 10 }), &crate::workspace::Region::All, false).unwrap();
        ws.rotate_yaw(30.0).unwrap();
        ws.add_box("table", Box3::from_bounds([2.0, 3.0, 0.7], [3.2, 3.8, 0.8]), "user").unwrap();
        ws.generate_plans(true, None, Default::default()).unwrap();
        ws.add_area(0, "Stairs", "no-go", vec![[4.0, 4.0], [5.0, 4.0], [5.0, 5.0]]).unwrap();
        // Modify edits (added voxels), a polygon and a saved view go into the recording too
        let drawn = ws.modify(&crate::workspace::Modify::Draw { floor: 0, path: vec![[1.0, 1.0], [2.0, 1.0]], width: 0.1, height: 1.0 }).unwrap();
        assert!(drawn.changed > 100);
        // an erase of part of the table: after reopening from the recording and building again, it stays erased
        let table = ws.visible_points().1.into_iter().find(|p| (p[2] - 0.775).abs() < 0.01).unwrap();
        let erased = ws.modify(&crate::workspace::Modify::Erase { floor: 0, path: vec![[table[0], table[1]]], radius: 0.2, reach: crate::workspace::ZEnd { z_end: Some(1.8), relative: true, full_column: false } }).unwrap();
        assert!(erased.changed > 20, "{}", erased.changed);
        ws.add_prism(0, "desk", vec![[1.0, 3.0], [2.0, 3.0], [2.0, 4.0]], 0.8, None, "user").unwrap();
        ws.add_view(crate::session::SavedView { name: "walls".into(), floor: 0, follow: true, z_min: 0.1, z_max: 1.8, ..Default::default() }).unwrap();
        ws.set_slice(Some(crate::session::Slice { z_min: -0.2, z_max: 2.0, yaw: 0.3, x_min: 0.0, x_max: 5.0, y_min: 0.5, y_max: 6.0 })).unwrap();
        let visible = ws.remaining();
        save(&ws, |_, _| {}).unwrap();
        save(&ws, |_, _| {}).unwrap(); // twice: replaces, never duplicates

        let recording = Recording::open(&path).unwrap();
        let streams: Vec<String> = recording.streams().unwrap().into_iter().map(|s| s.name).collect();
        for suffix in ["voxels", "path", "annotations", "views", "slice", "floor_0"] {
            assert!(streams.contains(&format!("map/{suffix}")), "{format}: {streams:?}");
        }
        assert!(streams.iter().any(|s| s.ends_with("lidar")), "the original data is still there");
        assert_eq!(streams.len(), 7);

        let (restored, map) = load(&recording, Session::default()).unwrap().unwrap();
        assert_eq!(map.points.len(), visible);
        assert_eq!(restored.annotations, ws.session.annotations);
        assert_eq!(restored.plans.len(), 1);
        assert_eq!(restored.plans[0].cells, ws.session.plans[0].cells);
        // the restored map, put through its transform, lands where the saved one was
        assert_eq!(restored.erased, ws.session.erased);
        let mut rebuilt = Workspace::new(restored.clone(), Some(map.clone()));
        rebuilt.set_map(room_map());
        assert_eq!(rebuilt.carry_erased(), erased.changed, "erased voxels stay erased in a rebuilt map");
        let again = Workspace::new(restored, Some(map));
        let (_, a, _) = again.visible_points();
        let (_, b, _) = ws.visible_points();
        assert!((a[0][0] - b[0][0]).abs() < 1e-3 && (a[0][1] - b[0][1]).abs() < 1e-3);
    }

    #[test]
    fn saves_into_and_restores_from_a_db() {
        round_trip("db");
    }

    #[test]
    fn saves_into_and_restores_from_an_mcap() {
        round_trip("mcap");
    }
}
