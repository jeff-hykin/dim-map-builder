//! The recording's frame tree as data: which edges are static and which move (decided from the samples, not from
//! the stream they came on: recordings publish static edges on `tf` once or at a rate), the trees (a recording can
//! have several), and which point cloud stream the tf correction should follow by default:
//! - a cloud whose frame is part of a dynamic edge (odom → lidar moves; base_link → lidar doesn't),
//! - in the biggest tree,
//! - nearest that tree's root without being the root.
//!
//! A cloud whose frame *is* a root (registered scans already in odom / world) can still be used, given the sensor's
//! own frame: its points are moved back into that frame first.
use crate::tf::{iso, EdgeSummary, TfTree};
use anyhow::Result;
use dimos_recording::{Kind, Message, Recording};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// tf at its header stamps; an unstamped edge (stamp 0) at the log time of the message that carried it.
pub fn tf_tree(recording: &Recording) -> Result<TfTree> {
    let mut tree = TfTree::default();
    for (log, edge) in recording.tf_edges_logged()? {
        let stamp = edge.header.ts();
        tree.add(if stamp > 0.0 { stamp } else { log }, &edge.header.frame_id, &edge.child, iso(edge.translation, edge.rotation));
    }
    tree.finish();
    Ok(tree)
}

/// The root of `frame`'s tree (following each frame's parent up), None when tf doesn't have the frame.
pub fn root_of(tree: &TfTree, frame: &str) -> Option<String> {
    let edges = tree.edges();
    let frame = frame.trim_start_matches('/');
    if !edges.iter().any(|e| e.parent == frame || e.child == frame) {
        return None;
    }
    let mut at = frame.to_string();
    for _ in 0..edges.len() {
        match edges.iter().filter(|e| e.child == at).max_by_key(|e| e.samples) {
            Some(edge) => at = edge.parent.clone(),
            None => break,
        }
    }
    Some(at)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameNode {
    pub name: String,
    pub parent: Option<String>,
    /// the edge to its parent moves
    pub dynamic: bool,
    /// steps below its root (a root is 0)
    pub depth: usize,
    pub root: String,
    /// frames in its tree
    pub tree_size: usize,
    pub children: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudChoice {
    pub stream: String,
    pub frame: String,
    pub count: u64,
    pub in_tf: bool,
    pub is_root: bool,
    /// its frame is a parent or child of an edge that moves
    pub dynamic: bool,
    pub depth: usize,
    pub tree_size: usize,
    /// a map / accumulated / aggregated cloud, not scans
    pub map_like: bool,
    /// another stream is a corrected copy of this one (`derived_from`)
    pub corrected_copy_exists: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TfAnalysis {
    pub frames: Vec<FrameNode>,
    pub edges: Vec<EdgeSummary>,
    pub roots: Vec<String>,
    pub clouds: Vec<CloudChoice>,
    pub default_cloud: Option<String>,
}

impl TfAnalysis {
    pub fn frame(&self, name: &str) -> Option<&FrameNode> {
        let name = name.trim_start_matches('/');
        self.frames.iter().find(|f| f.name == name)
    }

    /// The frames of `root`'s tree that could be a sensor under it: everything but the root, nearest first.
    pub fn sensor_candidates(&self, root: &str) -> Vec<String> {
        let mut found: Vec<&FrameNode> = self.frames.iter().filter(|f| f.root == root && f.parent.is_some()).collect();
        found.sort_by_key(|f| (f.depth, !f.dynamic, f.name.clone()));
        found.into_iter().map(|f| f.name.clone()).collect()
    }
}

pub(crate) fn map_like(name: &str) -> bool {
    let lower = name.to_lowercase();
    ["map", "accumulated", "aggregated", "voxel"].iter().any(|word| lower.contains(word))
}

/// The frames and edges of `tree`, plus every cloud stream's frame (from its first message) placed in it.
pub fn analyze(recording: &Recording, tree: &TfTree) -> Result<TfAnalysis> {
    let edges = tree.edges();
    // a frame's parent: the edge into it with the most samples (a broken recording can have two)
    let mut parent_of: BTreeMap<String, &EdgeSummary> = BTreeMap::new();
    for edge in &edges {
        let current = parent_of.get(&edge.child);
        if current.is_none_or(|c| edge.samples > c.samples) {
            parent_of.insert(edge.child.clone(), edge);
        }
    }
    let mut names: BTreeSet<String> = BTreeSet::new();
    for edge in &edges {
        names.insert(edge.parent.clone());
        names.insert(edge.child.clone());
    }
    let root_of = |name: &str| -> (String, usize) {
        let mut at = name.to_string();
        let mut depth = 0;
        let mut seen = BTreeSet::new();
        while let Some(edge) = parent_of.get(&at) {
            if !seen.insert(at.clone()) {
                break; // a cycle: stop where it closes
            }
            at = edge.parent.clone();
            depth += 1;
        }
        (at, depth)
    };
    let mut sizes: BTreeMap<String, usize> = BTreeMap::new();
    for name in &names {
        *sizes.entry(root_of(name).0).or_default() += 1;
    }
    let frames: Vec<FrameNode> = names
        .iter()
        .map(|name| {
            let (root, depth) = root_of(name);
            let parent = parent_of.get(name);
            FrameNode {
                name: name.clone(),
                parent: parent.map(|e| e.parent.clone()),
                dynamic: parent.is_some_and(|e| !e.fixed),
                depth,
                tree_size: sizes[&root],
                root,
                children: edges.iter().filter(|e| &e.parent == name).map(|e| e.child.clone()).collect(),
            }
        })
        .collect();
    let mut roots: Vec<String> = frames.iter().filter(|f| f.parent.is_none()).map(|f| f.name.clone()).collect();
    roots.sort_by_key(|r| std::cmp::Reverse(sizes[r]));
    let moving: BTreeSet<&str> = edges.iter().filter(|e| !e.fixed).flat_map(|e| [e.parent.as_str(), e.child.as_str()]).collect();
    let mut clouds = Vec::new();
    let streams = recording.streams()?;
    // a corrected copy (its channel names the raw scans in `derived_from`) comes before the scans it was made from
    let sources: BTreeSet<String> = streams.iter().filter_map(|s| s.derived_from.clone()).collect();
    for stream in streams.into_iter().filter(|s| s.kind == Kind::Cloud && s.count > 0) {
        let mut frame = None;
        recording.for_each(&stream.name, |_, message, _| {
            if let Message::Cloud(cloud) = message {
                frame = Some(cloud.frame_id.trim_start_matches('/').to_string());
                return Ok(false);
            }
            Ok(true)
        })?;
        let Some(frame) = frame else { continue };
        let node = frames.iter().find(|f| f.name == frame);
        clouds.push(CloudChoice {
            map_like: map_like(&stream.name),
            corrected_copy_exists: sources.contains(&stream.name),
            stream: stream.name,
            count: stream.count,
            in_tf: node.is_some(),
            is_root: node.is_some_and(|f| f.parent.is_none()),
            dynamic: moving.contains(frame.as_str()),
            depth: node.map_or(0, |f| f.depth),
            tree_size: node.map_or(0, |f| f.tree_size),
            frame,
        });
    }
    // the default: scans (not a map) placed in tf, a moving frame that isn't a root, the biggest tree, nearest its
    // root, a corrected copy before its source, the most messages
    let default_cloud = clouds
        .iter()
        .filter(|c| c.in_tf && !c.map_like)
        .min_by_key(|c| (c.is_root, !c.dynamic, std::cmp::Reverse(c.tree_size), c.depth, c.corrected_copy_exists, std::cmp::Reverse(c.count)))
        .map(|c| c.stream.clone());
    Ok(TfAnalysis { frames, edges, roots, clouds, default_cloud })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tf::iso;

    fn tree(edges: &[(&str, &str, bool)]) -> TfTree {
        let mut tree = TfTree::default();
        for (parent, child, moving) in edges {
            for step in 0..5 {
                let x = if *moving { step as f64 } else { 0.2 };
                tree.add(step as f64, parent, child, iso([x, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));
            }
        }
        tree.finish();
        tree
    }

    #[test]
    fn static_and_dynamic_come_from_the_samples() {
        let tree = tree(&[("odom", "mid360_link", true), ("mid360_link", "base_link", false), ("base_link", "camera", false), ("other_root", "thing", false)]);
        let edges = tree.edges();
        assert!(!edges.iter().find(|e| e.child == "mid360_link").unwrap().fixed);
        assert!(edges.iter().find(|e| e.child == "base_link").unwrap().fixed);
    }

    #[test]
    fn picks_the_moving_frame_nearest_the_root_of_the_biggest_tree() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.db");
        let mut db = dimos_recording::db::Connection::open(&path).unwrap();
        let cloud = |frame: &str| dimos_recording::lcm::encode_xyz_cloud(&dimos_recording::lcm::Header::at(1.0, frame), &[[1.0, 0.0, 0.0]], None);
        dimos_recording::db::write_stream(&mut db, "registered", "sensor_msgs.PointCloud2", &[(1.0, cloud("odom")), (2.0, cloud("odom")), (3.0, cloud("odom"))]).unwrap();
        dimos_recording::db::write_stream(&mut db, "lidar", "sensor_msgs.PointCloud2", &[(1.0, cloud("mid360_link"))]).unwrap();
        dimos_recording::db::write_stream(&mut db, "camera_points", "sensor_msgs.PointCloud2", &[(1.0, cloud("camera"))]).unwrap();
        dimos_recording::db::write_stream(&mut db, "global_map", "sensor_msgs.PointCloud2", &[(1.0, cloud("mid360_link"))]).unwrap();
        dimos_recording::db::write_stream(&mut db, "elsewhere", "sensor_msgs.PointCloud2", &[(1.0, cloud("thing"))]).unwrap();
        drop(db);
        let recording = Recording::open(&path).unwrap();
        let tree = tree(&[("odom", "mid360_link", true), ("mid360_link", "base_link", false), ("base_link", "camera", false), ("other_root", "thing", true)]);
        let analysis = analyze(&recording, &tree).unwrap();
        assert_eq!(analysis.default_cloud.as_deref(), Some("lidar"));
        assert_eq!(analysis.roots[0], "odom", "the bigger tree first");
        let registered = analysis.clouds.iter().find(|c| c.stream == "registered").unwrap();
        assert!(registered.is_root && registered.dynamic);
        assert_eq!(analysis.sensor_candidates("odom")[0], "mid360_link");
        assert_eq!(analysis.frame("camera").unwrap().depth, 3);
    }
}
