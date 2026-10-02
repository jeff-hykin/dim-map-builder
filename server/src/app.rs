//! The server's state: open workspaces (one per recording), background jobs (build / preview / save) with progress,
//! ETA and cancel, the event stream the page listens to, and the page's last reported view (what the agent "sees").
//! Every mutation autosaves the session to disk before it's acknowledged.
use crate::persist;
use crate::session::{self, BuildSummary, MapData, Session, Store};
use crate::workspace::{now_seconds, Workspace};
use anyhow::{bail, Context, Result};
use dimos_recording::Recording;
use mapping::build::{BuildOptions, Cancelled, Progress};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: u64,
    pub session: String,
    /// "build", "preview", "save"
    pub kind: String,
    /// "running", "done", "failed", "cancelled"
    pub state: String,
    pub progress: Option<Progress>,
    /// 0..1 over every stage
    pub fraction: f64,
    pub started_at: f64,
    pub elapsed: f64,
    pub eta_seconds: Option<f64>,
    pub error: Option<String>,
    #[serde(skip)]
    pub cancel: Arc<AtomicBool>,
}

pub struct App {
    pub store: Store,
    pub recordings_dir: PathBuf,
    workspaces: Mutex<HashMap<String, Arc<Mutex<Workspace>>>>,
    jobs: Mutex<HashMap<String, Job>>,
    previews: Mutex<HashMap<String, Arc<mapping::build::Preview>>>,
    pub events: broadcast::Sender<Value>,
    /// the session a page has open now (the agent's default target)
    pub active: Mutex<Option<String>>,
    captures: Mutex<HashMap<u64, oneshot::Sender<String>>>,
    next_job: AtomicU64,
    next_capture: AtomicU64,
}

/// Overall progress of a job from its stage and the stage's done/total. Build stages aren't equal: loop closure and
/// ray tracing each read every scan; reading tf and the normals are quick.
fn fraction(kind: &str, progress: &Progress) -> f64 {
    let within = if progress.total > 0 { (progress.done as f64 / progress.total as f64).clamp(0.0, 1.0) } else { 0.0 };
    let weights: &[f64] = match (kind, progress.stage_count) {
        ("build", 4) => &[0.02, 0.43, 0.5, 0.05],
        ("build", 3) => &[0.03, 0.9, 0.07],
        _ => &[1.0],
    };
    let index = progress.stage_index.min(weights.len() - 1);
    weights[..index].iter().sum::<f64>() + weights[index] * within
}

impl App {
    pub fn new(data_dir: PathBuf, recordings_dir: PathBuf) -> Arc<App> {
        Arc::new(App {
            store: Store::new(data_dir),
            recordings_dir,
            workspaces: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            previews: Mutex::new(HashMap::new()),
            events: broadcast::channel(256).0,
            active: Mutex::new(None),
            captures: Mutex::new(HashMap::new()),
            next_job: AtomicU64::new(1),
            next_capture: AtomicU64::new(1),
        })
    }

    pub fn emit(&self, event: Value) {
        let _ = self.events.send(event);
    }

    /// Opens (or resumes) the session for a recording. A recording with an earlier save and no session restores it.
    pub fn open(&self, recording_id: &str, path: &str, name: &str, writable: bool) -> Result<Arc<Mutex<Workspace>>> {
        let path = PathBuf::from(path);
        dimos_recording::format_of(&path)?;
        if !path.is_file() {
            bail!("no such recording file: {}", path.display());
        }
        let id = session::session_id(&path);
        let workspace = match self.workspace(&id)? {
            Some(workspace) => workspace,
            None => {
                let fresh = Session {
                    version: session::SESSION_VERSION,
                    id: id.clone(),
                    recording_id: recording_id.into(),
                    recording_path: path.display().to_string(),
                    writable,
                    name: if name.is_empty() { path.file_name().unwrap_or_default().to_string_lossy().to_string() } else { name.into() },
                    stage: "raw".into(),
                    history: vec![format!("Opened {}", path.display())],
                    ..Default::default()
                };
                let recording = Recording::open(&path)?;
                let (session, map) = match persist::load(&recording, fresh.clone()) {
                    Ok(Some((session, map))) => (Session { saved_revision: 0, revision: 0, ..session }, Some(map)),
                    Ok(None) => (fresh, None),
                    Err(error) => {
                        let mut session = fresh;
                        session.history.push(format!("Couldn't restore the saved map: {error:#}"));
                        (session, None)
                    }
                };
                self.store.save(&session)?;
                if let Some(map) = &map {
                    self.store.save_map(&id, map)?;
                }
                let workspace = Arc::new(Mutex::new(Workspace::new(session, map)));
                self.workspaces.lock().unwrap().insert(id.clone(), workspace.clone());
                workspace
            }
        };
        self.store.set_last_open(&id);
        *self.active.lock().unwrap() = Some(id);
        Ok(workspace)
    }

