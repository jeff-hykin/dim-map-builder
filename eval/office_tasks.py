"""Builds eval/tasks.json for the sim office from the simulator's own geometry (office_truth.py output).
Ground truth = MuJoCo's object boxes, in the sim's world frame (= the go2 recording's odom frame: the robot spawns at
the origin, unrotated; checked against the built map before scoring)."""
import json, sys

truth = json.load(open(sys.argv[1]))
session = sys.argv[2]

def box(lo, hi):
    return {"center": [(a + b) / 2 for a, b in zip(lo, hi)], "size": [b - a for a, b in zip(lo, hi)], "yaw": 0}

def union(parts):
    return [min(p[0][i] for p in parts) for i in range(3)], [max(p[1][i] for p in parts) for i in range(3)]

def pieces(name, x0, x1, y0, y1):
    return [(o["min"], o["max"]) for o in truth if name in o["name"] and x0 <= (o["min"][0] + o["max"][0]) / 2 <= x1 and y0 <= (o["min"][1] + o["max"][1]) / 2 <= y1]

swivel = sorted([(o["min"], o["max"]) for o in truth if "SwivlChair" in o["name"]], key=lambda b: (b[0][1], b[0][0]))
plastic = [(o["min"], o["max"]) for o in truth if "plastic_cycles" in o["name"] and o["parts"] < 40]
meeting = union(pieces("meetingTable", -3, 2, 6.5, 9.5))
white_a = union(pieces("BigWhiteTable", -1, 3, -2.5, -0.4))
white_b = union(pieces("BigWhiteTable", -1, 3, 2.2, 4.2))
shelf = next((o["min"], o["max"]) for o in truth if "shelving" in o["name"] and o["parts"] > 100)
desk_left = union(pieces("woodenDesk", -3, -1, -6.5, -3.5))
desk_right = union(pieces("woodenDesk", 1, 3, -8.5, -6.6))
# the chair nearest the big white table A, south-west of it
lone = min(swivel, key=lambda b: abs((b[0][0] + b[1][0]) / 2 - 0.66) + abs((b[0][1] + b[1][1]) / 2 + 0.16))

T = []
def task(id, prompt, kind, truth_boxes, view=None):
    T.append({"id": id, "session": session, "prompt": prompt, "kind": kind, "truth": truth_boxes, **({"view": view} if view else {})})

task("meeting_table", "Put a bounding box labelled 'meeting table' around the meeting table (the long table with plastic chairs around it, near y = 8).", "box", [box(*meeting)], {"target": [-0.4, 8.0, 0.4], "distance": 6})
task("shelf", "Add a bounding box labelled 'shelf' around the low shelving unit against the wall near x = 4.6, y = -8.4.", "box", [box(*shelf)], {"target": [4.6, -8.3, 0.5], "distance": 4})
task("white_table_south", "There's a big white table around x = 1.3, y = -1.4. Box it, label 'white table'.", "box", [box(*white_a)])
task("white_table_north", "Box the big white table near x = 1.3, y = 3.2 (label 'white table 2'). Fit it tightly.", "box", [box(*white_b)], {"target": [1.3, 3.2, 0.4], "distance": 5})
task("desk_left", "The camera is looking at a wooden desk. Put a box around that desk, labelled 'desk'.", "box", [box(*desk_left)], {"target": [-1.9, -4.8, 0.4], "distance": 3.5})
task("desk_right", "Box the wooden desk around x = 2, y = -7.5 (label 'desk 2').", "box", [box(*desk_right)])
task("one_chair", "Put a box labelled 'chair' around the swivel chair at about x = 0.66, y = -0.16.", "box", [box(*lone)], {"target": [0.66, -0.16, 0.5], "distance": 3})
task("chair_in_view", "Box the chair in the middle of the current view (label 'chair').", "box", [box(*lone)], {"target": [0.66, -0.16, 0.5], "distance": 2.5})
task("meeting_chairs", "Box every chair around the meeting table (the one near y = 8). Label each 'chair'.", "boxes", [box(*b) for b in plastic], {"target": [-0.4, 8.0, 0.4], "distance": 7})
task("swivel_chairs_south", "Box every swivel chair in the area between y = -8.5 and y = -2.5 (label each 'chair').", "boxes", [box(*b) for b in swivel if (b[0][1] + b[1][1]) / 2 < -2.5])
task("no_go_meeting", "Generate floor plans if there are none, then add a no-go area on floor 0 named 'meeting room' covering the meeting table and its chairs.", "area",
     [[-2.8, 6.5], [1.9, 6.5], [1.9, 9.5], [-2.8, 9.5]])
task("no_go_shelf", "Add a no-go area (floor 0) named 'shelf' around the shelving unit near x = 4.6, y = -8.4, with about 30 cm of margin.", "area",
     [[3.52, -8.85], [5.71, -8.85], [5.71, -7.86], [3.52, -7.86]])
json.dump(T, open(sys.argv[3], "w"), indent=1)
print(len(T), "tasks;", len(plastic), "meeting chairs,", len([b for b in swivel if (b[0][1] + b[1][1]) / 2 < -2.5]), "south swivel chairs")
