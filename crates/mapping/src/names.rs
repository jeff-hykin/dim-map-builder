//! Map names: a recording can hold several maps of the same space, so each one is saved under a name of its own and
//! everything the Map Editor writes for it goes under that name (`map/<name>/voxels`, `map/<name>/cloud`, ...). The
//! name is the map's identity downstream: dimos's relocalization finds a recording's maps by their `cloud` stream,
//! and the map's tf frame is `map/<name>`.
//!
//! An `.mcap` topic may hold slashes, so there the streams are `map/<name>/<leaf>`. A `.db` stream becomes a SQLite
//! table name, so there they are `map_<name>_<leaf>` — the same name, spelled the way the format allows (dimos reads
//! an .mcap topic with its slashes folded to underscores, so a reader sees one stream either way).
use anyhow::{bail, Result};
use dimos_recording::{Format, Recording};

/// The default name of a recording's first map; later ones count up from it (`v2`, `v3`, ...).
pub const DEFAULT_STEM: &str = "v";
/// `DEFAULT_STEM` + 1: a recording's first map, and what a session from before map names falls back to.
pub const FIRST: &str = "v1";

/// The leaves a named map's streams end in. Floor plans are `floor_<n>` on top of these.
pub const LEAVES: [&str; 8] = ["cloud", "voxels", "path", "annotations", "views", "slice", "pipeline", "named_locations"];

/// What saves before map names called the prefix of them all (`legacy` in crate::persist terms): not a map name.
const LEGACY: [&str; 2] = ["map_builder_", "map/"];

pub const MAX: usize = 64;

/// Whether `name` may be a map name. It becomes a tf frame (`map/<name>`) and part of a stream name in both formats
/// (a SQLite table name in a `.db`), so it has to be a plain identifier: a letter, then letters, digits and `_`.
/// Nothing is sanitised — a name that doesn't fit is refused, never mangled into one that does.
pub fn is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `is_valid`, with the reason it isn't.
pub fn check(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("a map needs a name");
    }
    if name.len() > MAX {
        bail!("the map name {name:?} is longer than {MAX} characters");
    }
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        bail!("the map name {name:?} has to start with a letter: it becomes a tf frame (map/{name}) and a stream name");
    }
    if !is_valid(name) {
        let bad: String = name.chars().filter(|c| !c.is_ascii_alphanumeric() && *c != '_').collect();
        bail!("the map name {name:?} can only hold letters, digits and underscores (not {bad:?}): it becomes a tf frame (map/{name}) and a stream name");
    }
    Ok(())
}

/// The map's tf frame, whatever the recording's format: `map/<name>`.
pub fn frame(name: &str) -> String {
    format!("map/{name}")
}

/// The stream a named map's `leaf` lives on: `map/<name>/<leaf>` in an `.mcap`, `map_<name>_<leaf>` in a `.db`.
pub fn stream(format: Format, name: &str, leaf: &str) -> String {
    match format {
        Format::Mcap => format!("map/{name}/{leaf}"),
        Format::Db => format!("map_{name}_{leaf}"),
    }
}

/// The map name an underscored (`.db`) stream belongs to, if it is one: `map_<name>_<leaf>`.
fn underscored_name_of(stream: &str) -> Option<String> {
    let stream = stream.trim_start_matches('/');
    if LEGACY.iter().any(|prefix| stream.starts_with(prefix)) {
        return None;
    }
    let rest = stream.strip_prefix("map_")?;
    let name = LEAVES.iter().find_map(|leaf| rest.strip_suffix(&format!("_{leaf}")))?;
    is_valid(name).then(|| name.to_string())
}

/// The map name a slashed (`.mcap`) stream belongs to, if it is one: `map/<name>/<leaf>`.
fn slashed_name_of(stream: &str) -> Option<String> {
    let mut parts = stream.trim_start_matches('/').split('/');
    if parts.next()? != "map" {
        return None;
    }
    let name = parts.next()?;
    let leaf = parts.next()?;
    if parts.next().is_some() || !(LEAVES.contains(&leaf) || leaf.starts_with("floor_")) {
        return None;
    }
    is_valid(name).then(|| name.to_string())
}

/// The map name a stream belongs to, if it is one of a named map's (either spelling, with or without a leading
/// `/`). The flat names a recording written before map names carries are not one.
pub fn of_stream(stream: &str) -> Option<String> {
    slashed_name_of(stream).or_else(|| underscored_name_of(stream))
}

/// Every map name the recording already holds, sorted. Both spellings are read whatever the format: a file written
/// by another tool (or an older version) is still a recording with maps in it.
pub fn taken(recording: &Recording) -> Vec<String> {
    let mut found: Vec<String> = match recording.stream_list() {
        Ok(streams) => streams.into_iter().filter_map(|s| of_stream(&s.name)).collect(),
        Err(_) => Vec::new(),
    };
    found.sort();
    found.dedup();
    found
}

/// `v1`, or `v2`, `v3`, ... — the first name of that shape `taken` doesn't hold.
pub fn next_free(taken: &[String]) -> String {
    (1..)
        .map(|n| format!("{DEFAULT_STEM}{n}"))
        .find(|name| !taken.iter().any(|t| t == name))
        .unwrap_or_else(|| FIRST.to_string())
}

