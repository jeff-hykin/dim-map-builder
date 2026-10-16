//! "A corrected tf tree": loop closure over the trajectory of one point cloud's sensor, written back as one tf edge.
//!
//! The target is a cloud stream's sensor frame (the cloud's own frame, or one the user names when the cloud is
//! already in its tree's root: its points are moved back into the sensor frame first). Walking up the tree from it,
//! the first edge that moves is the odometry; its parent is the `parent` frame (usually odom), and the drift
//! correction is the edge above it, grandparent → parent (map → odom, world → odom): the one already there, else a new
//! root (`world` by default). Every scan's raw pose P(t) = parent ← sensor comes from tf; the solve finds the
//! corrected poses X(t) = grandparent ← sensor, and the edge written is C(t) = X(t) · P(t)⁻¹, sampled at a fixed rate
//! over the recording (tf_edit: in place, dtk's way).
//!
//! Two solvers:
//! - fast: the app's own loop closure (the PGO port Generate uses: scan-to-submap ICP on revisits) gives the loops,
//!   then one pose graph (gsc_pgo's anisotropic odometry noise) with them and, when asked, AprilTag landmark factors
//!   (gsc_pgo offline_pgo.py's quality-weighted factors) is solved here (graph.rs);
//! - high: the icp_stitch tool (gsc_pgo's offline pipeline in Rust, GTSAM): tag PGO, then ICP stitching between every
//!   pair of revisited submaps (thinned to one per `closure_spacing` m of path), re-solved. It runs on a temporary
//!   memory2 db holding just what it needs (the trajectory, the scans in the sensor frame, the camera's extrinsic and
//!   the tags), deleted afterwards; the server runs the binary (`server/src/icp_stitch.rs`).
use crate::apriltag::{rejected_by, Detection, GlimpseGates};
use crate::build::{Cancelled, Progress};
use crate::frames::TfAnalysis;
use crate::graph::{diagonal, Graph};
use crate::pgo::{Pgo, PgoConfig};
use crate::tf::{interpolate, iso, parts, Iso, TfTree};
use anyhow::{bail, Context, Result};
use dimos_recording::lcm::{self, Header, Odometry, Pose, TfEdge};
use dimos_recording::{db, Message, Recording};
use nalgebra::Matrix6;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// the app's loop closure, no ICP stitching
    #[default]
    Fast,
    /// icp_stitch: ICP stitching between revisited submaps
    High,
}

/// The ICP stitching's knobs (icp_stitch's `solve tuning`, gsc_pgo offline_pgo.py's Tuning).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StitchOptions {
    /// at most one stitch per this many meters of path (0 = no thinning): how often stitches may happen
    pub closure_spacing: f64,
    /// keyframes this close (m, corrected) are a revisit candidate...
    pub icp_radius_m: f64,
    /// ...when at least this far apart in time (s)
    pub icp_min_dt_s: f64,
    pub icp_max_corr_m: f64,
    /// the submaps' voxel (m)
    pub icp_voxel_m: f64,
    /// a stitch is accepted at this inlier share or more...
    pub icp_fitness_min: f64,
    /// ...and this inlier RMSE or less (m)
    pub icp_rmse_max_m: f64,
    pub icp_huber_delta: f64,
    pub icp_rot_var: f64,
    pub icp_trans_var: f64,
    /// scans within ± this of a keyframe make its submap (s)
    pub submap_half_s: f64,
    pub lm_max_iterations: usize,
    /// the scans handed to icp_stitch are thinned to this voxel first (m; well under icp_voxel_m)
    pub scan_voxel: f64,
}

impl Default for StitchOptions {
    fn default() -> Self {
        StitchOptions {
            closure_spacing: 2.0,
            icp_radius_m: 4.0,
            icp_min_dt_s: 25.0,
            icp_max_corr_m: 0.6,
            icp_voxel_m: 0.15,
            icp_fitness_min: 0.45,
            icp_rmse_max_m: 0.25,
            icp_huber_delta: 1.345,
            icp_rot_var: 4e-4,
            icp_trans_var: 2.5e-3,
            submap_half_s: 1.0,
            lm_max_iterations: 200,
            scan_voxel: 0.05,
        }
    }
}

/// Odometry between-factor variances: stiff roll/pitch and z, looser yaw and xy (gsc_pgo), so the graph absorbs drift
/// as heading error rather than stretching the path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OdometryNoise {
    pub roll_pitch_var: f64,
    pub yaw_var: f64,
    pub xy_var: f64,
    pub z_var: f64,
}

