# Map Builder server API

`dimos-app-server` serves the page, this API under `/api`, and the agent's endpoints at `/agent.json` and `/agent/<name>` ([agent-tools.md](agent-tools.md)).
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
| `GET /api/events/ws` | websocket, one JSON event per message (the page uses this; `GET /api/events` is the same stream as SSE, kept for older pages) | `{type:"job", job}`, `{type:"session", id, revision}`, `{type:"preview"}`, `{type:"capture", request, options}`, `{type:"setView", ...}` |
| `POST /api/captures/:request` | a PNG data URL | the page answering a `capture` (the agent's screenshot) |

`op` is `floating` (`minVoxels`), `outliers` (`neighbors`, `stdRatio`), `floor` (`thickness`), `walls` (`minHeight`),
`keepWalls`, `cropOutside` / `deleteInside` (box region), `cropHeight` (`zMin`, `zMax`). `region` is `{ kind: "all" }`,
`{ kind: "box", center, size, yaw }` or `{ kind: "view", matrix }` (a column-major view-projection matrix).

Jobs: `{ id, kind: "build" | "preview" | "save", state: "running" | "done" | "failed" | "cancelled", progress: { stage,
stageIndex, stageCount, done, total, note }, fraction, elapsed, etaSeconds, error }`.

Sessions are autosaved under `$DIMOS_APP_DATA/sessions/<id>/` (`session.json`, `map.bin`, `removed.bin`) on every change.

## Desktop endpoints this app uses: uploads + cloud login

Served by dimOS Desktop's dimos server (dimos-desktop `docs/api.md`, branch `jeff/desktop_uploads` or later) and called
relative to the app (`../../dimos/...`). Declared in `dimos.yaml` `dimos-api:`, so an older Desktop refuses to install
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
  code, polled every 1.5 s), then the recording is queued. Unsaved edits → "Save, then upload" (waits for the save job)
  or "Upload as last saved".
- The page polls `GET /dimos/uploads` every second while anything is active or the drawer is open, every 10 s otherwise
  (Desktop also pushes `{type: "upload"}` on `/dimos/events`; the page doesn't hold that SSE connection).
- A 404 / 405 from these means a Desktop from before them: the page says it needs a newer Desktop.
- Page errors go to Desktop's error feed (`POST ../../api/errors`, dim-app `errors.js`) for its agent.

Dev only: `MOCK_UPLOADS=1 npm run dev` (in `frontend/`) answers these and `/api/errors` from an in-memory mock
(`frontend/dev/mockUploads.ts`: login approves itself after 6 s, 4 MB/s uploads, a path containing "fail" fails).
Without it the dev server forwards `/dimos` to `DESKTOP_URL`.
