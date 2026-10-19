// The Map Editor page: a map viewer, [ 3D | Split | 2D ], with two orbs over it: Generate (the build settings, the only
// step needed first) and Edit (a palette of tools: erase, draw, straighten walls, polygons, named points and areas,
// 3D boxes, cleanup, saved 2D views), used in any order. All state lives server-side (the session), so a refresh comes
// back to the same recording, view, tool, map, edits, cameras and selection; running jobs keep running and the page
// reattaches to their progress.
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react"
import { api, events, recordings, type Job, type LastSession, type Session } from "./core/api.ts"
import { MapScene } from "./core/scene.ts"
import type { FloorModel } from "./core/slice.ts"
import { DEFAULT_UI, restoreUi, TOOLS, type Context, type Modal as ModalName, type ToolId, type UiState, type ViewMode } from "./ui/context.ts"
import { OpenPanel } from "./ui/OpenPanel.tsx"
import { CleanPanel } from "./ui/CleanPanel.tsx"
import { AnnotatePanel } from "./ui/AnnotatePanel.tsx"
import { PlacesPanel } from "./ui/PlacesPanel.tsx"
import { PolygonPanel } from "./ui/PolygonPanel.tsx"
import { ModifyPanel } from "./ui/ModifyPanel.tsx"
import { ViewsPanel } from "./ui/ViewsPanel.tsx"
import { GenerateModal } from "./ui/GenerateModal.tsx"
import { SavePanel } from "./ui/SavePanel.tsx"
import { FIT_2D, View2D } from "./ui/View2D.tsx"
import { SliceBar } from "./ui/SliceBar.tsx"
import { JobCard } from "./ui/JobCard.tsx"
import { SlicerWizard } from "./ui/SlicerWizard.tsx"
import { useUploads, isActive } from "./ui/useUploads.ts"
import { UploadsPanel, overallFraction } from "./ui/UploadsPanel.tsx"
import { FloorPicker } from "./ui/FloorPicker.tsx"
import { ShareMenu, SharePanel } from "./ui/SharePanel.tsx"
import { notify } from "./dim-app/source/notify.js"
import { Alert, AppShell, Badge, Button, EmptyState, Fab, Hud, Icon, IconButton, Modal, Panel, ProgressRing, Row, Segmented, Spinner, toast, Toolbar, TopBar, useMobile } from "dimos-react"
import { SceneControls, SceneStats, Scene3D, type Viewer } from "dimos-react/three"

const MODES: { id: ViewMode; label: string }[] = [
    { id: "3d", label: "3D" },
    { id: "split", label: "Split" },
    { id: "2d", label: "2D" },
]
/** a mouse press on a toolbar button doesn't take the keyboard focus (so later shortcut keys don't ring it) */
const keepFocus = (event: { preventDefault: () => void }) => event.preventDefault()
/** the panes' slide, ms (matches .pane's CSS transition) */
const SLIDE_MS = 300

