// The Slicer: a step-by-step view of the map (not an edit: no voxel is removed). It flies out to the whole map, then
// 1/4 cuts it to a height band (live, in the shader), 2/4 turns it so the walls line up with x and y (in the 2D view,
// over a grid, the 3D camera held still; with an "auto" angle from the walls' directions), 3/4 crops x / y in that turned
// frame (a draggable rectangle), and 4/4 saves it (map/slice) and flies back to the cropped, aligned map.
import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react"
import * as THREE from "three"
import { api, type Slice } from "../core/api.ts"
import { AngleDial, Button, Card, Column, Heading, NumberInput, RangeSlider, Row, Slider, Spacer, Stepper, Text } from "dimos-react"
import type { Context } from "./context.ts"

const BIG = 1e9
const STEPS = ["Height", "Align", "Crop", "Done"]

function Meters({ value, onChange, step, data, unit }: { value: number; onChange: (value: number) => void; step: number; data: string; unit: string }) {
    return <NumberInput size="sm" step={step} precision={unit === "°" ? 1 : 2} unit={unit} value={value} onChange={onChange} data-slicer={data} style={{ width: 92 }} />
}

/** `onStep` hears each step (App shows the 2D view for the align step) */
export function SlicerWizard({ context, onClose, onStep }: { context: Context; onClose: () => void; onStep?: (step: number) => void }) {
    const { session, scene, run } = context
    const bounds = session?.bounds ?? [[-5, -5, -1], [5, 5, 3]]
    const previous = useRef<Slice | null>(scene?.slice ?? null)
    const [step, setStep] = useState(0)
    const [draft, setDraftState] = useState<Slice>(() => previous.current ?? { zMin: bounds[0][2], zMax: bounds[1][2], yaw: 0, xMin: -BIG, xMax: BIG, yMin: -BIG, yMax: BIG })
    const [, tick] = useState(0)
    const draftRef = useRef(draft)
    draftRef.current = draft

    const show = (next: Slice, at = step) => {
        // the crop only shows once it's picked: while picking, the whole (turned) map stays visible around it
        scene?.setSlice(next, at > 3)
    }
    const setDraft = (patch: Partial<Slice>) => {
        const next = { ...draftRef.current, ...patch }
        setDraftState(next)
        show(next)
    }
    const zMid = (draft.zMin + draft.zMax) / 2
    const cropped = draft.xMax - draft.xMin < BIG

    /** the map's box in the world (the turned frame), within the height band */
    const mapBox = (slice: Slice) => {
        const [x0, y0, x1, y1] = scene!.turnedBounds(slice.yaw, slice.zMin, slice.zMax)
        return { min: new THREE.Vector3(x0, y0, slice.zMin), max: new THREE.Vector3(x1, y1, slice.zMax) }
    }

    useEffect(() => {
        onStep?.(step)
    }, [step])
    // the align step happens in the 2D view: the 3D camera stays where it was, for after
    useEffect(() => {
        if (!scene || step !== 2) {
            return
        }
        scene.viewer.controls.enabled = false
        return () => {
            scene.viewer.controls.enabled = true
        }
    }, [scene, step])

    // 0: fly out to the whole map, then the first step
    useEffect(() => {
        if (!scene) {
            return
        }
        show(draftRef.current, 1)
        scene.setXray(false)
        const box = mapBox({ ...draftRef.current, zMin: bounds[0][2], zMax: bounds[1][2] })
        const view = scene.viewOf(box.min, box.max, false)
        scene.flyTo(view.position, view.target, 900).then(() => setStep(1))
        return () => scene.setXray(false)
    }, [])

    // entering a step sets up its view
    const go = async (next: number) => {
        if (!scene || !session) {
            return
        }
        if (next === 2) {
            scene.setXray(false)
            scene.setSlice(draftRef.current, false)
            setStep(2)
            return
        }
        if (next === 3) {
            scene.setXray(true)
            let current = draftRef.current
            if (!cropped) {
                const box = mapBox(current)
                const pad = 0.3
                current = { ...current, xMin: box.min.x - pad, xMax: box.max.x + pad, yMin: box.min.y - pad, yMax: box.max.y + pad }
                setDraftState(current)
            }
            scene.setSlice(current, false)
            const box = { min: new THREE.Vector3(current.xMin, current.yMin, current.zMin), max: new THREE.Vector3(current.xMax, current.yMax, current.zMax) }
            const view = scene.viewOf(box.min, box.max, true)
            setStep(next)
            await scene.flyTo(view.position, view.target, 700)
            return
        }
        if (next === 1) {
            scene.setXray(false)
            setStep(1)
            return
        }
        // 4: save, show it cropped, fly to it
        const done = draftRef.current
        scene.setXray(false)
        scene.setSlice(done, true)
        setStep(4)
        await run(api.setSlice(session.id, done), "Slice saved (a view: no voxels removed)")
        const view = scene.viewOf(new THREE.Vector3(done.xMin, done.yMin, done.zMin), new THREE.Vector3(done.xMax, done.yMax, done.zMax), false)
        await scene.flyTo(view.position, view.target, 900)
        onClose()
    }
    const cancel = () => {
        scene?.setXray(false)
        scene?.setSlice(previous.current, true)
        onClose()
    }
    const reset = async () => {
        if (!scene || !session) {
            return
        }
        scene.setXray(false)
        scene.setSlice(null)
        await run(api.setSlice(session.id, null), "Slice removed: the whole map shows")
        scene.frameMap()
        onClose()
    }
    const auto = async () => {
        if (!session) {
            return
        }
        const found = await run(api.alignment(session.id))
        if (found && found.yaw !== null) {
            setDraft({ yaw: found.yaw })
        }
    }

    // redraw the crop overlay as the camera moves
    useEffect(() => {
        if (!scene) {
            return
        }
        const listener = () => tick((n) => n + 1)
        scene.cameraListeners.add(listener)
        return () => {
            scene.cameraListeners.delete(listener)
        }
    }, [scene])

    if (!scene || !session) {
        return null
    }
    const degrees = (draft.yaw * 180) / Math.PI
    const zRange: [number, number] = [Math.floor(bounds[0][2] - 0.5), Math.ceil(bounds[1][2] + 0.5)]
    return (
        <>
            {(step === 2 || step === 3) && <AlignGrid />}
            {step === 3 && cropped && <CropOverlay context={context} draft={draft} z={zMid} onChange={setDraft} />}
            <Card glass data-slicer-step={step} style={{ width: "min(380px, calc(100vw - 40px))", borderColor: "color-mix(in srgb, var(--warn) 45%, transparent)" }}>
                <Stepper steps={STEPS.map((label) => ({ label }))} current={Math.max(0, Math.min(step, 4) - 1)} />
                {step === 0 && <Text small muted>Flying out to the whole map…</Text>}
                {step === 1 && (
                    <Column gap={6}>
                        <Heading level={3}>1/4 · Height</Heading>
                        <Text small muted>Cut the map to the heights you care about (the ceiling and what's under the floor go). It's live; nothing is deleted.</Text>
                        <RangeSlider min={zRange[0]} max={zRange[1]} step={0.05} value={[draft.zMin, draft.zMax]} showValue={false} onChange={([zMin, zMax]: [number, number]) => setDraft({ zMin, zMax })} />
                        <Row wrap gap={6}>
                            <Text small muted>z from</Text>
                            <Meters value={draft.zMin} step={0.05} unit="m" data="zMin" onChange={(zMin) => setDraft({ zMin: Math.min(zMin, draft.zMax) })} />
                            <Text small muted>to</Text>
                            <Meters value={draft.zMax} step={0.05} unit="m" data="zMax" onChange={(zMax) => setDraft({ zMax: Math.max(zMax, draft.zMin) })} />
                        </Row>
                    </Column>
                )}
                {step === 2 && (
                    <Column gap={6}>
                        <Heading level={3}>2/4 · Align</Heading>
                        <Text small muted>Turn the map until its walls run along the grid. Drag to pan, scroll to zoom.</Text>
                        <Row>
                            <AngleDial value={draft.yaw} onChange={(yaw: number) => setDraft({ yaw })} tone="warn" title="drag around to turn" data-slicer-dial />
                            <Slider min={-45} max={45} step={0.1} value={Math.max(-45, Math.min(45, degrees))} showValue={false} onChange={(value: number) => setDraft({ yaw: (value * Math.PI) / 180 })} data-slicer="yawSlider" />
                        </Row>
                        <Row gap={6}>
                            <Text small muted>turn</Text>
                            <Meters value={degrees} step={0.5} unit="°" data="yaw" onChange={(value) => setDraft({ yaw: (value * Math.PI) / 180 })} />
                            <Button size="sm" onClick={auto} title="from the walls' main direction" data-action="slicer-auto">
                                Auto
                            </Button>
                        </Row>
                    </Column>
                )}
                {step === 3 && (
                    <Column gap={6}>
                        <Heading level={3}>3/4 · Crop</Heading>
                        <Text small muted>Drag the rectangle's edges, corners or middle (in the turned frame), or type the numbers.</Text>
                        {(["x", "y"] as const).map((axis) => (
                            <Row key={axis} gap={6}>
                                <Text small muted>{axis}</Text>
                                <Meters value={draft[`${axis}Min`]} step={0.1} unit="m" data={`${axis}Min`} onChange={(value) => setDraft({ [`${axis}Min`]: Math.min(value, draft[`${axis}Max`] - 0.1) })} />
                                <Text small muted>to</Text>
                                <Meters value={draft[`${axis}Max`]} step={0.1} unit="m" data={`${axis}Max`} onChange={(value) => setDraft({ [`${axis}Max`]: Math.max(value, draft[`${axis}Min`] + 0.1) })} />
                            </Row>
                        ))}
                    </Column>
                )}
                {step === 4 && <Text small muted>Saved. Back to the map…</Text>}
                <Row wrap>
                    <Button size="sm" onClick={cancel} data-action="slicer-cancel">
                        Cancel
                    </Button>
                    {previous.current && step === 1 && (
                        <Button size="sm" variant="danger" onClick={reset} data-action="slicer-reset">
                            Show everything
                        </Button>
                    )}
                    <Spacer />
                    <Button size="sm" icon="arrow-left" disabled={step <= 1 || step >= 4} onClick={() => go(step - 1)} data-action="slicer-back">
                        Back
                    </Button>
                    <Button size="sm" variant="primary" iconRight={step === 3 ? "check" : "arrow-right"} disabled={step === 0 || step >= 4} onClick={() => go(step + 1)} data-action="slicer-next">
                        {step === 3 ? "Done" : "Next"}
                    </Button>
                </Row>
            </Card>
        </>
    )
}

/** a screen-aligned grid over the top-down view (the camera looks straight down with +y up), to line walls up against */
function AlignGrid() {
    return (
        <svg data-align-grid style={{ position: "fixed", inset: 0, width: "100%", height: "100%", zIndex: 4, pointerEvents: "none" }}>
            <defs>
                <pattern id="align-grid-small" width="40" height="40" patternUnits="userSpaceOnUse">
                    <path d="M 40 0 L 0 0 0 40" fill="none" style={{ stroke: "color-mix(in srgb, var(--ok) 18%, transparent)" }} strokeWidth="1" />
                </pattern>
                <pattern id="align-grid-big" width="200" height="200" patternUnits="userSpaceOnUse">
                    <rect width="200" height="200" fill="url(#align-grid-small)" />
                    <path d="M 200 0 L 0 0 0 200" fill="none" style={{ stroke: "color-mix(in srgb, var(--ok) 40%, transparent)" }} strokeWidth="1" />
                </pattern>
            </defs>
            <rect width="100%" height="100%" fill="url(#align-grid-big)" />
        </svg>
    )
}

/** the crop rectangle on the top-down view: the outside dimmed; edges, corners and the middle drag */
function CropOverlay({ context, draft, z, onChange }: { context: Context; draft: Slice; z: number; onChange: (patch: Partial<Slice>) => void }) {
    const { scene } = context
    const svg = useRef<SVGSVGElement>(null)
    if (!scene) {
        return null
    }
    const offset = svg.current?.getBoundingClientRect() ?? { left: 0, top: 0 }
    const page = (x: number, y: number) => {
        const [px, py] = scene.worldToPage(new THREE.Vector3(x, y, z))
        return [px - offset.left, py - offset.top]
    }
    const corners = [page(draft.xMin, draft.yMin), page(draft.xMax, draft.yMin), page(draft.xMax, draft.yMax), page(draft.xMin, draft.yMax)]
    const path = corners.map(([x, y], i) => `${i ? "L" : "M"} ${x} ${y}`).join(" ") + " Z"
    const drag = (part: string) => (event: ReactPointerEvent) => {
        event.preventDefault()
        event.stopPropagation()
        const start = scene.pageToWorld(event.clientX, event.clientY, z)
        const from = { ...draft }
        if (!start) {
            return
        }
        const move = (e: PointerEvent) => {
            const at = scene.pageToWorld(e.clientX, e.clientY, z)
            if (!at) {
                return
            }
            const [dx, dy] = [at.x - start.x, at.y - start.y]
            const patch: Partial<Slice> = {}
            if (part === "move") {
                Object.assign(patch, { xMin: from.xMin + dx, xMax: from.xMax + dx, yMin: from.yMin + dy, yMax: from.yMax + dy })
            }
            if (part.includes("w")) {
                patch.xMin = Math.min(from.xMin + dx, from.xMax - 0.2)
            }
            if (part.includes("e")) {
                patch.xMax = Math.max(from.xMax + dx, from.xMin + 0.2)
            }
            if (part.includes("s")) {
                patch.yMin = Math.min(from.yMin + dy, from.yMax - 0.2)
            }
            if (part.includes("n")) {
                patch.yMax = Math.max(from.yMax + dy, from.yMin + 0.2)
            }
            onChange(patch)
        }
        const up = () => {
            window.removeEventListener("pointermove", move)
            window.removeEventListener("pointerup", up)
        }
        window.addEventListener("pointermove", move)
        window.addEventListener("pointerup", up)
    }
    const [sw, se, ne, nw] = corners
    const mid = (a: number[], b: number[]) => [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2]
    const handles: [string, number[]][] = [["sw", sw], ["se", se], ["ne", ne], ["nw", nw], ["s", mid(sw, se)], ["e", mid(se, ne)], ["n", mid(ne, nw)], ["w", mid(nw, sw)]]
    return (
        <svg ref={svg} data-crop-overlay style={{ position: "fixed", inset: 0, width: "100%", height: "100%", zIndex: 4, pointerEvents: "none" }}>
            <path d={`M 0 0 H 10000 V 10000 H 0 Z ${path}`} style={{ fill: "color-mix(in srgb, var(--bg) 55%, transparent)" }} fillRule="evenodd" />
            <path d={path} strokeWidth="2" style={{ cursor: "move", fill: "color-mix(in srgb, var(--warn) 4%, transparent)", stroke: "var(--warn)", pointerEvents: "all" }} onPointerDown={drag("move")} />
            {handles.map(([name, [x, y]]) => (
                <rect key={name} x={x - 7} y={y - 7} width={14} height={14} rx={3} onPointerDown={drag(name)} data-crop-handle={name} style={{ fill: "var(--warn)", stroke: "var(--bg)", strokeWidth: 1.5, pointerEvents: "all", cursor: name.length === 2 ? "nwse-resize" : name === "e" || name === "w" ? "ew-resize" : "ns-resize" }} />
            ))}
        </svg>
    )
}
