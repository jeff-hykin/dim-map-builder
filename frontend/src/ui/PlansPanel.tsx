// Stage 5: floor plans. Generate a 2D plan per storey from the 3D map, then annotate it: named spots and named areas
// (no-go zones, rooms). The plan view replaces the 3D view while this stage is open: drag to pan, wheel to zoom.
import { useEffect, useRef, useState } from "react"
import { api, type Area, type PlanInfo } from "../core/api.ts"
import { Store, useStore } from "../core/store.ts"
import type { Context } from "./context.ts"

type PlanTool = "pan" | "point" | "area"

const planTool = new Store<{ tool: PlanTool; draft: [number, number][]; name: string; kind: string; hover: [number, number] | null }>({
    tool: "pan",
    draft: [],
    name: "",
    kind: "no-go",
    hover: null,
})

const AREA_COLORS: Record<string, string> = { "no-go": "#ff5f6d", zone: "#7fc8f8", slow: "#ffd166" }

export function PlansPanel({ context }: { context: Context }) {
    const { session, run, ui, setUi } = context
    const tool = useStore(planTool)
    const [multi, setMulti] = useState(true)
    if (!session) {
        return null
    }
    const floors = session.annotations.floors
    const floor = Math.min(ui.planFloor, Math.max(0, floors.length - 1))
    const points = session.annotations.planPoints.filter((p) => p.floor === floor)
    const areas = session.annotations.areas.filter((a) => a.floor === floor)
    const plan = session.plans[floor]
    return (
        <div>
            <div className="panel-head">
                <h2>Floor plans</h2>
                <p>A 2D plan per storey: black where something stands, white where the floor was seen, grey unknown.</p>
            </div>
            <div className="row">
                <label className="row" style={{ margin: 0 }}>
                    <input type="checkbox" checked={multi} onChange={(event) => setMulti(event.target.checked)} />
                    one per storey
                </label>
                <button type="button" className="button primary" data-action="generate-plans" onClick={() => run(api.plans(session.id, multi), (r) => `${r.floors.length} floor plan${r.floors.length === 1 ? "" : "s"}`)}>
                    {session.plans.length ? "Regenerate" : "Generate"}
                </button>
            </div>
            {floors.length > 0 && (
                <>
                    <h3>Floors</h3>
                    <div className="floor-tabs">
                        {floors.map((f) => (
                            <button key={f.index} type="button" className={`button ${floor === f.index ? "on" : ""}`} onClick={() => setUi({ planFloor: f.index })} data-floor={f.index}>
                                {f.name} · z {f.z.toFixed(2)}
                            </button>
                        ))}
                    </div>
                    {plan && (
                        <div className="hint">
                            {plan.width}×{plan.height} cells at {(plan.resolution * 100).toFixed(0)} cm · {Math.round((plan.free * plan.resolution ** 2))} m² free
                        </div>
                    )}
                    <h3>Annotate</h3>
                    <div className="field">
                        <span>name</span>
                        <input className="text" placeholder="e.g. Dock, Kitchen, Stairs" value={tool.name} onChange={(event) => planTool.update({ name: event.target.value })} />
                    </div>
                    <div className="row">
                        <div className="seg">
                            {(["pan", "point", "area"] as const).map((next) => (
                                <button key={next} type="button" className={tool.tool === next ? "on" : ""} data-plan-tool={next} onClick={() => planTool.update({ tool: next, draft: [] })}>
                                    {next === "pan" ? "pan" : next === "point" ? "+ named point" : "+ area"}
                                </button>
                            ))}
                        </div>
                        {tool.tool === "area" && (
                            <select value={tool.kind} onChange={(event) => planTool.update({ kind: event.target.value })}>
                                <option value="no-go">no-go zone</option>
                                <option value="zone">named zone</option>
                                <option value="slow">slow zone</option>
                            </select>
                        )}
                    </div>
                    {tool.tool === "point" && <div className="hint">Click the plan to place it.</div>}
                    {tool.tool === "area" && <div className="hint">Click corners; Enter or double-click closes the area, Esc cancels, Backspace drops the last corner.</div>}
                    <h3>Named points ({points.length})</h3>
                    <ul className="items">
                        {points.map((p) => (
                            <li key={p.id} className="item" data-plan-point={p.id}>
                                <span className="swatch" />
                                <input className="text" defaultValue={p.name} key={p.name} onBlur={(event) => event.target.value !== p.name && run(api.patch(session.id, p.id, { name: event.target.value }))} />
                                <span className="row" style={{ margin: 0 }}>
                                    <span className="kind">{p.position.map((v) => v.toFixed(1)).join(", ")}</span>
                                    <button type="button" className="icon-button" onClick={() => run(api.remove(session.id, p.id), "Deleted (⌘Z to undo)")}>
                                        ✕
                                    </button>
                                </span>
                            </li>
                        ))}
                    </ul>
                    <h3>Areas ({areas.length})</h3>
                    <ul className="items">
                        {areas.map((a: Area) => (
                            <li key={a.id} className="item" data-area={a.id}>
                                <span className={`swatch ${a.kind === "no-go" ? "no-go" : ""}`} />
                                <input className="text" defaultValue={a.name} key={a.name} onBlur={(event) => event.target.value !== a.name && run(api.patch(session.id, a.id, { name: event.target.value }))} />
                                <span className="row" style={{ margin: 0 }}>
                                    <select value={a.kind} onChange={(event) => run(api.patch(session.id, a.id, { kind: event.target.value }))}>
                                        <option value="no-go">no-go</option>
                                        <option value="zone">zone</option>
                                        <option value="slow">slow</option>
                                    </select>
                                    <button type="button" className="icon-button" onClick={() => run(api.remove(session.id, a.id), "Deleted (⌘Z to undo)")}>
                                        ✕
                                    </button>
                                </span>
                            </li>
                        ))}
                    </ul>
                </>
            )}
        </div>
    )
}

