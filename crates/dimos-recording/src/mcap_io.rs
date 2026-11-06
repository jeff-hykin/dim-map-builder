//! `.mcap` recordings: channels are either ROS 2 `cdr` (what the Live Viewer's recorder transcodes to, so Foxglove
//! opens them) or `lcm` (raw dimos LCM bytes, type in the channel's `lcm_type` metadata). Saving appends in place
//! (`append`): the summary is cut off, the new chunks go where it was and a summary covering both is put back, so a
//! save costs the bytes saved, never a copy of the recording.
use anyhow::{Context, Result};
use crate::mcap_index::{self, Index, ProgressFn};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

pub const LCM_TYPE_KEY: &str = "lcm_type";
/// The channel metadata dimos's McapStore decodes an `lcm` channel by (the payload class, as a .db stream's config names it)
pub const DIMOS_PAYLOAD_TYPE_KEY: &str = "dimos.payload_type";
/// channel metadata naming the topic a channel was made from (a corrected copy of a lidar names the raw one)
pub const DERIVED_FROM_KEY: &str = "derived_from";
/// channel metadata on what the Map Editor's generation steps write: when it was written (unix seconds). A re-run
/// replaces the topic's channel (`replacing`); files from before that have several channels on one topic, of which
/// readers here take the newest.
pub const GENERATION_KEY: &str = "map_builder_generation";
/// channel metadata on a tf channel the Map Editor wrote: the edges it holds ("parent>child", comma separated), which
/// replace those edges everywhere else in the recording for readers here
pub const EDGE_KEY: &str = "map_builder_edge";
/// The topic the Map Editor writes tf edges on, all of them on one channel: never a second channel of the
/// recording's own tf topic.
pub const EDGE_TOPIC: &str = "/map/tf";

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

    /// The tf edges ("parent>child") a Map Editor edge channel holds; empty for any other channel.
    pub fn edges(&self) -> Vec<String> {
        self.metadata.get(EDGE_KEY).map(|edges| edges.split(',').filter(|e| !e.is_empty()).map(String::from).collect()).unwrap_or_default()
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

    /// Older generations of a topic the Map Editor appended to again (files from before a re-run replaced the
    /// topic's channel): what readers skip, and the next write retires.
    pub fn superseded(&self) -> Result<std::collections::HashSet<u16>> {
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
        metadata.insert(DIMOS_PAYLOAD_TYPE_KEY.to_string(), crate::db::payload_module(&channel.kind));
        let id = appender.channel(&channel.topic, 0, "lcm", &metadata);
        for (ts, payload) in &channel.messages {
            appender.write(id, (ts.max(0.0) * 1e9) as u64, payload.clone())?;
        }
    }
    appender.finish()?;
    Ok(())
}

/// The complete new contents of a topic a generation step writes: message encoding, the schema (name, encoding, data)
/// for cdr, its metadata, and (log time, payload) messages. What the Map Editor wrote on that topic before is replaced.
pub struct GeneratedChannel {
    pub topic: String,
    pub encoding: String,
    pub schema: Option<(String, String, Vec<u8>)>,
    pub metadata: BTreeMap<String, String>,
    pub messages: Vec<(f64, Vec<u8>)>,
    /// channels on other topics this one replaces (an edge an older version wrote on the recording's tf topic)
    pub retires: Vec<u16>,
}

impl GeneratedChannel {
    /// An LCM channel of dimos type `kind`, stamped with generation `generation`.
    pub fn lcm(topic: &str, kind: &str, generation: f64, messages: Vec<(f64, Vec<u8>)>) -> GeneratedChannel {
        let metadata = BTreeMap::from([
            (LCM_TYPE_KEY.to_string(), kind.to_string()),
            (DIMOS_PAYLOAD_TYPE_KEY.to_string(), crate::db::payload_module(kind)),
            (GENERATION_KEY.to_string(), format!("{generation:.6}")),
        ]);
        GeneratedChannel { topic: topic.into(), encoding: "lcm".into(), schema: None, metadata, messages, retires: Vec::new() }
    }
}

