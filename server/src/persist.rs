//! "Save into recording": the map and everything drawn on it, written into the opened file as new streams (.db) /
//! channels (.mcap, message_encoding "lcm") under the map's name, next to the recording's own data, as dimos message
//! types where one fits (schemas: docs/schema.md). A recording can hold several maps of the same space, so each one
//! is saved under a name of its own (`v1`, `v2`, ... by default: crate names), and every stream of it goes under that
//! name — `map/<name>/<leaf>` in an .mcap, `map_<name>_<leaf>` in a .db (a .db stream is a SQLite table name, so it
//! can't hold slashes; dimos reads an .mcap topic with its slashes folded to underscores, so a reader sees the same
//! stream either way). The map's tf frame is `map/<name>` in both.
//!   <map>/cloud            sensor_msgs.PointCloud2  the edited map, what dimos's relocalization loads as its premap
//!   <map>/voxels           sensor_msgs.PointCloud2  the edited voxel map, frame "map/<name>"
//!   <map>/path             nav_msgs.Path            the loop-closed sensor path, frame "map/<name>"
//!   <map>/annotations      std_msgs.String          JSON: map ← world transform, boxes, planes, points, polygons,
//!                                                   floors, named points, areas (no-go zones), views, build summary
//!   <map>/views            std_msgs.String          JSON: the saved 2D views (each a storey and a height band)
//!   <map>/slice            std_msgs.String          JSON: the slicer's view {zMin, zMax, yaw, xMin, xMax, yMin, yMax} or null
//!   <map>/floor_<n>        nav_msgs.OccupancyGrid   storey n's occupancy grid, when exported; origin z = the floor
//!   <map>/named_locations  visualization_msgs.InteractiveMarkerUpdate  the whole list of places, in the map's
//!                                                   coordinates: what the relocalization seeds its locations from,
//!                                                   so the saved map is a premap with its places
//! Saving again replaces that map's streams in a .db (and drops the `map_builder_*` streams older versions wrote); in
//! an .mcap it appends a newer message to each, in place (no copy of the recording), and loading takes the newest.
//! Opening a recording that has a saved map (and no working session) restores the session from it; the flat `map/...`
//! saves of versions before map names, and the older `map_builder` ones, still open — nothing written before map
//! names is moved or removed.
use crate::session::{Annotations, BuildSummary, MapData, Session, Transform};
use crate::workspace::Workspace;
use anyhow::{bail, Context, Result};
use dimos_recording::lcm::{self, Header, OccupancyGrid, Pose, PoseStamped};
use dimos_recording::{db, mcap_io, Format, Recording};
use mapping::floorplan::FloorPlan;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SCHEMA: &str = "dimos.map_builder.v1";

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

/// dimos's own named locations stream, as the robot recorded it (the recorder's .mcap channel; a .db stream has no
/// leading slash). A map's own list is written under its name instead; this one is only ever read, to seed it.
const NAMED_LOCATIONS: [&str; 2] = ["/named_locations", "named_locations"];

/// The leaves of one map's streams, spelled for the recording's format (crate::names).
#[derive(Clone)]
struct Streams {
    format: Format,
    map: String,
}

impl Streams {
    fn new(format: Format, map: &str) -> Streams {
        Streams { format, map: map.to_string() }
    }

    fn leaf(&self, leaf: &str) -> String {
        mapping::names::stream(self.format, &self.map, leaf)
    }

    fn floor(&self, index: usize) -> String {
        self.leaf(&format!("floor_{index}"))
    }

    /// what every floor stream of this map starts with (stale ones are dropped by it)
    fn floor_prefix(&self) -> String {
        let name = self.floor(0);
        name[..name.len() - 1].to_string()
    }

    /// the map's tf frame: `map/<name>`, whatever the format
    fn frame(&self) -> String {
        mapping::names::frame(&self.map)
    }
}

