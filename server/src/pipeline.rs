//! The generation steps (README "Generate"): AprilTags from images, a corrected tf tree, the global map. Each runs on
//! its own as a background job with progress, ETA and cancel, and writes into the recording in place (a `.db`'s
//! streams replaced, an `.mcap` appended to). A step that needs an earlier one which the file doesn't have yet (read
//! from the file: its streams, its tf and `map/pipeline`) runs that one first, inside the same job.
use crate::app::{App, Job};
use crate::session::{BuildSummary, MapData};
use crate::workspace::now_seconds;
use anyhow::{bail, Context, Result};
use dimos_recording::Recording;
use mapping::build::{Cancelled, Progress};
use mapping::correct::{self, CorrectOptions, Quality};
use mapping::global::GlobalOptions;
use mapping::pipeline::{self, GlobalRecord, OdometryRecord, PipelineState, TagsRecord, TfRecord};
use mapping::pointlio;
use mapping::tags::{self, TagOptions};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// A share [lo, hi] of a job's progress bar, for a step inside it.
#[derive(Clone, Copy)]
pub struct Share<'a> {
    report: &'a dyn Fn(Progress),
    lo: f64,
    hi: f64,
    /// how a step's own progress becomes 0..1 within it
    weights: &'a [f64],
}

impl<'a> Share<'a> {
    pub fn whole(report: &'a dyn Fn(Progress)) -> Share<'a> {
        Share { report, lo: 0.0, hi: 1.0, weights: &[] }
    }

    pub fn part(&self, lo: f64, hi: f64, weights: &'a [f64]) -> Share<'a> {
        let span = self.hi - self.lo;
        Share { report: self.report, lo: self.lo + span * lo, hi: self.lo + span * hi, weights }
    }

    fn send(&self, progress: Progress) {
        let within = if progress.total > 0 { (progress.done as f64 / progress.total as f64).clamp(0.0, 1.0) } else { 0.0 };
        let fraction = match progress.overall {
            Some(overall) => overall,
            None if self.weights.is_empty() => (progress.stage_index as f64 + within) / progress.stage_count.max(1) as f64,
            None => {
                let index = progress.stage_index.min(self.weights.len() - 1);
                self.weights[..index].iter().sum::<f64>() + self.weights[index] * within
            }
        };
        (self.report)(Progress { overall: Some(self.lo + (self.hi - self.lo) * fraction.clamp(0.0, 1.0)), ..progress });
    }
}

const TAGS_WEIGHTS: [f64; 2] = [0.02, 0.98];
const FAST_WEIGHTS: [f64; 2] = [0.85, 0.15];
const HIGH_WEIGHTS: [f64; 4] = [0.3, 0.05, 0.55, 0.1];
const BUILD_WEIGHTS: [f64; 4] = [0.02, 0.43, 0.5, 0.05];
const BUILD_WEIGHTS_NO_LOOPS: [f64; 3] = [0.03, 0.9, 0.07];

fn generation() -> f64 {
    pipeline::now()
}

/// Removes a directory when dropped (the high-quality correction's temporary db, however the job ends).
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// AprilTags from images: detect, write raw_april_tags / april_tags, record it.
pub fn run_tags(path: &Path, options: &TagOptions, share: Share, cancel: &AtomicBool) -> Result<TagsRecord> {
    let share = share.part(0.0, 1.0, &TAGS_WEIGHTS);
    let recording = Recording::open(path)?;
    let result = tags::detect(&recording, options, &mut |p| share.send(p), cancel)?;
    drop(recording);
    share.send(Progress { overall: Some(1.0), stage: "Writing the tags into the recording".into(), stage_index: 1, stage_count: 2, done: 1, total: 1, note: String::new() });
    tags::write(path, &result, options, generation())?;
    let record = TagsRecord {
        at: now_seconds(),
        options: options.clone(),
        camera_stream: result.camera_stream.clone(),
        camera_frame: result.camera_frame.clone(),
        images: result.images,
        raw: result.raw.len(),
        grouped: result.grouped.len(),
        markers: result.markers.keys().copied().collect(),
    };
    pipeline::update(path, |state| state.tags = Some(record.clone()))?;
    Ok(record)
}

/// Point-LIO: odometry from the recording's lidar + IMU (mapping::pointlio), written into it in place, recorded.
/// Its deskewed scans are spooled through the data dir's `work` folder while it reads.
pub fn run_odometry(data_dir: &Path, path: &Path, share: Share, cancel: &AtomicBool) -> Result<OdometryRecord> {
    let recording = Recording::open(path)?;
    let streams = recording.streams()?;
    let inputs = pointlio::inputs(&streams, |s| s.count > 0).context("Point-LIO needs a lidar and an IMU, in a recording with no odometry yet")?;
    let tree = mapping::frames::tf_tree(&recording)?;
    let spool = (recording.format() == dimos_recording::Format::Mcap).then(|| TempDir(data_dir.join("work").join(format!("pointlio-{}", (now_seconds() * 1000.0) as u64))));
    if let Some(dir) = &spool {
        std::fs::create_dir_all(&dir.0)?;
    }
    let spool_file = spool.as_ref().map(|dir| dir.0.join("deskewed.spool"));
    let estimating = share.part(0.0, 0.9, &[]);
    let estimate = pointlio::estimate(&recording, &tree, &inputs, &pointlio::config(), spool_file.as_deref(), &mut |p| estimating.send(p), cancel)?;
    drop(recording);
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(anyhow::anyhow!(Cancelled));
    }
    share.send(Progress { overall: Some(0.92), stage: "Point-LIO: writing the odometry into the recording".into(), stage_index: 0, stage_count: 1, done: 0, total: 1, note: format!("{} poses", estimate.poses.len()) });
    let written = pointlio::write(path, &tree, &estimate, spool_file.as_deref(), generation())?;
    let mut notes = estimate.notes();
    notes.push(format!("Point-LIO: {} → {} on the tf, {} on {}{}", written.parent, written.child, written.poses, pointlio::ODOMETRY_TOPIC, if written.clouds > 0 { format!(", {} deskewed scans on {}", written.clouds, pointlio::CLOUD_TOPIC) } else { String::new() }));
    let record = OdometryRecord {
        at: now_seconds(),
        lidar: inputs.lidar,
        imu: inputs.imu,
        parent: written.parent,
        child: written.child,
        poses: written.poses,
        clouds: written.clouds,
        path_length: estimate.path_length,
        start_to_end: estimate.start_to_end(),
        notes,
    };
    pipeline::update(path, |state| state.odometry = Some(record.clone()))?;
    Ok(record)
}

/// Why Point-LIO should estimate the recording's odometry before its map can be built (mapping::pointlio::inputs: a
/// lidar and an IMU, no odometry), or None. From the streams alone: opening reads no tf.
pub fn odometry_pending(streams: &[dimos_recording::StreamInfo], has_messages: impl Fn(&dimos_recording::StreamInfo) -> bool) -> Option<String> {
    let inputs = pointlio::inputs(streams, has_messages)?;
    Some(format!("{} has an IMU ({}) and nothing places its scans yet: Point-LIO estimates the odometry first", inputs.lidar, inputs.imu))
}

/// Point-LIO first when asked for (`point_lio`) and the recording still has a lidar + IMU and no odometry: the rest
/// of the job gets the remaining `share`. The raw lidar a later step was told to follow becomes the deskewed copy.
fn odometry_first<'a>(data_dir: &Path, path: &Path, point_lio: bool, share: Share<'a>, cancel: &AtomicBool, cloud_streams: &mut [&mut String], notes: &mut Vec<String>) -> Result<Share<'a>> {
    if !point_lio || pointlio::inputs(&Recording::open(path)?.streams()?, |s| s.count > 0).is_none() {
        return Ok(share);
    }
    let record = run_odometry(data_dir, path, share.part(0.0, 0.4, &[]), cancel)?;
    for stream in cloud_streams.iter_mut() {
        if record.clouds > 0 && **stream == record.lidar {
            **stream = pointlio::CLOUD_TOPIC.to_string();
        }
    }
    notes.extend(record.notes);
    Ok(share.part(0.4, 1.0, &[]))
}

