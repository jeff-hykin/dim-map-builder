//! A raw lite_record recording (a Livox lidar + its IMU, nothing placing the scans yet) gets lite_record's own
//! post-processing before a map can be built from it: `lite_record post_process <file>`, the command its Post process
//! button runs. It rewrites the images into viewable codecs, appends the sensors' frame tree to /tf, Point-LIO
//! odometry (/pointlio_odometry, the odom -> body tf edge, /pointlio_path), the motion-compensated /pointlio_lidar and
//! its 4 cm voxel map (/global_map), all into the recording itself. Nothing here reimplements it: the binary is
//! lite_record's release (downloaded once into the data dir), or LITE_RECORD_BIN.
use anyhow::{bail, Context, Result};
use dimos_recording::{Kind, StreamInfo};
use mapping::build::{Cancelled, Progress};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const ODOMETRY_TOPIC: &str = "/pointlio_odometry";
pub const DESKEWED_TOPIC: &str = "/pointlio_lidar";
pub const MAP_TOPIC: &str = "/global_map";
const RELEASE: &str = "https://github.com/jeff-hykin/lite_record/releases/download/latest";
/// a downloaded binary older than this is fetched again (the release is rebuilt on every push to lite_record's main)
const REFRESH_AFTER: Duration = Duration::from_secs(24 * 3600);

/// What lite_record's post_process would add, or None when there's nothing for it to do. Its own rule: a
/// PointCloud2 and an Imu under the same topic prefix (/livox/lidar + /livox/imu), and no odometry or no map yet.
pub fn pending(streams: &[StreamInfo]) -> Option<String> {
    let prefix = |name: &str| name.rsplit_once('/').map_or(String::new(), |(prefix, _)| prefix.to_string());
    let is_imu = |s: &StreamInfo| s.type_name.replace("/msg/", ".").replace('/', ".") == "sensor_msgs.Imu";
    let lidar = streams
        .iter()
        .filter(|s| s.kind == Kind::Cloud && !s.name.contains("map") && s.name != DESKEWED_TOPIC)
        .find(|cloud| streams.iter().any(|imu| is_imu(imu) && prefix(&imu.name) == prefix(&cloud.name)))?;
    let has = |topic: &str| streams.iter().any(|s| s.name == topic && s.count > 0);
    match (has(ODOMETRY_TOPIC), has(MAP_TOPIC)) {
        (false, _) => Some(format!("{} has no odometry yet: lite_record estimates it (Point-LIO), motion-compensates the scans and builds its 4 cm voxel map", lidar.name)),
        (true, false) => Some("lite_record's 4 cm voxel map (/global_map) isn't in it yet".into()),
        (true, true) => None,
    }
}

fn asset() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "lite_record-aarch64-macos",
        ("linux", "x86_64") => "lite_record-x86_64-linux",
        ("linux", "aarch64") => "lite_record-aarch64-linux",
        (os, arch) => bail!("lite_record has no release for {os} {arch}: set LITE_RECORD_BIN to a lite_record binary"),
    })
}

/// LITE_RECORD_BIN, else the release binary in `<data>/bin`, fetched when missing or a day old (a failed refresh keeps
/// the copy there is).
pub fn binary(data_dir: &Path) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("LITE_RECORD_BIN").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let path = data_dir.join("bin").join("lite_record");
    let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
    if age.is_some_and(|age| age < REFRESH_AFTER) {
        return Ok(path);
    }
    match download(&path) {
        Ok(()) => Ok(path),
        Err(error) if path.is_file() => {
            eprintln!("map builder: couldn't refresh lite_record ({error:#}); using the copy from before");
            Ok(path)
        }
        Err(error) => Err(error),
    }
}

fn download(path: &Path) -> Result<()> {
    let url = format!("{RELEASE}/{}", asset()?);
    std::fs::create_dir_all(path.parent().context("bin dir")?)?;
    let partial = path.with_extension("download");
    let status = Command::new("curl").args(["-fsSL", "--retry", "2", "-o"]).arg(&partial).arg(&url).status().context("curl (to download lite_record)")?;
    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        bail!("couldn't download {url} (curl exit {status})");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&partial, path)?;
    Ok(())
}

/// One line of post_process's output, as progress: `[Step i of N] name` starts a stage, `  42%  1m10s  eta ...` is
/// how far through it is (printed every 15 s when not on a terminal).
pub fn parse_line(line: &str, current: &mut Progress) -> bool {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("[Step ") {
        let Some((numbers, name)) = rest.split_once("] ") else { return false };
        let Some((index, count)) = numbers.split_once(" of ") else { return false };
        let (Ok(index), Ok(count)) = (index.trim().parse::<usize>(), count.trim().parse::<usize>()) else { return false };
        *current = Progress { stage: format!("lite_record: {name}"), stage_index: index.saturating_sub(1), stage_count: count.max(1), done: 0, total: 100, note: String::new() };
        return true;
    }
    if let Some((percent, _)) = line.split_once('%') {
        if let Ok(percent) = percent.trim().parse::<f64>() {
            current.done = percent.clamp(0.0, 100.0) as u64;
            current.note = line.to_string();
            return true;
        }
    }
    if line.starts_with("done in") {
        current.done = current.total;
        return true;
    }
    if !line.is_empty() {
        current.note = line.to_string();
        return true;
    }
    false
}

/// The stages a kill would leave half-written: the in-place recode, the append and the map write.
fn writing(stage: &str) -> bool {
    !(stage.contains("reading the recording") || stage.contains("estimating odometry"))
}