/// Reads dimos's newest `/named_locations` into the session, once (after that the session's list is the truth):
/// re-expressed from the recording's frame into the map frame, keeping the frame_id they're written back with. The
/// list's coordinates are taken as the recording's (on a new map, dimos's premap is the run's map frame). Only this
/// stream, never the map's own `named_locations`: that list is already in map coordinates and comes back with the
/// rest of the annotations, so reading it here would put it through `transform` a second time.
pub fn read_named_locations(recording: &Recording, session: &mut Session) {
    if session.annotations.named_locations_read {
        return;
    }
    session.annotations.named_locations_read = true;
    let latest = NAMED_LOCATIONS.iter().find_map(|name| recording.latest(name).ok().flatten());
    let Some((_, payload)) = latest else { return };
    match lcm::decode_named_locations(&payload) {
        Ok(list) => {
            let to_map = session.transform.iso();
            session.annotations.named_locations = list
                .into_iter()
                .enumerate()
                .map(|(index, l)| crate::session::NamedLocation {
                    id: format!("place-{index}"),
                    position: mapping::tf::transform_point(&to_map, [l.position[0] as f32, l.position[1] as f32, l.position[2] as f32]),
                    name: l.name,
                    frame: l.frame_id,
                })
                .collect();
            session.history.push(format!("Read {} named locations from the recording", session.annotations.named_locations.len()));
        }
        Err(error) => session.history.push(format!("Couldn't read the recording's named locations: {error:#}")),
    }
}

