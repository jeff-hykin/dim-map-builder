// Stage 4: annotate the 3D map: labelled boxes, planes and points, placed by clicking the map, then moved / turned /
// resized with the gizmo. The agent's additions show in blue; everything is editable and undoable.
import { useEffect, useState } from "react"
import * as THREE from "three"
import { api } from "../core/api.ts"
import type { GizmoMode } from "../core/scene.ts"
import { Button, Card, Column, Field, Heading, Kbd, List, NumberInput, Row, Segmented, Text, TextInput } from "dimos-react"
import type { Context } from "./context.ts"
import { AnnotationRow } from "./rows.tsx"

type Placing = "box" | "plane" | "point" | null

export function AnnotatePanel({ context }: { context: Context }) {
    const { session, scene, run, ui } = context
    const [placing, setPlacing] = useState<Placing>(null)
    const [label, setLabel] = useState("")
    const [mode, setMode] = useState<GizmoMode>("translate")

    useEffect(() => {
        if (!scene || !session) {
            return
        }
        if (!placing) {
            scene.onPick = null
            return
        }
        scene.onPick = async (point) => {
            const name = label.trim() || placing
            let created: { id: string } | undefined
            if (placing === "box") {
                created = await run(api.add(session.id, { type: "box", label: name, box: { center: [point.x, point.y, point.z + 0.3], size: [0.6, 0.6, 0.6], yaw: 0 } }))
            } else if (placing === "point") {
                created = await run(api.add(session.id, { type: "point", label: name, position: [point.x, point.y, point.z] }))
            } else {
                // a vertical plane facing the camera (turn it with the gizmo)
                const toward = scene.viewer.camera.position.clone().sub(point).setZ(0).normalize()
                created = await run(api.add(session.id, { type: "plane", label: name, center: [point.x, point.y, point.z], normal: [toward.x || 1, toward.y, 0], size: [1, 1] }))
            }
            setPlacing(null)
            scene.onPick = null
            if (created) {
                await context.refresh()
                scene.select({ kind: placing, id: created.id })
            }
        }
        return () => {
            scene.onPick = null
        }
    }, [placing, label, scene, session, run, context])

    if (!session || !scene) {
        return null
    }
    const a = session.annotations
    const selected = ui.selected
    const gizmo = (next: GizmoMode) => {
        setMode(next)
        scene.setGizmoMode(next)
    }
    const fitRegion = async () => {
        const region = scene.region ?? scene.regionFromView()
        const fit = await run(api.fitBox(session.id, region))
        if (fit) {
            const created = await run(api.add(session.id, { type: "box", label: label.trim() || "object", box: fit.box }), `Box fitted to ${fit.voxels} voxels`)
            if (created) {
                await context.refresh()
                scene.select({ kind: "box", id: created.id })
            }
        }
    }
    const row = (kind: "box" | "plane" | "point", id: string, name: string, source: string, detail: string) => (
        <AnnotationRow
            key={id}
            name={name}
            onRename={(label) => run(api.patch(session.id, id, { label }))}
            tone={source === "agent" ? "info" : "ok"}
            toneLabel={source === "agent" ? "added by the agent" : "added by you"}
            detail={detail}
            selected={selected?.id === id}
            onClick={() => scene.select({ kind, id })}
            onDelete={() => {
                if (selected?.id === id) {
                    scene.select(null)
                }
                run(api.remove(session.id, id), "Deleted (⌘Z to undo)")
            }}
            data-annotation={id}
        />
    )
    const fmt = (v: number) => v.toFixed(2)
    return (
        <Column gap={8}>
            <Text small muted>
                Mark things in 3D. Click a tool, then click the map. Select one to move <Kbd>G</Kbd>, turn <Kbd>R</Kbd> or resize <Kbd>S</Kbd> it; <Kbd>Del</Kbd> removes it.
            </Text>
            <Field inline label="label">
                <TextInput size="sm" placeholder="e.g. chair, door, charger" value={label} onChange={setLabel} />
            </Field>
            <Row wrap>
                {(["box", "plane", "point"] as const).map((kind) => (
                    <Button key={kind} size="sm" icon="plus" pressed={placing === kind} data-place={kind} onClick={() => setPlacing(placing === kind ? null : kind)}>
                        {kind}
                    </Button>
                ))}
                <Button size="sm" title="A box hugging what's in the region box (or the view)" onClick={fitRegion}>
                    fit box
                </Button>
            </Row>
            {placing && <Text small muted>Click the map where the {placing} goes (Esc cancels).</Text>}
            <Field inline label="gizmo">
                <Segmented
                    size="sm"
                    value={mode}
                    onChange={gizmo}
                    items={[["translate", "move G"], ["rotate", "turn R"], ["scale", "size S"]]}
                />
            </Field>

            <Heading level={3}>Boxes ({a.boxes.length})</Heading>
            <List>{a.boxes.map((b) => row("box", b.id, b.label, b.source, `${fmt(b.box.size[0])}×${fmt(b.box.size[1])}×${fmt(b.box.size[2])} m`))}</List>
            <Heading level={3}>Planes ({a.planes.length})</Heading>
            <List>{a.planes.map((p) => row("plane", p.id, p.label, p.source, `${fmt(p.size[0])}×${fmt(p.size[1])} m`))}</List>
            <Heading level={3}>Points ({a.points.length})</Heading>
            <List>{a.points.map((p) => row("point", p.id, p.label, p.source, p.position.map(fmt).join(", ")))}</List>
            {selected && selected.kind !== "region" && <SelectedDetails context={context} />}
        </Column>
    )
}

/** exact numbers for the selected annotation */
function SelectedDetails({ context }: { context: Context }) {
    const { session, ui, run } = context
    const box = session?.annotations.boxes.find((b) => b.id === ui.selected?.id)
    if (!session || !box) {
        return null
    }
    const set = (key: "center" | "size", axis: number, value: number) => {
        const next = { ...box.box, [key]: box.box[key].map((v, i) => (i === axis ? value : v)) }
        run(api.patch(session.id, box.id, { box: next }))
    }
    return (
        <Card title={box.label}>
            {(["center", "size"] as const).map((key) => (
                <Field inline label={key} key={key}>
                    <Row gap={4}>
                        {box.box[key].map((value, axis) => (
                            <NumberInput key={`${box.id}${key}${axis}`} size="sm" step={0.05} precision={2} value={value} onChange={(next: number) => set(key, axis, next)} style={{ width: 72 }} />
                        ))}
                    </Row>
                </Field>
            ))}
            <Field inline label="yaw">
                <Text mono>{THREE.MathUtils.radToDeg(box.box.yaw).toFixed(1)}°</Text>
            </Field>
        </Card>
    )
}