/// Whether the recording has the raw tags a tf correction with tags needs.
fn has_raw_tags(recording: &Recording) -> Result<bool> {
    Ok(recording.streams()?.iter().any(|s| s.name == tags::RAW_STREAM && s.count > 0))
}

/// A corrected tf tree: (tags first when asked for and missing), solve, write the grandparent → parent edge.
pub fn run_tf(data_dir: &Path, path: &Path, options: &CorrectOptions, tag_options: Option<&TagOptions>, point_lio: bool, share: Share, cancel: &AtomicBool) -> Result<TfRecord> {
    let mut options = options.clone();
    let mut odometry_notes = Vec::new();
    let mut share = odometry_first(data_dir, path, point_lio, share, cancel, &mut [&mut options.cloud_stream], &mut odometry_notes)?;
    let options = &options;
    let mut recording = Recording::open(path)?;
    let mut state = pipeline::read(&recording)?;
    if options.use_tags && !has_raw_tags(&recording)? {
        drop(recording);
        let tag_options = tag_options.cloned().or_else(|| state.tags.as_ref().map(|t| t.options.clone())).unwrap_or_default();
        let tag_options = TagOptions { raw: true, ..tag_options };
        let record = run_tags(path, &tag_options, share.part(0.0, 0.4, &[]), cancel)?;
        state.tags = Some(record);
        share = share.part(0.4, 1.0, &[]);
        recording = Recording::open(path)?;
    }
    let tree = mapping::frames::tf_tree(&recording)?;
    let analysis = mapping::frames::analyze(&recording, &tree)?;
    let plan = correct::plan(&analysis, options)?;
    let tag_data = if options.use_tags { tags::read(&recording, tags::RAW_STREAM)? } else { None };
    let tags_view = tag_data.as_ref().map(|(list, frame)| (list.as_slice(), frame.as_str()));
    let (keyframes, mut report) = match options.quality {
        Quality::Fast => {
            let share = share.part(0.0, 0.97, &FAST_WEIGHTS);
            let (keyframes, _, report) = correct::solve_fast(&recording, &tree, &plan, options, tags_view, &mut |p| share.send(p), cancel)?;
            (keyframes, report)
        }
        Quality::High => {
            let share = share.part(0.0, 0.97, &HIGH_WEIGHTS);
            let binary = crate::icp_stitch::binary(data_dir).context("getting icp_stitch")?;
            let log = data_dir.join("logs").join(format!("icp_stitch-{}.log", (now_seconds() * 1000.0) as u64));
            let in_place = correct::stitch_odometry_topic(&recording, &plan, options, tags_view)?.filter(|_| crate::icp_stitch::reads_mcap(&binary));
            let (keyframes, sightings, mut report, output) = match in_place {
                // on the .mcap itself, no copy: its outputs stay in it as derived topics (/<odometry>_corrected is the
                // corrected Odometry stream benchmarks read), next to the edge written below
                Some(odometry_topic) => {
                    drop(recording);
                    let args = correct::stitch_args_in_place(&plan, options, path, &odometry_topic);
                    let output = crate::icp_stitch::run(&binary, &args, &log, &|p| share.send(p), cancel)?;
                    recording = Recording::open(path)?;
                    let keyframes = correct::read_stitch_output_in_place(&recording, &odometry_topic)?;
                    let mut report = correct::SolveReport::default();
                    report.scans = recording.streams()?.iter().find(|s| s.name == plan.cloud_stream).map_or(0, |s| s.count as usize);
                    report.scans_skipped = crate::icp_stitch::scans_skipped(&output);
                    report.scans = report.scans.saturating_sub(report.scans_skipped);
                    report.notes.push(format!("icp_stitch ran on the recording itself from {odometry_topic}; its corrected copy is {}", correct::corrected_topic(&odometry_topic)));
                    (keyframes, Vec::new(), report, output)
                }
                None => {
                    let work = TempDir(data_dir.join("work").join(format!("icp_stitch-{}", (now_seconds() * 1000.0) as u64)));
                    std::fs::create_dir_all(&work.0)?;
                    let db = work.0.join("work.db");
                    let (trajectory, sightings, report) = correct::write_stitch_inputs(&recording, &tree, &plan, options, tags_view, &db, &mut |p| share.send(p), cancel)?;
                    let args = correct::stitch_args(&plan, options, &db, tags_view.map(|(_, frame)| frame));
                    let output = crate::icp_stitch::run(&binary, &args, &log, &|p| share.send(p), cancel)?;
                    (correct::read_stitch_output(&db, &trajectory)?, sightings, report, output)
                }
            };
            report.loops = crate::icp_stitch::accepted(&output);
            report.notes.extend(crate::icp_stitch::summary_lines(&output));
            report.keyframes = output.lines().find_map(|l| l.trim().strip_prefix("PGO: ")).and_then(|r| r.split_whitespace().next()).and_then(|n| n.parse().ok()).unwrap_or(0);
            report.tag_factors = output.lines().find_map(|l| l.trim().strip_prefix("PGO: ")).and_then(|r| r.split(", ").nth(1)).and_then(|r| r.split_whitespace().next()).and_then(|n| n.parse().ok()).unwrap_or(0);
            report.markers = tags_view.map(|(list, _)| {
                let mut ids: Vec<i64> = list.iter().map(|d| d.marker_id).filter(|id| !options.exclude_tags.contains(id)).collect();
                ids.sort();
                ids.dedup();
                ids
            }).unwrap_or_default();
            correct::finish_report(&mut report, &keyframes, &sightings);
            (keyframes, report)
        }
    };
    // the edge covers the odometry it corrects (and every keyframe), not the whole file: a CameraInfo stamped at the
    // start of a longer recording would otherwise stretch it back over time nothing moved in
    let odometry = tree.samples(&plan.parent, &plan.odometry_child);
    let first = odometry.first().map(|s| s.0).into_iter().chain(keyframes.first().map(|k| k.ts)).fold(f64::INFINITY, f64::min);
    let last = odometry.last().map(|s| s.0).into_iter().chain(keyframes.last().map(|k| k.ts)).fold(f64::NEG_INFINITY, f64::max);
    let (start, end) = if first.is_finite() && last >= first { (first, last) } else { recording.time_range()?.context("the recording is empty")? };
    drop(recording);
    let samples = correct::edge_samples(&keyframes, start, end, options.rate_hz);
    share.send(Progress { overall: Some(0.98), stage: format!("Writing {} → {} into the recording", plan.grandparent, plan.parent), stage_index: 0, stage_count: 1, done: 0, total: 1, note: String::new() });
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(anyhow::anyhow!(Cancelled));
    }
    let written = dimos_recording::tf_edit::replace_edge(path, &plan.grandparent, &plan.parent, &samples, generation())?;
    report.notes.push(format!("{} samples of {} → {} on {} ({} earlier transforms of it replaced)", written.written, plan.grandparent, plan.parent, written.stream, written.replaced));
    report.notes.splice(0..0, odometry_notes);
    let record = TfRecord { at: now_seconds(), options: options.clone(), plan, report, samples: written.written, tags_at: if options.use_tags { state.tags.as_ref().map(|t| t.at) } else { None } };
    pipeline::update(path, |state| state.tf = Some(record.clone()))?;
    Ok(record)
}

