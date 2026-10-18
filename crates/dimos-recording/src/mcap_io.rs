//! `.mcap` recordings: channels are either ROS 2 `cdr` (what the Live Viewer's recorder transcodes to, so Foxglove
//! opens them) or `lcm` (raw dimos LCM bytes, type in the channel's `lcm_type` metadata). Saving appends in place
//! (`append`): the summary is cut off, the new chunks go where it was and a summary covering both is put back, so a
//! save costs the bytes saved, never a copy of the recording.
use anyhow::{Context, Result};
use crate::mcap_index::{self, Index, ProgressFn};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

pub const LCM_TYPE_KEY: &str = "lcm_type";
/// channel metadata naming the topic a channel was made from (a corrected copy of a lidar names the raw one)
pub const DERIVED_FROM_KEY: &str = "derived_from";
/// channel metadata on what the Map Editor's generation steps append: when it was written (unix seconds). An mcap is
/// only ever appended to, so a re-run adds a newer channel on the same topic; readers here take the newest one.
pub const GENERATION_KEY: &str = "map_builder_generation";
/// channel metadata on a tf channel the Map Editor appended: the edge it holds ("parent>child"), which replaces that
/// edge everywhere else in the recording for readers here
pub const EDGE_KEY: &str = "map_builder_edge";

/// One channel of the file, from its summary.
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub id: u16,
    pub topic: String,
    pub encoding: String,
    pub kind: String,
    pub metadata: BTreeMap<String, String>,
}

impl ChannelInfo {
    pub fn generation(&self) -> Option<f64> {
        self.metadata.get(GENERATION_KEY).and_then(|value| value.parse().ok())
    }
}

