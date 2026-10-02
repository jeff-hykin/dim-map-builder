//! The Map Builder's tools for an agent (Desktop's chat, dimcode, or any MCP client), as an MCP server over
//! streamable HTTP: `POST /mcp` takes JSON-RPC 2.0 and answers with JSON. Tools act on the recording open in the
//! page unless given `session`. Every edit is the same undoable edit a user makes, and shows up in the page live.
//! Tool docs: docs/agent-tools.md.
use crate::api::{add, NewAnnotation};
use crate::app::App;
use crate::workspace::{Region, Workspace};
use anyhow::{bail, Context, Result};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mapping::voxels::Box3;
use serde_json::{json, Value};
use std::sync::Arc;

const PROTOCOL: &str = "2025-06-18";

const GUIDE: &str = "Map Builder: a 3D voxel map of a robot's recording, in the 'map' frame: meters, +z up, the floor of the main storey near z = 0 once levelled. \
Coordinates you pass and get back are map-frame meters. Workflow: get_view (where the user is looking + a labelled screenshot) or get_status (map bounds, floors) -> \
query_region / find_objects to locate things by geometry -> fit_box to get a tight box around what's in a rough region -> add_box with that box. \
fit_box keeps the connected piece nearest the region's center, so center the rough region on the object you mean (a chair beside a table): \
a generous region is fine. Trust fit_box's extents over a screenshot: screenshots are for finding things, not measuring them. \
Cleanup tools remove voxels and are undoable (undo). 'region' can be \"view\" (what the user sees now), \"all\", or a box {center:[x,y,z], size:[dx,dy,dz], yaw}.";

const GUIDE_BASIC: &str = "Map Builder: a 3D voxel map of a robot's recording, in the 'map' frame: meters, +z up. \
Coordinates you pass and get back are map-frame meters. Use get_view to see what the user sees, then add_box etc. \
Cleanup tools remove voxels and are undoable (undo). 'region' can be \"view\", \"all\", or a box {center:[x,y,z], size:[dx,dy,dz], yaw}.";

