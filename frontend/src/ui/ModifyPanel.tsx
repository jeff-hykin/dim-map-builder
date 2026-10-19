// The paint tools' options: paint on the map in the 2D view, MS-Paint style, and the voxels change in 3D. Erase (columns from the local
// floor up to the slice's z-end; the floor under them is patched from the floor around), draw (voxels up from the
// floor to a height), and straighten a wall (drag along it: the band's voxels become one straight wall). Each stroke
// is one undoable edit.
import { Checkbox, Column, Field, NumberInput, Row, Segmented, Slider, Spinner, Text } from "dimos-react"
import { useStore } from "../core/store.ts"
import type { Context } from "./context.ts"
import { modifyTool } from "./tools.ts"

const ABOUT: Record<string, string> = {
    erase: "Brush away what stands on the floor: a couch, a person, clutter, a lone speck. One stroke takes everything under the brush; the floor stays and nothing is added.",
    brush: "Brush voxels onto the floor, up to the height below.",
    line: "Drag a straight line of voxels (a wall the robot should see).",
    straighten: "Drag along a noisy wall. Inside the band you drag, it becomes one straight slab of the wall's own thickness; specks, fringe and doubled scans there go. Nothing outside the band changes.",
}

function Meters({ label, value, min, max, step, onChange, data }: { label: string; value: number; min: number; max: number; step: number; onChange: (value: number) => void; data: string }) {
    return (
        <Field inline label={label}>
            <Row gap={6}>
                <Slider min={min} max={max} step={step} value={value} showValue={false} onChange={onChange} style={{ width: 110 }} />
                <NumberInput size="sm" min={min} max={max} step={step} unit="m" value={value} onChange={onChange} data-modify={data} style={{ width: 84 }} />
            </Row>
        </Field>
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
        <Column gap={8}>
            <Text small muted>{ABOUT[tool]}</Text>
            {tool === "erase" && (
                <>
                    <Meters label="radius" value={state.radius} min={0.05} max={2} step={0.05} onChange={(radius) => modifyTool.update({ radius })} data="radius" />
                    <Checkbox value={state.fullColumn} onChange={(fullColumn: boolean) => modifyTool.update({ fullColumn })} label="full column" data-modify="fullColumn" />
                    <Text small muted>Erases everything over the local floor up to {reachText}.</Text>
                </>
            )}
            {(tool === "brush" || tool === "line") && (
                <>
                    <Meters label="width" value={state.width} min={0.05} max={1.5} step={0.05} onChange={(width) => modifyTool.update({ width })} data="width" />
                    <Meters label="height" value={state.height} min={0.05} max={3} step={0.05} onChange={(height) => modifyTool.update({ height })} data="height" />
                    <Text small muted>Adds voxels from the local floor up to {state.height.toFixed(2)} m over it.</Text>
                </>
            )}
            {tool === "straighten" && (
                <>
                    <Field inline label="thickness">
                        <Row gap={6}>
                            <Segmented
                                size="sm"
                                value={state.thickness === null ? "auto" : "set"}
                                onChange={(choice: string) => modifyTool.update({ thickness: choice === "auto" ? null : state.thickness ?? 0.1 })}
                                items={[{ value: "auto", label: "auto", props: { "data-thickness": "auto" } }, { value: "set", label: "set", props: { "data-thickness": "set" } }]}
                            />
                            {state.thickness !== null && (
                                <NumberInput size="sm" min={5} step={5} unit="cm" value={Math.round(state.thickness * 100)} onChange={(cm: number) => modifyTool.update({ thickness: Math.max(0.05, cm / 100) })} data-modify="thickness" style={{ width: 84 }} />
                            )}
                        </Row>
                    </Field>
                    <Meters label="band width" value={state.band} min={0.1} max={1.5} step={0.05} onChange={(band) => modifyTool.update({ band })} data="band" />
                    <Text small muted>Your stroke picks the wall; the tool fits the whole wall past it, floor to top, keeps its thickness and corners, and clears stray voxels within half a metre of it. Doorways stay open.</Text>
                </>
            )}
            {state.busy && (
                <Row>
                    <Spinner size={14} /> <Text small muted>applying…</Text>
                </Row>
            )}
            <Text small muted>
                On {floor && floor.storeys.length > 1 ? `floor ${Math.min(ui.planFloor, floor.storeys.length - 1) + 1} of ${floor.storeys.length}` : "the floor"}. One finger or a left drag uses the tool; two fingers or a right-drag pan, pinch or the wheel zooms. Every stroke is one undoable edit (⌘Z).
            </Text>
        </Column>
    )
}
