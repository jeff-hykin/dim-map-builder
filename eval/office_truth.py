"""Ground-truth object boxes for the MuJoCo office (dimos data/mujoco_sim/scene_office1.xml), straight from the
simulator: every geom's world AABB, grouped by object name, split into instances where geoms don't touch.
Prints JSON (world/odom frame of the sim; z up, floor at 0)."""
import collections, json, os, re, sys
import mujoco, numpy as np

os.chdir(sys.argv[1] if len(sys.argv) > 1 else "data/mujoco_sim")
m = mujoco.MjModel.from_xml_path("scene_office1.xml")
d = mujoco.MjData(m)
mujoco.mj_forward(m, d)
groups = collections.defaultdict(list)
for g in range(m.ngeom):
    name = mujoco.mj_id2name(m, mujoco.mjtObj.mjOBJ_GEOM, g) or ""
    key = re.sub(r"(_convex_\d+|_BLACK|_\d{3})$", "", name)
    c, h = m.geom_aabb[g][:3], m.geom_aabb[g][3:]
    corners = np.array([[x, y, z] for x in (-1, 1) for y in (-1, 1) for z in (-1, 1)]) * h + c
    w = corners @ d.geom_xmat[g].reshape(3, 3).T + d.geom_xpos[g]
    groups[key].append((w.min(0), w.max(0)))
out = []
for key, boxes in groups.items():
    # one object's mesh can be split into convex pieces scattered over several instances (the chairs): drop pieces
    # bigger than a piece of furniture, then group the rest by overlap
    if len(boxes) > 3:
        boxes = [b for b in boxes if np.max(b[1][:2] - b[0][:2]) < 1.2] or boxes
    parent = list(range(len(boxes)))
    def find(i):
        while parent[i] != i:
            parent[i] = parent[parent[i]]
            i = parent[i]
        return i
    for i in range(len(boxes)):
        for j in range(i + 1, len(boxes)):
            a, b = boxes[i], boxes[j]
            if np.all(a[0] <= b[1] - 0.01) and np.all(b[0] <= a[1] - 0.01):
                parent[find(i)] = find(j)
    members = collections.defaultdict(list)
    for i in range(len(boxes)):
        members[find(i)].append(boxes[i])
    for parts in members.values():
        lo = np.min([p[0] for p in parts], 0)
        hi = np.max([p[1] for p in parts], 0)
        if np.any(np.abs(lo) > 1e5) or np.prod(hi - lo) < 0.002:
            continue
        out.append({"name": key.replace("geom_", ""), "min": np.round(lo, 3).tolist(), "max": np.round(hi, 3).tolist(), "parts": len(parts)})
out.sort(key=lambda o: o["name"])
print(json.dumps(out, indent=1))
