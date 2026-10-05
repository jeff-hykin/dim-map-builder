// The floor selector, bottom right in every view of a multi-floor map. The floors are the floor model's storeys (the
// ones the 2D view slices and the edit tools work on). In 3D, picking one shows only its voxels (its z band) and "All"
// shows every floor again; the 2D view always shows the picked floor. Stacked like a lift's buttons: the top floor on top.
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
    return (
        <div className="dim-panel glass floor-picker" role="radiogroup" aria-label="floor" data-floor-picker>
            <span className="dim-label">floor</span>
            <div className="dim-tabs seg vertical">
                {storeys
                    .map((storey, index) => ({ storey, index }))
                    .reverse()
                    .map(({ storey, index }) => {
                        const on = picked === index && (!all || ui.floorOnly)
                        return (
                            <button
                                key={index}
                                type="button"
                                role="radio"
                                aria-checked={on}
                                className={`dim-tab ${on ? "on" : ""}`}
                                onClick={() => setUi(all ? { planFloor: index, floorOnly: true } : { planFloor: index })}
                                title={`floor ${index + 1}: ${storey.band[0].toFixed(2)} to ${storey.band[1].toFixed(2)} m (floor at ${storey.level.toFixed(2)} m)`}
                                data-storey={index}
                            >
                                {`F${index + 1}`} <span className="dim">{storey.level.toFixed(1)}</span>
                            </button>
                        )
                    })}
                {all && (
                    <button type="button" role="radio" aria-checked={!ui.floorOnly} className={`dim-tab ${!ui.floorOnly ? "on" : ""}`} onClick={() => setUi({ floorOnly: false })} title="every floor in 3D" data-floor="all">
                        All
                    </button>
                )}
            </div>
        </div>
    )
}
