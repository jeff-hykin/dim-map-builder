//! AprilTag detection and grouping, a port of icp_stitch's `detect.rs` / `apriltags.rs` (same author), which port
//! dimos's `jnav/utils/apriltags.py` + `perception/fiducial/marker_pose.py`: kornia-apriltag finds the quads, a planar
//! square PnP (homography init, Gauss-Newton on pixel error through the full distortion model, plus the reflected
//! IPPE candidate) poses each tag in the camera's optical frame (the same y-up marker frame as cv2 aruco /
//! `solvePnP(IPPE_SQUARE)`). Each glimpse carries the diagnostics the quality gates use (sharpness, reprojection
//! error, size in pixels, distance, view angle, the camera's speed); grouping clusters a marker's gated glimpses in
//! time and reduces each cluster to one Huber-refined medoid pose, which is what `add_april.py` writes as
//! `april_tags`.
use crate::aruco::{ArucoDetector, ArucoDictionary};
use dimos_recording::lcm::{CameraInfo, Image};
use kornia_apriltag::family::TagFamilyKind;
use kornia_apriltag::{AprilTagDecoder, DecodeTagsConfig};
use kornia_image::allocator::CpuAllocator;
use kornia_image::{Image as KorniaImage, ImageSize};
use nalgebra::{Matrix3, Matrix4, Rotation3, SMatrix, SVector, UnitQuaternion, Vector3};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const UNDISTORT_ITERATIONS: usize = 5;
const EQUIDISTANT_UNDISTORT_ITERATIONS: usize = 10;
const RADIUS_EPSILON: f64 = 1e-12;
const HUBER_ITERATIONS: usize = 5;

/// One tag in one image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Detection {
    pub ts: f64,
    pub marker_id: i64,
    /// camera (optical) ← marker: x, y, z, qx, qy, qz, qw
    pub t_cam_marker: [f64; 7],
    pub sharpness: f64,
    pub reproj_px: f64,
    pub tag_px: f64,
    pub distance_m: f64,
    pub view_angle_deg: f64,
    /// the camera's speed when the image was taken (m/s, °/s); -1 = unknown
    pub lin_speed: f64,
    pub ang_speed: f64,
    /// glimpses a grouped detection stands for (1 for a raw one)
    pub observations: usize,
}

/// Per-glimpse quality gates: dimos apriltags.py's defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GlimpseGates {
    pub min_sharpness: f64,
    pub max_reproj_px: f64,
    pub min_tag_px: f64,
    pub max_distance_m: f64,
    pub max_view_angle_deg: f64,
    pub max_linear_speed_mps: f64,
    pub max_angular_speed_dps: f64,
}

impl Default for GlimpseGates {
    fn default() -> Self {
        GlimpseGates { min_sharpness: 60.0, max_reproj_px: 2.0, min_tag_px: 24.0, max_distance_m: 1.0, max_view_angle_deg: 45.0, max_linear_speed_mps: 0.5, max_angular_speed_dps: 50.0 }
    }
}

/// How gated glimpses become grouped tags: dimos apriltags.py's defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Grouping {
    /// a marker's next glimpse more than this later starts a new group (s)
    pub cluster_gap_sec: f64,
    /// groups with fewer glimpses are dropped
    pub min_observations: usize,
    /// past this residual (m) a glimpse is down-weighted
    pub huber_delta_m: f64,
    /// meters a radian of rotation counts as, when comparing poses
    pub rotation_weight_m_per_rad: f64,
}

impl Default for Grouping {
    fn default() -> Self {
        Grouping { cluster_gap_sec: 5.0, min_observations: 3, huber_delta_m: 0.05, rotation_weight_m_per_rad: 0.5 }
    }
}

/// (distance m, view angle °) of a tag pose in the optical frame: 0° = head on.
pub fn view_quality(t_cam_marker: &[f64; 7]) -> (f64, f64) {
    let translation = Vector3::new(t_cam_marker[0], t_cam_marker[1], t_cam_marker[2]);
    let distance = translation.norm();
    let normal = quaternion(t_cam_marker) * Vector3::z();
    let cos = (translation / (distance + 1e-9)).dot(&normal).abs().min(1.0);
    (distance, cos.acos().to_degrees())
}

pub fn quaternion(pose: &[f64; 7]) -> UnitQuaternion<f64> {
    UnitQuaternion::from_quaternion(nalgebra::Quaternion::new(pose[6], pose[3], pose[4], pose[5]))
}

/// None when a glimpse passes every gate, else why not.
pub fn rejected_by(detection: &Detection, gates: &GlimpseGates) -> Option<&'static str> {
    if detection.sharpness < gates.min_sharpness {
        return Some("blur");
    }
    if detection.reproj_px > gates.max_reproj_px {
        return Some("reproj");
    }
    if detection.tag_px < gates.min_tag_px {
        return Some("small");
    }
    let (distance, angle) = view_quality(&detection.t_cam_marker);
    if distance > gates.max_distance_m {
        return Some("far");
    }
    if angle > gates.max_view_angle_deg {
        return Some("oblique");
    }
    if detection.lin_speed >= 0.0 && (detection.lin_speed > gates.max_linear_speed_mps || detection.ang_speed > gates.max_angular_speed_dps) {
        return Some("motion");
    }
    None
}

