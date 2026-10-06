// Open: pick a recording from Desktop's shared recordings folder (newest first), see its streams, open it.
import { useEffect, useState } from "react"
import { recordings, type DesktopRecording, type RecordingMetadata } from "../core/api.ts"
import type { Context } from "./context.ts"
import { Icon } from "./Icon.tsx"
import { EmptyState } from "./EmptyState.tsx"
import { getZenoh } from "../dim-app/zenoh.js"

function size(bytes: number) {
    return bytes > 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(0.1, bytes / 1e6).toFixed(1)} MB`
}

function age(seconds: number) {
    const ago = Date.now() / 1000 - seconds
    if (ago < 3600) {
        return `${Math.max(1, Math.round(ago / 60))} min ago`
    }
    if (ago < 86400 * 2) {
        return `${Math.round(ago / 3600)} h ago`
    }
    return new Date(seconds * 1000).toLocaleDateString()
}

export function OpenPanel({ context }: { context: Context }) {
    const [list, setList] = useState<DesktopRecording[] | null>(null)
    const [dir, setDir] = useState("")
    const [problem, setProblem] = useState<string | null>(null)
    const [filter, setFilter] = useState("")
    const [details, setDetails] = useState<RecordingMetadata | null>(null)
    const [opening, setOpening] = useState<string | null>(null)

    const load = () =>
        recordings
            .list()
            .then((body) => {
                setList(body.recordings)
                setDir(body.dir)
                setProblem(null)
            })
            .catch((error) => setProblem(`Couldn't list Desktop's recordings: ${error.message}`))
    useEffect(() => {
        load()
        // live: a recording added, renamed or deleted anywhere (the Controller, the Recordings app) shows up here
        const zenoh = getZenoh()
        const offs = [zenoh.subscribeDesktop("recordings", () => load()), zenoh.onReconnect(() => load())]
        return () => offs.forEach((off) => off())
    }, [])

    const shown = (list ?? []).filter((r) => r.name.toLowerCase().includes(filter.toLowerCase()))
    return (
        <div>
            <div className="panel-head">
                <h2 className="dim-h2">Pick a recording</h2>
                <p>
                    Maps are built from a robot recording with lidar point clouds. These are Desktop's shared recordings{dir ? ` (${dir})` : ""}; the
                    Controller app records new ones.
                </p>
            </div>
            {context.session && (
                <div className="dim-panel tool-card">
                    <div className="name">Continue: {context.session.name}</div>
                    <div className="about">{context.session.stage === "map" ? `${context.session.voxels.toLocaleString()} voxels` : "not built yet"} · your work is kept automatically</div>
                    <button type="button" className="dim-btn sm primary" onClick={() => context.setModal(context.session?.stage === "map" ? null : "generate")}>
                        Continue
                    </button>
                </div>
            )}
            <div className="row">
                <input className="dim-input text grow" placeholder="filter…" value={filter} onChange={(event) => setFilter(event.target.value)} />
                <button type="button" className="dim-btn sm icon" onClick={load} title="Reload the list">
                    <Icon name="refresh" />
                </button>
            </div>
            {problem && (
                <div className="recordings-empty" data-testid="onboard-recordings-error">
                    <EmptyState
                        label="Recordings unavailable"
                        tone="warn"
                        title="Couldn't list Desktop's recordings"
                        body={`${problem}. Desktop may be restarting; try again in a moment.`}
                        actions={[{ label: "Try again", onClick: load }]}
                    />
                </div>
            )}
            {list && !list.length && (
                <div className="recordings-empty" data-testid="onboard-no-recordings">
                    <EmptyState
                        label="No recordings"
                        title="You don't have any recordings yet"
                        body="You can record a robot using the Controller app: run a blueprint (or a replay), open the Controller's Record tab, and press Record. New recordings show up here by themselves."
                        actions={[
                            { label: "Open the Controller", app: "dim-controller", appTitle: "the Controller" },
                            { label: "Open Recordings", app: "dim-recordings", appTitle: "the Recordings app", primary: false },
                        ]}
                    />
                </div>
            )}
            {list && list.length > 0 && !shown.length && <div className="hint">No recordings match “{filter}”.</div>}
            <ul className="recordings">
                {shown.map((recording) => (
                    <li key={recording.id}>
                        <button
                            type="button"
                            className={`dim-panel recording ${context.session?.recordingPath === recording.path ? "current" : ""}`}
                            data-recording={recording.id}
                            onClick={() => recordings.metadata(recording.id).then(setDetails).catch((error) => setProblem(error.message))}
                        >
                            <div>{recording.name}</div>
                            <div className="meta">
                                {recording.format} · {size(recording.size)} · {age(recording.modified)}
                                {recording.id.includes("/") ? ` · ${recording.id.split("/")[0]}` : ""}
                                {!recording.writable ? " · read-only (can't be saved into)" : ""}
                            </div>
                        </button>
                        {details?.id === recording.id && (
                            <div className="dim-panel tool-card">
                                <div className="dim-mono streams">
                                    {details.duration != null && <span>{Math.round(details.duration)} s recorded</span>}
                                    {details.streams.slice(0, 14).map((stream) => (
                                        <span key={stream.name}>
                                            {stream.name} · {stream.type.split(/[./]/).pop()} · {stream.count.toLocaleString()}
                                        </span>
                                    ))}
                                    {details.streams.length > 14 && <span>…and {details.streams.length - 14} more</span>}
                                </div>
                                {!details.streams.some((stream) => /PointCloud2$/.test(stream.type)) && (
                                    <div className="recording-warning" data-testid="onboard-no-pointclouds">
                                        <EmptyState
                                            label="No lidar"
                                            tone="warn"
                                            title="This recording has no lidar point clouds, so no map can be built from it"
                                            body="Pick another recording, or record one with the Controller while a blueprint publishes a point cloud (lidar)."
                                            actions={[{ label: "Open the Controller", app: "dim-controller", appTitle: "the Controller", primary: false }]}
                                        />
                                    </div>
                                )}
                                <button
                                    type="button"
                                    className="dim-btn sm primary"
                                    disabled={opening !== null || !details.streams.some((stream) => /PointCloud2$/.test(stream.type))}
                                    data-open={recording.id}
                                    onClick={async () => {
                                        setOpening(recording.id)
                                        await context.openRecording({ id: recording.id, path: recording.path, name: recording.name, writable: recording.writable })
                                        setOpening(null)
                                    }}
                                >
                                    {opening === recording.id ? "Opening…" : "Open"}
                                </button>
                            </div>
                        )}
                    </li>
                ))}
            </ul>
        </div>
    )
}
