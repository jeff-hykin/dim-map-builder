//! Every Map Editor action as an HTTP endpoint (routes.rs): the page calls these, Desktop's agent calls the same ones
//! (listed in the served agent.json and dimos.yaml's `agent:`). Session routes take a session id, or `current` for
//! the recording open in the page. Edits go through App::mutate (autosave, undo, a `session` event to every page), so
//! anyone's change shows up live. Errors are `{ "error": "..." }`. docs/api.md.
use crate::app::App;
use crate::probe::{find_objects, parse_box, parse_region};
use crate::routes::Routes;
use crate::workspace::{Modify, Region, Workspace};
use anyhow::Context;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequest, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::{SinkExt, Stream, StreamExt};
use mapping::voxels::Box3;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const DESCRIPTION: &str = "Map Editor: build, clean, annotate and save a 3D voxel map of a robot's recording, in the 'map' frame: meters, +z up, \
the floor of the main storey near z = 0 once levelled. Session routes take {id}: `current` is the recording open in the page. \
Workflow: GET api/view (where the user is looking + a labelled screenshot) or GET api/status (bounds, floors) -> query / find-objects to locate \
things by geometry -> fit-box for a tight box around what's in a rough region (add=true adds it). fit-box keeps the connected piece nearest the \
region's center, so center the rough region on the object you mean: a generous region is fine. Trust fit-box's extents over a screenshot. \
Edits are undoable (POST .../undo) and show up in the page immediately. A region is \"view\" (what the user sees now), \"all\", or a box \
{center:[x,y,z], size:[dx,dy,dz], yaw}.";

pub struct ApiError(StatusCode, String);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(error: E) -> Self {
        let error = error.into();
        let message = format!("{error:#}");
        let status = if message.starts_with("no session") || message.starts_with("no annotation") || message.starts_with("no such") || message.starts_with("no recording is open") {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        ApiError(status, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

type Result<T> = std::result::Result<T, ApiError>;

/// A JSON body, lenient: no body (or no content-type, as an agent may send) is `{}`.
pub struct Body(pub Value);

impl<S: Send + Sync> FromRequest<S> for Body {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> std::result::Result<Self, ApiError> {
        let bytes = Bytes::from_request(request, state).await.map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.to_string()))?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Body(json!({})));
        }
        serde_json::from_slice(&bytes).map(Body).map_err(|error| ApiError(StatusCode::BAD_REQUEST, format!("the body isn't JSON: {error}")))
    }
}

type Args = Query<HashMap<String, String>>;

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
        object.insert("mapVersion".into(), json!(workspace.map_version));
        object.insert("totalVoxels".into(), json!(workspace.map.as_ref().map_or(0, |m| m.points.len())));
        object.insert("voxelSize".into(), json!(workspace.map.as_ref().map(|m| m.voxel_size)));
        object.insert("bounds".into(), json!(workspace.bounds()));
        object.insert("job".into(), json!(app.job(&session.id)));
        let history = &session.history;
        object.insert("history".into(), json!(history[history.len().saturating_sub(30)..]));
    }
    value
}

fn s(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}
fn n(description: &str) -> Value {
    json!({ "type": "number", "description": description })
}
fn b(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}
fn int(description: &str) -> Value {
    json!({ "type": "integer", "description": description })
}
fn numbers(description: &str) -> Value {
    json!({ "type": "array", "items": { "type": "number" }, "description": description })
}
fn path_2d(description: &str) -> Value {
    json!({ "type": "array", "items": { "type": "array", "items": { "type": "number" } }, "description": description })
}
fn required(mut spec: Value) -> Value {
    spec["required"] = json!(true);
    spec
}

