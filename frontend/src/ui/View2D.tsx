// The 2D view: a top-down slice of the current voxels with the named points and areas, the polygon annotations, and
// the paint tools' strokes. As the main view it pans (right/middle drag, or the
// pan tool) and zooms (wheel), and its camera is kept with the session; as the minimap it frames the map, shows where
// the 3D camera is and what it sees, and a click or drag there moves the 3D camera.
import { useEffect, useMemo, useRef, useState } from "react"
import { api, type PrismAnnotation, type SavedView } from "../core/api.ts"
import { Store, useStore } from "../core/store.ts"
import { computeSlice, floorHeightImage, PLAN_STYLE, sliceLayers, type FloorModel } from "../core/slice.ts"
import type { Context, View2d } from "./context.ts"
import { modifyTool, planTool, polygonTool } from "./tools.ts"

const AREA_COLORS: Record<string, string> = { "no-go": "#ff5f6d", zone: "#7fc8f8", slow: "#ffd166" }
const PRISM_COLOR = "#7af0a8"
const NO_MAP = new Store({ revision: 0 })
/** a window event: frame the 2D view on the map again */
export const FIT_2D = "map-builder:fit-2d"
/** a window event carrying a saved view: look where it looked */
export const APPLY_2D = "map-builder:apply-2d"
const SELECTED_COLOR = "#ffd166"

interface Raster {
    floor: HTMLCanvasElement
    walls: HTMLCanvasElement
    /** world x, y of the raster's lowest corner, meters per pixel, size in pixels */
    origin: [number, number]
    resolution: number
    width: number
    height: number
    /** the world bounds worth framing (the bulk of what's drawn) */
    content: [number, number, number, number] | null
}

/** world bounds of the 2nd–98th percentile of known cells */
function contentBounds(kindAt: (column: number, row: number) => number, width: number, height: number, origin: [number, number], resolution: number): [number, number, number, number] | null {
    const xs: number[] = []
    const ys: number[] = []
    const step = Math.max(1, Math.floor(Math.sqrt((width * height) / 200000)))
    for (let row = 0; row < height; row += step) {
        for (let column = 0; column < width; column += step) {
            if (kindAt(column, row) !== 0) {
                xs.push(origin[0] + column * resolution)
                ys.push(origin[1] + (height - row) * resolution)
            }
        }
    }
    if (xs.length < 20) {
        return null
    }
    const pick = (values: number[], q: number) => values.sort((a, b) => a - b)[Math.floor(q * (values.length - 1))]
    return [pick(xs, 0.02), pick(ys, 0.02), pick(xs, 0.98), pick(ys, 0.98)]
}

/** the storey whose floor is just under height z */
export function storeyAt(floor: FloorModel | null, z: number): number {
    let best = 0
    floor?.storeys.forEach((storey, index) => {
        if (storey.level <= z + 0.5) {
            best = index
        }
    })
    return best
}

function inside(polygon: [number, number][], x: number, y: number) {
    let hit = false
    for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
        const [xi, yi] = polygon[i]
        const [xj, yj] = polygon[j]
        if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) {
            hit = !hit
        }
    }
    return hit
}

