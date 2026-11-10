//! What the generation steps have done to a recording, kept in the recording itself (the map's `pipeline` stream,
//! one JSON string; a `.db` replaces it, an `.mcap` appends a newer one), so a later step can tell from the file
//! whether an earlier one has to run first, and with what it ran. It is per map name (crate::names), so two maps of
//! the same space each remember their own generation.
use crate::correct::{CorrectOptions, Plan, SolveReport};
use crate::tags::TagOptions;
use anyhow::Result;
use dimos_recording::{db, lcm, mcap_io, Format, Recording};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// where the state lived before map names, still read (never written) so an older recording opens as it was
pub const LEGACY_STATE_STREAM: &str = "map/pipeline";
/// where the map's cloud lived before map names, still read (never written) by everything that takes a recording's
/// premap; dimos falls back to it for a recording with no named map
pub const GLOBAL_MAP_STREAM: &str = "map_cloud";

/// The map's generation-state stream: `map/<name>/pipeline` / `map_<name>_pipeline`.
pub fn state_stream(format: Format, map: &str) -> String {
    crate::names::stream(format, map, "pipeline")
}

/// The map's cloud stream: `map/<name>/cloud` / `map_<name>_cloud`.
pub fn cloud_stream(format: Format, map: &str) -> String {
    crate::names::stream(format, map, "cloud")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TagsRecord {
    pub at: f64,
    pub options: TagOptions,
    pub camera_stream: String,
    pub camera_frame: String,
    pub images: usize,
    pub raw: usize,
    pub grouped: usize,
    pub markers: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TfRecord {
    pub at: f64,
    pub options: CorrectOptions,
    pub plan: Plan,
    pub report: SolveReport,
    pub samples: usize,
    /// when the tags it used were made (tags re-run since = stale)
    pub tags_at: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GlobalRecord {
    pub at: f64,
    pub points: usize,
    pub voxel_size: f32,
    pub world_frame: String,
    pub cloud_stream: String,
    /// when the corrected tf it used was made (None = the recording's own tf)
    pub tf_at: Option<f64>,
    pub lcm_path: String,
    pub rrd_path: Option<String>,
}

/// Point-LIO's odometry (crate::pointlio), for a recording that had none.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OdometryRecord {
    pub at: f64,
    pub lidar: String,
    pub imu: String,
    /// the tf edge written: parent (odom) → child (the root above the lidar)
    pub parent: String,
    pub child: String,
    pub poses: usize,
    /// deskewed scans on crate::pointlio::CLOUD_TOPIC (0 in a .db)
    pub clouds: usize,
    pub path_length: f64,
    pub start_to_end: f64,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PipelineState {
    pub odometry: Option<OdometryRecord>,
    pub tags: Option<TagsRecord>,
    pub tf: Option<TfRecord>,
    pub global_map: Option<GlobalRecord>,
}

/// The state of map `map`'s generation: its own stream, or (for a recording written before map names) the flat
/// `map/pipeline` it used to live on.
pub fn read(recording: &Recording, map: &str) -> Result<PipelineState> {
    let own = recording.latest(&state_stream(recording.format(), map))?;
    let Some((_, payload)) = own.or(recording.latest(LEGACY_STATE_STREAM)?) else { return Ok(PipelineState::default()) };
    Ok(lcm::decode_string(&payload).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default())
}

pub fn write(path: &Path, map: &str, state: &PipelineState, ts: f64, generation: f64) -> Result<()> {
    let payload = lcm::encode_string(&serde_json::to_string(state)?);
    let format = dimos_recording::format_of(path)?;
    let stream = state_stream(format, map);
    match format {
        Format::Db => db::write_stream(&mut db::Connection::open(path)?, &stream, lcm::STRING_TYPE, &[(ts, payload)]),
        Format::Mcap => mcap_io::append_generated(path, &[mcap_io::GeneratedChannel::lcm(&stream, lcm::STRING_TYPE, generation, vec![(ts, payload)])]),
    }
}

/// Reads the state, changes it, writes it back.
pub fn update(path: &Path, map: &str, change: impl FnOnce(&mut PipelineState)) -> Result<PipelineState> {
    let recording = Recording::open(path)?;
    let mut state = read(&recording, map)?;
    let ts = recording.time_range()?.map_or(0.0, |r| r.0);
    drop(recording);
    change(&mut state);
    write(path, map, &state, ts, crate::pipeline::now())?;
    Ok(state)
}

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_round_trips_in_a_db_and_an_mcap() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["r.db", "r.mcap"] {
            let path = dir.path().join(name);
            if name.ends_with(".db") {
                db::write_stream(&mut db::Connection::open(&path).unwrap(), "x", lcm::STRING_TYPE, &[(5.0, lcm::encode_string("x"))]).unwrap();
            } else {
                mcap_io::write_fixture(&path, &[("/x", "lcm", lcm::STRING_TYPE, 5.0, lcm::encode_string("x"))]);
            }
            assert!(read(&Recording::open(&path).unwrap(), "v1").unwrap().tags.is_none());
            update(&path, "v1", |state| state.tags = Some(TagsRecord { raw: 3, ..Default::default() })).unwrap();
            update(&path, "v1", |state| state.tf = Some(TfRecord { samples: 9, ..Default::default() })).unwrap();
            let state = read(&Recording::open(&path).unwrap(), "v1").unwrap();
            assert_eq!((state.tags.unwrap().raw, state.tf.unwrap().samples), (3, 9), "{name}");
            // it went under the map's name, not the flat stream older versions used
            let recording = Recording::open(&path).unwrap();
            let streams: Vec<String> = recording.streams().unwrap().into_iter().map(|s| s.name).collect();
            assert!(streams.contains(&state_stream(recording.format(), "v1")), "{name}: {streams:?}");
            assert!(!streams.iter().any(|s| s == LEGACY_STATE_STREAM), "{name}: {streams:?}");
            // another map on the same recording keeps its own state
            assert!(read(&recording, "v2").unwrap().tags.is_none(), "{name}");
        }
    }

    /// a recording written before map names: its flat `map/pipeline` is what the map's state reads
    #[test]
    fn an_older_recordings_flat_state_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.mcap");
        let state = PipelineState { tags: Some(TagsRecord { raw: 7, ..Default::default() }), ..Default::default() };
        let payload = lcm::encode_string(&serde_json::to_string(&state).unwrap());
        mcap_io::write_fixture(&path, &[(LEGACY_STATE_STREAM, "lcm", lcm::STRING_TYPE, 5.0, payload)]);
        assert_eq!(read(&Recording::open(&path).unwrap(), "v1").unwrap().tags.unwrap().raw, 7);
    }
}
