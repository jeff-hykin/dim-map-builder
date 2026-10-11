// Saved 2D views: a 2D map is a perspective on the 3D map (a storey, a height band, where it looks), saved by name and
// switched between. Occupancy grids for navigation can still be exported alongside.
import { useState } from "react"
import { api, type SavedView } from "../core/api.ts"
import type { Context } from "./context.ts"
import { APPLY_2D } from "./View2D.tsx"
import { Button, Column, Heading, List, Row, Text, TextInput } from "dimos-react"
import { AnnotationRow } from "./rows.tsx"

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
        <Column gap={8}>
            <Text small muted>A 2D map is a saved view of the 3D map: a storey, a z-start / z-end band and where it looks. They're saved into the recording with the map.</Text>
            <Row>
                <TextInput size="sm" placeholder="name, e.g. Floor 2 walls" value={name} onChange={setName} data-view-name style={{ flex: 1 }} />
                <Button size="sm" variant="primary" onClick={() => saveCurrentView(context, name.trim())?.then(() => setName(""))} data-action="save-view">
                    Save current
                </Button>
            </Row>
            <List>
                {views.map((view) => (
                    <AnnotationRow
                        key={view.id}
                        name={view.name}
                        onRename={(next) => run(api.patch(session.id, view.id, { name: next }))}
                        detail={`F${view.floor + 1} · ${view.zMin.toFixed(1)}…${view.zMax.toFixed(1)} m${view.follow ? " over floor" : ""}`}
                        onClick={() => applyView(context, view)}
                        onDelete={() => run(api.remove(session.id, view.id), "Deleted (⌘Z to undo)")}
                        data-saved-view={view.id}
                    />
                ))}
            </List>
            <Heading level={3}>Export for navigation</Heading>
            <Text small muted>Occupancy grids (nav_msgs/OccupancyGrid, one per storey) written with the map on Save. Optional: the views above are the 2D maps.</Text>
            <Row>
                <Button size="sm" onClick={() => run(api.plans(session.id, true), (r) => `${r.floors.length} occupancy grid${r.floors.length === 1 ? "" : "s"} ready (written on Save)`)} data-action="export-grids">
                    {session.plans.length ? `Re-export ${session.plans.length} grid${session.plans.length === 1 ? "" : "s"}` : "Export occupancy grids"}
                </Button>
            </Row>
        </Column>
    )
}
