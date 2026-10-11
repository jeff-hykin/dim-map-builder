// Stage 3: clean the map. Every tool acts on a scope (what's in view, the yellow region box, or the whole map), can be
// previewed (the voxels it would remove turn red) and is undoable.
import { useState } from "react"
import { api, type Region } from "../core/api.ts"
import { Button, Card, Checkbox, Column, DEFAULT_PERIOD, Field, GRADIENTS, Heading, isPalette, Kbd, NumberInput, PALETTE_NAMES, PALETTES, Row, Segmented, Select, Slider, Text } from "dimos-react"
import { CUBE_SHADES } from "dimos-react/three"
import type { CubeShade } from "../core/scene.ts"
import type { Context } from "./context.ts"

interface Tool {
    op: string
    name: string
    about: string
    params?: { key: string; label: string; min: number; max: number; step: number; value: number; unit?: string }[]
}

const TOOLS: Tool[] = [
    { op: "floating", name: "Floating clusters", about: "Small groups of voxels touching nothing: dust, specks, a person walking past.", params: [{ key: "minVoxels", label: "smaller than", min: 3, max: 400, step: 1, value: 30, unit: "voxels" }] },
    { op: "outliers", name: "Outliers", about: "Voxels far from their neighbours (statistical).", params: [{ key: "stdRatio", label: "σ above mean", min: 0.5, max: 4, step: 0.1, value: 2 }] },
    { op: "floor", name: "Floor", about: "The floor surface of every storey (keeps what stands on it).", params: [{ key: "thickness", label: "thickness", min: 0.02, max: 0.3, step: 0.01, value: 0.08, unit: "m" }] },
    { op: "walls", name: "Walls", about: "Vertical surfaces above the floor.", params: [{ key: "minHeight", label: "above floor", min: 0, max: 2, step: 0.05, value: 0.3, unit: "m" }] },
]