export function App() {
    const viewBox = useRef<HTMLElement>(null)
    const [scene, setScene] = useState<MapScene | null>(null)
    const [session, setSession] = useState<Session | null>(null)
    /** undefined until the server's state is in; then the recording open before the last start, or null */
    const [last, setLast] = useState<LastSession | null | undefined>(undefined)
    const [ui, setUiState] = useState<UiState>(DEFAULT_UI)
    const [modal, setModal] = useState<ModalName>(null)
    const [slicing, setSlicing] = useState(false)
    const slicingRef = useRef(false)
    slicingRef.current = slicing
    /** the slicer's step; its align step (2) is done in the 2D view, the 3D camera held still */
    const [slicerStep, setSlicerStep] = useState(0)
    const aligning = slicing && slicerStep === 2
    const aligningRef = useRef(false)
    aligningRef.current = aligning
    const [connected, setConnected] = useState(true)
    const mobile = useMobile(760)
    const [floor, setFloor] = useState<FloorModel | null>(null)
    const sessionRef = useRef<Session | null>(null)
    const uiRef = useRef(ui)
    uiRef.current = ui
    sessionRef.current = session
    const loaded = useRef<{ id: string; mapVersion: number } | null>(null)
    const restoredCamera = useRef<string | null>(null)
    /** the floor key (floorKeyOf) the floor model was last fetched for */
    const floorFor = useRef("")

    const say = useCallback((text: string, error = false) => {
        toast({ message: text, tone: error ? "danger" : "info", durationMs: error ? 7000 : 3200 })
    }, [])

    const run = useCallback(
        async <T,>(action: Promise<T>, done?: string | ((value: T) => string)) => {
            try {
                const value = await action
                if (done) {
                    say(typeof done === "string" ? done : done(value))
                }
                return value
            } catch (error) {
                say(String((error as Error).message ?? error), true)
                return undefined
            }
        },
        [say],
    )

    const uploads = useUploads(say)

    // a save job this page watched run → a Desktop notification when it finishes (⌘S, the Save panel or the agent)
    const watchedSave = useRef<number | null>(null)
    useEffect(() => {
        const job = session?.job
        if (!session || job?.kind !== "save") {
            return
        }
        if (job.state === "running") {
            watchedSave.current = job.id
        } else if (watchedSave.current === job.id) {
            watchedSave.current = null
            if (job.state === "done") {
                notify({ title: "Map saved", body: `Saved into ${session.name}`, kind: "ok" })
            } else if (job.state === "failed") {
                notify({ title: "Map save failed", body: job.error ?? `Couldn't save into ${session.name}`, kind: "warn" })
            }
        }
    }, [session])


    /** uploads the open recording to Dimensional cloud (the backend saves first if asked, then queues it in Desktop) */
    const uploadRecording = useCallback(
        async (saveFirst: boolean) => {
            const current = sessionRef.current
            if (!current) {
                return
            }
            setModal(null)
            await uploads.upload(async () => {
                const result = await api.upload(current.id, saveFirst)
                return result.upload ? `Queued ${result.upload.name} for upload` : "Saving into the recording, then uploading…"
            })
        },
        [uploads.upload],
    )

    // the scene lives for the page's lifetime, on dimos-react's 3D view
    const onViewer = useCallback((viewer: Viewer) => {
        const created = new MapScene(viewer)
        setScene(created)
        // reachable from the console and the end-to-end tests
        ;(window as unknown as { mapBuilderScene: MapScene }).mapBuilderScene = created
    }, [])

    /** UI state changes go to the server with the camera (debounced), so a refresh restores them */
    const pushView = useMemo(() => {
        let timer = 0
        return () => {
            clearTimeout(timer)
            timer = window.setTimeout(() => {
                const current = sessionRef.current
                if (!current || !scene) {
                    return
                }
                const view = { ...scene.viewState(), ui: { ...uiRef.current, region: scene.region, selected: scene.selected } }
                api.view(current.id, view).catch(() => {})
            }, 250)
        }
    }, [scene])

    const setUi = useCallback(
        (patch: Partial<UiState>) => {
            setUiState((current) => ({ ...current, ...patch }))
            pushView()
        },
        [pushView],
    )

    /** picks a palette tool; a 2D tool brings up the 2D view, a 3D one the 3D view */
    const pickTool = useCallback(
        (tool: ToolId) => {
            const spec = TOOLS.find((t) => t.id === tool)
            const mode = uiRef.current.mode
            const next: Partial<UiState> = { tool, paletteOpen: true }
            if (spec?.view === "2d" && mode !== "2d") {
                next.mode = "2d"
            } else if (spec?.view === "3d" && mode === "2d") {
                next.mode = "3d"
            }
            if (scene) {
                scene.onPick = null
            }
            setUi(next)
        },
        [setUi, scene],
    )

    /** reloads the session summary, and the map / paths when they changed */
    const refreshOnce = useCallback(async () => {
        const current = sessionRef.current
        if (!current || !scene) {
            return
        }
        const fresh = await api.session(current.id)
        const last = loaded.current
        const mapChanged = !last || last.id !== fresh.id || last.mapVersion !== fresh.mapVersion
        const withMap = fresh.stage === "map" && mapChanged
        if (withMap) {
            // the floor model comes with the voxels (not from the floor effect), so the 2D view re-rasters once per edit
            floorFor.current = floorKeyOf(fresh)
        }
        setSession(fresh)
        if (withMap) {
            const [points, paths, model] = await Promise.all([api.points(fresh.id), api.paths(fresh.id), api.floor(fresh.id).catch(() => null)])
            scene.setMap(points, fresh.voxelSize ?? 0.05)
            scene.setPaths(paths)
            scene.setRaw(null, null)
            if (model) {
                setFloor(model)
            } else {
                floorFor.current = ""
            }
        }
        loaded.current = { id: fresh.id, mapVersion: fresh.mapVersion }
        scene.setAnnotations(fresh.annotations)
        // the saved slice (the slicer shows its own draft while it's open)
        if (!slicingRef.current && JSON.stringify(scene.slice) !== JSON.stringify(fresh.annotations.slice ?? null)) {
            scene.setSlice(fresh.annotations.slice ?? null)
        }
    }, [scene])
    /** one refresh at a time: a call while one runs makes one more run after it (a stroke's own refresh and its
     * session event then fetch the map once, not twice) */
    const refreshing = useRef<{ running: Promise<void> | null; again: boolean }>({ running: null, again: false })
    const refresh = useCallback(async (): Promise<void> => {
        const state = refreshing.current
        if (state.running) {
            state.again = true
            return state.running
        }
        state.running = (async () => {
            try {
                do {
                    state.again = false
                    await refreshOnce()
                } while (state.again)
            } finally {
                state.running = null
            }
        })()
        return state.running
    }, [refreshOnce])

    const adopt = useCallback(
        async (fresh: Session) => {
            if (!scene) {
                return
            }
            sessionRef.current = fresh
            setSession(fresh)
            loaded.current = null
            scene.setMap(new Float32Array(0), 0.05)
            scene.setPreview(null)
            const saved = (fresh.view?.ui ?? {}) as Partial<UiState>
            const restoredUi = restoreUi(saved)
            setUiState(restoredUi)
            scene.applyLook(restoredUi.look)
            scene.showPaths(restoredUi.showPaths)
            scene.pathsOnTop(restoredUi.pathsOnTop)
            scene.setRegion(restoredUi.region)
            await refresh()
            // a recording with its own global_map offers it in Generate: no raw preview to read meanwhile (a running
            // job would hold the shortcut up)
            if (fresh.stage !== "map") {
                setModal("generate")
            }
            if (fresh.stage !== "map" && !fresh.recordedMap) {
                const preview = await api.preview(fresh.id).catch(() => null)
                if (preview) {
                    scene.setRaw(preview.points, preview.path)
                }
            }
            const camera = (fresh.view as { camera?: { position: number[]; target: number[] } } | null)?.camera
            if (camera && restoredCamera.current !== fresh.id) {
                scene.viewer.controls.target.set(camera.target[0], camera.target[1], camera.target[2])
                scene.viewer.camera.position.set(camera.position[0], camera.position[1], camera.position[2])
                scene.viewer.controls.update()
                scene.viewer.requestRender()
            } else if (restoredCamera.current !== fresh.id) {
                scene.frameMap()
            }
            restoredCamera.current = fresh.id
            if (restoredUi.selected && ["box", "plane", "point", "region", "prism"].includes(restoredUi.selected.kind)) {
                scene.select(restoredUi.selected as never)
            }
        },
        [scene, refresh],
    )

    const openRecording = useCallback(
        async (recording: { id: string; path: string; name: string; writable?: boolean }) => {
            const fresh = await run(api.open(recording))
            if (fresh) {
                setModal(null)
                await adopt({ ...fresh, view: fresh.view ?? null })
            }
        },
        [run, adopt],
    )

    // a .db / .mcap dropped on the page opens, ready to Generate: the one Desktop already has (same name and size), else
    // it's streamed into Desktop's recordings folder first (a browser hands over the bytes, not the file's path)
    const [dropping, setDropping] = useState(false)
    useEffect(() => {
        const isFile = (event: DragEvent) => !!event.dataTransfer && [...event.dataTransfer.types].includes("Files")
        const over = (event: DragEvent) => {
            if (isFile(event)) {
                event.preventDefault()
                setDropping(true)
            }
        }
        const leave = (event: DragEvent) => {
            if (!event.relatedTarget) {
                setDropping(false)
            }
        }
        const drop = async (event: DragEvent) => {
            if (!isFile(event)) {
                return
            }
            event.preventDefault()
            setDropping(false)
            const file = [...(event.dataTransfer?.files ?? [])].find((f) => /\.(db|mcap)$/i.test(f.name))
            if (!file) {
                say("Drop a recording: a .db or an .mcap", true)
                return
            }
            const listed = await run(recordings.list())
            const known = listed?.recordings.find((r) => r.name === file.name || r.path.endsWith(`/${file.name}`))
            if (known && known.size === file.size) {
                await openRecording({ id: known.id, path: known.path, name: known.name, writable: known.writable })
                setModal("generate")
                return
            }
            const format = /\.mcap$/i.test(file.name) ? "mcap" : "db"
            const target = await run(recordings.create(file.name.replace(/\.(db|mcap)$/i, ""), format))
            if (!target) {
                return
            }
            say(`Copying ${file.name} (${(file.size / 1e6).toFixed(0)} MB) into Desktop's recordings…`)
            const copied = await run(api.importFile(target.path, file))
            if (copied) {
                await openRecording({ id: target.id, path: copied.path, name: file.name, writable: true })
                setModal("generate")
            }
        }
        window.addEventListener("dragover", over)
        window.addEventListener("dragleave", leave)
        window.addEventListener("drop", drop)
        return () => {
            window.removeEventListener("dragover", over)
            window.removeEventListener("dragleave", leave)
            window.removeEventListener("drop", drop)
        }
    }, [run, say, openRecording])

    // on load: go back to what's open since the server started (a refresh, Recordings' Open), else ask for a recording
    useEffect(() => {
        if (!scene) {
            return
        }
        api.state()
            .then((state) => {
                setLast(state.last ?? null)
                return state.session ? adopt(state.session) : undefined
            })
            .catch((error) => say(String(error), true))
    }, [scene, adopt, say])

    // the local floor follows the map and the storeys (refresh fetches it with a new map; this catches the rest)
    const floorKey = session ? floorKeyOf(session) : ""
    useEffect(() => {
        if (!floorKey || !session) {
            floorFor.current = ""
            setFloor(null)
            return
        }
        if (floorFor.current === floorKey) {
            return
        }
        floorFor.current = floorKey
        let cancelled = false
        api.floor(session.id)
            .then((model) => !cancelled && setFloor(model))
            .catch(() => {})
        return () => {
            cancelled = true
        }
    }, [floorKey])

    // server events
    useEffect(() => {
        if (!scene) {
            return
        }
        return events(async (event) => {
            const current = sessionRef.current
            if (event.type === "connected") {
                setConnected(true)
                if (current) {
                    refresh().catch(() => {})
                }
            } else if (event.type === "disconnected") {
                setConnected(false)
            } else if (event.type === "job" && current && event.job.session === current.id) {
                const job = event.job as Job
                setSession((s) => (s ? { ...s, job } : s))
                if (job.state === "done" && (job.kind === "build" || job.kind === "globalMap" || job.kind === "recordedMap")) {
                    say(job.kind === "globalMap" ? "Global map generated and written into the recording" : job.kind === "recordedMap" ? "The recording's global_map is the map" : "Map generated")
                    setModal(null)
                    await refresh()
                    scene.frameMap()
                } else if (job.state === "done" && (job.kind === "tags" || job.kind === "tf")) {
                    say(job.kind === "tags" ? "Fiducial markers written into the recording" : "Corrected tf written into the recording")
                    await refresh()
                } else if (job.state === "done" && (job.kind === "open" || job.kind === "read")) {
                    // a recording with no index was read in the background (its saved map), or its tf: now the preview,
                    // which waited for it
                    await refresh()
                    const fresh = await api.session(current.id).catch(() => null)
                    const preview = fresh && fresh.stage !== "map" ? await api.preview(current.id).catch(() => null) : null
                    if (preview) {
                        scene.setRaw(preview.points, preview.path)
                        scene.frameMap()
                    }
                } else if (job.state === "done" && job.kind === "save") {
                    say("Saved into the recording")
                    await refresh()
                } else if (job.state === "failed") {
                    say(`${job.kind} failed: ${job.error}`, true)
                }
            } else if (event.type === "preview" && current && event.id === current.id && current.stage !== "map") {
                const preview = await api.preview(current.id).catch(() => null)
                if (preview) {
                    scene.setRaw(preview.points, preview.path)
                    scene.frameMap()
                }
            } else if (event.type === "session" && current && event.id === current.id) {
                await refresh()
            } else if (event.type === "capture") {
                const image = scene.capture(event.options ?? {})
                api.deliverCapture(event.request, image)
            } else if (event.type === "ui") {
                // an agent changed what the page shows (PATCH api/ui)
                const patch = event.patch as Partial<UiState>
                setUiState((ui) => ({ ...ui, ...patch, look: { ...ui.look, ...(patch.look ?? {}) }, showPaths: { ...ui.showPaths, ...(patch.showPaths ?? {}) } }))
                if (patch.look) {
                    scene.applyLook(patch.look)
                }
                if (patch.showPaths) {
                    scene.showPaths({ ...uiRef.current.showPaths, ...patch.showPaths })
                }
            } else if (event.type === "upload") {
                if (event.error) {
                    say(`Couldn't queue the upload: ${event.error}`, true)
                } else {
                    uploads.refresh()
                }
            } else if (event.type === "setView") {
                scene.lookAt(event.target, event.distance ?? undefined, !!event.topDown)
            } else if (event.type === "opened" && event.id !== current?.id) {
                // an agent opened a recording: follow it
                const state = await api.state().catch(() => null)
                if (state?.session) {
                    setModal(null)
                    await adopt(state.session)
                }
            } else if (event.type === "discarded" && current && event.id === current.id) {
                setSession(null)
                setModal("open")
            }
        })
    }, [scene, refresh, say, setUi, uploads.refresh])

    // scene → UI: selection and edits from the gizmo, camera moves
    useEffect(() => {
        if (!scene) {
            return
        }
        scene.onSelect = (selection) => {
            setUiState((current) => ({ ...current, selected: selection, region: scene.region }))
            pushView()
        }
        scene.onViewChange = pushView
        scene.onEdit = (selection, change) => {
            const current = sessionRef.current
            if (!current) {
                return
            }
            if (selection.kind === "region") {
                setUi({ region: scene.region })
            } else if (selection.kind === "box") {
                run(api.patch(current.id, selection.id, { box: { center: change.center, size: change.size, yaw: change.yaw } }))
            } else if (selection.kind === "point") {
                run(api.patch(current.id, selection.id, { position: change.center }))
            } else if (selection.kind === "plane") {
                run(api.patch(current.id, selection.id, { center: change.center, normal: change.normal, size: [change.size[0], change.size[1]] }))
            } else if (selection.kind === "prism") {
                const prism = current.annotations.prisms?.find((p) => p.id === selection.id)
                if (prism) {
                    // the handle sits on the top face: dragging it sets the height
                    run(api.patch(current.id, selection.id, { height: Math.max(0.05, +(change.center[2] - prism.base).toFixed(3)) }))
                }
            }
        }
    }, [scene, pushView, run, setUi])

    // keyboard shortcuts
    useEffect(() => {
        const onKey = (event: KeyboardEvent) => {
            const target = event.target as HTMLElement
            if (target.closest?.("input, textarea, select")) {
                return
            }
            const current = sessionRef.current
            const mod = event.metaKey || event.ctrlKey
            if (mod && event.key.toLowerCase() === "z" && current) {
                event.preventDefault()
                run(event.shiftKey ? api.redo(current.id) : api.undo(current.id), (r: { undone?: string | null; redone?: string | null }) => (r.undone ? `Undid: ${r.undone}` : r.redone ? `Redid: ${r.redone}` : "Nothing to undo"))
                return
            }
            if (mod && event.key.toLowerCase() === "y" && current) {
                event.preventDefault()
                run(api.redo(current.id), (r) => (r.redone ? `Redid: ${r.redone}` : "Nothing to redo"))
                return
            }
            if (mod && event.key.toLowerCase() === "s" && current) {
                event.preventDefault()
                run(api.save(current.id), "Saving into the recording…")
                return
            }
            if (mod || event.altKey) {
                return
            }
            const aligning = aligningRef.current
            const hasMap = current?.stage === "map" && !aligning
            const mode = aligning ? "2d" : uiRef.current.mode
            const tool = TOOLS.find((t) => t.key === event.key && t.key.length === 1)
            if (event.key === "v" && hasMap) {
                setUi({ mode: mode === "2d" ? "3d" : "2d" })
            } else if (event.key === "m" && hasMap) {
                setUi({ mode: mode === "split" ? "3d" : "split" })
            } else if (event.key === "e" && hasMap) {
                setUi({ paletteOpen: !uiRef.current.paletteOpen })
            } else if (tool && hasMap) {
                pickTool(tool.id)
            } else if (event.key === "f") {
                if (mode === "2d") {
                    window.dispatchEvent(new Event(FIT_2D))
                } else {
                    scene?.frameMap()
                }
            } else if (event.key === "t" && mode !== "2d") {
                scene?.topDown()
            } else if (mode !== "2d" && (event.key === "g" || event.key === "r" || event.key === "s")) {
                scene?.setGizmoMode(event.key === "g" ? "translate" : event.key === "r" ? "rotate" : "scale")
            } else if (event.key === "Escape") {
                if (modal) {
                    setModal(null)
                } else if (uiRef.current.tool !== "select") {
                    pickTool("select")
                } else {
                    scene?.select(null)
                }
                if (scene) {
                    scene.onPick = null
                }
            } else if ((event.key === "Delete" || event.key === "Backspace") && current && scene?.selected && scene.selected.kind !== "region") {
                const selected = scene.selected
                scene.select(null)
                run(api.remove(current.id, selected.id), "Deleted (⌘Z to undo)")
            }
        }
        window.addEventListener("keydown", onKey)
        return () => window.removeEventListener("keydown", onKey)
    }, [scene, run, setUi, pickTool, modal])

    const context: Context = { session, scene, floor, ui, setUi, pickTool, setModal, run, refresh, openRecording, uploads, uploadRecording }
    const activeUploads = uploads.list.filter(isActive).length
    const uploadFraction = overallFraction(uploads.list)
    const uploadFailed = uploads.list.some((u) => u.state === "failed")
    const hasMap = session?.stage === "map"
    const job = session?.job
    const running = job?.state === "running" ? job : null
    // the slicer's align step shows the 2D view whatever the mode (the mode itself is kept for after)
    const mode: ViewMode = aligning ? "2d" : hasMap ? ui.mode : "3d"

    // a hidden 3D view draws nothing, and map edits reach its points when it shows again
    useEffect(() => {
        scene?.setShown(mode !== "2d")
    }, [scene, mode])
    // the 3D view shows the picked floor alone (its band in the floor model), or every floor; the slicer sees them all
    const floorBand = ui.floorOnly && !slicing ? floor?.storeys[Math.min(ui.planFloor, floor.storeys.length - 1)]?.band ?? null : null
    useEffect(() => {
        scene?.setFloorBand(floorBand ? [floorBand[0], floorBand[1]] : null)
    }, [scene, floorBand?.[0], floorBand?.[1]])

    // the panes: 3D on the left, 2D on the right; their widths slide between modes
    const [width, setWidth] = useState(0)
    useLayoutEffect(() => {
        const box = viewBox.current
        if (!box) {
            return
        }
        const observer = new ResizeObserver(() => setWidth(box.clientWidth))
        observer.observe(box)
        setWidth(box.clientWidth)
        return () => observer.disconnect()
    }, [])
    const share = Math.min(0.7, Math.max(0.2, ui.splitShare))
    const target = mode === "3d" ? [width, 0] : mode === "2d" ? [0, width] : [Math.round(width * (1 - share)), Math.round(width * share)]
    // during a slide each pane's content keeps the larger of its old and new width (no resize, no blank frame);
    // it settles to the new width when the slide ends. A hidden pane keeps its last width.
    const [inner, setInner] = useState<[number, number]>([0, 0])
    const settled = useRef<[number, number]>([0, 0])
    const [sliding, setSliding] = useState(false)
    useLayoutEffect(() => {
        const previous = settled.current
        const next: [number, number] = [target[0] || previous[0] || width, target[1] || previous[1] || width]
        if (previous[0] === 0 && previous[1] === 0) {
            settled.current = next
            setInner(next)
            return
        }
        setInner([Math.max(previous[0], next[0]), Math.max(previous[1], next[1])])
        setSliding(true)
        const timer = window.setTimeout(() => {
            settled.current = next
            setInner(next)
            setSliding(false)
        }, SLIDE_MS + 30)
        return () => window.clearTimeout(timer)
    }, [target[0], target[1]])
    const twoDKind = mode === "split" ? "minimap" : "main"
    const toolSpec = TOOLS.find((t) => t.id === ui.tool)

    const modeItems = MODES.map((m) => ({
        value: m.id,
        label: m.label,
        disabled: !hasMap || aligning,
        title: m.id === "2d" ? "2D slice (V)" : m.id === "split" ? "3D with a 2D map beside it (M)" : "3D (V)",
        props: { "data-mode": m.id, onMouseDown: keepFocus },
    }))

    const topBar = (
        <TopBar
            // inside Desktop, its window bar already names the app
            title={self === top ? "Map Editor" : undefined}
            left={
                <Button
                    size="sm"
                    variant="ghost"
                    icon={session ? "chevron-down" : undefined}
                    title={session ? `${session.recordingPath} (open another)` : "Pick a recording"}
                    onClick={() => setModal("open")}
                    data-action="open"
                    style={{ maxWidth: "26vw", overflow: "hidden", textOverflow: "ellipsis" }}
                >
                    {session ? session.name : "Pick a recording…"}
                </Button>
            }
            right={
                <>
                    {session && (
                        <>
                            <IconButton icon="rotate-left" size="sm" variant="default" disabled={!session.undoLabel} label={session.undoLabel ? `Undo: ${session.undoLabel} (⌘Z)` : "Nothing to undo"} onClick={() => run(api.undo(session.id))} />
                            <IconButton icon="rotate-right" size="sm" variant="default" disabled={!session.redoLabel} label={session.redoLabel ? `Redo: ${session.redoLabel} (⇧⌘Z)` : "Nothing to redo"} onClick={() => run(api.redo(session.id))} />
                            {hasMap && (
                                <Button
                                    size="sm"
                                    variant={session.unsaved ? "primary" : "default"}
                                    icon={session.unsaved ? undefined : "check"}
                                    onClick={() => setModal("save")}
                                    title={session.savedAt ? `last saved ${new Date(session.savedAt * 1000).toLocaleString()}` : "not saved into the recording yet"}
                                    data-action="save-menu"
                                >
                                    {session.unsaved ? "Save" : "Saved"}
                                </Button>
                            )}
                            <Button
                                size="sm"
                                icon="upload"
                                onClick={() => (session.unsaved && hasMap ? setModal("save") : uploadRecording(false))}
                                title={session.unsaved && hasMap ? "Upload to Dimensional cloud (save first, or upload as last saved)" : `Upload ${session.name} to Dimensional cloud`}
                                data-action="upload"
                            >
                                Upload
                            </Button>
                            {hasMap && <ShareMenu disabled={slicing} onHtml={() => setModal("share")} />}
                        </>
                    )}
                    {(uploads.list.length > 0 || uploads.waitingForLogin) && (
                        <Button
                            size="sm"
                            variant={uploadFailed ? "danger" : "default"}
                            pressed={uploads.panelOpen}
                            onClick={() => uploads.setPanelOpen(!uploads.panelOpen)}
                            title={activeUploads ? `${activeUploads} upload${activeUploads > 1 ? "s" : ""} in progress` : "Uploads"}
                            data-action="uploads-panel"
                        >
                            {activeUploads > 0
                                ? uploadFraction == null
                                    ? <Spinner size={16} label="uploading" />
                                    : <ProgressRing value={uploadFraction} size={20} stroke={2.5}><Icon name="upload" size={11} /></ProgressRing>
                                : <Icon name="upload" size={14} />}
                            {activeUploads > 0 ? <Badge tone="info" solid data-upload-count>{activeUploads}</Badge> : uploadFailed ? <Icon name="warn" size={14} /> : null}
                        </Button>
                    )}
                </>
            }
        >
            <Segmented items={modeItems} value={mode} onChange={(next: ViewMode) => setUi({ mode: next })} label="view" data-mode-switch={mode} />
        </TopBar>
    )

    return (
        <AppShell topBar={topBar} className={sliding ? "sliding" : undefined} data-mode={mode}>
            {dropping && <div className="drop-overlay" data-drop-overlay>Drop a .db or .mcap recording to open it</div>}
            <section className="view" ref={viewBox}>
                <div className="pane pane-3d" style={{ width: target[0] }}>
                    <div className="pane-inner" style={{ width: inner[0] }}>
                        <Scene3D fixedFrame="world" onViewer={onViewer} className="scene3d">
                            <SceneStats />
                            <SceneControls position="top-right" onFrameAll={() => scene?.frameMap()} onTopDown={() => scene?.topDown()} />
                        </Scene3D>
                    </div>
                </div>
                <div className="pane pane-2d" style={{ width: target[1] }}>
                    <div className="pane-inner" style={{ width: inner[1] }}>
                        {hasMap && <View2D key={twoDKind} context={context} kind={twoDKind} aligning={aligning} />}
                        {hasMap && mode === "2d" && !aligning && <SliceBar context={context} />}
                        {hasMap && mode === "2d" && (
                            <Hud position="top-right">
                                <IconButton icon="fit" variant="default" label="Frame the map (F)" onClick={() => window.dispatchEvent(new Event(FIT_2D))} />
                            </Hud>
                        )}
                    </div>
                </div>
                {mode === "split" && <SplitDivider context={context} width={width} />}

                <Hud position="top-left">
                    <Row gap={12}>
                        <Fab icon="edit" label={hasMap ? "Edit tools (E)" : "Generate the map first"} active={ui.paletteOpen} disabled={!hasMap} onMouseDown={keepFocus} onClick={() => setUi({ paletteOpen: !ui.paletteOpen })} data-orb="edit" />
                        <Fab
                            icon="refresh"
                            tone="info"
                            label={hasMap ? "Map generation settings (regenerate)" : "Generate the map"}
                            attention={running?.kind === "build" ? "spin" : session && !hasMap ? "pulse" : undefined}
                            disabled={!session}
                            onMouseDown={keepFocus}
                            onClick={() => setModal("generate")}
                            data-orb="generate"
                        />
                        <Fab
                            icon="layers"
                            tone="warn"
                            label={hasMap ? "Slicer: height band, alignment and crop (a view, not an edit)" : "Generate the map first"}
                            active={slicing}
                            attention={hasMap && !session?.annotations.slice && !slicing ? "glow" : undefined}
                            disabled={!hasMap}
                            onMouseDown={keepFocus}
                            onClick={() => {
                                if (!slicing) {
                                    setUi({ mode: "3d", paletteOpen: false })
                                }
                                setSlicing(!slicing)
                            }}
                            data-orb="slicer"
                        />
                        {hasMap && !session?.annotations.slice && !slicing && mode === "3d" && !mobile && (
                            <Badge tone="warn" data-slicer-hint>
                                <Icon name="arrow-left" size={12} /> Recommended: slice your map
                            </Badge>
                        )}
                    </Row>
                    {slicing && hasMap && <SlicerWizard context={context} onClose={() => setSlicing(false)} onStep={setSlicerStep} />}
                    {hasMap && ui.paletteOpen && !slicing && (
                        // the row is as tall as its tallest child: only the toolbar and the panel take taps, not the
                        // gaps beside them (over the 2D view, where a finger draws)
                        <Row align="flex-start" gap={10} style={{ pointerEvents: "none" }}>
                            <Toolbar floating vertical aria-label="edit tools" data-palette style={{ pointerEvents: "auto" }}>
                                {TOOLS.map((tool) => (
                                    <IconButton
                                        key={tool.id}
                                        icon={tool.icon}
                                        pressed={ui.tool === tool.id}
                                        onMouseDown={keepFocus}
                                        onClick={() => pickTool(tool.id)}
                                        label={`${tool.label}${tool.key.length === 1 ? ` (${tool.key.toUpperCase()})` : tool.key ? " (Esc)" : ""}`}
                                        data-tool={tool.id}
                                    />
                                ))}
                            </Toolbar>
                            {ui.tool !== "select" && toolSpec && (
                                <Panel
                                    glass
                                    icon={toolSpec.icon}
                                    title={toolSpec.label}
                                    actions={<IconButton icon="close" size="sm" label="Close (Esc)" onClick={() => pickTool("select")} />}
                                    data-tool-panel={ui.tool}
                                    collapsible={mobile}
                                    persistKey={mobile ? "map-builder:tool-panel" : undefined}
                                    style={{ pointerEvents: "auto", width: mobile ? "calc(100vw - 90px)" : 330, maxHeight: "calc(100vh - 190px - var(--dim-inset-bottom, 0px))" }}
                                >
                                    {ui.tool === "clean" && <CleanPanel context={context} />}
                                    {ui.tool === "annotate" && <AnnotatePanel context={context} />}
                                    {ui.tool === "polygon" && <PolygonPanel context={context} />}
                                    {ui.tool === "places" && <PlacesPanel context={context} />}
                                    {["erase", "brush", "line", "straighten"].includes(ui.tool) && <ModifyPanel context={context} />}
                                    {ui.tool === "views" && <ViewsPanel context={context} />}
                                </Panel>
                            )}
                        </Row>
                    )}
                </Hud>
                {hasMap && !slicing && <FloorPicker context={context} mode={mode} />}
                {!session && last !== undefined && modal !== "open" && (
                    <EmptyState
                        layer
                        icon="map"
                        title="Please pick a recording"
                        message="Maps are built from a robot recording with lidar point clouds, from Desktop's shared recordings folder."
                        data-testid="pick-a-recording"
                        actions={
                            <>
                                <Button variant="primary" onClick={() => setModal("open")}>Pick a recording</Button>
                                {last && <Button onClick={() => openRecording({ id: last.recordingId, path: last.path, name: last.name, writable: last.writable })}>Continue {last.name}</Button>}
                            </>
                        }
                    />
                )}
                {(running && modal !== "generate") || !connected
                    ? (
                        <Hud position="top">
                            {!connected && (
                                <Alert tone="danger" data-testid="onboard-backend-down">
                                    The Map Editor server isn't answering; reconnecting… If this stays, close the app (✕) and open it again.
                                </Alert>
                            )}
                            {running && modal !== "generate" && <JobCard job={running} onCancel={() => session && run(api.cancel(session.id), "Cancelling…")} style={{ width: "min(520px, 90vw)" }} />}
                        </Hud>
                    )
                    : null}
                {uploads.panelOpen && <UploadsPanel uploads={uploads} />}
            </section>
            {modal === "generate" && session && <GenerateModal context={context} onClose={() => setModal(null)} />}
            <Modal open={modal === "open"} onClose={() => setModal(null)} title="Pick a recording" data-modal="open">
                <OpenPanel context={context} />
            </Modal>
            <Modal open={modal === "share" && !!session && hasMap} onClose={() => setModal(null)} title="Share as HTML" data-modal="share">
                <SharePanel context={context} />
            </Modal>
            <Modal open={modal === "save" && !!session} onClose={() => setModal(null)} title="Save into the recording" data-modal="save">
                <SavePanel context={context} />
            </Modal>
        </AppShell>
    )
}

