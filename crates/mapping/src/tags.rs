//! "AprilTags from images": every image of a camera stream through the detector (`apriltag.rs`), posed with the
//! recording's CameraInfo, written back into the recording as `raw_april_tags` (every glimpse, with the gate
//! diagnostics) and `april_tags` (glimpses that pass the gates, grouped: one Huber medoid per marker visit), the
//! streams dimos's `add_april.py` writes. Both are geometry_msgs.PoseStamped of the tag in the camera's optical frame
//! (header frame), at the image's time; the marker id and diagnostics are the row's tags in a `.db`, and a JSON
//! string per message on `<stream>/tags` in an `.mcap`.
//!
//! The camera's speed (the motion gate) comes from tf: the camera frame's pose at each image's time. The `.pose` an
//! image row may carry is never used (old recordings stored a buggy odometry there).
use crate::apriltag::{decode_gray, group, CameraModel, Detection, GlimpseGates, GroupReport, Grouping, TagDetector};
use crate::build::{Cancelled, Progress};
use crate::tf::TfTree;
use anyhow::{bail, Context, Result};
use dimos_recording::lcm::{self, Header, Pose, PoseStamped};
use dimos_recording::{db, mcap_io, Format, Kind, Message, Recording};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub const RAW_STREAM: &str = "raw_april_tags";
pub const GROUPED_STREAM: &str = "april_tags";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TagOptions {
    /// the image stream; empty = a `color_image`-like one
    pub camera_stream: String,
    /// the CameraInfo stream; empty = `<camera>_camera_info`, else `camera_info`, else any CameraInfo stream
    pub camera_info_stream: String,
    /// the tag's black square, edge to edge (m)
    pub marker_length: f64,
    pub dictionary: String,
    /// write raw_april_tags
    pub raw: bool,
    /// write april_tags (grouped)
    pub grouped: bool,
    pub gates: GlimpseGates,
    pub grouping: Grouping,
    /// markers on things that move: kept in the raw stream, left out of the grouped one
    pub exclude: Vec<i64>,
    /// look at every Nth image (1 = all)
    pub every: usize,
}

