// Named points and areas on a storey, placed in the 2D view: a dock, a door, a kitchen, a no-go zone around the stairs.
import { Column, Field, Heading, List, Row, Segmented, Select, Text, TextInput } from "dimos-react"
import { api, type Area } from "../core/api.ts"
import { useStore } from "../core/store.ts"
import type { Context } from "./context.ts"
import { planTool } from "./tools.ts"
import { AnnotationRow } from "./rows.tsx"

const KINDS = [["no-go", "no-go zone"], ["zone", "named zone"], ["slow", "slow zone"]]

export function PlacesPanel({ context }: { context: Context }) {
    const { session, run, ui, floor } = context
    const tool = useStore(planTool)
    if (!session) {
        return null
    }
    const storey = Math.min(ui.planFloor, Math.max(0, (floor?.storeys.length ?? 1) - 1))
    const points = session.annotations.planPoints.filter((p) => p.floor === storey)
    const areas = session.annotations.areas.filter((a) => a.floor === storey)
    const remove = (id: string) => run(api.remove(session.id, id), "Deleted (⌘Z to undo)")
    return (
        <Column gap={8}>
            <Field inline label="name">
                <TextInput size="sm" placeholder="e.g. Dock, Kitchen, Stairs" value={tool.name} onChange={(name: string) => planTool.update({ name })} data-place-name />
            </Field>
            <Row wrap>
                <Segmented
                    size="sm"
                    value={tool.tool}
                    onChange={(next: "point" | "area") => planTool.update({ tool: next, draft: [] })}
                    items={[{ value: "point", label: "named point", props: { "data-plan-tool": "point" } }, { value: "area", label: "area", props: { "data-plan-tool": "area" } }]}
                />
                {tool.tool === "area" && <Select size="sm" value={tool.kind} onChange={(kind: string) => planTool.update({ kind })} options={KINDS} />}
            </Row>
            <Text small muted>{tool.tool === "point" ? "Click the 2D view to place it." : "Click corners; Enter or a double-click (double tap) closes the area, Esc cancels, Backspace drops the last corner."}</Text>
            <Heading level={3}>Named points ({points.length})</Heading>
            <List>
                {points.map((p) => (
                    <AnnotationRow
                        key={p.id}
                        name={p.name}
                        onRename={(name) => run(api.patch(session.id, p.id, { name }))}
                        detail={p.position.map((v) => v.toFixed(1)).join(", ")}
                        onDelete={() => remove(p.id)}
                        data-plan-point={p.id}
                    />
                ))}
            </List>
            <Heading level={3}>Areas ({areas.length})</Heading>
            <List>
                {areas.map((a: Area) => (
                    <AnnotationRow
                        key={a.id}
                        name={a.name}
                        tone={a.kind === "no-go" ? "danger" : "ok"}
                        onRename={(name) => run(api.patch(session.id, a.id, { name }))}
                        extra={<Select size="sm" value={a.kind} onChange={(kind: string) => run(api.patch(session.id, a.id, { kind }))} options={[["no-go", "no-go"], ["zone", "zone"], ["slow", "slow"]]} />}
                        onDelete={() => remove(a.id)}
                        data-area={a.id}
                    />
                ))}
            </List>
        </Column>
    )
}
