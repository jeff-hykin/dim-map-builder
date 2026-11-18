//! dimSLAM over a recording that has a lidar and an IMU but nothing placing the lidar's scans (no odometry, and
//! every tf edge above the lidar static): dimSLAM's filter (`dim_slam::DimSlamCore`, lidar + IMU only), fed the
//! recording's own messages, writes the odometry back in place the way lite_record's post_process does:
//! - `odom → <root>` on the tf (root: the top of the static tree the lidar hangs from, `base_link` here), so the
//!   static edges below it place every sensor,
//! - the same poses as nav_msgs.Odometry on `/dimslam_odometry`,
//! - in an .mcap, the scans motion-compensated on `/dimslam_lidar`, its channel naming the raw lidar in
//!   `derived_from` so the map build takes it instead. dimSLAM publishes one pose per scan, at the scan's end, and
//!   gives back no points: each point is moved to the scan's end along the poses interpolated between the previous
//!   scan's end and this one's (the pose's own twist when there is no previous pose that close), by its own time.
//!
//! Everything written is on the log clock (the lidar's header stamps moved by `clock::offset`), like the tf it joins.
//! The IMU's accelerations go in as m/s²: m/s² (dimos, most drivers) is told from g (Livox's ROS driver) by their norm.
//! The lidar → IMU extrinsic is tf's when tf connects the two frames, else a Mid-360's (its IMU inside the lidar).
use crate::build::{Cancelled, Progress};
use crate::tf::{iso, parts, Iso, TfTree};
use anyhow::{bail, Context, Result};
use dim_slam::nalgebra as slam_na;
use dim_slam::odometry_fusion::lio_odometry::LioConfig;
use dim_slam::{DimSlamCore, DimSlamCoreConfig, ImuConfig, ImuSample, LidarConfig, LidarScan};
use dimos_recording::lcm::{self, Header, Odometry, Pose, PointCloud2};
use dimos_recording::mcap_io::{DERIVED_FROM_KEY, GENERATION_KEY, LCM_TYPE_KEY};
use dimos_recording::{Format, Kind, Recording, StreamInfo};
use nalgebra::{Point3, Translation3, UnitQuaternion, Vector3};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub const ODOM_FRAME: &str = "odom";
/// the odometry and the deskewed scans, named for the estimator that made them
pub const ODOMETRY_TOPIC: &str = "/dimslam_odometry";
pub const CLOUD_TOPIC: &str = "/dimslam_lidar";
const GRAVITY: f64 = 9.80665;
/// an IMU whose accelerations average more than this is in m/s² (gravity alone is 1 g = 9.8 m/s²)
const METRES_PER_SECOND_SQUARED_ABOVE: f64 = 4.0;
/// IMU samples averaged to tell the units
const UNIT_SAMPLES: usize = 200;
/// imu ← lidar for a Livox Mid-360 (its IMU's place in the lidar, Livox's spec): when tf doesn't connect the two
const MID360_LIDAR_IN_IMU: [f64; 3] = [-0.011, -0.02329, 0.04412];
/// the filter runs a scan once the IMU passes its end, a scan or two behind: one held this long never gets a pose
const PENDING_SECONDS: f64 = 5.0;

/// dimSLAM's settings here: its defaults for one IMU and one lidar, whose frames `estimate` fills in.
pub fn config() -> DimSlamCoreConfig {
    DimSlamCoreConfig { imus: vec![ImuConfig::default()], lidars: vec![LidarConfig::default()], ..Default::default() }
}

pub const STAGE: &str = "dimSLAM: estimating odometry";

/// The lidar and IMU streams dimSLAM runs on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Inputs {
    pub lidar: String,
    pub imu: String,
}

fn is_imu(stream: &StreamInfo) -> bool {
    stream.type_name.replace("/msg/", ".").replace('/', ".") == lcm::IMU_TYPE
}

/// The lidar and IMU dimSLAM would run on, or None: the recording has odometry already, no IMU, or no lidar (a
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

/// Whether tf moves `lidar_frame` already (an edge on its way to its root isn't static): dimSLAM isn't needed then.
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

