// Stage 1: pick a recording from Desktop's shared recordings folder (newest first), see its streams, open it.
import { useEffect, useState } from "react"
import { recordings, type DesktopRecording, type RecordingMetadata } from "../core/api.ts"
import type { Context } from "./context.ts"

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
    }, [])

    const shown = (list ?? []).filter((r) => r.name.toLowerCase().includes(filter.toLowerCase()))
    return (
        <div>
            <div className="panel-head">
                <h2>Open a recording</h2>
                <p>From Desktop's shared recordings folder{dir ? `: ${dir}` : ""}. Record one with the Live Viewer.</p>
            </div>
            {context.session && (
                <div className="tool-card">
                    <div className="name">Continue: {context.session.name}</div>
                    <div className="about">{context.session.stage === "map" ? `${context.session.voxels.toLocaleString()} voxels` : "not built yet"} · your work is kept automatically</div>
                    <button type="button" className="button primary" onClick={() => context.setUi({ stage: context.session?.stage === "map" ? "clean" : "build" })}>
                        Continue
                    </button>
                </div>
            )}
            <div className="row">
                <input className="text grow" placeholder="filter…" value={filter} onChange={(event) => setFilter(event.target.value)} />
                <button type="button" className="icon-button" onClick={load} title="Reload the list">
                    ⟳
                </button>
            </div>
            {problem && <div className="problem">{problem}</div>}
            {list && !shown.length && <div className="hint">No recordings{filter ? " match" : " yet"}.</div>}
            <ul className="recordings">
                {shown.map((recording) => (
                    <li key={recording.id}>
                        <button
                            type="button"
                            className={`recording ${context.session?.recordingPath === recording.path ? "current" : ""}`}
                            data-recording={recording.id}
                            onClick={() => recordings.metadata(recording.id).then(setDetails).catch((error) => setProblem(error.message))}
                        >
                            <div>{recording.name}</div>
                            <div className="meta">
                                {recording.format} · {size(recording.size)} · {age(recording.modified)}
                                {recording.id.includes("/") ? ` · ${recording.id.split("/")[0]}` : ""}
                            </div>
                        </button>
                        {details?.id === recording.id && (
                            <div className="tool-card">
                                <div className="streams">
                                    {details.duration != null && <span>{Math.round(details.duration)} s recorded</span>}
                                    {details.streams.slice(0, 14).map((stream) => (
                                        <span key={stream.name}>
                                            {stream.name} · {stream.type.split(/[./]/).pop()} · {stream.count.toLocaleString()}
                                        </span>
                                    ))}
                                    {details.streams.length > 14 && <span>…and {details.streams.length - 14} more</span>}
                                </div>
                                <button
                                    type="button"
                                    className="button primary"
                                    disabled={opening !== null}
                                    data-open={recording.id}
                                    onClick={async () => {
                                        setOpening(recording.id)
                                        await context.openRecording({ id: recording.id, path: recording.path, name: recording.name })
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
