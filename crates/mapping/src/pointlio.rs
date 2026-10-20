//! Point-LIO over a recording that has a lidar and an IMU but nothing placing the lidar's scans (no odometry, and
//! every tf edge above the lidar static): dimos's estimator (pointlio-core, what dimos-pointlio runs), fed the
//! recording's own messages, writes the odometry back in place the way lite_record's post_process does:
//! - `odom → <root>` on the tf (root: the top of the static tree the lidar hangs from, `base_link` here), so the
//!   static edges below it place every sensor,
//! - the same poses as nav_msgs.Odometry on `/pointlio_odometry`,
//! - in an .mcap, the scans motion-compensated (deskewed by the estimator's per-point states) on `/pointlio_lidar`,
//!   its channel naming the raw lidar in `derived_from` so the map build takes it instead.
//!
//! Everything written is on the log clock (the lidar's header stamps moved by `clock::offset`), like the tf it joins.
//! The IMU's accelerations go in as g: m/s² (dimos, most drivers) is told from g (Livox's ROS driver) by their norm.
//! The lidar → IMU extrinsic is tf's when tf connects the two frames, else Point-LIO's default (a Mid-360's IMU).
use crate::build::{Cancelled, Progress};
use crate::tf::{iso, parts, Iso, TfTree};
use anyhow::{bail, Context, Result};
use dimos_recording::lcm::{self, Header, Odometry, Pose, PointCloud2};
use dimos_recording::mcap_io::{DERIVED_FROM_KEY, GENERATION_KEY, LCM_TYPE_KEY};
use dimos_recording::{Format, Kind, Recording, StreamInfo};
use nalgebra::Point3;
use dimos_pointlio::pointlio_core::{Config, LivoxPoint, PointLio};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub const ODOM_FRAME: &str = "odom";
/// the odometry and the deskewed scans, named as lite_record names its own Point-LIO output
pub const ODOMETRY_TOPIC: &str = "/pointlio_odometry";
pub const CLOUD_TOPIC: &str = "/pointlio_lidar";
const GRAVITY: f64 = 9.80665;
/// an IMU whose accelerations average more than this is in m/s² (gravity alone is 1 g = 9.8 m/s²)
const METRES_PER_SECOND_SQUARED_ABOVE: f64 = 4.0;
/// IMU samples averaged to tell the units
const UNIT_SAMPLES: usize = 200;
/// Point-LIO's settings here: its defaults (dimos's)
pub fn config() -> Config {
    Config::default()
}

pub const STAGE: &str = "Point-LIO: estimating odometry";

/// The lidar and IMU streams Point-LIO runs on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Inputs {
    pub lidar: String,
    pub imu: String,
}

fn is_imu(stream: &StreamInfo) -> bool {
    stream.type_name.replace("/msg/", ".").replace('/', ".") == lcm::IMU_TYPE
}

/// The lidar and IMU Point-LIO would run on, or None: the recording has odometry already, no IMU, or no lidar (a
/// cloud stream of scans, not a map or a corrected copy). The IMU is the one under the lidar's topic prefix, else the
/// only IMU there is; the lidar the one with the most messages. `has_messages` in place of the counts (opening a .db
/// doesn't count its rows).
pub fn inputs(streams: &[StreamInfo], has_messages: impl Fn(&StreamInfo) -> bool) -> Option<Inputs> {
    if streams.iter().any(|s| s.kind == Kind::Odometry && has_messages(s)) {
        return None;
    }
    let prefix = |name: &str| name.rsplit_once('/').map_or(String::new(), |(prefix, _)| prefix.to_string());
    let mut lidars: Vec<&StreamInfo> = streams.iter().filter(|s| s.kind == Kind::Cloud && s.derived_from.is_none() && !crate::frames::map_like(&s.name) && has_messages(s)).collect();
    lidars.sort_by_key(|s| std::cmp::Reverse(s.count));
    let imus: Vec<&StreamInfo> = streams.iter().filter(|s| is_imu(s) && has_messages(s)).collect();
    let paired = lidars.iter().find_map(|lidar| imus.iter().find(|imu| prefix(&imu.name) == prefix(&lidar.name)).map(|imu| (*lidar, *imu)));
    let (lidar, imu) = match paired {
        Some(pair) => pair,
        None if imus.len() == 1 => (*lidars.first()?, imus[0]),
        None => return None,
    };
    Some(Inputs { lidar: lidar.name.clone(), imu: imu.name.clone() })
}

