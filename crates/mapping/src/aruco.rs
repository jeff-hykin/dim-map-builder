//! ArUco detection with OpenCV's dictionaries and decoding rules, on kornia-apriltag's quads: kornia finds the
//! dark-inside quads (adaptive threshold, gradient clusters, line fits) and its edge refinement (ported here, it's
//! private) puts the corners on the marker's outer border; then, as cv2.aruco does, each quad's cell grid is
//! sampled, Otsu-thresholded, the black border checked (`maxErroneousBitsInBorderRate`) and the bits looked up in the
//! dictionary under all four rotations with OpenCV's tolerance (`maxCorrectionBits * errorCorrectionRate`, first
//! match in id order). Corners come out in cv2's order and pixel convention (TL, TR, BR, BL of the upright marker;
//! a pixel's center at its integer coordinate).
use crate::aruco_dictionaries::DICTIONARIES;
use kornia_apriltag::quad::{fit_quads, Quad};
use kornia_apriltag::segmentation::{find_connected_components, find_gradient_clusters, GradientInfo};
use kornia_apriltag::threshold::{adaptive_threshold, TileMinMax};
use kornia_apriltag::union_find::UnionFind;
use kornia_apriltag::utils::Pixel;
use kornia_apriltag::DecodeTagsConfig;
use kornia_image::allocator::CpuAllocator;
use kornia_image::{Image as KorniaImage, ImageSize};
use std::collections::HashMap;

/// cv2.aruco.DetectorParameters defaults
const ERROR_CORRECTION_RATE: f64 = 0.6;
const MAX_ERRONEOUS_BITS_IN_BORDER_RATE: f64 = 0.35;
const MIN_OTSU_STD_DEV: f64 = 5.0;
/// samples per cell side (cv2 warps each cell to 4x4 pixels)
const SAMPLES_PER_CELL: usize = 4;
/// kornia's: the quads are found at half resolution, refined at full
const DOWNSCALE: usize = 2;

#[derive(Debug, Clone, Copy)]
pub struct ArucoDictionary {
    pub name: &'static str,
    /// data cells per side (the black border is one more cell all round)
    pub size: usize,
    pub codes: &'static [u64],
    /// bits a read code may differ from its marker by
    pub max_correction: usize,
}

impl ArucoDictionary {
    /// An OpenCV dictionary by its cv2.aruco name (`DICT_4X4_50` .. `DICT_7X7_1000`, `DICT_ARUCO_ORIGINAL`,
    /// `DICT_ARUCO_MIP_36h12`), case-insensitively.
    pub fn named(name: &str) -> Option<ArucoDictionary> {
        DICTIONARIES.iter().find(|(known, ..)| known.eq_ignore_ascii_case(name)).map(|&(name, size, codes, count, max_correction_bits)| ArucoDictionary {
            name,
            size,
            codes: &codes[..count],
            max_correction: (max_correction_bits as f64 * ERROR_CORRECTION_RATE) as usize,
        })
    }

    pub fn names() -> impl Iterator<Item = &'static str> {
        DICTIONARIES.iter().map(|(name, ..)| *name)
    }

    /// (marker id, clockwise quarter turns from the bits as read to the upright marker), cv2's `Dictionary.identify`.
    pub fn identify(&self, bits: &[bool]) -> Option<(usize, usize)> {
        let mut rotations = [0u64; 4];
        let mut grid = bits.to_vec();
        for rotation in rotations.iter_mut() {
            *rotation = grid.iter().fold(0u64, |code, bit| (code << 1) | *bit as u64);
            grid = rotate_clockwise(&grid, self.size);
        }
        self.codes.iter().enumerate().find_map(|(id, code)| {
            let (distance, turns) = (0..4).map(|turns| ((code ^ rotations[turns]).count_ones() as usize, turns)).min()?;
            (distance <= self.max_correction).then_some((id, turns))
        })
    }
}

fn rotate_clockwise(grid: &[bool], size: usize) -> Vec<bool> {
    (0..size * size).map(|k| grid[(size - 1 - k % size) * size + k / size]).collect()
}

/// kornia's buffers for one frame size
struct QuadFinder {
    size: ImageSize,
    config: DecodeTagsConfig,
    downscaled: KorniaImage<u8, 1, CpuAllocator>,
    binary: KorniaImage<Pixel, 1, CpuAllocator>,
    tiles: TileMinMax,
    union_find: UnionFind,
    clusters: HashMap<(usize, usize), Vec<GradientInfo>>,
}

