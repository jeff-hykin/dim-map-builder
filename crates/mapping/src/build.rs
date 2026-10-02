//! "Build global map" from a recording, natively, in stages the UI shows with progress, ETA and cancel:
//! 1. read tf, pick the lidar stream and the world frame (dimos `map`'s rule: world, map, odom, in that order);
//! 2. loop closure: every scan's sensor pose (tf at the scan's time; a `.db` row's stored pose when tf can't place
//!    it) through the PGO port;
//! 3. ray tracing: every scan again, through the vendored dimos voxel ray tracer at its PGO-corrected pose, so free
//!    space a later scan sees through clears what moved;
//! 4. normals, for the cleanup tools and floor detection.
use crate::pgo::{Pgo, PgoConfig};
use crate::ray::mapper::{Mapper, Pose};
use crate::ray::voxel_ray_tracer::Config as RayConfig;
use crate::tf::{iso, parts, Iso, TfTree};
use anyhow::{bail, Result};
use dimos_recording::{Kind, Message, Recording};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

pub const WORLD_FRAMES: [&str; 3] = ["world", "map", "odom"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BuildOptions {
    /// the lidar stream; empty = the cloud stream with the most messages
    pub cloud_stream: String,
    /// the fixed frame; empty = world, map or odom, whichever places the clouds
    pub world_frame: String,
    pub voxel_size: f32,
    pub max_range: f32,
    pub loop_closure: bool,
    /// use every Nth scan (1 = all)
    pub every: usize,
    pub tf_tolerance: f64,
    pub pgo: PgoConfig,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            cloud_stream: String::new(),
            world_frame: String::new(),
            voxel_size: 0.05,
            max_range: 30.0,
            loop_closure: true,
            every: 1,
            tf_tolerance: 0.5,
            pgo: PgoConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub stage: String,
    pub stage_index: usize,
    pub stage_count: usize,
    pub done: u64,
    pub total: u64,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BuildResult {
    pub voxel_size: f32,
    pub world_frame: String,
    pub cloud_stream: String,
    pub points: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// sensor positions, odometry and corrected
    pub raw_path: Vec<[f32; 3]>,
    pub corrected_path: Vec<[f32; 3]>,
    /// keyframe poses after loop closure: ts, position, orientation (x, y, z, w)
    pub keyframes: Vec<(f64, [f64; 3], [f64; 4])>,
    pub loops: Vec<([f32; 3], [f32; 3])>,
    pub scans_used: usize,
    pub scans_skipped: usize,
    pub notes: Vec<String>,
}

#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// How each scan gets into the world.
enum Placement {
    /// tf from the cloud's frame to the world frame
    Tf(TfTree),
    /// the clouds are already in the world frame; the row's stored pose (if any) is the sensor origin
    World,
    /// the row's stored pose is the sensor pose in the world
    StoredPose,
}

fn pick_cloud_stream(recording: &Recording, wanted: &str) -> Result<(String, u64)> {
    let streams = recording.streams()?;
    if !wanted.is_empty() {
        let found = streams.iter().find(|s| s.name == wanted && s.kind == Kind::Cloud);
        let Some(found) = found else { bail!("no point cloud stream named {wanted}") };
        return Ok((found.name.clone(), found.count));
    }
    // the biggest cloud stream that isn't itself a map (a recorded global/local map would be circular)
    let mut clouds: Vec<_> = streams.iter().filter(|s| s.kind == Kind::Cloud).collect();
    clouds.sort_by_key(|s| (s.name.contains("map"), std::cmp::Reverse(s.count)));
    let Some(best) = clouds.first() else { bail!("this recording has no point clouds") };
    Ok((best.name.clone(), best.count))
}

pub fn build(recording: &Recording, options: &BuildOptions, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<BuildResult> {
    let stage_count = if options.loop_closure { 4 } else { 3 };
    let mut report = |stage: &str, index: usize, done: u64, total: u64, note: String| {
        progress(Progress { stage: stage.into(), stage_index: index, stage_count, done, total, note });
    };
    let check = || if cancel.load(Ordering::Relaxed) { Err(anyhow::anyhow!(Cancelled)) } else { Ok(()) };
    let mut result = BuildResult { voxel_size: options.voxel_size, ..Default::default() };

    // 1. tf + stream + frame
    report("Reading the recording", 0, 0, 1, String::new());
    let (stream, total) = pick_cloud_stream(recording, &options.cloud_stream)?;
    result.cloud_stream = stream.clone();
    let mut tree = TfTree::default();
    for edge in recording.tf_edges()? {
        tree.add(edge.header.ts(), &edge.header.frame_id, &edge.child, iso(edge.translation, edge.rotation));
    }
    tree.finish();
    check()?;
    let mut first: Option<(f64, String, Option<[f64; 7]>)> = None;
    recording.for_each(&stream, |ts, message, pose| {
        if let Message::Cloud(cloud) = message {
            first = Some((ts, cloud.frame_id, pose));
            return Ok(false);
        }
        Ok(true)
    })?;
    let Some((first_ts, cloud_frame, first_pose)) = first else { bail!("{stream} has no readable clouds") };
    let cloud_frame = cloud_frame.trim_start_matches('/').to_string();
    let mut world = options.world_frame.clone();
    if world.is_empty() {
        world = if WORLD_FRAMES.contains(&cloud_frame.as_str()) {
            cloud_frame.clone()
        } else {
            WORLD_FRAMES
                .iter()
                .find(|frame| tree.lookup(frame, &cloud_frame, first_ts, f64::INFINITY).is_some())
                .map(|frame| frame.to_string())
                .unwrap_or_else(|| "world".into())
        };
    }
    let placement = if cloud_frame == world || cloud_frame.is_empty() {
        result.notes.push(format!("clouds are already in {world}"));
        Placement::World
    } else if tree.lookup(&world, &cloud_frame, first_ts, f64::INFINITY).is_some() {
        result.notes.push(format!("placing {cloud_frame} clouds in {world} through tf"));
        Placement::Tf(tree)
    } else if first_pose.is_some() {
        result.notes.push(format!("no tf from {cloud_frame} to {world}: using the recording's stored scan poses"));
        Placement::StoredPose
    } else {
        bail!("can't place {cloud_frame} clouds: no tf to {} and no stored poses (tf frames: {})", WORLD_FRAMES.join("/"), tree.frames().join(", "));
    };
    result.world_frame = world.clone();
    report("Reading the recording", 0, 1, 1, result.notes.join("; "));

    let stored = |pose: Option<[f64; 7]>| pose.map(|p| iso([p[0], p[1], p[2]], [p[3], p[4], p[5], p[6]]));
    // the sensor's pose in the world for one scan, if it can be placed
    let sensor_pose = |ts: f64, frame: &str, pose: Option<[f64; 7]>| -> Option<Iso> {
        match &placement {
            Placement::Tf(tree) => tree.lookup(&world, frame.trim_start_matches('/'), ts, options.tf_tolerance).or(stored(pose)),
            Placement::World => stored(pose).or(Some(Iso::identity())),
            Placement::StoredPose => stored(pose),
        }
    };
    let every = options.every.max(1);

    // 2. loop closure
    let mut pgo = Pgo::new(options.pgo.clone());
    let mut stage = 1;
    if options.loop_closure {
        let mut index = 0u64;
        let mut loops = 0;
        recording.for_each(&stream, |ts, message, pose| {
            index += 1;
            if index % 64 == 0 {
                report("Closing loops", stage, index, total, format!("{} keyframes, {loops} loops", pgo.keyframes.len()));
                check()?;
            }
            let Message::Cloud(cloud) = message else { return Ok(true) };
            if (index - 1) % every as u64 != 0 {
                return Ok(true);
            }
            let Some(sensor) = sensor_pose(ts, &cloud.frame_id, pose) else { return Ok(true) };
            let body: Vec<[f32; 3]> = if matches!(placement, Placement::World) {
                let inverse = sensor.inverse();
                cloud.points.iter().map(|p| crate::tf::transform_point(&inverse, *p)).collect()
            } else {
                cloud.points
            };
            if pgo.process(sensor, ts, &body) {
                loops += 1;
            }
            Ok(true)
        })?;
        check()?;
        report("Closing loops", stage, total, total, format!("{} keyframes, {} loops", pgo.keyframes.len(), pgo.loops.len()));
        stage += 1;
    }

    // 3. ray tracing at the corrected poses
    let config = RayConfig {
        voxel_size: options.voxel_size,
        fine_divisor: 0,
        emit_fine: false,
        max_range: options.max_range,
        ray_subsample: 1,
        shadow_depth: 0.1,
        grace_depth: 0.2,
        min_health: -1,
        max_health: 5,
        graze_cos: 0.7,
        support_min: 4,
        emit_every: 0,
        global_emit_every: 0,
        region_percentile: 95.0,
        world_frame: world.clone(),
        tf_match_tolerance_s: options.tf_tolerance,
        worker_threads: std::thread::available_parallelism().map_or(4, |n| n.get() as u32).clamp(1, 8),
    };
    config.validate()?;
    let mut mapper = Mapper::new(config);
    let mut index = 0u64;
    let mut last_position: Option<[f32; 3]> = None;
    recording.for_each(&stream, |ts, message, pose| {
        index += 1;
        if index % 16 == 0 {
            report("Ray tracing the map", stage, index, total, format!("{} scans placed", result.scans_used));
            check()?;
        }
        let Message::Cloud(cloud) = message else { return Ok(true) };
        if (index - 1) % every as u64 != 0 {
            return Ok(true);
        }
        let Some(sensor) = sensor_pose(ts, &cloud.frame_id, pose) else {
            result.scans_skipped += 1;
            return Ok(true);
        };
        let correction = if options.loop_closure { pgo.correction_at(ts) } else { Iso::identity() };
        let corrected = correction * sensor;
        let raw_position = sensor.translation.vector;
        let corrected_position = corrected.translation.vector;
        let position = [corrected_position.x as f32, corrected_position.y as f32, corrected_position.z as f32];
        if last_position.is_none_or(|last| (0..3).map(|i| (last[i] - position[i]).powi(2)).sum::<f32>() > 0.0025) {
            result.raw_path.push([raw_position.x as f32, raw_position.y as f32, raw_position.z as f32]);
            result.corrected_path.push(position);
            last_position = Some(position);
        }
        let points: Vec<(f32, f32, f32)> = if matches!(placement, Placement::World) {
            let back = sensor.inverse();
            cloud.points.iter().map(|p| crate::tf::transform_point(&back, *p)).map(|p| (p[0], p[1], p[2])).collect()
        } else {
            cloud.points.iter().map(|p| (p[0], p[1], p[2])).collect()
        };
        let (t, q) = parts(&corrected);
        mapper.add_frame(points, Pose { position: (t[0] as f32, t[1] as f32, t[2] as f32), orientation: (q[0] as f32, q[1] as f32, q[2] as f32, q[3] as f32) });
        result.scans_used += 1;
        Ok(true)
    })?;
    check()?;
    let flat = mapper.global_points();
    result.points = flat.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    report("Ray tracing the map", stage, total, total, format!("{} voxels from {} scans", result.points.len(), result.scans_used));
    stage += 1;

    // 4. normals
    report("Estimating surfaces", stage, 0, 1, String::new());
    check()?;
    result.normals = crate::voxels::normals(&result.points, options.voxel_size * 3.5);
    result.keyframes = pgo.keyframes.iter().map(|k| { let (t, q) = parts(&k.optimized); (k.ts, t, q) }).collect();
    result.loops = pgo
        .loops
        .iter()
        .map(|l| {
            let a = pgo.keyframes[l.source].optimized.translation.vector;
            let b = pgo.keyframes[l.target].optimized.translation.vector;
            ([a.x as f32, a.y as f32, a.z as f32], [b.x as f32, b.y as f32, b.z as f32])
        })
        .collect();
    report("Estimating surfaces", stage, 1, 1, format!("{} loop closures", result.loops.len()));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dimos_recording::lcm::{encode_tf, encode_xyz_cloud, Header, TfEdge};

    /// a robot driving along a corridor; clouds in the lidar frame, odom -> base_link -> lidar via tf
    #[test]
    fn builds_a_corridor_from_a_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corridor.db");
        let mut db = rusqlite_free_db(&path);
        let mut clouds = Vec::new();
        let mut tfs = Vec::new();
        for step in 0..40 {
            let ts = step as f64 * 0.1;
            let x = step as f32 * 0.1;
            // walls at y = ±1.5 and a floor at z = -0.5 (relative to the sensor at height 0.5)
            let mut points = Vec::new();
            for i in -30..30 {
                let px = i as f32 * 0.1;
                for k in 0..10 {
                    points.push([px, 1.5, k as f32 * 0.1 - 0.4]);
                    points.push([px, -1.5, k as f32 * 0.1 - 0.4]);
                }
                for j in -14..15 {
                    points.push([px, j as f32 * 0.1, -0.5]);
                }
            }
            clouds.push((ts, encode_xyz_cloud(&Header::at(ts, "lidar"), &points, None)));
            tfs.push((
                ts,
                encode_tf(&[
                    TfEdge { header: Header::at(ts, "odom"), child: "base_link".into(), translation: [x as f64, 0.0, 0.0], rotation: [0.0, 0.0, 0.0, 1.0] },
                    TfEdge { header: Header::at(ts, "base_link"), child: "lidar".into(), translation: [0.0, 0.0, 0.5], rotation: [0.0, 0.0, 0.0, 1.0] },
                ]),
            ));
        }
        dimos_recording::db::write_stream(&mut db, "lidar", "sensor_msgs.PointCloud2", &clouds).unwrap();
        dimos_recording::db::write_stream(&mut db, "tf", "tf2_msgs.TFMessage", &tfs).unwrap();
        drop(db);
        let recording = Recording::open(&path).unwrap();
        let mut stages = Vec::new();
        let result = build(&recording, &BuildOptions { voxel_size: 0.1, ..Default::default() }, &mut |p| stages.push(p.stage), &AtomicBool::new(false)).unwrap();
        assert_eq!(result.world_frame, "odom");
        assert_eq!(result.scans_used, 40);
        assert!(result.points.len() > 1000, "{}", result.points.len());
        // walls at |y| = 1.5 and the floor at z = 0, in odom
        assert!(result.points.iter().any(|p| (p[1] - 1.5).abs() < 0.11));
        assert!(result.points.iter().all(|p| p[2] > -0.15), "nothing below the floor");
        assert!(stages.contains(&"Closing loops".to_string()));
        // cancel stops it
        let cancelled = build(&recording, &BuildOptions::default(), &mut |_| {}, &AtomicBool::new(true));
        assert!(cancelled.is_err_and(|e| e.is::<Cancelled>()));
    }

    fn rusqlite_free_db(path: &std::path::Path) -> dimos_recording::db::Connection {
        dimos_recording::db::Connection::open(path).unwrap()
    }
}