/// Whether tf moves `lidar_frame` already (an edge on its way to its root isn't static): Point-LIO isn't needed then.
pub fn moved_by_tf(tree: &TfTree, lidar_frame: &str) -> bool {
    let edges = tree.edges();
    let mut at = lidar_frame.trim_start_matches('/').to_string();
    for _ in 0..edges.len() {
        let Some(edge) = edges.iter().filter(|e| e.child == at).max_by_key(|e| e.samples) else { return false };
        if !edge.fixed {
            return true;
        }
        at = edge.parent.clone();
    }
    false
}

/// What Point-LIO made of the recording.
pub struct Estimate {
    pub inputs: Inputs,
    pub lidar_frame: String,
    pub imu_frame: String,
    /// imu ← lidar
    pub extrinsic: Iso,
    /// from tf (else Point-LIO's default)
    pub extrinsic_from_tf: bool,
    /// the IMU's accelerations were in m/s² (else g)
    pub acc_in_ms2: bool,
    /// added to the lidar's header stamps to put them on the log clock
    pub clock_offset: f64,
    /// the IMU's pose in odom at the end of each scan, on the lidar's clock
    pub poses: Vec<(f64, Iso)>,
    pub scans: usize,
    pub path_length: f64,
    /// the fastest the estimate moved between two scans, m/s (a lost estimate runs off at tens)
    pub max_speed: f64,
}

impl Estimate {
    pub fn notes(&self) -> Vec<String> {
        let [x, y, z] = parts(&self.extrinsic).0;
        let mut notes = vec![
            format!(
                "Point-LIO: {} poses from {} scans of {} + {}, {:.1} m of path, start to end {:.2} m",
                self.poses.len(),
                self.scans,
                self.inputs.lidar,
                self.inputs.imu,
                self.path_length,
                self.start_to_end()
            ),
            format!(
                "Point-LIO: IMU in {}, lidar → IMU ({:.3}, {:.3}, {:.3}) m {}",
                if self.acc_in_ms2 { "m/s²" } else { "g" },
                x,
                y,
                z,
                if self.extrinsic_from_tf { "from tf" } else { "(Point-LIO's default, a Mid-360's: tf doesn't connect the two)" }
            ),
        ];
        if self.max_speed > 5.0 {
            notes.push(format!("warning: Point-LIO's estimate moved at {:.0} m/s at its fastest: it may have lost track", self.max_speed));
        }
        notes
    }

    pub fn start_to_end(&self) -> f64 {
        match (self.poses.first(), self.poses.last()) {
            (Some(a), Some(b)) => (b.1.translation.vector - a.1.translation.vector).norm(),
            _ => 0.0,
        }
    }
}

/// A PointCloud2 field read as f64, by its datatype.
fn field_reader(cloud: &PointCloud2, name: &str) -> Option<(usize, u8)> {
    let field = cloud.fields.iter().find(|f| f.name == name)?;
    let size = match field.datatype {
        1 | 2 => 1,
        3 | 4 => 2,
        5..=7 => 4,
        8 => 8,
        _ => return None,
    };
    (field.offset as usize + size <= cloud.point_step as usize).then_some((field.offset as usize, field.datatype))
}

fn read(point: &[u8], (offset, datatype): (usize, u8)) -> f64 {
    let b = &point[offset..];
    match datatype {
        1 => b[0] as i8 as f64,
        2 => b[0] as f64,
        3 => i16::from_le_bytes([b[0], b[1]]) as f64,
        4 => u16::from_le_bytes([b[0], b[1]]) as f64,
        5 => i32::from_le_bytes(b[..4].try_into().unwrap()) as f64,
        6 => u32::from_le_bytes(b[..4].try_into().unwrap()) as f64,
        7 => f32::from_le_bytes(b[..4].try_into().unwrap()) as f64,
        _ => f64::from_le_bytes(b[..8].try_into().unwrap()),
    }
}