impl Default for OdometryNoise {
    fn default() -> Self {
        OdometryNoise { roll_pitch_var: 1e-8, yaw_var: 1e-5, xy_var: 1e-4, z_var: 1e-6 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CorrectOptions {
    /// the cloud stream whose sensor is corrected; empty = the default (frames.rs)
    pub cloud_stream: String,
    /// the sensor's frame, when the cloud's own frame is its tree's root
    pub sensor_frame: String,
    /// the frame above the odometry's parent when there's none: empty = "world"
    pub grandparent: String,
    /// AprilTag landmark factors from `raw_april_tags`
    pub use_tags: bool,
    pub exclude_tags: Vec<i64>,
    pub gates: GlimpseGates,
    pub quality: Quality,
    /// keyframe every (m) / (°)
    pub keyframe_translation_m: f64,
    pub keyframe_rotation_deg: f64,
    pub odometry: OdometryNoise,
    /// the fast solve's loop detection (the Generate loop closure's knobs)
    pub pgo: PgoConfig,
    pub stitch: StitchOptions,
    /// use every Nth scan
    pub every: usize,
    pub tf_tolerance: f64,
    /// the written edge's rate (Hz)
    pub rate_hz: f64,
}

impl Default for CorrectOptions {
    fn default() -> Self {
        CorrectOptions {
            cloud_stream: String::new(),
            sensor_frame: String::new(),
            grandparent: String::new(),
            use_tags: true,
            exclude_tags: Vec::new(),
            gates: GlimpseGates::default(),
            quality: Quality::Fast,
            keyframe_translation_m: 0.5,
            keyframe_rotation_deg: 10.0,
            odometry: OdometryNoise::default(),
            pgo: PgoConfig::default(),
            stitch: StitchOptions::default(),
            every: 1,
            tf_tolerance: 0.5,
            rate_hz: 10.0,
        }
    }
}

/// What the correction works on, resolved from the frame tree.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub cloud_stream: String,
    pub cloud_frame: String,
    /// the frame whose trajectory is corrected
    pub sensor_frame: String,
    /// the cloud is in its root frame: its points are moved back into sensor_frame
    pub compensate: bool,
    /// the odometry edge: parent ← odometry_child (the first edge above the sensor that moves)
    pub parent: String,
    pub odometry_child: String,
    /// the edge written: grandparent → parent
    pub grandparent: String,
    pub grandparent_exists: bool,
    pub notes: Vec<String>,
}

pub fn plan(analysis: &TfAnalysis, options: &CorrectOptions) -> Result<Plan> {
    let cloud_stream = if options.cloud_stream.is_empty() { analysis.default_cloud.clone().context("no point cloud stream is placed by tf")? } else { options.cloud_stream.clone() };
    let cloud = analysis.clouds.iter().find(|c| c.stream == cloud_stream).with_context(|| format!("no point cloud stream named {cloud_stream}"))?;
    if !cloud.in_tf {
        bail!("{cloud_stream}'s frame {} isn't in the tf tree", cloud.frame);
    }
    let mut notes = Vec::new();
    let (sensor_frame, compensate) = if cloud.is_root {
        let sensor = options.sensor_frame.trim_start_matches('/');
        if sensor.is_empty() {
            bail!("{cloud_stream} is in {}, the root of its tree: name the sensor's own frame (sensorFrame) to move its points back into", cloud.frame);
        }
        let node = analysis.frame(sensor).with_context(|| format!("no frame {sensor} in tf"))?;
        if node.root != cloud.frame || node.parent.is_none() {
            bail!("{sensor} isn't under {} in the tf tree", cloud.frame);
        }
        notes.push(format!("{cloud_stream} is in {} (the root): its points are moved back into {sensor} first", cloud.frame));
        (sensor.to_string(), true)
    } else {
        (cloud.frame.clone(), false)
    };
    // up to the first edge that moves
    let mut at = analysis.frame(&sensor_frame).context("sensor frame not in tf")?;
    loop {
        let Some(parent) = &at.parent else { bail!("nothing moves {sensor_frame}: every edge above it is static, so there's no odometry to correct") };
        if at.dynamic {
            break;
        }
        at = analysis.frame(parent).context("broken tf tree")?;
    }
    let odometry_child = at.name.clone();
    let parent = at.parent.clone().unwrap();
    if odometry_child != sensor_frame {
        notes.push(format!("{sensor_frame}'s own edge is static: the odometry is {parent} → {odometry_child}"));
    }
    let parent_node = analysis.frame(&parent).context("broken tf tree")?;
    let (grandparent, grandparent_exists) = match &parent_node.parent {
        Some(existing) => {
            notes.push(format!("{existing} → {parent} is already there: it's replaced"));
            (existing.clone(), true)
        }
        None => {
            let wanted = if options.grandparent.trim().is_empty() { "world".to_string() } else { options.grandparent.trim().trim_start_matches('/').to_string() };
            if analysis.frame(&wanted).is_some_and(|f| f.root == parent_node.root) {
                bail!("{wanted} is already in {parent}'s tree: pick another name for the frame above {parent}");
            }
            (wanted, false)
        }
    };
    Ok(Plan { cloud_stream, cloud_frame: cloud.frame.clone(), sensor_frame, compensate, parent, odometry_child, grandparent, grandparent_exists, notes })
}

/// One scan's place: its time (log), raw pose P (parent ← sensor), and its points in the sensor frame.
pub struct Scan {
    pub ts: f64,
    pub pose: Iso,
    pub points: Vec<[f32; 3]>,
}

/// Reads the plan's scans in order, each placed (scans tf can't place are skipped), and hands them to `each`.
/// Returns (used, skipped).
pub fn for_each_scan(recording: &Recording, tree: &TfTree, plan: &Plan, options: &CorrectOptions, progress: &mut dyn FnMut(u64, u64), cancel: &AtomicBool, mut each: impl FnMut(Scan) -> Result<()>) -> Result<(usize, usize)> {
    let total = recording.streams()?.iter().find(|s| s.name == plan.cloud_stream).map_or(0, |s| s.count);
    // the scans' clock: header stamps as they are, or moved onto the log clock when only that matches tf (build.rs)
    let mut samples = Vec::new();
    recording.for_each(&plan.cloud_stream, |ts, message, _| {
        if let Message::Cloud(cloud) = message {
            samples.push((ts, cloud.ts));
        }
        Ok(samples.len() < 200)
    })?;
    let shifted = crate::clock::offset(samples.iter().copied());
    let placed = |offset: f64| samples.iter().filter(|(_, stamp)| *stamp > 0.0 && tree.lookup(&plan.parent, &plan.sensor_frame, stamp + offset, options.tf_tolerance).is_some()).count();
    let offset = if shifted != 0.0 && placed(shifted) > placed(0.0) { shifted } else { 0.0 };
    let every = options.every.max(1) as u64;
    let (mut index, mut used, mut skipped) = (0u64, 0usize, 0usize);
    let mut failure = None;
    recording.for_each(&plan.cloud_stream, |ts, message, _| {
        index += 1;
        if index % 32 == 0 {
            progress(index, total);
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!(Cancelled));
            }
        }
        let Message::Cloud(cloud) = message else { return Ok(true) };
        if (index - 1) % every != 0 {
            return Ok(true);
        }
        let stamp = if cloud.ts > 0.0 { cloud.ts + offset } else { ts };
        let at = |target: &str, source: &str| tree.lookup(target, source, stamp, options.tf_tolerance).or_else(|| tree.lookup(target, source, ts, options.tf_tolerance));
        let Some(pose) = at(&plan.parent, &plan.sensor_frame) else {
            skipped += 1;
            return Ok(true);
        };
        let points = if plan.compensate {
            let Some(mount) = at(&plan.cloud_frame, &plan.sensor_frame) else {
                skipped += 1;
                return Ok(true);
            };
            let back = mount.inverse();
            cloud.points.iter().map(|p| crate::tf::transform_point(&back, *p)).collect()
        } else {
            cloud.points
        };
        used += 1;
        if let Err(error) = each(Scan { ts, pose, points }) {
            failure = Some(error);
            return Ok(false);
        }
        Ok(true)
    })?;
    if let Some(error) = failure {
        return Err(error);
    }
    progress(total, total);
    Ok((used, skipped))
}