pub fn routes() -> Routes<Arc<App>> {
    let session = json!({ "type": "string", "required": true, "description": "session id, or `current` (the recording open in the page)" });
    let box_schema = json!({
        "type": "object",
        "description": "an oriented box in map-frame meters",
        "properties": {
            "center": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
            "size": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "full extents along the box's x, y, z" },
            "yaw": { "type": "number", "description": "radians about +z (default 0)" }
        },
        "required": ["center", "size"]
    });
    let region = json!({ "description": "\"view\" (the user's current camera view), \"all\" (default), or a box {center, size, yaw}", "anyOf": [{ "type": "string", "enum": ["view", "all"] }, box_schema.clone()] });
    let with_id = |params: Value| {
        let mut params = params;
        params["id"] = session.clone();
        params
    };
    Routes::new()
        // the page and what's open
        .endpoint("GET", "api/status", "The open map, briefly: session id, recording, stage (raw = not built yet / map), voxel count, bounds, floors, annotation counts, running job, what can be undone, unsaved edits, recent history.", json!({ "session": s("session id (default: the recording open in the page)") }), status)
        .role("context")
        .endpoint("GET", "api/state", "What the page loads: the open session id, its full state (annotations, plans, job, history...) and Desktop's recordings folder.", json!({}), state)
        .endpoint("POST", "api/open", "Open a recording (.mcap / .db) in the Map Editor: every open page switches to it (its saved map and edits come back). path = absolute path (Desktop's GET /recordings lists them) or relative to the recordings folder.", json!({ "path": required(s("the recording file")), "name": s("display name"), "id": s("Desktop's recording id"), "writable": b("false for a read-only folder: saving copies it first (default true)") }), open)
        .endpoint("GET", "api/build-defaults", "The default map build options (voxelSize, loopClosure, rayTracing, every, maxRange, tfTolerance, worldFrame, cloudStream, ray, pgo).", json!({}), |  | async { Json(json!(mapping::build::BuildOptions::default())) })
        .endpoint("GET", "api/view", "What the user is looking at: camera position/target, the map-frame bounds of the visible voxels, the selection, and (screenshot=true, default) a screenshot of the 3D view with a 1 m grid, axis labels and annotation labels drawn on it (images[0]).", json!({ "screenshot": b("default true"), "topDown": b("screenshot from straight above the current target instead of the user's angle (the user's camera is restored after)"), "session": s("default: the open one") }), get_view)
        .role("view")
        .endpoint("POST", "api/camera", "Move the user's camera to look at a point (map frame).", json!({ "target": required(numbers("[x, y, z]")), "distance": n("meters from the target"), "topDown": b("look straight down") }), camera)
        .endpoint("PATCH", "api/ui", "Change what the page shows (every open page follows): mode (3d | split | 2d), tool (select | erase | brush | line | straighten | polygon | places | annotate | clean | views), planFloor (the storey the 2D view works on), paletteOpen, floorOverlay, showPaths {corrected, raw, loops}, look {style: voxel | disc | square | splat, gradient, scale}, scope (view | region | all), slice ([zStart, zEnd] over the local floor).", json!({ "mode": s("3d | split | 2d"), "tool": s("palette tool"), "planFloor": int("storey index"), "paletteOpen": b(""), "floorOverlay": b(""), "showPaths": json!({ "type": "object" }), "look": json!({ "type": "object" }), "scope": s("view | region | all"), "slice": json!({ "type": "object" }) }), set_ui)
        // a session
        .endpoint("GET", "api/sessions/{id}", "A session's full state: recording, stage, build summary and options, transform, annotations, floor plans, job, history, undo/redo labels, bounds.", with_id(json!({})), get_session)
        .endpoint("DELETE", "api/sessions/{id}", "Discard the working copy (start over; the recording file is untouched).", with_id(json!({})), discard)
        .endpoint("GET", "api/sessions/{id}/paths", "The robot's trajectory in the map frame: { raw, corrected, loops } (loop closures as point pairs).", with_id(json!({})), paths)
        .endpoint("POST", "api/sessions/{id}/build", "Start (re)building the global map from the recording (loop closure + ray tracing). Runs in the background: poll GET api/status (job) or watch the page. Every option is optional (GET api/build-defaults).", with_id(json!({ "voxelSize": n("meters"), "loopClosure": b(""), "rayTracing": b(""), "every": int("use every n-th scan"), "maxRange": n("meters"), "cloudStream": s("the point cloud stream"), "worldFrame": s("") })), build)
        .endpoint("DELETE", "api/sessions/{id}/job", "Cancel the running job (build, preview or save).", with_id(json!({})), cancel)
        .endpoint("POST", "api/sessions/{id}/op", "Clean up or crop (undoable). op: floating (small disconnected clusters; params.minVoxels, default 30), outliers (statistical; params.neighbors, params.stdRatio), floor (the floor surface; params.thickness), walls (vertical surfaces; params.minHeight), keepWalls (everything but walls), cropOutside (keep only a box region), deleteInside (remove a box region), cropHeight (keep params.zMin..params.zMax). preview=true only counts (and returns the points it would remove).", with_id(json!({ "op": required(s("floating | outliers | floor | walls | keepWalls | cropOutside | deleteInside | cropHeight")), "region": region.clone(), "params": json!({ "type": "object" }), "preview": b("only count") })), op)
        .endpoint("POST", "api/sessions/{id}/transform", "Orient the map (undoable; annotations move with it): kind=rotate spins it about +z through its center by degrees; kind=level tilts it so the main floor is flat at z = 0; kind=set sets the transform {translation, rotation (x,y,z,w)}.", with_id(json!({ "kind": required(s("rotate | level | set")), "degrees": n("for rotate"), "transform": json!({ "type": "object", "description": "for set" }) })), transform)
        .endpoint("GET", "api/sessions/{id}/floor", "The local floor per storey: a grid of floor heights (null = unknown) and which were measured.", with_id(json!({})), floor)
        .endpoint("PUT", "api/sessions/{id}/slice", "Set (undoable) the slicer's view: { zMin, zMax, yaw, xMin, xMax, yMin, yMax } in map meters, or null to clear it.", with_id(json!({ "zMin": n(""), "zMax": n(""), "yaw": n("radians"), "xMin": n(""), "xMax": n(""), "yMin": n(""), "yMax": n("") })), set_slice)
        .endpoint("GET", "api/sessions/{id}/alignment", "The suggested yaw (radians) that lines the walls up with x and y.", with_id(json!({})), alignment)
        .endpoint("POST", "api/sessions/{id}/modify", "Brush edits on a storey (undoable), map-frame x, y meters. tool=erase: remove what stands on the floor along path [[x, y], ...] of radius, up to zEnd (m over the local floor when relative, default 1.8; fullColumn = everything up to the next storey), patching the floor under it. tool=draw: add voxels along path of width (default 0.1) up to height (default 1 m). tool=straighten: replace a noisy wall along from..to (width band, default 0.4) with one straight slab (thickness = the wall's own unless given).", with_id(json!({ "tool": required(s("erase | draw | straighten")), "floor": required(int("storey index")), "path": path_2d("erase/draw: [[x, y], ...]"), "radius": n("erase"), "width": n("draw/straighten"), "height": n("draw"), "from": numbers("straighten: [x, y]"), "to": numbers("straighten: [x, y]"), "thickness": n("straighten"), "zEnd": n(""), "relative": b("zEnd over the local floor (default true)"), "fullColumn": b("") })), modify)
        .endpoint("GET", "api/sessions/{id}/annotations", "Every annotation: boxes, planes, points, floors, named plan points, areas, polygons (prisms), saved 2D views, the slice.", with_id(json!({})), list_annotations)
        .endpoint("POST", "api/sessions/{id}/annotations", "Add an annotation (undoable). type=box {label, box}; plane {label, center, normal, size [w, h]}; point {label, position}; planPoint {floor, name, position [x, y]} (a named spot on a floor plan); area {floor, name, kind (\"no-go\" = navigation avoids it, \"zone\", ...), polygon [[x, y], ...]}; prism {floor, label, polygon, height (default 1), base?} (a polygon standing up from the local floor); view {name, floor, zMin, zMax, center?, follow} (a saved 2D view). source: who made it (default user; pass \"agent\").", with_id(json!({ "type": required(s("box | plane | point | planPoint | area | prism | view")), "label": s(""), "name": s("planPoint / area / view"), "box": box_schema.clone(), "center": numbers(""), "normal": numbers("plane"), "size": numbers("plane: [w, h]"), "position": numbers(""), "floor": int("storey index"), "kind": s("area kind"), "polygon": path_2d("[[x, y], ...]"), "height": n("prism"), "base": n("prism"), "zMin": n("view"), "zMax": n("view"), "follow": b("view: z over the local floor (default true)"), "source": s("user | agent") })), add_annotation)
        .endpoint("PATCH", "api/sessions/{id}/annotations/{annotation}", "Change an annotation's fields by id (undoable), e.g. {\"label\": \"desk\"} or {\"box\": {\"center\": [..], \"size\": [..], \"yaw\": 0}}.", with_id(json!({ "annotation": required(s("annotation id")) })), patch_annotation)
        .endpoint("DELETE", "api/sessions/{id}/annotations/{annotation}", "Delete an annotation by id (undoable).", with_id(json!({ "annotation": required(s("annotation id")) })), delete_annotation)
        .endpoint("POST", "api/sessions/{id}/fit-box", "The tightest oriented box around the non-floor voxels inside a rough box. Pass a generous region; the result hugs the object. add=true also adds it as a box annotation (label).", with_id(json!({ "box": required(box_schema.clone()), "label": s("for add"), "add": b("") })), fit_box)
        .endpoint("POST", "api/sessions/{id}/query", "Probe the map: how many voxels are in a region, their min/max corner, how many lie on horizontal vs vertical surfaces, and a sample of positions.", with_id(json!({ "region": region.clone(), "limit": int("sample size (default 40)") })), query)
        .endpoint("POST", "api/sessions/{id}/find-objects", "Clusters of voxels standing on the floor (things like furniture) in a region: each with a tight box, voxel count and height. Floor, ceiling and walls are excluded. A good first step for 'box every chair'.", with_id(json!({ "region": region.clone(), "minVoxels": int("default 25"), "maxHeight": n("ignore clusters taller than this above the floor (m), default 2") })), find)
        .endpoint("POST", "api/sessions/{id}/plans", "Make 2D floor plans (occupancy: walls/furniture vs free floor) from the 3D map, one per storey when multiFloor (default true), optionally at explicit floor heights.", with_id(json!({ "multiFloor": b("default true"), "levels": numbers("floor heights (m)"), "options": json!({ "type": "object" }) })), plans)
        .endpoint("GET", "api/sessions/{id}/plans/{floor}", "A floor plan (images[0]: white free, black occupied, grey unknown; +y up) with its origin and resolution so pixels map to meters, plus that floor's named points and areas. `{floor}.png` is the bare image.", with_id(json!({ "floor": required(s("storey index")) })), plan)
        .endpoint("POST", "api/sessions/{id}/undo", "Undo the last edit (anyone's).", with_id(json!({})), undo)
        .endpoint("POST", "api/sessions/{id}/redo", "Redo the last undone edit.", with_id(json!({})), redo)
        .endpoint("POST", "api/sessions/{id}/save", "Write the map and every annotation into the recording file (a background job; a read-only recording is copied into the recordings folder first).", with_id(json!({})), save)
        .endpoint("POST", "api/sessions/{id}/upload", "Upload the recording to Dimensional cloud through Desktop's upload queue (Desktop's /dimos/uploads shows progress; it waits for a login if there is none). saveFirst (default true) saves unsaved edits into it first.", with_id(json!({ "saveFirst": b("default true") })), upload)
        // the page's plumbing: binary map data, its reports, the event socket
        .plumbing("GET", "api/sessions/{id}/points.bin", points)
        .plumbing("GET", "api/sessions/{id}/preview.bin", preview)
        .plumbing("PUT", "api/sessions/{id}/view", report_view)
        .plumbing("POST", "api/captures/{request}", capture)
        .plumbing("GET", "api/events/ws", events_ws)
        .plumbing("GET", "agent.json", || async { Json(routes().manifest(DESCRIPTION)) })
}