/// A scan as Point-LIO's Livox points: each point's time after the header stamp from `offset_time` (ns, Livox),
/// `timestamp` (absolute, s or ns: Livox's ROS driver) or `time` / `t` (relative: s as a float, ns as an integer);
/// reflectivity from `intensity` as it is, Livox's `tag` when there is one. A scan with no point times is one instant.
pub fn livox_points(cloud: &PointCloud2) -> Result<Vec<LivoxPoint>> {
    if cloud.is_bigendian {
        bail!("big-endian clouds aren't supported");
    }
    let (x, y, z) = match (field_reader(cloud, "x"), field_reader(cloud, "y"), field_reader(cloud, "z")) {
        (Some(x), Some(y), Some(z)) => (x, y, z),
        _ => bail!("the cloud has no x, y, z"),
    };
    let stamp = cloud.header.ts();
    let offset_ns: Box<dyn Fn(&[u8]) -> f64> = if let Some(f) = field_reader(cloud, "offset_time") {
        Box::new(move |p| read(p, f))
    } else if let Some(f) = field_reader(cloud, "timestamp") {
        Box::new(move |p| {
            let v = read(p, f);
            // seconds or nanoseconds: whichever is nearer the header stamp
            if (v - stamp).abs() < (v - stamp * 1e9).abs() { (v - stamp) * 1e9 } else { v - stamp * 1e9 }
        })
    } else if let Some(f) = field_reader(cloud, "time").or_else(|| field_reader(cloud, "t")) {
        let float = matches!(f.1, 7 | 8);
        Box::new(move |p| if float { read(p, f) * 1e9 } else { read(p, f) })
    } else {
        Box::new(|_| 0.0)
    };
    let intensity = field_reader(cloud, "intensity").or_else(|| field_reader(cloud, "reflectivity"));
    let tag = field_reader(cloud, "tag");
    let step = cloud.point_step as usize;
    let n = (cloud.width * cloud.height) as usize;
    if step == 0 || cloud.data.len() < n * step {
        bail!("the cloud has {} bytes for {n} points of {step}", cloud.data.len());
    }
    Ok(cloud
        .data
        .chunks_exact(step)
        .take(n)
        .map(|p| LivoxPoint {
            x: read(p, x) as f32,
            y: read(p, y) as f32,
            z: read(p, z) as f32,
            reflectivity: intensity.map_or(0, |f| read(p, f).round().clamp(0.0, u16::MAX as f64) as u16),
            tag: tag.map_or(0, |f| read(p, f) as u8),
            // Point-LIO's Livox handler drops lines past scan_line: other lidars' rings aren't Livox lines
            line: 0,
            offset_time: offset_ns(p).max(0.0) as u64,
        })
        .collect())
}

/// The first message's header frame on `stream`, and up to `count` decoded messages of it.
fn first_messages<T>(recording: &Recording, stream: &str, count: usize, decode: impl Fn(&str, &[u8]) -> Result<T>) -> Result<Vec<(f64, T)>> {
    let mut found = Vec::new();
    recording.for_each_payload_of(&[stream], None, |_, ts, encoding, payload| {
        if let Ok(message) = decode(encoding, payload) {
            found.push((ts, message));
        }
        Ok(found.len() < count)
    })?;
    Ok(found)
}