// ---- the camera ------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DistortionModel {
    /// plumb_bob / rational_polynomial: k1, k2, p1, p2, k3, k4, k5, k6
    Radtan,
    /// fisheye: k1..k4
    Equidistant,
}

/// Pinhole + OpenCV distortion, from a CameraInfo.
#[derive(Debug, Clone)]
pub struct CameraModel {
    pub fx: f64,
    pub fy: f64,
    pub cx: f64,
    pub cy: f64,
    pub skew: f64,
    pub dist: [f64; 8],
    pub model: DistortionModel,
}

impl CameraModel {
    pub fn from_info(info: &CameraInfo) -> CameraModel {
        let mut dist = [0.0; 8];
        for (slot, value) in dist.iter_mut().zip(&info.d) {
            *slot = *value;
        }
        let name = info.distortion_model.to_lowercase();
        let model = if name == "equidistant" || name == "fisheye" { DistortionModel::Equidistant } else { DistortionModel::Radtan };
        CameraModel { fx: info.k[0], skew: info.k[1], cx: info.k[2], fy: info.k[4], cy: info.k[5], dist, model }
    }

    /// pixel → normalized image coordinates (cv2.undistortPoints' fixed-count iteration)
    pub fn undistort(&self, pixel: [f64; 2]) -> [f64; 2] {
        let y0 = (pixel[1] - self.cy) / self.fy;
        let x0 = (pixel[0] - self.cx - self.skew * y0) / self.fx;
        if self.model == DistortionModel::Equidistant {
            let distorted = (x0 * x0 + y0 * y0).sqrt();
            if distorted < RADIUS_EPSILON {
                return [x0, y0];
            }
            let mut theta = distorted;
            for _ in 0..EQUIDISTANT_UNDISTORT_ITERATIONS {
                theta = distorted / self.equidistant_radial(theta);
            }
            let scale = theta.tan() / distorted;
            return [x0 * scale, y0 * scale];
        }
        let [k1, k2, p1, p2, k3, k4, k5, k6] = self.dist;
        let (mut x, mut y) = (x0, y0);
        for _ in 0..UNDISTORT_ITERATIONS {
            let r2 = x * x + y * y;
            let inverse = (1.0 + ((k6 * r2 + k5) * r2 + k4) * r2) / (1.0 + ((k3 * r2 + k2) * r2 + k1) * r2);
            let dx = 2.0 * p1 * x * y + p2 * (r2 + 2.0 * x * x);
            let dy = p1 * (r2 + 2.0 * y * y) + 2.0 * p2 * x * y;
            x = (x0 - dx) * inverse;
            y = (y0 - dy) * inverse;
        }
        [x, y]
    }

    fn equidistant_radial(&self, theta: f64) -> f64 {
        let [k1, k2, k3, k4, ..] = self.dist;
        let t2 = theta * theta;
        1.0 + t2 * (k1 + t2 * (k2 + t2 * (k3 + t2 * k4)))
    }

    /// camera-frame point → distorted pixel (cv2.projectPoints)
    pub fn project(&self, point: &Vector3<f64>) -> [f64; 2] {
        let x = point.x / point.z;
        let y = point.y / point.z;
        let (xd, yd) = if self.model == DistortionModel::Equidistant {
            let radius = (x * x + y * y).sqrt();
            let theta = radius.atan();
            let scale = if radius < RADIUS_EPSILON { 1.0 } else { theta * self.equidistant_radial(theta) / radius };
            (x * scale, y * scale)
        } else {
            let [k1, k2, p1, p2, k3, k4, k5, k6] = self.dist;
            let r2 = x * x + y * y;
            let radial = (1.0 + ((k3 * r2 + k2) * r2 + k1) * r2) / (1.0 + ((k6 * r2 + k5) * r2 + k4) * r2);
            (x * radial + 2.0 * p1 * x * y + p2 * (r2 + 2.0 * x * x), y * radial + p1 * (r2 + 2.0 * y * y) + 2.0 * p2 * x * y)
        };
        [self.fx * xd + self.skew * yd + self.cx, self.fy * yd + self.cy]
    }
}

// ---- planar square PnP -----------------------------------------------------------------------------------------

/// The homography (h33 = 1) taking 4 plane points to 4 image points.
fn homography(object: &[[f64; 2]; 4], image: &[[f64; 2]; 4]) -> Option<Matrix3<f64>> {
    let mut a = SMatrix::<f64, 8, 8>::zeros();
    let mut b = SVector::<f64, 8>::zeros();
    for k in 0..4 {
        let [ox, oy] = object[k];
        let [u, v] = image[k];
        let row = 2 * k;
        a[(row, 0)] = ox;
        a[(row, 1)] = oy;
        a[(row, 2)] = 1.0;
        a[(row, 6)] = -u * ox;
        a[(row, 7)] = -u * oy;
        b[row] = u;
        a[(row + 1, 3)] = ox;
        a[(row + 1, 4)] = oy;
        a[(row + 1, 5)] = 1.0;
        a[(row + 1, 6)] = -v * ox;
        a[(row + 1, 7)] = -v * oy;
        b[row + 1] = v;
    }
    let h = a.lu().solve(&b)?;
    Some(Matrix3::new(h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0))
}

