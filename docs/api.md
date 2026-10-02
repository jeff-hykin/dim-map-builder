# Map Builder server API

`dimos-app-server` serves the page, this API under `/api`, and the agent tools at `/mcp` ([agent-tools.md](agent-tools.md)).
Through Desktop it's all under `/apps/dim-map-builder/`. JSON unless noted; errors are `{ "error": "..." }`.

| Method + path | Body | Response |
| --- | --- | --- |
| `GET /api/state` | | `{ active, session: Session \| null, recordingsDir }` — the session the page last had open |
| `POST /api/open` | `{ id, path, name }` (a Desktop recording) | `Session` (resumed if one exists; restored from an earlier save in the file otherwise) |
| `GET /api/sessions/:id` | | `Session` |
| `DELETE /api/sessions/:id` | | discards the working copy (the recording is untouched) |
| `GET /api/sessions/:id/points.bin` | | f32 little-endian xyz of every visible voxel, map frame |
| `GET /api/sessions/:id/paths` | | `{ raw, corrected, loops }` in the map frame |
| `GET /api/sessions/:id/preview.bin` | | the raw recording at a glance; `202 { job }` while it's being read |
| `GET /api/build-defaults` | | the default `BuildOptions` (voxelSize, loopClosure, rayTracing, every, maxRange, tfTolerance, worldFrame, cloudStream, `ray: {...}`, `pgo: {...}`) |
| `POST /api/sessions/:id/build` | `BuildOptions` (all optional) | `{ job }` |
| `DELETE /api/sessions/:id/job` | | `{ cancelled }` |
| `POST /api/sessions/:id/op` | `{ op, region, params, preview }` | `{ label, changed, remaining, preview? }` |
| `POST /api/sessions/:id/transform` | `{ kind: "rotate", degrees }` \| `{ kind: "level" }` \| `{ kind: "set", transform }` | `{ ok }` |
| `PUT /api/sessions/:id/slice` | `Slice` or `null` | `{ ok }` — the slicer's view (undoable, saved as `map/slice`) |
| `GET /api/sessions/:id/alignment` | | `{ yaw }` — the turn (radians) that lines the walls up with x and y |
| `GET /api/sessions/:id/floor` | | the local floor: `{ cell, origin, width, height, storeys: [{ level, band, heights (null = unknown), measured }] }` |
| `POST /api/sessions/:id/modify` | `{ tool: "erase", floor, path, radius, zEnd?, relative?, fullColumn? }` \| `{ tool: "draw", floor, path, width, height }` \| `{ tool: "straighten", floor, from, to, width, zEnd?, relative?, fullColumn? }` | `{ label, changed, remaining }` — one undoable edit |
| `POST /api/sessions/:id/annotations` | `{ type: "box" \| "plane" \| "point" \| "planPoint" \| "area" \| "prism" \| "view", ... }` | `{ id }` |
| `PATCH /api/sessions/:id/annotations/:aid` | any fields | `{ ok }` |
| `DELETE /api/sessions/:id/annotations/:aid` | | `{ ok }` |
| `POST /api/sessions/:id/fit-box` | `{ box }` | `{ box, voxels }` |
| `POST /api/sessions/:id/query` | `{ region, limit }` | voxel counts, bounds, sample |
| `POST /api/sessions/:id/plans` | `{ multiFloor, levels?, options? }` | `{ floors }` |
| `GET /api/sessions/:id/plans/:n.png` | | the plan image |
| `POST /api/sessions/:id/undo` · `/redo` | | `{ undone }` · `{ redone }` |
| `PUT /api/sessions/:id/view` | camera, view-projection, visible bounds, UI state | saved with the session (not an edit) |
| `POST /api/sessions/:id/save` | | `{ job }` — write into the recording |
| `GET /api/events` | SSE | `{type:"job", job}`, `{type:"session", id, revision}`, `{type:"preview"}`, `{type:"capture", request, options}`, `{type:"setView", ...}` |
| `POST /api/captures/:request` | a PNG data URL | the page answering a `capture` (the agent's screenshot) |

`op` is `floating` (`minVoxels`), `outliers` (`neighbors`, `stdRatio`), `floor` (`thickness`), `walls` (`minHeight`),
`keepWalls`, `cropOutside` / `deleteInside` (box region), `cropHeight` (`zMin`, `zMax`). `region` is `{ kind: "all" }`,
`{ kind: "box", center, size, yaw }` or `{ kind: "view", matrix }` (a column-major view-projection matrix).

Jobs: `{ id, kind: "build" | "preview" | "save", state: "running" | "done" | "failed" | "cancelled", progress: { stage,
stageIndex, stageCount, done, total, note }, fraction, elapsed, etaSeconds, error }`.

Sessions are autosaved under `$DIMOS_APP_DATA/sessions/<id>/` (`session.json`, `map.bin`, `removed.bin`) on every change.