/// The pose graph's keyframes after the solve: time, raw pose P, corrected pose X.
#[derive(Debug, Clone)]
pub struct Keyframe {
    pub ts: f64,
    pub raw: Iso,
    pub corrected: Iso,
}

/// What a solve found.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SolveReport {
    pub scans: usize,
    pub scans_skipped: usize,
    pub keyframes: usize,
    pub loops: usize,
    pub tag_factors: usize,
    pub markers: Vec<i64>,
    pub max_shift_m: f64,
    /// mean spread (m) of each revisited tag's world position over its visits: raw odometry, then corrected
    pub tag_spread_raw: Option<f64>,
    pub tag_spread_corrected: Option<f64>,
    pub notes: Vec<String>,
}

/// gsc_pgo offline_pgo.py's tag factor covariance: planar PnP error grows with range and reprojection error, so a far
/// or blurry glimpse counts for little.
pub fn tag_information(base_tag: &Iso, distance_m: f64, reproj_px: f64) -> Matrix6<f64> {
    let scale = ((distance_m.max(0.2) / 0.4).powi(2) * (reproj_px.max(0.5) / 1.0).powi(2)).max(0.25);
    let rotation = base_tag.rotation.to_rotation_matrix().into_inner();
    let mut covariance = Matrix6::<f64>::zeros();
    let rot = rotation * nalgebra::Matrix3::from_diagonal(&nalgebra::Vector3::new(0.04, 0.04, 0.0025)) * rotation.transpose();
    let trans = rotation * nalgebra::Matrix3::from_diagonal(&nalgebra::Vector3::new(0.0025, 0.0025, 0.25)) * rotation.transpose();
    covariance.fixed_view_mut::<3, 3>(0, 0).copy_from(&(rot * scale));
    covariance.fixed_view_mut::<3, 3>(3, 3).copy_from(&(trans * scale));
    covariance.try_inverse().unwrap_or_else(Matrix6::identity)
}

/// The raw pose at `ts` from the scans' trajectory (interpolated), None outside it.
pub fn pose_at(trajectory: &[(f64, Iso)], ts: f64) -> Option<Iso> {
    let index = trajectory.partition_point(|(t, _)| *t < ts);
    if index == 0 {
        return trajectory.first().filter(|(t, _)| (t - ts).abs() < 0.2).map(|(_, p)| *p);
    }
    if index >= trajectory.len() {
        return trajectory.last().filter(|(t, _)| (t - ts).abs() < 0.2).map(|(_, p)| *p);
    }
    let (t0, a) = trajectory[index - 1];
    let (t1, b) = trajectory[index];
    if t1 - t0 > 2.0 {
        return None;
    }
    Some(interpolate(&a, &b, if t1 > t0 { (ts - t0) / (t1 - t0) } else { 0.0 }))
}

