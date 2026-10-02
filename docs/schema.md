# What "Save into recording" writes

The Map Builder writes its result into the recording it opened, as new streams (`.db`) or channels (`.mcap`) named
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
| `map/floor_<n>` | `nav_msgs.OccupancyGrid` | Only when exported (Saved views → Export occupancy grids): storey `n`'s grid, `-1` unknown, `0` free floor, `100` occupied. `info.origin.position` = the grid's (0,0) corner, with `z` = the floor's height |

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
