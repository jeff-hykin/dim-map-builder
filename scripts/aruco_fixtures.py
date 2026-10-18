"""Writes the ArUco test frames (crates/mapping/tests/fixtures/aruco): markers of several OpenCV dictionaries, turned,
in perspective, blurred and noisy, with what cv2.aruco finds in them (ids; corners with CORNER_REFINE_SUBPIX) and where
they were drawn. Run with any python that has cv2 (e.g. dimos's .venv): python scripts/aruco_fixtures.py
"""
import json
import os
import cv2
import numpy as np

aruco = cv2.aruco
OUT = os.path.join(os.path.dirname(__file__), "..", "crates", "mapping", "tests", "fixtures", "aruco")
WIDTH, HEIGHT = 320, 240
rng = np.random.default_rng(7)
frames = []
for index, (name, ids) in enumerate([("DICT_4X4_50", [3, 49]), ("DICT_5X5_1000", [777]), ("DICT_6X6_250", [0, 131]), ("DICT_7X7_100", [42]), ("DICT_ARUCO_ORIGINAL", [1023])]):
    dictionary = aruco.getPredefinedDictionary(getattr(aruco, name))
    image = np.full((HEIGHT, WIDTH), float(rng.integers(100, 190)), np.float32)
    placed = []
    for slot, marker_id in enumerate(ids):
        cells = dictionary.markerSize + 2
        pixels, pad = cells * 16, 16
        tile = np.full((pixels + 2 * pad, pixels + 2 * pad), 255, np.uint8)
        tile[pad : pad + pixels, pad : pad + pixels] = dictionary.generateImageMarker(marker_id, pixels)
        side = rng.uniform(55, 90)
        center = np.array([WIDTH * (slot + 1) / (len(ids) + 1), HEIGHT / 2 + rng.uniform(-30, 30)])
        angle = rng.uniform(0, 2 * np.pi)
        turn = np.array([[np.cos(angle), -np.sin(angle)], [np.sin(angle), np.cos(angle)]])
        corners = np.array([[-1, -1], [1, -1], [1, 1], [-1, 1]]) * side / 2 @ turn.T + center + rng.normal(0, side * 0.07, (4, 2))
        # a pixel's center is at its integer coordinate: the marker image's outer corner is at -0.5
        source = np.array([[pad, pad], [pad + pixels, pad], [pad + pixels, pad + pixels], [pad, pad + pixels]], np.float32) - 0.5
        warp = cv2.getPerspectiveTransform(source, corners.astype(np.float32))
        warped = cv2.warpPerspective(tile.astype(np.float32), warp, (WIDTH, HEIGHT), flags=cv2.INTER_LINEAR)
        mask = cv2.warpPerspective(np.ones_like(tile, np.float32), warp, (WIDTH, HEIGHT), flags=cv2.INTER_LINEAR)
        image = image * (1 - mask) + (128 + (warped - 128) * rng.uniform(0.6, 0.9)) * mask
        placed.append({"id": marker_id, "corners": corners.tolist()})
    image = cv2.GaussianBlur(image, (0, 0), rng.uniform(0.5, 1.0)) + rng.normal(0, 2.0, image.shape)
    image = np.clip(image, 0, 255).astype(np.uint8)
    file = f"{index}_{name}.png"
    cv2.imwrite(os.path.join(OUT, file), image)
    parameters = aruco.DetectorParameters()
    parameters.cornerRefinementMethod = aruco.CORNER_REFINE_SUBPIX
    found_corners, found_ids, _ = aruco.ArucoDetector(dictionary, parameters).detectMarkers(image)
    found = [{"id": int(i), "corners": c.reshape(4, 2).tolist()} for i, c in zip(found_ids.ravel(), found_corners)]
    frames.append({"file": file, "dictionary": name, "cv2": found, "placed": placed})
json.dump({"opencv": cv2.__version__, "frames": frames}, open(os.path.join(OUT, "expected.json"), "w"), indent=1)
print([(f["file"], [m["id"] for m in f["cv2"]]) for f in frames])
