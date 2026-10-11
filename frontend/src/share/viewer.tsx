// The shared map's page (Share → as HTML): built on its own (vite.share.config.ts → dist/share-viewer.js) and gzipped
// into the exported file. It draws the packed voxels with the editor's renderer (dimos-react's 3D view and point
// shader: the same look, slice turn and clip), the floor selector bottom right for a multi-floor map, Frame / Top.
// Off Desktop, dimos-react's bundled tokens give it the Portal look.
import { useCallback, useEffect, useRef, useState } from "react"
import { createRoot } from "react-dom/client"
import * as THREE from "three"
import { Card, DimThemeProvider, EmptyState, Hud, Label, Segmented, Text } from "dimos-react"
import { applyLook, CUBE_SHADES, makePointMaterial, Scene3D, SceneControls, setClipBox, type Viewer } from "dimos-react/three"
import { fromBase64, SHARE_IDS, type SharedMap, unpackVoxels } from "./pack.ts"

// the page's shade name, if this library still has it (an older share may name one it dropped)
const knownShade = (shade?: string): keyof typeof CUBE_SHADES => {
    return shade !== undefined && Object.hasOwn(CUBE_SHADES, shade) ? (shade as keyof typeof CUBE_SHADES) : "soft"
}

function percentile(values: Float32Array, axis: number, low: number, high: number): [number, number] {
    const count = values.length / 3
    const step = Math.max(1, Math.floor(count / 20000))
    const sample: number[] = []
    for (let index = 0; index < count; index += step) {
        sample.push(values[index * 3 + axis])
    }
    sample.sort((a, b) => a - b)
    return [sample[Math.floor(low * (sample.length - 1))] ?? 0, sample[Math.floor(high * (sample.length - 1))] ?? 1]
}

/** the map frame hangs under a root the slice's yaw turns, as in the editor (core/scene.ts) */
function buildMap(viewer: Viewer, meta: SharedMap, positions: Float32Array) {
    const root = new THREE.Group()
    root.rotation.z = meta.slice?.yaw ?? 0
    viewer.scene.add(root)
    const geometry = new THREE.BufferGeometry()
    const count = positions.length / 3
    geometry.setAttribute("position", new THREE.BufferAttribute(positions, 3))
    geometry.setAttribute("aTime", new THREE.BufferAttribute(new Float32Array(count), 1))
    geometry.setAttribute("aIntensity", new THREE.BufferAttribute(new Uint8Array(count), 1, true))
    const material = makePointMaterial(viewer.pixelsPerMeter)
    const points = new THREE.Points(geometry, material)
    points.frustumCulled = false
    points.renderOrder = 1
    root.add(points)
    applyLook(material, {
        style: meta.look.style,
        size: meta.voxelSize * meta.look.scale,
        colorMode: "height",
        gradient: meta.look.gradient,
        period: meta.look.period,
        axis: 2,
        rangeMin: null,
        rangeMax: null,
        opacity: 1,
        cubeShade: knownShade(meta.look.shade),
    }, meta.range)
    root.updateMatrixWorld()
    const frame = () => {
        const ranges = [0, 1, 2].map((axis) => percentile(positions, axis, 0.02, 0.98))
        const center = new THREE.Vector3(...ranges.map(([lo, hi]) => (lo + hi) / 2)).applyMatrix4(root.matrixWorld)
        viewer.frame(center, Math.max(3, Math.hypot(...ranges.map(([lo, hi]) => hi - lo)) * 0.75))
    }
    const top = () => {
        const target = viewer.controls.target.clone()
        viewer.topDown(target, Math.max(4, viewer.camera.position.distanceTo(target)))
    }
    // the clip: the slice's box (x, y in the turned frame) and the shown floor's band, intersected (scene.ts #applyClip)
    const showFloor = (floor: number | null) => {
        const slice = meta.slice
        const band = floor === null ? null : meta.floors[floor]?.band ?? null
        const big = 1e9
        setClipBox(
            material,
            [slice ? slice.xMin : -big, slice ? slice.yMin : -big, Math.max(slice ? slice.zMin : -big, band ? band[0] : -big)],
            [slice ? slice.xMax : big, slice ? slice.yMax : big, Math.min(slice ? slice.zMax : big, band ? band[1] : big)],
        )
        viewer.requestRender()
    }
    return { frame, top, showFloor }
}

