// Stage 3: clean the map. Every tool acts on a scope (what's in view, the yellow region box, or the whole map), can be
// previewed (the voxels it would remove turn red) and is undoable.
import { useState } from "react"
import { api, type Region } from "../core/api.ts"
import { GRADIENTS } from "../render/gradients.ts"
import type { Context } from "./context.ts"

interface Tool {
    op: string
    name: string
    about: string
    params?: { key: string; label: string; min: number; max: number; step: number; value: number; unit?: string }[]
}

const TOOLS: Tool[] = [
    { op: "floating", name: "Floating clusters", about: "Small groups of voxels touching nothing: dust, specks, a person walking past.", params: [{ key: "minVoxels", label: "smaller than", min: 3, max: 400, step: 1, value: 30, unit: "voxels" }] },
    { op: "outliers", name: "Outliers", about: "Voxels far from their neighbours (statistical).", params: [{ key: "stdRatio", label: "σ above mean", min: 0.5, max: 4, step: 0.1, value: 2 }] },
    { op: "floor", name: "Floor", about: "The floor surface of every storey (keeps what stands on it).", params: [{ key: "thickness", label: "thickness", min: 0.02, max: 0.3, step: 0.01, value: 0.08, unit: "m" }] },
    { op: "walls", name: "Walls", about: "Vertical surfaces above the floor.", params: [{ key: "minHeight", label: "above floor", min: 0, max: 2, step: 0.05, value: 0.3, unit: "m" }] },
]

