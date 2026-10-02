//! The page's HTTP API under /api (docs/api.md in this repo). Session-scoped routes take the session id from
//! `/api/open` (one per recording). Errors are `{ "error": "..." }`.
use crate::app::App;
use crate::workspace::{OpResult, Region, Workspace};
use anyhow::Context;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use futures::Stream;
use mapping::floorplan::PlanOptions;
use mapping::voxels::Box3;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct ApiError(StatusCode, String);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(error: E) -> Self {
        let error = error.into();
        let message = format!("{error:#}");
        let status = if message.starts_with("no session") || message.starts_with("no annotation") { StatusCode::NOT_FOUND } else { StatusCode::BAD_REQUEST };
        ApiError(status, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

type Result<T> = std::result::Result<T, ApiError>;

/// What the page needs about a session: everything but the undo payloads, plus derived bits.
pub fn summary(app: &App, workspace: &Workspace) -> Value {
    let session = &workspace.session;
    let mut value = serde_json::to_value(session).unwrap_or_default();
    if let Some(object) = value.as_object_mut() {
        object.remove("undo");
        object.remove("redo");
        object.remove("plans");
        object.insert(
            "plans".into(),
            json!(session
                .plans
                .iter()
                .enumerate()
                .map(|(index, plan)| {
                    let (free, occupied, unknown) = plan.counts();
                    json!({ "index": index, "z": plan.z, "resolution": plan.resolution, "origin": plan.origin, "width": plan.width, "height": plan.height, "free": free, "occupied": occupied, "unknown": unknown })
                })
                .collect::<Vec<_>>()),
        );
        object.insert("undoLabel".into(), json!(session.undo.last().map(|e| e.label.clone())));
        object.insert("redoLabel".into(), json!(session.redo.last().map(|e| e.label.clone())));
        object.insert("unsaved".into(), json!(session.revision != session.saved_revision));
        object.insert("voxels".into(), json!(workspace.remaining()));
        object.insert("totalVoxels".into(), json!(workspace.map.as_ref().map_or(0, |m| m.points.len())));
        object.insert("voxelSize".into(), json!(workspace.map.as_ref().map(|m| m.voxel_size)));
        object.insert("bounds".into(), json!(workspace.bounds()));
        object.insert("job".into(), json!(app.job(&session.id)));
        let history = &session.history;
        object.insert("history".into(), json!(history[history.len().saturating_sub(30)..]));
    }
    value
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/state", get(state))
        .route("/api/open", post(open))
        .route("/api/events", get(events))
        .route("/api/captures/{request}", post(capture))
        .route("/api/sessions/{id}", get(get_session).delete(discard))
        .route("/api/sessions/{id}/points.bin", get(points))
        .route("/api/sessions/{id}/paths", get(paths))
        .route("/api/sessions/{id}/preview.bin", get(preview))
        .route("/api/sessions/{id}/build", post(build))
        .route("/api/sessions/{id}/job", delete(cancel))
        .route("/api/sessions/{id}/op", post(op))
        .route("/api/sessions/{id}/transform", post(transform))
        .route("/api/sessions/{id}/annotations", post(add_annotation))
        .route("/api/sessions/{id}/annotations/{annotation}", axum::routing::patch(patch_annotation).delete(delete_annotation))
        .route("/api/sessions/{id}/fit-box", post(fit_box))
        .route("/api/sessions/{id}/query", post(query))
        .route("/api/sessions/{id}/plans", post(plans))
        .route("/api/sessions/{id}/plans/{file}", get(plan_png))
        .route("/api/sessions/{id}/undo", post(undo))
        .route("/api/sessions/{id}/redo", post(redo))
        .route("/api/sessions/{id}/view", put(view))
        .route("/api/sessions/{id}/save", post(save))
        .route("/mcp", post(crate::mcp::handle).get(|| async { (StatusCode::METHOD_NOT_ALLOWED, "POST JSON-RPC 2.0 here (MCP streamable HTTP)") }))
        .with_state(app)
}

async fn state(State(app): State<Arc<App>>) -> Result<Json<Value>> {
    let active = app.active.lock().unwrap().clone().or_else(|| app.store.last_open());
    let session = match &active {
        Some(id) => app.workspace(id)?.map(|w| summary(&app, &w.lock().unwrap())),
        None => None,
    };
    Ok(Json(json!({ "active": active, "session": session, "recordingsDir": app.recordings_dir })))
}

#[derive(Deserialize)]
struct OpenBody {
    #[serde(default)]
    id: String,
    path: String,
    #[serde(default)]
    name: String,
}

async fn open(State(app): State<Arc<App>>, Json(body): Json<OpenBody>) -> Result<Json<Value>> {
    let app2 = app.clone();
    let workspace = tokio::task::spawn_blocking(move || app2.open(&body.id, &body.path, &body.name)).await??;
    let value = summary(&app, &workspace.lock().unwrap());
    Ok(Json(value))
}

async fn get_session(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let workspace = app.require(&id)?;
    let value = summary(&app, &workspace.lock().unwrap());
    Ok(Json(value))
}

async fn discard(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    app.discard(&id)?;
    Ok(Json(json!({ "ok": true })))
}

fn binary(bytes: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response()
}

async fn points(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Response> {
    let workspace = app.require(&id)?;
    let bytes = tokio::task::spawn_blocking(move || workspace.lock().unwrap().points_bytes()).await?;
    Ok(binary(bytes))
}

async fn paths(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(app.require(&id)?.lock().unwrap().paths()))
}

/// The raw-recording preview: 202 + a running job until it's ready, then f32 xyz points followed by the path.
async fn preview(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Response> {
    let app2 = app.clone();
    let id2 = id.clone();
    let found = tokio::task::spawn_blocking(move || app2.preview(&id2)).await??;
    let Some(preview) = found else {
        return Ok((StatusCode::ACCEPTED, Json(json!({ "job": app.job(&id) }))).into_response());
    };
    let mut bytes = Vec::with_capacity(8 + (preview.points.len() + preview.path.len()) * 12);
    bytes.extend_from_slice(&(preview.points.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(preview.path.len() as u32).to_le_bytes());
    for p in preview.points.iter().chain(&preview.path) {
        for v in p {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    Ok(binary(bytes))
}

async fn build(State(app): State<Arc<App>>, Path(id): Path<String>, body: Option<Json<mapping::build::BuildOptions>>) -> Result<Json<Value>> {
    let options = body.map(|b| b.0).unwrap_or_default();
    let job = app.build(&id, options)?;
    Ok(Json(json!({ "job": job })))
}

async fn cancel(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(json!({ "cancelled": app.cancel(&id) })))
}

#[derive(Deserialize)]
pub struct OpBody {
    pub op: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default = "all")]
    pub region: Region,
    #[serde(default)]
    pub preview: bool,
}

fn all() -> Region {
    Region::All
}

async fn op(State(app): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<OpBody>) -> Result<Json<OpResult>> {
    let app2 = app.clone();
    let result = tokio::task::spawn_blocking(move || {
        if body.preview {
            let workspace = app2.require(&id)?;
            let mut workspace = workspace.lock().unwrap();
            workspace.op(&body.op, &body.params, &body.region, true)
        } else {
            app2.mutate(&id, |w| w.op(&body.op, &body.params, &body.region, false))
        }
    })
    .await??;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum TransformBody {
    Rotate { degrees: f64 },
    Level,
    Set { transform: crate::session::Transform },
}

async fn transform(State(app): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<TransformBody>) -> Result<Json<Value>> {
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || {
        app2.mutate(&id, |w| match body {
            TransformBody::Rotate { degrees } => w.rotate_yaw(degrees),
            TransformBody::Level => w.level().map(|_| ()),
            TransformBody::Set { transform } => w.set_transform("Set the map transform", transform),
        })
    })
    .await??;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum NewAnnotation {
    #[serde(rename_all = "camelCase")]
    Box {
        label: String,
        #[serde(rename = "box")]
        region: crate::session::Box3Json,
        #[serde(default)]
        source: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Plane {
        label: String,
        center: [f32; 3],
        normal: [f32; 3],
        size: [f32; 2],
        #[serde(default)]
        source: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Point {
        label: String,
        position: [f32; 3],
        #[serde(default)]
        source: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    PlanPoint { floor: usize, name: String, position: [f32; 2] },
    #[serde(rename_all = "camelCase")]
    Area { floor: usize, name: String, kind: String, polygon: Vec<[f32; 2]> },
}

pub fn add(workspace: &mut Workspace, annotation: NewAnnotation, default_source: &str) -> anyhow::Result<String> {
    match annotation {
        NewAnnotation::Box { label, region, source } => workspace.add_box(&label, region.into(), source.as_deref().unwrap_or(default_source)),
        NewAnnotation::Plane { label, center, normal, size, source } => workspace.add_plane(&label, center, normal, size, source.as_deref().unwrap_or(default_source)),
        NewAnnotation::Point { label, position, source } => workspace.add_point(&label, position, source.as_deref().unwrap_or(default_source)),
        NewAnnotation::PlanPoint { floor, name, position } => workspace.add_plan_point(floor, &name, position),
        NewAnnotation::Area { floor, name, kind, polygon } => workspace.add_area(floor, &name, &kind, polygon),
    }
}

async fn add_annotation(State(app): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<NewAnnotation>) -> Result<Json<Value>> {
    let created = app.mutate(&id, |w| add(w, body, "user"))?;
    Ok(Json(json!({ "id": created })))
}

async fn patch_annotation(State(app): State<Arc<App>>, Path((id, annotation)): Path<(String, String)>, Json(patch): Json<Value>) -> Result<Json<Value>> {
    app.mutate(&id, |w| w.update_annotation(&annotation, Some(&patch)))?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_annotation(State(app): State<Arc<App>>, Path((id, annotation)): Path<(String, String)>) -> Result<Json<Value>> {
    app.mutate(&id, |w| w.update_annotation(&annotation, None))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct RegionBody {
    #[serde(rename = "box")]
    region: crate::session::Box3Json,
}

async fn fit_box(State(app): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<RegionBody>) -> Result<Json<Value>> {
    let workspace = app.require(&id)?;
    let (fit, used) = tokio::task::spawn_blocking(move || workspace.lock().unwrap().fit_box(&Box3::from(body.region))).await??;
    Ok(Json(json!({ "box": crate::session::Box3Json::from(fit), "voxels": used })))
}

#[derive(Deserialize)]
struct QueryBody {
    region: Region,
    #[serde(default = "sample_limit")]
    limit: usize,
}

fn sample_limit() -> usize {
    50
}

async fn query(State(app): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<QueryBody>) -> Result<Json<Value>> {
    let workspace = app.require(&id)?;
    Ok(Json(tokio::task::spawn_blocking(move || workspace.lock().unwrap().query(&body.region, body.limit)).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlansBody {
    #[serde(default = "yes")]
    multi_floor: bool,
    #[serde(default)]
    levels: Option<Vec<f32>>,
    #[serde(default)]
    options: Option<PlanOptions>,
}

fn yes() -> bool {
    true
}

async fn plans(State(app): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<PlansBody>) -> Result<Json<Value>> {
    let app2 = app.clone();
    let floors = tokio::task::spawn_blocking(move || app2.mutate(&id, |w| w.generate_plans(body.multi_floor, body.levels, body.options.unwrap_or_default()))).await??;
    Ok(Json(json!({ "floors": floors })))
}

async fn plan_png(State(app): State<Arc<App>>, Path((id, file)): Path<(String, String)>) -> Result<Response> {
    let index: usize = file.trim_end_matches(".png").parse().context("plan index")?;
    let workspace = app.require(&id)?;
    let png = {
        let workspace = workspace.lock().unwrap();
        workspace.session.plans.get(index).context("no such floor plan")?.png()
    };
    Ok(([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-store")], png).into_response())
}

async fn undo(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let label = app.mutate(&id, |w| Ok(w.undo()))?;
    Ok(Json(json!({ "undone": label })))
}

async fn redo(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let label = app.mutate(&id, |w| Ok(w.redo()))?;
    Ok(Json(json!({ "redone": label })))
}

async fn view(State(app): State<Arc<App>>, Path(id): Path<String>, Json(body): Json<Value>) -> Result<Json<Value>> {
    app.set_view(&id, body)?;
    Ok(Json(json!({ "ok": true })))
}

async fn save(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(json!({ "job": app.save(&id)? })))
}

#[derive(Deserialize)]
struct CaptureQuery {}

async fn capture(State(app): State<Arc<App>>, Path(request): Path<u64>, Query(_): Query<CaptureQuery>, body: Bytes) -> Result<Json<Value>> {
    let image = String::from_utf8(body.to_vec()).context("capture must be a data URL")?;
    Ok(Json(json!({ "delivered": app.deliver_capture(request, image) })))
}

async fn events(State(app): State<Arc<App>>) -> Sse<impl Stream<Item = std::result::Result<Event, std::convert::Infallible>>> {
    let receiver = app.events.subscribe();
    let stream = futures::stream::unfold(receiver, |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(event) => return Some((Ok(Event::default().data(event.to_string())), receiver)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
