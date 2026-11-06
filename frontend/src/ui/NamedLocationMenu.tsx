// Right-click in the 3D view (a right-drag orbits): on the map, "Add named location" there; on one of dimos's named
// locations, rename or delete it. The list goes into the recording as /named_locations on Save (server/src/persist.rs).
import { useEffect, useRef, useState } from "react"
import { Button, Card, Row, Text, TextInput } from "dimos-react"
import { api, type NamedLocation, type Session } from "../core/api.ts"

export interface MenuAt {
    x: number
    y: number
    /** the map point under the click (map frame), when it's on the map */
    point: [number, number, number] | null
    /** the named location under the click */
    named: string | null
}

export function NamedLocationMenu({ menu, session, run, onDone, onClose }: { menu: MenuAt; session: Session; run: <T>(action: Promise<T>, done?: string) => Promise<T | undefined>; onDone: () => void; onClose: () => void }) {
    const existing: NamedLocation | undefined = session.annotations.namedLocations?.find((l) => l.id === menu.named)
    const [text, setText] = useState(existing?.name ?? "")
    const [editing, setEditing] = useState(!existing)
    const box = useRef<HTMLDivElement>(null)
    useEffect(() => {
        const key = (event: KeyboardEvent) => event.key === "Escape" && onClose()
        const outside = (event: PointerEvent) => !box.current?.contains(event.target as Node) && onClose()
        addEventListener("keydown", key)
        addEventListener("pointerdown", outside, true)
        return () => {
            removeEventListener("keydown", key)
            removeEventListener("pointerdown", outside, true)
        }
    }, [onClose])
    if (!existing && !menu.point) {
        return null
    }
    const submit = async () => {
        const name = text.trim()
        if (!name) {
            return
        }
        if (existing) {
            await run(api.patch(session.id, existing.id, { name }), `Renamed to “${name}”`)
        } else {
            await run(api.add(session.id, { type: "namedLocation", name, position: menu.point }), `Added “${name}” (Save writes it into the recording)`)
        }
        onDone()
        onClose()
    }
    return (
        <div ref={box} className="named-menu" style={{ position: "fixed", left: menu.x, top: menu.y, zIndex: 50 }} data-named-menu>
            <Card glass style={{ padding: 8, gap: 6, minWidth: 220 }}>
                {editing
                    ? (
                        <form onSubmit={(event) => (event.preventDefault(), submit())}>
                            <Text>{existing ? "Rename named location" : "Add named location here"}</Text>
                            <Row style={{ gap: 6, marginTop: 6 }}>
                                <TextInput size="sm" autoFocus value={text} placeholder="kitchen" onChange={setText} data-named-input />
                                <Button type="submit" variant="primary" size="sm" disabled={!text.trim()}>{existing ? "Rename" : "Add"}</Button>
                            </Row>
                        </form>
                    )
                    : (
                        <>
                            <Text>{existing!.name}</Text>
                            <Row style={{ gap: 6 }}>
                                <Button size="sm" onClick={() => setEditing(true)}>Rename</Button>
                                <Button size="sm" variant="danger" onClick={() => run(api.remove(session.id, existing!.id), `Deleted “${existing!.name}” (⌘Z to undo)`).then(() => (onDone(), onClose()))}>Delete</Button>
                            </Row>
                        </>
                    )}
            </Card>
        </div>
    )
}