export function CleanPanel({ context }: { context: Context }) {
    const { session, scene, ui, setUi, run } = context
    const [params, setParams] = useState<Record<string, Record<string, number>>>(() => Object.fromEntries(TOOLS.map((t) => [t.op, Object.fromEntries((t.params ?? []).map((p) => [p.key, p.value]))])))
    const [previewing, setPreviewing] = useState<string | null>(null)
    const [heights, setHeights] = useState<[number, number]>([-0.5, 2.5])
    const [yaw, setYaw] = useState(5)
    if (!session || !scene) {
        return null
    }
    const region = (): Region => {
        if (ui.scope === "region" && scene.region) {
            return { kind: "box", ...scene.region }
        }
        if (ui.scope === "view") {
            return { kind: "view", matrix: scene.viewState().viewProjection as number[] }
        }
        return { kind: "all" }
    }
    const preview = async (op: string, extra: Record<string, unknown> = {}) => {
        const result = await run(api.op(session.id, op, region(), { ...params[op], ...extra }, true))
        if (result) {
            scene.setPreview(result.preview ?? null)
            setPreviewing(`${result.label}: ${result.changed.toLocaleString()} voxels`)
        }
    }
    const apply = async (op: string, extra: Record<string, unknown> = {}, scope?: Region) => {
        scene.setPreview(null)
        setPreviewing(null)
        await run(api.op(session.id, op, scope ?? region(), { ...params[op], ...extra }), (r) => `${r.label}: removed ${r.changed.toLocaleString()} voxels (⌘Z undoes)`)
    }
    const ensureRegion = () => {
        if (!scene.region) {
            scene.setRegion(scene.regionFromView())
        }
        scene.select({ kind: "region", id: "region" })
        setUi({ scope: "region", region: scene.region })
    }
    return (
        <div>
            <div className="panel-head">
                <h2>Clean the map</h2>
                <p>{session.voxels.toLocaleString()} voxels. Preview shows what a tool would remove in red; every change can be undone (⌘Z).</p>
            </div>

            <h3>Scope</h3>
            <div className="row">
                <div className="seg">
                    {(["view", "region", "all"] as const).map((scope) => (
                        <button key={scope} type="button" className={ui.scope === scope ? "on" : ""} onClick={() => (scope === "region" ? ensureRegion() : setUi({ scope }))}>
                            {scope === "view" ? "in view" : scope === "region" ? "region box" : "whole map"}
                        </button>
                    ))}
                </div>
            </div>
            {ui.scope === "region" && (
                <div className="hint">
                    Drag the yellow box's handles: <kbd>G</kbd> move · <kbd>R</kbd> turn · <kbd>S</kbd> resize.{" "}
                    <button type="button" className="icon-button" onClick={() => { scene.setRegion(scene.regionFromView()); scene.select({ kind: "region", id: "region" }); setUi({ region: scene.region }) }}>
                        box around view
                    </button>
                </div>
            )}

            <h3>Remove</h3>
            {TOOLS.map((tool) => (
                <div className="tool-card" key={tool.op}>
                    <div className="name">{tool.name}</div>
                    <div className="about">{tool.about}</div>
                    {tool.params?.map((param) => (
                        <div className="field" key={param.key}>
                            <span>{param.label}</span>
                            <div className="row" style={{ margin: 0 }}>
                                <input type="range" min={param.min} max={param.max} step={param.step} value={params[tool.op][param.key]} onChange={(event) => setParams({ ...params, [tool.op]: { ...params[tool.op], [param.key]: Number(event.target.value) } })} />
                                <span>
                                    {params[tool.op][param.key]}
                                    {param.unit ? ` ${param.unit}` : ""}
                                </span>
                            </div>
                        </div>
                    ))}
                    <div className="row">
                        <button type="button" className="button" onClick={() => preview(tool.op)}>
                            Preview
                        </button>
                        <button type="button" className="button primary" data-op={tool.op} onClick={() => apply(tool.op)}>
                            Remove
                        </button>
                    </div>
                </div>
            ))}
            {previewing && (
                <div className="row">
                    <span className="hint grow">{previewing}</span>
                    <button type="button" className="icon-button" onClick={() => { scene.setPreview(null); setPreviewing(null) }}>
                        clear
                    </button>
                </div>
            )}

            <h3>Crop</h3>
            <div className="tool-card">
                <div className="about">With the region box: keep only what's inside it, or delete what's inside.</div>
                <div className="row">
                    <button type="button" className="button" onClick={ensureRegion}>
                        {scene.region ? "Edit region" : "Draw region"}
                    </button>
                    <button type="button" className="button" disabled={!scene.region} data-op="cropOutside" onClick={() => apply("cropOutside", {}, { kind: "box", ...scene.region! })}>
                        Keep inside
                    </button>
                    <button type="button" className="button danger" disabled={!scene.region} data-op="deleteInside" onClick={() => apply("deleteInside", {}, { kind: "box", ...scene.region! })}>
                        Delete inside
                    </button>
                </div>
                <div className="field">
                    <span>keep heights</span>
                    <div className="row" style={{ margin: 0 }}>
                        <input className="number" type="number" step={0.1} value={heights[0]} onChange={(event) => setHeights([Number(event.target.value), heights[1]])} />
                        <span>to</span>
                        <input className="number" type="number" step={0.1} value={heights[1]} onChange={(event) => setHeights([heights[0], Number(event.target.value)])} />
                        <span>m</span>
                    </div>
                </div>
                <div className="row">
                    <button type="button" className="button" onClick={() => preview("cropHeight", { zMin: heights[0], zMax: heights[1] })}>
                        Preview
                    </button>
                    <button type="button" className="button primary" data-op="cropHeight" onClick={() => apply("cropHeight", { zMin: heights[0], zMax: heights[1] }, { kind: "all" })}>
                        Crop heights
                    </button>
                </div>
            </div>

            <h3>Orient</h3>
            <div className="tool-card">
                <div className="about">Turn the map about its center, or level it so the floor is flat at z = 0. Annotations move with it.</div>
                <div className="row">
                    <button type="button" className="button" onClick={() => run(api.rotate(session.id, -90))}>↺ 90°</button>
                    <button type="button" className="button" onClick={() => run(api.rotate(session.id, 90))}>↻ 90°</button>
                    <input className="number" type="number" step={0.5} value={yaw} onChange={(event) => setYaw(Number(event.target.value))} />
                    <button type="button" className="button" onClick={() => run(api.rotate(session.id, -yaw))}>↺</button>
                    <button type="button" className="button" onClick={() => run(api.rotate(session.id, yaw))}>↻</button>
                </div>
                <div className="row">
                    <button type="button" className="button primary" data-action="level" onClick={() => run(api.level(session.id), "Levelled")}>
                        Level the floor
                    </button>
                </div>
            </div>

            <h3>Look</h3>
            <div className="field">
                <span>voxels as</span>
                <div className="seg">
                    {(["voxel", "disc", "square", "splat"] as const).map((style) => (
                        <button key={style} type="button" className={ui.look.style === style ? "on" : ""} onClick={() => { scene.applyLook({ style }); setUi({ look: { ...ui.look, style } }) }}>
                            {style === "voxel" ? "cubes" : style === "disc" ? "spheres" : style === "square" ? "squares" : "glow"}
                        </button>
                    ))}
                </div>
            </div>
            <div className="field">
                <span>colors</span>
                <select value={ui.look.gradient} onChange={(event) => { scene.applyLook({ gradient: event.target.value }); setUi({ look: { ...ui.look, gradient: event.target.value } }) }}>
                    {GRADIENTS.map((name) => (
                        <option key={name} value={name}>
                            {name}
                        </option>
                    ))}
                </select>
            </div>
            <div className="field">
                <span>paths</span>
                <div className="row" style={{ margin: 0 }}>
                    {(["corrected", "raw", "loops"] as const).map((key) => (
                        <label key={key} className="row" style={{ margin: 0 }}>
                            <input
                                type="checkbox"
                                checked={ui.showPaths[key]}
                                onChange={(event) => {
                                    const showPaths = { ...ui.showPaths, [key]: event.target.checked }
                                    scene.showPaths(showPaths)
                                    setUi({ showPaths })
                                }}
                            />
                            {key === "corrected" ? "loop-closed" : key === "raw" ? "odometry" : "loop links"}
                        </label>
                    ))}
                </div>
            </div>

            <h3>History</h3>
            <div className="history">
                {[...session.history].reverse().map((line, index) => (
                    <span key={index}>{line}</span>
                ))}
            </div>
        </div>
    )
}
