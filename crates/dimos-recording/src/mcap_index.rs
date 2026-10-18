//! Where each channel's messages are in an `.mcap`, so reading one topic reads only the chunks that hold it. A
//! finished recording's summary says it (its channels, per-channel counts and the chunk index, which names the channels
//! in each chunk): that's a read of the file's end, no message touched. A recording without one (cut short) is walked
//! once, with progress, and what it found is kept for as long as the file is unchanged (same size and mtime), so it's
//! walked once, not once per question.
use anyhow::{bail, Context, Result};
use mcap::records::op;
use mcap::{Channel, Schema};
use rayon::prelude::*;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A stretch of the file holding messages: a chunk, or records outside any chunk.
#[derive(Debug, Clone)]
pub struct Block {
    /// where its record starts (a chunk) or its first record starts (loose records)
    pub offset: u64,
    pub length: u64,
    pub chunk: bool,
    /// the channels with messages in it; None when the file doesn't say (read it whatever the topic)
    pub channels: Option<BTreeSet<u16>>,
}

#[derive(Debug, Clone, Default)]
pub struct Index {
    pub channels: BTreeMap<u16, Arc<Channel<'static>>>,
    pub counts: BTreeMap<u16, u64>,
    /// in file order
    pub blocks: Vec<Block>,
    /// first and last message log time (ns)
    pub time_range: Option<(u64, u64)>,
    /// from the file's summary (else from walking it)
    pub from_summary: bool,
}

/// What a scan reports: (bytes done, bytes total).
pub type ProgressFn<'a> = &'a dyn Fn(u64, u64);

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The records in `buf` as (opcode, body, offset after it); stops at one cut short.
fn records(buf: &[u8]) -> impl Iterator<Item = (u8, &[u8], usize)> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let op = *buf.get(at)?;
        let len = usize::try_from(u64_at(buf, at + 1)?).ok()?;
        let body = buf.get(at + 9..(at + 9).checked_add(len)?)?;
        at += 9 + len;
        Some((op, body, at))
    })
}

/// A message record's (channel, log time, payload).
fn message(body: &[u8]) -> Option<(u16, u64, &[u8])> {
    Some((u16_at(body, 0)?, u64_at(body, 6)?, body.get(22..)?))
}

/// A chunk record's body → its records, decompressed.
fn chunk_records(body: &[u8]) -> Result<Cow<'_, [u8]>> {
    let uncompressed = u64_at(body, 16).context("short chunk")? as usize;
    let name_len = u32_at(body, 28).context("short chunk")? as usize;
    let compression = std::str::from_utf8(body.get(32..32 + name_len).context("short chunk")?)?;
    let length = u64_at(body, 32 + name_len).context("short chunk")? as usize;
    let data = body.get(40 + name_len..40 + name_len + length).context("short chunk")?;
    Ok(match compression {
        "" => Cow::Borrowed(data),
        "zstd" => Cow::Owned(zstd::bulk::decompress(data, uncompressed).context("zstd chunk")?),
        "lz4" => {
            let mut out = Vec::with_capacity(uncompressed);
            std::io::Read::read_to_end(&mut lz4_flex::frame::FrameDecoder::new(data), &mut out).context("lz4 chunk")?;
            Cow::Owned(out)
        }
        other => bail!("unsupported chunk compression {other}"),
    })
}

/// The summary's index, when it covers every message: statistics, and a chunk index entry per chunk it counted.
fn from_summary(map: &[u8]) -> Option<Index> {
    let summary = mcap::Summary::read(map).ok().flatten()?;
    let stats = summary.stats.as_ref()?;
    let undeclared = stats.channel_message_counts.iter().any(|(id, n)| *n > 0 && !summary.channels.contains_key(id));
    if summary.chunk_indexes.len() != stats.chunk_count as usize || (stats.message_count > 0 && summary.chunk_indexes.is_empty()) || undeclared {
        return None;
    }
    let mut blocks: Vec<Block> = summary
        .chunk_indexes
        .iter()
        .map(|c| Block {
            offset: c.chunk_start_offset,
            length: c.chunk_length,
            chunk: true,
            channels: (!c.message_index_offsets.is_empty()).then(|| c.message_index_offsets.keys().copied().collect()),
        })
        .collect();
    blocks.sort_by_key(|b| b.offset);
    Some(Index {
        channels: summary.channels.iter().map(|(id, c)| (*id, c.clone())).collect(),
        counts: stats.channel_message_counts.clone(),
        blocks,
        time_range: (stats.message_count > 0).then_some((stats.message_start_time, stats.message_end_time)),
        from_summary: true,
    })
}

/// Channels and schemas as a walk declares them (the first declaration of an id wins).
#[derive(Default)]
struct Declared {
    schemas: HashMap<u16, Arc<Schema<'static>>>,
    channels: HashMap<u16, Arc<Channel<'static>>>,
}