/// Whether the recording's tf has the corrected edge its state says was written.
pub fn has_correction(state: &PipelineState, tree: &mapping::tf::TfTree) -> bool {
    state.tf.as_ref().is_some_and(|tf| tree.edges().iter().any(|e| e.parent == tf.plan.grandparent && e.child == tf.plan.parent))
}

/// The global map: (the tf step first when the corrected tf is wanted and missing), build, decimate, write the stream,
/// the .pc2.lcm and maybe the .rrd. Returns the build (for the session's map) and the record.
pub fn run_global(data_dir: &Path, path: &Path, options: &GlobalOptions, point_lio: bool, share: Share, cancel: &AtomicBool) -> Result<(mapping::build::BuildResult, GlobalRecord, Vec<String>)> {
    let mut options = options.clone();
    let mut notes = Vec::new();
    let GlobalOptions { build, tf, .. } = &mut options;
    let mut streams: Vec<&mut String> = vec![&mut build.cloud_stream];
    if let Some(tf) = tf.as_mut() {
        streams.push(&mut tf.cloud_stream);
    }
    let mut share = odometry_first(data_dir, path, point_lio, share, cancel, &mut streams, &mut notes)?;
    drop(streams);
    let options = &options;
    let mut recording = Recording::open(path)?;
    let mut state = pipeline::read(&recording)?;
    if options.use_corrected_tf && !has_correction(&state, &mapping::frames::tf_tree(&recording)?) {
        drop(recording);
        let tf_options = options.tf.clone().or_else(|| state.tf.as_ref().map(|t| t.options.clone())).unwrap_or_else(|| default_correction(path));
        notes.push("no corrected tf in the recording yet: corrected it first".to_string());
        state.tf = Some(run_tf(data_dir, path, &tf_options, options.tags.as_ref(), false, share.part(0.0, 0.5, &[]), cancel)?);
        share = share.part(0.5, 1.0, &[]);
        recording = Recording::open(path)?;
    }
    let mut build = options.build.clone();
    if options.use_corrected_tf {
        let tf = state.tf.as_ref().context("no tf correction")?;
        build.world_frame = tf.plan.grandparent.clone();
        build.cloud_stream = tf.plan.cloud_stream.clone();
        build.sensor_frame = if tf.plan.compensate { tf.plan.sensor_frame.clone() } else { String::new() };
        build.loop_closure = false;
    } else if build.world_frame.is_empty() {
        // without the correction: placed in the frame it hangs off (odom), not through the corrected edge above it
        if let Some(tf) = state.tf.as_ref().filter(|_| has_correction(&state, &mapping::frames::tf_tree(&recording).unwrap_or_default())) {
            build.world_frame = tf.plan.parent.clone();
        }
    }
    let rrd = options.rrd.enabled;
    let build_share = share.part(0.0, if rrd { 0.75 } else { 0.95 }, if build.loop_closure { &BUILD_WEIGHTS } else { &BUILD_WEIGHTS_NO_LOOPS });
    let result = mapping::build::build(&recording, &build, &mut |p| build_share.send(Progress { overall: None, ..p }), cancel)?;
    let decimated = mapping::global::decimate(&result.points, result.voxel_size, &options.decimation);
    let (start, _) = recording.time_range()?.context("the recording is empty")?;
    let mut rrd_path = None;
    if rrd {
        let rrd_share = share.part(0.75, 0.97, &[]);
        let tree = mapping::frames::tf_tree(&recording)?;
        let tag_data = tags::read(&recording, tags::RAW_STREAM)?;
        let input = mapping::rrd::RrdInput { recording: &recording, tree: &tree, world: &result.world_frame, map: &decimated, raw_path: &result.raw_path, corrected_path: &result.corrected_path, tags: tag_data };
        rrd_path = Some(mapping::rrd::write(path, &input, &options.rrd, &mut |p| rrd_share.send(p), cancel)?.display().to_string());
    }
    drop(recording);
    share.send(Progress { overall: Some(0.98), stage: "Writing the global map".into(), stage_index: 0, stage_count: 1, done: 0, total: 1, note: format!("{} points", decimated.len()) });
    let lcm = mapping::global::write(path, &decimated, &result.world_frame, start, generation())?;
    let record = GlobalRecord {
        at: now_seconds(),
        points: decimated.len(),
        voxel_size: if options.decimation.voxel > 0.0 { options.decimation.voxel } else { result.voxel_size },
        world_frame: result.world_frame.clone(),
        cloud_stream: result.cloud_stream.clone(),
        tf_at: if options.use_corrected_tf { state.tf.as_ref().map(|t| t.at) } else { None },
        lcm_path: lcm.display().to_string(),
        rrd_path,
    };
    pipeline::update(path, |state| state.global_map = Some(record.clone()))?;
    notes.push(format!("global_map_aggregated: {} points (from {} voxels), {}", decimated.len(), result.points.len(), lcm.display()));
    Ok((result, record, notes))
}