fn bool_arg(args: &HashMap<String, String>, name: &str, default: bool) -> bool {
    args.get(name).map_or(default, |value| value != "false" && value != "0")
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T> {
    Ok(tokio::task::spawn_blocking(work).await??)
}

/// `current` (or empty) → the session the page has open.
fn session(app: &App, id: &str) -> anyhow::Result<(String, Arc<Mutex<Workspace>>)> {
    if id.is_empty() || id == "current" {
        app.target(None)
    } else {
        Ok((id.to_string(), app.require(id)?))
    }
}

fn edit<T: Send + 'static>(app: &Arc<App>, id: String, change: impl FnOnce(&mut Workspace) -> anyhow::Result<T> + Send + 'static) -> impl std::future::Future<Output = Result<T>> {
    let app = app.clone();
    blocking(move || app.mutate(&id, change))
}

async fn status(State(app): State<Arc<App>>, Query(args): Args) -> Result<Json<Value>> {
    let (id, workspace) = session(&app, args.get("session").map_or("", String::as_str))?;
    let summary = summary(&app, &workspace.lock().unwrap());
    let annotations = &summary["annotations"];
    let count = |kind: &str| annotations[kind].as_array().map_or(0, |a| a.len());
    Ok(Json(json!({
        "session": id,
        "recording": summary["recordingPath"],
        "stage": summary["stage"],
        "voxels": summary["voxels"],
        "voxelSize": summary["voxelSize"],
        "bounds": summary["bounds"],
        "floors": annotations["floors"],
        "counts": { "boxes": count("boxes"), "planes": count("planes"), "points": count("points"), "areas": count("areas"), "prisms": count("prisms"), "views": count("views") },
        "plans": summary["plans"].as_array().map_or(0, |a| a.len()),
        "job": summary["job"],
        "canUndo": summary["undoLabel"],
        "unsaved": summary["unsaved"],
        "recent": summary["history"],
    })))
}

