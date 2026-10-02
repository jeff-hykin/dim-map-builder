// The Slicer: a step-by-step view of the map (not an edit: no voxel is removed). It flies out to the whole map, then
// 1/4 cuts it to a height band (live, in the shader), 2/4 turns it so the walls line up with x and y (a top-down
// x-ray where walls glow, over a grid, with an "auto" angle from the walls' directions), 3/4 crops x / y in that turned
// frame (a draggable rectangle), and 4/4 saves it (map/slice) and flies back to the cropped, aligned map.
import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react"
import * as THREE from "three"
import { api, type Slice } from "../core/api.ts"
import type { Context } from "./context.ts"

const BIG = 1e9
const STEPS = ["Height", "Align", "Crop", "Done"]

function NumberField({ value, onChange, step, data, unit }: { value: number; onChange: (value: number) => void; step: number; data: string; unit: string }) {
    const [text, setText] = useState(value.toFixed(2))
    useEffect(() => setText(unit === "°" ? value.toFixed(1) : value.toFixed(2)), [value, unit])
    return (
        <span className="row" style={{ margin: 0 }}>
            <input className="number" type="number" step={step} value={text} data-slicer={data} onChange={(event) => setText(event.target.value)} onBlur={() => text !== "" && onChange(Number(text))} onKeyDown={(event) => event.key === "Enter" && text !== "" && onChange(Number(text))} />
            <span className="dim">{unit}</span>
        </span>
    )
}

/** two handles on one track */
function DualRange({ low, high, min, max, onChange }: { low: number; high: number; min: number; max: number; onChange: (low: number, high: number) => void }) {
    const percent = (value: number) => `${((value - min) / (max - min)) * 100}%`
    return (
        <div className="dual-range wide">
            <div className="track" />
            <div className="fill" style={{ left: percent(Math.max(min, low)), right: `calc(100% - ${percent(Math.min(max, high))})` }} />
            <input type="range" min={min} max={max} step={0.05} value={low} onChange={(event) => onChange(Math.min(Number(event.target.value), high), high)} data-slicer-handle="zMin" />
            <input type="range" min={min} max={max} step={0.05} value={high} onChange={(event) => onChange(low, Math.max(Number(event.target.value), low))} data-slicer-handle="zMax" />
        </div>
    )
}