function SharedMapPage({ meta, positions }: { meta: SharedMap; positions: Float32Array }) {
    const [map, setMap] = useState<ReturnType<typeof buildMap> | null>(null)
    const [shown, setShown] = useState<number | null>(meta.floors.length > 1 ? meta.floor : null)
    const mapRef = useRef(map)
    mapRef.current = map
    const onViewer = useCallback((viewer: Viewer) => {
        const built = buildMap(viewer, meta, positions)
        if (meta.camera) {
            viewer.controls.target.set(...meta.camera.target)
            viewer.camera.position.set(...meta.camera.position)
            viewer.controls.update()
            viewer.requestRender()
        } else {
            built.frame()
        }
        setMap(built)
        document.documentElement.dataset.ready = "1"
    }, [])
    useEffect(() => map?.showFloor(shown), [map, shown])
    useEffect(() => {
        const keydown = (event: KeyboardEvent) => {
            if (event.metaKey || event.ctrlKey || event.altKey) {
                return
            }
            if (event.key === "f" || event.key === "F") {
                mapRef.current?.frame()
            } else if (event.key === "t" || event.key === "T") {
                mapRef.current?.top()
            }
        }
        addEventListener("keydown", keydown)
        return () => removeEventListener("keydown", keydown)
    }, [])
    // the floor selector (ui/FloorPicker.tsx's): a lift's buttons, the top floor on top, then All
    const floors = [
        ...meta.floors
            .map((storey, index) => ({
                value: index,
                label: `F${index + 1} · ${storey.level.toFixed(1)}`,
                title: `floor ${index + 1}: ${storey.band[0].toFixed(2)} to ${storey.band[1].toFixed(2)} m (floor at ${storey.level.toFixed(2)} m)`,
                props: { "data-storey": index },
            }))
            .reverse(),
        { value: -1, label: "All", title: "every floor", props: { "data-floor": "all" } },
    ]
    return (
        <Scene3D fixedFrame="world" onViewer={onViewer} style={{ position: "fixed", inset: 0 }}>
            <Hud position="top-left">
                <Card glass padded={false} style={{ padding: "6px 10px", gap: 2, maxWidth: "calc(100vw - 140px)" }}>
                    <Text className="dr-title" style={{ whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{meta.name}</Text>
                    <Text small mono muted>{`${(positions.length / 3).toLocaleString()} voxels · ${(meta.voxelSize * 100).toFixed(0)} cm · ${meta.exported.slice(0, 10)}`}</Text>
                </Card>
            </Hud>
            <SceneControls position="top-right" onFrameAll={() => map?.frame()} onTopDown={() => map?.top()} />
            {meta.floors.length > 1 && (
                <Hud position="bottom-right">
                    <Card glass padded={false} style={{ padding: 6, gap: 4 }} data-floor-picker>
                        <Label style={{ textAlign: "center" }}>floor</Label>
                        <Segmented vertical size="sm" label="floor" items={floors} value={shown ?? -1} onChange={(value: number) => setShown(value === -1 ? null : value)} />
                    </Card>
                </Hud>
            )}
        </Scene3D>
    )
}

async function main() {
    const meta: SharedMap = JSON.parse(document.getElementById(SHARE_IDS.meta)!.textContent!)
    const page = document.getElementById("dim-share")!
    const root = createRoot(page)
    try {
        const { positions } = await unpackVoxels(fromBase64(document.getElementById(SHARE_IDS.voxels)!.textContent!))
        root.render(
            <DimThemeProvider skin="portal">
                <SharedMapPage meta={meta} positions={positions} />
            </DimThemeProvider>,
        )
    } catch (error) {
        console.error(error)
        root.render(
            <DimThemeProvider skin="portal">
                <EmptyState layer icon="warn" tone="danger" title="This map couldn't be shown" message={String((error as Error)?.message ?? error)} />
            </DimThemeProvider>,
        )
    }
}

main()
