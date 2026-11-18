// Right-click in the 2D view on an area or a named point: rename or delete it there (the Places panel lists them too).
import { useEffect, useRef, useState } from "react"
import { Button, Card, Row, Text, TextInput } from "dimos-react"
import { api } from "../core/api.ts"

export interface PlanMenuAt {
    x: number
    y: number
    id: string
    name: string
    /** what it is, for the menu's words */
    kind: "area" | "named point"
}

export function PlanMenu({ menu, sessionId, run, onClose }: { menu: PlanMenuAt; sessionId: string; run: <T>(action: Promise<T>, done?: string) => Promise<T | undefined>; onClose: () => void }) {
    const [text, setText] = useState(menu.name)
    const [editing, setEditing] = useState(false)
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
    const rename = async () => {
        const name = text.trim()
        if (name && name !== menu.name) {
            await run(api.patch(sessionId, menu.id, { name }), `Renamed to “${name}” (⌘Z to undo)`)
        }
        onClose()
    }
    return (
        <div ref={box} className="named-menu" style={{ position: "fixed", left: menu.x, top: menu.y, zIndex: 50 }} data-plan-menu={menu.id}>
            <Card glass style={{ padding: 8, gap: 6, minWidth: 220 }}>
                {editing
                    ? (
                        <form onSubmit={(event) => (event.preventDefault(), rename())}>
                            <Text>Rename {menu.kind}</Text>
                            <Row style={{ gap: 6, marginTop: 6 }}>
                                <TextInput size="sm" autoFocus value={text} onChange={setText} data-plan-menu-input />
                                <Button type="submit" variant="primary" size="sm" disabled={!text.trim()}>Rename</Button>
                            </Row>
                        </form>
                    )
                    : (
                        <>
                            <Text>{menu.name}</Text>
                            <Row style={{ gap: 6 }}>
                                <Button size="sm" onClick={() => setEditing(true)} data-plan-menu-rename>Rename</Button>
                                <Button size="sm" variant="danger" onClick={() => run(api.remove(sessionId, menu.id), `Deleted “${menu.name}” (⌘Z to undo)`).then(onClose)} data-plan-menu-delete>Delete</Button>
                            </Row>
                        </>
                    )}
            </Card>
        </div>
    )
}
