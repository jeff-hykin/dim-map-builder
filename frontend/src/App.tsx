// The Map Builder page: a stage-by-stage side panel over one 3D view. All state lives server-side (the session), so
// a refresh comes back to the same recording, stage, map, edits, camera and selection; running jobs keep running and
// the page reattaches to their progress.
import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { api, events, type Job, type Session } from "./core/api.ts"
import { MapScene } from "./core/scene.ts"
import { DEFAULT_UI, STAGES, type Context, type Stage, type UiState } from "./ui/context.ts"
import { OpenPanel } from "./ui/OpenPanel.tsx"
import { BuildPanel } from "./ui/BuildPanel.tsx"
import { CleanPanel } from "./ui/CleanPanel.tsx"
import { AnnotatePanel } from "./ui/AnnotatePanel.tsx"
import { PlansPanel, PlanView } from "./ui/PlansPanel.tsx"
import { SavePanel } from "./ui/SavePanel.tsx"
import { JobCard } from "./ui/JobCard.tsx"

export function App() {
    const host = useRef<HTMLDivElement>(null)
    const [scene, setScene] = useState<MapScene | null>(null)
    const [session, setSession] = useState<Session | null>(null)
    const [ui, setUiState] = useState<UiState>(DEFAULT_UI)
    const [toast, setToast] = useState<{ text: string; error: boolean } | null>(null)
    const [connected, setConnected] = useState(true)
    const [stats, setStats] = useState("")
    const sessionRef = useRef<Session | null>(null)
    const uiRef = useRef(ui)
    uiRef.current = ui
    sessionRef.current = session
    const loaded = useRef<{ id: string; mapVersion: number } | null>(null)
    const restoredCamera = useRef<string | null>(null)

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

    // the scene lives for the page's lifetime
    useEffect(() => {
        if (!host.current) {
            return
        }
        const created = new MapScene(host.current)
        setScene(created)
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

    /** reloads the session summary, and the map / paths when they changed */
    const refresh = useCallback(async () => {
        const current = sessionRef.current
        if (!current || !scene) {
            return
        }
        const fresh = await api.session(current.id)
        setSession(fresh)
        const last = loaded.current
        const mapChanged = !last || last.id !== fresh.id || last.mapVersion !== fresh.mapVersion
        if (fresh.stage === "map" && mapChanged) {
            const [points, paths] = await Promise.all([api.points(fresh.id), api.paths(fresh.id)])
            scene.setMap(points, fresh.voxelSize ?? 0.05)
            scene.setPaths(paths)
            scene.setRaw(null, null)
        }
        loaded.current = { id: fresh.id, mapVersion: fresh.mapVersion }
        scene.setAnnotations(fresh.annotations)
    }, [scene])

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
            const restoredUi: UiState = { ...DEFAULT_UI, ...saved, stage: saved.stage ?? (fresh.stage === "map" ? "clean" : "build") }
            setUiState(restoredUi)
            scene.applyLook(restoredUi.look)
            scene.showPaths(restoredUi.showPaths)
            scene.setRegion(restoredUi.region)
            await refresh()
            if (fresh.stage !== "map") {
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
            if (restoredUi.selected && (restoredUi.selected.kind === "box" || restoredUi.selected.kind === "plane" || restoredUi.selected.kind === "point" || restoredUi.selected.kind === "region")) {
                scene.select(restoredUi.selected as never)
            }
        },
        [scene, refresh],
    )

    const openRecording = useCallback(
        async (recording: { id: string; path: string; name: string; writable?: boolean }) => {
            const fresh = await run(api.open(recording))
            if (fresh) {
                await adopt({ ...fresh, view: fresh.view ?? null })
                if (fresh.stage !== "map") {
                    setUi({ stage: "build" })
                }
            }
        },
        [run, adopt, setUi],
    )

    // on load: go back to whatever was open
    useEffect(() => {
        if (!scene) {
            return
        }
        api.state()
            .then((state) => state.session && adopt(state.session))
            .catch((error) => say(String(error), true))
    }, [scene, adopt, say])

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
                    say("Map built")
                    await refresh()
                    scene.frameMap()
                    setUi({ stage: "clean" })
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
            } else if (event.type === "setView") {
                scene.lookAt(event.target, event.distance ?? undefined, !!event.topDown)
            } else if (event.type === "discarded" && current && event.id === current.id) {
                setSession(null)
                setUi({ stage: "open" })
            }
        })
    }, [scene, refresh, say, setUi])

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
            }
        }
    }, [scene, pushView, run, setUi])

    // keyboard shortcuts
    useEffect(() => {
        const onKey = (event: KeyboardEvent) => {
            const target = event.target as HTMLElement
            if (target.closest("input, textarea, select")) {
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
            const stage = STAGES.find((s) => s.key === event.key)
            if (stage) {
                setUi({ stage: stage.id })
            } else if (event.key === "f") {
                scene?.frameMap()
            } else if (event.key === "t") {
                scene?.topDown()
            } else if (event.key === "g" || event.key === "w") {
                scene?.setGizmoMode("translate")
            } else if (event.key === "r" || event.key === "e") {
                scene?.setGizmoMode("rotate")
            } else if (event.key === "s") {
                scene?.setGizmoMode("scale")
            } else if (event.key === "Escape") {
                scene?.select(null)
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
    }, [scene, run, setUi])

    const context: Context = { session, scene, ui, setUi, run, refresh, openRecording }
    const hasMap = session?.stage === "map"
    const job = session?.job
    const running = job?.state === "running" ? job : null

    return (
        <div className={`app ${ui.stage === "plans" ? "plans" : ""}`}>
            <header className="topbar">
                <span className="title">Map Builder</span>
                {session && <span className="recording-name" title={session.recordingPath}>{session.name}</span>}
                <nav className="stages">
                    {STAGES.map((stage) => (
                        <button
                            key={stage.id}
                            type="button"
                            className={`stage ${ui.stage === stage.id ? "on" : ""}`}
                            disabled={(!session && stage.id !== "open") || (!hasMap && ["clean", "annotate", "plans", "save"].includes(stage.id))}
                            onClick={() => setUi({ stage: stage.id })}
                            title={`${stage.label} (${stage.key})`}
                        >
                            <span className="num">{stage.key}</span>
                            {stage.label}
                        </button>
                    ))}
                </nav>
                <span className="spacer" />
                {session && (
                    <>
                        <button type="button" className="icon-button" disabled={!session.undoLabel} title={session.undoLabel ? `Undo: ${session.undoLabel} (⌘Z)` : "Nothing to undo"} onClick={() => run(api.undo(session.id))}>
                            ↶ Undo
                        </button>
                        <button type="button" className="icon-button" disabled={!session.redoLabel} title={session.redoLabel ? `Redo: ${session.redoLabel} (⇧⌘Z)` : "Nothing to redo"} onClick={() => run(api.redo(session.id))}>
                            ↷ Redo
                        </button>
                        {hasMap && (
                            <span className={`badge ${session.unsaved ? "unsaved" : "saved"}`} title={session.savedAt ? `last saved ${new Date(session.savedAt * 1000).toLocaleString()}` : "not saved into the recording yet"}>
                                {session.unsaved ? "● unsaved changes" : "✓ saved in recording"}
                            </span>
                        )}
                    </>
                )}
            </header>
            <div className="workspace">
                <aside className="side">
                    {ui.stage === "open" && <OpenPanel context={context} />}
                    {ui.stage === "build" && <BuildPanel context={context} />}
                    {ui.stage === "clean" && <CleanPanel context={context} />}
                    {ui.stage === "annotate" && <AnnotatePanel context={context} />}
                    {ui.stage === "plans" && <PlansPanel context={context} />}
                    {ui.stage === "save" && <SavePanel context={context} />}
                </aside>
                <section className="view">
                    <div className="scene" ref={host} />
                    {ui.stage === "plans" && session && <PlanView context={context} />}
                    {running && ui.stage !== "build" && (
                        <div className="floating-job">
                            <JobCard job={running} onCancel={() => session && run(api.cancel(session.id), "Cancelling…")} />
                        </div>
                    )}
                    {ui.stage !== "plans" && (
                        <div className="view-tools">
                            <button type="button" className="icon-button" title="Frame the map (F)" onClick={() => scene?.frameMap()}>
                                ⤢ Frame
                            </button>
                            <button type="button" className="icon-button" title="Top-down view (T)" onClick={() => scene?.topDown()}>
                                ⊤ Top
                            </button>
                        </div>
                    )}
                    {ui.stage !== "plans" && stats && <div className="view-stats">{stats}</div>}
                    {toast && <div className={`toast ${toast.error ? "error" : ""}`}>{toast.text}</div>}
                    {!connected && <div className="connection">Reconnecting to the Map Builder server…</div>}
                </section>
            </div>
        </div>
    )
}

export type { Stage }
