// The 2D view's slice controls: z-start / z-end (a two-handle range and numbers), over the local floor or absolute,
// "auto" (local floor +0.1 to +1.8 m), and the floor-height overlay. The floor itself is picked bottom right (FloorPicker).
import { AUTO_RANGE, type SliceRange } from "../core/slice.ts"
import type { Context } from "./context.ts"

export function SliceBar({ context }: { context: Context }) {
    const { ui, setUi, floor } = context
    const range = ui.slice
    const levels = floor?.storeys.map((s) => s.level) ?? []
    const [low, high] = range.follow ? [-0.5, 3] : [Math.floor((levels[0] ?? 0) - 1), Math.ceil((levels[levels.length - 1] ?? 0) + 3)]
    const set = (patch: Partial<SliceRange>) => {
        const next = { ...range, ...patch }
        if (next.z0 > next.z1) {
            ;[next.z0, next.z1] = patch.z0 !== undefined ? [next.z0, next.z0] : [next.z1, next.z1]
        }
        setUi({ slice: next })
    }
    const storey = Math.min(ui.planFloor, Math.max(0, levels.length - 1))
    const isAuto = range.follow && range.z0 === AUTO_RANGE.z0 && range.z1 === AUTO_RANGE.z1
    const base = range.follow ? 0 : levels[storey] ?? 0
    const switchFollow = (follow: boolean) => setUi({ slice: follow ? { follow, z0: range.z0 - base, z1: range.z1 - base } : { follow, z0: +(range.z0 + (levels[storey] ?? 0)).toFixed(2), z1: +(range.z1 + (levels[storey] ?? 0)).toFixed(2) } })
    const percent = (value: number) => `${((value - low) / (high - low)) * 100}%`
    return (
        <div className="dim-panel glass slice-bar" data-slice-bar>
            <span className="dim">z</span>
            <input className="dim-input number" type="number" step={0.05} value={range.z0} data-slice="z0" onChange={(event) => event.target.value !== "" && set({ z0: Number(event.target.value) })} title="z-start (m)" />
            <div className="dual-range" title="drag either handle">
                <div className="track" />
                <div className="fill" style={{ left: percent(Math.max(low, range.z0)), right: `calc(100% - ${percent(Math.min(high, range.z1))})` }} />
                <input type="range" min={low} max={high} step={0.05} value={range.z0} onChange={(event) => set({ z0: Number(event.target.value) })} data-slice-handle="z0" />
                <input type="range" min={low} max={high} step={0.05} value={range.z1} onChange={(event) => set({ z1: Number(event.target.value) })} data-slice-handle="z1" />
            </div>
            <input className="dim-input number" type="number" step={0.05} value={range.z1} data-slice="z1" onChange={(event) => event.target.value !== "" && set({ z1: Number(event.target.value) })} title="z-end (m)" />
            <span className="dim">m</span>
            <label className="dim-check" title="heights over the local floor (follows ramps and stairs) instead of absolute z">
                <input type="checkbox" checked={range.follow} onChange={(event) => switchFollow(event.target.checked)} data-slice-follow />
                <span className="box" />
                over floor
            </label>
            <button type="button" className={`dim-btn sm icon ${isAuto ? "on" : ""}`} onClick={() => setUi({ slice: AUTO_RANGE })} title="local floor +0.1 m to +1.8 m" data-slice-auto>
                auto
            </button>
            <button type="button" className={`dim-btn sm icon ${ui.floorOverlay ? "on" : ""}`} onClick={() => setUi({ floorOverlay: !ui.floorOverlay })} title="color the local floor by its height" data-floor-overlay>
                floor heights
            </button>
        </div>
    )
}