impl Default for TagOptions {
    fn default() -> Self {
        TagOptions {
            camera_stream: String::new(),
            camera_info_stream: String::new(),
            marker_length: 0.10,
            dictionary: "DICT_APRILTAG_36h11".into(),
            raw: true,
            grouped: true,
            gates: GlimpseGates::default(),
            grouping: Grouping::default(),
            exclude: Vec::new(),
            every: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TagResult {
    pub camera_stream: String,
    pub camera_info_stream: String,
    pub camera_frame: String,
    pub images: usize,
    pub undecodable: usize,
    pub raw: Vec<Detection>,
    pub grouped: Vec<Detection>,
    pub report: GroupReport,
    /// marker id → raw glimpses
    pub markers: std::collections::BTreeMap<i64, usize>,
    pub notes: Vec<String>,
}

/// The camera stream: the one asked for, else an image stream named like a color camera, else the biggest one.
pub fn pick_camera(recording: &Recording, wanted: &str) -> Result<String> {
    let images: Vec<_> = recording.streams()?.into_iter().filter(|s| s.kind == Kind::Image && s.count > 0).collect();
    if !wanted.is_empty() {
        return images.iter().find(|s| s.name == wanted).map(|s| s.name.clone()).with_context(|| format!("no image stream named {wanted}"));
    }
    let mut ranked = images.clone();
    ranked.sort_by_key(|s| (!s.name.contains("color"), s.name.contains("depth"), std::cmp::Reverse(s.count)));
    ranked.first().map(|s| s.name.clone()).context("this recording has no images to find markers in")
}

/// The CameraInfo for `camera`: (stream, intrinsics).
pub fn camera_info(recording: &Recording, camera: &str, wanted: &str) -> Result<(String, lcm::CameraInfo)> {
    let infos: Vec<String> = recording.streams()?.into_iter().filter(|s| s.kind == Kind::CameraInfo && s.count > 0).map(|s| s.name).collect();
    let tried: Vec<String> = if wanted.is_empty() {
        let base = camera.trim_end_matches("/image_raw").trim_end_matches("/image").trim_end_matches("/compressed");
        let mut tried = vec![format!("{camera}_camera_info"), format!("{base}/camera_info"), "camera_info".into(), "/camera_info".into()];
        tried.extend(infos.iter().cloned());
        tried
    } else {
        vec![wanted.to_string()]
    };
    for name in tried.iter().filter(|name| infos.contains(name)) {
        let mut found = None;
        recording.for_each(name, |_, message, _| {
            if let Message::CameraInfo(info) = message {
                found = Some(info);
                return Ok(false);
            }
            Ok(true)
        })?;
        if let Some(info) = found {
            if info.k[0] > 0.0 {
                return Ok((name.clone(), info));
            }
        }
    }
    bail!("no CameraInfo for {camera} (looked for {}): the tags can't be posed without the camera's intrinsics", tried.iter().take(3).cloned().collect::<Vec<_>>().join(", "))
}

/// The camera's (m/s, °/s) at each image time from its pose in tf (against the image before it, or after it when the
/// one before is more than a second away), None where tf can't place it.
fn speeds(tree: &TfTree, root: &str, camera_frame: &str, times: &[f64]) -> Vec<Option<(f64, f64)>> {
    let poses: Vec<_> = times.iter().map(|t| tree.lookup(root, camera_frame, *t, 0.2)).collect();
    let between = |a: usize, b: usize| -> Option<(f64, f64)> {
        let dt = times[b] - times[a];
        if !(1e-6..=1.0).contains(&dt) {
            return None;
        }
        let motion = poses[a]?.inverse() * poses[b]?;
        Some((motion.translation.vector.norm() / dt, motion.rotation.angle().to_degrees() / dt))
    };
    (0..times.len()).map(|k| (k > 0).then(|| between(k - 1, k)).flatten().or_else(|| (k + 1 < times.len()).then(|| between(k, k + 1)).flatten())).collect()
}

/// Finds the tags in every image (every Nth) and groups them; nothing is written.
pub fn detect(recording: &Recording, options: &TagOptions, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<TagResult> {
    let camera_stream = pick_camera(recording, &options.camera_stream)?;
    let (info_stream, info) = camera_info(recording, &camera_stream, &options.camera_info_stream)?;
    let camera = CameraModel::from_info(&info);
    let total = recording.streams()?.iter().find(|s| s.name == camera_stream).map_or(0, |s| s.count);
    let mut result = TagResult { camera_stream: camera_stream.clone(), camera_info_stream: info_stream, camera_frame: info.header.frame_id.trim_start_matches('/').to_string(), ..Default::default() };
    progress(Progress { overall: None, stage: "Reading tf".into(), stage_index: 0, stage_count: 2, done: 0, total: 1, note: String::new() });
    let tree = crate::frames::tf_tree(recording)?;
    let every = options.every.max(1);
    // batches of decoded-on-demand frames, detected in parallel (a detector per thread)
    let mut batch: Vec<(f64, lcm::Image)> = Vec::new();
    let mut index = 0u64;
    let mut root: Option<Option<String>> = None;
    let mut process = |batch: &mut Vec<(f64, lcm::Image)>, result: &mut TagResult| -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        if result.camera_frame.is_empty() {
            result.camera_frame = batch[0].1.header.frame_id.trim_start_matches('/').to_string();
        }
        let frame = result.camera_frame.clone();
        let root = root.get_or_insert_with(|| crate::frames::root_of(&tree, &frame));
        let times: Vec<f64> = batch.iter().map(|(ts, _)| *ts).collect();
        let speed = match root {
            Some(root) => speeds(&tree, root, &frame, &times),
            None => vec![None; times.len()],
        };
        let found: Vec<Result<Option<Vec<Detection>>>> = batch
            .par_iter()
            .zip(speed.par_iter())
            .map_init(
                || TagDetector::new(&options.dictionary),
                |detector, ((ts, image), speed)| {
                    let detector = detector.as_mut().map_err(|e| anyhow::anyhow!("{e}"))?;
                    let Ok((gray, width, height)) = decode_gray(image) else { return Ok(None) };
                    Ok(Some(detector.detect(*ts, &gray, width, height, &camera, options.marker_length, speed.unwrap_or((-1.0, -1.0)))?))
                },
            )
            .collect();
        for found in found {
            match found? {
                Some(list) => result.raw.extend(list),
                None => result.undecodable += 1,
            }
        }
        result.images += batch.len();
        batch.clear();
        Ok(())
    };
    recording.for_each(&camera_stream, |ts, message, _| {
        index += 1;
        let Message::Image(image) = message else { return Ok(true) };
        if (index - 1) % every as u64 != 0 {
            return Ok(true);
        }
        batch.push((ts, image));
        if batch.len() >= 64 {
            process(&mut batch, &mut result)?;
            progress(Progress { overall: None, stage: format!("Finding markers in {camera_stream}"), stage_index: 1, stage_count: 2, done: index, total, note: format!("{} tags in {} images", result.raw.len(), result.images) });
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!(Cancelled));
            }
        }
        Ok(true)
    })?;
    process(&mut batch, &mut result)?;
    if result.images == 0 {
        bail!("{camera_stream} has no images");
    }
    if result.raw.iter().all(|d| d.lin_speed < 0.0) {
        result.notes.push(format!("tf doesn't place {}: the motion gate is off", result.camera_frame));
    }
    result.raw.sort_by(|a, b| a.ts.total_cmp(&b.ts));
    for detection in &result.raw {
        *result.markers.entry(detection.marker_id).or_default() += 1;
    }
    let (grouped, report) = group(&result.raw, &options.gates, &options.grouping, &options.exclude);
    result.grouped = grouped;
    result.report = report;
    progress(Progress { overall: None, stage: format!("Finding markers in {camera_stream}"), stage_index: 1, stage_count: 2, done: total, total, note: format!("{} raw, {} grouped", result.raw.len(), result.grouped.len()) });
    Ok(result)
}

fn raw_tags(detection: &Detection) -> serde_json::Value {
    serde_json::json!({
        "marker_id": detection.marker_id,
        "sharpness": detection.sharpness,
        "reproj_px": detection.reproj_px,
        "tag_px": detection.tag_px,
        "distance_m": detection.distance_m,
        "view_angle_deg": detection.view_angle_deg,
        "lin_speed": detection.lin_speed,
        "ang_speed": detection.ang_speed,
    })
}

fn grouped_tags(detection: &Detection) -> serde_json::Value {
    serde_json::json!({ "marker_id": detection.marker_id, "n_observations": detection.observations })
}

fn message(detection: &Detection, frame: &str) -> Vec<u8> {
    let p = detection.t_cam_marker;
    lcm::encode_pose_stamped(&PoseStamped { header: Header::at(detection.ts, frame), pose: Pose { position: [p[0], p[1], p[2]], orientation: [p[3], p[4], p[5], p[6]] } })
}

/// Writes the streams `options` asks for into the recording at `path` (a `.db` replaces them, an `.mcap` appends a
/// newer generation).
pub fn write(path: &Path, result: &TagResult, options: &TagOptions, generation: f64) -> Result<Vec<String>> {
    let mut outputs: Vec<(&str, &Vec<Detection>, fn(&Detection) -> serde_json::Value)> = Vec::new();
    if options.raw {
        outputs.push((RAW_STREAM, &result.raw, raw_tags));
    }
    if options.grouped {
        outputs.push((GROUPED_STREAM, &result.grouped, grouped_tags));
    }
    let frame = &result.camera_frame;
    match dimos_recording::format_of(path)? {
        Format::Db => {
            let mut connection = db::Connection::open(path)?;
            for (name, list, tags) in &outputs {
                let rows: Vec<db::NewRow> = list.iter().map(|d| db::NewRow { ts: d.ts, pose: Some(d.t_cam_marker), tags: Some(tags(d)), payload: message(d, frame) }).collect();
                db::write_rows(&mut connection, name, lcm::POSE_STAMPED_TYPE, &rows)?;
            }
        }
        Format::Mcap => {
            let mut channels = Vec::new();
            for (name, list, tags) in &outputs {
                channels.push(mcap_io::GeneratedChannel::lcm(name, lcm::POSE_STAMPED_TYPE, generation, list.iter().map(|d| (d.ts, message(d, frame))).collect()));
                channels.push(mcap_io::GeneratedChannel::lcm(&format!("{name}/tags"), lcm::STRING_TYPE, generation, list.iter().map(|d| (d.ts, lcm::encode_string(&tags(d).to_string()))).collect()));
            }
            mcap_io::append_generated(path, &channels)?;
        }
    }
    Ok(outputs.iter().map(|(name, list, _)| format!("{name}: {}", list.len())).collect())
}

/// A tag stream read back: its detections (diagnostics where the stream has them) and the camera frame they're in.
pub fn read(recording: &Recording, stream: &str) -> Result<Option<(Vec<Detection>, String)>> {
    if !recording.streams()?.iter().any(|s| s.name == stream && s.count > 0) {
        return Ok(None);
    }
    let mut frame = String::new();
    let mut found = Vec::new();
    recording.for_each_tagged(stream, |ts, message, tags| {
        let Message::PoseStamped(pose) = message else { return Ok(true) };
        let tags: serde_json::Value = tags.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let number = |key: &str, default: f64| tags[key].as_f64().or_else(|| tags[key].as_str().and_then(|s| s.parse().ok())).unwrap_or(default);
        let Some(marker_id) = tags["marker_id"].as_i64().or_else(|| tags["marker_id"].as_f64().map(|v| v as i64)) else { return Ok(true) };
        if frame.is_empty() {
            frame = pose.header.frame_id.trim_start_matches('/').to_string();
        }
        let p = pose.pose;
        let t_cam_marker = [p.position[0], p.position[1], p.position[2], p.orientation[0], p.orientation[1], p.orientation[2], p.orientation[3]];
        let (distance_m, view_angle_deg) = crate::apriltag::view_quality(&t_cam_marker);
        found.push(Detection {
            ts,
            marker_id,
            t_cam_marker,
            sharpness: number("sharpness", f64::INFINITY),
            reproj_px: number("reproj_px", 0.0),
            tag_px: number("tag_px", f64::INFINITY),
            distance_m,
            view_angle_deg,
            lin_speed: number("lin_speed", -1.0),
            ang_speed: number("ang_speed", -1.0),
            observations: number("n_observations", 1.0) as usize,
        });
        Ok(true)
    })?;
    Ok(Some((found, frame)))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::apriltag::tests::{render_marker, render_tag, test_camera};
    use crate::aruco::tests::aruco_intensity;
    use nalgebra::{Rotation3, Vector3};

    /// a camera driving past tag 5 twice (two visits), images as raw mono8, the camera placed by tf
    pub fn tagged_recording(path: &Path) {
        tagged_recording_with(path, |camera, translation, facing| render_tag(camera, 640, 480, 5, 0.1, facing, translation));
    }

    /// `tagged_recording` with each frame drawn by `render` (camera, the tag's translation, its rotation)
    pub fn tagged_recording_with(path: &Path, render: impl Fn(&crate::apriltag::CameraModel, &Vector3<f64>, &nalgebra::Matrix3<f64>) -> Vec<u8>) {
        let camera = test_camera();
        let facing = *Rotation3::from_euler_angles(std::f64::consts::PI, 0.0, 0.0).matrix();
        let mut images = Vec::new();
        let mut tfs = Vec::new();
        for (visit, start) in [0.0, 60.0].into_iter().enumerate() {
            for k in 0..6 {
                let ts = start + k as f64 * 0.2;
                let translation = Vector3::new(0.02 * k as f64 - 0.05, 0.01 * visit as f64, 0.5);
                let gray = render(&camera, &translation, &facing);
                images.push((ts, lcm::encode_image(&lcm::Image { header: Header::at(ts, "camera_optical"), height: 480, width: 640, encoding: "mono8".into(), step: 640, data: gray })));
                let edge = lcm::TfEdge { header: Header::at(ts, "odom"), child: "camera_optical".into(), translation: [0.02 * k as f64, 0.0, 0.0], rotation: [0.0, 0.0, 0.0, 1.0] };
                tfs.push((ts, lcm::encode_tf(&[edge])));
            }
        }
        let info = lcm::CameraInfo { header: Header::at(0.0, "camera_optical"), height: 480, width: 640, distortion_model: "plumb_bob".into(), d: vec![0.0; 5], k: [600.0, 0.0, 320.0, 0.0, 600.0, 240.0, 0.0, 0.0, 1.0] };
        let mut connection = db::Connection::open(path).unwrap();
        db::write_stream(&mut connection, "color_image", lcm::IMAGE_TYPE, &images).unwrap();
        db::write_stream(&mut connection, "camera_info", lcm::CAMERA_INFO_TYPE, &[(0.0, lcm::encode_camera_info(&info))]).unwrap();
        db::write_stream(&mut connection, "tf", lcm::TF_TYPE, &tfs).unwrap();
    }

    #[test]
    fn finds_writes_and_reads_back_raw_and_grouped_tags_in_a_db_and_an_mcap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.db");
        tagged_recording(&path);
        let recording = Recording::open(&path).unwrap();
        let options = TagOptions::default();
        let result = detect(&recording, &options, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!((result.images, result.raw.len()), (12, 12), "{:?}", result.notes);
        assert_eq!(result.grouped.len(), 2, "one per visit: {:?}", result.report);
        assert!(result.raw.iter().all(|d| d.lin_speed > 0.05 && d.lin_speed < 0.15), "speed from tf: {:?}", result.raw.iter().map(|d| d.lin_speed).collect::<Vec<_>>());
        drop(recording);
        write(&path, &result, &options, 1.0).unwrap();
        let recording = Recording::open(&path).unwrap();
        let (raw, frame) = read(&recording, RAW_STREAM).unwrap().unwrap();
        assert_eq!((raw.len(), frame.as_str(), raw[0].marker_id), (12, "camera_optical", 5));
        assert!((raw[0].sharpness - result.raw[0].sharpness).abs() < 1e-9);
        let (grouped, _) = read(&recording, GROUPED_STREAM).unwrap().unwrap();
        assert_eq!(grouped.iter().map(|d| d.observations).collect::<Vec<_>>(), vec![6, 6]);

        // the same into an mcap, twice: the newer one is what's read
        let mcap = dir.path().join("r.mcap");
        dimos_recording::mcap_io::write_fixture(&mcap, &[("/tf", "lcm", lcm::TF_TYPE, 0.0, lcm::encode_tf(&[]))]);
        write(&mcap, &TagResult { raw: result.raw[..3].to_vec(), ..result.clone() }, &options, 1.0).unwrap();
        write(&mcap, &result, &options, 2.0).unwrap();
        let recording = Recording::open(&mcap).unwrap();
        let (raw, _) = read(&recording, RAW_STREAM).unwrap().unwrap();
        assert_eq!(raw.len(), 12);
        assert!((raw[4].reproj_px - result.raw[4].reproj_px).abs() < 1e-9);
    }

    #[test]
    fn finds_writes_and_reads_back_aruco_markers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.db");
        let dictionary = crate::aruco::ArucoDictionary::named("DICT_5X5_250").unwrap();
        tagged_recording_with(&path, |camera, translation, facing| render_marker(camera, 640, 480, 0.1, facing, translation, |x, y| aruco_intensity(&dictionary, 123, x, y)));
        let recording = Recording::open(&path).unwrap();
        let options = TagOptions { dictionary: "DICT_5X5_250".into(), ..TagOptions::default() };
        let result = detect(&recording, &options, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!((result.images, result.raw.len()), (12, 12), "{:?}", result.notes);
        assert!(result.raw.iter().all(|d| d.marker_id == 123 && (d.t_cam_marker[2] - 0.5).abs() < 0.01), "{:?}", result.raw);
        assert_eq!(result.grouped.len(), 2, "one per visit: {:?}", result.report);
        drop(recording);
        write(&path, &result, &options, 1.0).unwrap();
        let recording = Recording::open(&path).unwrap();
        let (grouped, frame) = read(&recording, GROUPED_STREAM).unwrap().unwrap();
        assert_eq!((grouped.len(), frame.as_str(), grouped[0].marker_id, grouped[0].observations), (2, "camera_optical", 123, 6));
    }
}
