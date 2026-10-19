//! Reading and writing dimos recordings (`.db` memory2 SQLite and `.mcap`) without dimos: the message types a map
//! needs (clouds, tf, odometry), in both LCM (dimos's wire format) and ROS 2 CDR (what the Live Viewer's recorder
//! transcodes to). Shared by dim-map-builder's server; the Live Viewer's recorder writes the files it reads.
pub mod cdr;
pub mod db;
pub mod lcm;
pub mod mcap_append;
pub mod mcap_index;
pub mod mcap_io;
pub mod points;
pub mod tf_edit;

use anyhow::{bail, Context, Result};
use lcm::{Odometry, PoseStamped, TfEdge};
use points::Cloud;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Cloud,
    Tf,
    Odometry,
    PoseStamped,
    Path,
    OccupancyGrid,
    Text,
    Image,
    CameraInfo,
    Other,
}

impl Kind {
    /// From a dimos type (`sensor_msgs.PointCloud2`) or a ROS 2 schema name (`sensor_msgs/msg/PointCloud2`).
    pub fn of(type_name: &str) -> Kind {
        let normalized = type_name.replace("/msg/", ".").replace('/', ".");
        match normalized.as_str() {
            "sensor_msgs.PointCloud2" => Kind::Cloud,
            "tf2_msgs.TFMessage" => Kind::Tf,
            "nav_msgs.Odometry" => Kind::Odometry,
            "geometry_msgs.PoseStamped" => Kind::PoseStamped,
            "nav_msgs.Path" => Kind::Path,
            "nav_msgs.OccupancyGrid" => Kind::OccupancyGrid,
            "std_msgs.String" => Kind::Text,
            "sensor_msgs.Image" | "sensor_msgs.CompressedImage" => Kind::Image,
            "sensor_msgs.CameraInfo" => Kind::CameraInfo,
            _ => Kind::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub kind: Kind,
    pub count: u64,
    /// the stream this one was made from (a corrected copy of a lidar), from the mcap channel's `derived_from`
    /// metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
}

/// A decoded message, with the stored observation pose a `.db` row may carry (old datasets put world poses there).
pub enum Message {
    Cloud(Cloud),
    Tf(Vec<TfEdge>),
    Odometry(Odometry),
    PoseStamped(PoseStamped),
    Image(lcm::Image),
    CameraInfo(lcm::CameraInfo),
}

enum Backend {
    /// the streams listed, and counted on first use (`streams`)
    Db(rusqlite::Connection, Vec<db::DbStream>, std::sync::OnceLock<Vec<db::DbStream>>),
    Mcap(mcap_io::McapFile),
}

/// An .mcap's tf as read, by (path, size, mtime): the step list, the preview and a build all start from it. Not for
/// a .db: SQLite writes can land in its -wal file and leave the database file's size and mtime as they were.
static TF_READ: std::sync::Mutex<Vec<(mcap_index::Key, std::sync::Arc<Vec<(f64, TfEdge)>>)>> = std::sync::Mutex::new(Vec::new());
const KEEP_TF_READ: usize = 4;

pub struct Recording {
    pub path: PathBuf,
    backend: Backend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Db,
    Mcap,
}

pub fn format_of(path: &Path) -> Result<Format> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("db") => Ok(Format::Db),
        Some("mcap") => Ok(Format::Mcap),
        _ => bail!("not a recording (.db or .mcap): {}", path.display()),
    }
}

fn decode(encoding: &str, type_name: &str, payload: &[u8]) -> Result<Option<Message>> {
    let cdr = encoding == "cdr";
    let kind = Kind::of(type_name);
    Ok(Some(match kind {
        Kind::Image if cdr && type_name.ends_with("CompressedImage") => Message::Image(cdr::decode_compressed_image(payload)?),
        Kind::Image => Message::Image(if cdr { cdr::decode_image(payload)? } else { lcm::decode_image(payload)? }),
        Kind::CameraInfo => Message::CameraInfo(if cdr { cdr::decode_camera_info(payload)? } else { lcm::decode_camera_info(payload)? }),
        Kind::Cloud => Message::Cloud(points::extract(&if cdr { cdr::decode_point_cloud2(payload)? } else { lcm::decode_point_cloud2(payload)? })?),
        Kind::Tf => Message::Tf(if cdr { cdr::decode_tf(payload)? } else { lcm::decode_tf(payload)? }),
        Kind::Odometry => Message::Odometry(if cdr { cdr::decode_odometry(payload)? } else { lcm::decode_odometry(payload)? }),
        Kind::PoseStamped => Message::PoseStamped(if cdr { cdr::decode_pose_stamped(payload)? } else { lcm::decode_pose_stamped(payload)? }),
        _ => return Ok(None),
    }))
}