async fn state(State(app): State<Arc<App>>) -> Result<Json<Value>> {
    let active = app.active.lock().unwrap().clone().or_else(|| app.store.last_open());
    let session = match &active {
        Some(id) => app.workspace(id)?.map(|w| summary(&app, &w.lock().unwrap())),
        None => None,
    };
    Ok(Json(json!({ "active": active, "session": session, "recordingsDir": app.recordings_dir })))
}

async fn open(State(app): State<Arc<App>>, Body(body): Body) -> Result<Json<Value>> {
    let path = body["path"].as_str().context("path is required")?.to_string();
    let path = if std::path::Path::new(&path).is_absolute() { path } else { app.recordings_dir.join(&path).display().to_string() };
    let id = body["id"].as_str().unwrap_or_default().to_string();
    let name = body["name"].as_str().unwrap_or_default().to_string();
    let writable = body["writable"].as_bool().unwrap_or(true);
    let app2 = app.clone();
    let workspace = blocking(move || app2.open(&id, &path, &name, writable)).await?;
    let value = summary(&app, &workspace.lock().unwrap());
    // every open page follows (the one that asked already has it)
    app.emit(json!({ "type": "opened", "id": value["id"] }));
    Ok(Json(value))
}

async fn get_view(State(app): State<Arc<App>>, Query(args): Args) -> Result<Json<Value>> {
    let (_, workspace) = session(&app, args.get("session").map_or("", String::as_str))?;
    let view = workspace.lock().unwrap().session.view.clone();
    let mut result = json!({
        "camera": view.get("camera"),
        "visibleBounds": view.get("visibleBounds"),
        "selected": view.get("selected"),
        "ui": view.get("ui").map(|ui| json!({ "mode": ui["mode"], "tool": ui["tool"], "planFloor": ui["planFloor"], "look": ui["look"] })),
        "note": "map-frame meters; the screenshot's grid lines are 1 m apart, labelled at their x/y values",
    });
    if bool_arg(&args, "screenshot", true) {
        let image = app.capture(json!({ "overlay": true, "topDown": bool_arg(&args, "topDown", false) })).await?;
        let data = image.split_once(',').map_or(image.as_str(), |(_, data)| data).to_string();
        result["images"] = json!([{ "mimeType": "image/png", "data": data }]);
    }
    Ok(Json(result))
}

async fn camera(State(app): State<Arc<App>>, Body(body): Body) -> Result<Json<Value>> {
    let target: [f32; 3] = serde_json::from_value(body["target"].clone()).context("target [x, y, z]")?;
    app.emit(json!({ "type": "setView", "target": target, "distance": body["distance"], "topDown": body["topDown"] }));
    Ok(Json(json!({ "ok": true })))
}

const UI_KEYS: [&str; 9] = ["mode", "tool", "planFloor", "paletteOpen", "floorOverlay", "showPaths", "look", "scope", "slice"];

async fn set_ui(State(app): State<Arc<App>>, Body(body): Body) -> Result<Json<Value>> {
    let patch = body.as_object().context("a JSON object of UI fields")?;
    if let Some(unknown) = patch.keys().find(|key| !UI_KEYS.contains(&key.as_str())) {
        return Err(ApiError(StatusCode::BAD_REQUEST, format!("unknown UI field {unknown} (one of {})", UI_KEYS.join(", "))));
    }
    if let Some(mode) = patch.get("mode") {
        if !matches!(mode.as_str(), Some("3d" | "split" | "2d")) {
            return Err(ApiError(StatusCode::BAD_REQUEST, "mode is 3d, split or 2d".into()));
        }
    }
    // kept with the session (a refresh restores it), then every page applies it
    if let Ok((id, workspace)) = session(&app, "") {
        let mut view = workspace.lock().unwrap().session.view.clone();
        if !view.is_object() {
            view = json!({});
        }
        if !view["ui"].is_object() {
            view["ui"] = json!({});
        }
        for (key, value) in patch {
            view["ui"][key] = value.clone();
        }
        app.set_view(&id, view)?;
    }
    app.emit(json!({ "type": "ui", "patch": patch }));
    Ok(Json(json!({ "ok": true, "applied": patch })))
}

async fn get_session(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (_, workspace) = session(&app, &id)?;
    let value = summary(&app, &workspace.lock().unwrap());
    Ok(Json(value))
}

async fn discard(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    app.discard(&id)?;
    Ok(Json(json!({ "ok": true })))
}

fn binary(bytes: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response()
}

async fn points(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Response> {
    let (_, workspace) = session(&app, &id)?;
    let bytes = tokio::task::spawn_blocking(move || workspace.lock().unwrap().points_bytes()).await?;
    Ok(binary(bytes))
}

async fn paths(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(session(&app, &id)?.1.lock().unwrap().paths()))
}

