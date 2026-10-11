// Polygons: an area drawn in 2D (click corners; Enter or double-click closes it) that stands up in 3D as a prism from
// the local floor, 1 m tall unless you say otherwise. Select one in either view; drag its corners in 2D or its top
// handle in 3D; Del removes it.
import { Column, Field, Heading, List, NumberInput, Text, TextInput } from "dimos-react"
import { api, type PrismAnnotation } from "../core/api.ts"
import { useStore } from "../core/store.ts"
import type { Context } from "./context.ts"
import { polygonTool } from "./tools.ts"
import { AnnotationRow } from "./rows.tsx"

export function PolygonPanel({ context }: { context: Context }) {
    const { session, scene, run, ui, floor } = context
    const tool = useStore(polygonTool)
    if (!session || !scene) {
        return null
    }
    const storey = Math.min(ui.planFloor, Math.max(0, (floor?.storeys.length ?? 1) - 1))
    const prisms = (session.annotations.prisms ?? []).filter((p) => p.floor === storey)
    const number = (prism: PrismAnnotation, key: "height" | "base") => (
        <NumberInput
            key={`${prism.id}${key}`}
            size="sm"
            step={0.05}
            precision={2}
            value={prism[key]}
            title={key === "height" ? "height over its base (m)" : "base z (m)"}
            onClick={(event: MouseEvent) => event.stopPropagation()}
            onChange={(value: number) => run(api.patch(session.id, prism.id, { [key]: value }))}
            data-prism-field={key}
            style={{ width: 64 }}
        />
    )
    return (
        <Column gap={8}>
            <Field inline label="label">
                <TextInput size="sm" placeholder="e.g. couch, desk, lab bench" value={tool.label} onChange={(label: string) => polygonTool.update({ label })} data-polygon-label />
            </Field>
            <Field inline label="height" hint="m over the local floor">
                <NumberInput size="sm" min={0.05} step={0.05} unit="m" value={tool.height} onChange={(height: number) => polygonTool.update({ height })} data-polygon-height style={{ width: 90 }} />
            </Field>
            <Text small muted>Click corners in the 2D view; Enter or double-click closes it, Backspace drops the last corner, Esc cancels.</Text>
            <Heading level={3}>Polygons ({prisms.length})</Heading>
            <List>
                {prisms.map((p) => (
                    <AnnotationRow
                        key={p.id}
                        name={p.label}
                        tone={p.source === "agent" ? "info" : "ok"}
                        onRename={(label) => run(api.patch(session.id, p.id, { label }))}
                        selected={ui.selected?.id === p.id}
                        onClick={() => scene.select({ kind: "prism", id: p.id })}
                        extra={
                            <>
                                <Text small muted>h</Text>
                                {number(p, "height")}
                                <Text small muted>base</Text>
                                {number(p, "base")}
                            </>
                        }
                        onDelete={() => run(api.remove(session.id, p.id), "Deleted (⌘Z to undo)")}
                        data-prism={p.id}
                    />
                ))}
            </List>
        </Column>
    )
}
