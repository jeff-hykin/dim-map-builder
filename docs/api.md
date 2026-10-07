# Map Editor server API

`dimos-app-server` serves the page and every action as an HTTP endpoint. The page calls these, and Desktop's agent
calls the same ones: `server/src/api.rs` registers each route with its description and params (`routes.rs`), and that
one table is the served `GET /agent.json`. `dimos.yaml`'s `agent:` repeats it (`deno task check-endpoints --write`
regenerates it; CI fails when they differ). Through Desktop it's all under `/apps/dim-map-builder/`; Desktop's MCP finds
them with `search_endpoints` and runs them with `call_endpoint` (ids like `dim-map-builder:POST api/sessions/{id}/fit-box`,
params fill `{id}` first). JSON unless noted; errors are `{ "error": "..." }` with a 4xx status. Images (the view's
screenshot, a floor plan) come back as `images: [{ mimeType, data }]`, which Desktop hands the model as images.

`{id}` is a session id or **`current`**: the recording open in the page. Every edit is the same undoable edit a user
makes (autosaved, a `session` event to every open page), so it shows up live and Undo reverts it.

Coordinates are **meters in the map frame** (+z up; after levelling, the main floor is z = 0). Boxes are
`{ center: [x, y, z], size: [dx, dy, dz], yaw }` (full extents, yaw in radians about +z). A `region` is `"view"` (what
the user's camera sees now), `"all"`, a box, or the page's form `{ kind: "all" | "view" | "box", ... }`.

| Method + path | What |
| --- | --- |
| `GET api/status` (context) | the open map briefly: recording, stage, voxels, bounds, floors, annotation counts, job, unsaved, recent history |
| `GET api/state` | what the page loads: `{ active, session, last, recordingsDir }` (only a recording opened since the server started is open; `last` is the one before, offered on the empty page) |
| `POST api/open` | `{ path, name?, id?, writable? }`: open a recording; every page follows (`opened` event) |
| `GET api/build-defaults` | the default build options |
| `GET api/view` (view) | camera, visible bounds, selection, UI, and a screenshot with a 1 m grid and labels (`screenshot`, `topDown`) |
| `POST api/camera` | `{ target, distance?, topDown? }`: move the user's camera (`setView` event) |
| `PATCH api/ui` | `{ mode, tool, planFloor, look, showPaths, floorOverlay, paletteOpen, scope, slice }`: what the page shows (`ui` event) |
| `GET` / `DELETE api/sessions/{id}` | a session's full state / discard the working copy |
| `GET api/sessions/{id}/paths` | `{ raw, corrected, loops }` |
| `POST api/sessions/{id}/build` | build options (all optional): `{ job }` |
| `DELETE api/sessions/{id}/job` | cancel the running job |
| `POST api/sessions/{id}/op` | `{ op, region, params, preview }`: floating, outliers, floor, walls, keepWalls, cropOutside, deleteInside, cropHeight |
| `POST api/sessions/{id}/transform` | `{ kind: "rotate", degrees }` / `{ kind: "level" }` / `{ kind: "set", transform }` |
| `GET api/sessions/{id}/floor` | the local floor per storey |
| `PUT api/sessions/{id}/slice` | the slicer's view (or `null`) |
| `GET api/sessions/{id}/alignment` | `{ yaw }` that lines the walls up with x and y |
| `POST api/sessions/{id}/modify` | `{ tool: "erase" \| "draw" \| "straighten", floor, ... }` |
| `GET` / `POST api/sessions/{id}/annotations` | list / add (`type`: box, plane, point, planPoint, area, prism, view) |
| `PATCH` / `DELETE api/sessions/{id}/annotations/{annotation}` | change / remove |
| `POST api/sessions/{id}/fit-box` | `{ box, label?, add? }`: the tightest box around what's in a rough box |
| `POST api/sessions/{id}/query` | `{ region, limit }`: voxel counts, bounds, surfaces, a sample |
| `POST api/sessions/{id}/find-objects` | `{ region, minVoxels, maxHeight }`: furniture-like clusters with boxes |
| `POST api/sessions/{id}/plans` | `{ multiFloor, levels? }`: 2D floor plans |
| `GET api/sessions/{id}/plans/{floor}` | a plan (JSON + image, origin, resolution, its points and areas); `{floor}.png` is the bare image |
| `POST api/sessions/{id}/undo` · `redo` | anyone's last edit |
| `POST api/sessions/{id}/save` | write the map into the recording (a job) |
| `POST api/sessions/{id}/upload` | `{ saveFirst = true }`: queue the recording in Desktop's upload queue (after a save when there are unsaved edits) |

The page's plumbing, not actions (not in agent.json): `GET api/sessions/{id}/points.bin` (f32 xyz of the visible
voxels), `GET api/sessions/{id}/preview.bin` (the raw recording at a glance; 202 while it's read), `PUT
api/sessions/{id}/view` (the page reports its camera and UI state, saved with the session), `POST api/captures/{n}` (the
page answering a screenshot request). Events reach the page over zenoh (Desktop's docs/events.md): the server POSTs each
to Desktop's relay (`/desktop/frontend/<name>/events`), which publishes it on `<ns>/apps/<name>/frontend/events`, and
the page hears it on its one zenoh-gateway connection (dim-app's appEvents), in order: `job`, `session`, `preview`,
`opened`, `discarded`, `ui`, `setView`, `capture`, `upload`. After that connection comes back the page re-reads.

Jobs: `{ id, kind: "build" | "preview" | "save", state: "running" | "done" | "failed" | "cancelled", progress: { stage,
stageIndex, stageCount, done, total, note }, fraction, elapsed, etaSeconds, error }`.

Sessions are autosaved under `<dataDir>/sessions/<id>/` (the app data dir in Desktop's `DIMOS_APP`) (`session.json`, `map.bin`, `removed.bin`) on every change.

## How to box an object well

1. `GET api/view` to see what the user sees (or `find-objects` with `region: "view"` for candidates with boxes).
2. Narrow down: `query` on a rough box around where it should be (counts, heights).
3. `fit-box` with a generous box around it: the result hugs the voxels (floor excluded); `add: true, label: "chair"`.

The evaluation that shaped these endpoints (boxing tasks, IoU against hand-placed boxes) is in `eval/`.

## Desktop endpoints this app uses: uploads + cloud login

Served by dimOS Desktop's dimos server (dimos-desktop `docs/api.md`, branch `jeff/desktop_uploads` or later) and called
relative to the app (`../../dimos/...`); the agent reaches them as Desktop's own endpoints. Queueing this app's recording
goes through `POST api/sessions/{id}/upload` (the backend posts to `/dimos/uploads`, after a save when asked). Declared in `dimos.yaml` `dimos-api:`, so an older Desktop refuses to install
this version. The page code is `frontend/src/core/api.ts` (`cloud`, `uploads`), `ui/useUploads.ts`, `ui/UploadsPanel.tsx`
and `ui/LoginDialog.tsx`. The upload itself runs in Desktop (dimos's `CloudData().upload`), so it outlives the page.

| Method + path | Body | Response |
| --- | --- | --- |
| `GET /dimos/cloud/account` | | `CloudAccount` |
| `POST /dimos/cloud/login` | | `LoginState`: starts dimos's device login (`dimos login`'s flow) |
| `GET /dimos/cloud/login` | | `LoginState` |
| `DELETE /dimos/cloud/login` | | `LoginState`: cancels a pending login |
| `POST /dimos/cloud/logout` | | `CloudAccount` |
| `GET /dimos/uploads` | | `{ uploads: Upload[], waitingForLogin: boolean }`, oldest first |
| `POST /dimos/uploads` | `{ path, robotId?, kind? }` | `Upload`. The same path already queued or uploading → that item. 400 `{ error }` for a missing file or one that isn't `.mcap` / `.db` |
| `DELETE /dimos/uploads/{id}` | | `{ ok: true }`: cancels a queued / uploading one (→ `cancelled`), removes a finished one |
| `POST /dimos/uploads/{id}/retry` | | `Upload`, queued again (at the end) |
| `DELETE /dimos/uploads` | | `{ uploads }`: clears the finished ones (done, failed, cancelled) |

```ts
type CloudAccount = { loggedIn: boolean; email: string | null; scopes: string[] | null; source: "env" | "stored" | null; cloudUrl: string; error: string | null }
type LoginState = {
    state: "idle" | "starting" | "pending" | "approved" | "denied" | "expired" | "failed"
    url: string | null // the page to open
    urlComplete: string | null // the same with the code filled in, when the cloud gives one
    code: string | null // what the user enters there
    expiresAt: number | null // ms since the epoch
    email: string | null // once approved
    error: string | null
}
type Upload = {
    id: string; path: string; name: string; size: number; robotId: string | null; kind: string | null
    state: "queued" | "uploading" | "done" | "failed" | "cancelled"
    phase: "preparing" | "compress" | "upload" | "finishing" | null // only "upload" has byte progress
    bytesDone: number; bytesTotal: number; rateBps: number | null; etaSeconds: number | null // smoothed by Desktop
    uploadId: string | null; skipped: boolean // skipped: already in the cloud
    notice: string | null // e.g. a quota warning on a finished upload
    error: string | null; errorCode: null | "not_logged_in" | "network" | "quota" | "file_missing" | "failed"
    log: string | null // the log file with the details (tracebacks never reach the page)
    createdAt: number; startedAt: number | null; finishedAt: number | null // ms since the epoch
}
```

- One upload at a time, first in first out. When the one at the head finds no login it goes back to `queued` and
  `waitingForLogin` turns true: nothing is lost, the page shows "Uploads are waiting for you to log in", and a completed
  login resumes the queue by itself. `errorCode: "not_logged_in"` on a failed item (e.g. a key revoked mid-upload) gets
  a Log in button beside Retry.
- Upload button: checks `GET /dimos/cloud/account` first; not logged in → the login dialog (open the URL, enter the
  code; approval arrives as the dimos server's `cloud-login` zenoh event), then `POST api/sessions/{id}/upload`. Unsaved edits → "Save, then upload" (the backend
  waits for the save job, then queues it) or "Upload as last saved".
- The page reads `GET /dimos/uploads` once, then follows the dimos server's zenoh events (`<ns>/dimos/events/upload`
  carries the whole upload, progress included; `uploads` and `upload-removed` make it re-read), and re-reads after its
  zenoh-gateway connection comes back. No polling.
- A 404 / 405 from these means a Desktop from before them: the page says it needs a newer Desktop.
- Page errors go to Desktop's error feed (`POST ../../api/errors`, dim-app `errors.js`) for its agent.

Dev only: `MOCK_UPLOADS=1 npm run dev` (in `frontend/`) answers these and `/api/errors` from an in-memory mock
(`frontend/dev/mockUploads.ts`: login approves itself after 6 s, 4 MB/s uploads, a path containing "fail" fails).
Without it the dev server forwards `/dimos` to `DESKTOP_URL`.