/// what saves before map names were called (voxels, path, annotations): the flat `map/` ones, then — older still —
/// the `map_builder` ones, and the prefix of those
fn legacy_names(format: Format) -> [(String, String, String, &'static str); 2] {
    let old = match format {
        Format::Db => "map_builder_",
        Format::Mcap => "/map_builder/",
    };
    [
        ("map/voxels".into(), "map/path".into(), "map/annotations".into(), "map/"),
        (format!("{old}global_map"), format!("{old}path"), format!("{old}annotations"), old),
    ]
}

/// Writes the session into its recording. `progress(done, total)` for the mcap rewrite.
pub fn save(workspace: &Workspace, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let session = &workspace.session;
    let path = Path::new(&session.recording_path);
    let format = dimos_recording::format_of(path)?;
    mapping::names::check(session.map())?;
    let streams = Streams::new(format, session.map());
    let legacy_prefix = legacy_names(format)[1].3;
    let now = crate::workspace::now_seconds();
    let header = Header::at(now, &streams.frame());
    let (_, points, _) = workspace.visible_points();
    let map_payload = lcm::encode_xyz_cloud(&header, &points, None);
    let corrected: Vec<[f32; 3]> = workspace.paths()["corrected"].as_array().map(|list| list.iter().filter_map(|p| serde_json::from_value(p.clone()).ok()).collect()).unwrap_or_default();
    let poses: Vec<PoseStamped> = corrected
        .iter()
        .map(|p| PoseStamped { header: header.clone(), pose: Pose { position: [p[0] as f64, p[1] as f64, p[2] as f64], orientation: [0.0, 0.0, 0.0, 1.0] } })
        .collect();
    let path_payload = lcm::encode_path(&header, &poses);
    let floors: Vec<(String, Vec<u8>)> = session.plans.iter().enumerate().map(|(index, plan)| (streams.floor(index), lcm::encode_occupancy_grid(&grid(plan, &header)))).collect();
    let state = SavedState {
        schema: SCHEMA.into(),
        saved_at: now,
        frame: streams.frame(),
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
        (streams.leaf("cloud"), lcm::POINT_CLOUD2_TYPE.into(), map_payload.clone()),
        (streams.leaf("voxels"), lcm::POINT_CLOUD2_TYPE.into(), map_payload),
        (streams.leaf("path"), lcm::PATH_TYPE.into(), path_payload),
        (streams.leaf("annotations"), lcm::STRING_TYPE.into(), annotations_payload),
        (streams.leaf("views"), lcm::STRING_TYPE.into(), lcm::encode_string(&serde_json::to_string(&session.annotations.views)?)),
        (streams.leaf("slice"), lcm::STRING_TYPE.into(), lcm::encode_string(&serde_json::to_string(&session.annotations.slice)?)),
    ];
    writes.extend(floors.into_iter().map(|(name, payload)| (name, lcm::OCCUPANCY_GRID_TYPE.into(), payload)));
    // the whole list, even when empty: a list emptied here replaces the recording's
    if session.annotations.named_locations_read || !session.annotations.named_locations.is_empty() {
        let locations: Vec<lcm::NamedLocation> = session
            .annotations
            .named_locations
            .iter()
            .map(|l| lcm::NamedLocation { name: l.name.clone(), frame_id: l.frame.clone(), position: [l.position[0] as f64, l.position[1] as f64, l.position[2] as f64], orientation: [0.0, 0.0, 0.0, 1.0] })
            .collect();
        writes.push((streams.leaf("named_locations"), lcm::INTERACTIVE_MARKER_UPDATE_TYPE.into(), lcm::encode_named_locations(&header, "map_builder", &locations)));
    }
    match format {
        Format::Db => {
        // earlier saves' floors beyond today's count go too
        let stale: Vec<String> = Recording::open(path)?
            .streams()?
            .into_iter()
            .map(|s| s.name)
            .filter(|name| (name.starts_with(&streams.floor_prefix()) || name.starts_with(legacy_prefix)) && !writes.iter().any(|(w, _, _)| w == name))
            .collect();
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
            // appended in place: an earlier save's messages stay (the newest wins when loading), stale floors with them
            progress(0, 1);
            let channels: Vec<mcap_io::NewChannel> = writes.into_iter().map(|(topic, kind, payload)| mcap_io::NewChannel { topic, kind, messages: vec![(now, payload)] }).collect();
            mcap_io::append(path, &channels)?;
            progress(1, 1);
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

/// The saved map a recording is reopened on: the newest named one (several maps = the one saved last), with the
/// streams its voxels, path and annotations are on; None when it has no named map.
fn newest_saved_map(recording: &Recording) -> Option<(String, Streams, Vec<u8>)> {
    let mut best: Option<(f64, String, Streams, Vec<u8>)> = None;
    for map in mapping::names::taken(recording) {
        let streams = Streams::new(recording.format(), &map);
        let name = streams.leaf("annotations");
        let Some((_, payload)) = recording.latest(&name).ok().flatten() else { continue };
        let saved_at = lcm::decode_string(&payload)
            .ok()
            .and_then(|text| serde_json::from_str::<SavedState>(&text).ok())
            .map_or(0.0, |state| state.saved_at);
        if best.as_ref().is_none_or(|(at, ..)| saved_at >= *at) {
            best = Some((saved_at, map, streams, payload));
        }
    }
    best.map(|(_, map, streams, payload)| (map, streams, payload))
}

/// A session rebuilt from an earlier save inside the recording, if it has one. A named map (the newest, when it has
/// several) comes first and the session takes its name; without one, the flat `map/...` of versions before map names,
/// then the older `map_builder` save.
pub fn load(recording: &Recording, mut session: Session) -> Result<Option<(Session, MapData)>> {
    let (map_name, path_name, annotations_name, payload) = match newest_saved_map(recording) {
        Some((map, streams, payload)) => {
            session.map_name = map;
            (streams.leaf("voxels"), streams.leaf("path"), streams.leaf("annotations"), payload)
        }
        None => {
            let legacy = legacy_names(recording.format());
            let found = legacy.iter().find_map(|(map, path, annotations, _)| {
                recording.latest(annotations).ok().flatten().map(|(_, payload)| (map.clone(), path.clone(), annotations.clone(), payload))
            });
            match found {
                Some(found) => found,
                None => return Ok(None),
            }
        }
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
    session.history.push(format!("Restored from the recording's saved map \"{}\" ({} voxels)", session.map(), n));
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
        // and dimos's named locations, as the robot recorded them (in its premap frame)
        let kitchen = lcm::NamedLocation { name: "kitchen".into(), frame_id: "premap".into(), position: [1.0, 2.0, 0.0], orientation: [0.0, 0.0, 0.0, 1.0] };
        let named = lcm::encode_named_locations(&Header::at(1.0, ""), "run", &[kitchen]);
        match format {
            "db" => {
                let mut connection = db::Connection::open(&path).unwrap();
                db::write_stream(&mut connection, "lidar", lcm::POINT_CLOUD2_TYPE, &[(1.0, cloud)]).unwrap();
                db::write_stream(&mut connection, "named_locations", lcm::INTERACTIVE_MARKER_UPDATE_TYPE, &[(1.0, named)]).unwrap();
            }
            _ => mcap_io::write_fixture(&path, &[("/lidar", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0, cloud), ("/named_locations", "lcm", lcm::INTERACTIVE_MARKER_UPDATE_TYPE, 1.0, named)]),
        }
        let mut session = Session { id: "s".into(), recording_path: path.display().to_string(), stage: "map".into(), map_name: "v1".into(), ..Default::default() };
        // opening reads them in, once
        read_named_locations(&Recording::open(&path).unwrap(), &mut session);
        assert_eq!(session.annotations.named_locations.iter().map(|l| (l.name.as_str(), l.frame.as_str(), l.position)).collect::<Vec<_>>(), vec![("kitchen", "premap", [1.0, 2.0, 0.0])]);
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
        ws.add_named_location("dock", [3.0, 1.0, 0.0]).unwrap();
        assert!(ws.add_named_location("dock", [0.0, 0.0, 0.0]).is_err(), "names are unique");
        let visible = ws.remaining();
        save(&ws, |_, _| {}).unwrap();
        save(&ws, |_, _| {}).unwrap(); // twice: replaces, never duplicates

        let recording = Recording::open(&path).unwrap();
        let streams: Vec<String> = recording.streams().unwrap().into_iter().map(|s| s.name).collect();
        // every stream of the map is under its name, spelled the way the format allows
        let own = Streams::new(recording.format(), "v1");
        for leaf in ["cloud", "voxels", "path", "annotations", "views", "slice", "floor_0", "named_locations"] {
            assert!(streams.contains(&own.leaf(leaf)), "{format}: {leaf} not in {streams:?}");
        }
        assert_eq!(mapping::names::taken(&recording), ["v1"], "{format}");
        assert!(streams.iter().any(|s| s.ends_with("lidar")), "the original data is still there");
        // nothing on the flat names of versions before map names
        assert!(!streams.iter().any(|s| s == mapping::pipeline::GLOBAL_MAP_STREAM), "{format}: {streams:?}");
        assert!(!streams.iter().any(|s| s.starts_with("map/voxels")), "{format}: {streams:?}");
        assert_eq!(streams.len(), 10, "{format}: {streams:?}");
        // the recording's own /named_locations is left as it was: the map's list is its own stream
        assert!(streams.iter().any(|s| s.trim_start_matches('/') == "named_locations"), "{format}: {streams:?}");
        // the named locations as saved (the map's coordinates, the frame they came in): what dimos seeds a premap's from
        let (_, payload) = recording.latest(&own.leaf("named_locations")).unwrap().unwrap();
        let saved = lcm::decode_named_locations(&payload).unwrap();
        let expected: Vec<_> = ws.session.annotations.named_locations.iter().map(|l| (l.name.clone(), l.frame.clone(), [l.position[0] as f64, l.position[1] as f64, l.position[2] as f64])).collect();
        assert_eq!(saved.iter().map(|l| (l.name.clone(), l.frame_id.clone(), l.position)).collect::<Vec<_>>(), expected);
        assert_eq!(expected.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["kitchen", "dock"]);
        assert_eq!(expected[1].1, "premap", "a new one takes the list's frame");
        assert!((expected[0].2[0] - 1.0).abs() > 0.1, "the kitchen turned with the map (rotate_yaw)");

        let (restored, map) = load(&recording, Session::default()).unwrap().unwrap();
        assert_eq!(restored.map_name, "v1", "{format}: reopening restores the map it was saved as");
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

    /// a second map of the same space, saved under its own name: the first one's streams stay as they were
    fn two_maps(format: &str) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("run.{format}"));
        let cloud = lcm::encode_xyz_cloud(&Header::at(1.0, "lidar"), &[[1.0, 0.0, 0.0]], None);
        match format {
            "db" => db::write_stream(&mut db::Connection::open(&path).unwrap(), "lidar", lcm::POINT_CLOUD2_TYPE, &[(1.0, cloud)]).unwrap(),
            _ => mcap_io::write_fixture(&path, &[("/lidar", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0, cloud)]),
        }
        let session = Session { id: "s".into(), recording_path: path.display().to_string(), stage: "map".into(), map_name: "v1".into(), ..Default::default() };
        let mut ws = Workspace::new(session, Some(room_map()));
        ws.add_box("table", Box3::from_bounds([2.0, 3.0, 0.7], [3.2, 3.8, 0.8]), "user").unwrap();
        save(&ws, |_, _| {}).unwrap();
        let first = ws.session.annotations.clone();
        // the same space again, under another name
        assert_eq!(ws.set_map_name("office").unwrap(), "office");
        ws.add_box("desk", Box3::from_bounds([0.0, 0.0, 0.7], [1.0, 1.0, 0.8]), "user").unwrap();
        save(&ws, |_, _| {}).unwrap();

        let recording = Recording::open(&path).unwrap();
        assert_eq!(mapping::names::taken(&recording), ["office", "v1"], "{format}");
        // each name's annotations are its own
        let read = |map: &str| {
            let name = Streams::new(recording.format(), map).leaf("annotations");
            let (_, payload) = recording.latest(&name).unwrap().unwrap();
            serde_json::from_str::<SavedState>(&lcm::decode_string(&payload).unwrap()).unwrap()
        };
        assert_eq!(read("v1").annotations.boxes.len(), 1, "{format}");
        assert_eq!(read("office").annotations.boxes.len(), 2, "{format}");
        assert_eq!(read("v1").annotations, first, "{format}: the first map is untouched");
        assert_eq!(read("v1").frame, "map/v1", "{format}");
        assert_eq!(read("office").frame, "map/office", "{format}");
        // reopening takes the one saved last
        let (restored, _) = load(&recording, Session::default()).unwrap().unwrap();
        assert_eq!(restored.map_name, "office", "{format}");
        // and the default name for a next map counts past both
        assert_eq!(mapping::names::default_for(&recording), "v2", "{format}");
    }

    #[test]
    fn two_maps_of_one_space_in_a_db() {
        two_maps("db");
    }

    #[test]
    fn two_maps_of_one_space_in_an_mcap() {
        two_maps("mcap");
    }

    /// a recording saved by a version before map names: it still opens, and saving it again goes under a name
    /// without moving or dropping what was there
    #[test]
    fn a_flat_save_from_before_map_names_still_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.mcap");
        // "map" is the frame a save from before map names was written in (today it is map/<name>)
        let header = Header::at(7.0, "map");
        let points: Vec<[f32; 3]> = (0..40).map(|i| [i as f32 * 0.05, 0.0, 0.0]).collect();
        let state = SavedState {
            schema: SCHEMA.into(),
            saved_at: 7.0,
            frame: "map".into(),
            world_frame: "world".into(),
            transform: Transform::default(),
            voxel_size: 0.05,
            annotations: Annotations { boxes: vec![crate::session::BoxAnnotation { id: "b1".into(), label: "chair".into(), ..Default::default() }], ..Default::default() },
            floor_streams: Vec::new(),
            build: None,
            erased: Vec::new(),
        };
        mcap_io::write_fixture(
            &path,
            &[
                ("/lidar", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0, lcm::encode_xyz_cloud(&Header::at(1.0, "lidar"), &[[1.0, 0.0, 0.0]], None)),
                ("map/voxels", "lcm", lcm::POINT_CLOUD2_TYPE, 7.0, lcm::encode_xyz_cloud(&header, &points, None)),
                ("map/annotations", "lcm", lcm::STRING_TYPE, 7.0, lcm::encode_string(&serde_json::to_string(&state).unwrap())),
                ("map_cloud", "lcm", lcm::POINT_CLOUD2_TYPE, 7.0, lcm::encode_xyz_cloud(&header, &points, None)),
            ],
        );
        let recording = Recording::open(&path).unwrap();
        assert_eq!(mapping::names::taken(&recording), Vec::<String>::new(), "a flat save is not a named map");
        let fresh = Session { id: "s".into(), recording_path: path.display().to_string(), map_name: mapping::names::default_for(&recording), ..Default::default() };
        let (restored, map) = load(&recording, fresh).unwrap().unwrap();
        assert_eq!(map.points.len(), points.len(), "the flat map/voxels is still read");
        assert_eq!(restored.annotations.boxes.len(), 1);
        assert_eq!(restored.map_name, "v1", "the next save goes under a name");
        drop(recording);

        // saving it again writes the named streams and leaves the flat ones alone
        let ws = Workspace::new(restored, Some(map));
        save(&ws, |_, _| {}).unwrap();
        let recording = Recording::open(&path).unwrap();
        let streams: Vec<String> = recording.streams().unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(mapping::names::taken(&recording), ["v1"]);
        for kept in ["map/voxels", "map/annotations", "map_cloud"] {
            assert!(streams.contains(&kept.to_string()), "{kept} is still there: {streams:?}");
        }
        assert!(streams.contains(&"map/v1/cloud".to_string()), "{streams:?}");
        // and reopening now takes the named map, not the flat one
        let (again, _) = load(&recording, Session::default()).unwrap().unwrap();
        assert_eq!(again.map_name, "v1");
    }

    /// a name that isn't a plain identifier is refused, not quietly turned into one
    #[test]
    fn a_name_that_cannot_be_a_frame_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.db");
        db::write_stream(&mut db::Connection::open(&path).unwrap(), "lidar", lcm::STRING_TYPE, &[(1.0, lcm::encode_string("x"))]).unwrap();
        let session = Session { id: "s".into(), recording_path: path.display().to_string(), stage: "map".into(), map_name: "v1".into(), ..Default::default() };
        let mut ws = Workspace::new(session, Some(room_map()));
        for bad in ["my map", "map/v1", "", "1v", "a-b"] {
            assert!(ws.set_map_name(bad).is_err(), "{bad:?} should be refused");
            assert_eq!(ws.session.map_name, "v1", "{bad:?} left the name as it was");
        }
        assert_eq!(ws.set_map_name("  office  ").unwrap(), "office", "the typed name is trimmed, never mangled");
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
