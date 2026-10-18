//! dimos memory2 SQLite recordings (`.db`): `_streams(name, config)` lists the streams; each stream is a table
//! `(id, ts, value, pose_x..pose_qw, tags)` plus `<name>_blob(id, data)` holding the encoded payload, `<name>_rtree`
//! a spatial index. Payload codecs: `lcm` (the raw LCM message), `lz4+lcm` (an LZ4 frame around it) and `jpeg` (an LCM
//! Image whose pixels are a JPEG: the message is stored as is).
//! Writing creates new streams the way dimos's SqliteStore does, so dimos reads them back.
use anyhow::{bail, Context, Result};
pub use rusqlite::Connection;
use rusqlite::{OpenFlags, OptionalExtension};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct DbStream {
    pub name: String,
    /// dimos type, e.g. `sensor_msgs.PointCloud2`
    pub kind: String,
    pub codec: String,
    pub count: u64,
}

#[derive(Debug, Clone)]
pub struct DbRow {
    pub id: i64,
    pub ts: f64,
    /// the stored observation pose (x, y, z, qx, qy, qz, qw), when the recorder set one
    pub pose: Option<[f64; 7]>,
    /// the decoded (decompressed) LCM payload
    pub payload: Vec<u8>,
    /// the row's tags, as JSON text
    pub tags: Option<String>,
}

fn has_tags(db: &Connection, name: &str) -> bool {
    db.prepare(&format!("SELECT * FROM {} LIMIT 0", quote(name))).is_ok_and(|statement| statement.column_names().contains(&"tags"))
}

pub fn open_read(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .with_context(|| format!("opening {}", path.display()))
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `dimos.msgs.sensor_msgs.PointCloud2.PointCloud2` → `sensor_msgs.PointCloud2`
pub fn dimos_type(module: &str) -> String {
    let parts: Vec<&str> = module.split('.').collect();
    match parts.as_slice() {
        ["dimos", "msgs", package, _, class] => format!("{package}.{class}"),
        _ => module.to_string(),
    }
}

pub fn streams(db: &Connection) -> Result<Vec<DbStream>> {
    let mut streams = stream_list(db)?;
    for stream in &mut streams {
        stream.count = count(db, &stream.name);
    }
    Ok(streams)
}

/// The streams from `_streams` alone, `count` 0: counting a stream's rows reads its whole table (seconds on a big
/// recording), and opening one doesn't need it.
pub fn stream_list(db: &Connection) -> Result<Vec<DbStream>> {
    let mut statement = db.prepare("SELECT name, config FROM _streams ORDER BY name").context("not a dimos recording (no _streams)")?;
    let rows: Vec<(String, String)> = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .map(|(name, config)| {
            let config: serde_json::Value = serde_json::from_str(&config).unwrap_or_default();
            let kind = config["payload_module"].as_str().map(dimos_type).unwrap_or_default();
            let codec = config["codec_id"].as_str().unwrap_or("lcm").to_string();
            DbStream { name, kind, codec, count: 0 }
        })
        .collect())
}

pub fn count(db: &Connection, name: &str) -> u64 {
    db.query_row(&format!("SELECT count(*) FROM {}", quote(name)), [], |row| row.get(0)).unwrap_or(0)
}

/// Whether a stream has a row (without counting them).
pub fn has_rows(db: &Connection, name: &str) -> bool {
    db.query_row(&format!("SELECT EXISTS (SELECT 1 FROM {})", quote(name)), [], |row| row.get(0)).unwrap_or(false)
}

pub fn decode_blob(codec: &str, data: &[u8]) -> Result<Vec<u8>> {
    match codec {
        "lcm" | "jpeg" => Ok(data.to_vec()),
        "lz4+lcm" => {
            let mut out = Vec::new();
            lz4_flex::frame::FrameDecoder::new(data).read_to_end(&mut out).context("lz4 frame")?;
            Ok(out)
        }
        other => bail!("unsupported codec {other}"),
    }
}

