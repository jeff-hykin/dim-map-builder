# dim-map-builder

A [dimOS Desktop](https://github.com/dimensionalOS/dimos-desktop) app that turns a recording into a clean, annotated map:

1. **Open** a recording (`.db` or `.mcap`) from Desktop's shared recordings folder (the Live Viewer records there).
2. **Build** the global map: every scan placed through the recording's tf, loops closed (ICP + pose graph), ray traced
   so free space clears what moved. A progress bar with stage, ETA and Cancel; it keeps running if you close the page.
3. **Clean**: remove floating clusters, outliers, the floor, walls; crop to a box or a height band; turn and level the
   map. Each tool works on what's in view, a region box, or the whole map, has a preview, and is undoable.
4. **Annotate** in 3D: labelled boxes, planes and points, placed by clicking and edited with a gizmo.
5. **Floor plans**: a 2D plan per storey (multi-floor recordings split automatically), annotated with named points and
   named areas such as no-go zones.
6. **Save into the recording**: the map, plans and annotations become new streams in the same file
   ([docs/schema.md](docs/schema.md)), ready for a navigation blueprint to read.

Desktop's chat agent can do all of it too, through the MCP tools at `/mcp` ([docs/agent-tools.md](docs/agent-tools.md)):
"clean up the floating voxels in this view", "box every chair", "add a no-go area around the stairs".

Your work is never lost: the working session (map edits, undo history, annotations, plans, camera, panels) is kept
server-side and written to disk on every change, separately from "Save into recording". Refresh, close the tab or
restart Desktop and it comes back as it was.

## Install

In Desktop: App Store → add `https://github.com/jeff-hykin/dim-map-builder` (branch `dimos-desktop2`), or

    dimos-desktop install --name dim-map-builder https://github.com/jeff-hykin/dim-map-builder

## Layout

| Path | What |
| --- | --- |
| `crates/dimos-recording` | read/write dimos recordings without dimos: memory2 `.db`, `.mcap`; LCM + ROS 2 CDR clouds, tf, odometry |
| `crates/mapping` | the map math: the ray-traced voxel map (vendored from dimos), tf tree, ICP, loop closure (a port of dimos's PGO), cleanup selections, floors, floor plans, the staged build |
| `server` | `dimos-app-server`: sessions, jobs, the page's API (`/api`), the agent tools (`/mcp`), save/restore |
| `frontend` | the page: React + Vite + three.js; `src/render` is copied from the Live Viewer (same point styles) |
| `docs` | [schema.md](docs/schema.md) (what's saved), [agent-tools.md](docs/agent-tools.md), [api.md](docs/api.md) |

One compiled binary at runtime: no Python, no dimos needed to build or edit maps.

## Develop

    cargo test                                  # all the non-UI logic
    cargo run -p dimos-app-server -- --port 7190 --frontend frontend/dist
    (cd frontend && npm ci && npm run dev)      # the page, against a running Desktop (DESKTOP_URL)
    nix build .#dimosApp                        # what Desktop builds

## Keyboard

`1`–`6` stages · `⌘Z` / `⇧⌘Z` undo / redo · `⌘S` save · `F` frame the map · `T` top view · `G` / `R` / `S` move / turn /
resize the selection · `Del` delete it · `Esc` cancel a tool.