/// The tf correction's defaults for this recording: tags when it has them or a camera to find them in, gated the way
/// the tags step last gated them.
pub fn default_correction(path: &Path) -> CorrectOptions {
    match Recording::open(path) {
        Ok(recording) => default_correction_for(&recording),
        Err(_) => CorrectOptions::default(),
    }
}

/// `default_correction` for a recording already open (a .db counts its streams once per opening).
pub fn default_correction_for(recording: &Recording) -> CorrectOptions {
    let can_tag = has_raw_tags(recording).unwrap_or(false) || tags::pick_camera(recording, "").and_then(|camera| tags::camera_info(recording, &camera, "")).is_ok();
    let gates = pipeline::read(recording).ok().and_then(|state| state.tags).map(|tags| tags.options.gates).unwrap_or_default();
    CorrectOptions { use_tags: can_tag, gates, ..Default::default() }
}

/// What the Generate dialog shows: what the recording has, what each step would do by default, what ran before.
pub fn status(data_dir: &Path, path: &Path) -> Result<Value> {
    let recording = Recording::open(path)?;
    let state = pipeline::read(&recording)?;
    let streams = recording.streams()?;
    let tree = mapping::frames::tf_tree(&recording)?;
    let analysis = mapping::frames::analyze(&recording, &tree)?;
    let images: Vec<Value> = streams.iter().filter(|s| s.kind == dimos_recording::Kind::Image && s.count > 0).map(|s| json!({ "name": s.name, "count": s.count })).collect();
    let camera = tags::pick_camera(&recording, "").ok();
    let intrinsics = camera.as_ref().map(|camera| tags::camera_info(&recording, camera, "").map(|(stream, info)| json!({ "stream": stream, "frame": info.header.frame_id, "width": info.width, "height": info.height })).map_err(|e| format!("{e:#}")));
    let count = |name: &str| streams.iter().find(|s| s.name == name).map_or(0, |s| s.count);
    let raw = tags::read(&recording, tags::RAW_STREAM)?;
    let mut markers: Vec<(i64, usize)> = Vec::new();
    if let Some((list, _)) = &raw {
        let mut by: std::collections::BTreeMap<i64, usize> = Default::default();
        for d in list {
            *by.entry(d.marker_id).or_default() += 1;
        }
        markers = by.into_iter().collect();
    }
    let tf_defaults = default_correction_for(&recording);
    let plan = correct::plan(&analysis, &tf_defaults);
    let corrected = has_correction(&state, &tree);
    let tags_at = state.tags.as_ref().map(|t| t.at);
    let tf_stale = state.tf.as_ref().is_some_and(|tf| tf.tags_at.is_some() && tags_at.is_some_and(|at| Some(at) != tf.tags_at));
    let global_stale = state.global_map.as_ref().is_some_and(|g| g.tf_at.is_some() && state.tf.as_ref().map(|t| t.at) != g.tf_at);
    let lcm = mapping::global::lcm_path(path);
    let rrd = mapping::rrd::path_for(path);
    let can_stitch = crate::icp_stitch::available(data_dir);
    // Point-LIO is offered for a lidar + IMU with no odometry whose lidar nothing in tf moves
    let odometry_inputs = pointlio::inputs(&streams, |s| s.count > 0);
    let lidar_frame = odometry_inputs.as_ref().and_then(|i| analysis.clouds.iter().find(|c| c.stream == i.lidar)).map(|c| c.frame.clone());
    let odometry_needed = odometry_inputs.is_some() && lidar_frame.as_ref().is_none_or(|frame| !pointlio::moved_by_tf(&tree, frame));
    Ok(json!({
        "format": format!("{:?}", recording.format()).to_lowercase(),
        "streams": streams,
        "odometry": {
            "inputs": odometry_inputs,
            "lidarFrame": lidar_frame,
            "needed": odometry_needed,
            "record": state.odometry,
        },
        "tags": {
            "images": images,
            "camera": camera,
            "cameraInfo": intrinsics.as_ref().map(|r| r.as_ref().ok()),
            "problem": match &intrinsics {
                None => Some("no image stream to find markers in".to_string()),
                Some(Err(error)) => Some(error.clone()),
                _ => None,
            },
            "raw": count(tags::RAW_STREAM),
            "grouped": count(tags::GROUPED_STREAM),
            "markers": markers.iter().map(|(id, n)| json!({ "id": id, "sightings": n })).collect::<Vec<_>>(),
            "record": state.tags,
            "defaults": TagOptions::default(),
            "dictionaries": mapping::apriltag::dictionaries(),
        },
        "tf": {
            "analysis": analysis,
            "plan": plan.as_ref().ok(),
            "planProblem": plan.as_ref().err().map(|e| format!("{e:#}")),
            "corrected": corrected,
            "stale": tf_stale,
            "record": state.tf,
            "defaults": tf_defaults,
            "highQuality": can_stitch,
        },
        "globalMap": {
            "present": count(pipeline::GLOBAL_MAP_STREAM) > 0,
            "lcmPath": lcm.display().to_string(),
            "lcmExists": lcm.is_file(),
            "rrdPath": rrd.display().to_string(),
            "rrdExists": rrd.is_file(),
            "stale": global_stale,
            "record": state.global_map,
            "defaults": GlobalOptions::default(),
            "rrdStreams": mapping::rrd::default_streams(&recording)?,
        },
    }))
}

