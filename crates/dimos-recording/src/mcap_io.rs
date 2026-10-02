//! `.mcap` recordings: channels are either ROS 2 `cdr` (what the Live Viewer's recorder transcodes to, so Foxglove
//! opens them) or `lcm` (raw dimos LCM bytes, type in the channel's `lcm_type` metadata). mcap has no in-place append,
//! so saving writes a copy with every original record plus the new channels, then renames it over the original.
use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub const LCM_TYPE_KEY: &str = "lcm_type";

#[derive(Debug, Clone, PartialEq)]
pub struct McapChannel {
    pub topic: String,
    pub encoding: String,
    /// the schema name (`sensor_msgs/msg/PointCloud2`) for cdr, the dimos type (`sensor_msgs.PointCloud2`) for lcm
    pub kind: String,
    pub count: u64,
}

pub struct McapFile {
    map: memmap2::Mmap,
}

impl McapFile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        Ok(McapFile { map: unsafe { memmap2::Mmap::map(&file) }? })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.map
    }

    pub fn channels(&self) -> Result<Vec<McapChannel>> {
        let mut by_topic: BTreeMap<String, McapChannel> = BTreeMap::new();
        let summary = mcap::Summary::read(&self.map).ok().flatten();
        let mut add = |channel: &mcap::Channel, count: u64| {
            let kind = match channel.message_encoding.as_str() {
                "lcm" => channel.metadata.get(LCM_TYPE_KEY).cloned().unwrap_or_default(),
                _ => channel.schema.as_ref().map(|schema| schema.name.clone()).unwrap_or_default(),
            };
            let entry = by_topic.entry(channel.topic.clone()).or_insert(McapChannel {
                topic: channel.topic.clone(),
                encoding: channel.message_encoding.clone(),
                kind,
                count: 0,
            });
            entry.count += count;
        };
        match summary {
            Some(summary) if summary.stats.is_some() => {
                let counts = summary.stats.as_ref().map(|stats| stats.channel_message_counts.clone()).unwrap_or_default();
                for channel in summary.channels.values() {
                    add(channel, counts.get(&channel.id).copied().unwrap_or(0));
                }
            }
            _ => {
                for message in mcap::MessageStream::new(&self.map)? {
                    let Ok(message) = message else { break };
                    add(&message.channel, 1);
                }
            }
        }
        Ok(by_topic.into_values().collect())
    }

    /// Calls `each(log_time_seconds, encoding, kind, payload)` for every message on `topic`, in file order (log time
    /// for the Live Viewer's recordings). `each` returns false to stop.
    pub fn for_each(&self, topic: &str, mut each: impl FnMut(f64, &str, &str, &[u8]) -> Result<bool>) -> Result<()> {
        for message in mcap::MessageStream::new(&self.map)? {
            let message = match message {
                Ok(message) => message,
                // a recording cut short (no footer) still yields what was written
                Err(_) => break,
            };
            if message.channel.topic != topic {
                continue;
            }
            let kind = match message.channel.message_encoding.as_str() {
                "lcm" => message.channel.metadata.get(LCM_TYPE_KEY).cloned().unwrap_or_default(),
                _ => message.channel.schema.as_ref().map(|schema| schema.name.clone()).unwrap_or_default(),
            };
            if !each(message.log_time as f64 / 1e9, &message.channel.message_encoding, &kind, &message.data)? {
                break;
            }
        }
        Ok(())
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

/// Rewrites `path` with every original message except those on `replace_topics` (an earlier save's channels), plus
/// `channels`. Writes to a sibling temp file and renames it over the original only when complete.
pub fn rewrite_with(path: &Path, replace_topics: &[String], channels: &[NewChannel], mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let source = McapFile::open(path)?;
    let total = source.bytes().len() as u64;
    let temp = path.with_extension("mcap.saving");
    {
        let file = std::io::BufWriter::new(std::fs::File::create(&temp).with_context(|| format!("creating {}", temp.display()))?);
        let mut writer = mcap::WriteOptions::new().compression(Some(mcap::Compression::Zstd)).create(file)?;
        let mut ids: HashMap<u16, u16> = HashMap::new();
        let mut schemas: HashMap<u16, u16> = HashMap::new();
        let mut written = 0u64;
        for message in mcap::MessageStream::new(source.bytes())? {
            let Ok(message) = message else { break };
            if replace_topics.contains(&message.channel.topic) {
                continue;
            }
            let id = match ids.get(&message.channel.id) {
                Some(id) => *id,
                None => {
                    let schema_id = match &message.channel.schema {
                        Some(schema) => match schemas.get(&schema.id) {
                            Some(id) => *id,
                            None => {
                                let id = writer.add_schema(&schema.name, &schema.encoding, &schema.data)?;
                                schemas.insert(schema.id, id);
                                id
                            }
                        },
                        None => 0,
                    };
                    let id = writer.add_channel(schema_id, &message.channel.topic, &message.channel.message_encoding, &message.channel.metadata)?;
                    ids.insert(message.channel.id, id);
                    id
                }
            };
            let header = mcap::records::MessageHeader { channel_id: id, sequence: message.sequence, log_time: message.log_time, publish_time: message.publish_time };
            writer.write_to_known_channel(&header, &message.data)?;
            written += message.data.len() as u64;
            if written % (16 << 20) < message.data.len() as u64 {
                progress(written.min(total), total);
            }
        }
        for channel in channels {
            let mut metadata = BTreeMap::new();
            metadata.insert(LCM_TYPE_KEY.to_string(), channel.kind.clone());
            let id = writer.add_channel(0, &channel.topic, "lcm", &metadata)?;
            for (sequence, (ts, payload)) in channel.messages.iter().enumerate() {
                let time = (ts.max(0.0) * 1e9) as u64;
                let header = mcap::records::MessageHeader { channel_id: id, sequence: sequence as u32, log_time: time, publish_time: time };
                writer.write_to_known_channel(&header, payload)?;
            }
        }
        writer.finish()?;
    }
    std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))?;
    progress(total, total);
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn write_fixture(path: &Path, messages: &[(&str, &str, &str, f64, Vec<u8>)]) {
        let mut writer = mcap::Writer::new(std::io::BufWriter::new(std::fs::File::create(path).unwrap())).unwrap();
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

    #[test]
    fn rewrite_keeps_originals_and_replaces_previous_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.mcap");
        write_fixture(&path, &[("/lidar", "cdr", "sensor_msgs/msg/PointCloud2", 1.0, vec![1, 2]), ("/odd", "lcm", "x.Y", 2.0, vec![3])]);
        let save = |text: &[u8]| {
            let channel = NewChannel { topic: "/map_builder/annotations".into(), kind: "std_msgs.String".into(), messages: vec![(5.0, text.to_vec())] };
            rewrite_with(&path, &["/map_builder/annotations".into()], &[channel], |_, _| {}).unwrap();
        };
        save(b"first");
        save(b"second");
        let file = McapFile::open(&path).unwrap();
        let channels = file.channels().unwrap();
        let topics: Vec<_> = channels.iter().map(|c| (c.topic.as_str(), c.encoding.as_str(), c.kind.as_str(), c.count)).collect();
        assert_eq!(
            topics,
            [("/lidar", "cdr", "sensor_msgs/msg/PointCloud2", 1), ("/map_builder/annotations", "lcm", "std_msgs.String", 1), ("/odd", "lcm", "x.Y", 1)]
        );
        assert_eq!(file.latest("/map_builder/annotations").unwrap(), Some((5.0, b"second".to_vec())));
        assert!(!path.with_extension("mcap.saving").exists());
    }
}
