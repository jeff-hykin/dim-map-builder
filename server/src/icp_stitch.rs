//! The high-quality tf correction runs icp_stitch (github.com/jeff-hykin/icp_stitch: gsc_pgo's offline tag PGO + ICP
//! stitching, in Rust on GTSAM) on a temporary memory2 db the correction writes (mapping::correct): its release
//! binary, fetched into the data dir and re-fetched when the release is rebuilt, or ICP_STITCH_BIN. It only ever touches the temporary db, so Cancel kills it at any point.
use anyhow::{bail, Context, Result};
use mapping::build::{Cancelled, Progress};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const REPO: &str = "jeff-hykin/icp_stitch";
const CHECK_EVERY: Duration = Duration::from_secs(600);

fn asset() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "icp_stitch-aarch64-macos",
        ("linux", "x86_64") => "icp_stitch-x86_64-linux",
        ("linux", "aarch64") => "icp_stitch-aarch64-linux",
        (os, arch) => bail!("icp_stitch has no release for {os} {arch}: set ICP_STITCH_BIN to an icp_stitch binary, or use the fast correction"),
    })
}

/// Whether the high-quality correction can run here: ICP_STITCH_BIN, a copy already fetched, or a release for this
/// platform to fetch.
pub fn available(data_dir: &Path) -> bool {
    std::env::var_os("ICP_STITCH_BIN").is_some_and(|v| !v.is_empty()) || data_dir.join("bin").join("icp_stitch").is_file() || asset().is_ok()
}

/// ICP_STITCH_BIN, else the release binary in `<data>/bin`, fetched again whenever the `latest` release was rebuilt
/// from another commit; offline, the copy there is is used.
pub fn binary(data_dir: &Path) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("ICP_STITCH_BIN").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    static CHECKED: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let path = data_dir.join("bin").join("icp_stitch");
    let stamp = path.with_extension("commit");
    if path.is_file() && CHECKED.lock().unwrap().is_some_and(|at| at.elapsed() < CHECK_EVERY) {
        return Ok(path);
    }
    let have = std::fs::read_to_string(&stamp).unwrap_or_default().trim().to_string();
    let outcome = latest_commit_of(REPO).and_then(|latest| {
        if latest != have || !path.is_file() {
            download_asset(REPO, asset()?, &path)?;
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
            eprintln!("map builder: couldn't check icp_stitch's release ({error:#}); using the copy there is");
            Ok(path)
        }
        Err(error) => Err(error),
    }
}