impl QuadFinder {
    fn new(size: ImageSize, marker_cells: usize) -> anyhow::Result<QuadFinder> {
        let small = ImageSize { width: size.width / DOWNSCALE, height: size.height / DOWNSCALE };
        let mut config = DecodeTagsConfig::new(Vec::new());
        config.normal_border = true;
        config.reversed_border = false;
        config.min_tag_width = (marker_cells / DOWNSCALE).max(3);
        config.downscale_factor = DOWNSCALE;
        Ok(QuadFinder {
            size,
            config,
            downscaled: KorniaImage::from_size_val(small, 0, CpuAllocator)?,
            binary: KorniaImage::from_size_val(small, Pixel::Skip, CpuAllocator)?,
            tiles: TileMinMax::new(small, 4),
            union_find: UnionFind::new(small.width * small.height),
            clusters: HashMap::new(),
        })
    }

    /// Dark-inside quads, corners refined on the full frame (kornia's pixel convention: a pixel spans [i, i+1)).
    fn quads(&mut self, frame: &KorniaImage<u8, 1, CpuAllocator>) -> anyhow::Result<Vec<Quad>> {
        kornia_imgproc::resize::resize_fast_mono(frame, &mut self.downscaled, kornia_imgproc::interpolation::InterpolationMode::Nearest)?;
        adaptive_threshold(&self.downscaled, &mut self.binary, &mut self.tiles, self.config.min_white_black_difference).map_err(|e| anyhow::anyhow!("{e}"))?;
        find_connected_components(&self.binary, &mut self.union_find).map_err(|e| anyhow::anyhow!("{e}"))?;
        find_gradient_clusters(&self.binary, &mut self.union_find, &mut self.clusters);
        let mut quads = fit_quads(&self.binary, &mut self.clusters, &self.config);
        self.union_find.reset();
        self.clusters.clear();
        for quad in &mut quads {
            refine_edges(frame.as_slice(), frame.width(), frame.height(), quad);
        }
        Ok(quads)
    }
}

/// kornia-apriltag's `refine_edges` (Apache-2.0, private there; apriltag's own): each edge refit to the strongest
/// dark-to-light gradient across it, the corners re-intersected.
fn refine_edges(src: &[u8], width: usize, height: usize, quad: &mut Quad) {
    const RANGE: f32 = 2.0;
    const STEPS_PER_UNIT: usize = 4;
    const STEP_LENGTH: f32 = 1.0 / STEPS_PER_UNIT as f32;
    const MAX_STEPS: usize = 2 * STEPS_PER_UNIT * RANGE as usize + 1;
    const DELTA: f32 = 0.5;
    const GRANGE: f32 = 1.0;
    let bilinear = |x: f32, y: f32| -> Option<f32> {
        let (xi, a) = (x.trunc(), x.fract());
        let (yi, b) = (y.trunc(), y.fract());
        if xi < 0.0 || yi < 0.0 || xi + 1.0 >= width as f32 || yi + 1.0 >= height as f32 {
            return None;
        }
        let top = yi as usize * width + xi as usize;
        let bottom = top + width;
        Some((1.0 - a) * (1.0 - b) * src[top] as f32 + a * (1.0 - b) * src[top + 1] as f32 + (1.0 - a) * b * src[bottom] as f32 + a * b * src[bottom + 1] as f32)
    };
    let mut lines = [[0f32; 4]; 4];
    for edge in 0..4 {
        let (a, b) = (edge, (edge + 1) & 3);
        let mut nx = quad.corners[b].y - quad.corners[a].y;
        let mut ny = -quad.corners[b].x + quad.corners[a].x;
        let mag = (nx * nx + ny * ny).sqrt();
        nx /= mag;
        ny /= mag;
        if quad.reversed_border {
            nx = -nx;
            ny = -ny;
        }
        let nsamples = 16.max((mag / 8.0) as usize);
        let (mut mx, mut my, mut mxx, mut mxy, mut myy, mut n) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for s in 0..nsamples {
            let alpha = (1 + s) as f32 / (nsamples + 1) as f32;
            let x0 = alpha * quad.corners[a].x + (1.0 - alpha) * quad.corners[b].x;
            let y0 = alpha * quad.corners[a].y + (1.0 - alpha) * quad.corners[b].y;
            let (mut mn, mut m_count) = (0.0, 0.0);
            for step in 0..MAX_STEPS {
                let offset = -RANGE + STEP_LENGTH * step as f32;
                let Some(g1) = bilinear(x0 + (offset + GRANGE) * nx - DELTA, y0 + (offset + GRANGE) * ny - DELTA) else { continue };
                let Some(g2) = bilinear(x0 + (offset - GRANGE) * nx - DELTA, y0 + (offset - GRANGE) * ny - DELTA) else { continue };
                if g1 < g2 {
                    continue;
                }
                let weight = (g2 - g1).powi(2);
                mn += weight * offset;
                m_count += weight;
            }
            if m_count <= 0.0 {
                continue;
            }
            let n0 = mn / m_count;
            let (best_x, best_y) = (x0 + n0 * nx, y0 + n0 * ny);
            mx += best_x;
            my += best_y;
            mxx += best_x * best_x;
            mxy += best_x * best_y;
            myy += best_y * best_y;
            n += 1.0;
        }
        let (ex, ey) = (mx / n, my / n);
        let (cxx, cxy, cyy) = (mxx / n - ex * ex, mxy / n - ex * ey, myy / n - ey * ey);
        let normal_theta = 0.5 * (-2.0 * cxy).atan2(cyy - cxx);
        lines[edge] = [ex, ey, normal_theta.cos(), normal_theta.sin()];
    }
    for i in 0..4 {
        let next = (i + 1) & 3;
        let (a00, a01, a10, a11) = (lines[i][3], -lines[next][3], -lines[i][2], lines[next][2]);
        let (b0, b1) = (-lines[i][0] + lines[next][0], -lines[i][1] + lines[next][1]);
        let det = a00 * a11 - a10 * a01;
        if det.abs() > 0.001 {
            let l0 = (a11 / det) * b0 + (-a01 / det) * b1;
            quad.corners[next].x = lines[i][0] + l0 * a00;
            quad.corners[next].y = lines[i][1] + l0 * a10;
        }
    }
}