/// The tag glimpses a solve uses: gated, excluded markers dropped, each with its pose in the sensor frame at its time
/// (sensor ← camera from tf, then camera ← tag), and its raw sensor pose.
pub struct TagSighting {
    pub detection: Detection,
    /// sensor ← tag
    pub sensor_tag: Iso,
    /// parent ← sensor at the glimpse
    pub raw_pose: Iso,
}

pub fn tag_sightings(tree: &TfTree, plan: &Plan, detections: &[Detection], camera_frame: &str, trajectory: &[(f64, Iso)], options: &CorrectOptions, notes: &mut Vec<String>) -> Vec<TagSighting> {
    let mut sightings = Vec::new();
    let mut unplaced = 0;
    for detection in detections {
        if options.exclude_tags.contains(&detection.marker_id) || rejected_by(detection, &options.gates).is_some() {
            continue;
        }
        let mount = tree.lookup(&plan.sensor_frame, camera_frame, detection.ts, options.tf_tolerance).or_else(|| tree.lookup(&plan.sensor_frame, camera_frame, detection.ts, f64::INFINITY));
        let (Some(mount), Some(raw_pose)) = (mount, pose_at(trajectory, detection.ts)) else {
            unplaced += 1;
            continue;
        };
        let t = detection.t_cam_marker;
        sightings.push(TagSighting { detection: detection.clone(), sensor_tag: mount * iso([t[0], t[1], t[2]], [t[3], t[4], t[5], t[6]]), raw_pose });
    }
    if unplaced > 0 {
        notes.push(format!("{unplaced} tag glimpses couldn't be placed (no tf from {camera_frame} to {}, or outside the scans)", plan.sensor_frame));
    }
    sightings
}

/// Mean over revisited tags (≥ 2 visits 20 s apart) of the spread of their per-visit world positions under `pose`.
pub fn tag_spread(sightings: &[TagSighting], pose: &dyn Fn(&TagSighting) -> Iso) -> Option<f64> {
    let mut by_marker: BTreeMap<i64, Vec<(f64, nalgebra::Vector3<f64>)>> = BTreeMap::new();
    for sighting in sightings {
        by_marker.entry(sighting.detection.marker_id).or_default().push((sighting.detection.ts, (pose(sighting) * sighting.sensor_tag).translation.vector));
    }
    let mut spreads = Vec::new();
    for (_, mut list) in by_marker {
        list.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut visits: Vec<Vec<nalgebra::Vector3<f64>>> = Vec::new();
        let mut last = f64::NEG_INFINITY;
        for (ts, position) in list {
            if ts - last > 20.0 {
                visits.push(Vec::new());
            }
            visits.last_mut().unwrap().push(position);
            last = ts;
        }
        if visits.len() < 2 {
            continue;
        }
        let centers: Vec<nalgebra::Vector3<f64>> = visits.iter().map(|v| v.iter().sum::<nalgebra::Vector3<f64>>() / v.len() as f64).collect();
        let mean = centers.iter().sum::<nalgebra::Vector3<f64>>() / centers.len() as f64;
        spreads.push((centers.iter().map(|c| (c - mean).norm_squared()).sum::<f64>() / centers.len() as f64).sqrt());
    }
    (!spreads.is_empty()).then(|| spreads.iter().sum::<f64>() / spreads.len() as f64)
}