impl App {
    fn pipeline_job(self: &Arc<Self>, id: &str, kind: &str, work: impl FnOnce(&Arc<App>, &Path, Share, &AtomicBool) -> Result<String> + Send + 'static) -> Result<Job> {
        let shared = self.require(id)?;
        {
            let ws = shared.lock().unwrap();
            if !ws.session.writable {
                bail!("{} is in a read-only folder: the Map Editor writes into recordings in place and never copies them; move it into the recordings folder (Recordings can) and open it from there", ws.session.recording_path);
            }
        }
        let session_id = id.to_string();
        self.start_job(id, kind, move |app, report, cancel| {
            let shared = app.require(&session_id)?;
            let path = PathBuf::from(shared.lock().unwrap().session.recording_path.clone());
            let note = work(app, &path, Share::whole(report), cancel)?;
            app.invalidate_preview(&session_id);
            // odometry written (Point-LIO): not pending any more
            let pending = Recording::open(&path).and_then(|r| r.streams()).ok().and_then(|streams| odometry_pending(&streams, |s| s.count > 0));
            let mut ws = shared.lock().unwrap();
            ws.odometry_pending = pending;
            ws.session.history.push(note);
            app.store.save(&ws.session)?;
            app.emit(json!({ "type": "session", "id": ws.session.id, "revision": ws.session.revision }));
            app.emit(json!({ "type": "recording-changed", "path": path }));
            Ok(())
        })
    }

