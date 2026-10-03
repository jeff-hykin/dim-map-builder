// Stage 4: annotate the 3D map: labelled boxes, planes and points, placed by clicking the map, then moved / turned /
// resized with the gizmo. The agent's additions show in blue; everything is editable and undoable.
import { useEffect, useState } from "react"
import * as THREE from "three"
import { api } from "../core/api.ts"
import type { GizmoMode } from "../core/scene.ts"
import type { Context } from "./context.ts"
import { Icon } from "./Icon.tsx"

type Placing = "box" | "plane" | "point" | null

export function AnnotatePanel({ context }: { context: Context }) {
    const { session, scene, run, ui } = context
    const [placing, setPlacing] = useState<Placing>(null)
    const [label, setLabel] = useState("")
    const [mode, setMode] = useState<GizmoMode>("translate")

    useEffect(() => {
        if (!scene || !session) {
            return
        }
        if (!placing) {
            scene.onPick = null
            return
        }
        scene.onPick = async (point) => {
            const name = label.trim() || placing
            let created: { id: string } | undefined
            if (placing === "box") {
                created = await run(api.add(session.id, { type: "box", label: name, box: { center: [point.x, point.y, point.z + 0.3], size: [0.6, 0.6, 0.6], yaw: 0 } }))
            } else if (placing === "point") {
                created = await run(api.add(session.id, { type: "point", label: name, position: [point.x, point.y, point.z] }))
            } else {
                // a vertical plane facing the camera (turn it with the gizmo)
                const toward = scene.viewer.camera.position.clone().sub(point).setZ(0).normalize()
                created = await run(api.add(session.id, { type: "plane", label: name, center: [point.x, point.y, point.z], normal: [toward.x || 1, toward.y, 0], size: [1, 1] }))
            }
            setPlacing(null)
            scene.onPick = null
            if (created) {
                await context.refresh()
                scene.select({ kind: placing, id: created.id })
            }
        }
        return () => {
            scene.onPick = null
        }
    }, [placing, label, scene, session, run, context])

    if (!session || !scene) {
        return null
    }
    const a = session.annotations
    const selected = ui.selected
    const gizmo = (next: GizmoMode) => {
        setMode(next)
        scene.setGizmoMode(next)
    }
    const fitRegion = async () => {
        const region = scene.region ?? scene.regionFromView()
        const fit = await run(api.fitBox(session.id, region))
        if (fit) {
            const created = await run(api.add(session.id, { type: "box", label: label.trim() || "object", box: fit.box }), `Box fitted to ${fit.voxels} voxels`)
            if (created) {
                await context.refresh()
                scene.select({ kind: "box", id: created.id })
            }
        }
    }
    const row = (kind: "box" | "plane" | "point", id: string, name: string, source: string, detail: string) => (
        <li key={id} className={`item ${selected?.id === id ? "on" : ""}`} onClick={() => scene.select({ kind, id })} data-annotation={id}>
            <span className={`swatch ${source === "agent" ? "agent" : ""}`} title={source === "agent" ? "added by the agent" : "added by you"} />
            <input
                className="dim-input text"
                defaultValue={name}
                key={name}
                onClick={(event) => event.stopPropagation()}
                onBlur={(event) => event.target.value !== name && run(api.patch(session.id, id, { label: event.target.value }))}
                onKeyDown={(event) => event.key === "Enter" && (event.target as HTMLInputElement).blur()}
            />
            <span className="row" style={{ margin: 0 }}>
                <span className="kind">{detail}</span>
                <button
                    type="button"
                    className="dim-btn sm icon"
                    title="Delete (undoable)"
                    onClick={(event) => {
                        event.stopPropagation()
                        if (selected?.id === id) {
                            scene.select(null)
                        }
                        run(api.remove(session.id, id), "Deleted (⌘Z to undo)")
                    }}
                >
                    <Icon name="close" />
                </button>
            </span>
        </li>
    )
    const fmt = (v: number) => v.toFixed(2)
    return (
        <div>
            <div className="panel-head">
                <h2 className="dim-h2">Annotate</h2>
                <p>Mark things in 3D. Click a tool, then click the map. Select one to move <kbd>G</kbd>, turn <kbd>R</kbd> or resize <kbd>S</kbd> it; <kbd>Del</kbd> removes it.</p>
            </div>
            <div className="field">
                <span>label</span>
                <input className="dim-input text" placeholder="e.g. chair, door, charger" value={label} onChange={(event) => setLabel(event.target.value)} />
            </div>
            <div className="row">
                {(["box", "plane", "point"] as const).map((kind) => (
                    <button key={kind} type="button" className={`dim-btn sm ${placing === kind ? "on" : ""}`} data-place={kind} onClick={() => setPlacing(placing === kind ? null : kind)}>
                        + {kind}
                    </button>
                ))}
                <button type="button" className="dim-btn sm" title="A box hugging what's in the region box (or the view)" onClick={fitRegion}>
                    fit box
                </button>
            </div>
            {placing && <div className="hint">Click the map where the {placing} goes (Esc cancels).</div>}
            <div className="row">
                <span className="hint">gizmo</span>
                <div className="dim-tabs seg">
                    {(["translate", "rotate", "scale"] as const).map((next) => (
                        <button key={next} type="button" className={`dim-tab ${mode === next ? "on" : ""}`} onClick={() => gizmo(next)}>
                            {next === "translate" ? "move G" : next === "rotate" ? "turn R" : "size S"}
                        </button>
                    ))}
                </div>
            </div>

            <h3 className="dim-label">Boxes ({a.boxes.length})</h3>
            <ul className="items">{a.boxes.map((b) => row("box", b.id, b.label, b.source, `${fmt(b.box.size[0])}×${fmt(b.box.size[1])}×${fmt(b.box.size[2])} m`))}</ul>
            <h3 className="dim-label">Planes ({a.planes.length})</h3>
            <ul className="items">{a.planes.map((p) => row("plane", p.id, p.label, p.source, `${fmt(p.size[0])}×${fmt(p.size[1])} m`))}</ul>
            <h3 className="dim-label">Points ({a.points.length})</h3>
            <ul className="items">{a.points.map((p) => row("point", p.id, p.label, p.source, p.position.map(fmt).join(", ")))}</ul>
            {selected && selected.kind !== "region" && <SelectedDetails context={context} />}
        </div>
    )
}

/** exact numbers for the selected annotation */
function SelectedDetails({ context }: { context: Context }) {
    const { session, ui, run } = context
    const box = session?.annotations.boxes.find((b) => b.id === ui.selected?.id)
    if (!session || !box) {
        return null
    }
    const set = (key: "center" | "size", axis: number, value: number) => {
        const next = { ...box.box, [key]: box.box[key].map((v, i) => (i === axis ? value : v)) }
        run(api.patch(session.id, box.id, { box: next }))
    }
    return (
        <div className="dim-panel tool-card">
            <div className="name">{box.label}</div>
            {(["center", "size"] as const).map((key) => (
                <div className="field" key={key}>
                    <span>{key}</span>
                    <div className="row" style={{ margin: 0 }}>
                        {box.box[key].map((value, axis) => (
                            <input key={`${box.id}${key}${axis}${value}`} className="dim-input number" type="number" step={0.05} defaultValue={value.toFixed(2)} onBlur={(event) => Number(event.target.value) !== value && set(key, axis, Number(event.target.value))} />
                        ))}
                    </div>
                </div>
            ))}
            <div className="field">
                <span>yaw</span>
                <span>{THREE.MathUtils.radToDeg(box.box.yaw).toFixed(1)}°</span>
            </div>
        </div>
    )
}