/** The plan of the chosen floor with its points and areas, pannable and zoomable; clicks place the active tool. */
export function PlanView({ context }: { context: Context }) {
    const { session, ui, run } = context
    const canvas = useRef<HTMLCanvasElement>(null)
    const host = useRef<HTMLDivElement>(null)
    const tool = useStore(planTool)
    const [image, setImage] = useState<HTMLImageElement | null>(null)
    const view = useRef({ x: 0, y: 0, zoom: 1, fitted: "" })
    const [, redraw] = useState(0)
    const floor = Math.min(ui.planFloor, Math.max(0, (session?.annotations.floors.length ?? 1) - 1))
    const plan: PlanInfo | undefined = session?.plans[floor]

    /** pixel bounds (column/row) of the free floor's 1st–99th percentile (else every known cell's), padded: sparse far
     * returns (occupied stripes outside the walls) don't set the zoom */
    const content = useRef<[number, number, number, number] | null>(null)
    useEffect(() => {
        if (!session || !plan) {
            setImage(null)
            return
        }
        const next = new Image()
        next.onload = () => {
            const scratch = document.createElement("canvas")
            scratch.width = next.width
            scratch.height = next.height
            const g = scratch.getContext("2d")!
            g.drawImage(next, 0, 0)
            const pixels = g.getImageData(0, 0, next.width, next.height).data
            const gather = (keep: (value: number) => boolean) => {
                const columns: number[] = []
                const rows: number[] = []
                for (let row = 0; row < next.height; row++) {
                    for (let column = 0; column < next.width; column++) {
                        if (keep(pixels[(row * next.width + column) * 4])) {
                            columns.push(column)
                            rows.push(row)
                        }
                    }
                }
                return { columns, rows }
            }
            let cells = gather((v) => v === 255)
            if (cells.columns.length < 50) {
                cells = gather((v) => v !== 128)
            }
            const pick = (values: number[], q: number) => values.sort((a, b) => a - b)[Math.floor(q * (values.length - 1))] ?? 0
            const pad = 20
            content.current = cells.columns.length
                ? [pick(cells.columns, 0.01) - pad, pick(cells.rows, 0.01) - pad, pick(cells.columns, 0.99) + pad, pick(cells.rows, 0.99) + pad]
                : null
            view.current.fitted = ""
            setImage(next)
        }
        next.src = api.planUrl(session.id, floor, session.revision)
    }, [session?.id, session?.revision, floor, plan?.width])

    // pixel <-> world
    const toWorld = (px: number, py: number): [number, number] => {
        const v = view.current
        const column = (px - v.x) / v.zoom
        const row = (py - v.y) / v.zoom
        return [plan!.origin[0] + column * plan!.resolution, plan!.origin[1] + (plan!.height - row) * plan!.resolution]
    }
    const toScreen = (x: number, y: number): [number, number] => {
        const v = view.current
        const column = (x - plan!.origin[0]) / plan!.resolution
        const row = plan!.height - (y - plan!.origin[1]) / plan!.resolution
        return [v.x + column * v.zoom, v.y + row * v.zoom]
    }

    useEffect(() => {
        const element = canvas.current
        const box = host.current
        if (!element || !box) {
            return
        }
        const ratio = devicePixelRatio || 1
        element.width = box.clientWidth * ratio
        element.height = box.clientHeight * ratio
        element.style.width = `${box.clientWidth}px`
        element.style.height = `${box.clientHeight}px`
        const g = element.getContext("2d")!
        g.setTransform(ratio, 0, 0, ratio, 0, 0)
        g.clearRect(0, 0, box.clientWidth, box.clientHeight)
        if (!plan || !image || !session) {
            g.fillStyle = "#6c778c"
            g.font = "13px ui-monospace, monospace"
            g.fillText(session?.plans.length ? "loading the plan…" : "Generate floor plans to see them here.", 24, 40)
            return
        }
        const v = view.current
        const key = `${session.id}:${floor}:${plan.width}x${plan.height}`
        if (v.fitted !== key) {
            const [c0, r0, c1, r1] = content.current ?? [0, 0, plan.width, plan.height]
            const w = Math.max(20, c1 - c0 + 1)
            const h = Math.max(20, r1 - r0 + 1)
            v.zoom = Math.min((box.clientWidth - 60) / w, (box.clientHeight - 60) / h)
            v.x = box.clientWidth / 2 - ((c0 + c1 + 1) / 2) * v.zoom
            v.y = box.clientHeight / 2 - ((r0 + r1 + 1) / 2) * v.zoom
            v.fitted = key
        }
        g.imageSmoothingEnabled = false
        g.drawImage(image, v.x, v.y, plan.width * v.zoom, plan.height * v.zoom)
        // a 1 m grid with labels
        g.strokeStyle = "rgba(122, 240, 168, 0.14)"
        g.fillStyle = "rgba(122, 240, 168, 0.6)"
        g.font = "10px ui-monospace, monospace"
        g.lineWidth = 1
        const [x0, y1] = toWorld(0, 0)
        const [x1, y0] = toWorld(box.clientWidth, box.clientHeight)
        const step = v.zoom * plan.resolution < 12 ? 5 : 1
        for (let x = Math.ceil(x0 / step) * step; x <= x1; x += step) {
            const [sx] = toScreen(x, 0)
            g.beginPath()
            g.moveTo(sx, 0)
            g.lineTo(sx, box.clientHeight)
            g.stroke()
            g.fillText(`${x}`, sx + 2, 12)
        }
        for (let y = Math.ceil(y0 / step) * step; y <= y1; y += step) {
            const [, sy] = toScreen(0, y)
            g.beginPath()
            g.moveTo(0, sy)
            g.lineTo(box.clientWidth, sy)
            g.stroke()
            g.fillText(`${y}`, 2, sy - 2)
        }
        // labels placed greedily: one that would overlap a placed label is skipped (points first, then areas)
        const placed: [number, number, number, number][] = []
        const label = (text: string, x: number, y: number, color: string, font = "600 12px ui-monospace, monospace") => {
            g.font = font
            const width = g.measureText(text).width
            const rect: [number, number, number, number] = [x - 2, y - 12, x + width + 2, y + 4]
            if (placed.some((p) => rect[0] < p[2] && rect[2] > p[0] && rect[1] < p[3] && rect[3] > p[1])) {
                return
            }
            placed.push(rect)
            g.fillStyle = "rgba(6, 9, 15, 0.72)"
            g.fillRect(rect[0], rect[1], rect[2] - rect[0], rect[3] - rect[1])
            g.fillStyle = color
            g.fillText(text, x, y)
        }
        const pending: (() => void)[] = []
        const polygon = (corners: [number, number][], color: string, fill: boolean, text?: string) => {
            if (!corners.length) {
                return
            }
            g.beginPath()
            corners.forEach(([x, y], index) => {
                const [sx, sy] = toScreen(x, y)
                index ? g.lineTo(sx, sy) : g.moveTo(sx, sy)
            })
            if (fill) {
                g.closePath()
                g.fillStyle = `${color}38`
                g.fill()
            }
            g.strokeStyle = color
            g.lineWidth = 2
            g.stroke()
            if (text) {
                const cx = corners.reduce((s, c) => s + c[0], 0) / corners.length
                const cy = corners.reduce((s, c) => s + c[1], 0) / corners.length
                const [sx, sy] = toScreen(cx, cy)
                g.font = "600 12px ui-monospace, monospace"
                pending.push(() => label(text, sx - g.measureText(text).width / 2, sy, color))
            }
        }
        for (const area of session.annotations.areas.filter((a) => a.floor === floor)) {
            const color = AREA_COLORS[area.kind] ?? "#7af0a8"
            polygon(area.polygon, color, true, `${area.name}${area.kind === "no-go" ? " ⛔" : ""}`)
        }
        if (tool.draft.length) {
            polygon(tool.hover ? [...tool.draft, tool.hover] : tool.draft, AREA_COLORS[tool.kind] ?? "#7af0a8", false)
            for (const [x, y] of tool.draft) {
                const [sx, sy] = toScreen(x, y)
                g.fillStyle = "#ffd166"
                g.fillRect(sx - 3, sy - 3, 6, 6)
            }
        }
        for (const point of session.annotations.planPoints.filter((p) => p.floor === floor)) {
            const [sx, sy] = toScreen(point.position[0], point.position[1])
            g.fillStyle = "#7af0a8"
            g.beginPath()
            g.arc(sx, sy, 6, 0, Math.PI * 2)
            g.fill()
            g.strokeStyle = "#06090f"
            g.lineWidth = 2
            g.stroke()
            label(point.name, sx + 9, sy + 4, "#d8e6f4")
        }
        for (const draw of pending) {
            draw()
        }
    })

    // input
    useEffect(() => {
        const element = canvas.current
        if (!element || !session || !plan) {
            return
        }
        let drag: { x: number; y: number; vx: number; vy: number } | null = null
        const local = (event: MouseEvent) => {
            const rect = element.getBoundingClientRect()
            return [event.clientX - rect.left, event.clientY - rect.top] as const
        }
        const finishArea = () => {
            const state = planTool.get()
            if (state.draft.length >= 3) {
                run(api.add(session.id, { type: "area", floor, name: state.name.trim() || (state.kind === "no-go" ? "No-go" : "Area"), kind: state.kind, polygon: state.draft }), "Area added (⌘Z to undo)")
            }
            planTool.update({ draft: [] })
        }
        const down = (event: MouseEvent) => {
            const [x, y] = local(event)
            drag = { x, y, vx: view.current.x, vy: view.current.y }
        }
        const move = (event: MouseEvent) => {
            const [x, y] = local(event)
            if (drag && (event.buttons & 1) && (planTool.get().tool === "pan" || Math.hypot(x - drag.x, y - drag.y) > 5)) {
                view.current.x = drag.vx + x - drag.x
                view.current.y = drag.vy + y - drag.y
                redraw((n) => n + 1)
            } else if (planTool.get().tool === "area") {
                planTool.update({ hover: toWorld(x, y) })
            }
        }
        const up = (event: MouseEvent) => {
            // only a press that started on the plan counts (a mouseup after clicking a panel button doesn't)
            if (!drag) {
                return
            }
            const [x, y] = local(event)
            const moved = Math.hypot(x - drag.x, y - drag.y)
            drag = null
            if (moved > 5) {
                return
            }
            const state = planTool.get()
            const at = toWorld(x, y)
            if (state.tool === "point") {
                run(api.add(session.id, { type: "planPoint", floor, name: state.name.trim() || "Spot", position: at }), "Point added (⌘Z to undo)")
            } else if (state.tool === "area") {
                planTool.update({ draft: [...state.draft, at] })
            }
        }
        const wheel = (event: WheelEvent) => {
            event.preventDefault()
            const [x, y] = local(event)
            const factor = Math.exp(-event.deltaY * 0.0015)
            const v = view.current
            v.x = x - (x - v.x) * factor
            v.y = y - (y - v.y) * factor
            v.zoom *= factor
            redraw((n) => n + 1)
        }
        const key = (event: KeyboardEvent) => {
            if ((event.target as HTMLElement).closest("input, textarea, select")) {
                return
            }
            const state = planTool.get()
            if (event.key === "Enter" && state.tool === "area") {
                finishArea()
            } else if (event.key === "Escape") {
                planTool.update({ draft: [], tool: "pan" })
            } else if (event.key === "Backspace" && state.draft.length) {
                event.stopPropagation()
                planTool.update({ draft: state.draft.slice(0, -1) })
            }
        }
        const double = () => planTool.get().tool === "area" && finishArea()
        element.addEventListener("mousedown", down)
        window.addEventListener("mousemove", move)
        window.addEventListener("mouseup", up)
        element.addEventListener("wheel", wheel, { passive: false })
        element.addEventListener("dblclick", double)
        window.addEventListener("keydown", key, true)
        return () => {
            element.removeEventListener("mousedown", down)
            window.removeEventListener("mousemove", move)
            window.removeEventListener("mouseup", up)
            element.removeEventListener("wheel", wheel)
            element.removeEventListener("dblclick", double)
            window.removeEventListener("keydown", key, true)
        }
    }, [session?.id, plan, floor, run])

    useEffect(() => {
        const box = host.current
        if (!box) {
            return
        }
        const observer = new ResizeObserver(() => redraw((n) => n + 1))
        observer.observe(box)
        return () => observer.disconnect()
    }, [])

    return (
        <div className="plan-view" ref={host} data-plan-view>
            <canvas ref={canvas} />
        </div>
    )
}