    /// The step list's slow part in the background: the recording's tf, read once for every later reader.
    pub fn read_steps(self: &Arc<Self>, id: &str, path: PathBuf) -> Result<Job> {
        self.start_job(id, "read", move |_, report, _| {
            App::read_tf(&Recording::open(&path)?, report, 1.0)?;
            Ok(())
        })
    }

    /// Point-LIO on its own (Generate's step 0).
    pub fn generate_odometry(self: &Arc<Self>, id: &str) -> Result<Job> {
        self.pipeline_job(id, "odometry", move |app, path, share, cancel| {
            let record = run_odometry(&app.store.dir, path, share, cancel)?;
            Ok(record.notes.join("; "))
        })
    }

    pub fn generate_tags(self: &Arc<Self>, id: &str, options: TagOptions, point_lio: bool) -> Result<Job> {
        self.pipeline_job(id, "tags", move |app, path, share, cancel| {
            let mut notes = Vec::new();
            let share = odometry_first(&app.store.dir, path, point_lio, share, cancel, &mut [], &mut notes)?;
            let record = run_tags(path, &options, share, cancel)?;
            notes.push(format!("{}: {} raw, {} grouped over {} images (markers {:?})", options.dictionary, record.raw, record.grouped, record.images, record.markers));
            Ok(notes.join("; "))
        })
    }