/// The fast solve: the app's loop closure for loops, then one graph with them, gsc_pgo's odometry noise and the tags.
pub fn solve_fast(recording: &Recording, tree: &TfTree, plan: &Plan, options: &CorrectOptions, tags: Option<(&[Detection], &str)>, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<(Vec<Keyframe>, Vec<(f64, Iso)>, SolveReport)> {
    let mut pgo = Pgo::new(PgoConfig { key_pose_delta_trans: options.keyframe_translation_m, key_pose_delta_deg: options.keyframe_rotation_deg, ..options.pgo.clone() });
    let mut trajectory = Vec::new();
    let mut report = SolveReport::default();
    let counts = std::cell::Cell::new((0usize, 0usize));
    let (used, skipped) = for_each_scan(
        recording,
        tree,
        plan,
        options,
        &mut |done, total| {
            let (keyframes, loops) = counts.get();
            progress(Progress { overall: None, stage: "Closing loops".into(), stage_index: 0, stage_count: 2, done, total, note: format!("{keyframes} keyframes, {loops} loops") })
        },
        cancel,
        |scan| {
            pgo.process(scan.pose, scan.ts, &scan.points);
            counts.set((pgo.keyframes.len(), pgo.loops.len()));
            trajectory.push((scan.ts, scan.pose));
            Ok(())
        },
    )?;
    report.scans = used;
    report.scans_skipped = skipped;
    if pgo.keyframes.len() < 2 {
        bail!("{} gave {} keyframes: too little motion to correct", plan.cloud_stream, pgo.keyframes.len());
    }
    progress(Progress { overall: None, stage: "Solving the pose graph".into(), stage_index: 1, stage_count: 2, done: 0, total: 1, note: String::new() });
    let mut graph = Graph::default();
    for keyframe in &pgo.keyframes {
        graph.add(keyframe.optimized);
    }
    let noise = &options.odometry;
    let odometry = diagonal([noise.roll_pitch_var, noise.roll_pitch_var, noise.yaw_var, noise.xy_var, noise.xy_var, noise.z_var]);
    for k in 1..pgo.keyframes.len() {
        graph.between(k - 1, k, pgo.keyframes[k - 1].local.inverse() * pgo.keyframes[k].local, odometry, None);
    }
    for found in &pgo.loops {
        let rot = 1.0 / options.pgo.loop_rot_var;
        let trans = 1.0 / found.score.max(1e-4);
        graph.between(found.target, found.source, found.offset, Matrix6::from_diagonal(&nalgebra::Vector6::new(rot, rot, rot, trans, trans, trans)), None);
    }
    report.loops = pgo.loops.len();
    let times: Vec<f64> = pgo.keyframes.iter().map(|k| k.ts).collect();
    let mut sightings = Vec::new();
    if let Some((detections, camera_frame)) = tags {
        sightings = tag_sightings(tree, plan, detections, camera_frame, &trajectory, options, &mut report.notes);
        // one factor per (keyframe, marker): the glimpse with the lowest reprojection error
        let mut best: BTreeMap<(usize, i64), &TagSighting> = BTreeMap::new();
        for sighting in &sightings {
            let index = times.partition_point(|t| *t < sighting.detection.ts);
            let nearest = [index.saturating_sub(1), index.min(times.len() - 1)].into_iter().min_by(|a, b| (times[*a] - sighting.detection.ts).abs().total_cmp(&(times[*b] - sighting.detection.ts).abs())).unwrap();
            let entry = best.entry((nearest, sighting.detection.marker_id)).or_insert(sighting);
            if sighting.detection.reproj_px < entry.detection.reproj_px {
                *entry = sighting;
            }
        }
        let mut landmarks: BTreeMap<i64, usize> = BTreeMap::new();
        for ((keyframe, marker), sighting) in &best {
            // the tag from the keyframe, through odometry between the keyframe and the glimpse
            let base_tag = pgo.keyframes[*keyframe].local.inverse() * sighting.raw_pose * sighting.sensor_tag;
            let landmark = *landmarks.entry(*marker).or_insert_with(|| graph.add(pgo.keyframes[*keyframe].optimized * base_tag));
            graph.between(*keyframe, landmark, base_tag, tag_information(&base_tag, sighting.detection.distance_m, sighting.detection.reproj_px), None);
        }
        report.tag_factors = best.len();
        report.markers = landmarks.keys().copied().collect();
    }
    graph.optimize(options.stitch.lm_max_iterations.max(1), &|| cancel.load(Ordering::Relaxed));
    if cancel.load(Ordering::Relaxed) {
        return Err(anyhow::anyhow!(Cancelled));
    }
    let keyframes: Vec<Keyframe> = pgo.keyframes.iter().enumerate().map(|(k, keyframe)| Keyframe { ts: keyframe.ts, raw: keyframe.local, corrected: graph.values[k] }).collect();
    report.keyframes = keyframes.len();
    finish_report(&mut report, &keyframes, &sightings);
    progress(Progress { overall: None, stage: "Solving the pose graph".into(), stage_index: 1, stage_count: 2, done: 1, total: 1, note: format!("{} keyframes, {} loops, {} tag factors", report.keyframes, report.loops, report.tag_factors) });
    Ok((keyframes, trajectory, report))
}

/// The correction (grandparent ← parent) at `ts`, interpolated between keyframes, held past the ends.
pub fn correction_at(keyframes: &[Keyframe], ts: f64) -> Iso {
    let correction = |k: &Keyframe| k.corrected * k.raw.inverse();
    let index = keyframes.partition_point(|k| k.ts < ts);
    if index == 0 {
        return correction(&keyframes[0]);
    }
    if index >= keyframes.len() {
        return correction(&keyframes[keyframes.len() - 1]);
    }
    let (a, b) = (&keyframes[index - 1], &keyframes[index]);
    interpolate(&correction(a), &correction(b), if b.ts > a.ts { (ts - a.ts) / (b.ts - a.ts) } else { 0.0 })
}

pub fn finish_report(report: &mut SolveReport, keyframes: &[Keyframe], sightings: &[TagSighting]) {
    report.max_shift_m = keyframes.iter().map(|k| (k.corrected * k.raw.inverse()).translation.vector.norm()).fold(0.0, f64::max);
    if !sightings.is_empty() {
        report.tag_spread_raw = tag_spread(sightings, &|s| s.raw_pose);
        report.tag_spread_corrected = tag_spread(sightings, &|s| correction_at(keyframes, s.detection.ts) * s.raw_pose);
    }
}

/// The edge's samples: the correction every 1/rate s over [start, end], plus at every keyframe.
pub fn edge_samples(keyframes: &[Keyframe], start: f64, end: f64, rate_hz: f64) -> Vec<dimos_recording::tf_edit::Sample> {
    let step = 1.0 / rate_hz.clamp(0.1, 1000.0);
    let mut times: Vec<f64> = keyframes.iter().map(|k| k.ts).filter(|t| *t >= start && *t <= end).collect();
    let mut t = start;
    while t <= end {
        times.push(t);
        t += step;
    }
    times.push(end);
    times.sort_by(|a, b| a.total_cmp(b));
    times.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    times.into_iter().map(|t| {
        let (translation, rotation) = parts(&correction_at(keyframes, t));
        (t, translation, rotation)
    }).collect()
}

// ---- the high-quality solve's inputs and outputs (icp_stitch on a temporary db) --------------------------------

pub const STITCH_TRAJECTORY: &str = "trajectory";
pub const STITCH_SCANS: &str = "scans";

/// Writes the temporary db icp_stitch solves: the trajectory as odometry (parent ← sensor at every scan), the scans in
/// the sensor frame (thinned to scan_voxel), tf (the trajectory and, with tags, sensor → camera), and with tags the
/// CameraInfo and raw_april_tags (copied with their diagnostics). Returns the trajectory and the tag sightings.
pub fn write_stitch_inputs(
    recording: &Recording,
    tree: &TfTree,
    plan: &Plan,
    options: &CorrectOptions,
    tags: Option<(&[Detection], &str)>,
    work: &Path,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<(Vec<(f64, Iso)>, Vec<TagSighting>, SolveReport)> {
    let mut connection = db::Connection::open(work)?;
    for (name, kind) in [(STITCH_TRAJECTORY, lcm::ODOMETRY_TYPE), (STITCH_SCANS, lcm::POINT_CLOUD2_TYPE), ("tf", lcm::TF_TYPE)] {
        db::write_rows(&mut connection, name, kind, &[])?;
    }
    let camera = tags.map(|(_, frame)| frame.to_string());
    let mount = camera.as_ref().and_then(|frame| {
        let range = recording.time_range().ok().flatten()?;
        tree.lookup(&plan.sensor_frame, frame, (range.0 + range.1) / 2.0, f64::INFINITY)
    });
    let mut trajectory = Vec::new();
    let mut report = SolveReport::default();
    let mut pending: Vec<(db::NewRow, db::NewRow, db::NewRow)> = Vec::new();
    let flush = |connection: &mut db::Connection, pending: &mut Vec<(db::NewRow, db::NewRow, db::NewRow)>| -> Result<()> {
        let tx = connection.transaction()?;
        let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
        for (x, y, z) in pending.drain(..) {
            a.push(x);
            b.push(y);
            c.push(z);
        }
        db::append_rows(&tx, STITCH_TRAJECTORY, &a)?;
        db::append_rows(&tx, STITCH_SCANS, &b)?;
        db::append_rows(&tx, "tf", &c)?;
        tx.commit()?;
        Ok(())
    };
    let (used, skipped) = for_each_scan(
        recording,
        tree,
        plan,
        options,
        &mut |done, total| progress(Progress { overall: None, stage: "Preparing the scans for icp_stitch".into(), stage_index: 0, stage_count: 4, done, total, note: String::new() }),
        cancel,
        |scan| {
            let (t, q) = parts(&scan.pose);
            let pose = Pose { position: t, orientation: q };
            let odometry = lcm::encode_odometry(&Odometry { header: Header::at(scan.ts, &plan.parent), child_frame_id: plan.sensor_frame.clone(), pose });
            let thinned: Vec<[f32; 3]> = crate::icp::voxel_downsample(scan.points.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]), options.stitch.scan_voxel).into_iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
            let cloud = lcm::encode_xyz_cloud(&Header::at(scan.ts, &plan.sensor_frame), &thinned, None);
            let mut edges = vec![TfEdge { header: Header::at(scan.ts, &plan.parent), child: plan.sensor_frame.clone(), translation: t, rotation: q }];
            if let (Some(mount), Some(frame)) = (mount, &camera) {
                let (mt, mq) = parts(&mount);
                edges.push(TfEdge { header: Header::at(scan.ts, &plan.sensor_frame), child: frame.clone(), translation: mt, rotation: mq });
            }
            pending.push((db::NewRow::new(scan.ts, odometry), db::NewRow::new(scan.ts, cloud), db::NewRow::new(scan.ts, lcm::encode_tf(&edges))));
            trajectory.push((scan.ts, scan.pose));
            if pending.len() >= 256 {
                flush(&mut connection, &mut pending)?;
            }
            Ok(())
        },
    )?;
    flush(&mut connection, &mut pending)?;
    report.scans = used;
    report.scans_skipped = skipped;
    let mut sightings = Vec::new();
    if let Some((detections, camera_frame)) = tags {
        let (info_stream, info) = {
            let camera = crate::tags::pick_camera(recording, "").ok();
            crate::tags::camera_info(recording, camera.as_deref().unwrap_or(""), "")?
        };
        let _ = info_stream;
        db::write_rows(&mut connection, "camera_info", lcm::CAMERA_INFO_TYPE, &[db::NewRow::new(trajectory.first().map_or(0.0, |t| t.0), lcm::encode_camera_info(&lcm::CameraInfo { header: Header::at(info.header.ts(), camera_frame), ..info }))])?;
        let rows: Vec<db::NewRow> = detections
            .iter()
            .map(|d| {
                let p = d.t_cam_marker;
                db::NewRow {
                    ts: d.ts,
                    pose: Some(p),
                    tags: Some(serde_json::json!({ "marker_id": d.marker_id, "sharpness": d.sharpness, "reproj_px": d.reproj_px, "tag_px": d.tag_px, "lin_speed": d.lin_speed, "ang_speed": d.ang_speed })),
                    payload: lcm::encode_pose_stamped(&lcm::PoseStamped { header: Header::at(d.ts, camera_frame), pose: Pose { position: [p[0], p[1], p[2]], orientation: [p[3], p[4], p[5], p[6]] } }),
                }
            })
            .collect();
        db::write_rows(&mut connection, crate::tags::RAW_STREAM, lcm::POSE_STAMPED_TYPE, &rows)?;
        sightings = tag_sightings(tree, plan, detections, camera_frame, &trajectory, options, &mut report.notes);
    }
    Ok((trajectory, sightings, report))
}