/** what the floor model depends on: the map's version and the storeys the user set */
function floorKeyOf(session: Session): string {
    return session.stage === "map" ? `${session.id}:${session.mapVersion}:${session.annotations.floors.map((f) => f.z).join(",")}` : ""
}

/** The split's divider: drag it to give the 2D pane more or less of the width. */
function SplitDivider({ context, width }: { context: Context; width: number }) {
    const { ui, setUi } = context
    const share = Math.min(0.7, Math.max(0.2, ui.splitShare))
    const start = (event: ReactPointerEvent) => {
        event.preventDefault()
        const left = (event.currentTarget.parentElement as HTMLElement).getBoundingClientRect().left
        let latest = share
        const move = (e: PointerEvent) => {
            latest = Math.min(0.7, Math.max(0.2, 1 - (e.clientX - left) / width))
            ;(document.querySelector(".split-divider") as HTMLElement).style.left = `${Math.round(width * (1 - latest))}px`
        }
        const up = () => {
            window.removeEventListener("pointermove", move)
            window.removeEventListener("pointerup", up)
            setUi({ splitShare: +latest.toFixed(3) })
        }
        window.addEventListener("pointermove", move)
        window.addEventListener("pointerup", up)
    }
    return <div className="split-divider" style={{ left: Math.round(width * (1 - share)) }} onPointerDown={start} title="drag to resize" />
}