/// Runs `lite_record post_process` over `recording`, reporting each stage; returns its output. Cancel stops it while
/// it only reads; once it's writing into the file it finishes first (a kill there would leave the file without its
/// index), then the job ends as cancelled.
pub fn post_process(binary: &Path, recording: &Path, report: &dyn Fn(Progress), cancel: &AtomicBool) -> Result<String> {
    let mut child = Command::new(binary)
        .arg("post_process")
        .arg(recording)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("couldn't run {}", binary.display()))?;
    let stdout = child.stdout.take().context("stdout")?;
    let stderr = child.stderr.take().context("stderr")?;
    let errors = std::thread::spawn(move || std::io::read_to_string(stderr).unwrap_or_default());
    let (lines, receiver) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut output = String::new();
    let mut progress = Progress { stage: "lite_record: starting".into(), stage_index: 0, stage_count: 1, done: 0, total: 100, note: String::new() };
    report(progress.clone());
    let mut killed = false;
    loop {
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                output.push_str(&line);
                output.push('\n');
                if parse_line(&line, &mut progress) {
                    report(progress.clone());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if cancel.load(Ordering::Relaxed) && !killed && !writing(&progress.stage) {
            let _ = child.kill();
            killed = true;
        }
    }
    let status = child.wait()?;
    let errors = errors.join().unwrap_or_default();
    if killed || (cancel.load(Ordering::Relaxed) && status.success()) {
        return Err(anyhow::anyhow!(Cancelled));
    }
    if !status.success() {
        let tail: Vec<&str> = errors.lines().chain(output.lines()).filter(|l| !l.trim().is_empty()).collect();
        bail!("lite_record post_process failed ({status}): {}", tail[tail.len().saturating_sub(6)..].join(" / "));
    }
    Ok(output)
}

/// The figures post_process printed, for the build's notes: odometry, corrected scans and the map.
pub fn summary_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| l.contains(" poses, ") || l.contains("motion-compensated scans") || l.contains(" voxels at ") || l.starts_with("problem:") || l.starts_with("warning:"))
        .map(|l| format!("lite_record: {l}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(name: &str, type_name: &str, count: u64) -> StreamInfo {
        StreamInfo { name: name.into(), type_name: type_name.into(), kind: Kind::of(type_name), count }
    }

    #[test]
    fn a_raw_livox_recording_needs_post_processing_and_a_processed_one_does_not() {
        let raw = vec![
            stream("/livox/lidar", "sensor_msgs/msg/PointCloud2", 140),
            stream("/livox/imu", "sensor_msgs/msg/Imu", 2827),
            stream("/realsense/imu", "sensor_msgs/msg/Imu", 2832),
            stream("/tf_static", "tf2_msgs/msg/TFMessage", 1),
        ];
        assert!(pending(&raw).unwrap().contains("/livox/lidar"));
        let mut processed = raw.clone();
        processed.push(stream(ODOMETRY_TOPIC, "nav_msgs/msg/Odometry", 135));
        processed.push(stream(DESKEWED_TOPIC, "sensor_msgs/msg/PointCloud2", 135));
        assert!(pending(&processed).unwrap().contains("/global_map"));
        processed.push(stream(MAP_TOPIC, "sensor_msgs/msg/PointCloud2", 1));
        assert_eq!(pending(&processed), None);
        // a cloud with no imu beside it (a dimos recording) is not lite_record's to process
        let dimos = vec![stream("/lidar", "sensor_msgs.PointCloud2", 10), stream("/go2/imu", "sensor_msgs.Imu", 10)];
        assert_eq!(pending(&dimos), None);
    }

    #[test]
    fn post_process_output_becomes_progress() {
        let mut progress = Progress { stage: String::new(), stage_index: 0, stage_count: 1, done: 0, total: 100, note: String::new() };
        assert!(parse_line("[Step 3 of 5] estimating odometry from /livox/lidar + /livox/imu", &mut progress));
        assert_eq!((progress.stage_index, progress.stage_count), (2, 5));
        assert_eq!(progress.stage, "lite_record: estimating odometry from /livox/lidar + /livox/imu");
        assert!(parse_line("   42%  1m10s  eta 1m35s  1.4x rt  2.1 GB written", &mut progress));
        assert_eq!(progress.done, 42);
        assert!(parse_line("  done in 2s (6.7x rt)", &mut progress));
        assert_eq!(progress.done, 100);
        assert!(writing("lite_record: appending transforms, odometry and corrected clouds"));
        assert!(!writing(&progress.stage));
    }

    #[test]
    fn runs_a_binary_and_reports_its_stages() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("lite_record");
        std::fs::write(&fake, "#!/bin/sh\necho '[Step 1 of 2] reading the recording'\necho '  done in 0s'\necho '[Step 2 of 2] building the voxel map'\necho '  295876 voxels at 0.04 m from 135 scans -> /global_map and x.pc2.lcm'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let stages = std::sync::Mutex::new(Vec::new());
        let output = post_process(&fake, Path::new("x.mcap"), &|p| stages.lock().unwrap().push(p.stage), &AtomicBool::new(false)).unwrap();
        assert!(stages.lock().unwrap().contains(&"lite_record: building the voxel map".to_string()));
        assert_eq!(summary_lines(&output), ["lite_record: 295876 voxels at 0.04 m from 135 scans -> /global_map and x.pc2.lcm"]);
        std::fs::write(&fake, "#!/bin/sh\necho 'Error: no summary' >&2\nexit 1\n").unwrap();
        let error = post_process(&fake, Path::new("x.mcap"), &|_| {}, &AtomicBool::new(false)).unwrap_err();
        assert!(format!("{error:#}").contains("no summary"));
    }
}