/// Otsu's threshold over 8-bit values
fn otsu(values: &[f32]) -> f32 {
    let mut histogram = [0usize; 256];
    for v in values {
        histogram[v.round().clamp(0.0, 255.0) as usize] += 1;
    }
    let total = values.len() as f64;
    let sum: f64 = histogram.iter().enumerate().map(|(i, c)| i as f64 * *c as f64).sum();
    let (mut below, mut sum_below, mut best, mut threshold) = (0.0, 0.0, -1.0, 0usize);
    for (i, count) in histogram.iter().enumerate() {
        below += *count as f64;
        sum_below += i as f64 * *count as f64;
        let above = total - below;
        if below == 0.0 || above == 0.0 {
            continue;
        }
        let between = below * above * (sum_below / below - (sum - sum_below) / above).powi(2);
        if between > best {
            best = between;
            threshold = i;
        }
    }
    threshold as f32
}

/// The quad's (size+2)² cells as cv2 reads them (row by row, true = white), border included; None when the marker
/// area is flat (cv2's minOtsuStdDev).
fn read_cells(gray: &[u8], width: usize, height: usize, quad: &Quad, cells: usize) -> Option<Vec<bool>> {
    let sample = |x: f32, y: f32| -> f32 {
        // kornia's coordinates → pixel centers at integers, bilinear, clamped at the frame's edge
        let (x, y) = ((x - 0.5).clamp(0.0, width as f32 - 1.0), (y - 0.5).clamp(0.0, height as f32 - 1.0));
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
        let (a, b) = (x - x0 as f32, y - y0 as f32);
        let at = |x: usize, y: usize| gray[y * width + x] as f32;
        (1.0 - a) * (1.0 - b) * at(x0, y0) + a * (1.0 - b) * at(x1, y0) + (1.0 - a) * b * at(x0, y1) + a * b * at(x1, y1)
    };
    let per_side = cells * SAMPLES_PER_CELL;
    let mut values = Vec::with_capacity(per_side * per_side);
    for row in 0..per_side {
        for column in 0..per_side {
            let tag_x = 2.0 * (column as f32 + 0.5) / per_side as f32 - 1.0;
            let tag_y = 2.0 * (row as f32 + 0.5) / per_side as f32 - 1.0;
            let p = quad.homography_project(tag_x, tag_y);
            if !(p.x.is_finite() && p.y.is_finite()) {
                return None;
            }
            values.push(sample(p.x, p.y));
        }
    }
    // cv2 checks the spread inside the outer half-cell
    let inner: Vec<f32> = (0..per_side * per_side)
        .filter(|k| {
            let (row, column) = (k / per_side, k % per_side);
            let margin = SAMPLES_PER_CELL / 2;
            row >= margin && column >= margin && row < per_side - margin && column < per_side - margin
        })
        .map(|k| values[k])
        .collect();
    let mean = inner.iter().sum::<f32>() as f64 / inner.len() as f64;
    let spread = (inner.iter().map(|v| (*v as f64 - mean).powi(2)).sum::<f64>() / inner.len() as f64).sqrt();
    if spread < MIN_OTSU_STD_DEV {
        return None;
    }
    let threshold = otsu(&values);
    let mut out = Vec::with_capacity(cells * cells);
    for row in 0..cells {
        for column in 0..cells {
            let mut white = 0;
            for sy in 0..SAMPLES_PER_CELL {
                for sx in 0..SAMPLES_PER_CELL {
                    if values[(row * SAMPLES_PER_CELL + sy) * per_side + column * SAMPLES_PER_CELL + sx] > threshold {
                        white += 1;
                    }
                }
            }
            out.push(white > SAMPLES_PER_CELL * SAMPLES_PER_CELL / 2);
        }
    }
    Some(out)
}