/// Runs Point-LIO over `inputs` (one read of the recording), reporting progress; `spool`, when given, collects the
/// deskewed scans (in the lidar's frame, at each scan's end on the lidar's clock) for `write`.
pub fn estimate(recording: &Recording, tree: &TfTree, inputs: &Inputs, config: &Config, spool: Option<&Path>, report: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<Estimate> {
    let first_scan = first_messages(recording, &inputs.lidar, 1, dimos_recording::decode_point_cloud2)?;
    let (_, first_scan) = first_scan.first().with_context(|| format!("no readable scan on {}", inputs.lidar))?;
    let lidar_frame = first_scan.header.frame_id.trim_start_matches('/').to_string();
    let imu_head = first_messages(recording, &inputs.imu, UNIT_SAMPLES, dimos_recording::decode_imu)?;
    let (_, first_imu) = imu_head.first().with_context(|| format!("no readable message on {}", inputs.imu))?;
    let imu_frame = first_imu.header.frame_id.trim_start_matches('/').to_string();
    let norm = imu_head.iter().map(|(_, m)| m.linear_acceleration.iter().map(|a| a * a).sum::<f64>().sqrt()).sum::<f64>() / imu_head.len() as f64;
    let acc_in_ms2 = norm > METRES_PER_SECOND_SQUARED_ABOVE;
    let acc_scale = if acc_in_ms2 { 1.0 / GRAVITY } else { 1.0 };

    let mut config = config.clone();
    let from_tf = (lidar_frame != imu_frame).then(|| tree.lookup(&imu_frame, &lidar_frame, first_scan.header.ts(), f64::INFINITY)).flatten();
    let extrinsic = match from_tf {
        Some(extrinsic) => {
            let r = extrinsic.rotation.to_rotation_matrix();
            let m = r.matrix();
            config.extrinsic_r = (0..9).map(|i| m[(i / 3, i % 3)]).collect();
            config.extrinsic_t = extrinsic.translation.vector.iter().copied().collect();
            extrinsic
        }
        None => {
            let r = &config.extrinsic_r;
            let m = nalgebra::Matrix3::new(r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8]);
            let rotation = nalgebra::UnitQuaternion::from_matrix(&m);
            Iso::from_parts(nalgebra::Translation3::new(config.extrinsic_t[0], config.extrinsic_t[1], config.extrinsic_t[2]), rotation)
        }
    };
    let lidar_in_imu_inverse = extrinsic.inverse();
    let mut lio = PointLio::new(&config);
    let mut spool = spool.map(|path| std::fs::File::create(path).map(BufWriter::new)).transpose().context("the deskewed scans' spool")?;
    let mut poses: Vec<(f64, Iso)> = Vec::new();
    let mut offsets: Vec<(f64, f64)> = Vec::new();
    let mut scans = 0usize;
    let mut path_length = 0.0;
    let mut max_speed: f64 = 0.0;
    let stage = format!("{STAGE} from {} + {}", inputs.lidar, inputs.imu);
    let progress = std::cell::Cell::new((0u64, 1u64));
    let started = std::time::Instant::now();
    let mut last_report = std::time::Instant::now();
    let mut failure: Option<anyhow::Error> = None;
    let mut drain = |lio: &mut PointLio, poses: &mut Vec<(f64, Iso)>, path_length: &mut f64, max_speed: &mut f64| -> Result<()> {
        loop {
            let before = lio.lidar_buffer_len();
            if lio.process() {
                let odom = lio.odometry();
                let pose = iso(odom.pos, odom.quat);
                if let Some((t, last)) = poses.last() {
                    let step = (pose.translation.vector - last.translation.vector).norm();
                    *path_length += step;
                    if odom.ts > *t {
                        *max_speed = max_speed.max(step / (odom.ts - t));
                    }
                }
                if let Some(w) = spool.as_mut() {
                    let cloud: Vec<[f32; 4]> = lio
                        .body_cloud()
                        .iter()
                        .map(|p| {
                            let q = lidar_in_imu_inverse * Point3::new(p.x as f64, p.y as f64, p.z as f64);
                            [q.x as f32, q.y as f32, q.z as f32, p.intensity]
                        })
                        .collect();
                    spool_scan(w, odom.ts, &cloud)?;
                }
                poses.push((odom.ts, pose));
            }
            if lio.lidar_buffer_len() == before {
                return Ok(());
            }
        }
    };
    let on_progress = |done: u64, total: u64| progress.set((done, total));
    recording.for_each_payload_of(&[&inputs.lidar, &inputs.imu], Some(&on_progress), |which, log, encoding, payload| {
        if cancel.load(Ordering::Relaxed) {
            failure = Some(anyhow::anyhow!(Cancelled));
            return Ok(false);
        }
        if which == 1 {
            let Ok(imu) = dimos_recording::decode_imu(encoding, payload) else { return Ok(true) };
            lio.feed_imu(imu.header.ts(), imu.angular_velocity, imu.linear_acceleration.map(|a| a * acc_scale));
        } else {
            let Ok(cloud) = dimos_recording::decode_point_cloud2(encoding, payload) else { return Ok(true) };
            let Ok(points) = livox_points(&cloud) else { return Ok(true) };
            let stamp = cloud.header.ts();
            offsets.push((log, stamp));
            lio.feed_lidar((cloud.header.stamp_sec as i64 * 1_000_000_000 + cloud.header.stamp_nsec as i64).max(0) as u64, &points);
            scans += 1;
        }
        if let Err(error) = drain(&mut lio, &mut poses, &mut path_length, &mut max_speed) {
            failure = Some(error);
            return Ok(false);
        }
        if last_report.elapsed().as_millis() > 250 {
            last_report = std::time::Instant::now();
            let (done, total) = progress.get();
            report(Progress { overall: None, stage: stage.clone(), stage_index: 0, stage_count: 2, done, total, note: format!("{scans} scans, {} poses, {path_length:.1} m of path, {:.0} s", poses.len(), started.elapsed().as_secs_f64()) });
        }
        Ok(true)
    })?;
    if let Some(error) = failure {
        return Err(error);
    }
    if let Some(mut w) = spool {
        w.flush()?;
    }
    if poses.is_empty() {
        bail!("Point-LIO made no poses from {} + {} ({scans} scans): is {} the lidar the IMU {} is in?", inputs.lidar, inputs.imu, inputs.lidar, inputs.imu);
    }
    Ok(Estimate {
        inputs: inputs.clone(),
        lidar_frame,
        imu_frame,
        extrinsic,
        extrinsic_from_tf: from_tf.is_some(),
        acc_in_ms2,
        clock_offset: crate::clock::offset(offsets),
        poses,
        scans,
        path_length,
        max_speed,
    })
}