/// icp_stitch's command line for the temporary db.
pub fn stitch_args(plan: &Plan, options: &CorrectOptions, work: &Path, camera_frame: Option<&str>) -> Vec<String> {
    let s = &options.stitch;
    let n = &options.odometry;
    let mut args: Vec<String> = vec![
        "--db".into(),
        work.display().to_string(),
        "--odom".into(),
        STITCH_TRAJECTORY.into(),
        "--lidar".into(),
        STITCH_SCANS.into(),
        "--odom-tf".into(),
        format!("{}:{}", plan.parent, plan.sensor_frame),
        "--world-frame".into(),
        plan.parent.clone(),
        "--no-lidar".into(),
        "--closure-spacing".into(),
        s.closure_spacing.to_string(),
    ];
    if let Some(frame) = camera_frame {
        args.extend(["--tag-frame".into(), frame.into()]);
        if !options.exclude_tags.is_empty() {
            args.extend(["--ignore-tags".into(), options.exclude_tags.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(",")]);
        }
    }
    for (flag, value) in [
        ("--keyframe-translation-m", options.keyframe_translation_m),
        ("--keyframe-rotation-deg", options.keyframe_rotation_deg),
        ("--lm-max-iterations", s.lm_max_iterations as f64),
        ("--odom-rot-roll-pitch-var", n.roll_pitch_var),
        ("--odom-rot-yaw-var", n.yaw_var),
        ("--odom-trans-xy-var", n.xy_var),
        ("--odom-trans-z-var", n.z_var),
        ("--icp-radius-m", s.icp_radius_m),
        ("--icp-min-dt-s", s.icp_min_dt_s),
        ("--icp-max-corr-m", s.icp_max_corr_m),
        ("--icp-voxel-m", s.icp_voxel_m),
        ("--icp-fitness-min", s.icp_fitness_min),
        ("--icp-rmse-max-m", s.icp_rmse_max_m),
        ("--icp-huber-delta", s.icp_huber_delta),
        ("--icp-rot-var", s.icp_rot_var),
        ("--icp-trans-var", s.icp_trans_var),
        ("--submap-half-s", s.submap_half_s),
    ] {
        args.push(flag.into());
        args.push(if flag == "--lm-max-iterations" { (value as i64).to_string() } else { value.to_string() });
    }
    args
}

