// Saved 2D views: a 2D map is a perspective on the 3D map (a storey, a height band, where it looks), saved by name and
// switched between. Occupancy grids for navigation can still be exported alongside.
import { useState } from "react"
import { api, type SavedView } from "../core/api.ts"
import type { Context } from "./context.ts"
import { APPLY_2D } from "./View2D.tsx"
import { Icon } from "./Icon.tsx"

/** puts a saved view on the 2D view */
export function applyView(context: Context, view: SavedView) {
    context.setUi({
        mode: context.ui.mode === "3d" ? "2d" : context.ui.mode,
        planFloor: view.floor,
        slice: { follow: view.follow, z0: view.zMin, z1: view.zMax },
        ...(view.center && view.pixelsPerMeter ? { view2d: { cx: view.center[0], cy: view.center[1], pixelsPerMeter: view.pixelsPerMeter } } : {}),
    })
    if (view.center) {
        window.dispatchEvent(new CustomEvent(APPLY_2D, { detail: view }))
    }
}

export function saveCurrentView(context: Context, name: string) {
    const { session, ui, run } = context
    if (!session) {
        return
    }
    const view = ui.view2d
    return run(
        api.add(session.id, {
            type: "view",
            name: name || `Floor ${ui.planFloor + 1} ${ui.slice.z0.toFixed(1)}–${ui.slice.z1.toFixed(1)} m`,
            floor: ui.planFloor,
            follow: ui.slice.follow,
            zMin: ui.slice.z0,
            zMax: ui.slice.z1,
            center: view ? [view.cx, view.cy] : null,
            pixelsPerMeter: view?.pixelsPerMeter ?? null,
        }),
        "View saved",
    )
}

export function ViewsPanel({ context }: { context: Context }) {
    const { session, run } = context
    const [name, setName] = useState("")
    if (!session) {
        return null
    }
    const views = session.annotations.views ?? []
    return (
        <div>
            <div className="hint">A 2D map is a saved view of the 3D map: a storey, a z-start / z-end band and where it looks. They're saved into the recording with the map.</div>
            <div className="row">
                <input className="dim-input text grow" placeholder="name, e.g. Floor 2 walls" value={name} onChange={(event) => setName(event.target.value)} data-view-name />
                <button type="button" className="dim-btn sm primary" onClick={() => saveCurrentView(context, name.trim())?.then(() => setName(""))} data-action="save-view">
                    Save current
                </button>
            </div>
            <ul className="items">
                {views.map((view) => (
                    <li key={view.id} className="item" onClick={() => applyView(context, view)} data-saved-view={view.id}>
                        <span className="swatch" />
                        <input
                            className="dim-input text"
                            defaultValue={view.name}
                            key={view.name}
                            onClick={(event) => event.stopPropagation()}
                            onBlur={(event) => event.target.value !== view.name && run(api.patch(session.id, view.id, { name: event.target.value }))}
                        />
                        <span className="row" style={{ margin: 0 }}>
                            <span className="kind">
                                F{view.floor + 1} · {view.zMin.toFixed(1)}…{view.zMax.toFixed(1)} m{view.follow ? " over floor" : ""}
                            </span>
                            <button
                                type="button"
                                className="dim-btn sm icon"
                                onClick={(event) => {
                                    event.stopPropagation()
                                    run(api.remove(session.id, view.id), "Deleted (⌘Z to undo)")
                                }}
                            >
                                <Icon name="close" />
                            </button>
                        </span>
                    </li>
                ))}
            </ul>
            <h3 className="dim-label">Export for navigation</h3>
            <div className="hint">Occupancy grids (nav_msgs/OccupancyGrid, one per storey) written with the map on Save. Optional: the views above are the 2D maps.</div>
            <div className="row">
                <button type="button" className="dim-btn sm" onClick={() => run(api.plans(session.id, true), (r) => `${r.floors.length} occupancy grid${r.floors.length === 1 ? "" : "s"} ready (written on Save)`)} data-action="export-grids">
                    {session.plans.length ? `Re-export ${session.plans.length} grid${session.plans.length === 1 ? "" : "s"}` : "Export occupancy grids"}
                </button>
            </div>
        </div>
    )
}
