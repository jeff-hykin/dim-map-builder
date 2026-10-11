// The 2D view's slice controls: z-start / z-end (a two-handle range and numbers), over the local floor or absolute,
// "auto" (local floor +0.1 to +1.8 m), and the floor-height overlay. The floor itself is picked bottom right (FloorPicker).
import { Button, Checkbox, Hud, NumberInput, RangeSlider, Text, Toolbar } from "dimos-react"
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
    return (
        <Hud position="top" style={{ maxWidth: "calc(100% - 24px)" }}>
            <Toolbar floating data-slice-bar style={{ padding: "4px 8px", gap: 8 }}>
                <Text muted>z</Text>
                <NumberInput size="sm" step={0.05} value={range.z0} onChange={(z0: number) => set({ z0 })} title="z-start (m)" data-slice="z0" style={{ width: 66 }} />
                <div style={{ width: 170 }} title="drag either handle">
                    <RangeSlider min={low} max={high} step={0.05} value={[range.z0, range.z1]} onChange={([z0, z1]: [number, number]) => set({ z0, z1 })} showValue={false} />
                </div>
                <NumberInput size="sm" step={0.05} value={range.z1} onChange={(z1: number) => set({ z1 })} title="z-end (m)" data-slice="z1" unit="m" style={{ width: 76 }} />
                <Checkbox value={range.follow} onChange={switchFollow} label="over floor" title="heights over the local floor (follows ramps and stairs) instead of absolute z" data-slice-follow />
                <Button size="sm" pressed={isAuto} onClick={() => setUi({ slice: AUTO_RANGE })} title="local floor +0.1 m to +1.8 m" data-slice-auto>
                    auto
                </Button>
                <Button size="sm" pressed={ui.floorOverlay} onClick={() => setUi({ floorOverlay: !ui.floorOverlay })} title="color the local floor by its height" data-floor-overlay>
                    floor heights
                </Button>
            </Toolbar>
        </Hud>
    )
}