impl Recording {
    pub fn open(path: &Path) -> Result<Recording> {
        let backend = match format_of(path)? {
            Format::Db => {
                let connection = db::open_read(path)?;
                let streams = db::stream_list(&connection)?;
                Backend::Db(connection, streams, std::sync::OnceLock::new())
            }
            Format::Mcap => Backend::Mcap(mcap_io::McapFile::open(path)?),
        };
        Ok(Recording { path: path.to_path_buf(), backend })
    }

    pub fn format(&self) -> Format {
        match self.backend {
            Backend::Db(..) => Format::Db,
            Backend::Mcap(_) => Format::Mcap,
        }
    }

    pub fn streams(&self) -> Result<Vec<StreamInfo>> {
        Ok(match &self.backend {
            Backend::Db(connection, listed, counted) => counted
                .get_or_init(|| listed.iter().map(|s| db::DbStream { count: db::count(connection, &s.name), ..s.clone() }).collect())
                .iter()
                .map(|s| StreamInfo { name: s.name.clone(), type_name: s.kind.clone(), kind: Kind::of(&s.kind), count: s.count, derived_from: None })
                .collect(),
            Backend::Mcap(file) => file
                .channels()?
                .into_iter()
                .map(|c| StreamInfo { kind: Kind::of(&c.kind), name: c.topic, type_name: c.kind, count: c.count, derived_from: c.derived_from })
                .collect(),
        })
    }

    /// `streams` without counting a .db's rows (`count` is 0 there; an .mcap's come from its summary): what opening
    /// needs. `has_messages` says whether one has any.
    pub fn stream_list(&self) -> Result<Vec<StreamInfo>> {
        match &self.backend {
            Backend::Db(_, listed, counted) => Ok(counted
                .get()
                .unwrap_or(listed)
                .iter()
                .map(|s| StreamInfo { name: s.name.clone(), type_name: s.kind.clone(), kind: Kind::of(&s.kind), count: s.count, derived_from: None })
                .collect()),
            Backend::Mcap(_) => self.streams(),
        }
    }

    pub fn has_messages(&self, stream: &str) -> Result<bool> {
        match &self.backend {
            Backend::Db(connection, ..) => Ok(db::has_rows(connection, stream)),
            Backend::Mcap(_) => Ok(self.streams()?.iter().any(|s| s.name == stream && s.count > 0)),
        }
    }

    /// Every message of `stream` decoded, with the row's stored pose (db only). `each` returns false to stop.
    pub fn for_each(&self, stream: &str, mut each: impl FnMut(f64, Message, Option<[f64; 7]>) -> Result<bool>) -> Result<()> {
        match &self.backend {
            Backend::Db(connection, streams, _) => {
                let info = streams.iter().find(|s| s.name == stream).with_context(|| format!("no stream {stream}"))?;
                db::for_each(connection, info, |row| match decode("lcm", &info.kind, &row.payload) {
                    Ok(Some(message)) => each(row.ts, message, row.pose),
                    Ok(None) => Ok(true),
                    Err(_) => Ok(true), // one bad row doesn't end a recording
                })
            }
            Backend::Mcap(file) => file.for_each(stream, |ts, encoding, kind, payload| match decode(encoding, kind, payload) {
                Ok(Some(message)) => each(ts, message, None),
                _ => Ok(true),
            }),
        }
    }

    /// Every message of `stream` with its tags (a JSON object as text): a `.db` row's `tags` column; in an `.mcap`, the
    /// JSON string at the same index on the companion topic `<stream>/tags` (how the Map Editor writes tagged streams
    /// there).
    pub fn for_each_tagged(&self, stream: &str, mut each: impl FnMut(f64, Message, Option<String>) -> Result<bool>) -> Result<()> {
        match &self.backend {
            Backend::Db(connection, streams, _) => {
                let info = streams.iter().find(|s| s.name == stream).with_context(|| format!("no stream {stream}"))?;
                db::for_each(connection, info, |row| match decode("lcm", &info.kind, &row.payload) {
                    Ok(Some(message)) => each(row.ts, message, row.tags),
                    _ => Ok(true),
                })
            }
            Backend::Mcap(file) => {
                let mut tags = Vec::new();
                file.for_each(&format!("{stream}/tags"), |_, _, _, payload| {
                    tags.push(lcm::decode_string(payload).ok());
                    Ok(true)
                })?;
                let mut index = 0;
                file.for_each(stream, |ts, encoding, kind, payload| {
                    let tag = tags.get(index).cloned().flatten();
                    index += 1;
                    match decode(encoding, kind, payload) {
                        Ok(Some(message)) => each(ts, message, tag),
                        _ => Ok(true),
                    }
                })
            }
        }
    }

