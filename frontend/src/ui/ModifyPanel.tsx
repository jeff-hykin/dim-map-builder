// The paint tools' options: paint on the map in the 2D view, MS-Paint style, and the voxels change in 3D. Erase (columns from the local
// floor up to the slice's z-end; the floor under them is patched from the floor around), draw (voxels up from the
// floor to a height), and straighten a wall (drag along it: the band's voxels become one straight wall). Each stroke
// is one undoable edit.
import { useStore } from "../core/store.ts"
import type { Context } from "./context.ts"
import { modifyTool } from "./tools.ts"

const ABOUT: Record<string, string> = {
    erase: "Brush away what stands on the floor: a couch, a person, clutter. The floor under it is filled in from the floor around.",
    brush: "Brush voxels onto the floor, up to the height below.",
    line: "Drag a straight line of voxels (a wall the robot should see).",
    straighten: "Drag along a noisy wall: what's in the band becomes one straight wall on the line fitted through it.",
}

function Number_({ label, value, min, max, step, unit, onChange, data }: { label: string; value: number; min: number; max: number; step: number; unit: string; onChange: (value: number) => void; data: string }) {
    return (
        <div className="field">
            <span>{label}</span>
            <span className="row" style={{ margin: 0 }}>
                <input type="range" min={min} max={max} step={step} value={value} onChange={(event) => onChange(Number(event.target.value))} />
                <input className="number" type="number" min={min} max={max} step={step} value={value} onChange={(event) => event.target.value !== "" && onChange(Number(event.target.value))} data-modify={data} />
                <span className="dim">{unit}</span>
            </span>
        </div>
    )
}

export function ModifyPanel({ context }: { context: Context }) {
    const { session, ui, floor } = context
    const state = useStore(modifyTool)
    if (!session) {
        return null
    }
    const tool = ui.tool
    const range = ui.slice
    const reachText = state.fullColumn ? "the whole column (up to the next storey)" : range.follow ? `${range.z1.toFixed(2)} m over the floor (the slice's z-end)` : `z ${range.z1.toFixed(2)} m (the slice's z-end)`
    return (
        <div>
            <div className="hint">{ABOUT[tool]}</div>
            {tool === "erase" && (
                <>
                    <Number_ label="radius" value={state.radius} min={0.05} max={2} step={0.05} unit="m" onChange={(radius) => modifyTool.update({ radius })} data="radius" />
                    <label className="row">
                        <input type="checkbox" checked={state.fullColumn} onChange={(event) => modifyTool.update({ fullColumn: event.target.checked })} data-modify="fullColumn" />
                        full column
                    </label>
                    <div className="hint">Erases from one voxel over the local floor up to {reachText}.</div>
                </>
            )}
            {(tool === "brush" || tool === "line") && (
                <>
                    <Number_ label="width" value={state.width} min={0.05} max={1.5} step={0.05} unit="m" onChange={(width) => modifyTool.update({ width })} data="width" />
                    <Number_ label="height" value={state.height} min={0.05} max={3} step={0.05} unit="m" onChange={(height) => modifyTool.update({ height })} data="height" />
                    <div className="hint">Adds voxels from the local floor up to {state.height.toFixed(2)} m over it.</div>
                </>
            )}
            {tool === "straighten" && (
                <>
                    <Number_ label="band width" value={state.band} min={0.1} max={1.5} step={0.05} unit="m" onChange={(band) => modifyTool.update({ band })} data="band" />
                    <label className="row">
                        <input type="checkbox" checked={state.fullColumn} onChange={(event) => modifyTool.update({ fullColumn: event.target.checked })} />
                        full column
                    </label>
                    <div className="hint">Wall voxels within {(state.band / 2).toFixed(2)} m of the line, up to {reachText}, become a straight wall (it keeps its height and its doorways).</div>
                </>
            )}
            {state.busy && <div className="hint">applying…</div>}
            <div className="hint">
                On {floor && floor.storeys.length > 1 ? `floor ${Math.min(ui.planFloor, floor.storeys.length - 1) + 1} of ${floor.storeys.length}` : "the floor"}. Right-drag pans. Every stroke is one undoable edit (⌘Z).
            </div>
        </div>
    )
}
