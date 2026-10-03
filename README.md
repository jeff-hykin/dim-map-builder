# dim-map-builder

A [dimOS Desktop](https://github.com/dimensionalOS/dimos-desktop) app that turns a recording into a clean, annotated map:

It's a map viewer first: **[ 3D | Split | 2D ]** at the top (Split = 3D beside a 2D minimap that shows the camera and
moves it on a click). Opening a recording with a map goes straight to it. Only generating the map is needed first;
everything else is optional and in any order.

- **Open** a recording (`.db` or `.mcap`) from Desktop's shared recordings folder (the Live Viewer records there).
- **Generate** (the blue orb): the global map, every scan placed through the recording's tf, loops closed (ICP + pose
  graph), ray traced so free space clears what moved. Voxel size up front; skip loop closure / ray tracing and every
  ray-tracer and pose-graph tunable under Advanced. A progress bar with stage, ETA and Cancel.
- **2D**: a top-down slice of the current voxels between z-start and z-end, over the **local floor** (a per-cell floor
  height that follows ramps and stairs, `crates/mapping/src/floor.rs`) or absolute; "auto" = floor +0.1 to +1.8 m.
  Saved views are the 2D maps (a storey + a height band), saved into the recording.
- **Slicer** (the yellow orb, pulsing until a map has a slice): a 4-step view of the map, not an edit. It flies out to
  the whole map, then cuts it to a height band (live), turns it so the walls line up with x and y (a top-down x-ray
  over a grid, with an "auto" angle from the walls' directions), crops x / y with a draggable rectangle, and flies back
  to the result. 3D, 2D and the minimap all show it; it's saved as seven numbers (`map/slice`).
- **Edit** (the green orb): a palette of tools. **Erase** (brush what stands on the floor away; the floor under it is
  filled in from around), **Draw** / **Line** (voxels from the floor up to a height), **Straighten wall** (drag along
  a noisy wall: the whole wall becomes one clean slab of its own thickness, its corners meet the walls they run into), **Polygon** (an area drawn in 2D that stands up as a prism in 3D),
  **named points and areas** (no-go zones), **3D boxes, planes and points**, **Clean up** (floating specks, outliers,
  floor, walls, crop, level), **Saved views**. All undoable.
- **Save** into the recording: the map, annotations and views become `map/*` streams in the same file
  ([docs/schema.md](docs/schema.md)), ready for a navigation blueprint to read.
- **Upload** the recording (with the map saved in it) to your Dimensional cloud account: the Upload button in the top
  bar, or the Save menu (which offers "Save, then upload" when there are unsaved edits). Not logged in? A dialog shows
  the page to open and the code to approve from any signed-in browser, then the upload starts by itself. Uploads run
  in Desktop (they keep going when the page closes) and queue up; the uploads drawer shows each one's progress, speed
  and time left, with cancel, retry and readable errors.

Every action is an HTTP endpoint the page itself uses, and Desktop's agent calls the same ones (listed at `/agent.json`
and in `dimos.yaml`, [docs/api.md](docs/api.md)):
"clean up the floating voxels in this view", "box every chair", "add a no-go area around the stairs".

Your work is never lost: the working session (map edits, undo history, annotations, plans, camera, panels) is kept
server-side and written to disk on every change, separately from "Save into recording". Refresh, close the tab or
restart Desktop and it comes back as it was.

## Install

This version needs a dimOS Desktop with cloud uploads (the `/dimos/cloud/*` and `/dimos/uploads` endpoints, branch
`jeff/desktop_uploads` or later): it declares them in `dimos.yaml`, and an older Desktop rejects unknown `dimos-api:`
entries, so it won't install there. Page errors also go to Desktop's error feed (`POST /api/errors`, dim-app
`errors.js`) so its agent sees them; an older Desktop just drops those.

In Desktop: App Store → add `https://github.com/jeff-hykin/dim-map-builder` (branch `dimos-desktop2`), or

    dimos-desktop install --name dim-map-builder https://github.com/jeff-hykin/dim-map-builder

## Layout

| Path | What |
| --- | --- |
| `crates/dimos-recording` | read/write dimos recordings without dimos: memory2 `.db`, `.mcap`; LCM + ROS 2 CDR clouds, tf, odometry |
| `crates/mapping` | the map math: the ray-traced voxel map (vendored from dimos), tf tree, ICP, loop closure (a port of dimos's PGO), cleanup selections, floors, floor plans, the staged build |
| `server` | `dimos-app-server`: sessions, jobs, every action as an endpoint (`api.rs`, one table that is also `/agent.json`), save/restore |
| `scripts/check_endpoints.ts` | checks dimos.yaml's `agent:` against the server's `--agent-json` (`--write` regenerates it) |
| `frontend` | the page: React + Vite + three.js; `src/render` is copied from the Live Viewer (same point styles) |
| `docs` | [schema.md](docs/schema.md) (what's saved), [api.md](docs/api.md) (the endpoints) |

One compiled binary at runtime: no Python, no dimos needed to build or edit maps.

## Develop

    cargo test --workspace                      # all the non-UI logic and every endpoint
    deno task check-endpoints [--write]         # dimos.yaml's agent: = the served agent.json
    cargo run -p dimos-app-server -- --port 7190 --frontend frontend/dist
    (cd frontend && npm ci && npm run dev)      # the page, against a running Desktop (DESKTOP_URL)
    nix build .#dimosApp                        # what Desktop builds

## Keyboard

`V` 2D / 3D · `M` split · `E` the edit palette · `X` erase · `D` draw · `L` line · `W` straighten wall · `P` polygon ·
`N` named points / areas · `B` 3D boxes · `C` clean up · `⌘Z` / `⇧⌘Z` undo / redo · `⌘S` save · `F` frame the map ·
`T` top view · `G` / `R` / `S` move / turn / resize the selection · `Del` delete it · `Esc` cancel / put the tool down.