/// The commit `repo`'s `latest` release was built from ("Built from <sha>" in its notes).
pub fn latest_commit_of(repo: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/tags/latest");
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

/// Downloads `repo`'s `latest` release asset to `path` (executable), through a partial file.
pub fn download_asset(repo: &str, asset: &str, path: &Path) -> Result<()> {
    let url = format!("https://github.com/{repo}/releases/download/latest/{asset}");
    std::fs::create_dir_all(path.parent().context("bin dir")?)?;
    let partial = path.with_extension("download");
    let status = Command::new("curl").args(["-fsSL", "--retry", "2", "-o"]).arg(&partial).arg(&url).status().context("curl (to download a release binary)")?;
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

/// How far through its run icp_stitch is, from what it prints: (stage index of 4, fraction within it, note).
pub fn parse_line(line: &str) -> Option<(usize, f64, String)> {
    let line = line.trim();
    if line.starts_with("building factor graph") || line.starts_with("solving stage 1") {
        return Some((1, 0.0, line.to_string()));
    }
    if line.starts_with("ICP stage: thinned") || line.starts_with("ICP stage: reading") || line.starts_with("ICP stage: built") || line.starts_with("ICP stage:") && line.contains("candidate pairs") {
        return Some((2, 0.05, line.to_string()));
    }
    // "  5000/12000 pairs, 40 accepted, 31s"
    if let Some((done, rest)) = line.split_once('/') {
        if let (Ok(done), Some(total)) = (done.trim().parse::<f64>(), rest.split_whitespace().next().and_then(|t| t.parse::<f64>().ok())) {
            if rest.contains("pairs") && total > 0.0 {
                return Some((2, 0.05 + 0.95 * done / total, line.to_string()));
            }
        }
    }
    if line.starts_with("ICP stage: accepted") || line.starts_with("solving stage 2") {
        return Some((3, 0.0, line.to_string()));
    }
    if line.starts_with("PGO:") || line.starts_with("wrote ") || line.starts_with("writing ") {
        return Some((3, 0.5, line.to_string()));
    }
    None
}

/// Runs icp_stitch with `args`, reporting progress as stages 1..3 of 4 (stage 0 is writing its input); returns its
/// output. The full run (command, stdout, stderr) goes to `log`, which a failure names.
pub fn run(binary: &Path, args: &[String], log: &Path, report: &dyn Fn(Progress), cancel: &AtomicBool) -> Result<String> {
    let mut child = Command::new(binary)
        .args(args)
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
    let names = ["Preparing the scans for icp_stitch", "icp_stitch: tag PGO", "icp_stitch: ICP stitching", "icp_stitch: solving"];
    let mut output = String::new();
    report(Progress { overall: None, stage: names[1].into(), stage_index: 1, stage_count: 4, done: 0, total: 1000, note: String::new() });
    let mut killed = false;
    loop {
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                output.push_str(&line);
                output.push('\n');
                if let Some((stage, fraction, note)) = parse_line(&line) {
                    report(Progress { overall: None, stage: names[stage].into(), stage_index: stage, stage_count: 4, done: (fraction * 1000.0) as u64, total: 1000, note });
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if cancel.load(Ordering::Relaxed) && !killed {
            let _ = child.kill();
            killed = true;
        }
    }
    let status = child.wait()?;
    let errors = errors.join().unwrap_or_default();
    if killed || cancel.load(Ordering::Relaxed) {
        return Err(anyhow::anyhow!(Cancelled));
    }
    let command = std::iter::once(binary.display().to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ");
    let saved = log.parent().map(std::fs::create_dir_all).transpose().and_then(|_| {
        std::fs::write(log, format!("$ {command}\n\n== stdout ==\n{output}\n== stderr ==\n{errors}\n== {status} ==\n"))
    });
    if !status.success() {
        // The reason is at the end of stderr; stdout's tail is only progress.
        let errors: Vec<&str> = errors.lines().filter(|l| !l.trim().is_empty()).collect();
        let reason = if errors.is_empty() {
            let output: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
            output[output.len().saturating_sub(12)..].join("\n")
        } else {
            errors[errors.len().saturating_sub(12)..].join("\n")
        };
        let full = match saved {
            Ok(()) => format!("full log: {}", log.display()),
            Err(e) => format!("couldn't save the full log to {}: {e}", log.display()),
        };
        bail!("icp_stitch failed ({status}):\n{reason}\n({full})");
    }
    Ok(output)
}

/// The figures icp_stitch printed, for the notes.
pub fn summary_lines(output: &str) -> Vec<String> {
    output.lines().map(str::trim).filter(|l| l.starts_with("PGO:") || l.starts_with("ICP stage: accepted") || l.starts_with("tags with no loop-closure")).map(|l| format!("icp_stitch: {l}")).collect()
}

/// How many ICP stitches it accepted.
pub fn accepted(output: &str) -> usize {
    output.lines().find_map(|l| l.trim().strip_prefix("ICP stage: accepted ")).and_then(|rest| rest.split('/').next()).and_then(|n| n.trim().parse().ok()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_notes_name_the_commit() {
        assert_eq!(commit_of("Built from fe57ed983c26be0c02e5f5fa7748fdbbc02a58d7").as_deref(), Some("fe57ed983c26be0c02e5f5fa7748fdbbc02a58d7"));
        assert_eq!(commit_of("Built from"), None);
        assert_eq!(commit_of("something else"), None);
    }

    #[test]
    fn its_output_becomes_progress() {
        assert_eq!(parse_line("solving stage 1 (tag PGO)...").unwrap().0, 1);
        let (stage, fraction, _) = parse_line("  5000/10000 pairs, 40 accepted, 31s").unwrap();
        assert_eq!(stage, 2);
        assert!((fraction - 0.525).abs() < 1e-9);
        assert_eq!(parse_line("solving stage 2 (tag PGO + ICP closures)...").unwrap().0, 3);
        assert_eq!(accepted("ICP stage: accepted 37/120 loop closures\n"), 37);
        assert!(parse_line("recording: /x").is_none());
    }
}
