//! Point-LIO over a recording from the command line, as the Map Editor's step runs it:
//! `cargo run -p mapping --example pointlio -- <recording> [--tum out.tum] [--config '{"blind": 0.7}'] [--write]` (`--write` writes into it in place).
use dimos_recording::Recording;
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = Path::new(args.first().expect("usage: pointlio <recording> [--tum out.tum] [--write]"));
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let write = args.iter().any(|a| a == "--write");
    let started = std::time::Instant::now();
    let recording = Recording::open(path)?;
    let streams = recording.streams()?;
    let inputs = mapping::pointlio::inputs(&streams, |s| s.count > 0).ok_or_else(|| anyhow::anyhow!("no lidar + IMU without odometry here"))?;
    let tree = mapping::frames::tf_tree(&recording)?;
    let spool = std::env::temp_dir().join("pointlio_example.spool");
    let mut report = |p: mapping::build::Progress| eprint!("\r{} {:.0}%  {}      ", p.stage, 100.0 * p.done as f64 / p.total.max(1) as f64, p.note);
    let config: dimos_pointlio::pointlio_core::Config = match flag("--config") {
        Some(json) => serde_json::from_str(&json)?,
        None => mapping::pointlio::config(),
    };
    let estimate = mapping::pointlio::estimate(&recording, &tree, &inputs, &config, write.then_some(spool.as_path()), &mut report, &Default::default())?;
    eprintln!();
    for note in estimate.notes() {
        println!("{note}");
    }
    println!("estimated in {:.1} s, clock offset {:.3} s", started.elapsed().as_secs_f64(), estimate.clock_offset);
    if let Some(tum) = flag("--tum") {
        use std::io::Write;
        let mut out = std::io::BufWriter::new(std::fs::File::create(tum)?);
        for (t, pose) in &estimate.poses {
            let (p, q) = mapping::tf::parts(pose);
            writeln!(out, "{t:.6} {} {} {} {} {} {} {}", p[0], p[1], p[2], q[0], q[1], q[2], q[3])?;
        }
    }
    if write {
        drop(recording);
        let written = mapping::pointlio::write(path, &tree, &estimate, Some(&spool), mapping::pipeline::now())?;
        let _ = std::fs::remove_file(&spool);
        println!("wrote {written:?} in {:.1} s total", started.elapsed().as_secs_f64());
    }
    Ok(())
}