fn inside(point: [f64; 2], polygon: &[[f64; 2]; 4]) -> bool {
    let mut sign = 0.0;
    for k in 0..4 {
        let (a, b) = (polygon[k], polygon[(k + 1) % 4]);
        let cross = (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0]);
        if cross * sign < 0.0 {
            return false;
        }
        if cross != 0.0 {
            sign = cross;
        }
    }
    true
}

/// One ArUco marker in one frame: its id and corners (cv2's order and pixel convention).
#[derive(Debug, Clone, PartialEq)]
pub struct ArucoMarker {
    pub id: usize,
    pub corners: [[f64; 2]; 4],
}

/// Finds one dictionary's markers frame by frame, keeping kornia's buffers while the frame size stays the same.
pub struct ArucoDetector {
    pub dictionary: ArucoDictionary,
    finder: Option<QuadFinder>,
}

impl ArucoDetector {
    pub fn new(dictionary: ArucoDictionary) -> ArucoDetector {
        ArucoDetector { dictionary, finder: None }
    }

    pub fn detect(&mut self, gray: &[u8], width: usize, height: usize) -> anyhow::Result<Vec<ArucoMarker>> {
        let size = ImageSize { width, height };
        let cells = self.dictionary.size + 2;
        if self.finder.as_ref().is_none_or(|f| f.size != size) {
            self.finder = Some(QuadFinder::new(size, cells)?);
        }
        let frame = KorniaImage::<u8, 1, CpuAllocator>::new(size, gray.to_vec(), CpuAllocator)?;
        let quads = self.finder.as_mut().unwrap().quads(&frame)?;
        let size_bits = self.dictionary.size;
        let max_border_errors = ((size_bits * size_bits) as f64 * MAX_ERRONEOUS_BITS_IN_BORDER_RATE) as usize;
        let mut found: Vec<(f64, ArucoMarker)> = Vec::new();
        for mut quad in quads {
            if !quad.update_homographies() {
                continue;
            }
            let Some(grid) = read_cells(gray, width, height, &quad, cells) else { continue };
            let border_errors = (0..cells * cells).filter(|k| {
                let (row, column) = (k / cells, k % cells);
                (row == 0 || column == 0 || row == cells - 1 || column == cells - 1) && grid[*k]
            });
            if border_errors.count() > max_border_errors {
                continue;
            }
            let bits: Vec<bool> = (1..=size_bits).flat_map(|row| (1..=size_bits).map(move |column| (row, column))).map(|(row, column)| grid[row * cells + column]).collect();
            let Some((id, turns)) = self.dictionary.identify(&bits) else { continue };
            // the quad's TL, TR, BR, BL as read, then turned to the upright marker's, in cv2's pixel convention
            let read = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(x, y)| {
                let p = quad.homography_project(x, y);
                [p.x as f64 - 0.5, p.y as f64 - 0.5]
            });
            let corners = std::array::from_fn(|k| read[(k + 4 - turns) % 4]);
            found.push((crate::apriltag::tag_pixel_size(&corners), ArucoMarker { id, corners }));
        }
        // a marker read again from a quad inside it (or the same quad twice): keep the outermost
        found.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut kept: Vec<ArucoMarker> = Vec::new();
        for (_, marker) in found {
            let center = [marker.corners.iter().map(|c| c[0]).sum::<f64>() / 4.0, marker.corners.iter().map(|c| c[1]).sum::<f64>() / 4.0];
            if kept.iter().any(|k| inside(center, &k.corners)) {
                continue;
            }
            kept.push(marker);
        }
        Ok(kept)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::apriltag::tests::{render_marker, test_camera};
    use crate::apriltag::{quaternion, TagDetector};
    use nalgebra::{Rotation3, UnitQuaternion, Vector3};