/// Opens the recording at `path` to write new contents of `topics`, one channel each: the channels the Map Editor
/// wrote on them before are retired (`Appender::open_replacing`), as are `also_retire` and the older generations of
/// any topic (files from before a re-run replaced its channel). A topic the recording itself has (a channel with no
/// generation) is refused: the Map Editor never adds a second channel to a recorded topic, nor removes one.
pub fn replacing(path: &Path, topics: &[&str], also_retire: &[u16]) -> Result<crate::mcap_append::Appender> {
    let retire = {
        let file = McapFile::open(path)?;
        let infos = file.channel_infos()?;
        if let Some(recorded) = infos.iter().find(|c| topics.contains(&c.topic.as_str()) && c.generation().is_none()) {
            anyhow::bail!("{} is recorded in {}; the Map Editor won't add a second channel to it", recorded.topic, path.display());
        }
        let mut retire: BTreeSet<u16> = infos.iter().filter(|c| topics.contains(&c.topic.as_str())).map(|c| c.id).collect();
        retire.extend(also_retire);
        retire.extend(older_generations(&file, &retire)?);
        retire
    };
    crate::mcap_append::Appender::open_replacing(path, &retire)
}

/// The older generations (`McapFile::superseded`) a write retiring `retire` takes along: those whose chunks are
/// theirs alone (an older version's session might have put one with a topic that's still current). The rest stay,
/// read past as before.
fn older_generations(file: &McapFile, retire: &BTreeSet<u16>) -> Result<BTreeSet<u16>> {
    let mut older: BTreeSet<u16> = file.superseded()?.into_iter().collect();
    let chunks = mcap::Summary::read(file.bytes())?.map(|summary| summary.chunk_indexes).unwrap_or_default();
    while let Some(shared) = older.iter().copied().find(|id| {
        chunks.iter().any(|c| c.message_index_offsets.contains_key(id) && c.message_index_offsets.keys().any(|other| !older.contains(other) && !retire.contains(other)))
    }) {
        older.remove(&shared);
    }
    Ok(older)
}

/// Writes `channels` in place, each replacing its topic (`replacing`). Big payloads are streamed in time order. Tf
/// edges an older version wrote beside the recording's own tf move to EDGE_TOPIC on the way.
pub fn append_generated(path: &Path, channels: &[GeneratedChannel]) -> Result<()> {
    let moved = match channels.iter().any(|c| c.topic == EDGE_TOPIC) {
        true => None,
        false => crate::tf_edit::edge_channel(&McapFile::open(path)?, None)?,
    };
    let channels: Vec<&GeneratedChannel> = channels.iter().chain(moved.as_ref()).collect();
    let topics: Vec<&str> = channels.iter().map(|c| c.topic.as_str()).collect();
    let also: Vec<u16> = channels.iter().flat_map(|c| c.retires.iter().copied()).collect();
    let mut appender = replacing(path, &topics, &also)?;
    let mut all: Vec<(f64, u16, &Vec<u8>)> = Vec::new();
    for channel in channels {
        let schema = match &channel.schema {
            Some((name, encoding, data)) => appender.schema(name, encoding, data),
            None => 0,
        };
        let id = appender.channel(&channel.topic, schema, &channel.encoding, &channel.metadata);
        all.extend(channel.messages.iter().map(|(ts, payload)| (*ts, id, payload)));
    }
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (ts, id, payload) in all {
        appender.write_stream(id, (ts.max(0.0) * 1e9) as u64, payload.clone())?;
    }
    appender.finish()?;
    Ok(())
}

