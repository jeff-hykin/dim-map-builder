# What "Save into recording" writes

The Map Editor writes its result into the recording it opened, as new streams (`.db`) or channels (`.mcap`) beside the
recording's own data, which is never modified.

A recording can hold several maps of the same space, so each one is saved under a **map name** — `v1` by default, or
the first `v<n>` the recording hasn't got a map under; the Save panel's box takes any plain identifier (a letter, then
letters, digits and underscores; a name that isn't one is refused, never quietly changed into one). Every stream of a
map goes under its name:

- an `.mcap` channel can hold slashes, so there it is **`map/<name>/<leaf>`**
- a `.db` stream becomes a SQLite table name, so there it is **`map_<name>_<leaf>`**

That is the same name spelled two ways: dimos reads an `.mcap` topic with its slashes folded to underscores, so a
reader sees one stream either way. Below, `<map>` stands for whichever of the two the file is.

Everything is in the map's own frame, **`map/<name>`** (so two maps of one space never share a frame); `transform` (in
the annotations JSON) maps the recording's own world frame into it, so `map/<name> ← world` is recoverable for anything
else in the recording. Saving again replaces that map's streams (a smaller number of floors also removes the stale
`floor_<n>`, and the `map_builder_*` / `/map_builder/*` streams earlier versions wrote are dropped); the flat `map/...`
streams of versions before map names are left exactly as they were, so a recording saved then still opens in both.

| Stream / channel (`.db`: memory2, codec `lcm`; `.mcap`: encoding `lcm`, metadata `lcm_type`) | dimos type | Content |
| --- | --- | --- |
| `<map>/cloud` | `sensor_msgs.PointCloud2` | The edited map as one cloud, `frame_id: "map/<name>"`: what dimos's relocalization loads as its premap. Generate writes it first (in the recording's world frame); Save replaces it with the edited map |
| `<map>/voxels` | `sensor_msgs.PointCloud2` | The edited voxel map (cleanup, erase, draw, straightened walls applied): one point per voxel (center), `x`,`y`,`z` float32, `frame_id: "map/<name>"` |
| `<map>/path` | `nav_msgs.Path` | The loop-closed sensor path, `frame_id: "map/<name>"` |
| `<map>/annotations` | `std_msgs.String` | JSON, below |
| `<map>/views` | `std_msgs.String` | JSON: the saved 2D views (the "2D maps"), the same list as `annotations.views` |
| `<map>/slice` | `std_msgs.String` | JSON: the slicer's view `{ zMin, zMax, yaw, xMin, xMax, yMin, yMax }` or `null`, the same as `annotations.slice` |
| `<map>/floor_<n>` | `nav_msgs.OccupancyGrid` | Only when exported (Saved views → Export occupancy grids): storey `n`'s grid, `-1` unknown, `0` free floor, `100` occupied. `info.origin.position` = the grid's (0,0) corner, with `z` = the floor's height |
| `<map>/named_locations` | `visualization_msgs.InteractiveMarkerUpdate` | The whole list of places by name, in the map's coordinates: what dimos's relocalization seeds a premap's locations from. The recording's own `/named_locations` (as the robot recorded it) is only ever read, to seed this list once |

Every edit kind lands in these: the voxel edits (cleanup, erase, draw, straightened walls) in `<map>/voxels`; the map
transform, boxes, planes, points, polygons, floors, named points, areas, saved views and the slice in
`<map>/annotations` (views and slice also in their own streams). The local floor model isn't saved: it's recomputed
from `<map>/voxels`. Only the undo history stays in the working session.

Each stream holds one message, stamped with the save time. Read them with dimos (`SqliteStore(path).stream(name)`), the
`mcap` Python package, Foxglove (the `.mcap`), or this repo's `crates/dimos-recording`.

## `<map>/annotations` (JSON, `schema: "dimos.map_builder.v1"`)

```ts
{
    schema: "dimos.map_builder.v1"
    savedAt: number                 // unix seconds
    frame: "map"
    worldFrame: string              // the recording's frame the map was built in (e.g. "odom", "world")
    transform: { translation: [x, y, z], rotation: [x, y, z, w] }   // map ← worldFrame
    voxelSize: number               // meters
    annotations: {
        boxes:  { id, label, box: { center: [x, y, z], size: [dx, dy, dz], yaw /* rad about +z */ }, source: "user" | "agent" }[]
        planes: { id, label, center: [x, y, z], normal: [x, y, z], size: [w, h], source }[]
        points: { id, label, position: [x, y, z], source }[]
        floors: { index, name, z }[]                       // z = the floor's height in "map"
        planPoints: { id, floor, name, position: [x, y] }[]    // named spots on a floor plan
        areas: { id, floor, name, kind, polygon: [[x, y], ...] }[]  // kind "no-go" = navigation must avoid it; "zone", "slow", or free text
        prisms: { id, label, floor, polygon: [[x, y], ...], base, height, source }[]  // a polygon standing from z = base up by height
        views: { id, name, floor, follow, zMin, zMax, center: [x, y] | null, pixelsPerMeter | null }[]
            // a 2D map: storey `floor` sliced from zMin to zMax (over the local floor when follow, else absolute z)
        slice: { zMin, zMax, yaw, xMin, xMax, yMin, yMax } | null
            // the slicer's view, not an edit (<map>/voxels holds every voxel): show z in zMin..zMax, turn the map by yaw
            // (radians about +z through the origin), then x in xMin..xMax and y in yMin..yMax in that turned frame
    }
    floorStreams: string[]          // floor index → the stream / topic holding its OccupancyGrid
    build: { voxelSize, worldFrame, cloudStream, scansUsed, scansSkipped, loops, notes, seconds } | null
}
```