    /// A session from memory, else from disk.
    pub fn workspace(&self, id: &str) -> Result<Option<Arc<Mutex<Workspace>>>> {
        if let Some(workspace) = self.workspaces.lock().unwrap().get(id) {
            return Ok(Some(workspace.clone()));
        }
        let Some(session) = self.store.load(id)? else { return Ok(None) };
        let map = self.store.load_map(id)?;
        let workspace = Arc::new(Mutex::new(Workspace::new(session, map)));
        self.workspaces.lock().unwrap().insert(id.into(), workspace.clone());
        Ok(Some(workspace))
    }

    pub fn require(&self, id: &str) -> Result<Arc<Mutex<Workspace>>> {
        self.workspace(id)?.with_context(|| format!("no session {id}"))
    }

    /// The agent's target: the session given, else the one a page has open.
    pub fn target(&self, id: Option<&str>) -> Result<(String, Arc<Mutex<Workspace>>)> {
        let id = match id.filter(|id| !id.is_empty()) {
            Some(id) => id.to_string(),
            None => self.active.lock().unwrap().clone().or_else(|| self.store.last_open()).context("no recording is open in the Map Builder")?,
        };
        let workspace = self.require(&id)?;
        Ok((id, workspace))
    }

    /// Run `change` on a session; on success autosave it (and the deletion mask) and tell pages.
    pub fn mutate<T>(&self, id: &str, change: impl FnOnce(&mut Workspace) -> Result<T>) -> Result<T> {
        let workspace = self.require(id)?;
        let mut workspace = workspace.lock().unwrap();
        if self.jobs.lock().unwrap().get(id).is_some_and(|job| job.state == "running" && job.kind != "preview") {
            bail!("wait for the running job to finish (or cancel it)");
        }
        let before = workspace.session.revision;
        let result = change(&mut workspace)?;
        if workspace.session.revision != before {
            self.store.save(&workspace.session)?;
            if let Some(map) = &workspace.map {
                self.store.save_removed(id, &map.removed)?;
            }
            self.emit(json!({ "type": "session", "id": id, "revision": workspace.session.revision }));
        }
        Ok(result)
    }

    /// The page's UI state (camera, panels...): saved, but not an edit (no revision, no undo).
    pub fn set_view(&self, id: &str, view: Value) -> Result<()> {
        let workspace = self.require(id)?;
        let mut workspace = workspace.lock().unwrap();
        workspace.session.view = view;
        self.store.save(&workspace.session)?;
        *self.active.lock().unwrap() = Some(id.to_string());
        Ok(())
    }

    pub fn job(&self, id: &str) -> Option<Job> {
        self.jobs.lock().unwrap().get(id).cloned().map(|mut job| {
            if job.state == "running" {
                job.elapsed = now_seconds() - job.started_at;
            }
            job
        })
    }