    pub fn generate_tf(self: &Arc<Self>, id: &str, options: CorrectOptions, tag_options: Option<TagOptions>, point_lio: bool) -> Result<Job> {
        self.pipeline_job(id, "tf", move |app, path, share, cancel| {
            let record = run_tf(&app.store.dir, path, &options, tag_options.as_ref(), point_lio, share, cancel)?;
            let r = &record.report;
            Ok(format!(
                "Corrected tf: {} → {} ({} keyframes, {} loops, {} tag factors, max shift {:.2} m{})",
                record.plan.grandparent,
                record.plan.parent,
                r.keyframes,
                r.loops,
                r.tag_factors,
                r.max_shift_m,
                match (r.tag_spread_raw, r.tag_spread_corrected) {
                    (Some(a), Some(b)) => format!(", tag spread {a:.3} → {b:.3} m"),
                    _ => String::new(),
                }
            ))
        })
    }

    pub fn generate_global(self: &Arc<Self>, id: &str, options: GlobalOptions, point_lio: bool) -> Result<Job> {
        let session_id = id.to_string();
        self.pipeline_job(id, "globalMap", move |app, path, share, cancel| {
            let started = now_seconds();
            let (result, record, notes) = run_global(&app.store.dir, path, &options, point_lio, share, cancel)?;
            let chained = notes.iter().any(|n| n.starts_with("no corrected tf"));
            app.install_map(&session_id, result, options.build.clone(), notes, started)?;
            Ok(format!(
                "Global map{}: {} points in {}, written as global_map_aggregated and {}",
                if chained { " (the tf was corrected first)" } else { "" },
                record.points,
                record.world_frame,
                record.lcm_path
            ))
        })
    }