/// The raw-recording preview: 202 + a running job until it's ready, then f32 xyz points followed by the path.
async fn preview(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Response> {
    let (id, _) = session(&app, &id)?;
    let app2 = app.clone();
    let id2 = id.clone();
    let found = blocking(move || app2.preview(&id2)).await?;
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

async fn build(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    // the defaults, with whatever was given on top
    let mut options = serde_json::to_value(mapping::build::BuildOptions::default())?;
    if let (Some(options), Some(given)) = (options.as_object_mut(), body.as_object()) {
        for (key, value) in given {
            if key != "id" {
                options.insert(key.clone(), value.clone());
            }
        }
    }
    let options: mapping::build::BuildOptions = serde_json::from_value(options).context("build options")?;
    Ok(Json(json!({ "job": app.build(&id, options)? })))
}

async fn cancel(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    Ok(Json(json!({ "cancelled": app.cancel(&id) })))
}

async fn op(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    let (id, workspace) = session(&app, &id)?;
    let name = body["op"].as_str().context("op is required")?.to_string();
    const OPS: [&str; 8] = ["floating", "outliers", "floor", "walls", "keepWalls", "cropOutside", "deleteInside", "cropHeight"];
    if !OPS.contains(&name.as_str()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, format!("op must be one of {}", OPS.join(", "))));
    }
    let region = parse_region(&workspace.lock().unwrap(), &body["region"])?;
    if matches!(name.as_str(), "cropOutside" | "deleteInside") && !matches!(region, Region::Box { .. }) {
        return Err(ApiError(StatusCode::BAD_REQUEST, format!("{name} needs a box region")));
    }
    let params = if body["params"].is_object() { body["params"].clone() } else { json!({}) };
    let result = if body["preview"].as_bool().unwrap_or(false) {
        blocking(move || workspace.lock().unwrap().op(&name, &params, &region, true)).await?
    } else {
        edit(&app, id, move |w| w.op(&name, &params, &region, false)).await?
    };
    Ok(Json(serde_json::to_value(result)?))
}

async fn transform(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    match body["kind"].as_str() {
        Some("rotate") => {
            let degrees = body["degrees"].as_f64().context("degrees is required for rotate")?;
            edit(&app, id, move |w| w.rotate_yaw(degrees)).await?;
        }
        Some("level") => {
            edit(&app, id, |w| w.level().map(|_| ())).await?;
        }
        Some("set") => {
            let transform: crate::session::Transform = serde_json::from_value(body["transform"].clone()).context("transform {translation, rotation}")?;
            edit(&app, id, move |w| w.set_transform("Set the map transform", transform)).await?;
        }
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "kind is rotate, level or set".into())),
    }
    Ok(Json(json!({ "ok": true })))
}