fn tools() -> Value {
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
    let region = json!({
        "description": "\"view\" (the user's current camera view), \"all\", or a box object",
        "anyOf": [{ "type": "string", "enum": ["view", "all"] }, box_schema.clone()]
    });
    let session = json!({ "type": "string", "description": "session id (default: the recording open in the page)" });
    let tool = |name: &str, description: &str, properties: Value, required: &[&str]| {
        let mut properties = properties;
        properties["session"] = session.clone();
        json!({ "name": name, "description": description, "inputSchema": { "type": "object", "properties": properties, "required": required } })
    };
    json!([
        tool("get_status", &format!("{GUIDE}\n\nThe open map: recording, stage (raw = not built yet / map), voxel count, bounds, floors, annotation counts, running job, what can be undone."), json!({}), &[]),
        tool("get_view", "What the user is looking at: camera position/target, the map-frame bounds of the visible voxels, and (screenshot=true, default) a screenshot of the 3D view with a 1 m grid, axis labels and annotation labels drawn on it.", json!({ "screenshot": { "type": "boolean" }, "topDown": { "type": "boolean", "description": "screenshot from straight above the current target instead of the user's angle (the user's camera is restored after)" } }), &[]),
        tool("set_view", "Move the user's camera to look at a point (map frame).", json!({ "target": { "type": "array", "items": { "type": "number" } }, "distance": { "type": "number" }, "topDown": { "type": "boolean" } }), &["target"]),
        tool("query_region", "Probe the map: how many voxels are in a region, their min/max corner, how many lie on horizontal vs vertical surfaces, and a sample of positions. Use it to find objects before boxing them.", json!({ "region": region.clone(), "limit": { "type": "integer" } }), &["region"]),
        tool("find_objects", "Clusters of voxels standing on the floor (things like furniture), within a region: each with a tight box, voxel count and height. Floor, ceiling and walls are excluded. Good first step for 'box every chair'.", json!({ "region": region.clone(), "minVoxels": { "type": "integer" }, "maxHeight": { "type": "number", "description": "ignore clusters taller than this above the floor (m), default 2.0" } }), &[]),
        tool("fit_box", "The tightest oriented box around the non-floor voxels inside a rough box. Pass a generous region; the result hugs the object. Optionally adds it as an annotation (add=true, label).", json!({ "box": box_schema.clone(), "label": { "type": "string" }, "add": { "type": "boolean" } }), &["box"]),
        tool("add_box", "Add a labelled 3D bounding box annotation the user can edit.", json!({ "label": { "type": "string" }, "box": box_schema.clone() }), &["label", "box"]),
        tool("add_plane", "Add a labelled plane annotation (a rectangle with a normal), e.g. a wall face or a ramp.", json!({ "label": { "type": "string" }, "center": { "type": "array", "items": { "type": "number" } }, "normal": { "type": "array", "items": { "type": "number" } }, "size": { "type": "array", "items": { "type": "number" }, "description": "[width, height]" } }), &["label", "center", "normal", "size"]),
        tool("add_point", "Add a labelled 3D point annotation.", json!({ "label": { "type": "string" }, "position": { "type": "array", "items": { "type": "number" } } }), &["label", "position"]),
        tool("list_annotations", "Every annotation: boxes, planes, points, floors, named plan points, areas.", json!({}), &[]),
        tool("update_annotation", "Change an annotation's fields by id (e.g. {\"label\": \"desk\"} or {\"box\": {\"center\": [..]}}).", json!({ "id": { "type": "string" }, "patch": { "type": "object" } }), &["id", "patch"]),
        tool("delete_annotation", "Delete an annotation by id.", json!({ "id": { "type": "string" } }), &["id"]),
        tool("cleanup", "Remove voxels (undoable). op: floating (small disconnected clusters: specks, ghosts; param minVoxels, default 30), outliers (statistical; neighbors, stdRatio), floor (the floor surface; thickness), walls (vertical surfaces; minHeight), keepWalls (everything but walls). preview=true only counts.", json!({ "op": { "type": "string", "enum": ["floating", "outliers", "floor", "walls", "keepWalls"] }, "region": region.clone(), "params": { "type": "object" }, "preview": { "type": "boolean" } }), &["op"]),
        tool("crop", "Crop the map (undoable): kind=box keeps only voxels inside box; kind=height keeps zMin..zMax; kind=delete removes voxels inside box.", json!({ "kind": { "type": "string", "enum": ["box", "height", "delete"] }, "box": box_schema.clone(), "zMin": { "type": "number" }, "zMax": { "type": "number" } }), &["kind"]),
        tool("rotate_map", "Spin the map about +z through its center (degrees). Annotations move with it.", json!({ "degrees": { "type": "number" } }), &["degrees"]),
        tool("level_map", "Tilt the map so the main floor is flat and at z = 0.", json!({}), &[]),
        tool("generate_floor_plans", "Make 2D floor plans (occupancy: walls/furniture vs free floor) from the 3D map, one per storey when multiFloor (default true). Optional explicit floor heights.", json!({ "multiFloor": { "type": "boolean" }, "levels": { "type": "array", "items": { "type": "number" } } }), &[]),
        tool("get_floor_plan", "A floor plan as an image (white free, black occupied, grey unknown; +y up) with its origin and resolution so pixels map to meters, plus that floor's named points and areas.", json!({ "floor": { "type": "integer" } }), &["floor"]),
        tool("add_plan_point", "Name a spot on a floor plan (map-frame x, y in meters), e.g. a dock or a door.", json!({ "floor": { "type": "integer" }, "name": { "type": "string" }, "position": { "type": "array", "items": { "type": "number" } } }), &["floor", "name", "position"]),
        tool("add_area", "Add a named area on a floor plan: kind \"no-go\" (navigation must avoid it), \"zone\" (a named room/region), or another word. polygon = [[x, y], ...] map-frame meters, at least 3 corners.", json!({ "floor": { "type": "integer" }, "name": { "type": "string" }, "kind": { "type": "string" }, "polygon": { "type": "array", "items": { "type": "array", "items": { "type": "number" } } } }), &["floor", "name", "kind", "polygon"]),
        tool("add_polygon", "Add a polygon annotation on a storey: corners [[x, y], ...] (map-frame meters, at least 3), standing up from the local floor by height (default 1 m; base overrides the floor). Shows in 2D as an outline and in 3D as a prism.", json!({ "floor": { "type": "integer" }, "label": { "type": "string" }, "polygon": { "type": "array", "items": { "type": "array", "items": { "type": "number" } } }, "height": { "type": "number" }, "base": { "type": "number" } }), &["floor", "label", "polygon"]),
        tool("erase", "Erase (undoable) what stands on a storey's floor along a brush path [[x, y], ...] of the given radius: every voxel from one voxel over the local floor up to zEnd (m over the floor when relative, default true; or fullColumn=true for everything up to the next storey), then patches the floor under it from the floor around. For couches, people, clutter.", json!({ "floor": { "type": "integer" }, "path": { "type": "array", "items": { "type": "array", "items": { "type": "number" } } }, "radius": { "type": "number" }, "zEnd": { "type": "number" }, "relative": { "type": "boolean" }, "fullColumn": { "type": "boolean" } }), &["floor", "path", "radius"]),
        tool("draw", "Add voxels (undoable) along a path [[x, y], ...] of the given width on a storey, from the local floor up to height (m, default 1): a wall or obstacle the robot should see.", json!({ "floor": { "type": "integer" }, "path": { "type": "array", "items": { "type": "array", "items": { "type": "number" } } }, "width": { "type": "number" }, "height": { "type": "number" } }), &["floor", "path"]),
        tool("straighten_wall", "Replace a noisy wall (undoable) along the line from..to [x, y] on a storey: wall voxels within width/2 of the line become one straight wall on the fitted line, keeping its height and doorways. zEnd / relative / fullColumn as for erase (default: up to 2.5 m over the floor).", json!({ "floor": { "type": "integer" }, "from": { "type": "array", "items": { "type": "number" } }, "to": { "type": "array", "items": { "type": "number" } }, "width": { "type": "number" }, "zEnd": { "type": "number" }, "relative": { "type": "boolean" }, "fullColumn": { "type": "boolean" } }), &["floor", "from", "to"]),
        tool("undo", "Undo the last edit (anyone's).", json!({}), &[]),
        tool("redo", "Redo the last undone edit.", json!({}), &[]),
        tool("build_map", "Start (re)building the global map from the recording (loop closure + ray tracing). Runs in the background; poll get_status for progress.", json!({ "voxelSize": { "type": "number" }, "loopClosure": { "type": "boolean" } }), &[]),
        tool("save_to_recording", "Write the map and every annotation into the recording file (runs in the background).", json!({}), &[]),
    ])
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// `?toolset=basic` serves the first version of the tools (no find_objects / fit_box / query_region, plain
/// screenshots): kept for the agent evaluation's before/after (eval/), not for use.
#[derive(serde::Deserialize, Default)]
pub struct Options {
    #[serde(default)]
    toolset: String,
}

pub async fn handle(State(app): State<Arc<App>>, axum::extract::Query(options): axum::extract::Query<Options>, headers: HeaderMap, Json(request): Json<Value>) -> Response {
    let _ = headers;
    let basic = options.toolset == "basic";
    // a batch is answered as one
    if let Some(batch) = request.as_array() {
        let mut answers = Vec::new();
        for item in batch {
            if let Some(answer) = answer(&app, item.clone(), basic).await {
                answers.push(answer);
            }
        }
        return Json(Value::Array(answers)).into_response();
    }
    match answer(&app, request, basic).await {
        Some(answer) => {
            let mut response = Json(answer).into_response();
            response.headers_mut().insert("mcp-session-id", "map-builder".parse().unwrap());
            response
        }
        None => StatusCode::ACCEPTED.into_response(),
    }
}

const ADVANCED: [&str; 3] = ["find_objects", "fit_box", "query_region"];

async fn answer(app: &Arc<App>, request: Value, basic: bool) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request["method"].as_str().unwrap_or_default().to_string();
    let id = id?; // notifications get no answer
    Some(match method.as_str() {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": request["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "dim-map-builder", "version": env!("CARGO_PKG_VERSION") },
                "instructions": if basic { GUIDE_BASIC } else { GUIDE },
            }
        }),
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        "tools/list" => {
            let mut list = tools();
            if basic {
                list.as_array_mut().unwrap().retain(|tool| !ADVANCED.contains(&tool["name"].as_str().unwrap_or_default()));
                for tool in list.as_array_mut().unwrap() {
                    if let Some(text) = tool["description"].as_str() {
                        tool["description"] = json!(text.replace(GUIDE, GUIDE_BASIC).replace(" with a 1 m grid, axis labels and annotation labels drawn on it", ""));
                    }
                }
            }
            json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": list } })
        }
        "tools/call" => {
            let name = request["params"]["name"].as_str().unwrap_or_default().to_string();
            let arguments = request["params"]["arguments"].clone();
            let outcome = if basic && ADVANCED.contains(&name.as_str()) { Err(anyhow::anyhow!("unknown tool {name}")) } else { call(app, &name, arguments, basic).await };
            let content = match outcome {
                Ok(content) => json!({ "content": content, "isError": false }),
                Err(error) => json!({ "content": [{ "type": "text", "text": format!("error: {error:#}") }], "isError": true }),
            };
            json!({ "jsonrpc": "2.0", "id": id, "result": content })
        }
        _ => rpc_error(id, -32601, &format!("unknown method {method}")),
    })
}