    /// A build's result becomes the session's map (what Generate does when it finishes).
    pub fn install_map(&self, id: &str, result: mapping::build::BuildResult, options: mapping::build::BuildOptions, notes: Vec<String>, started: f64) -> Result<()> {
        let shared = self.require(id)?;
        let n = result.points.len();
        let map = MapData { voxel_size: result.voxel_size, points: result.points, normals: result.normals, removed: vec![false; n], raw_path: result.raw_path, corrected_path: result.corrected_path, loops: result.loops };
        let summary = BuildSummary {
            voxel_size: result.voxel_size,
            world_frame: result.world_frame,
            cloud_stream: result.cloud_stream,
            scans_used: result.scans_used,
            scans_skipped: result.scans_skipped,
            loops: map.loops.len(),
            notes: notes.into_iter().chain(result.notes).collect(),
            seconds: now_seconds() - started,
        };
        let mut workspace = shared.lock().unwrap();
        if workspace.discarded {
            return Ok(());
        }
        workspace.set_map(map);
        let carried = workspace.carry_erased();
        if let Some(map) = &workspace.map {
            self.store.save_map(&workspace.session.id, map)?;
        }
        let session = &mut workspace.session;
        session.build = Some(summary.clone());
        session.build_options = Some(options);
        session.undo.clear();
        session.redo.clear();
        session.plans.clear();
        session.revision += 1;
        session.history.push(format!("Built the global map: {n} voxels, {} loop closures, {} scans in {:.0} s", summary.loops, summary.scans_used, summary.seconds));
        if carried > 0 {
            session.history.push(format!("Kept {carried} erased voxels erased"));
        }
        self.store.save(session)?;
        self.emit(json!({ "type": "session", "id": session.id, "revision": session.revision }));
        Ok(())
    }
}