fn nearest_rotation(m: &Matrix3<f64>) -> Matrix3<f64> {
    let svd = m.svd(true, true);
    let (u, v_t) = (svd.u.unwrap(), svd.v_t.unwrap());
    let mut d = Matrix3::identity();
    d[(2, 2)] = (u * v_t).determinant().signum();
    u * d * v_t
}

fn pose_from_homography(h: &Matrix3<f64>) -> (Matrix3<f64>, Vector3<f64>) {
    let (h1, h2, h3) = (h.column(0).into_owned(), h.column(1).into_owned(), h.column(2).into_owned());
    let scale = 2.0 / (h1.norm() + h2.norm());
    let sign = if h3.z * scale < 0.0 { -1.0 } else { 1.0 };
    let (r1, r2, t) = (h1 * scale * sign, h2 * scale * sign, h3 * scale * sign);
    let r3 = r1.cross(&r2);
    (nearest_rotation(&Matrix3::from_columns(&[r1, r2, r3])), t)
}

/// The second IPPE candidate: the tag's normal reflected across the line of sight (the planar-pose ambiguity).
fn reflected(rotation: &Matrix3<f64>, translation: &Vector3<f64>) -> Option<(Matrix3<f64>, Vector3<f64>)> {
    let distance = translation.norm();
    if distance < 1e-9 {
        return None;
    }
    let sight = translation / distance;
    let normal = rotation.column(2).into_owned();
    let mirrored = sight * (2.0 * sight.dot(&normal)) - normal;
    let axis = normal.cross(&mirrored);
    if axis.norm() < 1e-12 {
        return None;
    }
    let angle = normal.dot(&mirrored).clamp(-1.0, 1.0).acos();
    let turn = Rotation3::from_scaled_axis(axis.normalize() * angle);
    Some((turn.matrix() * rotation, *translation))
}

fn residuals(camera: &CameraModel, object: &[Vector3<f64>; 4], pixels: &[[f64; 2]; 4], rotation: &Matrix3<f64>, translation: &Vector3<f64>) -> SVector<f64, 8> {
    let mut out = SVector::<f64, 8>::zeros();
    for k in 0..4 {
        let projected = camera.project(&(rotation * object[k] + translation));
        out[2 * k] = projected[0] - pixels[k][0];
        out[2 * k + 1] = projected[1] - pixels[k][1];
    }
    out
}

/// Gauss-Newton on pixel error over [ω, δt] (left perturbation).
fn refine(camera: &CameraModel, object: &[Vector3<f64>; 4], pixels: &[[f64; 2]; 4], rotation: Matrix3<f64>, translation: Vector3<f64>) -> (Matrix3<f64>, Vector3<f64>) {
    let (mut rotation, mut translation) = (rotation, translation);
    let step = 1e-6;
    for _ in 0..20 {
        let base = residuals(camera, object, pixels, &rotation, &translation);
        let mut jacobian = SMatrix::<f64, 8, 6>::zeros();
        for parameter in 0..6 {
            let mut delta = [0.0; 6];
            delta[parameter] = step;
            let turned = Rotation3::from_scaled_axis(Vector3::new(delta[0], delta[1], delta[2])).matrix() * rotation;
            let moved = translation + Vector3::new(delta[3], delta[4], delta[5]);
            let plus = residuals(camera, object, pixels, &turned, &moved);
            jacobian.set_column(parameter, &((plus - base) / step));
        }
        let normal = jacobian.transpose() * jacobian + SMatrix::<f64, 6, 6>::identity() * 1e-9;
        let Some(update) = normal.lu().solve(&(-(jacobian.transpose() * base))) else { break };
        rotation = Rotation3::from_scaled_axis(Vector3::new(update[0], update[1], update[2])).matrix() * rotation;
        translation += Vector3::new(update[3], update[4], update[5]);
        if update.norm() < 1e-12 {
            break;
        }
    }
    (rotation, translation)
}