/// The local floor: per storey, a grid of floor heights (null where unknown) and which were measured.
async fn floor(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (_, workspace) = session(&app, &id)?;
    let (model, version) = blocking(move || {
        let mut workspace = workspace.lock().unwrap();
        workspace.floor_model().map(|m| (m, workspace.map_version))
    })
    .await?;
    let storeys: Vec<Value> = model
        .storeys
        .iter()
        .map(|s| {
            json!({
                "level": s.level,
                "band": s.band,
                "heights": s.heights.iter().map(|h| if h.is_nan() { None } else { Some((h * 1000.0).round() / 1000.0) }).collect::<Vec<_>>(),
                "measured": s.measured.iter().map(|m| *m as u8).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Json(json!({ "mapVersion": version, "cell": model.cell, "origin": model.origin, "width": model.width, "height": model.height, "storeys": storeys })))
}

async fn set_slice(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    let slice: Option<crate::session::Slice> = match &body {
        Value::Null => None,
        Value::Object(fields) if fields.is_empty() => None,
        other => Some(serde_json::from_value(other.clone()).context("slice {zMin, zMax, yaw, xMin, xMax, yMin, yMax}")?),
    };
    edit(&app, id, move |w| w.set_slice(slice)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn alignment(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (_, workspace) = session(&app, &id)?;
    let yaw = blocking(move || Ok(workspace.lock().unwrap().alignment_yaw())).await?;
    Ok(Json(json!({ "yaw": yaw })))
}

async fn modify(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    let mut body = body;
    let tool = body["tool"].as_str().unwrap_or_default().to_string();
    let mut default = |key: &str, value: Value| {
        if body.get(key).is_none_or(Value::is_null) {
            body[key] = value;
        }
    };
    default("relative", json!(true));
    default("fullColumn", json!(false));
    default("zEnd", json!(if tool == "erase" { 1.8 } else { 2.5 }));
    default("width", json!(if tool == "draw" { 0.1 } else { 0.4 }));
    default("height", json!(1.0));
    if let Some(object) = body.as_object_mut() {
        object.remove("id");
    }
    let request: Modify = serde_json::from_value(body).context("tool is erase {floor, path, radius}, draw {floor, path} or straighten {floor, from, to}")?;
    let result = edit(&app, id, move |w| w.modify(&request)).await?;
    Ok(Json(serde_json::to_value(result)?))
}

async fn list_annotations(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (_, workspace) = session(&app, &id)?;
    let annotations = serde_json::to_value(&workspace.lock().unwrap().session.annotations)?;
    Ok(Json(annotations))
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
    #[serde(rename_all = "camelCase")]
    Prism {
        floor: usize,
        label: String,
        polygon: Vec<[f32; 2]>,
        #[serde(default = "one_meter")]
        height: f32,
        #[serde(default)]
        base: Option<f32>,
        #[serde(default)]
        source: Option<String>,
    },
    View(crate::session::SavedView),
}

fn one_meter() -> f32 {
    1.0
}

pub fn add(workspace: &mut Workspace, annotation: NewAnnotation) -> anyhow::Result<String> {
    let source = |source: Option<String>| source.unwrap_or_else(|| "user".into());
    match annotation {
        NewAnnotation::Box { label, region, source: by } => workspace.add_box(&label, parse_box(&serde_json::to_value(region)?)?, &source(by)),
        NewAnnotation::Plane { label, center, normal, size, source: by } => workspace.add_plane(&label, center, normal, size, &source(by)),
        NewAnnotation::Point { label, position, source: by } => workspace.add_point(&label, position, &source(by)),
        NewAnnotation::PlanPoint { floor, name, position } => workspace.add_plan_point(floor, &name, position),
        NewAnnotation::Area { floor, name, kind, polygon } => workspace.add_area(floor, &name, &kind, polygon),
        NewAnnotation::Prism { floor, label, polygon, height, base, source: by } => workspace.add_prism(floor, &label, polygon, height, base, &source(by)),
        NewAnnotation::View(view) => workspace.add_view(view),
    }
}

async fn add_annotation(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    let mut body = body;
    // the agent's shorthand: a planPoint/area/view named by `label`, a box/point/plane/prism labelled by `name`; a view defaults to a floor plan's band
    if body["type"] == "view" {
        for (key, value) in [("floor", json!(0)), ("follow", json!(true)), ("zMin", json!(0.1)), ("zMax", json!(1.8))] {
            if body.get(key).is_none_or(Value::is_null) {
                body[key] = value;
            }
        }
    }
    let (named, labelled) = (body.get("name").cloned(), body.get("label").cloned());
    if matches!(body["type"].as_str(), Some("planPoint" | "area" | "view")) && named.is_none() {
        body["name"] = labelled.unwrap_or(json!(""));
    } else if labelled.is_none() {
        body["label"] = named.unwrap_or(json!(""));
    }
    if let Some(object) = body.as_object_mut() {
        object.remove("id");
    }
    let annotation: NewAnnotation = serde_json::from_value(body).context("annotation (see type)")?;
    let created = edit(&app, id, move |w| add(w, annotation)).await?;
    Ok(Json(json!({ "id": created })))
}

async fn patch_annotation(State(app): State<Arc<App>>, Path((id, annotation)): Path<(String, String)>, Body(body): Body) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    let mut patch = body;
    if let Some(object) = patch.as_object_mut() {
        object.remove("id");
        object.remove("annotation");
    }
    edit(&app, id, move |w| w.update_annotation(&annotation, Some(&patch))).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_annotation(State(app): State<Arc<App>>, Path((id, annotation)): Path<(String, String)>) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    edit(&app, id, move |w| w.update_annotation(&annotation, None)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn fit_box(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    let (id, workspace) = session(&app, &id)?;
    let region: Box3 = parse_box(&body["box"])?;
    let (fit, used) = blocking(move || workspace.lock().unwrap().fit_box(&region)).await?;
    let mut result = json!({ "box": crate::session::Box3Json::from(fit), "voxels": used });
    if body["add"].as_bool().unwrap_or(false) {
        let label = body["label"].as_str().unwrap_or("object").to_string();
        let source = body["source"].as_str().unwrap_or("agent").to_string();
        result["id"] = json!(edit(&app, id, move |w| w.add_box(&label, fit, &source)).await?);
    }
    Ok(Json(result))
}

async fn query(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    let (_, workspace) = session(&app, &id)?;
    let limit = body["limit"].as_u64().unwrap_or(40) as usize;
    Ok(Json(
        blocking(move || {
            let workspace = workspace.lock().unwrap();
            let region = parse_region(&workspace, &body["region"])?;
            Ok(workspace.query(&region, limit))
        })
        .await?,
    ))
}

async fn find(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    let (_, workspace) = session(&app, &id)?;
    Ok(Json(
        blocking(move || {
            let workspace = workspace.lock().unwrap();
            let region = parse_region(&workspace, &body["region"])?;
            Ok(json!({ "objects": find_objects(&workspace, &region, body["minVoxels"].as_u64().unwrap_or(25) as usize, body["maxHeight"].as_f64().unwrap_or(2.0) as f32) }))
        })
        .await?,
    ))
}

async fn plans(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    let multi_floor = body["multiFloor"].as_bool().unwrap_or(true);
    let levels: Option<Vec<f32>> = serde_json::from_value(body["levels"].clone()).ok();
    let options: mapping::floorplan::PlanOptions = serde_json::from_value(body["options"].clone()).unwrap_or_default();
    let floors = edit(&app, id, move |w| w.generate_plans(multi_floor, levels, options)).await?;
    Ok(Json(json!({ "floors": floors })))
}

/// `{floor}`: the plan as JSON with its image; `{floor}.png`: the bare image (what the page shows).
async fn plan(State(app): State<Arc<App>>, Path((id, floor)): Path<(String, String)>) -> Result<Response> {
    let (_, workspace) = session(&app, &id)?;
    let png_only = floor.ends_with(".png");
    let index: usize = floor.trim_end_matches(".png").parse().context("floor is a storey index")?;
    let workspace = workspace.lock().unwrap();
    let plan = workspace.session.plans.get(index).context("no such floor plan (POST .../plans first)")?;
    if png_only {
        return Ok(([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-store")], plan.png()).into_response());
    }
    let annotations = &workspace.session.annotations;
    use base64::Engine;
    let image = base64::engine::general_purpose::STANDARD.encode(plan.png());
    Ok(Json(json!({
        "floor": index,
        "z": plan.z,
        "resolution": plan.resolution,
        "origin": plan.origin,
        "width": plan.width,
        "height": plan.height,
        "pixelToMeters": "x = origin[0] + (column + 0.5) * resolution; y = origin[1] + (height - 1 - row + 0.5) * resolution (row 0 is the image's top)",
        "points": annotations.plan_points.iter().filter(|p| p.floor == index).collect::<Vec<_>>(),
        "areas": annotations.areas.iter().filter(|a| a.floor == index).collect::<Vec<_>>(),
        "images": [{ "mimeType": "image/png", "data": image }],
    }))
    .into_response())
}

async fn undo(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    let label = edit(&app, id, |w| Ok(w.undo())).await?;
    Ok(Json(json!({ "undone": label })))
}

async fn redo(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    let label = edit(&app, id, |w| Ok(w.redo())).await?;
    Ok(Json(json!({ "redone": label })))
}

async fn report_view(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    app.set_view(&id, body)?;
    Ok(Json(json!({ "ok": true })))
}

async fn save(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let (id, _) = session(&app, &id)?;
    Ok(Json(json!({ "job": app.save(&id)? })))
}

/// Queues the recording in Desktop's upload queue; with unsaved edits and saveFirst, after a save job finishes.
async fn upload(State(app): State<Arc<App>>, Path(id): Path<String>, Body(body): Body) -> Result<Json<Value>> {
    let (id, workspace) = session(&app, &id)?;
    let (unsaved, built) = {
        let workspace = workspace.lock().unwrap();
        (workspace.session.revision != workspace.session.saved_revision, workspace.map.is_some())
    };
    let desktop = app.desktop_url.get().cloned().unwrap_or_default();
    if desktop.is_empty() {
        bail_api(StatusCode::SERVICE_UNAVAILABLE, "uploads go through dimOS Desktop's queue, and this server wasn't started by Desktop (no --desktop-url)")?;
    }
    if body["saveFirst"].as_bool().unwrap_or(true) && unsaved && built {
        let job = app.save(&id)?;
        let app = app.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                match app.job(&id) {
                    Some(current) if current.id == job.id && current.state == "running" => continue,
                    Some(current) if current.id == job.id && current.state != "done" => {
                        app.emit(json!({ "type": "upload", "error": format!("not uploaded: the save {}{}", current.state, current.error.map(|e| format!(": {e}")).unwrap_or_default()) }));
                        return;
                    }
                    _ => break,
                }
            }
            let Ok((_, workspace)) = session(&app, &id) else { return };
            // a read-only recording was saved into a copy: that is what gets uploaded
            let path = workspace.lock().unwrap().session.recording_path.clone();
            match crate::desktop::post(&desktop, "/dimos/uploads", &json!({ "path": path })).await {
                Ok(upload) => app.emit(json!({ "type": "upload", "upload": upload })),
                Err(error) => app.emit(json!({ "type": "upload", "error": format!("{error:#}") })),
            }
        });
        return Ok(Json(json!({ "job": job, "note": "saving into the recording first; it is queued for upload when the save finishes" })));
    }
    let path = workspace.lock().unwrap().session.recording_path.clone();
    let upload = crate::desktop::post(&desktop, "/dimos/uploads", &json!({ "path": path })).await?;
    app.emit(json!({ "type": "upload", "upload": upload }));
    Ok(Json(json!({ "upload": upload })))
}

fn bail_api(status: StatusCode, message: &str) -> Result<()> {
    Err(ApiError(status, message.into()))
}

async fn capture(State(app): State<Arc<App>>, Path(request): Path<u64>, body: Bytes) -> Result<Json<Value>> {
    let image = String::from_utf8(body.to_vec()).context("capture must be a data URL")?;
    if !image.starts_with("data:") {
        return Err(ApiError(StatusCode::BAD_REQUEST, "capture must be a data URL".into()));
    }
    Ok(Json(json!({ "delivered": app.deliver_capture(request, image) })))
}

/// every server event from now on, as JSON; a subscriber that falls behind skips what it missed
fn event_stream(app: &App) -> impl Stream<Item = Value> {
    futures::stream::unfold(app.events.subscribe(), |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(event) => return Some((event, receiver)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    })
}

/// the standard dimOS app event channel (dim-app events.js): one JSON event per text message
async fn events_ws(State(app): State<Arc<App>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| forward_events(socket, app))
}

async fn forward_events(socket: WebSocket, app: Arc<App>) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = Box::pin(event_stream(&app));
    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(event) => {
                    if sender.send(Message::Text(event.to_string().into())).await.is_err() {
                        return;
                    }
                }
                None => break,
            },
            // the page never sends anything meaningful; this only notices it going away
            incoming = receiver.next() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = sender.send(Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body as HttpBody;
    use tower::ServiceExt;

    fn app() -> (Arc<App>, axum::Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let app = App::new(dir.path().join("data"), dir.path().to_path_buf());
        let router = routes().router.with_state(app.clone());
        (app, router, dir)
    }

    async fn call(router: &axum::Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let request = axum::http::Request::builder().method(method).uri(path);
        let request = match body {
            Some(body) => request.header("content-type", "application/json").body(HttpBody::from(body.to_string())),
            None => request.body(HttpBody::empty()),
        };
        let response = router.clone().oneshot(request.unwrap()).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn recording(dir: &std::path::Path) -> String {
        let path = dir.join("r.db");
        let mut db = dimos_recording::db::Connection::open(&path).unwrap();
        dimos_recording::db::write_stream(&mut db, "lidar", "sensor_msgs.PointCloud2", &[]).unwrap();
        path.display().to_string()
    }

    /// every listed endpoint is routed: with no session open each answers with a readable JSON error or a result,
    /// never the router's bare 404 / 405
    #[tokio::test]
    async fn every_endpoint_is_routed() {
        let (_app, router, _dir) = app();
        let manifest = routes().manifest(DESCRIPTION);
        for endpoint in manifest["endpoints"].as_array().unwrap() {
            let path = endpoint["path"].as_str().unwrap().replace("{id}", "current").replace("{annotation}", "a").replace("{floor}", "0");
            let method = endpoint["method"].as_str().unwrap();
            let (status, body) = call(&router, method, &format!("/{path}"), None).await;
            assert!(status.is_success() || body["error"].is_string(), "{method} {path}: {status} {body}");
            assert!(endpoint["description"].as_str().unwrap().len() > 10);
        }
        let (status, body) = call(&router, "GET", "/agent.json", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, manifest);
        assert!(manifest["endpoints"].as_array().unwrap().iter().any(|e| e["role"] == "view"));
        assert!(manifest["endpoints"].as_array().unwrap().iter().any(|e| e["role"] == "context"));
    }

    #[tokio::test]
    async fn open_annotate_undo_and_errors() {
        let (app, router, dir) = app();
        let path = recording(dir.path());
        let (status, _) = call(&router, "GET", "/api/status", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "nothing open yet");
        let (status, body) = call(&router, "POST", "/api/open", Some(json!({ "path": "nope.db" }))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let mut events = app.events.subscribe();
        let (status, opened) = call(&router, "POST", "/api/open", Some(json!({ "path": "r.db" }))).await;
        assert_eq!(status, StatusCode::OK, "{opened}");
        assert_eq!(opened["recordingPath"], path);
        assert_eq!(events.try_recv().unwrap()["type"], "opened");
        let (_, status_body) = call(&router, "GET", "/api/status", None).await;
        assert_eq!(status_body["stage"], "raw");
        // the agent's shorthand and the page's form both add
        let (status, added) = call(&router, "POST", "/api/sessions/current/annotations", Some(json!({ "type": "point", "label": "dock", "position": [1, 2, 0] }))).await;
        assert_eq!(status, StatusCode::OK, "{added}");
        let id = added["id"].as_str().unwrap().to_string();
        let (status, _) = call(&router, "POST", "/api/sessions/current/annotations", Some(json!({ "type": "box", "label": "x", "box": { "center": [0, 0, 0], "size": [0, 1, 1] } }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (_, listed) = call(&router, "GET", "/api/sessions/current/annotations", None).await;
        assert_eq!(listed["points"][0]["label"], "dock");
        let (status, _) = call(&router, "PATCH", &format!("/api/sessions/current/annotations/{id}"), Some(json!({ "label": "door" }))).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&router, "PATCH", "/api/sessions/current/annotations/nope", Some(json!({ "label": "x" }))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, undone) = call(&router, "POST", "/api/sessions/current/undo", None).await;
        assert!(undone["undone"].is_string());
        let (_, listed) = call(&router, "GET", &format!("/api/sessions/{}/annotations", opened["id"].as_str().unwrap()), None).await;
        assert_eq!(listed["points"][0]["label"], "dock");
        let (_, redone) = call(&router, "POST", "/api/sessions/current/redo", None).await;
        assert!(redone["redone"].is_string());
        let (status, _) = call(&router, "DELETE", &format!("/api/sessions/current/annotations/{id}"), None).await;
        assert_eq!(status, StatusCode::OK);
        // map edits need a built map; build options are validated
        let (status, body) = call(&router, "POST", "/api/sessions/current/op", Some(json!({ "op": "floating" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let (status, _) = call(&router, "POST", "/api/sessions/current/op", Some(json!({ "op": "explode" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&router, "POST", "/api/sessions/current/save", None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "nothing built to save");
        let (status, _) = call(&router, "POST", "/api/sessions/current/transform", Some(json!({ "kind": "spin" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&router, "POST", "/api/sessions/current/upload", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "not started by Desktop");
    }

    #[tokio::test]
    async fn ui_and_camera_reach_the_page() {
        let (app, router, dir) = app();
        recording(dir.path());
        call(&router, "POST", "/api/open", Some(json!({ "path": "r.db" }))).await;
        let mut events = app.events.subscribe();
        let (status, _) = call(&router, "PATCH", "/api/ui", Some(json!({ "mode": "2d", "planFloor": 1 }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(events.try_recv().unwrap(), json!({ "type": "ui", "patch": { "mode": "2d", "planFloor": 1 } }));
        let (_, state) = call(&router, "GET", "/api/state", None).await;
        assert_eq!(state["session"]["view"]["ui"]["mode"], "2d", "kept with the session");
        assert_eq!(call(&router, "PATCH", "/api/ui", Some(json!({ "mode": "4d" }))).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(call(&router, "PATCH", "/api/ui", Some(json!({ "colour": "red" }))).await.0, StatusCode::BAD_REQUEST);
        let (status, _) = call(&router, "POST", "/api/camera", Some(json!({ "target": [1, 2, 0], "topDown": true }))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(events.try_recv().unwrap()["type"], "setView");
        assert_eq!(call(&router, "POST", "/api/camera", Some(json!({}))).await.0, StatusCode::BAD_REQUEST);
        // no page to take the screenshot
        let (status, body) = call(&router, "GET", "/api/view?screenshot=false", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.get("images").is_none());
    }

    #[tokio::test]
    async fn websocket_carries_each_event_as_one_json_message() {
        let (app, router, _dir) = app();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/api/events/ws")).await.unwrap();
        let received = async {
            loop {
                app.emit(json!({ "type": "session", "id": "x", "revision": 3 }));
                if let Ok(message) = tokio::time::timeout(std::time::Duration::from_millis(100), socket.next()).await {
                    return message;
                }
            }
        };
        let message = received.await.unwrap().unwrap();
        let event: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        assert_eq!(event, json!({ "type": "session", "id": "x", "revision": 3 }));
    }
}