export function View2D({ context, kind }: { context: Context; kind: "main" | "minimap" }) {
    const { session, scene, ui, setUi, run, floor } = context
    const minimap = kind === "minimap"
    const host = useRef<HTMLDivElement>(null)
    const canvas = useRef<HTMLCanvasElement>(null)
    const plans = useStore(planTool)
    const polygon = useStore(polygonTool)
    const mapRevision = useStore(scene?.mapRevision ?? NO_MAP).revision
    const [cameraTick, setCameraTick] = useState(0)
    const [planImage, setPlanImage] = useState<Raster | null>(null)
    /** the view's camera; persisted for the main view, framed afresh for the minimap */
    const view = useRef<View2d | null>(null)
    const restoredFor = useRef<string | null>(null)
    /** the view frames the map by itself (re-framed on resize) until it's panned or zoomed */
    const autoFit = useRef(true)
    if (!minimap && session && restoredFor.current !== session.id) {
        restoredFor.current = session.id
        view.current = ui.view2d ? { ...ui.view2d } : null
        autoFit.current = !ui.view2d
    }
    const stroke = useRef<{ tool: string; path: [number, number][] } | null>(null)
    /** a press on the view, and a pan in progress (refs: a re-render mid-drag mustn't lose them) */
    const press = useRef<{ x: number; y: number } | null>(null)
    const panning = useRef<{ x: number; y: number; cx: number; cy: number } | null>(null)
    const dragVertex = useRef<{ id: string; index: number; polygon: [number, number][] } | null>(null)
    const [, redraw] = useState(0)

    const storey = minimap && scene ? storeyAt(floor, scene.viewer.controls.target.z) : Math.min(ui.planFloor, Math.max(0, (floor?.storeys.length ?? 1) - 1))
    // the slice raster, recomputed when the map, the floor or the range change
    const slice = useMemo(() => {
        if (!scene || !floor || !floor.storeys.length) {
            return null
        }
        const started = performance.now()
        const range = ui.slice
        const computed = computeSlice(scene.map.positions as Float32Array, floor, storey, range)
        const layers = sliceLayers(computed)
        const kindAt = (column: number, row: number) => computed.cells[(computed.height - 1 - row) * computed.width + column]
        const raster: Raster = { ...layers, origin: computed.origin, resolution: computed.resolution, width: computed.width, height: computed.height, content: contentBounds(kindAt, computed.width, computed.height, computed.origin, computed.resolution) }
        return { raster, milliseconds: performance.now() - started }
    }, [scene, floor, storey, ui.slice.follow, ui.slice.z0, ui.slice.z1, mapRevision])

    const heightImage = useMemo(() => (floor && ui.floorOverlay && !minimap ? floorHeightImage(floor, storey) : null), [floor, storey, ui.floorOverlay, minimap])

    const raster = slice?.raster ?? null

    // the minimap follows the 3D camera
    useEffect(() => {
        if (!minimap || !scene) {
            return
        }
        let frame = 0
        const listener = () => {
            cancelAnimationFrame(frame)
            frame = requestAnimationFrame(() => setCameraTick((n) => n + 1))
        }
        scene.cameraListeners.add(listener)
        return () => {
            scene.cameraListeners.delete(listener)
            cancelAnimationFrame(frame)
        }
    }, [minimap, scene])

    const size = () => ({ width: host.current?.clientWidth ?? 1, height: host.current?.clientHeight ?? 1 })
    const toScreen = (x: number, y: number): [number, number] => {
        const v = view.current!
        const { width, height } = size()
        return [width / 2 + (x - v.cx) * v.pixelsPerMeter, height / 2 - (y - v.cy) * v.pixelsPerMeter]
    }
    const toWorld = (sx: number, sy: number): [number, number] => {
        const v = view.current!
        const { width, height } = size()
        return [v.cx + (sx - width / 2) / v.pixelsPerMeter, v.cy - (sy - height / 2) / v.pixelsPerMeter]
    }
    const persist = useMemo(() => {
        let timer = 0
        return () => {
            if (minimap) {
                return
            }
            clearTimeout(timer)
            timer = window.setTimeout(() => view.current && setUi({ view2d: { ...view.current } }), 300)
        }
    }, [minimap, setUi])

    const prisms: PrismAnnotation[] = (session?.annotations.prisms ?? []).filter((p) => p.floor === storey)
    const selectedPrism = ui.selected?.kind === "prism" ? prisms.find((p) => p.id === ui.selected!.id) ?? null : null

    // draw
    useEffect(() => {
        const element = canvas.current
        const box = host.current
        if (!element || !box) {
            return
        }
        const drawStarted = performance.now()
        const { width, height } = size()
        const ratio = devicePixelRatio || 1
        if (element.width !== Math.round(width * ratio) || element.height !== Math.round(height * ratio)) {
            element.width = Math.round(width * ratio)
            element.height = Math.round(height * ratio)
            element.style.width = `${width}px`
            element.style.height = `${height}px`
        }
        const g = element.getContext("2d")!
        g.setTransform(ratio, 0, 0, ratio, 0, 0)
        g.clearRect(0, 0, width, height)
        if (!session || !raster) {
            g.fillStyle = "#6c778c"
            g.font = "13px ui-monospace, monospace"
            g.fillText(!session ? "" : session.stage === "map" ? "loading the map…" : "Generate the map first.", 24, 40)
            return
        }
        if (width < 80 || height < 80) {
            return
        }
        // frame the content until the user moves the view (always, for the minimap)
        if (!view.current || minimap || autoFit.current) {
            autoFit.current = true
            const [x0, y0, x1, y1] = raster.content ?? [raster.origin[0], raster.origin[1], raster.origin[0] + raster.width * raster.resolution, raster.origin[1] + raster.height * raster.resolution]
            const pad = minimap ? 8 : 40
            const pixelsPerMeter = Math.min((width - pad * 2) / Math.max(1, x1 - x0), (height - pad * 2) / Math.max(1, y1 - y0))
            view.current = { cx: (x0 + x1) / 2, cy: (y0 + y1) / 2, pixelsPerMeter }
        }
        const v = view.current!
        const [left, top] = toScreen(raster.origin[0], raster.origin[1] + raster.height * raster.resolution)
        const drawWidth = raster.width * raster.resolution * v.pixelsPerMeter
        const drawHeight = raster.height * raster.resolution * v.pixelsPerMeter
        g.imageSmoothingEnabled = v.pixelsPerMeter * raster.resolution < 6
        g.imageSmoothingQuality = "high"
        g.drawImage(raster.floor, left, top, drawWidth, drawHeight)
        if (!minimap) {
            g.filter = PLAN_STYLE.glow
        }
        g.drawImage(raster.walls, left, top, drawWidth, drawHeight)
        g.filter = "none"
        if (heightImage && floor) {
            const [hx, hy] = toScreen(floor.origin[0], floor.origin[1] + floor.height * floor.cell)
            g.globalAlpha = 0.85
            g.imageSmoothingEnabled = false
            g.drawImage(heightImage.canvas, hx, hy, floor.width * floor.cell * v.pixelsPerMeter, floor.height * floor.cell * v.pixelsPerMeter)
            g.globalAlpha = 1
        }
        // a 1 m grid (5 m when zoomed out), labelled
        if (!minimap) {
            g.strokeStyle = PLAN_STYLE.grid
            g.fillStyle = PLAN_STYLE.gridText
            g.font = "10px ui-monospace, monospace"
            g.lineWidth = 1
            const [x0, y1] = toWorld(0, 0)
            const [x1, y0] = toWorld(width, height)
            const step = v.pixelsPerMeter < 12 ? 5 : 1
            for (let x = Math.ceil(x0 / step) * step; x <= x1; x += step) {
                const [sx] = toScreen(x, 0)
                g.beginPath()
                g.moveTo(sx, 0)
                g.lineTo(sx, height)
                g.stroke()
                g.fillText(`${x}`, sx + 2, 66)
            }
            for (let y = Math.ceil(y0 / step) * step; y <= y1; y += step) {
                const [, sy] = toScreen(0, y)
                g.beginPath()
                g.moveTo(0, sy)
                g.lineTo(width, sy)
                g.stroke()
                g.fillText(`${y}`, 2, sy - 2)
            }
        }
        // labels placed greedily: one that would overlap a placed label is skipped
        const placed: [number, number, number, number][] = []
        const label = (text: string, x: number, y: number, color: string) => {
            if (minimap) {
                return
            }
            g.font = "600 12px ui-monospace, monospace"
            const textWidth = g.measureText(text).width
            const rect: [number, number, number, number] = [x - 2, y - 12, x + textWidth + 2, y + 4]
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
        const outline = (corners: [number, number][], color: string, fill: boolean, text?: string, dashed = false) => {
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
            g.setLineDash(dashed ? [6, 4] : [])
            g.strokeStyle = color
            g.lineWidth = minimap ? 1.2 : 2
            g.stroke()
            g.setLineDash([])
            if (text) {
                const cx = corners.reduce((s, c) => s + c[0], 0) / corners.length
                const cy = corners.reduce((s, c) => s + c[1], 0) / corners.length
                const [sx, sy] = toScreen(cx, cy)
                g.font = "600 12px ui-monospace, monospace"
                pending.push(() => label(text, sx - g.measureText(text).width / 2, sy, color))
            }
        }
        for (const area of session.annotations.areas.filter((a) => a.floor === storey)) {
            const color = AREA_COLORS[area.kind] ?? "#7af0a8"
            outline(area.polygon, color, true, `${area.name}${area.kind === "no-go" ? " ⛔" : ""}`)
        }
        for (const prism of prisms) {
            const selected = selectedPrism?.id === prism.id
            const corners = dragVertex.current?.id === prism.id ? dragVertex.current.polygon : prism.polygon
            outline(corners, selected ? SELECTED_COLOR : prism.source === "agent" ? "#7fc8f8" : PRISM_COLOR, true, `${prism.label} · ${prism.height.toFixed(2)} m`)
            if (selected && !minimap) {
                for (const [x, y] of corners) {
                    const [sx, sy] = toScreen(x, y)
                    g.fillStyle = SELECTED_COLOR
                    g.fillRect(sx - 4, sy - 4, 8, 8)
                }
            }
        }
        const drafts: [string, [number, number][], [number, number] | null][] = [
            [AREA_COLORS[plans.kind] ?? "#7af0a8", plans.draft, plans.hover],
            [PRISM_COLOR, polygon.draft, polygon.hover],
        ]
        for (const [color, draft, hover] of drafts) {
            if (draft.length) {
                outline(hover ? [...draft, hover] : draft, color, false, undefined, true)
                for (const [x, y] of draft) {
                    const [sx, sy] = toScreen(x, y)
                    g.fillStyle = "#ffd166"
                    g.fillRect(sx - 3, sy - 3, 6, 6)
                }
            }
        }
        for (const point of session.annotations.planPoints.filter((p) => p.floor === storey)) {
            const [sx, sy] = toScreen(point.position[0], point.position[1])
            g.fillStyle = "#7af0a8"
            g.beginPath()
            g.arc(sx, sy, minimap ? 3 : 6, 0, Math.PI * 2)
            g.fill()
            g.strokeStyle = "#06090f"
            g.lineWidth = 2
            g.stroke()
            label(point.name, sx + 9, sy + 4, "#d8e6f4")
        }
        for (const draw of pending) {
            draw()
        }
        // the Modify stroke in progress
        const active = stroke.current
        if (active && active.path.length) {
            const state = modifyTool.get()
            const radius = active.tool === "erase" ? state.radius : active.tool === "straighten" ? state.band / 2 : state.width / 2
            g.strokeStyle = active.tool === "erase" ? "rgba(255, 95, 109, 0.9)" : active.tool === "straighten" ? "rgba(255, 209, 102, 0.95)" : "rgba(122, 240, 168, 0.95)"
            g.fillStyle = active.tool === "erase" ? "rgba(255, 95, 109, 0.25)" : active.tool === "straighten" ? "rgba(255, 209, 102, 0.18)" : "rgba(122, 240, 168, 0.3)"
            g.lineCap = "round"
            g.lineJoin = "round"
            g.lineWidth = Math.max(2, radius * 2 * v.pixelsPerMeter)
            g.beginPath()
            active.path.forEach(([x, y], index) => {
                const [sx, sy] = toScreen(x, y)
                index ? g.lineTo(sx, sy) : g.moveTo(sx, sy)
            })
            if (active.path.length === 1) {
                const [sx, sy] = toScreen(active.path[0][0], active.path[0][1])
                g.lineTo(sx + 0.01, sy)
            }
            g.strokeStyle = g.fillStyle
            g.stroke()
            g.lineWidth = 1.5
            g.strokeStyle = active.tool === "erase" ? "#ff5f6d" : active.tool === "straighten" ? "#ffd166" : "#7af0a8"
            g.stroke()
            g.lineCap = "butt"
        }
        // the 3D camera on the minimap, like a game's player marker: a chevron where the camera stands pointing the way
        // it looks, with a short view-cone wedge (fixed on screen); off the map it sits on the edge, pointing out to it
        if (minimap && scene) {
            const { camera, target } = scene.footprint(0)
            const lens = scene.viewer.camera
            const halfFov = Math.min(1.3, Math.atan(Math.tan(((lens.fov * Math.PI) / 180) / 2) * lens.aspect))
            const angle = -Math.atan2(target[1] - camera[1], target[0] - camera[0])
            const [rx, ry] = toScreen(camera[0], camera[1])
            const margin = 16
            const px = Math.min(width - margin, Math.max(margin, rx))
            const py = Math.min(height - margin, Math.max(margin, ry))
            const offMap = px !== rx || py !== ry
            const accent = "122, 240, 168"
            const [tx, ty] = toScreen(target[0], target[1])
            if (!offMap) {
                g.strokeStyle = `rgba(${accent}, 0.55)`
                g.lineWidth = 1.2
                g.beginPath()
                g.arc(tx, ty, 3.5, 0, Math.PI * 2)
                g.stroke()
            }
            g.save()
            g.translate(px, py)
            g.rotate(angle)
            const reach = 74
            const cone = g.createRadialGradient(0, 0, 6, 0, 0, reach)
            cone.addColorStop(0, `rgba(${accent}, 0.42)`)
            cone.addColorStop(1, `rgba(${accent}, 0)`)
            g.fillStyle = cone
            g.beginPath()
            g.moveTo(0, 0)
            g.arc(0, 0, reach, -halfFov, halfFov)
            g.closePath()
            g.fill()
            g.shadowColor = `rgba(${accent}, 0.9)`
            g.shadowBlur = 12
            g.fillStyle = `rgb(${accent})`
            g.globalAlpha = offMap ? 0.75 : 1
            g.beginPath()
            g.moveTo(12, 0)
            g.lineTo(-8, -8.5)
            g.lineTo(-3.5, 0)
            g.lineTo(-8, 8.5)
            g.closePath()
            g.fill()
            g.shadowBlur = 0
            g.strokeStyle = "#06090f"
            g.lineWidth = 1.5
            g.stroke()
            g.restore()
            if (offMap) {
                // a small pointer on the edge, toward where the camera really is
                const away = Math.atan2(ry - py, rx - px)
                g.save()
                g.translate(px + Math.cos(away) * 11, py + Math.sin(away) * 11)
                g.rotate(away)
                g.fillStyle = `rgba(${accent}, 0.9)`
                g.beginPath()
                g.moveTo(5, 0)
                g.lineTo(-3, -4)
                g.lineTo(-3, 4)
                g.closePath()
                g.fill()
                g.restore()
            }
        }
        if (host.current && slice) {
            host.current.dataset.sliceMs = slice.milliseconds.toFixed(1)
            host.current.dataset.drawMs = (performance.now() - drawStarted).toFixed(1)
            host.current.dataset.storey = String(storey)
        }
    })

    // input
    useEffect(() => {
        const element = canvas.current
        if (!element || !session || !scene) {
            return
        }
        const local = (event: MouseEvent) => {
            const rect = element.getBoundingClientRect()
            return [event.clientX - rect.left, event.clientY - rect.top] as const
        }
        const current = () => context
        // which tool a left press uses here
        const tool = (): string => {
            if (minimap) {
                return "camera"
            }
            const picked = current().ui.tool
            if (picked === "places") {
                return `plan-${planTool.get().tool}`
            }
            return ["erase", "brush", "line", "straighten", "polygon"].includes(picked) ? picked : "select"
        }
        const finishPolygon = (draftOf: "plan" | "prism") => {
            if (draftOf === "plan") {
                const state = planTool.get()
                if (state.draft.length >= 3) {
                    run(api.add(session.id, { type: "area", floor: storey, name: state.name.trim() || (state.kind === "no-go" ? "No-go" : "Area"), kind: state.kind, polygon: state.draft }), "Area added (⌘Z to undo)")
                }
                planTool.update({ draft: [] })
            } else {
                const state = polygonTool.get()
                if (state.draft.length >= 3) {
                    run(api.add(session.id, { type: "prism", floor: storey, label: state.label.trim() || "area", polygon: state.draft, height: state.height }), "Polygon added (⌘Z to undo)").then((created) => {
                        if (created) {
                            current().refresh().then(() => scene.select({ kind: "prism", id: created.id }))
                        }
                    })
                }
                polygonTool.update({ draft: [], hover: null })
            }
        }
        const sendStroke = async (done: { tool: string; path: [number, number][] }) => {
            const state = modifyTool.get()
            const range = current().ui.slice
            const reach = state.fullColumn ? { fullColumn: true } : { zEnd: range.z1, relative: range.follow }
            let body: Record<string, unknown>
            if (done.tool === "erase") {
                body = { tool: "erase", floor: storey, path: done.path, radius: state.radius, ...reach }
            } else if (done.tool === "brush" || done.tool === "line") {
                body = { tool: "draw", floor: storey, path: done.tool === "line" ? [done.path[0], done.path[done.path.length - 1]] : done.path, width: state.width, height: state.height }
            } else {
                const [from, to] = [done.path[0], done.path[done.path.length - 1]]
                if (Math.hypot(to[0] - from[0], to[1] - from[1]) < 0.2) {
                    return
                }
                body = { tool: "straighten", floor: storey, from, to, width: state.band, thickness: state.thickness, ...reach }
            }
            modifyTool.update({ busy: true })
            await run(api.modify(session.id, body), (r) => `${r.label}: ${r.changed.toLocaleString()} voxels changed (⌘Z undoes)`)
            modifyTool.update({ busy: false })
        }
        const down = (event: MouseEvent) => {
            const [x, y] = local(event)
            const at = toWorld(x, y)
            press.current = { x, y }
            if (minimap && event.button === 0) {
                scene.moveTargetTo(at[0], at[1])
                panning.current = null
                return
            }
            const which = tool()
            if (event.button !== 0 || (which === "select" && !selectedPrism)) {
                panning.current = { x, y, cx: view.current!.cx, cy: view.current!.cy }
            }
            if (event.button !== 0) {
                return
            }
            if (["erase", "brush", "line", "straighten"].includes(which)) {
                stroke.current = { tool: which, path: [at] }
                redraw((n) => n + 1)
            } else if (which === "select" && selectedPrism) {
                // grab a corner of the selected polygon, else pan
                const hit = selectedPrism.polygon.findIndex(([px, py]) => {
                    const [sx, sy] = toScreen(px, py)
                    return Math.hypot(sx - x, sy - y) < 9
                })
                if (hit >= 0) {
                    dragVertex.current = { id: selectedPrism.id, index: hit, polygon: selectedPrism.polygon.map((c) => [...c] as [number, number]) }
                } else {
                    panning.current = { x, y, cx: view.current!.cx, cy: view.current!.cy }
                }
            }
        }
        const move = (event: MouseEvent) => {
            const [x, y] = local(event)
            if (minimap) {
                if (press.current && event.buttons & 1) {
                    const at = toWorld(x, y)
                    scene.moveTargetTo(at[0], at[1])
                }
                return
            }
            const at = toWorld(x, y)
            if (dragVertex.current && event.buttons & 1) {
                dragVertex.current.polygon[dragVertex.current.index] = at
                redraw((n) => n + 1)
                return
            }
            if (stroke.current && event.buttons & 1) {
                const path = stroke.current.path
                if (stroke.current.tool === "line" || stroke.current.tool === "straighten") {
                    stroke.current.path = [path[0], at]
                } else {
                    const last = path[path.length - 1]
                    const spacing = Math.max(0.02, (stroke.current.tool === "erase" ? modifyTool.get().radius : modifyTool.get().width) / 3)
                    if (Math.hypot(at[0] - last[0], at[1] - last[1]) >= spacing) {
                        path.push(at)
                    }
                }
                redraw((n) => n + 1)
                return
            }
            if (panning.current && event.buttons) {
                autoFit.current = false
                const v = view.current!
                v.cx = panning.current.cx - (x - panning.current.x) / v.pixelsPerMeter
                v.cy = panning.current.cy + (y - panning.current.y) / v.pixelsPerMeter
                redraw((n) => n + 1)
                persist()
            } else if (planTool.get().tool === "area" && current().ui.tool === "places") {
                planTool.update({ hover: at })
            } else if (current().ui.tool === "polygon") {
                polygonTool.update({ hover: at })
            }
        }
        const up = (event: MouseEvent) => {
            if (!press.current) {
                return
            }
            const [x, y] = local(event)
            const moved = Math.hypot(x - press.current.x, y - press.current.y)
            press.current = null
            panning.current = null
            if (minimap || event.button !== 0) {
                return
            }
            if (dragVertex.current) {
                const done = dragVertex.current
                dragVertex.current = null
                if (moved > 2) {
                    run(api.patch(session.id, done.id, { polygon: done.polygon }))
                }
                redraw((n) => n + 1)
                return
            }
            if (stroke.current) {
                const done = stroke.current
                stroke.current = null
                sendStroke(done).finally(() => redraw((n) => n + 1))
                return
            }
            if (moved > 5) {
                return
            }
            const at = toWorld(x, y)
            const which = tool()
            if (which === "plan-point") {
                run(api.add(session.id, { type: "planPoint", floor: storey, name: planTool.get().name.trim() || "Spot", position: at }), "Point added (⌘Z to undo)")
            } else if (which === "plan-area") {
                planTool.update({ draft: [...planTool.get().draft, at] })
            } else if (which === "polygon") {
                polygonTool.update({ draft: [...polygonTool.get().draft, at] })
            } else if (which === "select") {
                const hit = [...prisms].reverse().find((p) => inside(p.polygon, at[0], at[1]))
                scene.select(hit ? { kind: "prism", id: hit.id } : null)
            }
        }
        const wheel = (event: WheelEvent) => {
            event.preventDefault()
            if (minimap) {
                return
            }
            const [x, y] = local(event)
            const before = toWorld(x, y)
            const v = view.current!
            autoFit.current = false
            v.pixelsPerMeter = Math.min(2000, Math.max(2, v.pixelsPerMeter * Math.exp(-event.deltaY * 0.0015)))
            const after = toWorld(x, y)
            v.cx += before[0] - after[0]
            v.cy += before[1] - after[1]
            redraw((n) => n + 1)
            persist()
        }
        const key = (event: KeyboardEvent) => {
            if (minimap || (event.target as HTMLElement).closest?.("input, textarea, select")) {
                return
            }
            const plan = planTool.get()
            const poly = polygonTool.get()
            if (event.key === "Enter" && current().ui.tool === "polygon") {
                finishPolygon("prism")
            } else if (event.key === "Enter" && plan.tool === "area" && current().ui.tool === "places") {
                finishPolygon("plan")
            } else if (event.key === "Escape" && (plan.draft.length || poly.draft.length || stroke.current)) {
                // a drawing in progress is cancelled first; the next Esc puts the tool down (App)
                event.stopPropagation()
                planTool.update({ draft: [] })
                polygonTool.update({ draft: [], hover: null })
                stroke.current = null
                redraw((n) => n + 1)
            } else if (event.key === "Backspace" && (plan.draft.length || poly.draft.length)) {
                event.stopPropagation()
                if (poly.draft.length) {
                    polygonTool.update({ draft: poly.draft.slice(0, -1) })
                } else {
                    planTool.update({ draft: plan.draft.slice(0, -1) })
                }
            }
        }
        const double = () => {
            if (current().ui.tool === "polygon") {
                finishPolygon("prism")
            } else if (planTool.get().tool === "area" && current().ui.tool === "places") {
                finishPolygon("plan")
            }
        }
        const menu = (event: Event) => event.preventDefault()
        element.addEventListener("mousedown", down)
        window.addEventListener("mousemove", move)
        window.addEventListener("mouseup", up)
        element.addEventListener("wheel", wheel, { passive: false })
        element.addEventListener("dblclick", double)
        element.addEventListener("contextmenu", menu)
        window.addEventListener("keydown", key, true)
        return () => {
            element.removeEventListener("mousedown", down)
            window.removeEventListener("mousemove", move)
            window.removeEventListener("mouseup", up)
            element.removeEventListener("wheel", wheel)
            element.removeEventListener("dblclick", double)
            element.removeEventListener("contextmenu", menu)
            window.removeEventListener("keydown", key, true)
        }
    })

    useEffect(() => {
        if (minimap) {
            return
        }
        const fit = () => {
            view.current = null
            autoFit.current = true
            redraw((n) => n + 1)
            setUi({ view2d: null })
        }
        // a saved view brings its own camera
        const apply = (event: Event) => {
            const saved = (event as CustomEvent<SavedView>).detail
            if (saved.center && saved.pixelsPerMeter) {
                view.current = { cx: saved.center[0], cy: saved.center[1], pixelsPerMeter: saved.pixelsPerMeter }
                autoFit.current = false
                persist()
                redraw((n) => n + 1)
            }
        }
        window.addEventListener(FIT_2D, fit)
        window.addEventListener(APPLY_2D, apply)
        return () => {
            window.removeEventListener(FIT_2D, fit)
            window.removeEventListener(APPLY_2D, apply)
        }
    }, [minimap, setUi, persist])

    useEffect(() => {
        const box = host.current
        if (!box) {
            return
        }
        const observer = new ResizeObserver(() => redraw((n) => n + 1))
        observer.observe(box)
        return () => observer.disconnect()
    }, [])

    const cursor = minimap ? "pointer" : ["erase", "brush", "line", "straighten", "polygon", "places"].includes(ui.tool) ? "crosshair" : "grab"
    void cameraTick
    return (
        <div className={minimap ? "minimap-view" : "plan-view"} ref={host} data-view2d={kind} style={{ cursor }}>
            <canvas ref={canvas} />
            {heightImage && !minimap && (
                <div className="height-legend">
                    floor height {heightImage.range[0].toFixed(2)} … {heightImage.range[1].toFixed(2)} m <span className="ramp" />
                </div>
            )}
        </div>
    )
}