/// What dimSLAM made of the recording.
pub struct Estimate {
    pub inputs: Inputs,
    pub lidar_frame: String,
    pub imu_frame: String,
    /// imu ← lidar
    pub extrinsic: Iso,
    /// from tf (else a Mid-360's)
    pub extrinsic_from_tf: bool,
    /// the IMU's accelerations were in m/s² (else g)
    pub acc_in_ms2: bool,
    /// the lidar's points carry their own times (else its scans are written as they are, not motion-compensated)
    pub point_times: bool,
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
                "dimSLAM: {} poses from {} scans of {} + {}, {:.1} m of path, start to end {:.2} m",
                self.poses.len(),
                self.scans,
                self.inputs.lidar,
                self.inputs.imu,
                self.path_length,
                self.start_to_end()
            ),
            format!(
                "dimSLAM: IMU in {}, lidar → IMU ({:.3}, {:.3}, {:.3}) m {}",
                if self.acc_in_ms2 { "m/s²" } else { "g" },
                x,
                y,
                z,
                if self.extrinsic_from_tf { "from tf" } else { "(a Mid-360's: tf doesn't connect the two)" }
            ),
        ];
        if !self.point_times {
            notes.push(format!("dimSLAM: {}'s points carry no times: its scans are written at each pose as they are, not motion-compensated", self.inputs.lidar));
        }
        if self.max_speed > 5.0 {
            notes.push(format!("warning: dimSLAM's estimate moved at {:.0} m/s at its fastest: it may have lost track", self.max_speed));
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

/// A header's stamp in ns.
fn stamp_ns(header: &Header) -> i64 {
    header.stamp_sec as i64 * 1_000_000_000 + header.stamp_nsec as i64
}

/// A scan as dimSLAM's: each point's time after the header stamp from `offset_time` (ns, Livox), `timestamp`
/// (absolute, s or ns: Livox's ROS driver) or `time` / `t` (relative: s as a float, ns as an integer), no times when
/// it has none of those (every point at the stamp); Livox's `tag` and the intensity (or reflectivity) as they are.
pub fn lidar_scan(cloud: &PointCloud2, frame_id: &str) -> Result<LidarScan> {
    if cloud.is_bigendian {
        bail!("big-endian clouds aren't supported");
    }
    let (x, y, z) = match (field_reader(cloud, "x"), field_reader(cloud, "y"), field_reader(cloud, "z")) {
        (Some(x), Some(y), Some(z)) => (x, y, z),
        _ => bail!("the cloud has no x, y, z"),
    };
    let stamp = cloud.header.ts();
    let offset_ns: Option<Box<dyn Fn(&[u8]) -> f64>> = if let Some(f) = field_reader(cloud, "offset_time") {
        Some(Box::new(move |p| read(p, f)))
    } else if let Some(f) = field_reader(cloud, "timestamp") {
        Some(Box::new(move |p| {
            let v = read(p, f);
            // seconds or nanoseconds: whichever is nearer the header stamp
            if (v - stamp).abs() < (v - stamp * 1e9).abs() { (v - stamp) * 1e9 } else { v - stamp * 1e9 }
        }))
    } else if let Some(f) = field_reader(cloud, "time").or_else(|| field_reader(cloud, "t")) {
        let float = matches!(f.1, 7 | 8);
        Some(Box::new(move |p| if float { read(p, f) * 1e9 } else { read(p, f) }))
    } else {
        None
    };
    let intensity = field_reader(cloud, "intensity").or_else(|| field_reader(cloud, "reflectivity"));
    let tag = field_reader(cloud, "tag");
    let step = cloud.point_step as usize;
    let n = (cloud.width * cloud.height) as usize;
    if step == 0 || cloud.data.len() < n * step {
        bail!("the cloud has {} bytes for {n} points of {step}", cloud.data.len());
    }
    let points = cloud.data.chunks_exact(step).take(n);
    Ok(LidarScan {
        timestamp_ns: stamp_ns(&cloud.header),
        frame_id: frame_id.to_string(),
        points: points.clone().map(|p| [read(p, x) as f32, read(p, y) as f32, read(p, z) as f32]).collect(),
        offsets_ns: offset_ns.map_or_else(Vec::new, |offset| points.clone().map(|p| offset(p).clamp(0.0, u32::MAX as f64) as u32).collect()),
        tags: tag.map_or_else(Vec::new, |f| points.clone().map(|p| read(p, f) as u8).collect()),
        intensities: intensity.map_or_else(Vec::new, |f| points.clone().map(|p| read(p, f) as f32).collect()),
    })
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

fn to_slam(pose: &Iso) -> slam_na::Isometry3<f64> {
    let ([x, y, z], [i, j, k, w]) = parts(pose);
    slam_na::Isometry3::from_parts(slam_na::Translation3::new(x, y, z), slam_na::UnitQuaternion::from_quaternion(slam_na::Quaternion::new(w, i, j, k)))
}

fn from_slam(pose: &slam_na::Isometry3<f64>) -> Iso {
    let (t, q) = (pose.translation.vector, pose.rotation);
    iso([t.x, t.y, t.z], [q.i, q.j, q.k, q.w])
}

/// A scan waiting for its pose: when it began and ended (ns, the lidar's clock), its points as the filter reads them.
struct Pending {
    begin: i64,
    end: i64,
    /// x, y, z, intensity, and the point's time after `begin` (ns)
    points: Vec<([f32; 4], u32)>,
}

/// The scan's points the filter reads (Livox's low-confidence returns, the blind range and all but every
/// `keep_every`th point left out, as its own thinning does), with their times and intensities.
fn pending(scan: &LidarScan, lio: &LioConfig) -> Pending {
    let points: Vec<([f32; 4], u32)> = dim_slam::odometry_fusion::surface_points(scan, lio.min_range, lio.keep_every)
        .iter()
        // surface_points carries the intensity times 255 and the time in ms
        .map(|p| ([p.position[0], p.position[1], p.position[2], p.reflectivity / 255.0], (p.time_ms as f64 * 1e6).round().max(0.0) as u32))
        .collect();
    let last = scan.offsets_ns.iter().copied().max().unwrap_or(0) as i64;
    Pending { begin: scan.timestamp_ns, end: scan.timestamp_ns + last, points }
}

/// The body's motion from the scan's end back to `dt` seconds before it: interpolated along `from` (an earlier pose,
/// `span` seconds before the end) to `to` when there is one, else the end's twist held constant (body frame).
fn back_from_end(to: &Iso, from: Option<(&Iso, f64)>, twist: (Vector3<f64>, Vector3<f64>), dt: f64) -> Iso {
    match from {
        Some((from, span)) => {
            let s = 1.0 - dt / span;
            let turn = (from.rotation.inverse() * to.rotation).scaled_axis();
            let rotation = from.rotation * UnitQuaternion::from_scaled_axis(turn * s);
            let translation = from.translation.vector + (to.translation.vector - from.translation.vector) * s;
            to.inverse() * Iso::from_parts(Translation3::from(translation), rotation)
        }
        None => {
            let (linear, angular) = twist;
            Iso::from_parts(Translation3::from(-linear * dt), UnitQuaternion::from_scaled_axis(-angular * dt))
        }
    }
}

/// `scan`'s points moved to its end (in the lidar's frame): each along the IMU's motion between its own time and the
/// end. `previous`: the pose before this one, when it is no more than a scan back.
fn deskew(scan: &Pending, end: &Iso, previous: Option<(&Iso, f64)>, twist: (Vector3<f64>, Vector3<f64>), extrinsic: &Iso) -> Vec<[f32; 4]> {
    let lidar_in_imu_inverse = extrinsic.inverse();
    let span = (scan.end - scan.begin) as f64 / 1e9;
    let mut cached: Option<(u32, Iso)> = None;
    scan.points
        .iter()
        .map(|&([x, y, z, intensity], offset)| {
            let motion = match cached {
                Some((at, motion)) if at == offset => motion,
                _ => {
                    let dt = span - offset as f64 / 1e9;
                    let motion = lidar_in_imu_inverse * back_from_end(end, previous, twist, dt) * extrinsic;
                    cached = Some((offset, motion));
                    motion
                }
            };
            let q = motion * Point3::new(x as f64, y as f64, z as f64);
            [q.x as f32, q.y as f32, q.z as f32, intensity]
        })
        .collect()
}

/// Runs dimSLAM over `inputs` (one read of the recording), reporting progress; `spool`, when given, collects the
/// deskewed scans (in the lidar's frame, at each scan's end on the lidar's clock) for `write`.
pub fn estimate(recording: &Recording, tree: &TfTree, inputs: &Inputs, config: &DimSlamCoreConfig, spool: Option<&Path>, report: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<Estimate> {
    let first_scan = first_messages(recording, &inputs.lidar, 1, dimos_recording::decode_point_cloud2)?;
    let (_, first_scan) = first_scan.first().with_context(|| format!("no readable scan on {}", inputs.lidar))?;
    let lidar_frame = first_scan.header.frame_id.trim_start_matches('/').to_string();
    let imu_head = first_messages(recording, &inputs.imu, UNIT_SAMPLES, dimos_recording::decode_imu)?;
    let (_, first_imu) = imu_head.first().with_context(|| format!("no readable message on {}", inputs.imu))?;
    let imu_frame = first_imu.header.frame_id.trim_start_matches('/').to_string();
    let norm = imu_head.iter().map(|(_, m)| m.linear_acceleration.iter().map(|a| a * a).sum::<f64>().sqrt()).sum::<f64>() / imu_head.len() as f64;
    let acc_in_ms2 = norm > METRES_PER_SECOND_SQUARED_ABOVE;
    let to_ms2 = if acc_in_ms2 { 1.0 } else { GRAVITY };
    let point_times = lidar_scan(first_scan, "").map(|scan| !scan.offsets_ns.is_empty()).unwrap_or(false);

    let from_tf = (lidar_frame != imu_frame).then(|| tree.lookup(&imu_frame, &lidar_frame, first_scan.header.ts(), f64::INFINITY)).flatten();
    let extrinsic = from_tf.unwrap_or_else(|| iso(MID360_LIDAR_IN_IMU, [0.0, 0.0, 0.0, 1.0]));
    // the filter's own frame names: the IMU is its body, the lidar mounted in it by the extrinsic (a lidar and IMU
    // both stamped with one frame are still two places)
    let slam_lidar = if lidar_frame.is_empty() { "lidar".to_string() } else { lidar_frame.clone() };
    let slam_imu = if imu_frame.is_empty() || imu_frame == slam_lidar { format!("{slam_lidar}_imu") } else { imu_frame.clone() };
    let mut config = config.clone();
    config.odom_frame_id = ODOM_FRAME.to_string();
    config.output_frame_id = slam_imu.clone();
    if config.imus.is_empty() {
        config.imus.push(ImuConfig::default());
    }
    if config.lidars.is_empty() {
        config.lidars.push(LidarConfig::default());
    }
    config.imus.truncate(1);
    config.lidars.truncate(1);
    config.imus[0].frame_id = slam_imu.clone();
    config.lidars[0].frame_id = slam_lidar.clone();
    let mut slam = DimSlamCore::new(config).map_err(|e| anyhow::anyhow!("dimSLAM's config: {e}"))?;
    // tf as the filter asks it: the lidar ← its body (the IMU)
    let imu_in_lidar = to_slam(&extrinsic.inverse());
    let mount = |parent: &str, child: &str| (parent == slam_lidar && child == slam_imu).then_some(imu_in_lidar);
    let lio = LioConfig::default();

    let mut spool = spool.map(|path| std::fs::File::create(path).map(BufWriter::new)).transpose().context("the deskewed scans' spool")?;
    let deskewing = spool.is_some();
    let mut waiting: VecDeque<Pending> = VecDeque::new();
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
    // the newest pose the filter published, placed with its scan (deskewed onto the spool)
    let mut take = |slam: &mut DimSlamCore, waiting: &mut VecDeque<Pending>, poses: &mut Vec<(f64, Iso)>, path_length: &mut f64, max_speed: &mut f64| -> Result<()> {
        let Some(published) = slam.maybe_publish() else { return Ok(()) };
        let at = published.timestamp_ns;
        let ts = at as f64 / 1e9;
        let pose = from_slam(&published.pose);
        if let Some(w) = spool.as_mut() {
            // its scan: the one ending nearest the pose, of those begun by then (any before it got no pose)
            let mine = waiting.iter().enumerate().filter(|(_, s)| s.begin <= at).min_by_key(|(_, s)| (s.end - at).abs()).map(|(i, _)| i);
            if let Some(index) = mine {
                waiting.drain(..index);
                let scan = waiting.pop_front().expect("found above");
                let span = (scan.end - scan.begin) as f64 / 1e9;
                // the previous pose interpolates across this scan when it is at most a scan back from its start
                let previous = poses.last().filter(|(t, _)| span > 0.0 && ts - t <= 2.0 * span).map(|(t, p)| (p, ts - t));
                let (l, a) = (published.twist.linear, published.twist.angular);
                let twist = (Vector3::new(l.x, l.y, l.z), Vector3::new(a.x, a.y, a.z));
                spool_scan(w, ts, &deskew(&scan, &pose, previous, twist, &extrinsic))?;
            } else {
                spool_scan(w, ts, &[])?;
            }
        }
        if let Some((t, last)) = poses.last() {
            let step = (pose.translation.vector - last.translation.vector).norm();
            *path_length += step;
            if ts > *t {
                *max_speed = max_speed.max(step / (ts - t));
            }
        }
        poses.push((ts, pose));
        Ok(())
    };
    let on_progress = |done: u64, total: u64| progress.set((done, total));
    recording.for_each_payload_of(&[&inputs.lidar, &inputs.imu], Some(&on_progress), |which, log, encoding, payload| {
        if cancel.load(Ordering::Relaxed) {
            failure = Some(anyhow::anyhow!(Cancelled));
            return Ok(false);
        }
        if which == 1 {
            let Ok(imu) = dimos_recording::decode_imu(encoding, payload) else { return Ok(true) };
            let [gx, gy, gz] = imu.angular_velocity;
            let [ax, ay, az] = imu.linear_acceleration.map(|a| a * to_ms2);
            let sample = ImuSample { timestamp_ns: stamp_ns(&imu.header), frame_id: slam_imu.clone(), angular_velocity: slam_na::Vector3::new(gx, gy, gz), linear_acceleration: slam_na::Vector3::new(ax, ay, az) };
            slam.handle_imu(&sample, &mount);
        } else {
            let Ok(cloud) = dimos_recording::decode_point_cloud2(encoding, payload) else { return Ok(true) };
            let Ok(scan) = lidar_scan(&cloud, &slam_lidar) else { return Ok(true) };
            offsets.push((log, cloud.header.ts()));
            if deskewing {
                let held = pending(&scan, &lio);
                let newest = held.begin;
                waiting.push_back(held);
                while waiting.front().is_some_and(|s| (newest - s.begin) as f64 / 1e9 > PENDING_SECONDS) {
                    waiting.pop_front();
                }
            }
            slam.handle_cloud(&scan, &mount);
            scans += 1;
        }
        if let Err(error) = take(&mut slam, &mut waiting, &mut poses, &mut path_length, &mut max_speed) {
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
    drop(take);
    if let Some(mut w) = spool {
        w.flush()?;
    }
    if poses.is_empty() {
        bail!("dimSLAM made no poses from {} + {} ({scans} scans): is {} the lidar the IMU {} is in?", inputs.lidar, inputs.imu, inputs.lidar, inputs.imu);
    }
    Ok(Estimate {
        inputs: inputs.clone(),
        lidar_frame,
        imu_frame,
        extrinsic,
        extrinsic_from_tf: from_tf.is_some(),
        acc_in_ms2,
        point_times,
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

/// The edge the odometry goes on: odom → the root of the lidar's static tree (dimslam_odom when that root is
/// called odom), and root ← lidar.
fn edge_for(tree: &TfTree, lidar_frame: &str) -> (String, String, Iso) {
    let root = crate::frames::root_of(tree, lidar_frame).unwrap_or_else(|| lidar_frame.to_string());
    let lidar_in_root = tree.lookup(&root, lidar_frame, 0.0, f64::INFINITY).unwrap_or_else(Iso::identity);
    let parent = if root == ODOM_FRAME { "dimslam_odom".to_string() } else { ODOM_FRAME.to_string() };
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
            // both replaced: scans deskewed along the old poses don't go with new ones
            let mut appender = dimos_recording::mcap_io::replacing(path, &[ODOMETRY_TOPIC, CLOUD_TOPIC], &[])?;
            let metadata = |kind: &str| BTreeMap::from([(LCM_TYPE_KEY.to_string(), kind.to_string()), (GENERATION_KEY.to_string(), format!("{generation:.6}"))]);
            let odometry_channel = appender.channel(ODOMETRY_TOPIC, 0, "lcm", &metadata(lcm::ODOMETRY_TYPE));
            let mut cloud_metadata = metadata(lcm::POINT_CLOUD2_TYPE);
            cloud_metadata.insert(DERIVED_FROM_KEY.to_string(), estimate.inputs.lidar.clone());
            let cloud_channel = spool.map(|_| appender.channel(CLOUD_TOPIC, 0, "lcm", &cloud_metadata));
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
    fn a_lidar_and_an_imu_with_no_odometry_get_dimslam() {
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
        let mut copy = vec![stream("/imu", "sensor_msgs.Imu", 10), stream("/dimslam_lidar", "sensor_msgs.PointCloud2", 10)];
        copy[1].derived_from = Some("/lidar".into());
        assert_eq!(inputs(&copy, any), None);
    }

    #[test]
    fn tf_that_moves_the_lidar_needs_no_dimslam() {
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
            point_times: true,
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
        let scan = lidar_scan(&livox, "lidar").unwrap();
        assert_eq!((scan.points[0], scan.intensities[0], scan.tags[0], scan.offsets_ns[0], scan.timestamp_ns), ([1.0, 2.0, 3.0], 17.0, 0x10, 5_000_000, 100_000_000_000));
        // only the absolute timestamp, in ns
        let absolute = cloud(&[("x", 0, 7), ("y", 4, 7), ("z", 8, 7), ("timestamp", 16, 8)], 24, &[[xyz(1.0), vec![0; 4], (100.002e9f64).to_le_bytes().to_vec()].concat()]);
        assert!((lidar_scan(&absolute, "lidar").unwrap().offsets_ns[0] as i64 - 2_000_000).abs() < 1000);
        // a relative time in seconds; none at all
        let relative = cloud(&[("x", 0, 7), ("y", 4, 7), ("z", 8, 7), ("time", 12, 7)], 16, &[[xyz(1.0), 0.01f32.to_le_bytes().to_vec()].concat()]);
        assert!((lidar_scan(&relative, "lidar").unwrap().offsets_ns[0] as i64 - 10_000_000).abs() < 1000);
        let bare = lidar_scan(&cloud(&[("x", 0, 7), ("y", 4, 7), ("z", 8, 7)], 12, &[xyz(1.0)]), "lidar").unwrap();
        assert!(bare.offsets_ns.is_empty() && bare.tags.is_empty() && bare.intensities.is_empty());
    }

    /// a wall 2 m ahead seen while the rig walks toward it at 1 m/s (the IMU 10 cm behind the lidar): a point taken
    /// halfway through the scan is 5 cm further from where the lidar is at the scan's end, along the poses or the twist
    #[test]
    fn a_scans_points_move_to_its_end() {
        let extrinsic = iso([0.1, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]);
        let scan = Pending { begin: 0, end: 100_000_000, points: vec![([2.0, 0.0, 0.0, 5.0], 50_000_000), ([2.0, 0.0, 0.0, 5.0], 100_000_000)] };
        let end = iso([1.1, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]);
        let start = iso([1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]);
        let still = (Vector3::zeros(), Vector3::zeros());
        for (previous, twist) in [(Some((&start, 0.1)), still), (None, (Vector3::new(1.0, 0.0, 0.0), Vector3::zeros()))] {
            let moved = deskew(&scan, &end, previous, twist, &extrinsic);
            assert!((moved[0][0] - 1.95).abs() < 1e-5 && moved[0][1].abs() < 1e-6 && moved[0][3] == 5.0, "{moved:?}");
            assert!((moved[1][0] - 2.0).abs() < 1e-6, "the scan's last point is already at its end");
        }
        // turning: 90°/s about z, the IMU at the lidar: a point 50 ms back turns 4.5° back
        let turned = iso([0.0; 3], [0.0, 0.0, (0.05f64 * std::f64::consts::FRAC_PI_2).sin(), (0.05f64 * std::f64::consts::FRAC_PI_2).cos()]);
        let moved = deskew(&scan, &turned, Some((&Iso::identity(), 0.1)), still, &Iso::identity());
        let angle = (moved[0][1] as f64).atan2(moved[0][0] as f64).to_degrees();
        assert!((angle + 4.5).abs() < 1e-3, "{angle}");
    }
}
