//! Writing one tf edge into a recording, in place, the way dtk's `db_tf_*` tools edit tf: in a `.db` every tf
//! message that carries the edge loses it (a message left empty is deleted), and the new samples go onto the `tf`
//! stream one transform per message at their own timestamps, each row tagged with its `child_frame`. An `.mcap` is
//! only ever appended to: the samples go on a new channel of the tf topic that names the edge (`EDGE_KEY`), and
//! readers here (`Recording::tf_edges_logged`) take that edge from the newest such channel only.
use crate::db::{self, NewRow};
use crate::lcm::{self, Header, TfEdge};
use crate::mcap_io::{self, GeneratedChannel, EDGE_KEY, GENERATION_KEY, LCM_TYPE_KEY};
use crate::{Format, Kind, Recording};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// One sample of an edge: time, translation, rotation (x, y, z, w).
pub type Sample = (f64, [f64; 3], [f64; 4]);

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeWrite {
    /// transforms of that edge the recording had before (dropped in a .db, overridden in an .mcap)
    pub replaced: usize,
    /// messages left empty and deleted (.db)
    pub deleted_messages: usize,
    pub written: usize,
    /// the stream / topic the samples went on
    pub stream: String,
}

pub fn edge_key(parent: &str, child: &str) -> String {
    format!("{parent}>{child}")
}

fn tf_message(parent: &str, child: &str, sample: &Sample) -> TfEdge {
    TfEdge { header: Header::at(sample.0, parent), child: child.into(), translation: sample.1, rotation: sample.2 }
}

fn same(name: &str, other: &str) -> bool {
    name.trim_start_matches('/') == other.trim_start_matches('/')
}

/// Replaces edge parent → child in the recording at `path` with `samples`.
pub fn replace_edge(path: &Path, parent: &str, child: &str, samples: &[Sample], generation: f64) -> Result<EdgeWrite> {
    if samples.is_empty() {
        bail!("no samples to write for {parent} -> {child}");
    }
    match crate::format_of(path)? {
        Format::Db => replace_in_db(path, parent, child, samples),
        Format::Mcap => replace_in_mcap(path, parent, child, samples, generation),
    }
}

fn recompress(codec: &str, payload: Vec<u8>) -> Result<Vec<u8>> {
    match codec {
        "lcm" => Ok(payload),
        "lz4+lcm" => {
            let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
            encoder.write_all(&payload)?;
            Ok(encoder.finish()?)
        }
        other => bail!("a tf stream stored as {other} can't be rewritten"),
    }
}

fn replace_in_db(path: &Path, parent: &str, child: &str, samples: &[Sample]) -> Result<EdgeWrite> {
    let mut connection = db::Connection::open(path).with_context(|| format!("opening {} to write", path.display()))?;
    let streams = db::streams(&connection)?;
    let tf_streams: Vec<&db::DbStream> = streams.iter().filter(|s| Kind::of(&s.kind) == Kind::Tf).collect();
    let mut report = EdgeWrite::default();
    let tx = connection.transaction()?;
    for stream in &tf_streams {
        let mut changes: Vec<(i64, Option<Vec<u8>>)> = Vec::new();
        db::for_each(&tx, stream, |row| {
            let Ok(edges) = lcm::decode_tf(&row.payload) else { return Ok(true) };
            let kept: Vec<TfEdge> = edges.iter().filter(|e| !(same(&e.header.frame_id, parent) && same(&e.child, child))).cloned().collect();
            if kept.len() != edges.len() {
                report.replaced += edges.len() - kept.len();
                changes.push((row.id, (!kept.is_empty()).then(|| lcm::encode_tf(&kept))));
            }
            Ok(true)
        })?;
        let table = format!("\"{}\"", stream.name.replace('"', "\"\""));
        let blob = format!("\"{}_blob\"", stream.name.replace('"', "\"\""));
        for (id, payload) in changes {
            match payload {
                Some(payload) => {
                    tx.execute(&format!("UPDATE {blob} SET data = ?1 WHERE id = ?2"), (recompress(&stream.codec, payload)?, id))?;
                }
                None => {
                    tx.execute(&format!("DELETE FROM {blob} WHERE id = ?1"), [id])?;
                    tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [id])?;
                    report.deleted_messages += 1;
                }
            }
        }
    }
    // the dynamic tf stream: `tf`, else the first one that isn't static, else a new `tf`
    let target = tf_streams
        .iter()
        .find(|s| s.name == "tf")
        .or_else(|| tf_streams.iter().find(|s| !s.name.ends_with("_static")))
        .filter(|s| s.codec == "lcm")
        .map(|s| s.name.clone());
    let rows: Vec<NewRow> = samples
        .iter()
        .map(|sample| NewRow { ts: sample.0, pose: None, tags: Some(serde_json::json!({ "child_frame": child })), payload: lcm::encode_tf(&[tf_message(parent, child, sample)]) })
        .collect();
    match &target {
        Some(name) => db::append_rows(&tx, name, &rows)?,
        None => {
            let name = if tf_streams.iter().any(|s| s.name == "tf") { "tf_map_builder" } else { "tf" };
            tx.commit()?;
            db::write_rows(&mut connection, name, lcm::TF_TYPE, &rows)?;
            report.written = rows.len();
            report.stream = name.into();
            return Ok(report);
        }
    }
    tx.commit()?;
    report.written = rows.len();
    report.stream = target.unwrap_or_default();
    Ok(report)
}

