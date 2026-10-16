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
use mapping::pipeline::{self, GlobalRecord, PipelineState, TagsRecord, TfRecord};
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

    fn part(&self, lo: f64, hi: f64, weights: &'a [f64]) -> Share<'a> {
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

/// Whether the recording has the raw tags a tf correction with tags needs.
fn has_raw_tags(recording: &Recording) -> Result<bool> {
    Ok(recording.streams()?.iter().any(|s| s.name == tags::RAW_STREAM && s.count > 0))
}

/// A corrected tf tree: (tags first when asked for and missing), solve, write the grandparent → parent edge.
pub fn run_tf(data_dir: &Path, path: &Path, options: &CorrectOptions, tag_options: Option<&TagOptions>, share: Share, cancel: &AtomicBool) -> Result<TfRecord> {
    let mut recording = Recording::open(path)?;
    let mut state = pipeline::read(&recording)?;
    let mut share = share;
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
            let work = TempDir(data_dir.join("work").join(format!("icp_stitch-{}", (now_seconds() * 1000.0) as u64)));
            std::fs::create_dir_all(&work.0)?;
            let db = work.0.join("work.db");
            let (trajectory, sightings, mut report) = correct::write_stitch_inputs(&recording, &tree, &plan, options, tags_view, &db, &mut |p| share.send(p), cancel)?;
            let args = correct::stitch_args(&plan, options, &db, tags_view.map(|(_, frame)| frame));
            let output = crate::icp_stitch::run(&binary, &args, &|p| share.send(p), cancel)?;
            let keyframes = correct::read_stitch_output(&db, &trajectory)?;
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
pub fn run_global(data_dir: &Path, path: &Path, options: &GlobalOptions, share: Share, cancel: &AtomicBool) -> Result<(mapping::build::BuildResult, GlobalRecord, Vec<String>)> {
    let mut recording = Recording::open(path)?;
    let mut state = pipeline::read(&recording)?;
    let mut share = share;
    let mut notes = Vec::new();
    if options.use_corrected_tf && !has_correction(&state, &mapping::frames::tf_tree(&recording)?) {
        drop(recording);
        let tf_options = options.tf.clone().or_else(|| state.tf.as_ref().map(|t| t.options.clone())).unwrap_or_else(|| default_correction(path));
        notes.push("no corrected tf in the recording yet: corrected it first".to_string());
        state.tf = Some(run_tf(data_dir, path, &tf_options, options.tags.as_ref(), share.part(0.0, 0.5, &[]), cancel)?);
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
    let Ok(recording) = Recording::open(path) else { return CorrectOptions::default() };
    let can_tag = has_raw_tags(&recording).unwrap_or(false) || tags::pick_camera(&recording, "").and_then(|camera| tags::camera_info(&recording, &camera, "")).is_ok();
    let gates = pipeline::read(&recording).ok().and_then(|state| state.tags).map(|tags| tags.options.gates).unwrap_or_default();
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
    let tf_defaults = default_correction(path);
    let plan = correct::plan(&analysis, &tf_defaults);
    let corrected = has_correction(&state, &tree);
    let tags_at = state.tags.as_ref().map(|t| t.at);
    let tf_stale = state.tf.as_ref().is_some_and(|tf| tf.tags_at.is_some() && tags_at.is_some_and(|at| Some(at) != tf.tags_at));
    let global_stale = state.global_map.as_ref().is_some_and(|g| g.tf_at.is_some() && state.tf.as_ref().map(|t| t.at) != g.tf_at);
    let lcm = mapping::global::lcm_path(path);
    let rrd = mapping::rrd::path_for(path);
    let can_stitch = crate::icp_stitch::available(data_dir);
    Ok(json!({
        "format": format!("{:?}", recording.format()).to_lowercase(),
        "streams": streams,
        "tags": {
            "images": images,
            "camera": camera,
            "cameraInfo": intrinsics.as_ref().map(|r| r.as_ref().ok()),
            "problem": match &intrinsics {
                None => Some("no image stream to find AprilTags in".to_string()),
                Some(Err(error)) => Some(error.clone()),
                _ => None,
            },
            "raw": count(tags::RAW_STREAM),
            "grouped": count(tags::GROUPED_STREAM),
            "markers": markers.iter().map(|(id, n)| json!({ "id": id, "sightings": n })).collect::<Vec<_>>(),
            "record": state.tags,
            "defaults": TagOptions::default(),
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
            let mut ws = shared.lock().unwrap();
            ws.session.history.push(note);
            app.store.save(&ws.session)?;
            app.emit(json!({ "type": "session", "id": ws.session.id, "revision": ws.session.revision }));
            app.emit(json!({ "type": "recording-changed", "path": path }));
            Ok(())
        })
    }

    pub fn generate_tags(self: &Arc<Self>, id: &str, options: TagOptions) -> Result<Job> {
        self.pipeline_job(id, "tags", move |_, path, share, cancel| {
            let record = run_tags(path, &options, share, cancel)?;
            Ok(format!("AprilTags: {} raw, {} grouped over {} images (markers {:?})", record.raw, record.grouped, record.images, record.markers))
        })
    }

    pub fn generate_tf(self: &Arc<Self>, id: &str, options: CorrectOptions, tag_options: Option<TagOptions>) -> Result<Job> {
        self.pipeline_job(id, "tf", move |app, path, share, cancel| {
            let record = run_tf(&app.store.dir, path, &options, tag_options.as_ref(), share, cancel)?;
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

    pub fn generate_global(self: &Arc<Self>, id: &str, options: GlobalOptions) -> Result<Job> {
        let session_id = id.to_string();
        self.pipeline_job(id, "globalMap", move |app, path, share, cancel| {
            let started = now_seconds();
            let (result, record, notes) = run_global(&app.store.dir, path, &options, share, cancel)?;
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