/// The name a recording's next map gets by default: `v1`, or the first `v<n>` it doesn't have yet.
pub fn default_for(recording: &Recording) -> String {
    next_free(&taken(recording))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dimos_recording::{db, lcm, mcap_io};

    #[test]
    fn a_name_is_a_plain_identifier() {
        for name in ["v1", "v12", "office", "my_map", "a"] {
            assert!(is_valid(name), "{name}");
            check(name).unwrap();
        }
        for name in ["", "1v", "_v1", "my map", "map/v1", "a-b", "a.b", "v1/", &"x".repeat(MAX + 1)] {
            assert!(!is_valid(name), "{name}");
            assert!(check(name).is_err(), "{name}");
        }
        // what the refusal says, so the page can show it
        assert!(format!("{:#}", check("my map").unwrap_err()).contains("letters, digits and underscores"));
        assert!(format!("{:#}", check("1v").unwrap_err()).contains("start with a letter"));
    }

    #[test]
    fn the_streams_are_spelled_for_the_format() {
        assert_eq!(stream(Format::Mcap, "v1", "cloud"), "map/v1/cloud");
        assert_eq!(stream(Format::Db, "v1", "cloud"), "map_v1_cloud");
        assert_eq!(stream(Format::Mcap, "v2", "named_locations"), "map/v2/named_locations");
        assert_eq!(stream(Format::Db, "v2", "named_locations"), "map_v2_named_locations");
        assert_eq!(frame("v1"), "map/v1");
    }

    #[test]
    fn a_streams_map_name_is_read_back_from_either_spelling() {
        assert_eq!(slashed_name_of("map/v1/cloud").as_deref(), Some("v1"));
        assert_eq!(slashed_name_of("/map/v1/voxels").as_deref(), Some("v1"));
        assert_eq!(slashed_name_of("map/my_map/floor_3").as_deref(), Some("my_map"));
        assert_eq!(underscored_name_of("map_v1_cloud").as_deref(), Some("v1"));
        assert_eq!(of_stream("map_v1_cloud").as_deref(), Some("v1"));
        assert_eq!(of_stream("map/v1/cloud").as_deref(), Some("v1"));
        assert_eq!(underscored_name_of("map_my_map_named_locations").as_deref(), Some("my_map"));
        // the flat names older recordings carry are not named maps
        for name in ["map_cloud", "/map_cloud", "global_map", "map/pipeline", "map/voxels", "/map/tf", "map_builder_annotations", "/map_builder/path", "lidar"] {
            assert_eq!(slashed_name_of(name), None, "{name}");
            assert_eq!(underscored_name_of(name), None, "{name}");
            assert_eq!(of_stream(name), None, "{name}");
        }
    }

    #[test]
    fn the_default_name_counts_up_past_the_maps_the_recording_has() {
        assert_eq!(next_free(&[]), FIRST);
        assert_eq!(FIRST, format!("{DEFAULT_STEM}1"));
        assert_eq!(next_free(&["v1".to_string()]), "v2");
        assert_eq!(next_free(&["v1".to_string(), "v2".to_string(), "office".to_string()]), "v3");
        assert_eq!(next_free(&["v2".to_string()]), "v1", "a gap is filled");

        let dir = tempfile::tempdir().unwrap();
        let text = |s: &str| lcm::encode_string(s);
        // an .mcap with v1 and v2 saved in it, beside a legacy flat save
        let mcap = dir.path().join("walk.mcap");
        mcap_io::write_fixture(
            &mcap,
            &[
                ("/lidar", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0, text("x")),
                ("map/pipeline", "lcm", lcm::STRING_TYPE, 1.0, text("x")),
                ("map/voxels", "lcm", lcm::STRING_TYPE, 1.0, text("x")),
                ("map/v1/annotations", "lcm", lcm::STRING_TYPE, 1.0, text("x")),
                ("map/v2/cloud", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0, text("x")),
            ],
        );
        let recording = Recording::open(&mcap).unwrap();
        assert_eq!(taken(&recording), ["v1", "v2"]);
        assert_eq!(default_for(&recording), "v3");

        // a .db with the same maps, spelled the way a .db spells them
        let db_path = dir.path().join("walk.db");
        let mut connection = db::Connection::open(&db_path).unwrap();
        for name in ["lidar", "map_cloud", "map_builder_annotations", "map_v1_annotations", "map_v2_cloud"] {
            db::write_stream(&mut connection, name, lcm::STRING_TYPE, &[(1.0, text("x"))]).unwrap();
        }
        drop(connection);
        let recording = Recording::open(&db_path).unwrap();
        assert_eq!(taken(&recording), ["v1", "v2"]);
        assert_eq!(default_for(&recording), "v3");

        // nothing saved yet: v1
        let empty = dir.path().join("empty.db");
        db::write_stream(&mut db::Connection::open(&empty).unwrap(), "lidar", lcm::STRING_TYPE, &[(1.0, text("x"))]).unwrap();
        assert_eq!(default_for(&Recording::open(&empty).unwrap()), "v1");
    }
}