    /// marker `id`'s cells at tag coordinates (y down, the black border's outer edge at ±1, a cell of white quiet zone
    /// around it); None outside
    pub fn aruco_intensity(dictionary: &ArucoDictionary, id: usize, x: f64, y: f64) -> Option<u8> {
        let cells = dictionary.size + 2;
        let margin = (cells + 2) as f64 / cells as f64;
        if x.abs() > margin || y.abs() > margin {
            return None;
        }
        if x.abs() > 1.0 || y.abs() > 1.0 {
            return Some(255);
        }
        let cell = |v: f64| (((v / 2.0 + 0.5) * cells as f64).floor() as usize).min(cells - 1);
        let (column, row) = (cell(x), cell(y));
        if column == 0 || row == 0 || column == cells - 1 || row == cells - 1 {
            return Some(0);
        }
        let bit = (row - 1) * dictionary.size + column - 1;
        Some(if (dictionary.codes[id] >> (dictionary.size * dictionary.size - 1 - bit)) & 1 == 1 { 255 } else { 0 })
    }

    #[test]
    fn finds_and_poses_rendered_aruco_markers() {
        let camera = test_camera();
        for (name, id, turn) in [("DICT_4X4_50", 7, 0.0f64), ("DICT_5X5_100", 61, 100.0), ("DICT_6X6_250", 200, -35.0), ("DICT_7X7_1000", 999, 190.0), ("DICT_ARUCO_ORIGINAL", 1000, 60.0)] {
            let dictionary = ArucoDictionary::named(name).unwrap();
            // facing the camera, spun `turn`° about its normal, then 20° about y
            let rotation = Rotation3::from_euler_angles(0.0, 20f64.to_radians(), 0.0).matrix() * Rotation3::from_euler_angles(std::f64::consts::PI, 0.0, 0.0).matrix() * Rotation3::from_euler_angles(0.0, 0.0, turn.to_radians()).matrix();
            let translation = Vector3::new(0.05, -0.02, 0.6);
            let gray = render_marker(&camera, 640, 480, 0.1, &rotation, &translation, |x, y| aruco_intensity(&dictionary, id, x, y));
            let found = TagDetector::new(name).unwrap().detect(1.0, &gray, 640, 480, &camera, 0.1, (-1.0, -1.0)).unwrap();
            assert_eq!(found.iter().map(|t| t.marker_id).collect::<Vec<_>>(), vec![id as i64], "{name}");
            let tag = &found[0];
            assert!((tag.t_cam_marker[2] - 0.6).abs() < 0.01 && (tag.t_cam_marker[0] - 0.05).abs() < 0.01, "{name} {:?}", tag.t_cam_marker);
            let angle = quaternion(&tag.t_cam_marker).angle_to(&UnitQuaternion::from_matrix(&rotation));
            assert!(angle < 3f64.to_radians(), "{name}: rotation off by {} deg", angle.to_degrees());
            assert!(tag.reproj_px < 1.0, "{name} {tag:?}");
        }
    }

    #[test]
    fn dictionaries_are_opencvs() {
        assert_eq!(ArucoDictionary::names().count(), 18);
        let small = ArucoDictionary::named("DICT_4X4_50").unwrap();
        assert_eq!((small.size, small.codes.len(), small.max_correction), (4, 50, 0));
        // cv2.aruco.getPredefinedDictionary(DICT_4X4_50).generateImageMarker(0, 6): rows 1011 / 0101 / 0011 / 0010
        assert_eq!(small.codes[0], 0b1011_0101_0011_0010);
        let big = ArucoDictionary::named("dict_7x7_250").unwrap();
        assert_eq!((big.size, big.codes.len(), big.max_correction), (7, 250, 4));
        assert!(ArucoDictionary::named("DICT_APRILTAG_36h11").is_none());
    }