fn text(value: Value) -> Vec<Value> {
    vec![json!({ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() })]
}

fn parse_box(value: &Value) -> Result<Box3> {
    let b: crate::session::Box3Json = serde_json::from_value(value.clone()).context("box needs center [x,y,z] and size [dx,dy,dz]")?;
    if b.size.iter().any(|s| *s <= 0.0) {
        bail!("box sizes must be > 0");
    }
    Ok(b.into())
}

/// "view" → the user's camera frustum (from the page's last report), "all", or a box.
fn parse_region(workspace: &Workspace, value: &Value) -> Result<Region> {
    match value {
        Value::Null => Ok(Region::All),
        Value::String(kind) if kind == "all" => Ok(Region::All),
        Value::String(kind) if kind == "view" => {
            let matrix = &workspace.session.view["viewProjection"];
            let matrix: [f32; 16] = serde_json::from_value(matrix.clone()).context("the page hasn't reported its view yet (is the Map Builder open?)")?;
            Ok(Region::View { matrix })
        }
        other => Ok(Region::Box { region: parse_box(other)?.into() }),
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work).await?
}

pub async fn call(app: &Arc<App>, name: &str, args: Value, basic: bool) -> Result<Vec<Value>> {
    let (id, workspace) = app.target(args["session"].as_str())?;
    let app2 = app.clone();
    let id2 = id.clone();
    // edits go through App::mutate (autosave + live update in the page)
    let edit = move |change: Box<dyn FnOnce(&mut Workspace) -> Result<Value> + Send>| async move { blocking(move || app2.mutate(&id2, change)).await };
    Ok(match name {
        "get_status" => {
            let summary = crate::api::summary(app, &workspace.lock().unwrap());
            let annotations = &summary["annotations"];
            text(json!({
                "session": id,
                "recording": summary["recordingPath"],
                "stage": summary["stage"],
                "voxels": summary["voxels"],
                "voxelSize": summary["voxelSize"],
                "bounds": summary["bounds"],
                "floors": annotations["floors"],
                "counts": {
                    "boxes": annotations["boxes"].as_array().map_or(0, |a| a.len()),
                    "planes": annotations["planes"].as_array().map_or(0, |a| a.len()),
                    "points": annotations["points"].as_array().map_or(0, |a| a.len()),
                    "areas": annotations["areas"].as_array().map_or(0, |a| a.len()),
                },
                "job": summary["job"],
                "canUndo": summary["undoLabel"],
                "unsaved": summary["unsaved"],
                "recent": summary["history"],
            }))
        }
        "get_view" => {
            let view = workspace.lock().unwrap().session.view.clone();
            let mut content = text(json!({
                "camera": view.get("camera"),
                "visibleBounds": view.get("visibleBounds"),
                "selected": view.get("selected"),
                "note": "map-frame meters; the screenshot's grid lines are 1 m apart, labelled at their x/y values",
            }));
            if args["screenshot"].as_bool().unwrap_or(true) {
                let image = app.capture(json!({ "overlay": !basic, "topDown": args["topDown"].as_bool().unwrap_or(false) })).await?;
                let data = image.split_once(",").map_or(image.as_str(), |(_, data)| data).to_string();
                content.push(json!({ "type": "image", "data": data, "mimeType": "image/png" }));
            }
            content
        }
        "set_view" => {
            let target: [f32; 3] = serde_json::from_value(args["target"].clone()).context("target [x,y,z]")?;
            app.emit(json!({ "type": "setView", "target": target, "distance": args["distance"], "topDown": args["topDown"] }));
            text(json!({ "ok": true }))
        }
        "query_region" => {
            let workspace = workspace.clone();
            blocking(move || {
                let ws = workspace.lock().unwrap();
                let region = parse_region(&ws, &args["region"])?;
                Ok(text(ws.query(&region, args["limit"].as_u64().unwrap_or(40) as usize)))
            })
            .await?
        }
        "find_objects" => {
            let workspace = workspace.clone();
            blocking(move || {
                let ws = workspace.lock().unwrap();
                let region = parse_region(&ws, &args["region"])?;
                Ok(text(json!({ "objects": find_objects(&ws, &region, args["minVoxels"].as_u64().unwrap_or(25) as usize, args["maxHeight"].as_f64().unwrap_or(2.0) as f32) })))
            })
            .await?
        }
        "fit_box" => {
            let region = parse_box(&args["box"])?;
            let (fit, used) = {
                let workspace = workspace.clone();
                blocking(move || workspace.lock().unwrap().fit_box(&region)).await?
            };
            let mut result = json!({ "box": crate::session::Box3Json::from(fit), "voxels": used });
            if args["add"].as_bool().unwrap_or(false) {
                let label = args["label"].as_str().unwrap_or("object").to_string();
                let created = edit(Box::new(move |w| Ok(json!(w.add_box(&label, fit, "agent")?)))).await?;
                result["id"] = created;
            }
            text(result)
        }
        "add_box" => {
            let region = parse_box(&args["box"])?;
            let label = args["label"].as_str().context("label")?.to_string();
            text(json!({ "id": edit(Box::new(move |w| Ok(json!(w.add_box(&label, region, "agent")?)))).await? }))
        }
        "erase" | "draw" | "straighten_wall" => {
            let mut body = args.clone();
            let defaults = |body: &mut Value, key: &str, value: Value| {
                if body.get(key).is_none_or(|v| v.is_null()) {
                    body[key] = value;
                }
            };
            body["tool"] = json!(match name {
                "erase" => "erase",
                "draw" => "draw",
                _ => "straighten",
            });
            defaults(&mut body, "relative", json!(true));
            defaults(&mut body, "zEnd", json!(if name == "erase" { 1.8 } else { 2.5 }));
            defaults(&mut body, "width", json!(if name == "draw" { 0.1 } else { 0.4 }));
            defaults(&mut body, "height", json!(1.0));
            if let Some(object) = body.as_object_mut() {
                object.remove("session");
            }
            let request: crate::workspace::Modify = serde_json::from_value(body).context("arguments")?;
            let result = edit(Box::new(move |w| Ok(json!(w.modify(&request)?)))).await?;
            text(result)
        }
        "add_polygon" => {
            let mut body = args.clone();
            body["type"] = json!("prism");
            if let Some(object) = body.as_object_mut() {
                object.remove("session");
            }
            let annotation: NewAnnotation = serde_json::from_value(body).context("arguments")?;
            text(json!({ "id": edit(Box::new(move |w| Ok(json!(add(w, annotation, "agent")?)))).await? }))
        }
        "add_plane" | "add_point" | "add_plan_point" | "add_area" => {
            let mut body = args.clone();
            body["type"] = json!(match name {
                "add_plane" => "plane",
                "add_point" => "point",
                "add_plan_point" => "planPoint",
                _ => "area",
            });
            if body.get("label").is_none() {
                body["label"] = body.get("name").cloned().unwrap_or(json!(""));
            }
            let annotation: NewAnnotation = serde_json::from_value(body).context("arguments")?;
            text(json!({ "id": edit(Box::new(move |w| Ok(json!(add(w, annotation, "agent")?)))).await? }))
        }
        "list_annotations" => text(serde_json::to_value(&workspace.lock().unwrap().session.annotations)?),
        "update_annotation" => {
            let target = args["id"].as_str().context("id")?.to_string();
            let patch = args["patch"].clone();
            edit(Box::new(move |w| w.update_annotation(&target, Some(&patch)).map(|_| json!(null)))).await?;
            text(json!({ "ok": true }))
        }
        "delete_annotation" => {
            let target = args["id"].as_str().context("id")?.to_string();
            edit(Box::new(move |w| w.update_annotation(&target, None).map(|_| json!(null)))).await?;
            text(json!({ "ok": true }))
        }
        "cleanup" => {
            let op = args["op"].as_str().context("op")?.to_string();
            if !["floating", "outliers", "floor", "walls", "keepWalls"].contains(&op.as_str()) {
                bail!("op must be floating, outliers, floor, walls or keepWalls");
            }
            let region = parse_region(&workspace.lock().unwrap(), &args["region"])?;
            let params = args["params"].clone();
            let result = if args["preview"].as_bool().unwrap_or(false) {
                let workspace = workspace.clone();
                blocking(move || workspace.lock().unwrap().op(&op, &params, &region, true)).await?
            } else {
                serde_json::from_value(edit(Box::new(move |w| Ok(serde_json::to_value(w.op(&op, &params, &region, false)?)?))).await?)?
            };
            text(json!({ "label": result.label, "changed": result.changed, "remaining": result.remaining, "preview": result.preview.is_some() }))
        }
        "crop" => {
            let kind = args["kind"].as_str().context("kind")?.to_string();
            let (op, region, params) = match kind.as_str() {
                "box" => ("cropOutside", Region::Box { region: parse_box(&args["box"])?.into() }, json!({})),
                "delete" => ("deleteInside", Region::Box { region: parse_box(&args["box"])?.into() }, json!({})),
                "height" => ("cropHeight", Region::All, json!({ "zMin": args["zMin"], "zMax": args["zMax"] })),
                _ => bail!("kind must be box, height or delete"),
            };
            let result = edit(Box::new(move |w| Ok(serde_json::to_value(w.op(op, &params, &region, false)?)?))).await?;
            text(json!({ "changed": result["changed"], "remaining": result["remaining"] }))
        }
        "rotate_map" => {
            let degrees = args["degrees"].as_f64().context("degrees")?;
            edit(Box::new(move |w| w.rotate_yaw(degrees).map(|_| json!(null)))).await?;
            text(json!({ "ok": true }))
        }
        "level_map" => {
            edit(Box::new(|w| w.level().map(|up| json!([up.x, up.y, up.z])))).await?;
            text(json!({ "ok": true }))
        }
        "generate_floor_plans" => {
            let multi = args["multiFloor"].as_bool().unwrap_or(true);
            let levels: Option<Vec<f32>> = serde_json::from_value(args["levels"].clone()).ok();
            text(json!({ "floors": edit(Box::new(move |w| Ok(serde_json::to_value(w.generate_plans(multi, levels, Default::default())?)?))).await? }))
        }
        "get_floor_plan" => {
            let floor = args["floor"].as_u64().context("floor")? as usize;
            let ws = workspace.lock().unwrap();
            let plan = ws.session.plans.get(floor).context("no such floor plan (generate_floor_plans first)")?;
            let annotations = &ws.session.annotations;
            let info = json!({
                "floor": floor,
                "z": plan.z,
                "resolution": plan.resolution,
                "origin": plan.origin,
                "width": plan.width,
                "height": plan.height,
                "pixelToMeters": "x = origin[0] + (column + 0.5) * resolution; y = origin[1] + (height - 1 - row + 0.5) * resolution (row 0 is the image's top)",
                "points": annotations.plan_points.iter().filter(|p| p.floor == floor).collect::<Vec<_>>(),
                "areas": annotations.areas.iter().filter(|a| a.floor == floor).collect::<Vec<_>>(),
            });
            use base64::Engine;
            let image = base64::engine::general_purpose::STANDARD.encode(plan.png());
            let mut content = text(info);
            content.push(json!({ "type": "image", "data": image, "mimeType": "image/png" }));
            content
        }
        "undo" => text(json!({ "undone": edit(Box::new(|w| Ok(json!(w.undo())))).await? })),
        "redo" => text(json!({ "redone": edit(Box::new(|w| Ok(json!(w.redo())))).await? })),
        "build_map" => {
            let mut options = mapping::build::BuildOptions::default();
            if let Some(size) = args["voxelSize"].as_f64() {
                options.voxel_size = size as f32;
            }
            if let Some(loops) = args["loopClosure"].as_bool() {
                options.loop_closure = loops;
            }
            text(json!({ "job": app.build(&id, options)? }))
        }
        "save_to_recording" => text(json!({ "job": app.save(&id)? })),
        other => bail!("unknown tool {other}"),
    })
}