    pub fn cancel(&self, id: &str) -> bool {
        match self.jobs.lock().unwrap().get(id) {
            Some(job) if job.state == "running" => {
                job.cancel.store(true, Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }

    fn start_job(self: &Arc<Self>, id: &str, kind: &str, work: impl FnOnce(&Arc<App>, &dyn Fn(Progress), &AtomicBool) -> Result<()> + Send + 'static) -> Result<Job> {
        {
            let jobs = self.jobs.lock().unwrap();
            if let Some(job) = jobs.get(id).filter(|job| job.state == "running") {
                bail!("a {} job is already running", job.kind);
            }
        }
        let job = Job {
            id: self.next_job.fetch_add(1, Ordering::Relaxed),
            session: id.into(),
            kind: kind.into(),
            state: "running".into(),
            progress: None,
            fraction: 0.0,
            started_at: now_seconds(),
            elapsed: 0.0,
            eta_seconds: None,
            error: None,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        self.jobs.lock().unwrap().insert(id.into(), job.clone());
        self.emit(json!({ "type": "job", "job": job }));
        let app = self.clone();
        let session = id.to_string();
        let cancel = job.cancel.clone();
        let kind = kind.to_string();
        tokio::task::spawn_blocking(move || {
            let last_emit = Mutex::new(0.0f64);
            let report = |progress: Progress| {
                let mut jobs = app.jobs.lock().unwrap();
                let Some(job) = jobs.get_mut(&session) else { return };
                let fraction = fraction(&kind, &progress).max(job.fraction);
                job.fraction = fraction;
                job.elapsed = now_seconds() - job.started_at;
                job.eta_seconds = (fraction > 0.02 && job.elapsed > 1.0).then(|| job.elapsed * (1.0 - fraction) / fraction);
                job.progress = Some(progress);
                let now = now_seconds();
                let mut last = last_emit.lock().unwrap();
                if now - *last > 0.25 {
                    *last = now;
                    app.emit(json!({ "type": "job", "job": job.clone() }));
                }
            };
            let outcome = work(&app, &report, &cancel);
            let mut jobs = app.jobs.lock().unwrap();
            if let Some(job) = jobs.get_mut(&session) {
                job.elapsed = now_seconds() - job.started_at;
                job.eta_seconds = None;
                match outcome {
                    Ok(()) => {
                        job.state = "done".into();
                        job.fraction = 1.0;
                    }
                    Err(error) if error.is::<Cancelled>() => job.state = "cancelled".into(),
                    Err(error) => {
                        job.state = "failed".into();
                        job.error = Some(format!("{error:#}"));
                    }
                }
                app.emit(json!({ "type": "job", "job": job.clone() }));
            }
        });
        Ok(job)
    }

    /// Build the global map in the background. The current map (and edits) stay until the new one is done; a
    /// cancelled or failed build leaves the session exactly as it was.
    pub fn build(self: &Arc<Self>, id: &str, options: BuildOptions) -> Result<Job> {
        let path = self.require(id)?.lock().unwrap().session.recording_path.clone();
        self.start_job(id, "build", move |app, report, cancel| {
            let started = now_seconds();
            let recording = Recording::open(std::path::Path::new(&path))?;
            let mut progress = |p: Progress| report(p);
            let result = mapping::build::build(&recording, &options, &mut progress, cancel)?;
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!(Cancelled));
            }
            let n = result.points.len();
            let map = MapData {
                voxel_size: result.voxel_size,
                points: result.points,
                normals: result.normals,
                removed: vec![false; n],
                raw_path: result.raw_path,
                corrected_path: result.corrected_path,
                loops: result.loops,
            };
            let summary = BuildSummary {
                voxel_size: result.voxel_size,
                world_frame: result.world_frame,
                cloud_stream: result.cloud_stream,
                scans_used: result.scans_used,
                scans_skipped: result.scans_skipped,
                loops: map.loops.len(),
                notes: result.notes,
                seconds: now_seconds() - started,
            };
            let workspace = app.require(&session_of(&path))?;
            let mut workspace = workspace.lock().unwrap();
            app.store.save_map(&workspace.session.id, &map)?;
            workspace.set_map(map);
            let session = &mut workspace.session;
            session.build = Some(summary.clone());
            session.build_options = Some(options);
            // a new map: old deletions/plans don't apply to it (annotations stay: they're in the map frame)
            session.undo.clear();
            session.redo.clear();
            session.plans.clear();
            session.revision += 1;
            session.history.push(format!("Built the global map: {n} voxels, {} loop closures, {} scans in {:.0} s", summary.loops, summary.scans_used, summary.seconds));
            app.store.save(session)?;
            app.emit(json!({ "type": "session", "id": session.id, "revision": session.revision }));
            Ok(())
        })
    }

    pub fn preview(self: &Arc<Self>, id: &str) -> Result<Option<Arc<mapping::build::Preview>>> {
        if let Some(preview) = self.previews.lock().unwrap().get(id) {
            return Ok(Some(preview.clone()));
        }
        let path = self.require(id)?.lock().unwrap().session.recording_path.clone();
        let session = id.to_string();
        let running = self.job(id).is_some_and(|job| job.state == "running");
        if !running {
            self.start_job(id, "preview", move |app, report, cancel| {
                let recording = Recording::open(std::path::Path::new(&path))?;
                let mut progress = |p: Progress| report(p);
                let preview = mapping::build::preview(&recording, &BuildOptions::default(), 300, 0.1, &mut progress, cancel)?;
                app.previews.lock().unwrap().insert(session.clone(), Arc::new(preview));
                app.emit(json!({ "type": "preview", "id": session }));
                Ok(())
            })?;
        }
        Ok(None)
    }

    pub fn save(self: &Arc<Self>, id: &str) -> Result<Job> {
        let workspace = self.require(id)?;
        if workspace.lock().unwrap().map.is_none() {
            bail!("nothing to save yet: build the global map first");
        }
        let session = id.to_string();
        self.start_job(id, "save", move |app, report, _cancel| {
            let shared = app.require(&session)?;
            // a recording in a read-only folder is copied into the recordings folder first; the session moves to it
            let (writable, source) = {
                let ws = shared.lock().unwrap();
                (ws.session.writable, ws.session.recording_path.clone())
            };
            if !writable {
                let source = std::path::PathBuf::from(&source);
                let dir = app.recordings_dir.join("map-builder");
                std::fs::create_dir_all(&dir)?;
                let stem = source.file_stem().unwrap_or_default().to_string_lossy().to_string();
                let extension = source.extension().unwrap_or_default().to_string_lossy().to_string();
                let mut target = dir.join(format!("{stem}.{extension}"));
                let mut n = 2;
                while target.exists() {
                    target = dir.join(format!("{stem}-{n}.{extension}"));
                    n += 1;
                }
                report(Progress { stage: "Copying the recording into the recordings folder".into(), stage_index: 0, stage_count: 1, done: 0, total: 1, note: target.display().to_string() });
                std::fs::copy(&source, &target).with_context(|| format!("copying {} to {}", source.display(), target.display()))?;
                let mut ws = shared.lock().unwrap();
                ws.session.recording_path = target.display().to_string();
                ws.session.recording_id = format!("map-builder/{}", target.file_name().unwrap_or_default().to_string_lossy());
                ws.session.writable = true;
                ws.session.history.push(format!("{} is read-only: saving into a copy, {}", source.display(), target.display()));
            }
            // a snapshot, so the page can keep reading while a big mcap is rewritten
            let snapshot = shared.lock().unwrap().clone();
            persist::save(&snapshot, |done, total| {
                report(Progress { stage: "Writing into the recording".into(), stage_index: 0, stage_count: 1, done, total, note: String::new() })
            })?;
            let mut workspace = shared.lock().unwrap();
            let session = &mut workspace.session;
            session.saved_revision = snapshot.session.revision;
            session.saved_at = Some(now_seconds());
            session.history.push(format!("Saved into {}", session.recording_path));
            app.store.save(session)?;
            app.emit(json!({ "type": "session", "id": session.id, "revision": session.revision }));
            Ok(())
        })
    }

    /// Start over: forget the session (the recording is untouched).
    pub fn discard(&self, id: &str) -> Result<()> {
        self.cancel(id);
        self.workspaces.lock().unwrap().remove(id);
        self.previews.lock().unwrap().remove(id);
        self.jobs.lock().unwrap().remove(id);
        self.store.delete(id)?;
        let mut active = self.active.lock().unwrap();
        if active.as_deref() == Some(id) {
            *active = None;
        }
        self.store.clear_last_open(id);
        self.emit(json!({ "type": "discarded", "id": id }));
        Ok(())
    }

    /// Ask the page for a screenshot of its 3D view; it answers through `deliver_capture`.
    pub async fn capture(&self, options: Value) -> Result<String> {
        let request = self.next_capture.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.captures.lock().unwrap().insert(request, sender);
        self.emit(json!({ "type": "capture", "request": request, "options": options }));
        match tokio::time::timeout(Duration::from_secs(8), receiver).await {
            Ok(Ok(image)) => Ok(image),
            _ => {
                self.captures.lock().unwrap().remove(&request);
                bail!("no Map Builder page answered (is it open in Desktop?)")
            }
        }
    }

    pub fn deliver_capture(&self, request: u64, image: String) -> bool {
        match self.captures.lock().unwrap().remove(&request) {
            Some(sender) => sender.send(image).is_ok(),
            None => false,
        }
    }
}

fn session_of(path: &str) -> String {
    session::session_id(std::path::Path::new(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_weights_add_up() {
        let at = |stage_index, done, total| fraction("build", &Progress { stage: String::new(), stage_index, stage_count: 4, done, total, note: String::new() });
        assert!((at(3, 1, 1) - 1.0).abs() < 1e-9);
        assert!(at(1, 50, 100) > at(1, 10, 100));
        assert!(at(2, 0, 100) > at(1, 99, 100) - 0.01);
    }
}
