# What "Save into recording" writes

The Map Editor writes its result into the recording it opened, as new streams (`.db`) or channels (`.mcap`) named
`map/...` beside the recording's own data, which is never modified. Saving again replaces them (a smaller number of
floors also removes the stale `floor_<n>`, and the `map_builder_*` / `/map_builder/*` streams earlier versions wrote are
dropped). Everything is in one fixed frame, **`map`**; `transform` (in the annotations JSON) maps the
recording's own world frame into it, so `map ← world` is recoverable for anything else in the recording.

| Stream / channel (`.db`: memory2, codec `lcm`; `.mcap`: encoding `lcm`, metadata `lcm_type`) | dimos type | Content |
| --- | --- | --- |
| `map/voxels` | `sensor_msgs.PointCloud2` | The edited voxel map (cleanup, erase, draw, straightened walls applied): one point per voxel (center), `x`,`y`,`z` float32, `frame_id: "map"` |
| `map/path` | `nav_msgs.Path` | The loop-closed sensor path, `frame_id: "map"` |
| `map/annotations` | `std_msgs.String` | JSON, below |
| `map/views` | `std_msgs.String` | JSON: the saved 2D views (the "2D maps"), the same list as `annotations.views` |
| `map/slice` | `std_msgs.String` | JSON: the slicer's view `{ zMin, zMax, yaw, xMin, xMax, yMin, yMax }` or `null`, the same as `annotations.slice` |
| `map/floor_<n>` | `nav_msgs.OccupancyGrid` | Only when exported (Saved views → Export occupancy grids): storey `n`'s grid, `-1` unknown, `0` free floor, `100` occupied. `info.origin.position` = the grid's (0,0) corner, with `z` = the floor's height |

Every edit kind lands in these: the voxel edits (cleanup, erase, draw, straightened walls) in `map/voxels`; the map
transform, boxes, planes, points, polygons, floors, named points, areas, saved views and the slice in `map/annotations`
(views and slice also in their own streams). Nothing is written beside the recording. The local floor model isn't
saved: it's recomputed from `map/voxels`. Only the undo history stays in the working session.

Each stream holds one message, stamped with the save time. Read them with dimos (`SqliteStore(path).stream(name)`), the
`mcap` Python package, Foxglove (the `.mcap`), or this repo's `crates/dimos-recording`.

## `map/annotations` (JSON, `schema: "dimos.map_builder.v1"`)

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
            // the slicer's view, not an edit (map/voxels holds every voxel): show z in zMin..zMax, turn the map by yaw
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

Opening a recording that has `map/annotations` (or, from earlier versions, `map_builder_annotations`) and no working
session for it restores the map, the transform, every annotation, the saved views and the grids from these streams.

# What Generate writes

Generate's steps (the Generate dialog, or `POST api/sessions/{id}/generate/...`) also write into the recording in
place: a `.db` has the stream replaced, an `.mcap` gets a newer channel appended on the same topic (channel metadata
`map_builder_generation` = unix seconds; readers here take the newest, and older ones are left as they were, since an
mcap is only ever appended to). Nothing is left beside the recording but the two files asked for: the `.pc2.lcm` and,
when asked for, the `.rrd`. The high-quality tf correction runs icp_stitch on an `.mcap` itself when the recording's
Odometry topic is the corrected edge's odometry and no tag factors are used (`stitch.inPlace`, default on), and its
derived topics stay in the file next to the edge: `/<odometry>_corrected` (the corrected Odometry), `/tf_corrected`,
`/pose_graph`, `/tf_deformation_nodes_corrected` (ROS 2 CDR, replaced by the next run); otherwise it runs on a temporary
db in the app's data folder, deleted when the step ends.

| Stream / channel | dimos type | Content |
| --- | --- | --- |
| the tf topic, `/pointlio_odometry` | `tf2_msgs.TFMessage`, `nav_msgs.Odometry` | Step 0 (Point-LIO, for a lidar + IMU with no odometry): the lidar rig's pose at the end of every scan, as the edge `odom → <the root of the lidar's static tree>` (written like step 2's edge) and as Odometry (`odom` → that root; a `.db` stream `pointlio_odometry`), on the log clock |
| `/pointlio_lidar` (`.mcap`) | `sensor_msgs.PointCloud2` | Step 0: every scan motion-compensated by Point-LIO's per-point states (blind-range and every-3rd-point filtered as Point-LIO reads it), in the lidar's frame at the scan's end; channel metadata `derived_from` names the raw lidar, which the build and the tf correction's default then step aside for |
| `raw_april_tags` | `geometry_msgs.PoseStamped` | Step 1: every marker glimpse (AprilTag or ArUco, whichever dictionary step 1 ran with): the tag's pose in the camera's optical frame (`frame_id`), at the image's time. `.db` row tags (and pose columns = the same pose, as dimos's `add_april.py` writes): `marker_id`, `sharpness`, `reproj_px`, `tag_px`, `distance_m`, `view_angle_deg`, `lin_speed`, `ang_speed` (the camera's speed from tf; -1 unknown). `.mcap`: the same JSON object per message on `raw_april_tags/tags` (`std_msgs.String`) |
| `april_tags` | `geometry_msgs.PoseStamped` | Step 1: the glimpses that pass the quality gates, grouped per marker visit (a gap of more than 5 s starts a new one), each group reduced to one Huber-refined medoid pose; tags `marker_id`, `n_observations` |
| `tf` (`.db`) / the tf topic (`.mcap`) | `tf2_msgs.TFMessage` | Step 2: the drift correction as one edge, grandparent → parent (e.g. `world → odom`), at 10 Hz over the whole recording and at every keyframe, one transform per message (rows tagged `child_frame`). In a `.db` every transform of that edge already there is dropped from the tf streams (a message left empty is deleted), the way dtk's `db_tf_*` tools edit tf. In an `.mcap` the samples are a new channel on the tf topic with metadata `map_builder_edge: "<parent>><child>"`; readers here take that edge from the newest such channel only (other readers see the old samples too) |
| `global_map_aggregated` | `sensor_msgs.PointCloud2` | Step 3: the global map (through the corrected tf, ray traced, decimated, stray points removed), one message at the recording's first timestamp, in the world frame. Also written beside the recording as `<name>_global_map_aggregated.pc2.lcm` (that one LCM-encoded PointCloud2) |
| `map/pipeline` | `std_msgs.String` | JSON: what each step last ran with, when, and what it found (so a later step knows whether it needs an earlier one, and the dialog shows it) |

`<name>.rrd` (step 3, optional, beside the recording): the global map, the raw and corrected paths, each AprilTag as
its own pattern at its place (with its id), and the streams picked at the rates picked (color images at 2 Hz by
default, on a posed pinhole), all placed through the recording's tf.
