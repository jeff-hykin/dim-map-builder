# The Map Builder's agent endpoints

The Map Builder runs no MCP server of its own. It describes its actions in Desktop's endpoint-manifest model
(dimos-desktop's docs/agent.md): **`GET /agent.json`** lists them (description + JSON Schema params) and
**`POST /agent/<name>`** runs one with a JSON body, e.g. through Desktop:

    curl -X POST http://127.0.0.1:7077/apps/dim-map-builder/agent/fit_box -d '{"box": {"center": [1, 2, 0.5], "size": [2, 2, 1]}}'

Desktop's one MCP server (`/mcp`, which Desktop's dimcode uses) finds them with `search_endpoints` and runs them with
`call_endpoint` (ids like `dim-map-builder:POST agent/fit_box`); `get_view` is the app's `screenshot`, `get_status` its
state in `desktop_context`. A result is one JSON object; screenshots and plans are in `images: [{ mimeType, data }]`,
which Desktop hands the model as images. Actions act on the recording open in the Map Builder page (or `session`), and
every edit is the same undoable edit a user makes: it shows up in the page immediately, and `undo` / the page's Undo
reverts it.

Coordinates are **meters in the map frame** (+z up; after "Level the floor", the main floor is z = 0). Boxes are
`{ center: [x, y, z], size: [dx, dy, dz], yaw }` (full extents, yaw in radians about +z). A `region` argument is
`"view"` (what the user's camera sees now), `"all"`, or a box.

| Action | What it does |
| --- | --- |
| `get_status` | the open map: recording, stage, voxel count, bounds, floors, annotation counts, running job, unsaved?, recent history |
| `get_view` | the user's camera, the bounds of the visible voxels, and a **screenshot** of the 3D view with a labelled 1 m grid, axes and annotation labels (`topDown: true` for a plan-like shot) |
| `set_view` | move the user's camera to look at a point |
| `query_region` | count / bounds / horizontal-vs-vertical surface split / sample of the voxels in a region |
| `find_objects` | clusters standing on a floor (furniture-like) in a region, each with a tight box, size and height |
| `fit_box` | the tightest box around the non-floor voxels in a rough box (`add: true` adds it) |
| `add_box`, `add_plane`, `add_point` | add labelled annotations |
| `list_annotations`, `update_annotation`, `delete_annotation` | read / change / remove by id |
| `cleanup` | remove `floating` clusters, `outliers`, the `floor`, `walls`, or everything but walls (`keepWalls`), in a region; `preview: true` only counts |
| `crop` | keep only a box, keep a height band, or delete a box |
| `rotate_map`, `level_map` | orient the map (annotations move with it) |
| `generate_floor_plans` | 2D plans per storey |
| `get_floor_plan` | a plan as an image plus its origin / resolution (pixel → meters) and its named points and areas |
| `add_plan_point`, `add_area` | named spots and areas (`kind: "no-go"` for navigation to avoid) on a floor |
| `add_polygon` | a polygon on a storey standing up from the local floor (`height`, default 1 m): a prism in 3D |
| `erase` | brush away what stands on a storey's floor along a path (`radius`, up to `zEnd` over the floor or the whole column); the floor under it is patched |
| `draw` | add voxels along a path (`width`) from the local floor up to `height` (default 1 m) |
| `straighten_wall` | replace a noisy wall along `from`→`to` (`width` band) with one straight wall on the fitted line |
| `undo`, `redo` | anyone's last edit |
| `build_map`, `save_to_recording` | start the background jobs (poll `get_status`) |

## How to box an object well

1. `get_view` to see what the user sees (or `find_objects` with `region: "view"` to get candidates with boxes).
2. Narrow down: `query_region` on a rough box around where it should be (counts, heights).
3. `fit_box` with a generous box around it: the result hugs the voxels (floor excluded); `add: true, label: "chair"`.

The evaluation that shaped these tools (≥10 boxing tasks, IoU against hand-placed boxes, before/after the
`find_objects` / `fit_box` / labelled-screenshot additions) is in `eval/`.
