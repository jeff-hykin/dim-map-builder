//! The server's state: open workspaces (one per recording), background jobs (build / preview / save) with progress,
//! ETA and cancel, the event stream the page listens to, and the page's last reported view (what the agent "sees").
//! Every mutation autosaves the session to disk before it's acknowledged.
use crate::persist;
use crate::session::{self, Session, Store};
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
    /// "open", "read", "build", "preview", "save", "tags", "tf", "globalMap"
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
    /// a build that ran lite_record's post_process first: that took the first 60% of the bar
    #[serde(skip)]
    post_processed: bool,
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
    /// Desktop's own HTTP API (its upload queue), from DIMOS_APP's desktopUrl
    pub desktop_url: std::sync::OnceLock<String>,
    next_job: AtomicU64,
    next_capture: AtomicU64,
}

/// Reading this many bytes of an .mcap's chunks for its tf is a job (the Generate step list's "read" job, or the
/// preview's first stage), not something an HTTP request waits on
pub const SLOW_TF_BYTES: u64 = 1 << 30;
pub const READING_TF: &str = "Reading the tf tree";

/// How much of a build's progress bar lite_record's post_process takes when it runs first (it reads the whole recording
/// two or three times, about what the map build does)
const POST_PROCESS_SHARE: f64 = 0.6;

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
            desktop_url: std::sync::OnceLock::new(),
            next_job: AtomicU64::new(1),
            next_capture: AtomicU64::new(1),
        })
    }

    pub fn emit(&self, event: Value) {
        let _ = self.events.send(event);
    }

    /// Opens (or resumes) the session for a recording. A recording with an earlier save and no session restores it.
    /// Nothing here reads messages: a `.db` lists its streams, an `.mcap` reads its summary (and the chunks holding
    /// an earlier save). An `.mcap` with no summary (cut short) opens at once and is read in an "open" job.
    pub fn open(self: &Arc<Self>, recording_id: &str, path: &str, name: &str, writable: bool) -> Result<Arc<Mutex<Workspace>>> {
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
                let unindexed = dimos_recording::format_of(&path)? == dimos_recording::Format::Mcap && !dimos_recording::mcap_index::is_indexed(&path);
                let (session, map, post_process, recorded_map) = if unindexed { (fresh.clone(), None, None, None) } else { Self::read_for_open(&path, fresh.clone())? };
                self.store.save(&session)?;
                if let Some(map) = &map {
                    self.store.save_map(&id, map)?;
                }
                let mut workspace = Workspace::new(session, map);
                workspace.post_process = post_process;
                workspace.recorded_map = recorded_map;
                let workspace = Arc::new(Mutex::new(workspace));
                self.workspaces.lock().unwrap().insert(id.clone(), workspace.clone());
                if unindexed {
                    self.start_open_job(&id, path.clone(), fresh)?;
                }
                workspace
            }
        };
        self.store.set_last_open(&id);
        *self.active.lock().unwrap() = Some(id);
        Ok(workspace)
    }

    /// What opening reads from the recording: an earlier save to restore (else `fresh`), whether it needs
    /// lite_record's post_process, and its own `global_map` stream if it has one.
    fn read_for_open(path: &std::path::Path, fresh: Session) -> Result<(Session, Option<crate::session::MapData>, Option<String>, Option<String>)> {
        let recording = Recording::open(path)?;
        let post_process = crate::lite_record::pending_with(&recording.stream_list()?, |s| recording.has_messages(&s.name).unwrap_or(false));
        let recorded_map = mapping::recorded::find(&recording)?;
        Ok(match persist::load(&recording, fresh.clone()) {
            Ok(Some((session, map))) => (Session { saved_revision: 0, revision: 0, ..session }, Some(map), post_process, recorded_map),
            Ok(None) => (fresh, None, post_process, recorded_map),
            Err(error) => {
                let mut session = fresh;
                session.history.push(format!("Couldn't restore the saved map: {error:#}"));
                (session, None, post_process, recorded_map)
            }
        })
    }

    /// The recording's own `global_map` stream, for a session restored from disk (None for an .mcap with no index:
    /// listing its streams would read all of it).
    fn recorded_map_of(path: &std::path::Path) -> Option<String> {
        let unindexed = dimos_recording::format_of(path).ok()? == dimos_recording::Format::Mcap && !dimos_recording::mcap_index::is_indexed(path);
        if unindexed || !path.is_file() {
            return None;
        }
        mapping::recorded::find(&Recording::open(path).ok()?).ok().flatten()
    }

    /// Whether a restored session's recording still needs lite_record's post_process (as `read_for_open` decides).
    fn post_process_of(path: &std::path::Path) -> Option<String> {
        let unindexed = dimos_recording::format_of(path).ok()? == dimos_recording::Format::Mcap && !dimos_recording::mcap_index::is_indexed(path);
        if unindexed || !path.is_file() {
            return None;
        }
        let recording = Recording::open(path).ok()?;
        crate::lite_record::pending_with(&recording.stream_list().ok()?, |s| recording.has_messages(&s.name).unwrap_or(false))
    }

    /// An .mcap with no summary: walk it once (bytes read / total, ETA, cancel), then restore an earlier save into the
    /// session if nothing was done to it meanwhile.
    fn start_open_job(self: &Arc<Self>, id: &str, path: PathBuf, fresh: Session) -> Result<Job> {
        let session_id = id.to_string();
        self.start_job(id, "open", move |app, report, cancel| {
            let file = dimos_recording::mcap_io::McapFile::open(&path)?;
            let progress = |done: u64, total: u64| {
                let note = format!("{:.1} of {:.1} GB (the recording has no index: it was cut short)", done as f64 / 1e9, total as f64 / 1e9);
                report(Progress { overall: None, stage: "Reading the recording".into(), stage_index: 0, stage_count: 1, done, total, note })
            };
            if let Err(error) = file.index_with(Some(&progress), Some(cancel)) {
                return Err(if cancel.load(Ordering::Relaxed) { anyhow::anyhow!(Cancelled) } else { error });
            }
            drop(file);
            let (session, map, post_process, recorded_map) = Self::read_for_open(&path, fresh)?;
            let shared = app.require(&session_id)?;
            let mut ws = shared.lock().unwrap();
            if ws.discarded {
                return Ok(());
            }
            if map.is_some() && ws.map.is_none() && ws.session.revision == 0 {
                if let Some(map) = &map {
                    app.store.save_map(&session_id, map)?;
                }
                let view = std::mem::take(&mut ws.session.view);
                *ws = Workspace::new(Session { view, ..session }, map);
            }
            ws.post_process = post_process;
            ws.recorded_map = recorded_map;
            app.store.save(&ws.session)?;
            app.emit(json!({ "type": "session", "id": session_id, "revision": ws.session.revision }));
            Ok(())
        })
    }

    /// A session from memory, else from disk.
    pub fn workspace(&self, id: &str) -> Result<Option<Arc<Mutex<Workspace>>>> {
        if let Some(workspace) = self.workspaces.lock().unwrap().get(id) {
            return Ok(Some(workspace.clone()));
        }
        let Some(mut session) = self.store.load(id)? else { return Ok(None) };
        let map = self.store.load_map(id)?;
        if map.is_none() && session.stage == "map" {
            // its map file is gone (deleted by hand, or a crash between writes): back to "not built", annotations kept
            session.stage = "raw".into();
            session.history.push("The built map's file was missing: build the map again".into());
        }
        let path = std::path::Path::new(&session.recording_path);
        let (recorded_map, post_process) = (Self::recorded_map_of(path), Self::post_process_of(path));
        let mut workspace = Workspace::new(session, map);
        workspace.recorded_map = recorded_map;
        workspace.post_process = post_process;
        let workspace = Arc::new(Mutex::new(workspace));
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
            None => self.active.lock().unwrap().clone().or_else(|| self.store.last_open()).context("no recording is open in the Map Editor")?,
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
        if workspace.discarded {
            bail!("no session {id} (discarded)");
        }
        let before = workspace.session.revision;
        let voxels_before = workspace.map.as_ref().map_or(0, |m| m.points.len());
        let result = change(&mut workspace)?;
        if workspace.session.revision != before {
            self.store.save(&workspace.session)?;
            if let Some(map) = &workspace.map {
                // an edit that added voxels rewrites the map file; otherwise the mask is enough
                if map.points.len() != voxels_before {
                    self.store.save_map(id, map)?;
                } else {
                    self.store.save_removed(id, &map.removed)?;
                }
            }
            self.emit(json!({ "type": "session", "id": id, "revision": workspace.session.revision }));
        }
        Ok(result)
    }

    /// The page's UI state (camera, panels...): saved, but not an edit (no revision, no undo).
    pub fn set_view(&self, id: &str, view: Value) -> Result<()> {
        let workspace = self.require(id)?;
        let mut workspace = workspace.lock().unwrap();
        if workspace.discarded {
            return Ok(());
        }
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

    pub(crate) fn start_job(self: &Arc<Self>, id: &str, kind: &str, work: impl FnOnce(&Arc<App>, &dyn Fn(Progress), &AtomicBool) -> Result<()> + Send + 'static) -> Result<Job> {
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
            post_processed: false,
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
                let lite_record = progress.stage.starts_with("lite_record:");
                job.post_processed |= lite_record;
                let fraction = match progress.overall {
                    Some(overall) => overall,
                    None => match (lite_record, job.post_processed) {
                    (true, _) => POST_PROCESS_SHARE * (progress.stage_index as f64 + fraction("", &progress)) / progress.stage_count.max(1) as f64,
                    (false, true) => POST_PROCESS_SHARE + (1.0 - POST_PROCESS_SHARE) * fraction(&kind, &progress),
                    (false, false) => fraction(&kind, &progress),
                    },
                }
                .max(job.fraction);
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
        let session_id = id.to_string();
        self.start_job(id, "build", move |app, report, cancel| {
            let started = now_seconds();
            let shared = app.require(&session_id)?;
            let mut post_notes = Vec::new();
            let pending = shared.lock().unwrap().post_process.clone();
            if let Some(reason) = pending {
                Self::require_writable(&shared)?;
                let path = shared.lock().unwrap().session.recording_path.clone();
                let binary = crate::lite_record::binary(&app.store.dir)?;
                shared.lock().unwrap().session.history.push(format!("Post-processing with lite_record ({reason})"));
                let output = crate::lite_record::post_process(&binary, std::path::Path::new(&path), report, cancel)?;
                post_notes = crate::lite_record::summary_lines(&output);
                let processed = Recording::open(std::path::Path::new(&path))?;
                let still = crate::lite_record::pending(&processed.streams()?);
                let recorded_map = mapping::recorded::find(&processed)?;
                let mut ws = shared.lock().unwrap();
                ws.post_process = still;
                ws.recorded_map = recorded_map;
                ws.session.history.push(format!("lite_record post-processed {path}"));
                ws.session.history.extend(post_notes.iter().cloned());
                app.store.save(&ws.session)?;
                app.emit(json!({ "type": "session", "id": ws.session.id, "revision": ws.session.revision }));
                app.emit(json!({ "type": "recording-changed", "path": path }));
            }
            let path = shared.lock().unwrap().session.recording_path.clone();
            let recording = Recording::open(std::path::Path::new(&path))?;
            let mut progress = |p: Progress| report(p);
            let result = mapping::build::build(&recording, &options, &mut progress, cancel)?;
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!(Cancelled));
            }
            app.install_map(&session_id, result, options, post_notes, started)
        })
    }

    /// The recording's own `global_map` (its newest message) becomes the session's map in place of Generate:
    /// `stream` (default: the one found when it was opened), thinned to `voxel` when given (mapping::recorded::read).
    /// Nothing is written into the recording, so a read-only one works too.
    pub fn use_recorded_map(self: &Arc<Self>, id: &str, stream: Option<String>, voxel: Option<f32>) -> Result<Job> {
        let shared = self.require(id)?;
        let stream = match stream.filter(|s| !s.is_empty()) {
            Some(stream) => stream,
            None => shared.lock().unwrap().recorded_map.clone().context("this recording has no global_map stream: generate the map instead")?,
        };
        let session_id = id.to_string();
        self.start_job(id, "recordedMap", move |app, report, cancel| {
            let started = now_seconds();
            let path = app.require(&session_id)?.lock().unwrap().session.recording_path.clone();
            report(Progress { overall: Some(0.1), stage: format!("Reading the newest message on {stream}"), stage_index: 0, stage_count: 1, done: 0, total: 1, note: String::new() });
            let result = mapping::recorded::read(&Recording::open(std::path::Path::new(&path))?, &stream, voxel)?;
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!(Cancelled));
            }
            let options = BuildOptions { cloud_stream: stream.clone(), world_frame: result.world_frame.clone(), voxel_size: result.voxel_size, ..Default::default() };
            app.install_map(&session_id, result, options, vec![format!("the map is {stream}'s newest message, not generated")], started)
        })
    }

    /// The raw-recording preview is read again next time (the recording changed).
    pub fn invalidate_preview(&self, id: &str) {
        self.previews.lock().unwrap().remove(id);
    }

    pub fn preview(self: &Arc<Self>, id: &str) -> Result<Option<Arc<mapping::build::Preview>>> {
        if let Some(preview) = self.previews.lock().unwrap().get(id) {
            return Ok(Some(preview.clone()));
        }
        let (path, pending) = {
            let workspace = self.require(id)?;
            let workspace = workspace.lock().unwrap();
            (workspace.session.recording_path.clone(), workspace.post_process.is_some())
        };
        // a raw lite_record recording has nothing placing its scans until Generate post-processes it: no preview
        if pending {
            return Ok(None);
        }
        let session = id.to_string();
        let running = self.job(id).is_some_and(|job| job.state == "running");
        if !running {
            self.start_job(id, "preview", move |app, report, cancel| {
                let recording = Recording::open(std::path::Path::new(&path))?;
                // the tf first, on its own share of the bar (the Generate step list waits for it, not for the scans)
                let tf_share = Self::read_tf(&recording, report, 0.5)?;
                let mut progress = |p: Progress| report(Progress { overall: Some(tf_share + (1.0 - tf_share) * fraction("", &p)), ..p });
                let preview = mapping::build::preview(&recording, &BuildOptions::default(), 300, 0.1, &mut progress, cancel)?;
                app.previews.lock().unwrap().insert(session.clone(), Arc::new(preview));
                app.emit(json!({ "type": "preview", "id": session }));
                Ok(())
            })?;
        }
        Ok(None)
    }

    /// Reads the recording's tf (kept for every later reader of the same file) when that's slow, reporting it as the
    /// first `share` of a job's bar; returns the share it took (0 when there was nothing slow to read).
    pub(crate) fn read_tf(recording: &Recording, report: &dyn Fn(Progress), share: f64) -> Result<f64> {
        if recording.tf_bytes_to_read()? < SLOW_TF_BYTES {
            return Ok(0.0);
        }
        let progress = |done: u64, total: u64| {
            let within = if total > 0 { done as f64 / total as f64 } else { 1.0 };
            let note = format!("{:.1} of {:.1} GB of chunks holding tf", done as f64 / 1e9, total as f64 / 1e9);
            report(Progress { overall: Some(share * within), stage: READING_TF.into(), stage_index: 0, stage_count: 1, done, total, note })
        };
        recording.tf_edges_logged_with(Some(&progress))?;
        Ok(share)
    }

    /// Everything the Map Editor writes goes into the recording in place; one in a read-only folder is refused, never
    /// copied (a copy per edit is how a disk of recordings fills up).
    fn require_writable(shared: &Arc<Mutex<Workspace>>) -> Result<()> {
        let ws = shared.lock().unwrap();
        if !ws.session.writable {
            bail!("{} is in a read-only folder: the Map Editor writes into recordings in place and never copies them; move it into the recordings folder (Recordings can) and open it from there", ws.session.recording_path);
        }
        Ok(())
    }

    pub fn save(self: &Arc<Self>, id: &str) -> Result<Job> {
        let workspace = self.require(id)?;
        if workspace.lock().unwrap().map.is_none() {
            bail!("nothing to save yet: build the global map first");
        }
        let session = id.to_string();
        self.start_job(id, "save", move |app, report, _cancel| {
            let shared = app.require(&session)?;
            Self::require_writable(&shared)?;
            // a snapshot, so the page can keep reading while a big mcap is rewritten
            let snapshot = shared.lock().unwrap().clone();
            persist::save(&snapshot, |done, total| {
                report(Progress { overall: None, stage: "Writing into the recording".into(), stage_index: 0, stage_count: 1, done, total, note: String::new() })
            })?;
            let mut workspace = shared.lock().unwrap();
            if workspace.discarded {
                return Ok(());
            }
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
        if let Some(workspace) = self.workspaces.lock().unwrap().remove(id) {
            // anyone still holding it (a request in flight, a save job) sees it's gone and writes nothing
            workspace.lock().unwrap().discarded = true;
        }
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
                bail!("no Map Editor page answered (is it open in Desktop?)")
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

#[cfg(test)]
mod tests {
    use super::*;

    /// a view update or edit that arrives after Discard (the page's debounced PUT) must not bring the session back
    #[test]
    fn discarded_sessions_stay_gone() {
        let dir = tempfile::tempdir().unwrap();
        let recording = dir.path().join("r.db");
        let mut db = dimos_recording::db::Connection::open(&recording).unwrap();
        dimos_recording::db::write_stream(&mut db, "lidar", "sensor_msgs.PointCloud2", &[]).unwrap();
        drop(db);
        let app = App::new(dir.path().join("data"), dir.path().to_path_buf());
        let held = app.open("r.db", &recording.display().to_string(), "r", true).unwrap();
        let id = held.lock().unwrap().session.id.clone();
        app.discard(&id).unwrap();
        app.set_view(&id, json!({ "late": true })).ok();
        assert!(held.lock().unwrap().discarded);
        assert!(app.store.load(&id).unwrap().is_none(), "the late view update recreated the session");
        assert!(app.store.last_open().is_none());
        assert!(app.mutate(&id, |w| w.add_point("x", [0.0; 3], "user")).is_err());
        assert!(app.store.load(&id).unwrap().is_none());
    }

    /// a raw lidar + IMU recording still offers lite_record's post_process after a restart restores its session
    #[test]
    fn a_restored_session_still_needs_post_process() {
        let dir = tempfile::tempdir().unwrap();
        let recording = dir.path().join("r.db");
        let mut db = dimos_recording::db::Connection::open(&recording).unwrap();
        dimos_recording::db::write_stream(&mut db, "rig/lidar", "sensor_msgs.PointCloud2", &[]).unwrap();
        dimos_recording::db::write_stream(&mut db, "rig/imu", "sensor_msgs.Imu", &[]).unwrap();
        drop(db);
        let opened = App::new(dir.path().join("data"), dir.path().to_path_buf());
        let id = opened.open("r.db", &recording.display().to_string(), "r", true).unwrap().lock().unwrap().session.id.clone();
        assert!(opened.require(&id).unwrap().lock().unwrap().post_process.is_some());
        let restarted = App::new(dir.path().join("data"), dir.path().to_path_buf());
        assert!(restarted.require(&id).unwrap().lock().unwrap().post_process.is_some());
    }

    /// an .mcap with no summary isn't read while opening: open returns, an open job reads it and restores its save
    #[tokio::test(flavor = "multi_thread")]
    async fn an_mcap_cut_short_opens_at_once_and_is_read_in_a_job() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.mcap");
        let cloud = dimos_recording::lcm::encode_xyz_cloud(&dimos_recording::lcm::Header::at(1.0, "lidar"), &[[1.0, 0.0, 0.0]], None);
        dimos_recording::mcap_io::write_fixture(&path, &[("/lidar", "lcm", dimos_recording::lcm::POINT_CLOUD2_TYPE, 1.0, cloud)]);
        let saved = Session { id: "s".into(), recording_path: path.display().to_string(), stage: "map".into(), ..Default::default() };
        persist::save(&Workspace::new(saved, Some(crate::workspace::tests::room_map())), |_, _| {}).unwrap();
        // cut the summary off: the footer's summary_start, less the 13-byte DataEnd record before it
        let bytes = std::fs::read(&path).unwrap();
        let summary_start = u64::from_le_bytes(bytes[bytes.len() - 28..bytes.len() - 20].try_into().unwrap()) as usize;
        std::fs::write(&path, &bytes[..summary_start - 13]).unwrap();

        let app = App::new(dir.path().join("data"), dir.path().to_path_buf());
        let held = app.open("", &path.display().to_string(), "r", true).unwrap();
        let id = held.lock().unwrap().session.id.clone();
        assert_eq!(held.lock().unwrap().session.stage, "raw", "opened without reading it");
        assert_eq!(app.job(&id).unwrap().kind, "open");
        let started = std::time::Instant::now();
        while app.job(&id).unwrap().state == "running" {
            assert!(started.elapsed() < Duration::from_secs(20));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let job = app.job(&id).unwrap();
        assert_eq!(job.state, "done", "{:?}", job.error);
        let ws = held.lock().unwrap();
        assert_eq!(ws.session.stage, "map", "the save in it was restored");
        assert!(ws.map.as_ref().is_some_and(|m| !m.points.is_empty()));
        drop(ws);
        // the same file again (another session's open, or after a restart): walked already, nothing to wait for
        assert!(dimos_recording::mcap_index::is_indexed(&path));
    }

    /// a recording with its own global_map says so when opened, and its newest message becomes the map with no
    /// Generate (read-only is fine: nothing is written into it); one without has no shortcut
    #[tokio::test(flavor = "multi_thread")]
    async fn a_recorded_global_map_is_the_map_without_generating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.mcap");
        let cloud = |at: f64, points: &[[f32; 3]]| dimos_recording::lcm::encode_xyz_cloud(&dimos_recording::lcm::Header::at(at, "world"), points, None);
        let grid: Vec<[f32; 3]> = (0..400).map(|i| [(i % 20) as f32 * 0.05, (i / 20) as f32 * 0.05, 0.0]).collect();
        let pc2 = dimos_recording::lcm::POINT_CLOUD2_TYPE;
        dimos_recording::mcap_io::write_fixture(&path, &[("/lidar", "lcm", pc2, 1.0, cloud(1.0, &[[9.0, 9.0, 9.0]])), ("/global_map", "lcm", pc2, 1.0, cloud(1.0, &grid[..10])), ("/global_map", "lcm", pc2, 2.0, cloud(2.0, &grid))]);
        let before = std::fs::read(&path).unwrap();
        let app = App::new(dir.path().join("data"), dir.path().to_path_buf());
        let held = app.open("", &path.display().to_string(), "r", false).unwrap();
        let id = held.lock().unwrap().session.id.clone();
        assert_eq!(held.lock().unwrap().recorded_map.as_deref(), Some("/global_map"));
        app.use_recorded_map(&id, None, None).unwrap();
        let started = std::time::Instant::now();
        while app.job(&id).unwrap().state == "running" {
            assert!(started.elapsed() < Duration::from_secs(20));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let job = app.job(&id).unwrap();
        assert_eq!((job.kind.as_str(), job.state.as_str()), ("recordedMap", "done"), "{:?}", job.error);
        let ws = held.lock().unwrap();
        assert_eq!(ws.session.stage, "map");
        let map = ws.map.as_ref().unwrap();
        assert_eq!((map.points.len(), map.voxel_size), (400, 0.05), "the newest message, kept on its grid");
        assert_eq!(ws.session.build.as_ref().unwrap().world_frame, "world");
        drop(ws);
        assert_eq!(std::fs::read(&path).unwrap(), before, "the recording is untouched");

        let plain = dir.path().join("plain.mcap");
        dimos_recording::mcap_io::write_fixture(&plain, &[("/lidar", "lcm", pc2, 1.0, cloud(1.0, &[[9.0, 9.0, 9.0]]))]);
        let held = app.open("", &plain.display().to_string(), "plain", true).unwrap();
        let id = held.lock().unwrap().session.id.clone();
        assert!(held.lock().unwrap().recorded_map.is_none());
        assert!(app.use_recorded_map(&id, None, None).unwrap_err().to_string().contains("no global_map"));
    }

    /// a finished .mcap (or a .db) opens with no job: its summary says what's in it
    #[tokio::test(flavor = "multi_thread")]
    async fn a_finished_mcap_opens_with_no_job() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.mcap");
        dimos_recording::mcap_io::write_fixture(&path, &[("/lidar", "cdr", "sensor_msgs/msg/PointCloud2", 1.0, vec![1, 2])]);
        let app = App::new(dir.path().join("data"), dir.path().to_path_buf());
        let held = app.open("", &path.display().to_string(), "r", true).unwrap();
        let id = held.lock().unwrap().session.id.clone();
        assert!(app.job(&id).is_none());
    }

    #[test]
    fn stage_weights_add_up() {
        let at = |stage_index, done, total| fraction("build", &Progress { overall: None, stage: String::new(), stage_index, stage_count: 4, done, total, note: String::new() });
        assert!((at(3, 1, 1) - 1.0).abs() < 1e-9);
        assert!(at(1, 50, 100) > at(1, 10, 100));
        assert!(at(2, 0, 100) > at(1, 99, 100) - 0.01);
    }
}
