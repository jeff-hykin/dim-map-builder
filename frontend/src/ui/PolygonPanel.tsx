// Polygons: an area drawn in 2D (click corners; Enter or double-click closes it) that stands up in 3D as a prism from
// the local floor, 1 m tall unless you say otherwise. Select one in either view; drag its corners in 2D or its top
// handle in 3D; Del removes it.
import { api, type PrismAnnotation } from "../core/api.ts"
import { useStore } from "../core/store.ts"
import type { Context } from "./context.ts"
import { polygonTool } from "./tools.ts"

export function PolygonPanel({ context }: { context: Context }) {
    const { session, scene, run, ui, floor } = context
    const tool = useStore(polygonTool)
    if (!session || !scene) {
        return null
    }
    const storey = Math.min(ui.planFloor, Math.max(0, (floor?.storeys.length ?? 1) - 1))
    const prisms = (session.annotations.prisms ?? []).filter((p) => p.floor === storey)
    const number = (prism: PrismAnnotation, key: "height" | "base") => (
        <input
            key={`${prism.id}${key}${prism[key]}`}
            className="number"
            type="number"
            step={0.05}
            defaultValue={prism[key].toFixed(2)}
            title={key === "height" ? "height over its base (m)" : "base z (m)"}
            onClick={(event) => event.stopPropagation()}
            onBlur={(event) => Number(event.target.value) !== prism[key] && run(api.patch(session.id, prism.id, { [key]: Number(event.target.value) }))}
            onKeyDown={(event) => event.key === "Enter" && (event.target as HTMLInputElement).blur()}
            data-prism-field={key}
        />
    )
    return (
        <div>
            <div className="field">
                <span>label</span>
                <input className="text" placeholder="e.g. couch, desk, lab bench" value={tool.label} onChange={(event) => polygonTool.update({ label: event.target.value })} data-polygon-label />
            </div>
            <div className="field">
                <span>height</span>
                <span className="row" style={{ margin: 0 }}>
                    <input className="number" type="number" min={0.05} step={0.05} value={tool.height} onChange={(event) => event.target.value !== "" && polygonTool.update({ height: Number(event.target.value) })} data-polygon-height />
                    <span className="dim">m over the local floor</span>
                </span>
            </div>
            <div className="hint">Click corners in the 2D view; Enter or double-click closes it, Backspace drops the last corner, Esc cancels.</div>
            <h3>Polygons ({prisms.length})</h3>
            <ul className="items">
                {prisms.map((p) => (
                    <li key={p.id} className={`item stacked ${ui.selected?.id === p.id ? "on" : ""}`} onClick={() => scene.select({ kind: "prism", id: p.id })} data-prism={p.id}>
                        <span className={`swatch ${p.source === "agent" ? "agent" : ""}`} />
                        <input
                            className="text"
                            defaultValue={p.label}
                            key={p.label}
                            onClick={(event) => event.stopPropagation()}
                            onBlur={(event) => event.target.value !== p.label && run(api.patch(session.id, p.id, { label: event.target.value }))}
                        />
                        <span className="row" style={{ margin: 0 }}>
                            <span className="kind">h</span>
                            {number(p, "height")}
                            <span className="kind">base</span>
                            {number(p, "base")}
                            <button
                                type="button"
                                className="icon-button"
                                onClick={(event) => {
                                    event.stopPropagation()
                                    run(api.remove(session.id, p.id), "Deleted (⌘Z to undo)")
                                }}
                            >
                                ✕
                            </button>
                        </span>
                    </li>
                ))}
            </ul>
        </div>
    )
}