/// camera ← marker of a square tag from its corners in aruco order (TL, TR, BR, BL): (rotation, translation, RMS px).
pub fn solve_square_pnp(camera: &CameraModel, corners: &[[f64; 2]; 4], marker_length: f64) -> Option<(Matrix3<f64>, Vector3<f64>, f64)> {
    let half = marker_length / 2.0;
    let plane = [[-half, half], [half, half], [half, -half], [-half, -half]];
    let object = plane.map(|[x, y]| Vector3::new(x, y, 0.0));
    let normalized = corners.map(|c| camera.undistort(c));
    let (rotation, translation) = pose_from_homography(&homography(&plane, &normalized)?);
    let mut candidates = vec![(rotation, translation)];
    candidates.extend(reflected(&rotation, &translation));
    let mut best: Option<(Matrix3<f64>, Vector3<f64>, f64)> = None;
    for (rotation, translation) in candidates {
        let (rotation, translation) = refine(camera, &object, corners, rotation, translation);
        if translation.z <= 0.0 {
            continue;
        }
        let rms = (residuals(camera, &object, corners, &rotation, &translation).norm_squared() / 4.0).sqrt();
        if best.as_ref().is_none_or(|b| rms < b.2) {
            best = Some((rotation, translation, rms));
        }
    }
    best
}

// ---- pixels ------------------------------------------------------------------------------------------------

fn gray_of(data: &[u8], channels: usize, weights: [f64; 3]) -> impl Iterator<Item = u8> + '_ {
    data.chunks_exact(channels).map(move |px| (weights[0] * px[0] as f64 + weights[1] * px[1] as f64 + weights[2] * px[2] as f64).round().clamp(0.0, 255.0) as u8)
}

const REC601: [f64; 3] = [0.299, 0.587, 0.114];
const REC601_BGR: [f64; 3] = [0.114, 0.587, 0.299];

/// Grayscale pixels and (width, height) of a camera frame: jpeg, png or raw (mono8, rgb8, bgr8, rgba8, bgra8).
pub fn decode_gray(image: &Image) -> anyhow::Result<(Vec<u8>, usize, usize)> {
    let encoding = image.encoding.to_lowercase();
    if encoding == "jpeg" || encoding == "jpg" {
        use zune_jpeg::zune_core::bytestream::ZCursor;
        let mut decoder = zune_jpeg::JpegDecoder::new(ZCursor::new(&image.data[..]));
        let pixels = decoder.decode().map_err(|e| anyhow::anyhow!("jpeg: {e}"))?;
        let info = decoder.info().ok_or_else(|| anyhow::anyhow!("jpeg without a header"))?;
        let channels = decoder.output_colorspace().map_or(3, |c| c.num_components());
        let gray = if channels == 1 { pixels } else { gray_of(&pixels, channels, REC601).collect() };
        return Ok((gray, info.width as usize, info.height as usize));
    }
    if encoding == "png" {
        let decoder = png::Decoder::new(std::io::Cursor::new(&image.data));
        let mut reader = decoder.read_info()?;
        let mut pixels = vec![0; reader.output_buffer_size()];
        let frame = reader.next_frame(&mut pixels)?;
        let channels = frame.color_type.samples();
        pixels.truncate(frame.buffer_size());
        let gray = if channels <= 2 { pixels.chunks_exact(channels).map(|px| px[0]).collect() } else { gray_of(&pixels, channels, REC601).collect() };
        return Ok((gray, frame.width as usize, frame.height as usize));
    }
    let (width, height, step) = (image.width as usize, image.height as usize, image.step as usize);
    let per_pixel = match encoding.as_str() {
        "mono8" | "8uc1" => 1,
        "rgb8" | "bgr8" => 3,
        "rgba8" | "bgra8" => 4,
        other => anyhow::bail!("unsupported image encoding {other:?}"),
    };
    let step = if step == 0 { width * per_pixel } else { step };
    if image.data.len() < step * height || step < width * per_pixel {
        anyhow::bail!("image buffer too small: {} bytes for {width}x{height} {encoding}", image.data.len());
    }
    let weights = if encoding.starts_with("bgr") { REC601_BGR } else { REC601 };
    let mut gray = Vec::with_capacity(width * height);
    for row in 0..height {
        let line = &image.data[row * step..row * step + width * per_pixel];
        if per_pixel == 1 {
            gray.extend_from_slice(line);
        } else {
            gray.extend(gray_of(line, per_pixel, weights));
        }
    }
    Ok((gray, width, height))
}

/// A tag's side in pixels: the square root of its quad's area.
pub fn tag_pixel_size(corners: &[[f64; 2]; 4]) -> f64 {
    let doubled: f64 = (0..4).map(|k| corners[k][0] * corners[(k + 1) % 4][1] - corners[(k + 1) % 4][0] * corners[k][1]).sum();
    (doubled / 2.0).abs().sqrt()
}