/// One deskewed scan onto the spool `write` reads: its time, its point count, then x, y, z, intensity (f32) each.
fn spool_scan(w: &mut impl Write, ts: f64, points: &[[f32; 4]]) -> std::io::Result<()> {
    w.write_all(&ts.to_le_bytes())?;
    w.write_all(&(points.len() as u32).to_le_bytes())?;
    for p in points {
        for v in p {
            w.write_all(&v.to_le_bytes())?;
        }
    }
    Ok(())
}

/// What `write` put into the recording.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Written {
    /// the edge written: parent (odom) → child (the root above the lidar)
    pub parent: String,
    pub child: String,
    pub poses: usize,
    pub clouds: usize,
}

/// The edge the odometry goes on: odom → the root of the lidar's static tree (pointlio_odom when that root is
/// called odom), and root ← lidar.
fn edge_for(tree: &TfTree, lidar_frame: &str) -> (String, String, Iso) {
    let root = crate::frames::root_of(tree, lidar_frame).unwrap_or_else(|| lidar_frame.to_string());
    let lidar_in_root = tree.lookup(&root, lidar_frame, 0.0, f64::INFINITY).unwrap_or_else(Iso::identity);
    let parent = if root == ODOM_FRAME { "pointlio_odom".to_string() } else { ODOM_FRAME.to_string() };
    (parent, root, lidar_in_root)
}