/// Calls `each` for every row of `stream` in timestamp order. `each` returns false to stop early. The rows are put in
/// order without their payloads and each payload read as it's reached (a sort that carried the blobs would read every
/// one before the first row: a reader wanting the first message would read the whole stream).
pub fn for_each(db: &Connection, stream: &DbStream, mut each: impl FnMut(DbRow) -> Result<bool>) -> Result<()> {
    let table = quote(&stream.name);
    let blob = quote(&format!("{}_blob", stream.name));
    let mut statement = db.prepare(&format!(
        "SELECT m.ts, m.pose_x, m.pose_y, m.pose_z, m.pose_qx, m.pose_qy, m.pose_qz, m.pose_qw, m.id{} \
         FROM {table} AS m WHERE EXISTS (SELECT 1 FROM {blob} AS b WHERE b.id = m.id) ORDER BY m.ts",
        if has_tags(db, &stream.name) { ", json(m.tags)" } else { ", NULL" }
    ))?;
    let mut payload = db.prepare(&format!("SELECT data FROM {blob} WHERE id = ?1"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let ts: f64 = row.get(0)?;
        let pose: Vec<Option<f64>> = (1..8).map(|i| row.get(i)).collect::<rusqlite::Result<_>>()?;
        let pose = if pose.iter().all(Option::is_some) { Some(std::array::from_fn(|i| pose[i].unwrap())) } else { None };
        let id: i64 = row.get(8)?;
        let tags: Option<String> = row.get(9).unwrap_or(None);
        let data: Vec<u8> = payload.query_row([id], |row| row.get(0))?;
        let payload = decode_blob(&stream.codec, &data)?;
        if !each(DbRow { id, ts, pose, payload, tags })? {
            break;
        }
    }
    Ok(())
}

/// What a new stream holds, for `_streams.config` (the module dimos imports to decode it).
pub fn payload_module(kind: &str) -> String {
    let (package, class) = kind.split_once('.').unwrap_or(("std_msgs", kind));
    format!("dimos.msgs.{package}.{class}.{class}")
}

/// One row to write: its time, the observation pose dimos indexes (x, y, z, qx, qy, qz, qw), its tags (a JSON object)
/// and the LCM payload.
#[derive(Debug, Clone, Default)]
pub struct NewRow {
    pub ts: f64,
    pub pose: Option<[f64; 7]>,
    pub tags: Option<serde_json::Value>,
    pub payload: Vec<u8>,
}

impl NewRow {
    pub fn new(ts: f64, payload: Vec<u8>) -> NewRow {
        NewRow { ts, payload, ..Default::default() }
    }
}

/// Replaces stream `name` (dropping any previous one of that name) with `rows` (ts, LCM payload) of `kind`.
pub fn write_stream(db: &mut Connection, name: &str, kind: &str, rows: &[(f64, Vec<u8>)]) -> Result<()> {
    let rows: Vec<NewRow> = rows.iter().map(|(ts, payload)| NewRow::new(*ts, payload.clone())).collect();
    write_rows(db, name, kind, &rows)
}

/// Drops stream `name` (its tables and its `_streams` row), if it's there.
pub fn delete_stream(db: &Connection, name: &str) -> Result<()> {
    for suffix in ["", "_blob", "_rtree", "_vec"] {
        db.execute_batch(&format!("DROP TABLE IF EXISTS {}", quote(&format!("{name}{suffix}"))))?;
    }
    if db.query_row("SELECT count(*) FROM sqlite_master WHERE name = '_streams'", [], |row| row.get::<_, i64>(0))? > 0 {
        db.execute("DELETE FROM _streams WHERE name = ?1", [name])?;
    }
    Ok(())
}

/// Appends rows to an existing stream (its codec must be plain `lcm`).
pub fn append_rows(db: &Connection, name: &str, rows: &[NewRow]) -> Result<()> {
    let table = quote(name);
    let has_tags = db.prepare(&format!("SELECT * FROM {table} LIMIT 0"))?.column_names().contains(&"tags");
    let mut meta = db.prepare(&format!(
        "INSERT INTO {table} (ts, pose_x, pose_y, pose_z, pose_qx, pose_qy, pose_qz, pose_qw{}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8{})",
        if has_tags { ", tags" } else { "" },
        if has_tags { ", jsonb(?9)" } else { "" }
    ))?;
    let mut blob = db.prepare(&format!("INSERT INTO {} (id, data) VALUES (?1, ?2)", quote(&format!("{name}_blob"))))?;
    let rtree_exists = db.query_row("SELECT count(*) FROM sqlite_master WHERE name = ?1", [format!("{name}_rtree")], |row| row.get::<_, i64>(0))? > 0;
    let mut rtree = if rtree_exists { Some(db.prepare(&format!("INSERT INTO {} VALUES (?1, ?2, ?2, ?3, ?3, ?4, ?4)", quote(&format!("{name}_rtree"))))?) } else { None };
    for row in rows {
        let p: [Option<f64>; 7] = match row.pose {
            Some(pose) => pose.map(Some),
            None => [None; 7],
        };
        let tags = row.tags.as_ref().map_or_else(|| "{}".to_string(), |t| t.to_string());
        if has_tags {
            meta.execute(rusqlite::params![row.ts, p[0], p[1], p[2], p[3], p[4], p[5], p[6], tags])?;
        } else {
            meta.execute(rusqlite::params![row.ts, p[0], p[1], p[2], p[3], p[4], p[5], p[6]])?;
        }
        let id = db.last_insert_rowid();
        blob.execute((id, &row.payload))?;
        if let (Some(rtree), Some(pose)) = (rtree.as_mut(), row.pose) {
            rtree.execute((id, pose[0], pose[1], pose[2]))?;
        }
    }
    Ok(())
}

/// Replaces stream `name` (dropping any previous one of that name) with `rows` of `kind`, poses and tags kept.
pub fn write_rows(db: &mut Connection, name: &str, kind: &str, rows: &[NewRow]) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS _streams (name TEXT PRIMARY KEY, config TEXT NOT NULL)")?;
    for suffix in ["", "_blob", "_rtree", "_vec"] {
        tx.execute_batch(&format!("DROP TABLE IF EXISTS {}", quote(&format!("{name}{suffix}"))))?;
    }
    let config = serde_json::json!({
        "payload_module": payload_module(kind),
        "codec_id": "lcm",
        "eager_blobs": false,
        "page_size": 256,
        "blob_store": { "class": "dimos.memory2.blobstore.sqlite.SqliteBlobStore", "config": { "path": null } },
        "vector_store": { "class": "dimos.memory2.vectorstore.sqlite.SqliteVectorStore", "config": { "path": null } },
        "notifier": { "class": "dimos.memory2.notifier.subject.SubjectNotifier", "config": {} },
    });
    tx.execute("INSERT OR REPLACE INTO _streams (name, config) VALUES (?1, ?2)", (name, config.to_string()))?;
    let table = quote(name);
    tx.execute_batch(&format!(
        "CREATE TABLE {table} (id INTEGER PRIMARY KEY AUTOINCREMENT, ts REAL NOT NULL, value NUMERIC, \
         pose_x REAL, pose_y REAL, pose_z REAL, pose_qx REAL, pose_qy REAL, pose_qz REAL, pose_qw REAL, \
         tags BLOB DEFAULT (jsonb('{{}}')));
         CREATE TABLE {} (id INTEGER PRIMARY KEY, data BLOB NOT NULL);
         CREATE VIRTUAL TABLE {} USING rtree(id, x_min, x_max, y_min, y_max, z_min, z_max);",
        quote(&format!("{name}_blob")),
        quote(&format!("{name}_rtree")),
    ))?;
    append_rows(&tx, name, rows)?;
    tx.commit()?;
    Ok(())
}

