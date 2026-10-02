"""Builds the agent-eval tasks for the sim office recording from the simulator's own geometry (office_truth.py output).
Ground truth = MuJoCo's object boxes in the sim's world frame, which is the recording's (and so the map's) frame:
checked by counting map voxels inside each truth box before scoring. Only objects the recording's lidar covered."""
import json, sys

truth = json.load(open(sys.argv[1]))
session = sys.argv[2]

def box(lo, hi):
    return {"center": [round((a + b) / 2, 3) for a, b in zip(lo, hi)], "size": [round(b - a, 3) for a, b in zip(lo, hi)], "yaw": 0}

def union(parts):
    return [min(p[0][i] for p in parts) for i in range(3)], [max(p[1][i] for p in parts) for i in range(3)]

def pieces(name, x0, x1, y0, y1):
    return [(o["min"], o["max"]) for o in truth if name in o["name"] and x0 <= (o["min"][0] + o["max"][0]) / 2 <= x1 and y0 <= (o["min"][1] + o["max"][1]) / 2 <= y1]

def center(b):
    return [(b[0][i] + b[1][i]) / 2 for i in range(3)]

chairs = [(o["min"], o["max"]) for o in truth if "SwivlChair" in o["name"]]
def chair_near(x, y):
    return min(chairs, key=lambda b: abs(center(b)[0] - x) + abs(center(b)[1] - y))

south = union(pieces("BigWhiteTable", -1, 3, -2.5, -0.4))
north = union(pieces("BigWhiteTable", -1, 3, 2.2, 4.2))
desk = union(pieces("woodenDesk", -3, -1, -6.5, -3.5))
south_chairs = [b for b in chairs if -2.6 < center(b)[1] < 0.5 and -1.2 < center(b)[0] < 3]
north_chairs = [b for b in chairs if 1.5 < center(b)[1] < 3.5]

def margin(lo, hi, m):
    return [[lo[0] - m, lo[1] - m], [hi[0] + m, lo[1] - m], [hi[0] + m, hi[1] + m], [lo[0] - m, hi[1] + m]]

T = []
def task(id, prompt, kind, truth, view=None):
    T.append({"id": id, "session": session, "prompt": prompt, "kind": kind, "truth": truth, **({"view": view} if view else {})})

task("white_table_south", "There's a big white table around x = 1.3, y = -1.4. Put a bounding box around it, labelled 'white table'.", "box", [box(*south)])
task("white_table_north_view", "The camera is looking at a big white table. Box that table tightly, label 'white table'.", "box", [box(*north)], {"target": [1.3, 3.15, 0.4], "distance": 4})
task("one_chair", "Put a box labelled 'chair' around the swivel chair at about x = 0.66, y = -0.16.", "box", [box(*chair_near(0.66, -0.16))])
task("chair_in_view", "Box the chair in the middle of the current view (label 'chair').", "box", [box(*chair_near(2.18, -0.74))], {"target": [2.18, -0.74, 0.5], "distance": 2.2})
task("chair_by_coords", "Box the chair closest to x = 2.4, y = 2.3 (label 'chair').", "box", [box(*chair_near(2.37, 2.28))])
task("desk_view", "The camera is looking at a wooden desk. Put a box around that desk, labelled 'desk'.", "box", [box(*desk)], {"target": [-1.87, -4.6, 0.35], "distance": 3.5})
task("north_chairs", "Box every chair around the white table near y = 3 (label each 'chair').", "boxes", [box(*b) for b in north_chairs])
task("south_chairs", "Box every swivel chair around the white table near x = 1.3, y = -1.4 (label each 'chair').", "boxes", [box(*b) for b in south_chairs])
task("chairs_in_view", "Box every chair you can see in the current view (label each 'chair').", "boxes", [box(*b) for b in north_chairs], {"target": [1.55, 2.6, 0.5], "distance": 3.2})
task("both_tables", "Box both big white tables (label each 'white table').", "boxes", [box(*south), box(*north)])
task("no_go_south_table", "Generate floor plans if there are none, then add a no-go area on floor 0 named 'meeting area' covering the white table near y = -1.4 with about half a meter of margin.", "area", margin(south[0], south[1], 0.5))
task("no_go_desk", "Add a no-go area on floor 0 named 'desk' around the wooden desk near x = -1.9, y = -4.6, about 30 cm of margin (generate floor plans first if needed).", "area", margin(desk[0], desk[1], 0.3))
json.dump(T, open(sys.argv[3], "w"), indent=1)
print(len(T), "tasks;", len(south_chairs), "south chairs,", len(north_chairs), "north chairs")
