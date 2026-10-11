// The floor selector, bottom right in every view of a multi-floor map. The floors are the floor model's storeys (the
// ones the 2D view slices and the edit tools work on). In 3D, picking one shows only its voxels (its z band) and "All"
// shows every floor again; the 2D view always shows the picked floor. Stacked like a lift's buttons: the top floor on top.
import { Card, Hud, Label, Segmented } from "dimos-react"
import type { Context, ViewMode } from "./context.ts"

export function FloorPicker({ context, mode }: { context: Context; mode: ViewMode }) {
    const { ui, setUi, floor } = context
    const storeys = floor?.storeys ?? []
    if (storeys.length < 2) {
        return null
    }
    const picked = Math.min(ui.planFloor, storeys.length - 1)
    // the 2D view always shows one floor: "All" is a 3D choice
    const all = mode !== "2d"
    const items = storeys
        .map((storey, index) => ({
            value: index,
            label: `F${index + 1} · ${storey.level.toFixed(1)}`,
            title: `floor ${index + 1}: ${storey.band[0].toFixed(2)} to ${storey.band[1].toFixed(2)} m (floor at ${storey.level.toFixed(2)} m)`,
            props: { "data-storey": index },
        }))
        .reverse()
    if (all) {
        items.push({ value: -1, label: "All", title: "every floor in 3D", props: { "data-floor": "all" } as never })
    }
    const value = all && !ui.floorOnly ? -1 : picked
    return (
        <Hud position="bottom-right" style={mode === "3d" ? undefined : { right: 0 }}>
            <Card glass padded={false} style={{ padding: 6, gap: 4 }} data-floor-picker>
                <Label style={{ textAlign: "center" }}>floor</Label>
                <Segmented
                    vertical
                    size="sm"
                    label="floor"
                    items={items}
                    value={value}
                    onChange={(index: number) => setUi(index === -1 ? { floorOnly: false } : all ? { planFloor: index, floorOnly: true } : { planFloor: index })}
                />
            </Card>
        </Hud>
    )
}