/// The newest payload of `name`, if the stream exists.
pub fn latest(db: &Connection, name: &str) -> Result<Option<(f64, Vec<u8>)>> {
    let exists: Option<String> = db.query_row("SELECT config FROM _streams WHERE name = ?1", [name], |row| row.get(0)).optional()?;
    let Some(config) = exists else { return Ok(None) };
    let codec = serde_json::from_str::<serde_json::Value>(&config).ok().and_then(|c| c["codec_id"].as_str().map(String::from)).unwrap_or("lcm".into());
    let row: Option<(f64, Vec<u8>)> = db
        .query_row(
            &format!("SELECT m.ts, b.data FROM {} AS m JOIN {} AS b ON b.id = m.id ORDER BY m.ts DESC LIMIT 1", quote(name), quote(&format!("{name}_blob"))),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(ts, data)| Ok((ts, decode_blob(&codec, &data)?))).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_streams_dimos_can_list_and_reads_them_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.db");
        let mut db = Connection::open(&path).unwrap();
        write_stream(&mut db, "map_builder_annotations", "std_msgs.String", &[(1.0, b"a".to_vec()), (2.0, b"b".to_vec())]).unwrap();
        // a rewrite replaces, never duplicates
        write_stream(&mut db, "map_builder_annotations", "std_msgs.String", &[(3.0, b"c".to_vec())]).unwrap();
        let listed = streams(&db).unwrap();
        assert_eq!(listed, vec![DbStream { name: "map_builder_annotations".into(), kind: "std_msgs.String".into(), codec: "lcm".into(), count: 1 }]);
        assert_eq!(latest(&db, "map_builder_annotations").unwrap(), Some((3.0, b"c".to_vec())));
        assert_eq!(latest(&db, "nope").unwrap(), None);
        let mut seen = Vec::new();
        for_each(&db, &listed[0], |row| {
            seen.push((row.ts, row.payload, row.pose));
            Ok(true)
        })
        .unwrap();
        assert_eq!(seen, vec![(3.0, b"c".to_vec(), None)]);
        // poses and tags survive, and appending adds to the stream
        let row = NewRow { ts: 4.0, pose: Some([1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0]), tags: Some(serde_json::json!({ "marker_id": 7 })), payload: b"d".to_vec() };
        write_rows(&mut db, "tags", "geometry_msgs.PoseStamped", &[row.clone()]).unwrap();
        append_rows(&db, "tags", &[NewRow { ts: 5.0, ..row }]).unwrap();
        let listed = streams(&db).unwrap();
        let tags = listed.iter().find(|s| s.name == "tags").unwrap();
        let mut rows = Vec::new();
        for_each(&db, tags, |row| {
            rows.push(row);
            Ok(true)
        })
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].pose.unwrap()[1], 2.0);
        assert_eq!(serde_json::from_str::<serde_json::Value>(rows[0].tags.as_deref().unwrap()).unwrap()["marker_id"], 7);
        delete_stream(&db, "tags").unwrap();
        assert!(streams(&db).unwrap().iter().all(|s| s.name != "tags"));
    }

    #[test]
    fn lz4_frames_decode() {
        let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
        std::io::Write::write_all(&mut encoder, b"hello lcm").unwrap();
        let framed = encoder.finish().unwrap();
        assert_eq!(decode_blob("lz4+lcm", &framed).unwrap(), b"hello lcm");
        assert!(decode_blob("pickle", b"").is_err());
    }
}