    /// The first and last message time over every stream, if any has messages.
    pub fn time_range(&self) -> Result<Option<(f64, f64)>> {
        match &self.backend {
            Backend::Db(connection, streams, _) => {
                let mut range: Option<(f64, f64)> = None;
                for stream in streams {
                    let found: Option<(Option<f64>, Option<f64>)> = connection
                        .query_row(&format!("SELECT min(ts), max(ts) FROM \"{}\"", stream.name.replace('"', "\"\"")), [], |row| Ok((row.get(0)?, row.get(1)?)))
                        .ok();
                    if let Some((Some(lo), Some(hi))) = found {
                        range = Some(range.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
                    }
                }
                Ok(range)
            }
            Backend::Mcap(file) => Ok(file.index()?.time_range.map(|(start, end)| (start as f64 / 1e9, end as f64 / 1e9))),
        }
    }

    /// Every transform in every tf stream (`tf`, `/tf`, `tf_static`, ...).
    pub fn tf_edges(&self) -> Result<Vec<TfEdge>> {
        Ok(self.tf_edges_logged()?.into_iter().map(|(_, edge)| edge).collect())
    }

    /// Every transform with the log time of the message that carried it (to tell a header stamp's clock). In an .mcap
    /// an edge the Map Editor wrote (a channel naming it in `EDGE_KEY`) comes from the newest such channel only.
    pub fn tf_edges_logged(&self) -> Result<Vec<(f64, TfEdge)>> {
        self.tf_edges_logged_with(None)
    }

    /// The tf topics of an .mcap, when its tf hasn't been read yet (see TF_READ): (topic, bytes of the chunks holding
    /// it). Empty for a .db, or an .mcap whose tf was read already.
    fn tf_to_read(&self) -> Result<Vec<(String, u64)>> {
        let Backend::Mcap(file) = &self.backend else { return Ok(Vec::new()) };
        if mcap_index::key(&self.path).is_some_and(|key| TF_READ.lock().unwrap().iter().any(|(k, _)| *k == key)) {
            return Ok(Vec::new());
        }
        let mut topics: Vec<String> = file.channel_infos()?.into_iter().filter(|c| Kind::of(&c.kind) == Kind::Tf).map(|c| c.topic).collect();
        topics.sort();
        topics.dedup();
        topics.into_iter().map(|topic| Ok((topic.clone(), file.bytes_for(&topic)?))).collect()
    }

    /// The bytes reading the tf takes (0 when it's quick: a .db, or an .mcap whose tf was read already).
    pub fn tf_bytes_to_read(&self) -> Result<u64> {
        Ok(self.tf_to_read()?.iter().map(|(_, bytes)| bytes).sum())
    }

    /// `tf_edges_logged` with `progress(bytes done, total)` while an .mcap's tf chunks are read. An .mcap's tf is
    /// read once per file (same size and mtime: it only changes by growing), whoever asks.
    pub fn tf_edges_logged_with(&self, progress: Option<mcap_index::ProgressFn>) -> Result<Vec<(f64, TfEdge)>> {
        if let Backend::Mcap(file) = &self.backend {
            let key = mcap_index::key(&self.path);
            if let Some(found) = key.as_ref().and_then(|key| TF_READ.lock().unwrap().iter().find(|(k, _)| k == key).map(|(_, edges)| edges.clone())) {
                return Ok(found.as_ref().clone());
            }
            let infos = file.channel_infos()?;
            let mut overrides: std::collections::HashMap<String, (f64, u16)> = std::collections::HashMap::new();
            for info in &infos {
                if let (Some(edge), Some(generation)) = (info.metadata.get(mcap_io::EDGE_KEY), info.generation()) {
                    let entry = overrides.entry(edge.clone()).or_insert((generation, info.id));
                    if generation > entry.0 {
                        *entry = (generation, info.id);
                    }
                }
            }
            let mut edges = Vec::new();
            let topics = self.tf_to_read()?;
            let total: u64 = topics.iter().map(|(_, bytes)| bytes).sum();
            let mut before = 0;
            for (topic, bytes) in topics {
                let report = |done: u64, _: u64| {
                    if let Some(progress) = progress {
                        progress(before + done, total);
                    }
                };
                file.for_each_channel_with(&topic, Some(&report), |ts, channel, payload| {
                    let decoded = if channel.message_encoding == "cdr" { cdr::decode_tf(payload) } else { lcm::decode_tf(payload) };
                    let Ok(list) = decoded else { return Ok(true) };
                    for edge in list {
                        let key = tf_edit::edge_key(edge.header.frame_id.trim_start_matches('/'), edge.child.trim_start_matches('/'));
                        if overrides.get(&key).is_none_or(|(_, id)| *id == channel.id) {
                            edges.push((ts, edge));
                        }
                    }
                    Ok(true)
                })?;
                before += bytes;
            }
            if let Some(key) = key {
                let mut cache = TF_READ.lock().unwrap();
                cache.retain(|(k, _)| k.0 != key.0);
                cache.push((key, std::sync::Arc::new(edges.clone())));
                if cache.len() > KEEP_TF_READ {
                    cache.remove(0);
                }
            }
            return Ok(edges);
        }
        let mut edges = Vec::new();
        for stream in self.streams()?.into_iter().filter(|s| s.kind == Kind::Tf) {
            self.for_each(&stream.name, |ts, message, _| {
                if let Message::Tf(list) = message {
                    edges.extend(list.into_iter().map(|edge| (ts, edge)));
                }
                Ok(true)
            })?;
        }
        Ok(edges)
    }

    /// The newest point cloud on `stream` (by log time), decoded; None when it has none. Only the newest payload is
    /// decoded (a recorded map can be large and logged many times).
    pub fn latest_cloud(&self, stream: &str) -> Result<Option<Cloud>> {
        let newest = match &self.backend {
            Backend::Db(connection, streams, _) => {
                let info = streams.iter().find(|s| s.name == stream).with_context(|| format!("no stream {stream}"))?;
                db::latest(connection, stream)?.map(|(ts, payload)| (ts, "lcm".to_string(), info.kind.clone(), payload))
            }
            Backend::Mcap(file) => {
                let mut newest: Option<(f64, String, String, Vec<u8>)> = None;
                file.for_each(stream, |ts, encoding, kind, payload| {
                    if newest.as_ref().is_none_or(|(at, ..)| ts >= *at) {
                        newest = Some((ts, encoding.to_string(), kind.to_string(), payload.to_vec()));
                    }
                    Ok(true)
                })?;
                newest
            }
        };
        let Some((ts, encoding, kind, payload)) = newest else { return Ok(None) };
        match decode(&encoding, &kind, &payload).with_context(|| format!("the newest message on {stream}"))? {
            Some(Message::Cloud(mut cloud)) => {
                if cloud.ts == 0.0 {
                    cloud.ts = ts;
                }
                Ok(Some(cloud))
            }
            _ => bail!("{stream} isn't a point cloud ({kind})"),
        }
    }

    /// The newest raw payload of a stream / topic (for state the Map Editor saved earlier).
    pub fn latest(&self, stream: &str) -> Result<Option<(f64, Vec<u8>)>> {
        match &self.backend {
            Backend::Db(connection, ..) => db::latest(connection, stream),
            Backend::Mcap(file) => file.latest(stream),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// opening a .db lists its streams without counting them; a reader gets the rows in time order, payloads read as
    /// they're reached
    #[test]
    fn a_db_opens_without_counting_and_reads_in_time_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.db");
        let mut connection = db::Connection::open(&path).unwrap();
        let tf = |x: f64| lcm::encode_tf(&[TfEdge { header: lcm::Header::at(x, "odom"), child: "base_link".into(), translation: [x, 0.0, 0.0], rotation: [0.0, 0.0, 0.0, 1.0] }]);
        db::write_stream(&mut connection, "tf", lcm::TF_TYPE, &[(2.0, tf(2.0)), (1.0, tf(1.0)), (3.0, tf(3.0))]).unwrap();
        db::write_stream(&mut connection, "odom", "nav_msgs.Odometry", &[]).unwrap();
        drop(connection);
        let recording = Recording::open(&path).unwrap();
        let listed: Vec<(String, u64)> = recording.stream_list().unwrap().into_iter().map(|s| (s.name, s.count)).collect();
        assert_eq!(listed, [("odom".to_string(), 0), ("tf".into(), 0)]);
        assert!(recording.has_messages("tf").unwrap() && !recording.has_messages("odom").unwrap());
        let counted: Vec<(String, u64)> = recording.streams().unwrap().into_iter().map(|s| (s.name, s.count)).collect();
        assert_eq!(counted, [("odom".to_string(), 0), ("tf".into(), 3)]);
        let order: Vec<f64> = recording.tf_edges_logged().unwrap().into_iter().map(|(ts, edge)| ts + edge.translation[0] * 10.0).collect();
        assert_eq!(order, [11.0, 22.0, 33.0]);
    }

    #[test]
    fn mcap_cdr_and_lcm_clouds_read_alike() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drive.mcap");
        let lcm_cloud = lcm::encode_xyz_cloud(&lcm::Header::at(2.0, "lidar"), &[[4.0, 5.0, 6.0]], None);
        mcap_io::write_fixture(
            &path,
            &[
                ("/lidar", "cdr", "sensor_msgs/msg/PointCloud2", 1.0, cdr::tests::cloud_cdr(1, "lidar", &[[1.0, 2.0, 3.0]])),
                ("/lidar", "cdr", "sensor_msgs/msg/PointCloud2", 2.0, lcm_cloud.clone()), // garbage as cdr: skipped
                ("/lcm_lidar", "lcm", "sensor_msgs.PointCloud2", 2.0, lcm_cloud),
                ("/tf", "cdr", "tf2_msgs/msg/TFMessage", 1.0, cdr::tests::tf_cdr(1, "odom", "base_link", [1.0, 0.0, 0.0])),
            ],
        );
        let recording = Recording::open(&path).unwrap();
        let kinds: Vec<_> = recording.streams().unwrap().iter().map(|s| (s.name.clone(), s.kind)).collect();
        assert_eq!(kinds, [("/lcm_lidar".to_string(), Kind::Cloud), ("/lidar".into(), Kind::Cloud), ("/tf".into(), Kind::Tf)]);
        let mut clouds = Vec::new();
        for topic in ["/lidar", "/lcm_lidar"] {
            recording
                .for_each(topic, |_, message, _| {
                    if let Message::Cloud(cloud) = message {
                        clouds.push(cloud.points);
                    }
                    Ok(true)
                })
                .unwrap();
        }
        assert_eq!(clouds, vec![vec![[1.0, 2.0, 3.0]], vec![[4.0, 5.0, 6.0]]]);
        assert_eq!(recording.tf_edges().unwrap()[0].child, "base_link");
    }

    /// latest_cloud decodes only the newest message, in an .mcap (cdr) and a .db (lcm) alike
    #[test]
    fn latest_cloud_is_the_newest_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("map.mcap");
        mcap_io::write_fixture(
            &path,
            &[
                ("/global_map", "cdr", "sensor_msgs/msg/PointCloud2", 3.0, cdr::tests::cloud_cdr(3, "world", &[[3.0, 0.0, 0.0], [3.0, 1.0, 0.0]])),
                ("/global_map", "cdr", "sensor_msgs/msg/PointCloud2", 1.0, cdr::tests::cloud_cdr(1, "world", &[[1.0, 0.0, 0.0]])),
                ("/tf", "cdr", "tf2_msgs/msg/TFMessage", 4.0, cdr::tests::tf_cdr(4, "odom", "base_link", [1.0, 0.0, 0.0])),
            ],
        );
        let recording = Recording::open(&path).unwrap();
        let cloud = recording.latest_cloud("/global_map").unwrap().unwrap();
        assert_eq!((cloud.frame_id.as_str(), cloud.points), ("world", vec![[3.0, 0.0, 0.0], [3.0, 1.0, 0.0]]));
        assert!(recording.latest_cloud("/tf").is_err(), "not a cloud");

        let path = dir.path().join("map.db");
        let mut connection = db::Connection::open(&path).unwrap();
        let cloud = |x: f32| lcm::encode_xyz_cloud(&lcm::Header::at(x as f64, "world"), &[[x, 0.0, 0.0]], None);
        db::write_stream(&mut connection, "global_map", "sensor_msgs.PointCloud2", &[(2.0, cloud(2.0)), (5.0, cloud(5.0)), (1.0, cloud(1.0))]).unwrap();
        drop(connection);
        let recording = Recording::open(&path).unwrap();
        assert_eq!(recording.latest_cloud("global_map").unwrap().unwrap().points, vec![[5.0, 0.0, 0.0]]);
    }
}