    /// scripts/aruco_fixtures.py's frames: the ids cv2.aruco finds, the corners within a pixel of cv2's
    /// (CORNER_REFINE_SUBPIX) and of where they were drawn
    #[test]
    fn agrees_with_opencv_on_its_fixtures() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/aruco");
        let expected: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap()).unwrap();
        let corners_of = |marker: &serde_json::Value| -> [[f64; 2]; 4] { std::array::from_fn(|k| [marker["corners"][k][0].as_f64().unwrap(), marker["corners"][k][1].as_f64().unwrap()]) };
        let farthest = |a: &[[f64; 2]; 4], b: &[[f64; 2]; 4]| (0..4).map(|k| ((a[k][0] - b[k][0]).powi(2) + (a[k][1] - b[k][1]).powi(2)).sqrt()).fold(0.0, f64::max);
        for frame in expected["frames"].as_array().unwrap() {
            let file = frame["file"].as_str().unwrap();
            let image = dimos_recording::lcm::Image { header: dimos_recording::lcm::Header::at(0.0, ""), height: 0, width: 0, encoding: "png".into(), step: 0, data: std::fs::read(dir.join(file)).unwrap() };
            let (gray, width, height) = crate::apriltag::decode_gray(&image).unwrap();
            let mut found = ArucoDetector::new(ArucoDictionary::named(frame["dictionary"].as_str().unwrap()).unwrap()).detect(&gray, width, height).unwrap();
            found.sort_by_key(|m| m.id);
            let mut cv2: Vec<&serde_json::Value> = frame["cv2"].as_array().unwrap().iter().collect();
            cv2.sort_by_key(|m| m["id"].as_u64());
            assert_eq!(found.iter().map(|m| m.id as u64).collect::<Vec<_>>(), cv2.iter().map(|m| m["id"].as_u64().unwrap()).collect::<Vec<_>>(), "{file}");
            for (ours, theirs) in found.iter().zip(&cv2) {
                let placed = frame["placed"].as_array().unwrap().iter().find(|p| p["id"] == theirs["id"]).unwrap();
                let (off_cv2, off_truth) = (farthest(&ours.corners, &corners_of(theirs)), farthest(&ours.corners, &corners_of(placed)));
                assert!(off_cv2 < 1.0 && off_truth < 1.0, "{file} marker {}: {off_cv2:.2} px from cv2's corners, {off_truth:.2} px from the drawn ones", ours.id);
            }
        }
    }

    /// `ARUCO_FRAMES=dir cargo test -p mapping dump_detections -- --ignored`: every `<n>_<DICT>.png` in dir through
    /// the detector (ArUco or AprilTag), into dir/rust.json (for comparing with cv2.aruco)
    #[test]
    #[ignore]
    fn dump_detections() {
        let dir = std::path::PathBuf::from(std::env::var("ARUCO_FRAMES").unwrap());
        let mut out = Vec::new();
        let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().into_string().unwrap()).filter(|f| f.ends_with(".png")).collect();
        files.sort();
        for file in files {
            let name = file.trim_end_matches(".png").split_once('_').unwrap().1.to_string();
            let image = dimos_recording::lcm::Image { header: dimos_recording::lcm::Header::at(0.0, ""), height: 0, width: 0, encoding: "png".into(), step: 0, data: std::fs::read(dir.join(&file)).unwrap() };
            let (gray, width, height) = crate::apriltag::decode_gray(&image).unwrap();
            let found: Vec<serde_json::Value> = TagDetector::new(&name).unwrap().markers(&gray, width, height).unwrap().into_iter().map(|(id, corners)| serde_json::json!({"id": id, "corners": corners})).collect();
            out.push(serde_json::json!({"file": file, "found": found}));
        }
        std::fs::write(dir.join("rust.json"), serde_json::to_string(&out).unwrap()).unwrap();
    }

    #[test]
    fn identifies_every_rotation_and_tolerates_a_flipped_bit() {
        let dictionary = ArucoDictionary::named("DICT_6X6_250").unwrap();
        let bits: Vec<bool> = (0..36).rev().map(|k| (dictionary.codes[42] >> k) & 1 == 1).collect();
        let mut grid = bits.clone();
        for turns in 0..4 {
            assert_eq!(dictionary.identify(&grid), Some((42, turns)));
            // reading it turned one more quarter counter-clockwise needs one more clockwise turn
            grid = rotate_clockwise(&rotate_clockwise(&rotate_clockwise(&grid, 6), 6), 6);
        }
        let mut damaged = bits.clone();
        damaged[7] = !damaged[7];
        damaged[20] = !damaged[20];
        assert_eq!(dictionary.identify(&damaged), Some((42, 0)));
    }
}
