//! One working session per recording, kept server-side and written to disk on every change (the "autosave"), so a
//! page refresh, a closed tab or a server restart loses nothing. Separate from "Save into recording", which writes
//! the result into the .db/.mcap. `revision` counts changes, `saved_revision` the last one written to the recording.
//!
//! The map is the built voxel cloud (`base`, in the recording's world frame) plus a `removed` mask; the map frame is
//! `transform` × world (rotate / level / move). Every edit is undoable: an entry records the mask flips it made and
//! the annotation / transform / floor-plan state before and after.
use anyhow::{bail, Context, Result};
use mapping::floorplan::FloorPlan;
use mapping::tf::Iso;
use mapping::voxels::Box3;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SESSION_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BoxAnnotation {
    pub id: String,
    pub label: String,
    #[serde(rename = "box")]
    pub region: Box3Json,
    #[serde(default)]
    pub color: Option<String>,
    /// "user" or "agent"
    #[serde(default)]
    pub source: String,
}

/// Box3 with defaults so partial JSON (from the agent) works
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Box3Json {
    pub center: [f32; 3],
    pub size: [f32; 3],
    #[serde(default)]
    pub yaw: f32,
}

impl From<Box3Json> for Box3 {
    fn from(b: Box3Json) -> Box3 {
        Box3 { center: b.center, size: b.size, yaw: b.yaw }
    }
}