/// Writes `estimate` into the recording at `path` in place: the tf edge, the odometry, and (an .mcap, given the
/// spool `estimate` filled) the deskewed scans.
pub fn write(path: &Path, tree: &TfTree, estimate: &Estimate, spool: Option<&Path>, generation: f64) -> Result<Written> {
    let (parent, child, lidar_in_root) = edge_for(tree, &estimate.lidar_frame);
    let root_in_lidar = lidar_in_root.inverse();
    let offset = estimate.clock_offset;
    let placed: Vec<(f64, Iso)> = estimate.poses.iter().map(|(t, imu)| (t + offset, imu * estimate.extrinsic * root_in_lidar)).collect();
    let samples: Vec<dimos_recording::tf_edit::Sample> = placed.iter().map(|(t, pose)| {
        let (translation, rotation) = parts(pose);
        (*t, translation, rotation)
    }).collect();
    dimos_recording::tf_edit::replace_edge(path, &parent, &child, &samples, generation)?;
    let odometry: Vec<(f64, Vec<u8>)> = samples
        .iter()
        .map(|(t, position, orientation)| (*t, lcm::encode_odometry(&Odometry { header: Header::at(*t, &parent), child_frame_id: child.clone(), pose: Pose { position: *position, orientation: *orientation } })))
        .collect();
    let mut clouds = 0;
    match dimos_recording::format_of(path)? {
        Format::Db => {
            let mut connection = dimos_recording::db::Connection::open(path)?;
            dimos_recording::db::write_stream(&mut connection, ODOMETRY_TOPIC.trim_start_matches('/'), lcm::ODOMETRY_TYPE, &odometry)?;
        }
        Format::Mcap => {
            let mut appender = dimos_recording::mcap_append::Appender::open(path)?;
            let metadata = |kind: &str| BTreeMap::from([(LCM_TYPE_KEY.to_string(), kind.to_string()), (GENERATION_KEY.to_string(), format!("{generation:.6}"))]);
            let odometry_channel = appender.new_channel(ODOMETRY_TOPIC, 0, "lcm", &metadata(lcm::ODOMETRY_TYPE));
            let mut cloud_metadata = metadata(lcm::POINT_CLOUD2_TYPE);
            cloud_metadata.insert(DERIVED_FROM_KEY.to_string(), estimate.inputs.lidar.clone());
            let cloud_channel = spool.map(|_| appender.new_channel(CLOUD_TOPIC, 0, "lcm", &cloud_metadata));
            let mut reader = spool.map(|path| std::fs::File::open(path).map(BufReader::new)).transpose()?;
            for (t, payload) in odometry {
                if let (Some(reader), Some(channel)) = (reader.as_mut(), cloud_channel) {
                    // the spool holds one scan per pose, in the same order
                    let mut head = [0u8; 12];
                    reader.read_exact(&mut head)?;
                    let n = u32::from_le_bytes(head[8..12].try_into().unwrap()) as usize;
                    let mut raw = vec![0u8; n * 16];
                    reader.read_exact(&mut raw)?;
                    let mut points = Vec::with_capacity(n);
                    let mut intensity = Vec::with_capacity(n);
                    for p in raw.chunks_exact(16) {
                        let f = |i: usize| f32::from_le_bytes(p[i * 4..i * 4 + 4].try_into().unwrap());
                        points.push([f(0), f(1), f(2)]);
                        intensity.push(f(3));
                    }
                    let cloud = lcm::encode_xyz_cloud(&Header::at(t, &estimate.lidar_frame), &points, Some(&intensity));
                    appender.write_stream(channel, (t.max(0.0) * 1e9) as u64, cloud)?;
                    clouds += 1;
                }
                appender.write_stream(odometry_channel, (t.max(0.0) * 1e9) as u64, payload)?;
            }
            appender.finish()?;
        }
    }
    Ok(Written { parent, child, poses: samples.len(), clouds })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dimos_recording::lcm::PointField;

    fn stream(name: &str, type_name: &str, count: u64) -> StreamInfo {
        StreamInfo { name: name.into(), type_name: type_name.into(), kind: Kind::of(type_name), count, derived_from: None }
    }

    #[test]
    fn a_lidar_and_an_imu_with_no_odometry_get_point_lio() {
        let any = |s: &StreamInfo| s.count > 0;
        let office = vec![stream("/livox/lidar", "sensor_msgs/msg/PointCloud2", 4717), stream("/livox/imu", "sensor_msgs/msg/Imu", 94634), stream("/tf", "tf2_msgs/msg/TFMessage", 2000)];
        assert_eq!(inputs(&office, any), Some(Inputs { lidar: "/livox/lidar".into(), imu: "/livox/imu".into() }));
        // the only IMU, under another prefix
        let apart = vec![stream("/lidar", "sensor_msgs.PointCloud2", 10), stream("/go2/imu", "sensor_msgs.Imu", 10), stream("/global_map", "sensor_msgs.PointCloud2", 99)];
        assert_eq!(inputs(&apart, any), Some(Inputs { lidar: "/lidar".into(), imu: "/go2/imu".into() }));
        // two IMUs, neither beside the lidar: which one is in it isn't known
        let mut two = apart.clone();
        two.push(stream("/camera/imu", "sensor_msgs.Imu", 10));
        assert_eq!(inputs(&two, any), None);
        // the prefix decides between two
        two.push(stream("/imu", "sensor_msgs.Imu", 10));
        assert_eq!(inputs(&two, any).unwrap().imu, "/imu");
        // odometry already places the scans
        let mut placed = office.clone();
        placed.push(stream("/odom", "nav_msgs/msg/Odometry", 100));
        assert_eq!(inputs(&placed, any), None);
        // a corrected copy isn't a lidar to run it on
        let mut copy = vec![stream("/imu", "sensor_msgs.Imu", 10), stream("/pointlio_lidar", "sensor_msgs.PointCloud2", 10)];
        copy[1].derived_from = Some("/lidar".into());
        assert_eq!(inputs(&copy, any), None);
    }

    #[test]
    fn tf_that_moves_the_lidar_needs_no_point_lio() {
        let mut tree = TfTree::default();
        for t in 0..3 {
            tree.add(t as f64, "base_link", "livox_frame", iso([0.1, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));
        }
        tree.finish();
        assert!(!moved_by_tf(&tree, "livox_frame"));
        // the odometry goes above the static tree's root
        let (parent, child, lidar_in_root) = edge_for(&tree, "livox_frame");
        assert_eq!((parent.as_str(), child.as_str()), ("odom", "base_link"));
        assert!((lidar_in_root.translation.vector.x - 0.1).abs() < 1e-9);
        assert_eq!(edge_for(&TfTree::default(), "lidar").1, "lidar", "no tf: the lidar's own frame");
        for t in 0..3 {
            tree.add(t as f64, "odom", "base_link", iso([t as f64, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));
        }
        tree.finish();
        assert!(moved_by_tf(&tree, "livox_frame"));
    }

    /// what write leaves in an .mcap: the edge above the lidar's static tree, the odometry, the deskewed scans (named
    /// a copy of the raw lidar), and the map build places the copy through the new edge
    #[test]
    fn the_estimate_goes_into_the_recording_and_places_the_scans() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rig.mcap");
        let tf_static = lcm::encode_tf(&[lcm::TfEdge { header: Header::at(1.0, "base_link"), child: "livox_frame".into(), translation: [0.0, 0.0, 0.5], rotation: [0.0, 0.0, 0.0, 1.0] }]);
        let raw: Vec<(&str, &str, &str, f64, Vec<u8>)> = (0..5)
            .map(|i| ("/livox/lidar", "lcm", lcm::POINT_CLOUD2_TYPE, 1.0 + i as f64 * 0.1, lcm::encode_xyz_cloud(&Header::at(1.0 + i as f64 * 0.1, "livox_frame"), &[[2.0, 0.0, 0.0]], None)))
            .chain([("/tf_static", "lcm", lcm::TF_TYPE, 1.0, tf_static), ("/livox/imu", "lcm", lcm::IMU_TYPE, 1.0, vec![0; 8])])
            .collect();
        dimos_recording::mcap_io::write_fixture(&path, &raw);
        let recording = Recording::open(&path).unwrap();
        let tree = crate::frames::tf_tree(&recording).unwrap();
        drop(recording);
        // walking along x at 1 m/s, the IMU 10 cm in front of the lidar
        let extrinsic = iso([-0.1, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]);
        let poses: Vec<(f64, Iso)> = (0..5).map(|i| (1.1 + i as f64 * 0.1, iso([0.1 * i as f64, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]))).collect();
        let spool = dir.path().join("spool");
        let mut w = BufWriter::new(std::fs::File::create(&spool).unwrap());
        for (t, _) in &poses {
            spool_scan(&mut w, *t, &[[2.0, 0.0, 0.0, 7.0]]).unwrap();
        }
        drop(w);
        let estimate = Estimate {
            inputs: Inputs { lidar: "/livox/lidar".into(), imu: "/livox/imu".into() },
            lidar_frame: "livox_frame".into(),
            imu_frame: "livox_imu_frame".into(),
            extrinsic,
            extrinsic_from_tf: false,
            acc_in_ms2: true,
            clock_offset: 0.0,
            poses,
            scans: 5,
            path_length: 0.4,
            max_speed: 1.0,
        };
        let written = write(&path, &tree, &estimate, Some(&spool), 1.0).unwrap();
        assert_eq!((written.parent.as_str(), written.child.as_str(), written.poses, written.clouds), ("odom", "base_link", 5, 5));

        let recording = Recording::open(&path).unwrap();
        let streams = recording.streams().unwrap();
        let copy = streams.iter().find(|s| s.name == CLOUD_TOPIC).unwrap();
        assert_eq!((copy.count, copy.derived_from.as_deref()), (5, Some("/livox/lidar")));
        assert_eq!(streams.iter().find(|s| s.name == ODOMETRY_TOPIC).unwrap().kind, Kind::Odometry);
        assert_eq!(inputs(&streams, |s| s.count > 0), None, "it has odometry now");
        let tree = crate::frames::tf_tree(&recording).unwrap();
        assert!(moved_by_tf(&tree, "livox_frame"));
        // base_link = imu * (imu ← lidar) * (lidar ← base_link): the lidar 0.5 m above base_link, the IMU 0.1 m behind
        // the lidar, so at the third pose (x 0.2) base_link is at x 0.2 - 0.1, z -0.5
        let base = tree.lookup("odom", "base_link", 1.3, 0.01).unwrap();
        let (t, _) = parts(&base);
        assert!((t[0] - 0.1).abs() < 1e-6 && t[1].abs() < 1e-6 && (t[2] + 0.5).abs() < 1e-6, "{t:?}");
        let analysis = crate::frames::analyze(&recording, &tree).unwrap();
        assert_eq!(analysis.default_cloud.as_deref(), Some(CLOUD_TOPIC), "the deskewed copy before the raw scans");
    }

    fn cloud(fields: &[(&str, u32, u8)], step: u32, points: &[Vec<u8>]) -> PointCloud2 {
        PointCloud2 {
            header: Header::at(100.0, "livox_frame"),
            height: 1,
            width: points.len() as u32,
            fields: fields.iter().map(|(name, offset, datatype)| PointField { name: name.to_string(), offset: *offset, datatype: *datatype, count: 1 }).collect(),
            is_bigendian: false,
            point_step: step,
            row_step: step * points.len() as u32,
            data: points.concat(),
            is_dense: true,
        }
    }

    #[test]
    fn point_times_come_from_whichever_field_has_them() {
        let xyz = |x: f32| [x.to_le_bytes(), 2f32.to_le_bytes(), 3f32.to_le_bytes()].concat();
        // Livox's ROS driver: offset_time (ns) and an absolute timestamp (s)
        let livox = cloud(&[("x", 0, 7), ("y", 4, 7), ("z", 8, 7), ("intensity", 12, 7), ("tag", 16, 2), ("line", 17, 2), ("offset_time", 20, 6), ("timestamp", 24, 8)], 32, &[[xyz(1.0), 17f32.to_le_bytes().to_vec(), vec![0x10, 3, 0, 0], 5_000_000u32.to_le_bytes().to_vec(), 100.005f64.to_le_bytes().to_vec()].concat()]);
        let p = livox_points(&livox).unwrap()[0];
        assert_eq!((p.x, p.reflectivity, p.tag, p.line, p.offset_time), (1.0, 17, 0x10, 0, 5_000_000));
        // only the absolute timestamp, in ns
        let absolute = cloud(&[("x", 0, 7), ("y", 4, 7), ("z", 8, 7), ("timestamp", 16, 8)], 24, &[[xyz(1.0), vec![0; 4], (100.002e9f64).to_le_bytes().to_vec()].concat()]);
        assert!((livox_points(&absolute).unwrap()[0].offset_time as i64 - 2_000_000).abs() < 1000);
        // a relative time in seconds; none at all
        let relative = cloud(&[("x", 0, 7), ("y", 4, 7), ("z", 8, 7), ("time", 12, 7)], 16, &[[xyz(1.0), 0.01f32.to_le_bytes().to_vec()].concat()]);
        assert!((livox_points(&relative).unwrap()[0].offset_time as i64 - 10_000_000).abs() < 1000);
        let bare = cloud(&[("x", 0, 7), ("y", 4, 7), ("z", 8, 7)], 12, &[xyz(1.0)]);
        assert_eq!(livox_points(&bare).unwrap()[0].offset_time, 0);
    }
}