export function SlicerWizard({ context, onClose }: { context: Context; onClose: () => void }) {
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
        if (next === 2 || next === 3) {
            scene.setXray(true)
            let current = draftRef.current
            if (next === 3 && !cropped) {
                const box = mapBox(current)
                const pad = 0.3
                current = { ...current, xMin: box.min.x - pad, xMax: box.max.x + pad, yMin: box.min.y - pad, yMax: box.max.y + pad }
                setDraftState(current)
            }
            scene.setSlice(current, false)
            const box = next === 3 ? { min: new THREE.Vector3(current.xMin, current.yMin, current.zMin), max: new THREE.Vector3(current.xMax, current.yMax, current.zMax) } : mapBox(current)
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

    // the dial: drag around its centre to turn the map
    const dial = (event: ReactPointerEvent<HTMLDivElement>) => {
        const rect = event.currentTarget.getBoundingClientRect()
        const centre = [rect.left + rect.width / 2, rect.top + rect.height / 2]
        const start = Math.atan2(event.clientY - centre[1], event.clientX - centre[0])
        const yaw0 = draftRef.current.yaw
        const move = (e: PointerEvent) => {
            const angle = Math.atan2(e.clientY - centre[1], e.clientX - centre[0])
            // screen y points down: turning clockwise on screen is a negative yaw
            let yaw = yaw0 - (angle - start)
            yaw = Math.atan2(Math.sin(yaw), Math.cos(yaw))
            setDraft({ yaw })
        }
        const up = () => {
            window.removeEventListener("pointermove", move)
            window.removeEventListener("pointerup", up)
        }
        window.addEventListener("pointermove", move)
        window.addEventListener("pointerup", up)
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
            <div className="slicer-panel" data-slicer-step={step}>
                <div className="stepper">
                    {STEPS.map((name, index) => (
                        <span key={name} className={`dot ${step === index + 1 ? "on" : step > index + 1 ? "done" : ""}`}>
                            <b>{index + 1}</b> {name}
                        </span>
                    ))}
                    <span className="dim">{step === 0 ? "…" : `${Math.min(step, 4)}/4`}</span>
                </div>
                {step === 0 && <div className="hint">Flying out to the whole map…</div>}
                {step === 1 && (
                    <>
                        <h3>1/4 · Height</h3>
                        <div className="hint">Cut the map to the heights you care about (the ceiling and what's under the floor go). It's live; nothing is deleted.</div>
                        <DualRange low={draft.zMin} high={draft.zMax} min={zRange[0]} max={zRange[1]} onChange={(zMin, zMax) => setDraft({ zMin, zMax })} />
                        <div className="row">
                            <span className="dim">z from</span>
                            <NumberField value={draft.zMin} step={0.05} unit="m" data="zMin" onChange={(zMin) => setDraft({ zMin: Math.min(zMin, draft.zMax) })} />
                            <span className="dim">to</span>
                            <NumberField value={draft.zMax} step={0.05} unit="m" data="zMax" onChange={(zMax) => setDraft({ zMax: Math.max(zMax, draft.zMin) })} />
                        </div>
                    </>
                )}
                {step === 2 && (
                    <>
                        <h3>2/4 · Align</h3>
                        <div className="hint">Turn the map until its walls run along the grid. The x-ray view makes walls glow brightest.</div>
                        <div className="row">
                            <div className="dial" onPointerDown={dial} title="drag around to turn" data-slicer-dial>
                                <div className="dial-hand" style={{ transform: `rotate(${-degrees}deg)` }} />
                            </div>
                            <input type="range" min={-45} max={45} step={0.1} value={Math.max(-45, Math.min(45, degrees))} onChange={(event) => setDraft({ yaw: (Number(event.target.value) * Math.PI) / 180 })} data-slicer="yawSlider" />
                        </div>
                        <div className="row">
                            <span className="dim">turn</span>
                            <NumberField value={degrees} step={0.5} unit="°" data="yaw" onChange={(value) => setDraft({ yaw: (value * Math.PI) / 180 })} />
                            <button type="button" className="button" onClick={auto} title="from the walls' main direction" data-action="slicer-auto">
                                Auto
                            </button>
                        </div>
                    </>
                )}
                {step === 3 && (
                    <>
                        <h3>3/4 · Crop</h3>
                        <div className="hint">Drag the rectangle's edges, corners or middle (in the turned frame), or type the numbers.</div>
                        {(["x", "y"] as const).map((axis) => (
                            <div className="row" key={axis}>
                                <span className="dim">{axis}</span>
                                <NumberField value={draft[`${axis}Min`]} step={0.1} unit="m" data={`${axis}Min`} onChange={(value) => setDraft({ [`${axis}Min`]: Math.min(value, draft[`${axis}Max`] - 0.1) })} />
                                <span className="dim">to</span>
                                <NumberField value={draft[`${axis}Max`]} step={0.1} unit="m" data={`${axis}Max`} onChange={(value) => setDraft({ [`${axis}Max`]: Math.max(value, draft[`${axis}Min`] + 0.1) })} />
                            </div>
                        ))}
                    </>
                )}
                {step === 4 && <div className="hint">Saved. Back to the map…</div>}
                <div className="row slicer-buttons">
                    <button type="button" className="button" onClick={cancel} data-action="slicer-cancel">
                        Cancel
                    </button>
                    {previous.current && step === 1 && (
                        <button type="button" className="button danger" onClick={reset} data-action="slicer-reset">
                            Show everything
                        </button>
                    )}
                    <span className="spacer" />
                    <button type="button" className="button" disabled={step <= 1 || step >= 4} onClick={() => go(step - 1)} data-action="slicer-back">
                        ← Back
                    </button>
                    <button type="button" className="button primary" disabled={step === 0 || step >= 4} onClick={() => go(step + 1)} data-action="slicer-next">
                        {step === 3 ? "Done ✓" : "Next →"}
                    </button>
                </div>
            </div>
        </>
    )
}

/** a screen-aligned grid over the top-down view (the camera looks straight down with +y up), to line walls up against */
function AlignGrid() {
    return (
        <svg className="align-grid" data-align-grid>
            <defs>
                <pattern id="align-grid-small" width="40" height="40" patternUnits="userSpaceOnUse">
                    <path d="M 40 0 L 0 0 0 40" fill="none" stroke="rgba(122, 240, 168, 0.18)" strokeWidth="1" />
                </pattern>
                <pattern id="align-grid-big" width="200" height="200" patternUnits="userSpaceOnUse">
                    <rect width="200" height="200" fill="url(#align-grid-small)" />
                    <path d="M 200 0 L 0 0 0 200" fill="none" stroke="rgba(122, 240, 168, 0.4)" strokeWidth="1" />
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
        <svg className="crop-overlay" ref={svg} data-crop-overlay>
            <path d={`M 0 0 H 10000 V 10000 H 0 Z ${path}`} fill="rgba(3, 5, 9, 0.55)" fillRule="evenodd" />
            <path d={path} fill="rgba(255, 209, 102, 0.04)" stroke="#ffd166" strokeWidth="2" style={{ cursor: "move" }} onPointerDown={drag("move")} />
            {handles.map(([name, [x, y]]) => (
                <rect key={name} x={x - 7} y={y - 7} width={14} height={14} rx={3} className="crop-handle" onPointerDown={drag(name)} data-crop-handle={name} style={{ cursor: name.length === 2 ? "nwse-resize" : name === "e" || name === "w" ? "ew-resize" : "ns-resize" }} />
            ))}
        </svg>
    )
}
