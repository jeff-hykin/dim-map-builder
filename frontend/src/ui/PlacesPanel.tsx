// Named points and areas on a storey, placed in the 2D view: a dock, a door, a kitchen, a no-go zone around the stairs.
import { api, type Area } from "../core/api.ts"
import { useStore } from "../core/store.ts"
import type { Context } from "./context.ts"
import { planTool } from "./tools.ts"
import { Icon } from "./Icon.tsx"

export function PlacesPanel({ context }: { context: Context }) {
    const { session, run, ui, floor } = context
    const tool = useStore(planTool)
    if (!session) {
        return null
    }
    const storey = Math.min(ui.planFloor, Math.max(0, (floor?.storeys.length ?? 1) - 1))
    const points = session.annotations.planPoints.filter((p) => p.floor === storey)
    const areas = session.annotations.areas.filter((a) => a.floor === storey)
    return (
        <div>
            <div className="field">
                <span>name</span>
                <input className="dim-input text" placeholder="e.g. Dock, Kitchen, Stairs" value={tool.name} onChange={(event) => planTool.update({ name: event.target.value })} data-place-name />
            </div>
            <div className="row">
                <div className="dim-tabs seg">
                    {(["point", "area"] as const).map((next) => (
                        <button key={next} type="button" className={`dim-tab ${tool.tool === next ? "on" : ""}`} data-plan-tool={next} onClick={() => planTool.update({ tool: next, draft: [] })}>
                            {next === "point" ? "named point" : "area"}
                        </button>
                    ))}
                </div>
                {tool.tool === "area" && (
                    <select className="dim-select" value={tool.kind} onChange={(event) => planTool.update({ kind: event.target.value })}>
                        <option value="no-go">no-go zone</option>
                        <option value="zone">named zone</option>
                        <option value="slow">slow zone</option>
                    </select>
                )}
            </div>
            <div className="hint">{tool.tool === "point" ? "Click the 2D view to place it." : "Click corners; Enter or double-click closes the area, Esc cancels, Backspace drops the last corner."}</div>
            <h3 className="dim-label">Named points ({points.length})</h3>
            <ul className="items">
                {points.map((p) => (
                    <li key={p.id} className="item" data-plan-point={p.id}>
                        <span className="swatch" />
                        <input className="dim-input text" defaultValue={p.name} key={p.name} onBlur={(event) => event.target.value !== p.name && run(api.patch(session.id, p.id, { name: event.target.value }))} />
                        <span className="row" style={{ margin: 0 }}>
                            <span className="kind">{p.position.map((v) => v.toFixed(1)).join(", ")}</span>
                            <button type="button" className="dim-btn sm icon" onClick={() => run(api.remove(session.id, p.id), "Deleted (⌘Z to undo)")}>
                                <Icon name="close" />
                            </button>
                        </span>
                    </li>
                ))}
            </ul>
            <h3 className="dim-label">Areas ({areas.length})</h3>
            <ul className="items">
                {areas.map((a: Area) => (
                    <li key={a.id} className="item" data-area={a.id}>
                        <span className={`swatch ${a.kind === "no-go" ? "no-go" : ""}`} />
                        <input className="dim-input text" defaultValue={a.name} key={a.name} onBlur={(event) => event.target.value !== a.name && run(api.patch(session.id, a.id, { name: event.target.value }))} />
                        <span className="row" style={{ margin: 0 }}>
                            <select className="dim-select" value={a.kind} onChange={(event) => run(api.patch(session.id, a.id, { kind: event.target.value }))}>
                                <option value="no-go">no-go</option>
                                <option value="zone">zone</option>
                                <option value="slow">slow</option>
                            </select>
                            <button type="button" className="dim-btn sm icon" onClick={() => run(api.remove(session.id, a.id), "Deleted (⌘Z to undo)")}>
                                <Icon name="close" />
                            </button>
                        </span>
                    </li>
                ))}
            </ul>
        </div>
    )
}
