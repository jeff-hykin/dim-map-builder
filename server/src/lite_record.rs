//! A recording with a lidar and an IMU but no odometry (lite_record's raw recordings) gets lite_record's own
//! post-processing before a map can be built from it: `lite_record post_process <file>`, the command its
//! Post process button runs. It works on the recording in place: images recoded into viewable codecs, the sensors'
//! frame tree on /tf, odometry, a motion-compensated copy of the lidar (its channel names the raw one in
//! `derived_from`) and a voxel map, appended. Nothing here knows a sensor or a topic: which streams to use is
//! lite_record's rule (a cloud and an IMU under one prefix), and the map build picks the corrected copy by its
//! metadata. The binary is lite_record's release, fetched into the data dir and re-fetched when the release is
//! rebuilt, or LITE_RECORD_BIN.
use anyhow::{bail, Context, Result};
use dimos_recording::{Kind, StreamInfo};
use mapping::build::{Cancelled, Progress};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const REPO: &str = "jeff-hykin/lite_record";
/// how often the release is checked for a new build (each Generate that needs it, at most this often)
const CHECK_EVERY: Duration = Duration::from_secs(600);

/// Why lite_record's post_process should run, or None: a PointCloud2 and an Imu under the same topic prefix (its own
/// rule for finding a lidar and the IMU inside it), and no odometry anywhere in the recording to place the scans.
pub fn pending(streams: &[StreamInfo]) -> Option<String> {
    let prefix = |name: &str| name.rsplit_once('/').map_or(String::new(), |(prefix, _)| prefix.to_string());
    let is_imu = |s: &StreamInfo| s.type_name.replace("/msg/", ".").replace('/', ".") == "sensor_msgs.Imu";
    let lidar = streams
        .iter()
        .filter(|s| s.kind == Kind::Cloud && s.derived_from.is_none())
        .find(|cloud| streams.iter().any(|imu| is_imu(imu) && prefix(&imu.name) == prefix(&cloud.name)))?;
    if streams.iter().any(|s| s.kind == Kind::Odometry && s.count > 0) {
        return None;
    }
    Some(format!("{} has an IMU beside it and nothing places its scans yet: lite_record estimates the odometry, corrects the scans and builds its voxel map", lidar.name))
}

fn asset() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "lite_record-aarch64-macos",
        ("linux", "x86_64") => "lite_record-x86_64-linux",
        ("linux", "aarch64") => "lite_record-aarch64-linux",
        (os, arch) => bail!("lite_record has no release for {os} {arch}: set LITE_RECORD_BIN to a lite_record binary"),
    })
}

/// LITE_RECORD_BIN, else the release binary in `<data>/bin`, fetched again whenever the `latest` release was rebuilt
/// from another commit (its notes say "Built from <sha>"; the sha fetched is kept beside the binary). Checked at most
/// every CHECK_EVERY; offline, the copy there is is used.
pub fn binary(data_dir: &Path) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("LITE_RECORD_BIN").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    static CHECKED: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let path = data_dir.join("bin").join("lite_record");
    let stamp = path.with_extension("commit");
    let fresh = CHECKED.lock().unwrap().is_some_and(|at| at.elapsed() < CHECK_EVERY);
    if path.is_file() && fresh {
        return Ok(path);
    }
    let have = std::fs::read_to_string(&stamp).unwrap_or_default().trim().to_string();
    let outcome = latest_commit().and_then(|latest| {
        if latest != have || !path.is_file() {
            download(&path)?;
            std::fs::write(&stamp, &latest)?;
        }
        Ok(())
    });
    match outcome {
        Ok(()) => {
            *CHECKED.lock().unwrap() = Some(std::time::Instant::now());
            Ok(path)
        }
        Err(error) if path.is_file() => {
            eprintln!("map builder: couldn't check lite_record's release ({error:#}); using the copy there is");
            Ok(path)
        }
        Err(error) => Err(error),
    }
}

/// The commit the `latest` release was built from.
fn latest_commit() -> Result<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/tags/latest");
    let output = Command::new("curl").args(["-fsSL", "--max-time", "10", "-H", "Accept: application/vnd.github+json"]).arg(&url).output().context("curl")?;
    if !output.status.success() {
        bail!("couldn't read {url}");
    }
    let release: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    commit_of(release["body"].as_str().unwrap_or_default()).with_context(|| format!("no \"Built from <sha>\" in {url}"))
}