/// icp_stitch's corrected trajectory (`<odom>_corrected`), matched to the raw one by time: keyframes for
/// correction_at.
pub fn read_stitch_output(work: &Path, trajectory: &[(f64, Iso)]) -> Result<Vec<Keyframe>> {
    let recording = Recording::open(work)?;
    let name = format!("{STITCH_TRAJECTORY}_corrected");
    let mut corrected = Vec::new();
    recording.for_each(&name, |ts, message, _| {
        if let Message::Odometry(odometry) = message {
            corrected.push((ts, iso(odometry.pose.position, odometry.pose.orientation)));
        }
        Ok(true)
    }).with_context(|| format!("icp_stitch wrote no {name}"))?;
    let mut keyframes = Vec::with_capacity(corrected.len());
    for (ts, x) in corrected {
        let index = trajectory.partition_point(|(t, _)| *t < ts - 1e-6);
        let Some((t, raw)) = trajectory.get(index) else { continue };
        if (t - ts).abs() < 1e-3 {
            keyframes.push(Keyframe { ts, raw: *raw, corrected: x });
        }
    }
    if keyframes.len() < 2 {
        bail!("icp_stitch's corrected trajectory didn't line up with the scans ({} matched)", keyframes.len());
    }
    Ok(keyframes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::analyze;

    #[test]
    fn plans_the_edge_above_the_odometry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.db");
        let mut connection = db::Connection::open(&path).unwrap();
        let cloud = |frame: &str| lcm::encode_xyz_cloud(&Header::at(1.0, frame), &[[1.0, 0.0, 0.0]], None);
        db::write_stream(&mut connection, "lidar", lcm::POINT_CLOUD2_TYPE, &[(1.0, cloud("mid360_link"))]).unwrap();
        db::write_stream(&mut connection, "registered", lcm::POINT_CLOUD2_TYPE, &[(1.0, cloud("odom"))]).unwrap();
        db::write_stream(&mut connection, "camera_points", lcm::POINT_CLOUD2_TYPE, &[(1.0, cloud("camera"))]).unwrap();
        drop(connection);
        let recording = Recording::open(&path).unwrap();
        let mut tree = TfTree::default();
        for step in 0..5 {
            let t = step as f64;
            tree.add(t, "odom", "mid360_link", iso([t, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));
            tree.add(t, "mid360_link", "base_link", iso([0.0, 0.0, -0.1], [0.0, 0.0, 0.0, 1.0]));
            tree.add(t, "base_link", "camera", iso([0.3, 0.0, 0.1], [0.0, 0.0, 0.0, 1.0]));
        }
        tree.finish();
        let analysis = analyze(&recording, &tree).unwrap();
        let default = plan(&analysis, &CorrectOptions::default()).unwrap();
        assert_eq!((default.cloud_stream.as_str(), default.sensor_frame.as_str(), default.parent.as_str(), default.grandparent.as_str(), default.grandparent_exists), ("lidar", "mid360_link", "odom", "world", false));
        // a cloud under static edges: the odometry is still odom → mid360_link
        let camera = plan(&analysis, &CorrectOptions { cloud_stream: "camera_points".into(), ..Default::default() }).unwrap();
        assert_eq!((camera.parent.as_str(), camera.odometry_child.as_str()), ("odom", "mid360_link"));
        // a cloud in the root frame needs the sensor's frame
        assert!(plan(&analysis, &CorrectOptions { cloud_stream: "registered".into(), ..Default::default() }).is_err());
        let registered = plan(&analysis, &CorrectOptions { cloud_stream: "registered".into(), sensor_frame: "mid360_link".into(), grandparent: "map".into(), ..Default::default() }).unwrap();
        assert!(registered.compensate);
        assert_eq!(registered.grandparent, "map");
    }

    /// clouds published already registered in odom (the root) give the same scans, moved back into the sensor frame
    #[test]
    fn a_root_frame_cloud_is_moved_back_into_the_sensor_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.db");
        let mut connection = db::Connection::open(&path).unwrap();
        let local: Vec<[f32; 3]> = vec![[1.0, 0.0, 0.0], [0.0, 2.0, 0.5]];
        let (mut sensor, mut registered, mut tfs) = (Vec::new(), Vec::new(), Vec::new());
        for step in 0..10 {
            let ts = step as f64 * 0.1;
            let pose = iso([step as f64 * 0.3, 0.1, 0.0], [0.0, 0.0, (step as f64 * 0.05).sin(), (step as f64 * 0.05).cos()]);
            sensor.push((ts, lcm::encode_xyz_cloud(&Header::at(ts, "lidar"), &local, None)));
            let placed: Vec<[f32; 3]> = local.iter().map(|p| crate::tf::transform_point(&pose, *p)).collect();
            registered.push((ts, lcm::encode_xyz_cloud(&Header::at(ts, "odom"), &placed, None)));
            let (t, q) = parts(&pose);
            tfs.push((ts, lcm::encode_tf(&[TfEdge { header: Header::at(ts, "odom"), child: "lidar".into(), translation: t, rotation: q }])));
        }
        db::write_stream(&mut connection, "lidar", lcm::POINT_CLOUD2_TYPE, &sensor).unwrap();
        db::write_stream(&mut connection, "registered", lcm::POINT_CLOUD2_TYPE, &registered).unwrap();
        db::write_stream(&mut connection, "tf", lcm::TF_TYPE, &tfs).unwrap();
        drop(connection);
        let recording = Recording::open(&path).unwrap();
        let tree = crate::frames::tf_tree(&recording).unwrap();
        let analysis = crate::frames::analyze(&recording, &tree).unwrap();
        let scans = |options: CorrectOptions| {
            let plan = plan(&analysis, &options).unwrap();
            let mut found = Vec::new();
            for_each_scan(&recording, &tree, &plan, &options, &mut |_, _| {}, &AtomicBool::new(false), |scan| {
                found.push(scan);
                Ok(())
            })
            .unwrap();
            (plan, found)
        };
        let (direct_plan, direct) = scans(CorrectOptions::default());
        let (moved_plan, moved) = scans(CorrectOptions { cloud_stream: "registered".into(), sensor_frame: "lidar".into(), ..Default::default() });
        assert_eq!(direct_plan.cloud_stream, "lidar");
        assert!(moved_plan.compensate && !direct_plan.compensate);
        assert_eq!((direct.len(), moved.len()), (10, 10));
        for (a, b) in direct.iter().zip(&moved) {
            assert!((a.pose.translation.vector - b.pose.translation.vector).norm() < 1e-9);
            for (p, q) in a.points.iter().zip(&b.points) {
                assert!((0..3).all(|i| (p[i] - q[i]).abs() < 1e-4), "{p:?} vs {q:?}");
            }
        }
    }

    #[test]
    fn edge_samples_cover_the_range_at_the_rate() {
        let keyframes = vec![
            Keyframe { ts: 1.0, raw: Iso::identity(), corrected: Iso::translation(1.0, 0.0, 0.0) },
            Keyframe { ts: 3.0, raw: Iso::identity(), corrected: Iso::translation(3.0, 0.0, 0.0) },
        ];
        let samples = edge_samples(&keyframes, 0.0, 4.0, 2.0);
        assert_eq!(samples.first().unwrap().0, 0.0);
        assert_eq!(samples.last().unwrap().0, 4.0);
        assert_eq!(samples.len(), 9);
        let at2 = samples.iter().find(|s| (s.0 - 2.0).abs() < 1e-9).unwrap();
        assert!((at2.1[0] - 2.0).abs() < 1e-9);
        assert!((samples[0].1[0] - 1.0).abs() < 1e-9, "held before the first keyframe");
    }
}