/// Things standing on a floor: voxels above the floor (not floor, not walls taller than `max_height`, not ceiling),
/// grouped by 2D connectivity of their footprint, each with its fitted box.
pub fn find_objects(ws: &Workspace, region: &Region, min_voxels: usize, max_height: f32) -> Vec<Value> {
    let (_, points, normals) = ws.visible_points();
    let voxel = ws.map.as_ref().map_or(0.05, |m| m.voxel_size);
    let floors: Vec<f32> = if ws.session.annotations.floors.is_empty() {
        mapping::voxels::floor_levels(&points, &normals, 1.8)
    } else {
        ws.session.annotations.floors.iter().map(|f| f.z).collect()
    };
    let floor_below = |z: f32| floors.iter().copied().filter(|f| *f <= z + 0.05).fold(f32::NEG_INFINITY, f32::max);
    let mut cells: std::collections::HashMap<(i32, i32, i32), Vec<usize>> = std::collections::HashMap::new();
    let cell = (voxel * 2.0).max(0.08);
    for (index, p) in points.iter().enumerate() {
        if !region.contains(*p) {
            continue;
        }
        let floor = floor_below(p[2]);
        if !floor.is_finite() {
            continue;
        }
        let above = p[2] - floor;
        if above < 0.06 || above > max_height {
            continue;
        }
        let storey = floors.iter().position(|f| *f == floor).unwrap_or(0) as i32;
        cells.entry(((p[0] / cell).floor() as i32, (p[1] / cell).floor() as i32, storey)).or_default().push(index);
    }
    // footprints connected in 2D (8-neighbour) per storey; tall groups that run long are walls, skipped
    let mut seen = std::collections::HashSet::new();
    let mut objects = Vec::new();
    let keys: Vec<_> = cells.keys().copied().collect();
    for start in keys {
        if !seen.insert(start) {
            continue;
        }
        let mut stack = vec![start];
        let mut members = Vec::new();
        while let Some(k) = stack.pop() {
            members.extend(cells[&k].iter().copied());
            for dx in -1..=1 {
                for dy in -1..=1 {
                    let n = (k.0 + dx, k.1 + dy, k.2);
                    if cells.contains_key(&n) && seen.insert(n) {
                        stack.push(n);
                    }
                }
            }
        }
        if members.len() < min_voxels {
            continue;
        }
        let group: Vec<[f32; 3]> = members.iter().map(|i| points[*i]).collect();
        let bounds = Box3::from_bounds(
            std::array::from_fn(|axis| group.iter().map(|p| p[axis]).fold(f32::MAX, f32::min) - 0.01),
            std::array::from_fn(|axis| group.iter().map(|p| p[axis]).fold(f32::MIN, f32::max) + 0.01),
        );
        let Some((fit, used)) = mapping::voxels::fit_box(&group, None, &bounds, &[]) else { continue };
        let long = fit.size[0].max(fit.size[1]);
        if long > 4.0 && fit.size[2] > 1.2 {
            continue; // a wall run, not an object
        }
        let floor = floor_below(fit.center[2]);
        objects.push(json!({
            "box": crate::session::Box3Json::from(fit),
            "voxels": used,
            "heightAboveFloor": ((fit.center[2] + fit.size[2] / 2.0 - floor) * 100.0).round() / 100.0,
            "footprint": [(fit.size[0] * 100.0).round() / 100.0, (fit.size[1] * 100.0).round() / 100.0],
        }));
    }
    objects.sort_by(|a, b| b["voxels"].as_u64().cmp(&a["voxels"].as_u64()));
    objects.truncate(60);
    objects
}
