//! A map the recording already carries, used as the aggregated scan in place of Generate: the newest message of its
//! `global_map` PointCloud2 (dimos's `global_map` stream in a `.db`; `/global_map`, or `<namespace>/global_map`, in an
//! `.mcap`: what dimos's mapper publishes, and what lite_record's post_process writes). Nothing is written into the
//! recording; the cloud becomes the editor's map as it is (a voxel map stays on its own grid).
use crate::build::BuildResult;
use anyhow::{bail, Context, Result};
use dimos_recording::{Kind, Recording};
use kiddo::{ImmutableKdTree, SquaredEuclidean};
use std::num::NonZero;

/// the voxel a cloud that isn't a voxel map (its points closer than `MIN_VOXEL`) is thinned to
pub const DEFAULT_VOXEL: f32 = 0.05;
pub const MIN_VOXEL: f32 = 0.02;

/// `global_map`, `/global_map` or `/robot/global_map`; not `global_map_aggregated` (Generate's own output)
pub fn is_global_map(name: &str) -> bool {
    name.rsplit('/').next() == Some("global_map")
}

/// The recording's `global_map` cloud stream with messages (the shortest name when there are several), if any.
pub fn find(recording: &Recording) -> Result<Option<String>> {
    let mut found: Vec<String> = recording.stream_list()?.into_iter().filter(|s| s.kind == Kind::Cloud && is_global_map(&s.name)).map(|s| s.name).collect();
    found.sort_by_key(|name| (name.len(), name.clone()));
    Ok(found.into_iter().find(|name| recording.has_messages(name).unwrap_or(false)))
}

/// The spacing of a voxel map's points: the median distance to the nearest other point (over a sample), to the mm.
/// None for fewer than two distinct points.
pub fn spacing(points: &[[f32; 3]]) -> Option<f32> {
    if points.len() < 2 {
        return None;
    }
    let tree: ImmutableKdTree<f32, 3> = ImmutableKdTree::new_from_slice(points);
    let step = (points.len() / 4000).max(1);
    let mut nearest: Vec<f32> = points.iter().step_by(step).map(|p| tree.nearest_n::<SquaredEuclidean>(p, NonZero::new(2).unwrap())[1].distance.sqrt()).collect();
    nearest.sort_by(f32::total_cmp);
    let median = nearest[nearest.len() / 2];
    (median > 0.0).then(|| (median * 1000.0).round() / 1000.0)
}

/// The newest message of `stream` as a map: its points (non-finite ones dropped) in its own frame, with normals. A
/// `voxel` > 0 thins it to that voxel; else a voxel map keeps its points and its spacing is the voxel size, and a
/// denser cloud is thinned to `DEFAULT_VOXEL`.
pub fn read(recording: &Recording, stream: &str, voxel: Option<f32>) -> Result<BuildResult> {
    let cloud = recording.latest_cloud(stream)?.with_context(|| format!("{stream} has no messages"))?;
    let given = cloud.points.len();
    let points: Vec<[f32; 3]> = cloud.points.into_iter().filter(|p| p.iter().all(|v| v.is_finite())).collect();
    if points.is_empty() {
        bail!("the newest message on {stream} has no points");
    }
    let (voxel_size, points, how) = match (voxel.filter(|v| *v > 0.0), spacing(&points)) {
        (Some(voxel), _) => (voxel, crate::global::voxel_downsample(&points, voxel), format!("thinned to {:.0} cm voxels", voxel * 100.0)),
        (None, Some(spacing)) if spacing >= MIN_VOXEL => (spacing, points, format!("a {:.0} cm voxel map, kept as it is", spacing * 100.0)),
        (None, _) => (DEFAULT_VOXEL, crate::global::voxel_downsample(&points, DEFAULT_VOXEL), format!("denser than {:.0} cm: thinned to {:.0} cm voxels", MIN_VOXEL * 100.0, DEFAULT_VOXEL * 100.0)),
    };
    let normals = crate::voxels::normals(&points, voxel_size * 3.5);
    let note = format!("{stream} (its newest message, at {:.3} s, frame {}): {given} points, {how}: {} voxels", cloud.ts, cloud.frame_id, points.len());
    Ok(BuildResult { voxel_size, world_frame: cloud.frame_id, cloud_stream: stream.to_string(), points, normals, notes: vec![note], ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dimos_recording::lcm::{self, Header};
    use dimos_recording::mcap_io;

    /// an LCM PointCloud2 (cdr is read alike: dimos-recording's own tests)
    fn cloud(at: f64, frame: &str, points: &[[f32; 3]]) -> Vec<u8> {
        lcm::encode_xyz_cloud(&Header::at(at, frame), points, None)
    }

    #[test]
    fn global_map_names() {
        for name in ["global_map", "/global_map", "/robot/global_map"] {
            assert!(is_global_map(name), "{name}");
        }
        for name in ["global_map_aggregated", "/global_map_aggregated", "/global_map/tags", "/lidar", "map/voxels", "/my_global_map"] {
            assert!(!is_global_map(name), "{name}");
        }
    }

    /// a 5 cm voxel slab (x, y in a grid, two layers)
    fn slab(n: usize, voxel: f32) -> Vec<[f32; 3]> {
        (0..n * n * 2).map(|i| [(i % n) as f32 * voxel + voxel / 2.0, ((i / n) % n) as f32 * voxel + voxel / 2.0, (i / (n * n)) as f32 * voxel]).collect()
    }

    #[test]
    fn spacing_is_the_voxel_size() {
        assert_eq!(spacing(&slab(30, 0.05)), Some(0.05));
        assert_eq!(spacing(&slab(30, 0.1)), Some(0.1));
        assert_eq!(spacing(&[[1.0, 1.0, 1.0]]), None);
        assert_eq!(spacing(&[[1.0, 1.0, 1.0], [1.0, 1.0, 1.0]]), None, "duplicates only");
    }

    #[test]
    fn finds_the_global_map_and_uses_its_newest_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("walk.mcap");
        let old = slab(10, 0.05);
        let new = slab(20, 0.05);
        mcap_io::write_fixture(
            &path,
            &[
                ("/lidar", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0, cloud(1.0, "lidar", &[[1.0, 2.0, 3.0]])),
                ("/global_map_aggregated", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0, cloud(1.0, "world", &old)),
                ("/robot/global_map", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0, cloud(1.0, "world", &old)),
                ("/global_map", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0, cloud(1.0, "world", &old)),
                ("/global_map", "lcm", lcm::POINT_CLOUD2_TYPE, 2.0, cloud(2.0, "world", &new)),
            ],
        );
        let recording = Recording::open(&path).unwrap();
        assert_eq!(find(&recording).unwrap().as_deref(), Some("/global_map"));
        let map = read(&recording, "/global_map", None).unwrap();
        assert_eq!((map.points.len(), map.voxel_size, map.world_frame.as_str()), (new.len(), 0.05, "world"));
        assert_eq!(map.normals.len(), map.points.len());
        assert!(map.notes[0].contains("kept as it is"), "{:?}", map.notes);
        let thinned = read(&recording, "/global_map", Some(0.1)).unwrap();
        assert_eq!((thinned.points.len(), thinned.voxel_size), (10 * 10, 0.1));
    }

    #[test]
    fn no_global_map_no_shortcut() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("walk.mcap");
        mcap_io::write_fixture(&path, &[("/lidar", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0, cloud(1.0, "lidar", &[[1.0, 2.0, 3.0]]))]);
        assert_eq!(find(&Recording::open(&path).unwrap()).unwrap(), None);
    }
}
