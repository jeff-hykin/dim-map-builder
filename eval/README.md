# Agent evaluation of the Map Builder tools

These results are from when the app served its own `/mcp` (with `?toolset=basic`). The app now only describes its
endpoints (`/agent.json`) and agents reach them through Desktop's MCP; `run.ts` runs the same tasks that way.

Claude (`claude -p`, Opus 5, given only the Map Builder MCP tools) on 12 tasks on the sim go2 office map (Live Viewer
recording `live_viewer_1790926614.mcap`, 68 lidar scans). Ground truth = the MuJoCo scene's own object boxes
(`office_truth.py` from `scene_office1.xml`; `office_tasks.py` builds `tasks.json`). Box tasks score 3D IoU (each truth
box against its best agent box); area tasks score 2D IoU of the polygon. One run per task per toolset.

- **basic** = `?toolset=basic`: no find_objects / fit_box / query_region, screenshots without the labelled grid.
- **full v1** = all tools + labelled-grid screenshots.
- **full v2** = v1 after the failures it showed: fit_box keeps the connected piece nearest the region's center (it used
  to merge a chair with the table beside it), prefers axis-aligned boxes, and the guide says to trust fit_box over
  screenshots.
- **ceiling** = fit_box given the truth box itself (grown 30%): what this map supports at all.

| task | basic IoU (calls, s) | full v1 | full v2 (fit_box fix) | fit_box on truth (ceiling) |
|---|---|---|---|---|
| white_table_south | 0.77 (9, 38 s) | 0.76 (10, 34 s) | 0.46 (15, 48 s) | 0.643 |
| white_table_north_view | 0.97 (4, 28 s) | 0.63 (7, 35 s) | 0.74 (14, 53 s) | 0.711 |
| one_chair | 0.58 (7, 26 s) | 0.38 (4, 18 s) | 0.38 (3, 12 s) | 0.376 |
| chair_in_view | 0.29 (7, 50 s) | 0.50 (9, 45 s) | 0.41 (22, 85 s) | 0.526 |
| chair_by_coords | 0.34 (10, 56 s) | 0.46 (6, 30 s) | 0.32 (11, 63 s) | 0.566 |
| desk_view | 0.16 (7, 40 s) | 0.24 (9, 45 s) | 0.13 (6, 29 s) | 0.131 |
| north_chairs | 0.38 (20, 118 s), found 2/2 | 0.38 (18, 64 s), found 2/2 | 0.52 (25, 76 s), found 2/2 | 0.553 |
| south_chairs | 0.42 (24, 130 s), found 5/5 | 0.40 (39, 124 s), found 4/5 | 0.29 (39, 132 s), found 1/5 | 0.445 |
| chairs_in_view | 0.40 (12, 53 s), found 2/2 | 0.34 (18, 62 s), found 2/2 | 0.52 (15, 56 s), found 2/2 | 0.553 |
| both_tables | 0.79 (14, 52 s), found 2/2 | 0.63 (18, 59 s), found 2/2 | 0.66 (16, 85 s), found 2/2 | 0.677 |
| no_go_south_table | 0.85 (8, 37 s) | 0.67 (13, 49 s) | 0.81 (7, 30 s) | – |
| no_go_desk | 0.31 (11, 71 s) | 0.32 (11, 68 s) | 0.37 (11, 51 s) | – |
| **mean** | **0.52** | **0.48** | **0.47** | |

Reading: every variant finds the objects (the chair counts are right in nearly every run), but box IoU is capped by
the map, not the tools: the lidar saw only the tops and near sides of chairs and the desk, while MuJoCo's boxes are the
full meshes (legs, undersides), so even fit_box on the truth box reaches ~0.45 for chairs and 0.13 for the desk (whose
truth union also takes in desk parts the robot never saw). The agents already sit at or above that ceiling with the
basic tools (they guess full-height boxes, which matches the meshes better), so the new tools didn't move the mean; they
did cut screenshot round-trips (basic: 3-6 get_view per chair task, full: 0-2) and fix the clearly wrong fits. A fair
before/after needs a ground truth of what the sensor saw (or a denser recording), and several runs per task.