/// Brings a recording an older version wrote up to one channel per topic, in place: older generations of a topic
/// are retired and tf edges beside the recording's own tf move to EDGE_TOPIC. Returns whether it changed anything
/// (any generation step's write does this as well).
pub fn migrate(path: &Path) -> Result<bool> {
    let file = McapFile::open(path)?;
    if older_generations(&file, &BTreeSet::new())?.is_empty() && crate::tf_edit::edge_channel(&file, None)?.is_none() {
        return Ok(false);
    }
    drop(file);
    append_generated(path, &[])?;
    Ok(true)
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
        assert_eq!(file.channel_infos().unwrap().iter().filter(|c| c.topic == "raw_april_tags").count(), 1);
    }

    /// What a reader outside the Map Editor sees, which merges channels that share a topic: (topic, log time,
    /// payload) of every message, read linearly (every record in the file) and through the summary's index.
    fn outside(path: &Path) -> (Vec<(String, f64, Vec<u8>)>, Vec<(String, f64, Vec<u8>)>) {
        let bytes = std::fs::read(path).unwrap();
        let row = |m: mcap::Message| (m.channel.topic.clone(), m.log_time as f64 / 1e9, m.data.to_vec());
        let mut linear: Vec<_> = mcap::MessageStream::new(&bytes).unwrap().map(|m| row(m.unwrap())).collect();
        let summary = mcap::Summary::read(&bytes).unwrap().unwrap();
        let mut indexed: Vec<_> = summary.chunk_indexes.iter().flat_map(|c| summary.stream_chunk(&bytes, c).unwrap().map(|m| row(m.unwrap())).collect::<Vec<_>>()).collect();
        linear.sort_by(|a, b| a.partial_cmp(b).unwrap());
        indexed.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (linear, indexed)
    }

    fn channels_on(path: &Path, topic: &str) -> usize {
        McapFile::open(path).unwrap().channel_infos().unwrap().iter().filter(|c| c.topic == topic).count()
    }

    #[test]
    fn a_rerun_replaces_its_topic_on_one_channel_for_every_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.mcap");
        write_fixture(&path, &[("/lidar", "cdr", "sensor_msgs/msg/PointCloud2", 1.0, vec![1, 2])]);
        let tags = |generation: f64, messages: Vec<(f64, Vec<u8>)>| GeneratedChannel::lcm("raw_april_tags", "geometry_msgs.PoseStamped", generation, messages);
        append_generated(&path, &[tags(10.0, vec![(1.0, vec![1]), (2.0, vec![2])])]).unwrap();
        let first = std::fs::metadata(&path).unwrap().len();
        // re-run right away: the old chunks ended the file, so they're cut off
        append_generated(&path, &[tags(20.0, vec![(1.0, vec![3]), (2.0, vec![4])])]).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), first);
        // a save lands between, then a re-run: the old chunks are inside the file now, retired in place
        append(&path, &[NewChannel { topic: "/map_builder/annotations".into(), kind: "std_msgs.String".into(), messages: vec![(5.0, b"a".to_vec())] }]).unwrap();
        append_generated(&path, &[tags(30.0, vec![(1.5, vec![5])])]).unwrap();
        assert_eq!(channels_on(&path, "raw_april_tags"), 1);
        let (linear, indexed) = outside(&path);
        let on = |rows: &[(String, f64, Vec<u8>)]| rows.iter().filter(|r| r.0 == "raw_april_tags").map(|r| (r.1, r.2.clone())).collect::<Vec<_>>();
        assert_eq!(on(&linear), vec![(1.5, vec![5])]);
        assert_eq!(on(&indexed), vec![(1.5, vec![5])]);
        assert_eq!(linear.len(), 3, "the lidar, the save and the newest tag");
        let file = McapFile::open(&path).unwrap();
        assert_eq!(file.latest("raw_april_tags").unwrap(), Some((1.5, vec![5])));
        let counts: Vec<_> = file.channels().unwrap().iter().map(|c| (c.topic.clone(), c.count)).collect();
        assert_eq!(counts, [("/lidar".to_string(), 1), ("/map_builder/annotations".into(), 1), ("raw_april_tags".into(), 1)]);
    }

    #[test]
    fn a_topic_the_recording_has_is_never_given_a_second_channel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.mcap");
        write_fixture(&path, &[("raw_april_tags", "lcm", "geometry_msgs.PoseStamped", 1.0, vec![1])]);
        let before = std::fs::read(&path).unwrap();
        let error = append_generated(&path, &[GeneratedChannel::lcm("raw_april_tags", "geometry_msgs.PoseStamped", 10.0, vec![(1.0, vec![2])])]).unwrap_err();
        assert!(error.to_string().contains("is recorded"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "refused before anything was written");
    }

    /// A file an older version wrote: two generations of map/pipeline, and a world>map edge on a second channel of
    /// the recording's /tf (what controller_1793931551.mcap on the M20 has).
    fn older_version_file(path: &Path) {
        // a chunk per message: what each was appended in apart (the sessions of an older version) keeps to its own chunks
        let mut writer = mcap::WriteOptions::new().compression(None).chunk_size(Some(1)).create(std::io::BufWriter::new(std::fs::File::create(path).unwrap())).unwrap();
        let lcm_channel = |writer: &mut mcap::Writer<_>, topic: &str, kind: &str, extra: &[(&str, &str)]| {
            let mut metadata = BTreeMap::from([(LCM_TYPE_KEY.to_string(), kind.to_string())]);
            metadata.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
            writer.add_channel(0, topic, "lcm", &metadata).unwrap()
        };
        let tf = lcm_channel(&mut writer, "/tf", crate::lcm::TF_TYPE, &[]);
        let edge = lcm_channel(&mut writer, "/tf", crate::lcm::TF_TYPE, &[(GENERATION_KEY, "100.0"), (EDGE_KEY, "world>map")]);
        let old_state = lcm_channel(&mut writer, "map/pipeline", "std_msgs.String", &[(GENERATION_KEY, "90.0")]);
        let new_state = lcm_channel(&mut writer, "map/pipeline", "std_msgs.String", &[(GENERATION_KEY, "100.0")]);
        let transform = |ts: f64, parent: &str, child: &str, x: f64| {
            crate::lcm::encode_tf(&[crate::lcm::TfEdge { header: crate::lcm::Header::at(ts, parent), child: child.into(), translation: [x, 0.0, 0.0], rotation: [0.0, 0.0, 0.0, 1.0] }])
        };
        let mut write = |channel: u16, ts: f64, payload: Vec<u8>| {
            let time = (ts * 1e9) as u64;
            writer.write_to_known_channel(&mcap::records::MessageHeader { channel_id: channel, sequence: 0, log_time: time, publish_time: time }, &payload).unwrap();
        };
        for i in 0..3 {
            write(tf, i as f64, transform(i as f64, "map", "base_link", i as f64));
            write(edge, i as f64, transform(i as f64, "world", "map", 7.0));
        }
        write(old_state, 3.0, crate::lcm::encode_string("old"));
        write(new_state, 4.0, crate::lcm::encode_string("new"));
        writer.finish().unwrap();
    }

    #[test]
    fn a_file_with_duplicate_channels_reads_right_and_migrates_to_one_per_topic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.mcap");
        older_version_file(&path);
        let edges = |path: &Path| {
            let mut found: Vec<_> = crate::Recording::open(path).unwrap().tf_edges().unwrap().into_iter().map(|e| (e.header.frame_id, e.child, e.translation[0])).collect();
            found.sort_by(|a, b| a.partial_cmp(b).unwrap());
            found
        };
        let state = |path: &Path| McapFile::open(path).unwrap().latest("map/pipeline").unwrap().map(|(_, p)| crate::lcm::decode_string(&p).unwrap());
        let before = edges(&path);
        assert_eq!(before.len(), 6);
        assert_eq!(state(&path).as_deref(), Some("new"));
        assert!(migrate(&path).unwrap());
        assert_eq!((channels_on(&path, "/tf"), channels_on(&path, EDGE_TOPIC), channels_on(&path, "map/pipeline")), (1, 1, 1));
        assert_eq!(edges(&path), before, "the same tf, now from /map/tf");
        assert_eq!(state(&path).as_deref(), Some("new"));
        let (linear, indexed) = outside(&path);
        assert_eq!(linear, indexed);
        assert_eq!(linear.iter().filter(|r| r.0 == "map/pipeline").count(), 1);
        assert_eq!(linear.iter().filter(|r| r.0 == "/tf").count(), 3, "only the recorded map>base_link");
        assert_eq!(linear.iter().filter(|r| r.0 == EDGE_TOPIC).count(), 3);
        assert!(!migrate(&path).unwrap(), "nothing left to migrate");
    }

    #[test]
    fn appended_lcm_channels_name_their_dimos_payload_class() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.mcap");
        write_fixture(&path, &[("/lidar", "cdr", "sensor_msgs/msg/PointCloud2", 1.0, vec![1, 2])]);
        append_generated(&path, &[GeneratedChannel::lcm("map_cloud", "sensor_msgs.PointCloud2", 10.0, vec![(1.0, vec![1])])]).unwrap();
        append(&path, &[NewChannel { topic: "map/path".into(), kind: "nav_msgs.Path".into(), messages: vec![(1.0, vec![1])] }]).unwrap();
        let infos = McapFile::open(&path).unwrap().channel_infos().unwrap();
        let payload = |topic: &str| infos.iter().find(|c| c.topic == topic).and_then(|c| c.metadata.get(DIMOS_PAYLOAD_TYPE_KEY).cloned());
        assert_eq!(payload("map_cloud").as_deref(), Some("dimos.msgs.sensor_msgs.PointCloud2.PointCloud2"));
        assert_eq!(payload("map/path").as_deref(), Some("dimos.msgs.nav_msgs.Path.Path"));
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