impl From<Box3> for Box3Json {
    fn from(b: Box3) -> Box3Json {
        Box3Json { center: b.center, size: b.size, yaw: b.yaw }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaneAnnotation {
    pub id: String,
    pub label: String,
    pub center: [f32; 3],
    /// unit normal
    pub normal: [f32; 3],
    /// width, height of the drawn rectangle
    pub size: [f32; 2],
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PointAnnotation {
    pub id: String,
    pub label: String,
    pub position: [f32; 3],
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Floor {
    pub index: usize,
    pub name: String,
    /// the floor's height in the map frame
    pub z: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlanPoint {
    pub id: String,
    pub floor: usize,
    pub name: String,
    pub position: [f32; 2],
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Area {
    pub id: String,
    pub floor: usize,
    pub name: String,
    /// "no-go", "zone" (a named room/region), "slow" — free text is allowed; "no-go" is what navigation avoids
    pub kind: String,
    pub polygon: Vec<[f32; 2]>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Annotations {
    pub boxes: Vec<BoxAnnotation>,
    pub planes: Vec<PlaneAnnotation>,
    pub points: Vec<PointAnnotation>,
    pub floors: Vec<Floor>,
    pub plan_points: Vec<PlanPoint>,
    pub areas: Vec<Area>,
}

/// map ← recording world, as translation + quaternion (x, y, z, w)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Transform {
    pub translation: [f64; 3],
    pub rotation: [f64; 4],
}

impl Default for Transform {
    fn default() -> Self {
        Transform { translation: [0.0; 3], rotation: [0.0, 0.0, 0.0, 1.0] }
    }
}

impl Transform {
    pub fn iso(&self) -> Iso {
        mapping::tf::iso(self.translation, self.rotation)
    }

    pub fn from_iso(value: &Iso) -> Transform {
        let (translation, rotation) = mapping::tf::parts(value);
        Transform { translation, rotation }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoEntry {
    pub label: String,
    /// indices whose `removed` flag this edit flipped
    #[serde(default)]
    pub flipped: Vec<u32>,
    /// voxels this edit added to the map (appended to `points`; undo hides them again)
    #[serde(default)]
    pub added: Vec<u32>,
    #[serde(default)]
    pub annotations: Option<(Annotations, Annotations)>,
    #[serde(default)]
    pub transform: Option<(Transform, Transform)>,
    #[serde(default)]
    pub plans: Option<(Vec<FloorPlan>, Vec<FloorPlan>)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildSummary {
    pub voxel_size: f32,
    pub world_frame: String,
    pub cloud_stream: String,
    pub scans_used: usize,
    pub scans_skipped: usize,
    pub loops: usize,
    pub notes: Vec<String>,
    pub seconds: f64,
}

/// Everything about one recording's editing state except the big arrays (those live in `map.bin`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Session {
    pub version: u32,
    pub id: String,
    pub recording_id: String,
    pub recording_path: String,
    /// false for recordings in one of Desktop's read-only folders: saving writes a copy into the recordings folder
    #[serde(default = "yes")]
    pub writable: bool,
    pub name: String,
    /// "raw" (nothing built yet), "map" (a map to edit)
    pub stage: String,
    pub build: Option<BuildSummary>,
    pub build_options: Option<mapping::build::BuildOptions>,
    pub transform: Transform,
    pub annotations: Annotations,
    pub plans: Vec<FloorPlan>,
    /// whatever the page wants back after a refresh: camera, open panels, tool, selected item
    pub view: serde_json::Value,
    pub revision: u64,
    pub saved_revision: u64,
    pub saved_at: Option<f64>,
    pub undo: Vec<UndoEntry>,
    pub redo: Vec<UndoEntry>,
    /// a short log of what happened, newest last (shown in the UI, and to the agent)
    pub history: Vec<String>,
}

/// The built map: arrays too big for JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MapData {
    pub voxel_size: f32,
    pub points: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub removed: Vec<bool>,
    pub raw_path: Vec<[f32; 3]>,
    pub corrected_path: Vec<[f32; 3]>,
    pub loops: Vec<([f32; 3], [f32; 3])>,
}

fn yes() -> bool {
    true
}

pub fn session_id(recording_path: &Path) -> String {
    let text = recording_path.display().to_string();
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let stem: String = recording_path
        .file_stem()
        .map(|s| s.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect())
        .unwrap_or_default();
    format!("{stem}-{:08x}", hash as u32)
}

pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        let _ = std::fs::create_dir_all(dir.join("sessions"));
        Store { dir }
    }

    fn session_dir(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty() || id.contains('/') || id.contains("..") {
            bail!("bad session id {id}");
        }
        Ok(self.dir.join("sessions").join(id))
    }

    pub fn load(&self, id: &str) -> Result<Option<Session>> {
        let path = self.session_dir(id)?.join("session.json");
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Written to a temp file and renamed, so a crash mid-write never leaves half a session.
    pub fn save(&self, session: &Session) -> Result<()> {
        let dir = self.session_dir(&session.id)?;
        std::fs::create_dir_all(&dir)?;
        let temp = dir.join("session.json.tmp");
        std::fs::write(&temp, serde_json::to_vec(session)?)?;
        std::fs::rename(&temp, dir.join("session.json"))?;
        Ok(())
    }

    pub fn load_map(&self, id: &str) -> Result<Option<MapData>> {
        let path = self.session_dir(id)?.join("map.bin");
        match std::fs::read(&path) {
            Ok(bytes) => {
                let mut map = decode_map(&bytes)?;
                // edits since the map was written live in removed.bin
                if let Ok(mask) = std::fs::read(self.session_dir(id)?.join("removed.bin")) {
                    if mask.len() == map.points.len() {
                        map.removed = mask.iter().map(|b| *b != 0).collect();
                    }
                }
                Ok(Some(map))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save_map(&self, id: &str, map: &MapData) -> Result<()> {
        let dir = self.session_dir(id)?;
        std::fs::create_dir_all(&dir)?;
        let temp = dir.join("map.bin.tmp");
        std::fs::write(&temp, encode_map(map))?;
        std::fs::rename(&temp, dir.join("map.bin"))?;
        self.save_removed(id, &map.removed)
    }

    /// Just the deletion mask: what every edit changes, small enough to write on each one.
    pub fn save_removed(&self, id: &str, removed: &[bool]) -> Result<()> {
        let dir = self.session_dir(id)?;
        let temp = dir.join("removed.bin.tmp");
        std::fs::write(&temp, removed.iter().map(|r| *r as u8).collect::<Vec<u8>>())?;
        std::fs::rename(&temp, dir.join("removed.bin"))?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.session_dir(id)?;
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    /// which session the page had open, so a fresh page goes straight back to it
    pub fn last_open(&self) -> Option<String> {
        std::fs::read_to_string(self.dir.join("open.txt")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    pub fn clear_last_open(&self, id: &str) {
        if self.last_open().as_deref() == Some(id) {
            let _ = std::fs::remove_file(self.dir.join("open.txt"));
        }
    }

    pub fn set_last_open(&self, id: &str) {
        let _ = std::fs::write(self.dir.join("open.txt"), id);
    }
}

/// map.bin: a little-endian header (magic, version, counts) then the arrays; ~7x smaller and much faster than JSON.
const MAGIC: &[u8; 4] = b"MBM1";

fn encode_map(map: &MapData) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + map.points.len() * 25);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&map.voxel_size.to_le_bytes());
    for count in [map.points.len(), map.raw_path.len(), map.corrected_path.len(), map.loops.len()] {
        out.extend_from_slice(&(count as u32).to_le_bytes());
    }
    let floats = |values: &[[f32; 3]], out: &mut Vec<u8>| {
        for p in values {
            for v in p {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    };
    floats(&map.points, &mut out);
    let normals: Vec<[f32; 3]> = if map.normals.len() == map.points.len() { map.normals.clone() } else { vec![[0.0; 3]; map.points.len()] };
    floats(&normals, &mut out);
    out.extend(map.removed.iter().map(|r| *r as u8));
    out.extend(std::iter::repeat_n(0u8, map.points.len().saturating_sub(map.removed.len())));
    floats(&map.raw_path, &mut out);
    floats(&map.corrected_path, &mut out);
    let loop_points: Vec<[f32; 3]> = map.loops.iter().flat_map(|(a, b)| [*a, *b]).collect();
    floats(&loop_points, &mut out);
    out
}

fn decode_map(bytes: &[u8]) -> Result<MapData> {
    if bytes.len() < 24 || &bytes[..4] != MAGIC {
        bail!("not a map file");
    }
    let f32_at = |offset: usize| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let u32_at = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    let voxel_size = f32_at(4);
    let (n, raw, corrected, loops) = (u32_at(8), u32_at(12), u32_at(16), u32_at(20));
    let need = 24 + n * 24 + n + (raw + corrected + loops * 2) * 12;
    if bytes.len() < need {
        bail!("truncated map file");
    }
    let mut offset = 24;
    let mut take = |count: usize| -> Vec<[f32; 3]> {
        let values = (0..count).map(|i| std::array::from_fn(|k| f32_at(offset + i * 12 + k * 4))).collect();
        offset += count * 12;
        values
    };
    let points = take(n);
    let normals = take(n);
    let removed_start = offset;
    let removed: Vec<bool> = bytes[removed_start..removed_start + n].iter().map(|b| *b != 0).collect();
    offset = removed_start + n;
    let mut take = |count: usize| -> Vec<[f32; 3]> {
        let values = (0..count).map(|i| std::array::from_fn(|k| f32_at(offset + i * 12 + k * 4))).collect();
        offset += count * 12;
        values
    };
    let raw_path = take(raw);
    let corrected_path = take(corrected);
    let loop_points = take(loops * 2);
    let loops = loop_points.chunks_exact(2).map(|pair| (pair[0], pair[1])).collect();
    Ok(MapData { voxel_size, points, normals, removed, raw_path, corrected_path, loops })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_and_maps_round_trip_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf());
        let id = session_id(Path::new("/x/drive 1.mcap"));
        assert!(id.starts_with("drive_1-"));
        assert!(store.load(&id).unwrap().is_none());
        let mut session = Session { id: id.clone(), stage: "map".into(), revision: 3, ..Default::default() };
        session.annotations.boxes.push(BoxAnnotation { id: "b1".into(), label: "chair".into(), ..Default::default() });
        store.save(&session).unwrap();
        let back = store.load(&id).unwrap().unwrap();
        assert_eq!(back.annotations, session.annotations);
        assert_eq!(back.revision, 3);
        let map = MapData {
            voxel_size: 0.05,
            points: vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]],
            normals: vec![[0.0, 0.0, 1.0], [1.0, 0.0, 0.0]],
            removed: vec![false, true],
            raw_path: vec![[0.0; 3]],
            corrected_path: vec![[0.1; 3]],
            loops: vec![([0.0; 3], [1.0; 3])],
        };
        store.save_map(&id, &map).unwrap();
        let loaded = store.load_map(&id).unwrap().unwrap();
        assert_eq!((loaded.points, loaded.normals, loaded.removed, loaded.loops), (map.points, map.normals, map.removed, map.loops));
        assert!(store.load(&"../x".to_string()).is_err());
    }
}
