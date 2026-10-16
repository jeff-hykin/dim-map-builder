//! "Global map" outputs: the built map decimated (a final voxel pass, then statistical outlier removal, icp_stitch's
//! `.pc2.lcm` recipe) and written twice: into the recording as `global_map_aggregated` (one PointCloud2 at the
//! recording's first timestamp, in the world frame) and beside it as `<name>_global_map_aggregated.pc2.lcm` (that
//! one LCM-encoded PointCloud2, the file dimos's map loaders read).
use anyhow::Result;
use dimos_recording::lcm::{self, Header};
use dimos_recording::{db, mcap_io, Format};
use kiddo::{ImmutableKdTree, SquaredEuclidean};
use serde::{Deserialize, Serialize};
use std::num::NonZero;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Decimation {
    /// the final voxel (m); 0 = the map's own voxel size
    pub voxel: f32,
    pub remove_outliers: bool,
    /// a point whose mean distance to its `neighbors` nearest is more than `std_ratio` standard deviations past the
    /// average goes
    pub neighbors: usize,
    pub std_ratio: f64,
}

impl Default for Decimation {
    fn default() -> Self {
        Decimation { voxel: 0.0, remove_outliers: true, neighbors: 20, std_ratio: 2.0 }
    }
}

/// One point per occupied voxel (the voxel's mean).
pub fn voxel_downsample(points: &[[f32; 3]], voxel: f32) -> Vec<[f32; 3]> {
    if voxel <= 0.0 {
        return points.to_vec();
    }
    let mut cells: ahash::AHashMap<(i32, i32, i32), ([f64; 3], u32)> = ahash::AHashMap::new();
    for p in points {
        let key = ((p[0] / voxel).floor() as i32, (p[1] / voxel).floor() as i32, (p[2] / voxel).floor() as i32);
        let entry = cells.entry(key).or_insert(([0.0; 3], 0));
        for axis in 0..3 {
            entry.0[axis] += p[axis] as f64;
        }
        entry.1 += 1;
    }
    let mut out: Vec<[f32; 3]> = cells.into_values().map(|(sum, n)| [(sum[0] / n as f64) as f32, (sum[1] / n as f64) as f32, (sum[2] / n as f64) as f32]).collect();
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Open3D's remove_statistical_outlier.
pub fn remove_outliers(points: &[[f32; 3]], neighbors: usize, std_ratio: f64) -> Vec<[f32; 3]> {
    if points.len() <= neighbors + 1 || neighbors == 0 {
        return points.to_vec();
    }
    let positions: Vec<[f64; 3]> = points.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]).collect();
    let tree = ImmutableKdTree::new_from_slice(&positions);
    use rayon::prelude::*;
    let means: Vec<f64> = positions
        .par_iter()
        .map(|p| {
            let near = tree.nearest_n::<SquaredEuclidean>(p, NonZero::new(neighbors + 1).unwrap());
            near.iter().skip(1).map(|n| n.distance.sqrt()).sum::<f64>() / neighbors as f64
        })
        .collect();
    let average = means.iter().sum::<f64>() / means.len() as f64;
    let deviation = (means.iter().map(|m| (m - average).powi(2)).sum::<f64>() / (means.len() - 1) as f64).sqrt();
    let limit = average + std_ratio * deviation;
    points.iter().zip(&means).filter(|(_, m)| **m <= limit).map(|(p, _)| *p).collect()
}

pub fn decimate(points: &[[f32; 3]], voxel_size: f32, options: &Decimation) -> Vec<[f32; 3]> {
    let voxel = if options.voxel > 0.0 { options.voxel } else { voxel_size };
    let thinned = voxel_downsample(points, voxel);
    if options.remove_outliers {
        remove_outliers(&thinned, options.neighbors, options.std_ratio)
    } else {
        thinned
    }
}

/// `<dir>/<recording name>_global_map_aggregated.pc2.lcm`
pub fn lcm_path(recording: &Path) -> PathBuf {
    let stem = recording.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    recording.with_file_name(format!("{stem}_{}.pc2.lcm", crate::pipeline::GLOBAL_MAP_STREAM))
}

/// Writes the map into the recording (`global_map_aggregated` at `start`) and beside it (the .pc2.lcm). Returns the
/// .pc2.lcm's path.
pub fn write(path: &Path, points: &[[f32; 3]], world_frame: &str, start: f64, generation: f64) -> Result<PathBuf> {
    let payload = lcm::encode_xyz_cloud(&Header::at(start, world_frame), points, None);
    match dimos_recording::format_of(path)? {
        Format::Db => db::write_stream(&mut db::Connection::open(path)?, crate::pipeline::GLOBAL_MAP_STREAM, lcm::POINT_CLOUD2_TYPE, &[(start, payload.clone())])?,
        Format::Mcap => mcap_io::append_generated(path, &[mcap_io::GeneratedChannel::lcm(crate::pipeline::GLOBAL_MAP_STREAM, lcm::POINT_CLOUD2_TYPE, generation, vec![(start, payload.clone())])])?,
    }
    let beside = lcm_path(path);
    let partial = beside.with_extension("partial");
    std::fs::write(&partial, &payload)?;
    std::fs::rename(&partial, &beside)?;
    Ok(beside)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimates_and_drops_a_stray_point() {
        let mut points: Vec<[f32; 3]> = Vec::new();
        for i in 0..30 {
            for j in 0..30 {
                points.push([i as f32 * 0.02, j as f32 * 0.02, 0.0]);
                points.push([i as f32 * 0.02 + 0.005, j as f32 * 0.02, 0.0]);
            }
        }
        points.push([5.0, 5.0, 5.0]);
        let out = decimate(&points, 0.02, &Decimation::default());
        assert!(out.len() <= 30 * 30 && out.len() > 800, "{}", out.len());
        assert!(out.iter().all(|p| p[2] < 1.0), "the stray point is gone");
    }

    #[test]
    fn writes_the_stream_and_the_file_beside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("walk.db");
        db::write_stream(&mut db::Connection::open(&path).unwrap(), "x", lcm::STRING_TYPE, &[(5.0, lcm::encode_string("x"))]).unwrap();
        let beside = write(&path, &[[1.0, 2.0, 3.0]], "world", 5.0, 1.0).unwrap();
        assert_eq!(beside.file_name().unwrap(), "walk_global_map_aggregated.pc2.lcm");
        let cloud = lcm::decode_point_cloud2(&std::fs::read(&beside).unwrap()).unwrap();
        assert_eq!((cloud.width, cloud.header.frame_id.as_str()), (1, "world"));
        let recording = dimos_recording::Recording::open(&path).unwrap();
        assert_eq!(recording.latest(crate::pipeline::GLOBAL_MAP_STREAM).unwrap().unwrap().0, 5.0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2, "nothing else left beside it");
    }
}