fn commit_of(notes: &str) -> Option<String> {
    let sha = notes.split("Built from").nth(1)?.split_whitespace().next()?;
    (sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit())).then(|| sha.to_string())
}

fn download(path: &Path) -> Result<()> {
    let url = format!("https://github.com/{REPO}/releases/download/latest/{}", asset()?);
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
    remove_beside(recording);
    if killed || (cancel.load(Ordering::Relaxed) && status.success()) {
        return Err(anyhow::anyhow!(Cancelled));
    }
    if !status.success() {
        let tail: Vec<&str> = errors.lines().chain(output.lines()).filter(|l| !l.trim().is_empty()).collect();
        bail!("lite_record post_process failed ({status}): {}", tail[tail.len().saturating_sub(6)..].join(" / "));
    }
    Ok(output)
}

/// Removes what post_process leaves next to the recording: the voxel map again as a .pc2.lcm (the same map is in the
/// recording, and a copy beside every recording is how a disk fills), and an interrupted run's temporary files.
fn remove_beside(recording: &Path) {
    let mut found = vec![recording.with_extension("pc2.lcm")];
    for suffix in [".converting", ".deskew-spool"] {
        let mut name = recording.as_os_str().to_os_string();
        name.push(suffix);
        found.push(PathBuf::from(name));
    }
    for path in found.into_iter().filter(|path| path.exists()) {
        if let Err(error) = std::fs::remove_file(&path) {
            eprintln!("map builder: couldn't remove {}: {error}", path.display());
        }
    }
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
        StreamInfo { name: name.into(), type_name: type_name.into(), kind: Kind::of(type_name), count, derived_from: None }
    }

    #[test]
    fn a_lidar_with_an_imu_and_no_odometry_is_post_processed() {
        let raw = vec![
            stream("/rig/lidar", "sensor_msgs/msg/PointCloud2", 140),
            stream("/rig/imu", "sensor_msgs/msg/Imu", 2827),
            stream("/camera/imu", "sensor_msgs/msg/Imu", 2832),
            stream("/tf", "tf2_msgs/msg/TFMessage", 71),
        ];
        assert!(pending(&raw).unwrap().contains("/rig/lidar"));
        // once it has odometry there's nothing to do
        let mut processed = raw.clone();
        processed.push(stream("/rig_odometry", "nav_msgs/msg/Odometry", 135));
        assert_eq!(pending(&processed), None);
        // a cloud with no imu under its prefix isn't a lidar this can process
        let lone = vec![stream("/lidar", "sensor_msgs.PointCloud2", 10), stream("/go2/imu", "sensor_msgs.Imu", 10)];
        assert_eq!(pending(&lone), None);
    }

    #[test]
    fn the_release_notes_name_the_commit() {
        assert_eq!(commit_of("Built from fe57ed983c26be0c02e5f5fa7748fdbbc02a58d7").as_deref(), Some("fe57ed983c26be0c02e5f5fa7748fdbbc02a58d7"));
        assert_eq!(commit_of("Built from"), None);
        assert_eq!(commit_of("something else"), None);
    }

    #[test]
    fn post_process_output_becomes_progress() {
        let mut progress = Progress { stage: String::new(), stage_index: 0, stage_count: 1, done: 0, total: 100, note: String::new() };
        assert!(parse_line("[Step 3 of 5] estimating odometry from /rig/lidar + /rig/imu", &mut progress));
        assert_eq!((progress.stage_index, progress.stage_count), (2, 5));
        assert_eq!(progress.stage, "lite_record: estimating odometry from /rig/lidar + /rig/imu");
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
        let recording = dir.path().join("x.mcap");
        std::fs::write(dir.path().join("x.pc2.lcm"), b"map").unwrap();
        let output = post_process(&fake, &recording, &|p| stages.lock().unwrap().push(p.stage), &AtomicBool::new(false)).unwrap();
        assert!(!dir.path().join("x.pc2.lcm").exists(), "nothing is left beside the recording");
        assert!(stages.lock().unwrap().contains(&"lite_record: building the voxel map".to_string()));
        assert_eq!(summary_lines(&output), ["lite_record: 295876 voxels at 0.04 m from 135 scans -> /global_map and x.pc2.lcm"]);
        std::fs::write(&fake, "#!/bin/sh\necho 'Error: no summary' >&2\nexit 1\n").unwrap();
        let error = post_process(&fake, Path::new("x.mcap"), &|_| {}, &AtomicBool::new(false)).unwrap_err();
        assert!(format!("{error:#}").contains("no summary"));
    }
}
