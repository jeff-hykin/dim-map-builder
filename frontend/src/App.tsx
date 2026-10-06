// The Map Editor page: a map viewer, [ 3D | Split | 2D ], with two orbs over it: Generate (the build settings, the only
// step needed first) and Edit (a palette of tools: erase, draw, straighten walls, polygons, named points and areas,
// 3D boxes, cleanup, saved 2D views), used in any order. All state lives server-side (the session), so a refresh comes
// back to the same recording, view, tool, map, edits, cameras and selection; running jobs keep running and the page
// reattaches to their progress.
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react"
import { api, events, type Job, type LastSession, type Session } from "./core/api.ts"
import { MapScene } from "./core/scene.ts"
import type { FloorModel } from "./core/slice.ts"
import { DEFAULT_UI, restoreUi, TOOLS, type Context, type Modal, type ToolId, type UiState, type ViewMode } from "./ui/context.ts"
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
import { Icon } from "./ui/Icon.tsx"
import { useUploads, isActive } from "./ui/useUploads.ts"
import { UploadsPanel, overallFraction } from "./ui/UploadsPanel.tsx"
import { LoginDialog } from "./ui/LoginDialog.tsx"
import { FloorPicker } from "./ui/FloorPicker.tsx"
import { ShareMenu, SharePanel } from "./ui/SharePanel.tsx"
import { EmptyState } from "./ui/EmptyState.tsx"
import { notify } from "./dim-app/notify.js"

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
    const host = useRef<HTMLDivElement>(null)
    const viewBox = useRef<HTMLElement>(null)
    const [scene, setScene] = useState<MapScene | null>(null)
    const [session, setSession] = useState<Session | null>(null)
    /** undefined until the server's state is in; then the recording open before the last start, or null */
    const [last, setLast] = useState<LastSession | null | undefined>(undefined)
    const [ui, setUiState] = useState<UiState>(DEFAULT_UI)
    const [modal, setModal] = useState<Modal>(null)
    const [slicing, setSlicing] = useState(false)
    const slicingRef = useRef(false)
    slicingRef.current = slicing
    /** the slicer's step; its align step (2) is done in the 2D view, the 3D camera held still */
    const [slicerStep, setSlicerStep] = useState(0)
    const aligning = slicing && slicerStep === 2
    const aligningRef = useRef(false)
    aligningRef.current = aligning
    const [toast, setToast] = useState<{ text: string; error: boolean } | null>(null)
    const [connected, setConnected] = useState(true)
    const [stats, setStats] = useState("")
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
        setToast({ text, error })
        window.setTimeout(() => setToast((current) => (current?.text === text ? null : current)), error ? 7000 : 3200)
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

    // the scene lives for the page's lifetime
    useEffect(() => {
        if (!host.current) {
            return
        }
        const created = new MapScene(host.current)
        setScene(created)
        // reachable from the console and the end-to-end tests
        ;(window as unknown as { mapBuilderScene: MapScene }).mapBuilderScene = created
        const unsubscribe = created.viewer.stats.subscribe(() => {
            const s = created.viewer.stats.get()
            setStats(`${s.fps} fps · ${(s.points / 1e6).toFixed(2)}M voxels`)
        })
        return unsubscribe
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
            scene.setRegion(restoredUi.region)
            await refresh()
            if (fresh.stage !== "map") {
                setModal("generate")
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
                if (job.state === "done" && job.kind === "build") {
                    say("Map generated")
                    setModal(null)
                    await refresh()
                    scene.frameMap()
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

    return (
        <div className={`app mode-${mode} ${sliding ? "sliding" : ""}`}>
            <header className="topbar">
                <span className="dim-title title">Map Editor</span>
                <button type="button" className="dim-btn sm icon recording-name" title={session ? `${session.recordingPath} (open another)` : "Pick a recording"} onClick={() => setModal("open")} data-action="open">
                    {session ? <><Icon name="chevron-down" /> {session.name}</> : "Pick a recording…"}
                </button>
                <span className="spacer" />
                <div className="dim-tabs mode-switch" role="tablist" aria-label="view" data-mode-switch={mode}>
                    {MODES.map((m) => (
                        <button key={m.id} type="button" role="tab" aria-selected={mode === m.id} className={`dim-tab ${mode === m.id ? "on" : ""}`} disabled={!hasMap || aligning} onMouseDown={keepFocus} onClick={() => setUi({ mode: m.id })} data-mode={m.id} title={m.id === "2d" ? "2D slice (V)" : m.id === "split" ? "3D with a 2D map beside it (M)" : "3D (V)"}>
                            {m.label}
                        </button>
                    ))}
                </div>
                <span className="spacer" />
                {session && (
                    <>
                        <button type="button" className="dim-btn sm icon" disabled={!session.undoLabel} title={session.undoLabel ? `Undo: ${session.undoLabel} (⌘Z)` : "Nothing to undo"} onClick={() => run(api.undo(session.id))}>
                            <Icon name="rotate-left" />
                        </button>
                        <button type="button" className="dim-btn sm icon" disabled={!session.redoLabel} title={session.redoLabel ? `Redo: ${session.redoLabel} (⇧⌘Z)` : "Nothing to redo"} onClick={() => run(api.redo(session.id))}>
                            <Icon name="rotate-right" />
                        </button>
                        {hasMap && (
                            <button type="button" className={`dim-btn sm save ${session.unsaved ? "primary unsaved" : "saved"}`} onClick={() => setModal("save")} title={session.savedAt ? `last saved ${new Date(session.savedAt * 1000).toLocaleString()}` : "not saved into the recording yet"} data-action="save-menu">
                                {session.unsaved ? "Save" : <><Icon name="check" /> Saved</>}
                            </button>
                        )}
                        <button
                            type="button"
                            className="dim-btn sm icon"
                            onClick={() => (session.unsaved && hasMap ? setModal("save") : uploadRecording(false))}
                            title={session.unsaved && hasMap ? "Upload to Dimensional cloud (save first, or upload as last saved)" : `Upload ${session.name} to Dimensional cloud`}
                            data-action="upload"
                        >
                            <Icon name="upload" /> Upload
                        </button>
                        {hasMap && <ShareMenu disabled={slicing} onHtml={() => setModal("share")} />}
                    </>
                )}
                {(uploads.list.length > 0 || uploads.waitingForLogin) && (
                    <button
                        type="button"
                        className={`dim-btn sm icon uploads-button ${uploads.panelOpen ? "on" : ""} ${uploadFailed ? "failed" : ""}`}
                        onClick={() => uploads.setPanelOpen(!uploads.panelOpen)}
                        title={activeUploads ? `${activeUploads} upload${activeUploads > 1 ? "s" : ""} in progress` : "Uploads"}
                        data-action="uploads-panel"
                    >
                        <span className="upload-ring" style={{ ["--done" as string]: uploadFraction ?? 0 }} data-busy={activeUploads > 0 ? (uploadFraction == null ? "spin" : "ring") : undefined}>
                            <Icon name="upload" />
                        </span>
                        {activeUploads > 0 ? <span className="count" data-upload-count>{activeUploads}</span> : uploadFailed ? <Icon name="warn" /> : null}
                    </button>
                )}
            </header>
            <section className="view" ref={viewBox}>
                <div className="pane pane-3d" style={{ width: target[0] }}>
                    <div className="pane-inner" style={{ width: inner[0] }}>
                        <div className="scene" ref={host} />
                        {stats && <div className="dim-panel glass dim-mono view-stats">{stats}</div>}
                        <div className="view-tools">
                            <button type="button" className="dim-btn sm icon" title="Frame the map (F)" onClick={() => scene?.frameMap()}>
                                <Icon name="fullscreen" /> Frame
                            </button>
                            <button type="button" className="dim-btn sm icon" title="Top-down view (T)" onClick={() => scene?.topDown()}>
                                <Icon name="top" /> Top
                            </button>
                        </div>
                    </div>
                </div>
                <div className="pane pane-2d" style={{ width: target[1] }}>
                    <div className="pane-inner" style={{ width: inner[1] }}>
                        {hasMap && <View2D key={twoDKind} context={context} kind={twoDKind} aligning={aligning} />}
                        {hasMap && mode === "2d" && !aligning && <SliceBar context={context} />}
                        {hasMap && mode === "2d" && (
                            <div className="view-tools">
                                <button type="button" className="dim-btn sm icon" title="Frame the map (F)" onClick={() => window.dispatchEvent(new Event(FIT_2D))}>
                                    <Icon name="fullscreen" /> Frame
                                </button>
                            </div>
                        )}
                    </div>
                </div>
                {mode === "split" && <SplitDivider context={context} width={width} />}

                <div className="orbs">
                    <button type="button" className={`orb orb-edit ${ui.paletteOpen ? "open" : ""}`} disabled={!hasMap} onMouseDown={keepFocus} onClick={() => setUi({ paletteOpen: !ui.paletteOpen })} title={hasMap ? "Edit tools (E)" : "Generate the map first"} data-orb="edit">
                        <Icon name="edit" />
                    </button>
                    <button type="button" className={`orb orb-generate ${session && !hasMap ? "next" : ""} ${running?.kind === "build" ? "busy" : ""}`} disabled={!session} onMouseDown={keepFocus} onClick={() => setModal("generate")} title={hasMap ? "Map generation settings (regenerate)" : "Generate the map"} data-orb="generate">
                        <Icon name="refresh" />
                    </button>
                    <button
                        type="button"
                        className={`orb orb-slicer ${slicing ? "open" : ""} ${hasMap && !session?.annotations.slice && !slicing ? "recommend" : ""}`}
                        disabled={!hasMap}
                        onMouseDown={keepFocus}
                        onClick={() => {
                            if (!slicing) {
                                setUi({ mode: "3d", paletteOpen: false })
                            }
                            setSlicing(!slicing)
                        }}
                        title={hasMap ? "Slicer: height band, alignment and crop (a view, not an edit)" : "Generate the map first"}
                        data-orb="slicer"
                    >
                        <Icon name="layers" />
                    </button>
                </div>
                {hasMap && !session?.annotations.slice && !slicing && (
                    <div className="dim-badge warn orb-hint" data-slicer-hint>
                        <Icon name="arrow-left" /> Recommended: slice your map
                    </div>
                )}
                {slicing && hasMap && <SlicerWizard context={context} onClose={() => setSlicing(false)} onStep={setSlicerStep} />}
                {hasMap && !slicing && <FloorPicker context={context} mode={mode} />}
                {hasMap && ui.paletteOpen && !slicing && (
                    <div className="dim-panel glass palette" role="toolbar" aria-label="edit tools" data-palette>
                        {TOOLS.map((tool) => (
                            <button key={tool.id} type="button" className={`dim-btn ghost icon ${ui.tool === tool.id ? "on" : ""}`} onMouseDown={keepFocus} onClick={() => pickTool(tool.id)} title={`${tool.label}${tool.key.length === 1 ? ` (${tool.key.toUpperCase()})` : tool.key ? " (Esc)" : ""}`} data-tool={tool.id}>
                                <Icon name={tool.icon} />
                            </button>
                        ))}
                    </div>
                )}
                {hasMap && ui.paletteOpen && ui.tool !== "select" && toolSpec && (
                    <div className="dim-panel glass tool-panel" data-tool-panel={ui.tool}>
                        <div className="tool-panel-head">
                            <span>
                                <Icon name={toolSpec.icon} /> {toolSpec.label}
                            </span>
                            <button type="button" className="dim-btn sm icon" onClick={() => pickTool("select")} title="Close (Esc)">
                                <Icon name="close" />
                            </button>
                        </div>
                        {ui.tool === "clean" && <CleanPanel context={context} />}
                        {ui.tool === "annotate" && <AnnotatePanel context={context} />}
                        {ui.tool === "polygon" && <PolygonPanel context={context} />}
                        {ui.tool === "places" && <PlacesPanel context={context} />}
                        {["erase", "brush", "line", "straighten"].includes(ui.tool) && <ModifyPanel context={context} />}
                        {ui.tool === "views" && <ViewsPanel context={context} />}
                    </div>
                )}
                {!session && last !== undefined && modal !== "open" && (
                    <EmptyState
                        layer
                        label="No recording open"
                        title="Please pick a recording"
                        body="Maps are built from a robot recording with lidar point clouds, from Desktop's shared recordings folder."
                        testId="pick-a-recording"
                        actions={[
                            { label: "Pick a recording", onClick: () => setModal("open") },
                            ...(last
                                ? [{
                                    label: `Continue ${last.name}`,
                                    primary: false,
                                    onClick: () => openRecording({ id: last.recordingId, path: last.path, name: last.name, writable: last.writable }),
                                }]
                                : []),
                        ]}
                    />
                )}
                {running && modal !== "generate" && (
                    <div className="floating-job">
                        <JobCard job={running} onCancel={() => session && run(api.cancel(session.id), "Cancelling…")} />
                    </div>
                )}
                {uploads.panelOpen && <UploadsPanel uploads={uploads} />}
                <div className="dim-toasts">{toast && <div className={`dim-toast ${toast.error ? "danger" : ""}`}>{toast.text}</div>}</div>
                {!connected && (
                    <div className="dim-alert danger connection" data-testid="onboard-backend-down">
                        The Map Editor server isn't answering; reconnecting… If this stays, close the app (✕) and open it again.
                    </div>
                )}
            </section>
            {uploads.loginOpen && (
                <LoginDialog
                    reason={uploads.waitingForLogin ? "Log in to Dimensional cloud to upload: your uploads wait here until you do." : null}
                    onApproved={uploads.loggedIn}
                    onClose={uploads.closeLogin}
                />
            )}
            {modal === "generate" && session && <GenerateModal context={context} onClose={() => setModal(null)} />}
            {modal === "open" && (
                <div className="modal-backdrop" onMouseDown={(event) => event.target === event.currentTarget && setModal(null)}>
                    <div className="dim-panel modal" data-modal="open">
                        <div className="modal-head">
                            <span />
                            <button type="button" className="dim-btn sm icon" onClick={() => setModal(null)}>
                                <Icon name="close" />
                            </button>
                        </div>
                        <OpenPanel context={context} />
                    </div>
                </div>
            )}
            {modal === "share" && session && hasMap && (
                <div className="modal-backdrop" onMouseDown={(event) => event.target === event.currentTarget && setModal(null)}>
                    <div className="dim-panel modal" data-modal="share">
                        <div className="modal-head">
                            <span />
                            <button type="button" className="dim-btn sm icon" onClick={() => setModal(null)}>
                                <Icon name="close" />
                            </button>
                        </div>
                        <SharePanel context={context} />
                    </div>
                </div>
            )}
            {modal === "save" && session && (
                <div className="modal-backdrop" onMouseDown={(event) => event.target === event.currentTarget && setModal(null)}>
                    <div className="dim-panel modal" data-modal="save">
                        <div className="modal-head">
                            <span />
                            <button type="button" className="dim-btn sm icon" onClick={() => setModal(null)}>
                                <Icon name="close" />
                            </button>
                        </div>
                        <SavePanel context={context} />
                    </div>
                </div>
            )}
        </div>
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
