//! The global map step's optional `.rrd` (rerun), written beside the recording: the global map, the raw and corrected
//! paths, every AprilTag as its own black-and-white pattern at its place in the world (with its id), and whichever
//! streams were picked at the rate picked for each: camera images on a posed pinhole (color images at 2 Hz by
//! default), point clouds placed in the world, odometry as a moving frame and its path. Everything is placed by the
//! recording's tf (the corrected edge included once it's written).
use crate::apriltag::{Detection, GlimpseGates};
use crate::build::{Cancelled, Progress};
use crate::tf::{iso, Iso, TfTree};
use anyhow::{Context, Result};
use dimos_recording::{Kind, Message, Recording};
use kornia_apriltag::family::TagFamily;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const TAG_IMAGE_PX: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RrdStream {
    pub name: String,
    pub enabled: bool,
    /// messages per second at most (0 = every one)
    pub hz: f64,
}

impl Default for RrdStream {
    fn default() -> Self {
        RrdStream { name: String::new(), enabled: false, hz: 2.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RrdOptions {
    pub enabled: bool,
    /// empty = the defaults (`default_streams`)
    pub streams: Vec<RrdStream>,
    pub map: bool,
    pub tags: bool,
    pub trajectory: bool,
}

impl Default for RrdOptions {
    fn default() -> Self {
        RrdOptions { enabled: false, streams: Vec::new(), map: true, tags: true, trajectory: true }
    }
}

/// Every stream an .rrd can show, with its default: color images at 2 Hz on, every other image stream, cloud and
/// odometry off (2 Hz, 2 Hz, 10 Hz).
pub fn default_streams(recording: &Recording) -> Result<Vec<RrdStream>> {
    Ok(recording
        .streams()?
        .into_iter()
        .filter(|s| s.count > 0)
        .filter_map(|s| match s.kind {
            Kind::Image => Some(RrdStream { enabled: s.name.contains("color") && !s.name.contains("depth"), hz: 2.0, name: s.name }),
            Kind::Cloud if s.name != crate::pipeline::GLOBAL_MAP_STREAM => Some(RrdStream { enabled: false, hz: 2.0, name: s.name }),
            Kind::Odometry | Kind::PoseStamped if !s.name.contains("april_tags") => Some(RrdStream { enabled: false, hz: 10.0, name: s.name }),
            _ => None,
        })
        .collect())
}

pub fn path_for(recording: &Path) -> PathBuf {
    recording.with_extension("rrd")
}

/// The 36h11 pattern of `id` (with its white margin), RGB.
fn tag_image(id: i64) -> Option<Vec<u8>> {
    let family = TagFamily::tag36_h11();
    let code = *family.code_data.get(usize::try_from(id).ok()?)?;
    let cells = family.total_width as usize; // includes the white margin
    let inner = family.width_at_border as usize;
    let margin = (cells - inner) / 2;
    let module = TAG_IMAGE_PX / cells;
    let size = module * cells;
    let mut rgb = vec![255u8; size * size * 3];
    for cy in 0..cells {
        for cx in 0..cells {
            let value = if cx < margin || cy < margin || cx >= margin + inner || cy >= margin + inner {
                255
            } else {
                let (x, y) = (cx - margin, cy - margin);
                if x == 0 || y == 0 || x == inner - 1 || y == inner - 1 {
                    0
                } else {
                    (0..family.nbits).find(|&bit| family.bit_x[bit] == x as i8 && family.bit_y[bit] == y as i8).map_or(0, |bit| if (code >> (family.nbits - 1 - bit)) & 1 == 1 { 255 } else { 0 })
                }
            };
            for row in cy * module..(cy + 1) * module {
                for column in cx * module..(cx + 1) * module {
                    let at = (row * size + column) * 3;
                    rgb[at..at + 3].copy_from_slice(&[value; 3]);
                }
            }
        }
    }
    Some(rgb)
}

fn translation(pose: &Iso) -> [f32; 3] {
    let t = pose.translation.vector;
    [t.x as f32, t.y as f32, t.z as f32]
}

fn transform(pose: &Iso) -> rerun::Transform3D {
    let q = pose.rotation;
    rerun::Transform3D::from_translation_rotation(translation(pose), rerun::Quaternion::from_xyzw([q.i as f32, q.j as f32, q.k as f32, q.w as f32]))
}

fn height_colors(points: &[[f32; 3]]) -> Vec<rerun::Color> {
    let mut heights: Vec<f32> = points.iter().map(|p| p[2]).collect();
    heights.sort_by(|a, b| a.total_cmp(b));
    let at = |q: f64| heights.get(((heights.len().max(1) - 1) as f64 * q) as usize).copied().unwrap_or(0.0);
    let (low, high) = (at(0.02), at(0.98).max(at(0.02) + 1e-3));
    points
        .iter()
        .map(|p| {
            let t = ((p[2] - low) / (high - low)).clamp(0.0, 1.0);
            // deep blue → teal → warm yellow
            let ramp = |a: f32, b: f32, c: f32| if t < 0.5 { a + (b - a) * t * 2.0 } else { b + (c - b) * (t - 0.5) * 2.0 };
            rerun::Color::from_rgb(ramp(40.0, 40.0, 250.0) as u8, ramp(60.0, 190.0, 220.0) as u8, ramp(170.0, 170.0, 80.0) as u8)
        })
        .collect()
}

pub struct RrdInput<'a> {
    pub recording: &'a Recording,
    pub tree: &'a TfTree,
    pub world: &'a str,
    pub map: &'a [[f32; 3]],
    pub raw_path: &'a [[f32; 3]],
    pub corrected_path: &'a [[f32; 3]],
    /// tags (raw glimpses) and the camera frame they're in
    pub tags: Option<(Vec<Detection>, String)>,
}

/// Writes the .rrd; returns its path.
pub fn write(path: &Path, input: &RrdInput, options: &RrdOptions, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<PathBuf> {
    let out = path_for(path);
    let partial = out.with_extension("rrd.partial");
    let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let rec = rerun::RecordingStreamBuilder::new(format!("map_builder/{name}")).save(&partial).context("starting the .rrd")?;
    rec.log_static("world", &rerun::ViewCoordinates::RIGHT_HAND_Z_UP())?;
    if options.map && !input.map.is_empty() {
        rec.log_static("world/global_map", &rerun::Points3D::new(input.map.iter().copied()).with_colors(height_colors(input.map)).with_radii([0.025f32]))?;
    }
    if options.trajectory {
        for (label, line, color) in [("raw", input.raw_path, [235u8, 90, 120]), ("corrected", input.corrected_path, [80u8, 220, 120])] {
            if line.len() > 1 {
                rec.log_static(format!("world/path/{label}"), &rerun::LineStrips3D::new([line.to_vec()]).with_colors([rerun::Color::from_rgb(color[0], color[1], color[2])]).with_radii([0.03f32]))?;
            }
        }
    }
    let streams = if options.streams.is_empty() { default_streams(input.recording)? } else { options.streams.clone() };
    let selected: Vec<RrdStream> = streams.into_iter().filter(|s| s.enabled).collect();
    let stage_count = selected.len() + 1;
    if options.tags {
        if let Some((detections, camera_frame)) = &input.tags {
            log_tags(&rec, input.tree, input.world, detections, camera_frame)?;
        }
    }
    let infos = input.recording.streams()?;
    for (index, stream) in selected.iter().enumerate() {
        let Some(info) = infos.iter().find(|s| s.name == stream.name) else { continue };
        let total = info.count;
        let gap = if stream.hz > 0.0 { 1.0 / stream.hz } else { 0.0 };
        let mut last = f64::NEG_INFINITY;
        let mut seen = 0u64;
        let entity = format!("world/{}", stream.name.trim_start_matches('/').replace('/', "_"));
        let mut camera_info: Option<dimos_recording::lcm::CameraInfo> = None;
        if info.kind == Kind::Image {
            camera_info = crate::tags::camera_info(input.recording, &stream.name, "").ok().map(|(_, info)| info);
        }
        let mut path_points: Vec<[f32; 3]> = Vec::new();
        input.recording.for_each(&stream.name, |ts, message, _| {
            seen += 1;
            if seen % 16 == 0 {
                progress(Progress { overall: None, stage: format!("Writing the .rrd: {}", stream.name), stage_index: index, stage_count, done: seen, total, note: String::new() });
                if cancel.load(Ordering::Relaxed) {
                    return Err(anyhow::anyhow!(Cancelled));
                }
            }
            if ts - last < gap {
                return Ok(true);
            }
            last = ts;
            rec.set_timestamp_secs_since_epoch("time", ts);
            let place = |frame: &str, stamp: f64| -> Option<Iso> {
                let frame = frame.trim_start_matches('/');
                (stamp > 0.0).then(|| input.tree.lookup(input.world, frame, stamp, 0.5)).flatten().or_else(|| input.tree.lookup(input.world, frame, ts, 0.5))
            };
            match message {
                Message::Image(image) => {
                    let frame = camera_info.as_ref().map(|c| c.header.frame_id.clone()).filter(|f| !f.is_empty()).unwrap_or(image.header.frame_id.clone());
                    let posed = place(&frame, image.header.ts());
                    let target = if posed.is_some() { entity.clone() } else { format!("images/{}", stream.name.trim_start_matches('/').replace('/', "_")) };
                    if let (Some(pose), Some(info)) = (posed, &camera_info) {
                        rec.log(target.as_str(), &transform(&pose))?;
                        let k = &info.k;
                        let projection = rerun::datatypes::Mat3x3([k[0] as f32, 0.0, 0.0, k[1] as f32, k[4] as f32, 0.0, k[2] as f32, k[5] as f32, 1.0]);
                        rec.log(target.as_str(), &rerun::Pinhole::new(projection).with_resolution([info.width as f32, info.height as f32]).with_camera_xyz(rerun::components::ViewCoordinates::RDF).with_image_plane_distance(0.5f32))?;
                    }
                    match image.encoding.to_lowercase().as_str() {
                        "jpeg" | "jpg" => rec.log(target.as_str(), &rerun::EncodedImage::new(image.data).with_media_type(rerun::MediaType::JPEG))?,
                        "png" => rec.log(target.as_str(), &rerun::EncodedImage::new(image.data).with_media_type(rerun::MediaType::PNG))?,
                        _ => {
                            if let Ok((gray, width, height)) = crate::apriltag::decode_gray(&image) {
                                rec.log(target.as_str(), &rerun::Image::from_l8(gray, [width as u32, height as u32]))?;
                            }
                        }
                    }
                }
                Message::Cloud(cloud) => {
                    if let Some(pose) = place(&cloud.frame_id, cloud.ts) {
                        let points: Vec<[f32; 3]> = cloud.points.iter().map(|p| crate::tf::transform_point(&pose, *p)).collect();
                        rec.log(entity.as_str(), &rerun::Points3D::new(points).with_radii([0.02f32]))?;
                    }
                }
                Message::Odometry(odometry) => {
                    let pose = place(&odometry.header.frame_id, odometry.header.ts()).map(|frame| frame * iso(odometry.pose.position, odometry.pose.orientation));
                    if let Some(pose) = pose {
                        rec.log(entity.as_str(), &transform(&pose))?;
                        rec.log(entity.as_str(), &rerun::TransformAxes3D::new(0.3f32))?;
                        path_points.push(translation(&pose));
                    }
                }
                Message::PoseStamped(stamped) => {
                    let pose = place(&stamped.header.frame_id, stamped.header.ts()).map(|frame| frame * iso(stamped.pose.position, stamped.pose.orientation));
                    if let Some(pose) = pose {
                        rec.log(entity.as_str(), &transform(&pose))?;
                        path_points.push(translation(&pose));
                    }
                }
                _ => {}
            }
            Ok(true)
        })?;
        if path_points.len() > 1 {
            rec.log_static(format!("{entity}_path"), &rerun::LineStrips3D::new([path_points]).with_radii([0.02f32]))?;
        }
    }
    rec.flush_blocking()?;
    drop(rec);
    std::fs::rename(&partial, &out)?;
    Ok(out)
}

/// Each tag once, where its glimpses put it (the mean of their world positions, the medoid's orientation): its
/// pattern as a textured square the tag's size, and its id.
fn log_tags(rec: &rerun::RecordingStream, tree: &TfTree, world: &str, detections: &[Detection], camera_frame: &str) -> Result<()> {
    let gates = GlimpseGates { max_distance_m: 1.5, max_view_angle_deg: 65.0, min_sharpness: 25.0, max_reproj_px: 3.5, min_tag_px: 12.0, ..Default::default() };
    let mut by_marker: BTreeMap<i64, Vec<(f64, Iso)>> = BTreeMap::new();
    for detection in detections.iter().filter(|d| crate::apriltag::rejected_by(d, &gates).is_none()) {
        let Some(camera) = tree.lookup(world, camera_frame, detection.ts, 0.5) else { continue };
        let t = detection.t_cam_marker;
        by_marker.entry(detection.marker_id).or_default().push((detection.reproj_px, camera * iso([t[0], t[1], t[2]], [t[3], t[4], t[5], t[6]])));
    }
    let half = 0.05 * 10.0 / 8.0; // a 10 cm tag's black square, plus its white margin
    let mut labels = Vec::new();
    for (marker, poses) in &by_marker {
        let mean = poses.iter().map(|(_, p)| p.translation.vector).sum::<nalgebra::Vector3<f64>>() / poses.len() as f64;
        let best = poses.iter().min_by(|a, b| a.0.total_cmp(&b.0)).unwrap().1;
        let placed = Iso::from_parts(nalgebra::Translation3::from(mean), best.rotation);
        let corners: Vec<[f32; 3]> = [[-half, half], [half, half], [half, -half], [-half, -half]].iter().map(|[x, y]| translation(&(placed * Iso::translation(*x, *y, 0.0)))).collect();
        if let Some(texture) = tag_image(*marker) {
            let size = (texture.len() / 3) as f64;
            let side = size.sqrt() as u32;
            rec.log_static(
                format!("world/april_tags/tag_{marker}"),
                &rerun::Mesh3D::new(corners)
                    .with_triangle_indices([[0u32, 1, 2], [0, 2, 3]])
                    .with_vertex_texcoords([[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]])
                    .with_albedo_texture_image(rerun::Image::from_rgb24(texture, [side, side])),
            )?;
        }
        labels.push((translation(&placed), format!("tag {marker} ({} sightings)", poses.len())));
    }
    if !labels.is_empty() {
        rec.log_static(
            "world/april_tags/labels",
            &rerun::Points3D::new(labels.iter().map(|l| l.0)).with_labels(labels.iter().map(|l| l.1.clone())).with_colors([rerun::Color::from_rgb(255, 210, 0)]).with_radii([0.02f32]).with_show_labels(true),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_an_rrd_with_the_map_tags_and_images() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.db");
        crate::tags::tests::tagged_recording(&path);
        let recording = Recording::open(&path).unwrap();
        let tree = crate::frames::tf_tree(&recording).unwrap();
        let tags = crate::tags::detect(&recording, &Default::default(), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        let input = RrdInput { recording: &recording, tree: &tree, world: "odom", map: &[[0.0, 0.0, 0.0], [1.0, 0.0, 0.5]], raw_path: &[[0.0; 3], [1.0, 0.0, 0.0]], corrected_path: &[], tags: Some((tags.raw.clone(), tags.camera_frame.clone())) };
        let defaults = default_streams(&recording).unwrap();
        assert_eq!(defaults, vec![RrdStream { name: "color_image".into(), enabled: true, hz: 2.0 }]);
        let out = write(&path, &input, &RrdOptions { enabled: true, ..Default::default() }, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(out, dir.path().join("r.rrd"));
        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.len() > 10_000 && &bytes[..4] == b"RRF2", "{} bytes", bytes.len());
        assert!(!dir.path().join("r.rrd.partial").exists());
        assert!(tag_image(5).is_some() && tag_image(100_000).is_none());
    }
}
