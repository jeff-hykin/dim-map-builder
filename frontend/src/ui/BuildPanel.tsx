// Stage 2: build the global map: pick the lidar stream and voxel size, start, watch progress / ETA, cancel.
import { useEffect, useState } from "react"
import { api, recordings, type BuildOptions, type RecordingMetadata } from "../core/api.ts"
import { JobCard, duration } from "./JobCard.tsx"
import type { Context } from "./context.ts"

export function BuildPanel({ context }: { context: Context }) {
    const session = context.session
    const [meta, setMeta] = useState<RecordingMetadata | null>(null)
    const [options, setOptions] = useState<Partial<BuildOptions>>({ voxelSize: 0.05, loopClosure: true, cloudStream: "", every: 1 })
    useEffect(() => {
        if (session?.recordingId) {
            recordings.metadata(session.recordingId).then(setMeta).catch(() => setMeta(null))
        }
        if (session?.buildOptions) {
            setOptions(session.buildOptions)
        }
    }, [session?.recordingId, session?.buildOptions])
    if (!session) {
        return <div className="hint">Open a recording first.</div>
    }
    const clouds = (meta?.streams ?? []).filter((s) => /PointCloud2$/.test(s.type))
    const job = session.job
    const building = job?.state === "running"
    const build = session.build
    return (
        <div>
            <div className="panel-head">
                <h2>Build the global map</h2>
                <p>Every scan placed by the recording's tf, loops closed (ICP + pose graph), then ray traced so free space clears what moved.</p>
            </div>
            <div className="field">
                <span>lidar stream</span>
                <select value={options.cloudStream ?? ""} onChange={(event) => setOptions({ ...options, cloudStream: event.target.value })} disabled={building}>
                    <option value="">auto (the biggest)</option>
                    {clouds.map((stream) => (
                        <option key={stream.name} value={stream.name}>
                            {stream.name} ({stream.count})
                        </option>
                    ))}
                </select>
            </div>
            <div className="field">
                <span>voxel size</span>
                <div className="row" style={{ margin: 0 }}>
                    <input type="range" min={0.02} max={0.2} step={0.01} value={options.voxelSize ?? 0.05} onChange={(event) => setOptions({ ...options, voxelSize: Number(event.target.value) })} disabled={building} />
                    <span>{((options.voxelSize ?? 0.05) * 100).toFixed(0)} cm</span>
                </div>
            </div>
            <div className="field">
                <span>loop closure</span>
                <label className="row" style={{ margin: 0 }}>
                    <input type="checkbox" checked={options.loopClosure ?? true} onChange={(event) => setOptions({ ...options, loopClosure: event.target.checked })} disabled={building} />
                    <span className="hint">fixes drift where the robot came back</span>
                </label>
            </div>
            <div className="field">
                <span>use every</span>
                <div className="row" style={{ margin: 0 }}>
                    <input className="number" type="number" min={1} max={20} value={options.every ?? 1} onChange={(event) => setOptions({ ...options, every: Math.max(1, Number(event.target.value)) })} disabled={building} />
                    <span className="hint">th scan (faster, sparser)</span>
                </div>
            </div>
            <div className="row">
                <button type="button" className="button primary" disabled={building} data-action="build" onClick={() => context.run(api.build(session.id, options))}>
                    {session.stage === "map" ? "Rebuild" : "Build map"}
                </button>
                {session.stage === "map" && <span className="hint">rebuilding replaces the map (annotations stay)</span>}
            </div>
            {job && job.kind !== "save" && <JobCard job={job} onCancel={() => context.run(api.cancel(session.id), "Cancelling…")} />}
            {build && (
                <>
                    <h3>Last build</h3>
                    <div className="streams">
                        <span>
                            {session.totalVoxels.toLocaleString()} voxels at {(build.voxelSize * 100).toFixed(0)} cm in {duration(build.seconds)}
                        </span>
                        <span>
                            {build.scansUsed.toLocaleString()} scans from {build.cloudStream} · {build.loops} loop closures
                        </span>
                        {build.scansSkipped > 0 && <span>{build.scansSkipped} scans skipped (no pose)</span>}
                        {build.notes.map((note) => (
                            <span key={note}>{note}</span>
                        ))}
                    </div>
                    <div className="row">
                        <button type="button" className="button" onClick={() => context.setUi({ stage: "clean" })}>
                            Next: clean →
                        </button>
                    </div>
                </>
            )}
            {session.stage !== "map" && <div className="hint">The view shows the raw recording (odometry only) until the map is built.</div>}
        </div>
    )
}
