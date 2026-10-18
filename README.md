# dim-map-builder

A [dimOS Desktop](https://github.com/dimensionalOS/dimos-desktop) app that turns a recording into a clean, annotated map:

It's a map viewer first: **[ 3D | Split | 2D ]** at the top (Split = 3D beside a 2D minimap that shows the camera and
moves it on a click). Opening a recording with a map goes straight to it. Only generating the map is needed first;
everything else is optional and in any order.

- **Open** a recording (`.db` or `.mcap`) from Desktop's shared recordings folder (the Live Viewer records there).
  Opening reads only its index (an `.mcap`'s summary, a `.db`'s stream list), so it's instant at any size; a topic is
  read from the chunks that hold it. An `.mcap` cut short (no summary) is read once in the background, with progress,
  and so is a big recording's tf for the Generate steps.
- **Generate** (the blue orb): a corrected map in one press ("Generate corrected map"), or any one of the steps that make
  it, each with its own Run button, progress bar, ETA and Cancel. Every step writes into the recording in place
  ([docs/schema.md](docs/schema.md#what-generate-writes)), and a step whose input the recording doesn't have yet runs
  the earlier one first (it reads what's there from the file: its streams, its tf and `map/pipeline`).
  1. **Fiducial markers (AprilTag / ArUco)**: every image through a marker detector (tag36h11, 10 cm by default;
     kornia-apriltag, or for an OpenCV ArUco dictionary, `DICT_4X4_50` .. `DICT_7X7_1000`, `DICT_ARUCO_ORIGINAL`,
     `DICT_ARUCO_MIP_36h12`, cv2.aruco's decoding on kornia's quads; then a planar
     PnP, ported from icp_stitch / dimos's `apriltags.py`), posed with the recording's CameraInfo: `raw_april_tags`
     (every glimpse, with its quality diagnostics; the camera's speed comes from tf, never from an image's stored pose)
     and/or `april_tags` (gated, grouped per marker visit into one Huber medoid, as gsc_pgo's `add_april.py` does).
  2. **A corrected tf tree**: loop closure over the trajectory of one point cloud's sensor, written as the tf edge above
     the odometry (`world → odom` / `map → odom`: the one there is replaced, else a new root, `world` by default), the way
     dtk edits tf. Static vs moving edges are told apart from the data. The default cloud is one whose frame moves,
     in the biggest tree, nearest its root; a cloud already in its root frame asks for the sensor's frame and is moved
     back into it first. Optional AprilTag landmarks (and tags to leave out). **Faster** = the app's loop closure (the
     PGO port) and one pose graph solved here; **Higher quality** = [icp_stitch](https://github.com/jeff-hykin/icp_stitch)
     (gsc_pgo's tag PGO + ICP stitching on GTSAM; its release binary, or `ICP_STITCH_BIN`) run on a temporary db in the
     data folder, with its stitching knobs under Advanced.
  3. **Global map**: every scan placed through the corrected tf (on by default), ray traced (on by default; the ray
     tracer's tunables under Advanced), decimated (a final voxel and stray-point removal), written into the recording as
     `global_map_aggregated` (one cloud at its start) and beside it as `<name>_global_map_aggregated.pc2.lcm`; it becomes
     the map in the editor. Optionally an `.rrd` beside the recording too: the map, the paths, each AprilTag drawn as its
     own pattern with its id, and the streams picked at the rates picked (color images at 2 Hz by default).
- **Drop a recording** (`.db` / `.mcap`) on the page to open it with Generate up: one Desktop already has opens in place;
  any other is streamed into Desktop's recordings folder first (a page is handed a dropped file's bytes, not its path).
- **Not post-processed yet** (a lidar + IMU and no odometry, e.g. a raw [lite_record](https://github.com/jeff-hykin/lite_record)
  recording): Generate first runs lite_record's own `post_process` on it, in place (its release binary, re-fetched into
  the data dir when the release is rebuilt, or `LITE_RECORD_BIN`): viewable images, the frame tree, odometry, a
  motion-compensated copy of the lidar and a voxel map, appended to the recording. The map is then built at 4 cm from
  the corrected copy (a cloud channel whose `derived_from` metadata names another is preferred over it).
- **Clocks**: tf is looked up at each scan's header stamp; when a device stamped the scans on its own clock and tf is on
  the log clock, the scans' stamps are moved onto the log clock (whichever tf covers). Nothing names a sensor.
- **In place, never a copy**: Generate's post-processing and Save write into the recording itself (an `.mcap` save is
  appended: the summary is cut off, the new chunks go where it was, the summary goes back; the newest save is what
  opens). A recording in a read-only folder is refused, not copied. Nothing is left beside the recording.
- **2D**: a top-down slice of the current voxels between z-start and z-end, over the **local floor** (a per-cell floor
  height that follows ramps and stairs, `crates/mapping/src/floor.rs`) or absolute; "auto" = floor +0.1 to +1.8 m.
  Saved views are the 2D maps (a storey + a height band), saved into the recording.
- **Slicer** (the yellow orb, pulsing until a map has a slice): a 4-step view of the map, not an edit. It flies out to
  the whole map, then cuts it to a height band (live), turns it so the walls line up with x and y (a top-down x-ray
  over a grid, with an "auto" angle from the walls' directions), crops x / y with a draggable rectangle, and flies back
  to the result. 3D, 2D and the minimap all show it; it's saved as seven numbers (`map/slice`).
- **Edit** (the green orb): a palette of tools. **Erase** (one stroke takes everything the brush covers, from the floor
  up to the slice's top, the floor itself stays; a lone speck goes in one click; nothing is ever added), **Draw** / **Line** (voxels from the floor up to a height), **Straighten wall** (drag along
  a noisy wall: the wall in the band becomes one clean slab of its own thickness; nothing outside the band changes), **Polygon** (an area drawn in 2D that stands up as a prism in 3D),
  **named points and areas** (no-go zones), **3D boxes, planes and points**, **Clean up** (floating specks, outliers,
  floor, walls, crop, level), **Saved views**. All undoable.
- **Save** into the recording, in place: the map, annotations and views become `map/*` streams in the same file
  ([docs/schema.md](docs/schema.md)), ready for a navigation blueprint to read.
- **Share** (top bar) → **as HTML**: one self-contained `.html` anyone can open offline, no Desktop: the voxels the 3D
  view shows (current edits, cropped to the slice, at the map's voxel size), in the same look (the editor's renderer
  and voxel shader, `frontend/src/share/viewer.ts`) from the same camera, with the floor selector bottom right for a
  multi-floor map. The voxels are packed to about half a byte each (`frontend/src/share/pack.ts`), so a 450k-voxel map
  is a ~400 KB file. The dialog shows the size, then downloads it.
- **Upload** the recording (with the map saved in it) to your Dimensional cloud account: the Upload button in the top
  bar, or the Save menu (which offers "Save, then upload" when there are unsaved edits). Not logged in? A dialog shows
  the page to open and the code to approve from any signed-in browser, then the upload starts by itself. Uploads run
  in Desktop (they keep going when the page closes) and queue up; the uploads drawer shows each one's progress, speed
  and time left, with cancel, retry and readable errors.

Every action is an HTTP endpoint the page itself uses, and Desktop's agent calls the same ones (listed at `/agent.json`
and in `dimos.yaml`, [docs/api.md](docs/api.md)):
"clean up the floating voxels in this view", "box every chair", "add a no-go area around the stairs".

Your work is never lost: the working session (map edits, undo history, annotations, plans, camera, panels) is kept
server-side and written to disk on every change, separately from "Save into recording". Refresh or close the tab and
it comes back as it was. After Desktop restarts, the Map Editor opens on "Please pick a recording" (with a Continue
button for the last one), and picking that recording again brings its session back.

## Install

This version needs a dimOS Desktop whose dimos gateway is API 4.0 (the `/dimos-gateway/cloud/account` and
`/dimos-gateway/cloud/uploads` endpoints): it declares them in `dimos.yaml` `uses:`, and a Desktop whose gateway doesn't
provide them won't install it. Page errors also go to Desktop's error feed (`POST /api/errors`, dim-app
`errors.js`) so its agent sees them; an older Desktop just drops those.

In Desktop: App Store → add `https://github.com/jeff-hykin/dim-map-builder` (branch `dimos-desktop2`), or

    dimos-desktop install --name dim-map-builder https://github.com/jeff-hykin/dim-map-builder

## Layout

| Path | What |
| --- | --- |
| `crates/dimos-recording` | read/write dimos recordings without dimos: memory2 `.db`, `.mcap`; LCM + ROS 2 CDR clouds, tf, odometry |
| `crates/mapping` | the map math: the ray-traced voxel map (dimos's `dimos-voxel-ray-tracing` crate, a git dependency), tf tree, ICP, loop closure (a port of dimos's PGO), cleanup selections, floors, floor plans, the staged build |
| `server` | `dimos-app-server`: sessions, jobs, every action as an endpoint (`api.rs`, one table that is also `/agent.json`), save/restore |
| `scripts/check_endpoints.ts` | checks dimos.yaml's `agent:` against the server's `--agent-json` (`--write` regenerates it) |
| `frontend` | the page: React + Vite, built from [dimos-react](https://github.com/jeff-hykin/dimos-react) (every control, panel and dialog, Desktop's theme, and the 3D view: its renderer, point styles and palettes); `src/core/scene.ts` is the editor on top of that view; `src/share` the shared-map page (`vite.share.config.ts` builds its viewer) |
| `docs` | [schema.md](docs/schema.md) (what's saved), [api.md](docs/api.md) (the endpoints) |

One compiled binary at runtime: no Python, no dimos needed to build or edit maps.

## Develop

    cargo test --workspace                      # all the non-UI logic and every endpoint
    deno test frontend/src/share/               # the shared map's packing
    deno task check-endpoints [--write]         # dimos.yaml's agent: = the served agent.json
    cargo run -p dimos-app-server -- --port 7190 --frontend frontend/dist
    (cd frontend && npm ci && npm run dev)      # the page, against a running Desktop (DESKTOP_URL)
    nix build .#dimosApp                        # what Desktop builds

## Keyboard

`V` 2D / 3D · `M` split · `E` the edit palette · `X` erase · `D` draw · `L` line · `W` straighten wall · `P` polygon ·
`N` named points / areas · `B` 3D boxes · `C` clean up · `⌘Z` / `⇧⌘Z` undo / redo · `⌘S` save · `F` frame the map ·
`T` top view · `G` / `R` / `S` move / turn / resize the selection · `Del` delete it · `Esc` cancel / put the tool down.