export function CleanPanel({ context }: { context: Context }) {
    const { session, scene, ui, setUi, run } = context
    const [params, setParams] = useState<Record<string, Record<string, number>>>(() => Object.fromEntries(TOOLS.map((t) => [t.op, Object.fromEntries((t.params ?? []).map((p) => [p.key, p.value]))])))
    const [previewing, setPreviewing] = useState<string | null>(null)
    const [heights, setHeights] = useState<[number, number]>([-0.5, 2.5])
    const [yaw, setYaw] = useState(5)
    if (!session || !scene) {
        return null
    }
    const region = (): Region => {
        if (ui.scope === "region" && scene.region) {
            return { kind: "box", ...scene.region }
        }
        if (ui.scope === "view") {
            return { kind: "view", matrix: scene.viewState().viewProjection as number[] }
        }
        return { kind: "all" }
    }
    const preview = async (op: string, extra: Record<string, unknown> = {}) => {
        const result = await run(api.op(session.id, op, region(), { ...params[op], ...extra }, true))
        if (result) {
            scene.setPreview(result.preview ?? null)
            setPreviewing(`${result.label}: ${result.changed.toLocaleString()} voxels`)
        }
    }
    const apply = async (op: string, extra: Record<string, unknown> = {}, scope?: Region) => {
        scene.setPreview(null)
        setPreviewing(null)
        await run(api.op(session.id, op, scope ?? region(), { ...params[op], ...extra }), (r) => `${r.label}: removed ${r.changed.toLocaleString()} voxels (⌘Z undoes)`)
    }
    const ensureRegion = () => {
        if (!scene.region) {
            scene.setRegion(scene.regionFromView())
        }
        scene.select({ kind: "region", id: "region" })
        setUi({ scope: "region", region: scene.region })
    }
    const look = (patch: Partial<typeof ui.look>) => {
        scene.applyLook(patch)
        setUi({ look: { ...ui.look, ...patch } })
    }
    const boxAroundView = () => {
        scene.setRegion(scene.regionFromView())
        scene.select({ kind: "region", id: "region" })
        setUi({ region: scene.region })
    }
    return (
        <Column gap={8}>
            <Text small muted>{session.voxels.toLocaleString()} voxels. Preview shows what a tool would remove in red; every change can be undone (⌘Z).</Text>

            <Heading level={3}>Scope</Heading>
            <Segmented
                size="sm"
                value={ui.scope}
                onChange={(scope: "view" | "region" | "all") => (scope === "region" ? ensureRegion() : setUi({ scope }))}
                items={[["view", "in view"], ["region", "region box"], ["all", "whole map"]]}
            />
            {ui.scope === "region" && (
                <Row wrap>
                    <Text small muted>
                        Drag the yellow box's handles: <Kbd>G</Kbd> move · <Kbd>R</Kbd> turn · <Kbd>S</Kbd> resize.
                    </Text>
                    <Button size="sm" onClick={boxAroundView}>box around view</Button>
                </Row>
            )}

            <Heading level={3}>Remove</Heading>
            {TOOLS.map((tool) => (
                <Card key={tool.op} title={tool.name} description={tool.about}>
                    {tool.params?.map((param) => (
                        <Field inline label={param.label} key={param.key}>
                            <Slider
                                min={param.min}
                                max={param.max}
                                step={param.step}
                                value={params[tool.op][param.key]}
                                format={(value: number) => `${value}${param.unit ? ` ${param.unit}` : ""}`}
                                onChange={(value: number) => setParams({ ...params, [tool.op]: { ...params[tool.op], [param.key]: value } })}
                            />
                        </Field>
                    ))}
                    <Row>
                        <Button size="sm" data-preview={tool.op} onClick={() => preview(tool.op)}>
                            Preview
                        </Button>
                        <Button size="sm" variant="primary" data-op={tool.op} onClick={() => apply(tool.op)}>
                            Remove
                        </Button>
                    </Row>
                </Card>
            ))}
            {previewing && (
                <Row>
                    <Text small muted style={{ flex: 1 }}>{previewing}</Text>
                    <Button size="sm" variant="ghost" onClick={() => { scene.setPreview(null); setPreviewing(null) }}>
                        clear
                    </Button>
                </Row>
            )}

            <Heading level={3}>Crop</Heading>
            <Card description="With the region box: keep only what's inside it, or delete what's inside.">
                <Row wrap>
                    <Button size="sm" onClick={ensureRegion}>
                        {scene.region ? "Edit region" : "Draw region"}
                    </Button>
                    <Button size="sm" disabled={!scene.region} data-op="cropOutside" onClick={() => apply("cropOutside", {}, { kind: "box", ...scene.region! })}>
                        Keep inside
                    </Button>
                    <Button size="sm" variant="danger" disabled={!scene.region} data-op="deleteInside" onClick={() => apply("deleteInside", {}, { kind: "box", ...scene.region! })}>
                        Delete inside
                    </Button>
                </Row>
                <Field inline label="keep heights">
                    <Row gap={4}>
                        <NumberInput size="sm" step={0.1} value={heights[0]} onChange={(low: number) => setHeights([low, heights[1]])} style={{ width: 70 }} />
                        <Text small muted>to</Text>
                        <NumberInput size="sm" step={0.1} unit="m" value={heights[1]} onChange={(high: number) => setHeights([heights[0], high])} style={{ width: 80 }} />
                    </Row>
                </Field>
                <Row>
                    <Button size="sm" onClick={() => preview("cropHeight", { zMin: heights[0], zMax: heights[1] })}>
                        Preview
                    </Button>
                    <Button size="sm" variant="primary" data-op="cropHeight" onClick={() => apply("cropHeight", { zMin: heights[0], zMax: heights[1] }, { kind: "all" })}>
                        Crop heights
                    </Button>
                </Row>
            </Card>

            <Heading level={3}>Orient</Heading>
            <Card description="Turn the map about its center, or level it so the floor is flat at z = 0. Annotations move with it.">
                <Row wrap gap={4}>
                    <Button size="sm" icon="rotate-left" onClick={() => run(api.rotate(session.id, -90))}>90°</Button>
                    <Button size="sm" icon="rotate-right" onClick={() => run(api.rotate(session.id, 90))}>90°</Button>
                    <NumberInput size="sm" step={0.5} unit="°" value={yaw} onChange={setYaw} style={{ width: 76 }} />
                    <Button size="sm" icon="rotate-left" aria-label={`turn ${yaw}° clockwise`} onClick={() => run(api.rotate(session.id, -yaw))} />
                    <Button size="sm" icon="rotate-right" aria-label={`turn ${yaw}° counter-clockwise`} onClick={() => run(api.rotate(session.id, yaw))} />
                </Row>
                <Row>
                    <Button size="sm" variant="primary" data-action="level" onClick={() => run(api.level(session.id), "Levelled")}>
                        Level the floor
                    </Button>
                </Row>
            </Card>

            <Heading level={3}>Look</Heading>
            <Field inline label="voxels as">
                <Segmented
                    size="sm"
                    value={ui.look.style}
                    onChange={(style: "voxel" | "disc" | "square" | "splat") => look({ style })}
                    items={[["voxel", "cubes"], ["disc", "spheres"], ["square", "squares"], ["splat", "glow"]]}
                />
            </Field>
            {ui.look.style === "voxel" && (
                <Field inline label="cube shading">
                    <Select
                        size="sm"
                        value={ui.look.shade ?? "soft"}
                        onChange={(shade: CubeShade) => look({ shade })}
                        options={Object.entries(CUBE_SHADES as Record<string, { label: string }>).map(([id, shade]) => [id, shade.label])}
                        data-cube-shade
                    />
                </Field>
            )}
            <Field inline label="colors">
                <Select
                    size="sm"
                    value={ui.look.gradient}
                    onChange={(gradient: string) => look({ gradient })}
                    options={[
                        ...PALETTE_NAMES.map((name: string) => ({ value: name, label: PALETTES[name].label, group: "repeating with height" })),
                        ...GRADIENTS.filter((name: string) => !isPalette(name)).map((name: string) => ({ value: name, label: name, group: "one ramp, bottom to top" })),
                    ]}
                    data-look-colors
                />
            </Field>
            {isPalette(ui.look.gradient) && (
                <Field inline label="repeat every">
                    <NumberInput
                        size="sm"
                        min={0.25}
                        step={0.5}
                        unit="m"
                        value={ui.look.period ?? DEFAULT_PERIOD}
                        title="the colors cycle once per this much height; the raw scans use the next palette"
                        data-look-period
                        onChange={(period: number) => period > 0 && look({ period })}
                        style={{ width: 90 }}
                    />
                </Field>
            )}
            <Field inline label="paths">
                <Row wrap gap={10}>
                    {(["corrected", "raw", "loops"] as const).map((key) => (
                        <Checkbox
                            key={key}
                            value={ui.showPaths[key]}
                            label={key === "corrected" ? "loop-closed" : key === "raw" ? "odometry" : "loop links"}
                            onChange={(checked: boolean) => {
                                const showPaths = { ...ui.showPaths, [key]: checked }
                                scene.showPaths(showPaths)
                                setUi({ showPaths })
                            }}
                        />
                    ))}
                    <Checkbox
                        value={ui.pathsOnTop}
                        label="on top"
                        title="draw the paths over the voxels (off: the voxels in front hide them)"
                        onChange={(checked: boolean) => {
                            scene.pathsOnTop(checked)
                            setUi({ pathsOnTop: checked })
                        }}
                    />
                </Row>
            </Field>

            <Heading level={3}>History</Heading>
            <Column gap={2} style={{ maxHeight: 180, overflow: "auto" }}>
                {[...session.history].reverse().map((line, index) => (
                    <Text small muted key={index}>{line}</Text>
                ))}
            </Column>
        </Column>
    )
}