/// Variance of the 3x3 Laplacian over the tag's bounding box, reflect-101 borders (cv2's recipe): low = motion blur.
pub fn tag_sharpness(gray: &[u8], width: usize, height: usize, corners: &[[f64; 2]; 4]) -> f64 {
    let x_min = (corners.iter().map(|c| c[0]).fold(f64::INFINITY, f64::min).floor().max(0.0)) as usize;
    let y_min = (corners.iter().map(|c| c[1]).fold(f64::INFINITY, f64::min).floor().max(0.0)) as usize;
    let x_max = (corners.iter().map(|c| c[0]).fold(f64::NEG_INFINITY, f64::max).ceil().max(0.0) as usize).min(width);
    let y_max = (corners.iter().map(|c| c[1]).fold(f64::NEG_INFINITY, f64::max).ceil().max(0.0) as usize).min(height);
    if x_max.saturating_sub(x_min) < 2 || y_max.saturating_sub(y_min) < 2 {
        return 0.0;
    }
    let (w, h) = ((x_max - x_min) as isize, (y_max - y_min) as isize);
    let sample = |x: isize, y: isize| -> f64 {
        let x = if x < 0 { -x } else if x >= w { 2 * w - 2 - x } else { x };
        let y = if y < 0 { -y } else if y >= h { 2 * h - 2 - y } else { y };
        gray[(y_min + y as usize) * width + x_min + x as usize] as f64
    };
    let (mut sum, mut squares) = (0.0, 0.0);
    for y in 0..h {
        for x in 0..w {
            let response = sample(x - 1, y) + sample(x + 1, y) + sample(x, y - 1) + sample(x, y + 1) - 4.0 * sample(x, y);
            sum += response;
            squares += response * response;
        }
    }
    let count = (w * h) as f64;
    squares / count - (sum / count).powi(2)
}

// ---- detecting -------------------------------------------------------------------------------------------------

pub fn family(dictionary: &str) -> anyhow::Result<TagFamilyKind> {
    Ok(match dictionary {
        "DICT_APRILTAG_36h11" | "DICT_APRILTAG_36H11" | "tag36h11" | "36h11" => TagFamilyKind::Tag36H11,
        "DICT_APRILTAG_36h10" | "DICT_APRILTAG_36H10" | "tag36h10" => TagFamilyKind::Tag36H10,
        "DICT_APRILTAG_25h9" | "DICT_APRILTAG_25H9" | "tag25h9" => TagFamilyKind::Tag25H9,
        "DICT_APRILTAG_16h5" | "DICT_APRILTAG_16H5" | "tag16h5" => TagFamilyKind::Tag16H5,
        other => anyhow::bail!("unsupported tag dictionary {other:?} (DICT_APRILTAG_36h11, 36h10, 25h9 or 16h5, or an ArUco one: {})", ArucoDictionary::names().collect::<Vec<_>>().join(", ")),
    })
}

/// Every dictionary the detector knows, by its OpenCV name: the AprilTag families, then the ArUco ones.
pub fn dictionaries() -> Vec<&'static str> {
    let mut names = vec!["DICT_APRILTAG_36h11", "DICT_APRILTAG_36h10", "DICT_APRILTAG_25h9", "DICT_APRILTAG_16h5"];
    names.extend(ArucoDictionary::names());
    names
}

/// A kornia detection's corners in aruco order (TL, TR, BR, BL of the canonical tag): the quad's own corners ignore
/// the decoded orientation, its homography doesn't.
fn oriented_corners(detection: &kornia_apriltag::decoder::Detection) -> [[f64; 2]; 4] {
    let tag_corners = [[-1.0f32, 1.0], [1.0, 1.0], [1.0, -1.0], [-1.0, -1.0]];
    let mut out = [[0.0; 2]; 4];
    for (slot, corner) in out.iter_mut().rev().zip(tag_corners) {
        let point = detection.quad.homography_project(corner[0], corner[1]);
        *slot = [point.x as f64, point.y as f64];
    }
    out
}

enum Finder {
    /// kornia's decoder, kept while the frame size stays the same
    AprilTag { kind: TagFamilyKind, decoder: Option<(AprilTagDecoder, ImageSize)> },
    Aruco(ArucoDetector),
}

/// Detects tags (an AprilTag family or an ArUco dictionary, by OpenCV name) frame by frame.
pub struct TagDetector {
    finder: Finder,
}

impl TagDetector {
    pub fn new(dictionary: &str) -> anyhow::Result<TagDetector> {
        let finder = match ArucoDictionary::named(dictionary) {
            Some(aruco) => Finder::Aruco(ArucoDetector::new(aruco)),
            None => Finder::AprilTag { kind: family(dictionary)?, decoder: None },
        };
        Ok(TagDetector { finder })
    }

    /// (marker id, corners in aruco order) of every tag in one grayscale frame.
    pub(crate) fn markers(&mut self, gray: &[u8], width: usize, height: usize) -> anyhow::Result<Vec<(i64, [[f64; 2]; 4])>> {
        match &mut self.finder {
            Finder::Aruco(detector) => Ok(detector.detect(gray, width, height)?.into_iter().map(|m| (m.id as i64, m.corners)).collect()),
            Finder::AprilTag { kind, decoder } => {
                let size = ImageSize { width, height };
                if decoder.as_ref().is_none_or(|(_, existing)| existing.width != width || existing.height != height) {
                    let config = DecodeTagsConfig::new(vec![kind.clone()]);
                    *decoder = Some((AprilTagDecoder::new(config, size).map_err(|e| anyhow::anyhow!("{e}"))?, size));
                }
                let (decoder, _) = decoder.as_mut().unwrap();
                let frame = KorniaImage::<u8, 1, CpuAllocator>::new(size, gray.to_vec(), CpuAllocator).map_err(|e| anyhow::anyhow!("{e}"))?;
                let found = decoder.decode(&frame).map_err(|e| anyhow::anyhow!("{e}"))?;
                decoder.clear();
                Ok(found.iter().map(|tag| (tag.id as i64, oriented_corners(tag))).collect())
            }
        }
    }