fn replace_in_mcap(path: &Path, parent: &str, child: &str, samples: &[Sample], generation: f64) -> Result<EdgeWrite> {
    let file = mcap_io::McapFile::open(path)?;
    let infos = file.channel_infos()?;
    let tf_channels: Vec<_> = infos.iter().filter(|c| Kind::of(&c.kind) == Kind::Tf).collect();
    // the recording's dynamic tf topic and its encoding (cdr: the schema is reused)
    let base = tf_channels.iter().find(|c| !c.topic.trim_start_matches('/').ends_with("static") && !c.metadata.contains_key(EDGE_KEY)).or(tf_channels.first()).copied();
    let topic = base.map_or("/tf".to_string(), |c| c.topic.clone());
    let mut replaced = 0;
    let recording = Recording::open(path)?;
    for (_, edge) in recording.tf_edges_logged()? {
        if same(&edge.header.frame_id, parent) && same(&edge.child, child) {
            replaced += 1;
        }
    }
    drop(recording);
    let schema = base.filter(|c| c.encoding == "cdr").and_then(|c| {
        let summary = mcap::Summary::read(file.bytes()).ok().flatten()?;
        let schema = summary.channels.get(&c.id)?.schema.clone()?;
        Some((schema.name.clone(), schema.encoding.clone(), schema.data.to_vec()))
    });
    drop(file);
    let cdr = schema.is_some();
    let messages: Vec<(f64, Vec<u8>)> = samples
        .iter()
        .map(|sample| {
            let edge = tf_message(parent, child, sample);
            (sample.0, if cdr { crate::cdr::encode_tf(&[edge]) } else { lcm::encode_tf(&[edge]) })
        })
        .collect();
    let mut metadata = BTreeMap::from([(GENERATION_KEY.to_string(), format!("{generation:.6}")), (EDGE_KEY.to_string(), edge_key(parent, child))]);
    if !cdr {
        metadata.insert(LCM_TYPE_KEY.to_string(), lcm::TF_TYPE.to_string());
    }
    let written = messages.len();
    mcap_io::append_generated(path, &[GeneratedChannel { topic: topic.clone(), encoding: if cdr { "cdr".into() } else { "lcm".into() }, schema, metadata, messages }])?;
    Ok(EdgeWrite { replaced, deleted_messages: 0, written, stream: topic })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(ts: f64, parent: &str, child: &str, x: f64) -> TfEdge {
        TfEdge { header: Header::at(ts, parent), child: child.into(), translation: [x, 0.0, 0.0], rotation: [0.0, 0.0, 0.0, 1.0] }
    }

    fn edges_of(path: &Path) -> Vec<(String, String, f64)> {
        let mut found: Vec<_> = Recording::open(path).unwrap().tf_edges().unwrap().into_iter().map(|e| (e.header.frame_id, e.child, e.translation[0])).collect();
        found.sort_by(|a, b| a.partial_cmp(b).unwrap());
        found
    }

    #[test]
    fn replaces_an_edge_in_a_db_and_in_an_mcap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.db");
        let mut connection = db::Connection::open(&path).unwrap();
        let messages: Vec<(f64, Vec<u8>)> = (0..3).map(|i| (i as f64, lcm::encode_tf(&[edge(i as f64, "map", "odom", 9.0), edge(i as f64, "odom", "lidar", i as f64)]))).collect();
        db::write_stream(&mut connection, "tf", lcm::TF_TYPE, &messages).unwrap();
        db::write_stream(&mut connection, "tf_static", lcm::TF_TYPE, &[(0.0, lcm::encode_tf(&[edge(0.0, "map", "odom", 9.0)]))]).unwrap();
        drop(connection);
        let written = replace_edge(&path, "map", "odom", &[(0.0, [1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]), (2.0, [2.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0])], 1.0).unwrap();
        assert_eq!((written.replaced, written.deleted_messages, written.written, written.stream.as_str()), (4, 1, 2, "tf"));
        let found = edges_of(&path);
        assert_eq!(found.iter().filter(|e| e.0 == "map").map(|e| e.2).collect::<Vec<_>>(), vec![1.0, 2.0]);
        assert_eq!(found.iter().filter(|e| e.0 == "odom").count(), 3, "the other edge is untouched");

        let mcap = dir.path().join("r.mcap");
        let fixture: Vec<(&str, &str, &str, f64, Vec<u8>)> = messages.iter().map(|(ts, payload)| ("/tf", "lcm", lcm::TF_TYPE, *ts, payload.clone())).collect();
        mcap_io::write_fixture(&mcap, &fixture);
        replace_edge(&mcap, "map", "odom", &[(0.0, [1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0])], 1.0).unwrap();
        replace_edge(&mcap, "map", "odom", &[(0.0, [5.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0])], 2.0).unwrap();
        let found = edges_of(&mcap);
        assert_eq!(found.iter().filter(|e| e.0 == "map").map(|e| e.2).collect::<Vec<_>>(), vec![5.0], "only the newest override is read");
        assert_eq!(found.iter().filter(|e| e.0 == "odom").count(), 3);
    }
}