fn kind_of(channel: &mcap::Channel) -> String {
    match channel.message_encoding.as_str() {
        "lcm" => channel.metadata.get(LCM_TYPE_KEY).cloned().unwrap_or_default(),
        _ => channel.schema.as_ref().map(|schema| schema.name.clone()).unwrap_or_default(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct McapChannel {
    pub topic: String,
    pub encoding: String,
    /// the schema name (`sensor_msgs/msg/PointCloud2`) for cdr, the dimos type (`sensor_msgs.PointCloud2`) for lcm
    pub kind: String,
    pub count: u64,
    pub derived_from: Option<String>,
}

pub struct McapFile {
    path: std::path::PathBuf,
    map: memmap2::Mmap,
    index: std::sync::OnceLock<std::sync::Arc<Index>>,
}

impl McapFile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        Ok(McapFile { path: path.to_path_buf(), map: unsafe { memmap2::Mmap::map(&file) }?, index: std::sync::OnceLock::new() })
    }

    /// Where each channel's messages are (mcap_index): the summary's, else the file walked once, on first use.
    pub fn index(&self) -> Result<std::sync::Arc<Index>> {
        self.index_with(None, None)
    }

    /// `index`, walking a file without a summary with `progress(bytes done, total)` and `cancel`.
    pub fn index_with(&self, progress: Option<ProgressFn>, cancel: Option<&std::sync::atomic::AtomicBool>) -> Result<std::sync::Arc<Index>> {
        if let Some(index) = self.index.get() {
            return Ok(index.clone());
        }
        let index = mcap_index::index(&self.path, &self.map, progress, cancel)?;
        Ok(self.index.get_or_init(|| index).clone())
    }

    /// The compression of the file's chunks: the first chunk's ("" = none). A file with no chunk index (cut short)
    /// is taken as uncompressed, the Live Viewer recorder's default.
    pub fn compression(&self) -> Option<mcap::Compression> {
        let summary = mcap::Summary::read(&self.map).ok().flatten()?;
        match summary.chunk_indexes.first()?.compression.as_str() {
            "zstd" => Some(mcap::Compression::Zstd),
            "lz4" => Some(mcap::Compression::Lz4),
            _ => None,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.map
    }

    /// Every channel (several may share a topic), from the summary, else by reading the file.
    pub fn channel_infos(&self) -> Result<Vec<ChannelInfo>> {
        Ok(self
            .index()?
            .channels
            .values()
            .map(|channel| ChannelInfo { id: channel.id, topic: channel.topic.clone(), encoding: channel.message_encoding.clone(), kind: kind_of(channel), metadata: channel.metadata.clone() })
            .collect())
    }

    /// The channels of `topic` a reader should use: the newest Map Editor generation when it appended any (an edge
    /// override channel on a tf topic doesn't count: tf readers sort those out per edge), else all of them.
    pub fn current_channels(&self, topic: &str) -> Result<Vec<u16>> {
        let channels: Vec<ChannelInfo> = self.channel_infos()?.into_iter().filter(|c| c.topic == topic).collect();
        let generated = channels.iter().filter(|c| c.generation().is_some() && !c.metadata.contains_key(EDGE_KEY)).max_by(|a, b| a.generation().unwrap().total_cmp(&b.generation().unwrap()));
        Ok(match generated {
            Some(newest) => vec![newest.id],
            None => channels.iter().map(|c| c.id).collect(),
        })
    }

    /// Older generations of a topic the Map Editor appended to again: what readers skip.
    fn superseded(&self) -> Result<std::collections::HashSet<u16>> {
        let infos = self.channel_infos()?;
        let mut newest: HashMap<&str, (f64, u16)> = HashMap::new();
        for info in infos.iter().filter(|c| !c.metadata.contains_key(EDGE_KEY)) {
            if let Some(generation) = info.generation() {
                let entry = newest.entry(&info.topic).or_insert((generation, info.id));
                if generation > entry.0 {
                    *entry = (generation, info.id);
                }
            }
        }
        Ok(infos.iter().filter(|c| newest.get(c.topic.as_str()).is_some_and(|(_, id)| *id != c.id) && !c.metadata.contains_key(EDGE_KEY)).map(|c| c.id).collect())
    }

    pub fn channels(&self) -> Result<Vec<McapChannel>> {
        let mut by_topic: BTreeMap<String, McapChannel> = BTreeMap::new();
        let index = self.index()?;
        let superseded = self.superseded()?;
        for channel in index.channels.values().filter(|c| !superseded.contains(&c.id)) {
            let entry = by_topic.entry(channel.topic.clone()).or_insert(McapChannel {
                topic: channel.topic.clone(),
                encoding: channel.message_encoding.clone(),
                kind: kind_of(channel),
                count: 0,
                derived_from: channel.metadata.get(DERIVED_FROM_KEY).filter(|v| !v.is_empty()).cloned(),
            });
            entry.count += index.counts.get(&channel.id).copied().unwrap_or(0);
        }
        Ok(by_topic.into_values().collect())
    }

    /// Calls `each(log_time_seconds, encoding, kind, payload)` for every message on `topic`, in file order (log time
    /// for the Live Viewer's recordings). `each` returns false to stop.
    pub fn for_each(&self, topic: &str, mut each: impl FnMut(f64, &str, &str, &[u8]) -> Result<bool>) -> Result<()> {
        let current = self.current_channels(topic)?;
        self.for_each_channel(topic, |ts, channel, payload| {
            if !current.contains(&channel.id) {
                return Ok(true);
            }
            each(ts, &channel.message_encoding, &kind_of(channel), payload)
        })
    }

    /// Every message on `topic` (all its channels), with the channel it came on, read from the chunks that hold it.
    pub fn for_each_channel(&self, topic: &str, each: impl FnMut(f64, &mcap::Channel, &[u8]) -> Result<bool>) -> Result<()> {
        self.for_each_channel_with(topic, None, each)
    }

    /// The bytes `for_each_channel(topic)` reads: the chunks holding it.
    pub fn bytes_for(&self, topic: &str) -> Result<u64> {
        let index = self.index()?;
        let ids: BTreeSet<u16> = index.channels.values().filter(|c| c.topic == topic).map(|c| c.id).collect();
        Ok(mcap_index::bytes_for(&index, &ids))
    }

    /// `for_each_channel` with `progress(bytes done, total)` over the chunks it reads.
    pub fn for_each_channel_with(&self, topic: &str, progress: Option<ProgressFn>, mut each: impl FnMut(f64, &mcap::Channel, &[u8]) -> Result<bool>) -> Result<()> {
        let index = self.index()?;
        let ids: BTreeSet<u16> = index.channels.values().filter(|c| c.topic == topic).map(|c| c.id).collect();
        mcap_index::for_each(&self.map, &index, &ids, progress, |log_time, channel, payload| each(log_time as f64 / 1e9, channel, payload))
    }

    /// The newest message on `topic`.
    pub fn latest(&self, topic: &str) -> Result<Option<(f64, Vec<u8>)>> {
        let mut found = None;
        self.for_each(topic, |ts, _, _, payload| {
            if found.as_ref().is_none_or(|(at, _): &(f64, Vec<u8>)| ts >= *at) {
                found = Some((ts, payload.to_vec()));
            }
            Ok(true)
        })?;
        Ok(found)
    }
}

/// One new LCM channel to add: topic, dimos type, (log time, payload) messages.
pub struct NewChannel {
    pub topic: String,
    pub kind: String,
    pub messages: Vec<(f64, Vec<u8>)>,
}

/// Appends `channels` to the finished recording at `path` in place (mcap_append): nothing already in it is rewritten
/// or removed, so a reader of an earlier save's channels takes the newest message (`McapFile::latest`). A recording
/// with no summary (cut short) can't be appended to; it's refused rather than copied.
pub fn append(path: &Path, channels: &[NewChannel]) -> Result<()> {
    let mut appender = crate::mcap_append::Appender::open(path)?;
    for channel in channels {
        let mut metadata = BTreeMap::new();
        metadata.insert(LCM_TYPE_KEY.to_string(), channel.kind.clone());
        let id = appender.channel(&channel.topic, 0, "lcm", &metadata);
        for (ts, payload) in &channel.messages {
            appender.write(id, (ts.max(0.0) * 1e9) as u64, payload.clone())?;
        }
    }
    appender.finish()?;
    Ok(())
}

/// A channel to add as a new one even when its topic exists (a generation step's output): message encoding, the schema
/// (name, encoding, data) for cdr, its metadata, and (log time, payload) messages.
pub struct GeneratedChannel {
    pub topic: String,
    pub encoding: String,
    pub schema: Option<(String, String, Vec<u8>)>,
    pub metadata: BTreeMap<String, String>,
    pub messages: Vec<(f64, Vec<u8>)>,
}

impl GeneratedChannel {
    /// An LCM channel of dimos type `kind`, stamped with generation `generation`.
    pub fn lcm(topic: &str, kind: &str, generation: f64, messages: Vec<(f64, Vec<u8>)>) -> GeneratedChannel {
        let metadata = BTreeMap::from([(LCM_TYPE_KEY.to_string(), kind.to_string()), (GENERATION_KEY.to_string(), format!("{generation:.6}"))]);
        GeneratedChannel { topic: topic.into(), encoding: "lcm".into(), schema: None, metadata, messages }
    }
}

/// Appends `channels` in place, each as a new channel (see GENERATION_KEY). Big payloads are streamed in time order.
pub fn append_generated(path: &Path, channels: &[GeneratedChannel]) -> Result<()> {
    let mut appender = crate::mcap_append::Appender::open(path)?;
    let mut all: Vec<(f64, u16, &Vec<u8>)> = Vec::new();
    for channel in channels {
        let schema = match &channel.schema {
            Some((name, encoding, data)) => appender.schema(name, encoding, data),
            None => 0,
        };
        let id = appender.new_channel(&channel.topic, schema, &channel.encoding, &channel.metadata);
        all.extend(channel.messages.iter().map(|(ts, payload)| (*ts, id, payload)));
    }
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (ts, id, payload) in all {
        appender.write_stream(id, (ts.max(0.0) * 1e9) as u64, payload.clone())?;
    }
    appender.finish()?;
    Ok(())
}

/// Writes a small mcap (topic, encoding, kind, time, payload): fixtures for tests here and in dependents.
/// Uncompressed chunks, like the Live Viewer's recorder by default.
pub fn write_fixture(path: &Path, messages: &[(&str, &str, &str, f64, Vec<u8>)]) {
    let mut writer = mcap::WriteOptions::new().compression(None).create(std::io::BufWriter::new(std::fs::File::create(path).unwrap())).unwrap();
    let mut channels: HashMap<String, u16> = HashMap::new();
    for (index, (topic, encoding, kind, ts, payload)) in messages.iter().enumerate() {
        let id = *channels.entry(topic.to_string()).or_insert_with(|| {
            if *encoding == "lcm" {
                let metadata = BTreeMap::from([(LCM_TYPE_KEY.to_string(), kind.to_string())]);
                writer.add_channel(0, topic, "lcm", &metadata).unwrap()
            } else {
                let schema = writer.add_schema(kind, "ros2msg", b"").unwrap();
                writer.add_channel(schema, topic, encoding, &BTreeMap::new()).unwrap()
            }
        });
        let time = (ts * 1e9) as u64;
        writer
            .write_to_known_channel(&mcap::records::MessageHeader { channel_id: id, sequence: index as u32, log_time: time, publish_time: time }, payload)
            .unwrap();
    }
    writer.finish().unwrap();
    }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_appends_in_place_and_the_newest_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.mcap");
        write_fixture(&path, &[("/lidar", "cdr", "sensor_msgs/msg/PointCloud2", 1.0, vec![1, 2]), ("/odd", "lcm", "x.Y", 2.0, vec![3])]);
        let before = std::fs::metadata(&path).unwrap().len();
        let save = |ts: f64, text: &[u8]| {
            let channel = NewChannel { topic: "/map_builder/annotations".into(), kind: "std_msgs.String".into(), messages: vec![(ts, text.to_vec())] };
            append(&path, &[channel]).unwrap();
        };
        save(5.0, b"first");
        save(6.0, b"second");
        let file = McapFile::open(&path).unwrap();
        let channels = file.channels().unwrap();
        let topics: Vec<_> = channels.iter().map(|c| (c.topic.as_str(), c.encoding.as_str(), c.kind.as_str(), c.count)).collect();
        assert_eq!(
            topics,
            [("/lidar", "cdr", "sensor_msgs/msg/PointCloud2", 1), ("/map_builder/annotations", "lcm", "std_msgs.String", 2), ("/odd", "lcm", "x.Y", 1)]
        );
        assert_eq!(file.latest("/map_builder/annotations").unwrap(), Some((6.0, b"second".to_vec())));
        assert_eq!(file.latest("/lidar").unwrap(), Some((1.0, vec![1, 2])));
        // grew by the saves, not by a copy; nothing left beside it
        assert!(std::fs::metadata(&path).unwrap().len() < before + 4096);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_newer_generation_replaces_an_older_one_for_readers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.mcap");
        write_fixture(&path, &[("/lidar", "cdr", "sensor_msgs/msg/PointCloud2", 1.0, vec![1, 2])]);
        append_generated(&path, &[GeneratedChannel::lcm("raw_april_tags", "geometry_msgs.PoseStamped", 10.0, vec![(1.0, vec![1]), (2.0, vec![2])])]).unwrap();
        append_generated(&path, &[GeneratedChannel::lcm("raw_april_tags", "geometry_msgs.PoseStamped", 20.0, vec![(1.5, vec![3])])]).unwrap();
        let file = McapFile::open(&path).unwrap();
        let mut seen = Vec::new();
        file.for_each("raw_april_tags", |ts, _, _, payload| {
            seen.push((ts, payload.to_vec()));
            Ok(true)
        })
        .unwrap();
        assert_eq!(seen, vec![(1.5, vec![3])]);
        assert_eq!(file.channel_infos().unwrap().iter().filter(|c| c.topic == "raw_april_tags").count(), 2);
    }

    #[test]
    fn a_recording_cut_short_is_refused_not_copied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.mcap");
        write_fixture(&path, &[("/lidar", "cdr", "sensor_msgs/msg/PointCloud2", 1.0, vec![1, 2])]);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 40]).unwrap();
        let channel = NewChannel { topic: "/a".into(), kind: "std_msgs.String".into(), messages: vec![(1.0, vec![1])] };
        assert!(append(&path, &[channel]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes[..bytes.len() - 40]);
    }
}