    /// Every tag in one grayscale frame, posed. `speed` = the camera's (m/s, °/s) or (-1, -1).
    pub fn detect(&mut self, ts: f64, gray: &[u8], width: usize, height: usize, camera: &CameraModel, marker_length: f64, speed: (f64, f64)) -> anyhow::Result<Vec<Detection>> {
        let mut detections = Vec::new();
        for (marker_id, corners) in self.markers(gray, width, height)? {
            let Some((rotation, translation, reproj_px)) = solve_square_pnp(camera, &corners, marker_length) else { continue };
            let q = UnitQuaternion::from_matrix(&rotation);
            let t_cam_marker = [translation.x, translation.y, translation.z, q.i, q.j, q.k, q.w];
            let (distance_m, view_angle_deg) = view_quality(&t_cam_marker);
            detections.push(Detection {
                ts,
                marker_id,
                t_cam_marker,
                sharpness: tag_sharpness(gray, width, height, &corners),
                reproj_px,
                tag_px: tag_pixel_size(&corners),
                distance_m,
                view_angle_deg,
                lin_speed: speed.0,
                ang_speed: speed.1,
                observations: 1,
            });
        }
        Ok(detections)
    }
}

// ---- grouping --------------------------------------------------------------------------------------------------

fn pose_distance(a: &[f64; 7], b: &[f64; 7], rotation_weight: f64) -> f64 {
    let translation = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    let dot = (a[3] * b[3] + a[4] * b[4] + a[5] * b[5] + a[6] * b[6]).abs().min(1.0);
    translation + rotation_weight * 2.0 * dot.acos()
}

/// A marker's detections split wherever the next one comes more than `gap` later.
pub fn cluster_by_time(detections: &[Detection], gap: f64) -> Vec<Vec<Detection>> {
    let mut by_marker: BTreeMap<i64, Vec<Detection>> = BTreeMap::new();
    for detection in detections {
        by_marker.entry(detection.marker_id).or_default().push(detection.clone());
    }
    let mut clusters = Vec::new();
    for mut list in by_marker.into_values() {
        list.sort_by(|a, b| a.ts.total_cmp(&b.ts));
        let mut current: Vec<Detection> = Vec::new();
        for detection in list {
            if current.last().is_some_and(|last| detection.ts - last.ts > gap) {
                clusters.push(std::mem::take(&mut current));
            }
            current.push(detection);
        }
        if !current.is_empty() {
            clusters.push(current);
        }
    }
    clusters
}

fn huber(residual: f64, delta: f64) -> f64 {
    if residual > delta {
        delta / residual
    } else {
        1.0
    }
}

/// A cluster's representative: its medoid, refined by Huber-weighted IRLS (weighted mean translation, weighted
/// Markley quaternion mean).
pub fn robust_cluster_pose(cluster: &[Detection], grouping: &Grouping) -> Detection {
    let weight = grouping.rotation_weight_m_per_rad;
    let medoid_index = (0..cluster.len())
        .min_by(|&i, &j| {
            let cost = |k: usize| cluster.iter().enumerate().filter(|(m, _)| *m != k).map(|(_, d)| pose_distance(&cluster[k].t_cam_marker, &d.t_cam_marker, weight)).sum::<f64>();
            cost(i).total_cmp(&cost(j))
        })
        .unwrap_or(0);
    let medoid = cluster[medoid_index].clone();
    if cluster.len() < 2 {
        return medoid;
    }
    let reference = nalgebra::Vector4::new(medoid.t_cam_marker[3], medoid.t_cam_marker[4], medoid.t_cam_marker[5], medoid.t_cam_marker[6]);
    let quaternions: Vec<nalgebra::Vector4<f64>> = cluster
        .iter()
        .map(|d| {
            let q = nalgebra::Vector4::new(d.t_cam_marker[3], d.t_cam_marker[4], d.t_cam_marker[5], d.t_cam_marker[6]);
            if q.dot(&reference) < 0.0 {
                -q
            } else {
                q
            }
        })
        .collect();
    let translations: Vec<Vector3<f64>> = cluster.iter().map(|d| Vector3::new(d.t_cam_marker[0], d.t_cam_marker[1], d.t_cam_marker[2])).collect();
    let mut translation = translations[medoid_index];
    let mut rotation = reference;
    let delta_rad = grouping.huber_delta_m / weight.max(1e-9);
    for _ in 0..HUBER_ITERATIONS {
        let weights: Vec<f64> = translations.iter().map(|t| huber((t - translation).norm(), grouping.huber_delta_m)).collect();
        translation = translations.iter().zip(&weights).map(|(t, w)| t * *w).sum::<Vector3<f64>>() / weights.iter().sum::<f64>();
        let mut scatter = Matrix4::<f64>::zeros();
        for q in &quaternions {
            let residual = 2.0 * q.dot(&rotation).abs().clamp(0.0, 1.0).acos();
            scatter += q * q.transpose() * huber(residual, delta_rad);
        }
        let eigen = scatter.symmetric_eigen();
        let largest = (0..4).max_by(|&a, &b| eigen.eigenvalues[a].total_cmp(&eigen.eigenvalues[b])).unwrap();
        rotation = eigen.eigenvectors.column(largest).into_owned();
        if rotation.dot(&reference) < 0.0 {
            rotation = -rotation;
        }
    }
    Detection { t_cam_marker: [translation.x, translation.y, translation.z, rotation[0], rotation[1], rotation[2], rotation[3]], observations: cluster.len(), ..medoid }
}