impl Declared {
    fn add(&mut self, op: u8, body: &[u8]) {
        match mcap::parse_record(op, body) {
            Ok(mcap::records::Record::Schema { header, data }) => {
                self.schemas.entry(header.id).or_insert_with(|| Arc::new(Schema { id: header.id, name: header.name, encoding: header.encoding, data: Cow::Owned(data.into_owned()) }));
            }
            Ok(mcap::records::Record::Channel(c)) => {
                let schema = self.schemas.get(&c.schema_id).cloned();
                self.channels.entry(c.id).or_insert_with(|| Arc::new(Channel { id: c.id, topic: c.topic, schema, message_encoding: c.message_encoding, metadata: c.metadata }));
            }
            _ => {}
        }
    }
}

/// Walks the whole data section once: the channels with messages, their counts, the chunks (and loose records) and
/// which channels each holds. Stops where the file is cut short.
fn by_walking(map: &[u8], progress: Option<ProgressFn>, cancel: Option<&AtomicBool>) -> Result<Index> {
    let total = map.len() as u64;
    let mut index = Index::default();
    let mut declared = Declared::default();
    let start = 8.min(map.len()); // the magic
    let mut loose: Option<Block> = None;
    let mut last_report = 0u64;
    let seen = |index: &mut Index, declared: &Declared, channel: u16, log_time: u64| -> bool {
        let Some(c) = declared.channels.get(&channel) else { return false };
        index.channels.entry(channel).or_insert_with(|| c.clone());
        *index.counts.entry(channel).or_default() += 1;
        index.time_range = Some(index.time_range.map_or((log_time, log_time), |(a, b)| (a.min(log_time), b.max(log_time))));
        true
    };
    for (op, body, end) in records(&map[start..]) {
        let offset = (start + end - 9 - body.len()) as u64;
        let end = (start + end) as u64;
        if end - last_report > 64 << 20 {
            last_report = end;
            if let Some(progress) = progress {
                progress(end, total);
            }
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                bail!("cancelled");
            }
        }
        match op {
            op::DATA_END | op::FOOTER => break,
            op::SCHEMA | op::CHANNEL => declared.add(op, body),
            op::MESSAGE => {
                let Some((channel, log_time, _)) = message(body) else { break };
                if !seen(&mut index, &declared, channel, log_time) {
                    break; // a message on a channel never declared: what a linear reader stops at too
                }
                let block = loose.get_or_insert(Block { offset, length: 0, chunk: false, channels: Some(BTreeSet::new()) });
                block.length = end - block.offset;
                block.channels.get_or_insert_with(BTreeSet::new).insert(channel);
            }
            op::CHUNK => {
                if let Some(block) = loose.take() {
                    index.blocks.push(block);
                }
                let Ok(inner) = chunk_records(body) else { break };
                let mut channels = BTreeSet::new();
                for (op, body, _) in records(&inner) {
                    match op {
                        op::SCHEMA | op::CHANNEL => declared.add(op, body),
                        op::MESSAGE => {
                            if let Some((channel, log_time, _)) = message(body) {
                                if seen(&mut index, &declared, channel, log_time) {
                                    channels.insert(channel);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                index.blocks.push(Block { offset, length: end - offset, chunk: true, channels: Some(channels) });
            }
            _ => {
                // an attachment, metadata, a message index...: ends a run of loose messages
                if let Some(block) = loose.take() {
                    index.blocks.push(block);
                }
            }
        }
    }
    if let Some(block) = loose.take() {
        index.blocks.push(block);
    }
    if let Some(progress) = progress {
        progress(total, total);
    }
    Ok(index)
}

/// Indexes worked out by walking a file, by (path, size, mtime): a file that changes is walked again.
pub(crate) type Key = (PathBuf, u64, Option<std::time::SystemTime>);
static WALKED: Mutex<Vec<(Key, Arc<Index>)>> = Mutex::new(Vec::new());
const KEEP_WALKED: usize = 8;

pub(crate) fn key(path: &Path) -> Option<Key> {
    let meta = std::fs::metadata(path).ok()?;
    Some((path.canonicalize().unwrap_or_else(|_| path.to_path_buf()), meta.len(), meta.modified().ok()))
}

fn walked(path: &Path) -> Option<Arc<Index>> {
    let key = key(path)?;
    WALKED.lock().unwrap().iter().find(|(k, _)| *k == key).map(|(_, index)| index.clone())
}

/// The index of the file at `path` mapped as `map`: from its summary, from an earlier walk of the same file, else by
/// walking it now (`progress` / `cancel` for that walk).
pub fn index(path: &Path, map: &[u8], progress: Option<ProgressFn>, cancel: Option<&AtomicBool>) -> Result<Arc<Index>> {
    if let Some(index) = from_summary(map) {
        return Ok(Arc::new(index));
    }
    if let Some(index) = walked(path) {
        return Ok(index);
    }
    let index = Arc::new(by_walking(map, progress, cancel)?);
    if let Some(key) = key(path) {
        let mut cache = WALKED.lock().unwrap();
        cache.retain(|(k, _)| k.0 != key.0);
        cache.push((key, index.clone()));
        if cache.len() > KEEP_WALKED {
            cache.remove(0);
        }
    }
    Ok(index)
}

/// Whether asking `path` about its messages is cheap: its summary indexes it, or it was walked already.
pub fn is_indexed(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else { return false };
    let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else { return false };
    walked(path).is_some() || from_summary(&map).is_some()
}

/// One block's messages on `channels`, in file order: (log time ns, channel, payload).
fn block_messages(map: &[u8], block: &Block, channels: &BTreeSet<u16>) -> Result<Vec<(u64, u16, Vec<u8>)>> {
    let start = block.offset as usize;
    let end = start + block.length as usize;
    let bytes = map.get(start..end).context("an index entry past the end of the file")?;
    let walk = |buf: &[u8], found: &mut Vec<(u64, u16, Vec<u8>)>| {
        for (op, body, _) in records(buf) {
            if op == op::MESSAGE {
                if let Some((channel, log_time, data)) = message(body) {
                    if channels.contains(&channel) {
                        found.push((log_time, channel, data.to_vec()));
                    }
                }
            }
        }
    };
    let mut found = Vec::new();
    if block.chunk {
        let body = bytes.get(9..).context("short chunk")?;
        if bytes.first() != Some(&op::CHUNK) {
            bail!("the chunk index points at something that isn't a chunk");
        }
        walk(&chunk_records(body)?, &mut found);
    } else {
        walk(bytes, &mut found);
    }
    Ok(found)
}

/// The bytes `for_each` reads for `channels`: the blocks holding them.
pub fn bytes_for(index: &Index, channels: &BTreeSet<u16>) -> u64 {
    if channels.is_empty() {
        return 0;
    }
    index.blocks.iter().filter(|b| b.channels.as_ref().is_none_or(|ids| !ids.is_disjoint(channels))).map(|b| b.length).sum()
}

/// How many blocks are read at once (in parallel); it starts at one and doubles, so a reader that wants only the
/// first message decompresses one chunk, not a window of them.
const MAX_WINDOW: usize = 64;

/// Calls `each(log_time_ns, channel, payload)` for every message on `channels`, in file order, reading only the blocks
/// that hold them. `each` returns false to stop. `progress(done, total)` in bytes of those blocks.
pub fn for_each(map: &[u8], index: &Index, channels: &BTreeSet<u16>, progress: Option<ProgressFn>, mut each: impl FnMut(u64, &Arc<Channel<'static>>, &[u8]) -> Result<bool>) -> Result<()> {
    if channels.is_empty() {
        return Ok(());
    }
    let blocks: Vec<&Block> = index.blocks.iter().filter(|b| b.channels.as_ref().is_none_or(|ids| !ids.is_disjoint(channels))).collect();
    let total: u64 = blocks.iter().map(|b| b.length).sum();
    let mut done = 0u64;
    let mut at = 0;
    let mut window = 1;
    while at < blocks.len() {
        let group = &blocks[at..(at + window).min(blocks.len())];
        at += group.len();
        window = (window * 2).min(MAX_WINDOW);
        let read: Vec<Result<Vec<(u64, u16, Vec<u8>)>>> = group.par_iter().map(|block| block_messages(map, block, channels)).collect();
        for (block, messages) in group.iter().zip(read) {
            // a chunk that can't be read ends the read, as a linear reader stops at it
            let Ok(messages) = messages else { return Ok(()) };
            for (log_time, channel, data) in messages {
                let Some(channel) = index.channels.get(&channel) else { continue };
                if !each(log_time, channel, &data)? {
                    return Ok(());
                }
            }
            done += block.length;
        }
        if let Some(progress) = progress {
            progress(done, total);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcap_io::McapFile;

    /// Three topics over many small chunks (or none), compressed or not: /tf on every 10th message only.
    fn write(path: &Path, compression: Option<mcap::Compression>, chunks: bool) {
        let options = mcap::WriteOptions::new().compression(compression).chunk_size(Some(512)).use_chunks(chunks);
        let mut writer = options.create(std::io::BufWriter::new(std::fs::File::create(path).unwrap())).unwrap();
        let schema = writer.add_schema("sensor_msgs/msg/Image", "ros2msg", b"").unwrap();
        let image = writer.add_channel(schema, "/camera", "cdr", &BTreeMap::new()).unwrap();
        let tf = writer.add_channel(schema, "/tf", "cdr", &BTreeMap::new()).unwrap();
        let lcm = writer.add_channel(0, "/lcm", "lcm", &BTreeMap::from([("lcm_type".to_string(), "std_msgs.String".to_string())])).unwrap();
        for n in 0..400u32 {
            let channel = if n % 10 == 0 { tf } else if n % 3 == 0 { lcm } else { image };
            let time = 1_000_000_000 + n as u64 * 10_000_000;
            let payload: Vec<u8> = (0..(n % 50 + 8)).map(|i| (i + n) as u8).collect();
            writer.write_to_known_channel(&mcap::records::MessageHeader { channel_id: channel, sequence: n, log_time: time, publish_time: time }, &payload).unwrap();
        }
        writer.finish().unwrap();
    }

    /// What the mcap crate's linear reader sees on `topic`: (seconds, payload).
    fn linear(path: &Path, topic: &str) -> Vec<(f64, Vec<u8>)> {
        let bytes = std::fs::read(path).unwrap();
        mcap::MessageStream::new(&bytes).unwrap().map_while(Result::ok).filter(|m| m.channel.topic == topic).map(|m| (m.log_time as f64 / 1e9, m.data.to_vec())).collect()
    }

    fn read(file: &McapFile, topic: &str) -> Vec<(f64, Vec<u8>)> {
        let mut found = Vec::new();
        file.for_each_channel(topic, |ts, _, payload| {
            found.push((ts, payload.to_vec()));
            Ok(true)
        })
        .unwrap();
        found
    }

    fn reads_alike(path: &Path, from_summary: bool) {
        let file = McapFile::open(path).unwrap();
        let index = file.index().unwrap();
        assert_eq!(index.from_summary, from_summary);
        for topic in ["/camera", "/tf", "/lcm", "/absent"] {
            assert_eq!(read(&file, topic), linear(path, topic), "{topic}");
        }
        let counts: Vec<(String, u64)> = file.channels().unwrap().into_iter().map(|c| (c.topic, c.count)).collect();
        let expected: Vec<(String, u64)> = ["/camera", "/lcm", "/tf"].iter().map(|t| (t.to_string(), linear(path, t).len() as u64)).collect();
        assert_eq!(counts, expected);
        // the first message only: one chunk read
        let mut first = None;
        file.for_each_channel("/camera", |ts, _, _| {
            first = Some(ts);
            Ok(false)
        })
        .unwrap();
        assert_eq!(first, linear(path, "/camera").first().map(|m| m.0));
    }

    #[test]
    fn a_summary_lets_a_topic_be_read_from_its_own_chunks() {
        let dir = tempfile::tempdir().unwrap();
        for (name, compression) in [("zstd", Some(mcap::Compression::Zstd)), ("lz4", Some(mcap::Compression::Lz4)), ("none", None)] {
            let path = dir.path().join(format!("{name}.mcap"));
            write(&path, compression, true);
            reads_alike(&path, true);
            let file = McapFile::open(&path).unwrap();
            let index = file.index().unwrap();
            assert!(index.blocks.len() > 20, "{name}: {} chunks", index.blocks.len());
            let tf: BTreeSet<u16> = index.channels.values().filter(|c| c.topic == "/tf").map(|c| c.id).collect();
            let holding = index.blocks.iter().filter(|b| !b.channels.as_ref().unwrap().is_disjoint(&tf)).count();
            assert!(holding < index.blocks.len(), "{name}: /tf is in {holding} of {} chunks", index.blocks.len());
        }
    }

    #[test]
    fn a_recording_without_a_summary_is_walked_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cut.mcap");
        write(&path, Some(mcap::Compression::Zstd), true);
        // cut where the data section ends: no summary, no footer
        let bytes = std::fs::read(&path).unwrap();
        let summary_start = mcap::read::footer(&bytes).unwrap().summary_start as usize;
        std::fs::write(&path, &bytes[..summary_start - 13]).unwrap();
        assert!(mcap::Summary::read(&std::fs::read(&path).unwrap()).ok().flatten().is_none());
        assert!(!is_indexed(&path));
        let reports = Mutex::new(Vec::new());
        McapFile::open(&path).unwrap().index_with(Some(&|done, total| reports.lock().unwrap().push((done, total))), None).unwrap();
        assert_eq!(reports.lock().unwrap().last(), Some(&(summary_start as u64 - 13, summary_start as u64 - 13)));
        assert!(is_indexed(&path), "kept for the same file");
        reads_alike(&path, false);
        // cut mid-chunk: what's before it still reads
        std::fs::write(&path, &bytes[..summary_start / 2]).unwrap();
        assert!(!is_indexed(&path), "a changed file is walked again");
        reads_alike(&path, false);
    }

    #[test]
    fn messages_outside_chunks_are_read_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flat.mcap");
        write(&path, None, false);
        reads_alike(&path, false);
    }
}