All coordinates are meters in `map`. A navigation blueprint wanting the no-go zones reads the annotations JSON,
takes `areas` with `kind == "no-go"` for the floor it's on, and rasterizes those polygons into its costmap (the
`OccupancyGrid` of that floor shares the frame).

## Re-opening

Opening a recording that has a map's `annotations` (the newest, when it has several maps; or, from versions before map
names, the flat `map/annotations`, or `map_builder_annotations` from earlier still) and no working
session for it restores the map, the transform, every annotation, the saved views and the grids from these streams.

# What Generate writes

Generate's steps (the Generate dialog, or `POST api/sessions/{id}/generate/...`) also write into the recording in
place: a `.db` has the stream replaced, an `.mcap` has the topic replaced on its one channel (channel metadata
`map_builder_generation` = unix seconds): the step's new messages are appended and the previous run's chunks are cut
off the end of the file, or, inside it, covered by a filler record (opcode `0x80`, which every mcap reader skips) and
dropped from the index, so every reader (dimos, Foxglove, the Recordings app) sees one copy. The Map Editor never adds
a second channel to a topic: a recorded topic of the same name is refused, never replaced. A file written by an older
version (several channels on one topic, an edge on a second channel of `/tf`) still reads right (the newest wins) and
is brought to one channel per topic by the next step written into it. Nothing is left beside the recording but the two files asked for: the `.pc2.lcm` and,
when asked for, the `.rrd`. The high-quality tf correction runs icp_stitch on an `.mcap` itself (`stitch.inPlace`,
default on): the odometry is the recording's Odometry topic on the corrected edge's odometry, else that tf edge
(`tf_odometry`), and `raw_april_tags` are its tags. Its derived topics stay in the file next to the edge:
`/<odometry>_corrected` (the corrected Odometry), `/tf_corrected`, `/pose_graph`, `/tf_deformation_nodes_corrected`
(ROS 2 CDR, `written_by: icp_stitch`, replaced by the next run; when a recorded topic already has one of those names they
get a `_map_editor` suffix, and the recorded one is left alone). A `.db` (or a cloud registered in a root frame other
than the odometry's parent) runs on a temporary db in the app's data folder, deleted when the step ends.

| Stream / channel | dimos type | Content |
| --- | --- | --- |
| `/map/tf`, `/pointlio_odometry` | `tf2_msgs.TFMessage`, `nav_msgs.Odometry` | Step 0 (Point-LIO, for a lidar + IMU with no odometry): the lidar rig's pose at the end of every scan, as the edge `odom → <the root of the lidar's static tree>` (written like step 2's edge) and as Odometry (`odom` → that root; a `.db` stream `pointlio_odometry`), on the log clock |
| `/pointlio_lidar` (`.mcap`) | `sensor_msgs.PointCloud2` | Step 0: every scan motion-compensated by Point-LIO's per-point states (blind-range and every-3rd-point filtered as Point-LIO reads it), in the lidar's frame at the scan's end; channel metadata `derived_from` names the raw lidar, which the build and the tf correction's default then step aside for |
| `raw_april_tags` | `geometry_msgs.PoseStamped` | Step 1: every marker glimpse (AprilTag or ArUco, whichever dictionary step 1 ran with): the tag's pose in the camera's optical frame (`frame_id`), at the image's time. `.db` row tags (and pose columns = the same pose, as dimos's `add_april.py` writes): `marker_id`, `sharpness`, `reproj_px`, `tag_px`, `distance_m`, `view_angle_deg`, `lin_speed`, `ang_speed` (the camera's speed from tf; -1 unknown). `.mcap`: the same JSON object per message on `raw_april_tags/tags` (`std_msgs.String`) |
| `april_tags` | `geometry_msgs.PoseStamped` | Step 1: the glimpses that pass the quality gates, grouped per marker visit (a gap of more than 5 s starts a new one), each group reduced to one Huber-refined medoid pose; tags `marker_id`, `n_observations` |
| `tf` (`.db`) / `/map/tf` (`.mcap`) | `tf2_msgs.TFMessage` | Step 2: the drift correction as one edge, grandparent → parent (e.g. `world → odom`), at 10 Hz over the whole recording and at every keyframe, one transform per message (rows tagged `child_frame`). In a `.db` every transform of that edge already there is dropped from the tf streams (a message left empty is deleted), the way dtk's `db_tf_*` tools edit tf. In an `.mcap` every edge the Map Editor writes is on the one channel of `/map/tf` (encoded like the recording's tf, cdr or lcm), with metadata `map_builder_edge: "<parent>><child>,..."`; writing an edge rewrites that channel with the other edges carried over. Readers here take those edges from it instead of the recording's own tf. Foxglove reads it (it merges every tf topic); dimos's `StreamTF` and the Recordings app read only `tf`/`tf_static`, so they don't see it |
| `<map>/cloud` | `sensor_msgs.PointCloud2` | Step 3: the global map (through the corrected tf, ray traced, decimated, stray points removed), one message at the recording's start, in the world frame; Save writes the edited map there again (frame `map/<name>`). dimos relocalization finds a recording's maps by this stream and loads one as its premap, falling back to the flat `map_cloud` for a recording with no named map. Also written beside the recording as `<recording>_map_cloud.pc2.lcm` (that one LCM-encoded PointCloud2) |
| `<map>/pipeline` | `std_msgs.String` | JSON: what each step last ran with, when, and what it found (so a later step knows whether it needs an earlier one, and the dialog shows it). Per map name, so two maps of one space each remember their own generation; the flat `map/pipeline` of versions before map names is still read when the map has none |

`<name>.rrd` (step 3, optional, beside the recording): the global map, the raw and corrected paths, each AprilTag as
its own pattern at its place (with its id), and the streams picked at the rates picked (color images at 2 Hz by
default, on a posed pinhole), all placed through the recording's tf.