/// What gating and grouping did to the raw glimpses.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupReport {
    pub raw: usize,
    pub passed: usize,
    pub rejected: BTreeMap<String, usize>,
    pub thin_clusters: usize,
    pub excluded: usize,
    pub groups: usize,
}

/// Raw glimpses → gated → time-clustered → one Huber medoid per cluster (`excluded` markers dropped), by time.
pub fn group(raw: &[Detection], gates: &GlimpseGates, grouping: &Grouping, excluded: &[i64]) -> (Vec<Detection>, GroupReport) {
    let mut report = GroupReport { raw: raw.len(), ..Default::default() };
    let mut kept = Vec::new();
    for detection in raw {
        if excluded.contains(&detection.marker_id) {
            report.excluded += 1;
            continue;
        }
        match rejected_by(detection, gates) {
            Some(reason) => *report.rejected.entry(reason.to_string()).or_default() += 1,
            None => kept.push(detection.clone()),
        }
    }
    report.passed = kept.len();
    let mut grouped = Vec::new();
    for cluster in cluster_by_time(&kept, grouping.cluster_gap_sec) {
        if cluster.len() < grouping.min_observations {
            report.thin_clusters += 1;
            continue;
        }
        grouped.push(robust_cluster_pose(&cluster, grouping));
    }
    grouped.sort_by(|a, b| a.ts.total_cmp(&b.ts));
    report.groups = grouped.len();
    (grouped, report)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use kornia_apriltag::family::TagFamily;

    pub fn test_camera() -> CameraModel {
        CameraModel { fx: 600.0, fy: 600.0, cx: 320.0, cy: 240.0, skew: 0.0, dist: [0.0; 8], model: DistortionModel::Radtan }
    }

    /// The tag36h11 bitmap of `id` at tag coordinates (y down, border at ±1, white margin to ±1.25); None outside.
    fn tag_intensity(family: &TagFamily, id: usize, x: f64, y: f64) -> Option<u8> {
        let margin = family.total_width as f64 / family.width_at_border as f64;
        if x.abs() > margin || y.abs() > margin {
            return None;
        }
        if x.abs() > 1.0 || y.abs() > 1.0 {
            return Some(255);
        }
        let cells = family.width_at_border as f64;
        let cell_x = (((x / 2.0 + 0.5) * cells).floor() as i64).clamp(0, family.width_at_border as i64 - 1) as i8;
        let cell_y = (((y / 2.0 + 0.5) * cells).floor() as i64).clamp(0, family.width_at_border as i64 - 1) as i8;
        let border = family.width_at_border as i8 - 1;
        if cell_x == 0 || cell_y == 0 || cell_x == border || cell_y == border {
            return Some(0);
        }
        let code = family.code_data[id];
        for bit in 0..family.nbits {
            if family.bit_x[bit] == cell_x && family.bit_y[bit] == cell_y {
                return Some(if (code >> (family.nbits - 1 - bit)) & 1 == 1 { 255 } else { 0 });
            }
        }
        Some(0)
    }

    /// A gray frame with tag `id` (side `length` m) at camera ← marker `pose` (marker frame: x right, y up, z out of
    /// the tag), rendered by casting each pixel's ray onto the tag plane.
    pub fn render_tag(camera: &CameraModel, width: usize, height: usize, id: usize, length: f64, rotation: &Matrix3<f64>, translation: &Vector3<f64>) -> Vec<u8> {
        let family = TagFamily::tag36_h11();
        render_marker(camera, width, height, length, rotation, translation, |x, y| tag_intensity(&family, id, x, y))
    }

    /// A gray frame with a marker drawn by `intensity` (tag coordinates as in `tag_intensity`) at camera ← marker.
    pub fn render_marker(camera: &CameraModel, width: usize, height: usize, length: f64, rotation: &Matrix3<f64>, translation: &Vector3<f64>, intensity: impl Fn(f64, f64) -> Option<u8>) -> Vec<u8> {
        let normal = rotation.column(2).into_owned();
        let mut gray = vec![128u8; width * height];
        for row in 0..height {
            for column in 0..width {
                let [x, y] = camera.undistort([column as f64 + 0.5, row as f64 + 0.5]);
                let ray = Vector3::new(x, y, 1.0);
                let along = normal.dot(translation) / normal.dot(&ray);
                if along <= 0.0 {
                    continue;
                }
                let local = rotation.transpose() * (ray * along - translation);
                // tag coordinates: ±1 at the black border's outer edge, y down
                let (tx, ty) = (local.x / (length / 2.0), -local.y / (length / 2.0));
                if let Some(value) = intensity(tx, ty) {
                    gray[row * width + column] = value;
                }
            }
        }
        gray
    }

    #[test]
    fn finds_and_poses_a_rendered_tag() {
        let camera = test_camera();
        // the tag 0.6 m ahead facing the camera (its +z toward it: half a turn about x), turned 20° about y
        let rotation = Rotation3::from_euler_angles(0.0, 20f64.to_radians(), 0.0).matrix() * Rotation3::from_euler_angles(std::f64::consts::PI, 0.0, 0.0).matrix();
        let translation = Vector3::new(0.05, -0.02, 0.6);
        let gray = render_tag(&camera, 640, 480, 7, 0.1, &rotation, &translation);
        let mut detector = TagDetector::new("DICT_APRILTAG_36h11").unwrap();
        let found = detector.detect(1.0, &gray, 640, 480, &camera, 0.1, (-1.0, -1.0)).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        let tag = &found[0];
        assert_eq!(tag.marker_id, 7);
        assert!((tag.t_cam_marker[2] - 0.6).abs() < 0.01 && (tag.t_cam_marker[0] - 0.05).abs() < 0.01, "{:?}", tag.t_cam_marker);
        let angle = quaternion(&tag.t_cam_marker).angle_to(&UnitQuaternion::from_matrix(&rotation));
        assert!(angle < 3f64.to_radians(), "rotation off by {} deg", angle.to_degrees());
        assert!(tag.reproj_px < 1.0 && tag.tag_px > 60.0 && tag.sharpness > 60.0, "{tag:?}");
        let q = UnitQuaternion::from_matrix(&rotation);
        let truth = view_quality(&[translation.x, translation.y, translation.z, q.i, q.j, q.k, q.w]);
        assert!((tag.view_angle_deg - truth.1).abs() < 3.0, "{} vs {}", tag.view_angle_deg, truth.1);
        assert_eq!(rejected_by(tag, &GlimpseGates::default()), None);
    }

    #[test]
    fn undistort_inverts_project() {
        for model in [DistortionModel::Radtan, DistortionModel::Equidistant] {
            let mut camera = test_camera();
            camera.model = model;
            camera.dist = [-0.073, -0.023, 0.001, -0.0005, 0.0, 0.0, 0.0, 0.0];
            for pixel in [[100.0, 80.0], [320.0, 240.0], [500.0, 400.0]] {
                let [x, y] = camera.undistort(pixel);
                let back = camera.project(&Vector3::new(x, y, 1.0));
                assert!((back[0] - pixel[0]).abs() < 1e-3 && (back[1] - pixel[1]).abs() < 1e-3, "{model:?} {pixel:?} -> {back:?}");
            }
        }
    }

    fn glimpse(ts: f64, id: i64, x: f64) -> Detection {
        Detection { ts, marker_id: id, t_cam_marker: [x, 0.0, 0.5, 0.0, 0.0, 0.0, 1.0], sharpness: 100.0, reproj_px: 0.5, tag_px: 50.0, distance_m: 0.5, view_angle_deg: 0.0, lin_speed: 0.1, ang_speed: 1.0, observations: 1 }
    }

    #[test]
    fn groups_by_visit_and_resists_an_outlier() {
        let mut raw: Vec<Detection> = (0..6).map(|i| glimpse(i as f64 * 0.2, 3, 0.01 * (i % 2) as f64)).collect();
        raw.push(glimpse(1.3, 3, 0.4)); // a bad glimpse in the first visit
        raw.extend((0..4).map(|i| glimpse(100.0 + i as f64, 3, 0.0))); // a second visit
        raw.extend((0..2).map(|i| glimpse(50.0 + i as f64, 9, 0.0))); // too thin to keep
        raw.push(Detection { sharpness: 5.0, ..glimpse(2.0, 3, 0.0) }); // blurry
        raw.push(glimpse(60.0, 17, 0.0)); // excluded (a tag on something that moves)
        let (grouped, report) = group(&raw, &GlimpseGates::default(), &Grouping::default(), &[17]);
        assert_eq!(grouped.len(), 2, "{grouped:?}");
        assert_eq!(report.rejected.get("blur"), Some(&1));
        assert_eq!((report.thin_clusters, report.excluded), (1, 1));
        assert!(grouped[0].t_cam_marker[0] < 0.03, "the outlier barely moves it: {:?}", grouped[0].t_cam_marker);
        assert_eq!(grouped[0].observations, 7);
    }
}
