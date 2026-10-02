// Generate the map: the build settings in a modal. The defaults are good (just press Generate); voxel size and the
// lidar stream are up front, everything else (skip loop closure / ray tracing, the ray tracer's and the pose graph's
// tunables) under Advanced. Regenerating over a map asks first: it replaces the map and the edits made on it.
import { useEffect, useState } from "react"
import { api, recordings, type RecordingMetadata } from "../core/api.ts"
import { JobCard, duration } from "./JobCard.tsx"
import type { Context } from "./context.ts"

type Options = Record<string, any>

const LABELS: Record<string, [string, string]> = {
    every: ["use every Nth scan", "1 = all; higher is faster and sparser"],
    maxRange: ["max range", "m; returns farther than this are dropped"],
    tfTolerance: ["tf tolerance", "s; the largest gap between a scan and the transform placing it"],
    worldFrame: ["world frame", "empty = world, map or odom, whichever places the clouds"],
    shadowDepth: ["shadow depth", "m behind a hit that still counts as the surface"],
    graceDepth: ["grace depth", "m in front of a hit that a miss doesn't clear"],
    minHealth: ["min health", "a voxel's lowest score (misses)"],
    maxHealth: ["max health", "a voxel's highest score (hits)"],
    grazeCos: ["graze cos", "a miss at a shallower angle than this doesn't clear a surface"],
    raySubsample: ["ray subsample", "trace every Nth ray of a scan"],
    key_pose_delta_trans: ["keyframe every (m)", "a new keyframe after moving this far"],
    key_pose_delta_deg: ["keyframe every (°)", "a new keyframe after turning this much"],
    loop_search_radius: ["loop search radius", "m"],
    loop_time_thresh: ["loop time gap", "s; only revisits at least this much later"],
    loop_score_thresh: ["loop ICP score", "max mean error to accept a loop"],
    loop_submap_half_range: ["submap half range", "keyframes either side"],
    min_icp_inliers: ["min ICP inliers", ""],
    min_keyframes_for_loop_search: ["min keyframes", "before searching for loops"],
    submap_resolution: ["submap resolution", "m"],
    min_loop_detect_duration: ["min loop interval", "s between loop detections"],
    max_icp_iterations: ["max ICP iterations", ""],
    max_icp_correspondence_dist: ["ICP correspondence", "m"],
    odom_rot_var: ["odometry rotation variance", ""],
    odom_trans_var_xy: ["odometry xy variance", ""],
    odom_trans_var_z: ["odometry z variance", ""],
    loop_rot_var: ["loop rotation variance", ""],
}

function Field({ name, value, onChange, disabled }: { name: string; value: unknown; onChange: (value: unknown) => void; disabled: boolean }) {
    const [label, about] = LABELS[name] ?? [name, ""]
    return (
        <div className="field" title={about}>
            <span>{label}</span>
            {typeof value === "number" ? (
                <input className="number" type="number" step="any" value={value} disabled={disabled} onChange={(event) => event.target.value !== "" && onChange(Number(event.target.value))} data-option={name} />
            ) : (
                <input className="text" value={String(value ?? "")} disabled={disabled} onChange={(event) => onChange(event.target.value)} data-option={name} />
            )}
        </div>
    )
}

export function GenerateModal({ context, onClose }: { context: Context; onClose: () => void }) {
    const { session, run } = context
    const [defaults, setDefaults] = useState<Options | null>(null)
    const [options, setOptions] = useState<Options | null>(null)
    const [meta, setMeta] = useState<RecordingMetadata | null>(null)
    const [confirming, setConfirming] = useState(false)
    useEffect(() => {
        // f32 settings arrive as 0.05000000074505806: show them as typed
        const tidy = (value: Options) => JSON.parse(JSON.stringify(value), (_key, v) => (typeof v === "number" ? +v.toPrecision(6) : v))
        api.buildDefaults().then((base) => {
            setDefaults(tidy(base))
            setOptions(tidy({ ...base, ...(session?.buildOptions ?? {}) }))
        })
        if (session?.recordingId) {
            recordings.metadata(session.recordingId).then(setMeta).catch(() => setMeta(null))
        }
    }, [session?.id])
    if (!session || !options || !defaults) {
        return null
    }
    const job = session.job?.kind === "build" || session.job?.kind === "preview" ? session.job : null
    const building = session.job?.state === "running" && session.job.kind === "build"
    const built = session.stage === "map"
    const set = (patch: Options) => setOptions({ ...options, ...patch })
    const group = (key: "ray" | "pgo", patch: Options) => setOptions({ ...options, [key]: { ...options[key], ...patch } })
    const clouds = (meta?.streams ?? []).filter((s) => /PointCloud2$/.test(s.type))
    const changed = JSON.stringify({ ...options, cloudStream: "" }) !== JSON.stringify({ ...defaults, cloudStream: "" })
    const generate = () => {
        if (built && !confirming) {
            setConfirming(true)
            return
        }
        setConfirming(false)
        run(api.build(session.id, options))
    }
    const a = session.annotations
    const annotationCount = a.boxes.length + a.planes.length + a.points.length + a.planPoints.length + a.areas.length + (a.prisms?.length ?? 0)
    return (
        <div className="modal-backdrop" onMouseDown={(event) => event.target === event.currentTarget && !building && onClose()}>
            <div className="modal" role="dialog" aria-label="Generate the map" data-modal="generate">
                <div className="modal-head">
                    <h2>{built ? "Regenerate the map" : "Generate the map"}</h2>
                    <button type="button" className="icon-button" onClick={onClose} title="Close (the build keeps running)">
                        ✕
                    </button>
                </div>
                <p className="hint">Every scan placed by the recording's tf, loops closed (ICP + pose graph), then ray traced so free space clears what moved. The defaults work for most recordings.</p>
                <div className="field">
                    <span>voxel size</span>
                    <span className="row" style={{ margin: 0 }}>
                        <input type="range" min={0.02} max={0.2} step={0.01} value={options.voxelSize} disabled={building} onChange={(event) => set({ voxelSize: Number(event.target.value) })} />
                        <input className="number" type="number" min={0.01} max={0.5} step={0.01} value={options.voxelSize} disabled={building} onChange={(event) => event.target.value !== "" && set({ voxelSize: Number(event.target.value) })} data-option="voxelSize" />
                        <span className="dim">m</span>
                    </span>
                </div>
                <div className="field">
                    <span>lidar stream</span>
                    <select value={options.cloudStream ?? ""} onChange={(event) => set({ cloudStream: event.target.value })} disabled={building}>
                        <option value="">auto (the biggest)</option>
                        {clouds.map((stream) => (
                            <option key={stream.name} value={stream.name}>
                                {stream.name} ({stream.count})
                            </option>
                        ))}
                    </select>
                </div>
                <details className="advanced" data-advanced>
                    <summary>Advanced {changed ? <span className="dim">· changed</span> : <span className="dim">· loop closure, ray tracing, tunables</span>}</summary>
                    <label className="row">
                        <input type="checkbox" checked={!options.loopClosure} disabled={building} onChange={(event) => set({ loopClosure: !event.target.checked })} data-option="skipLoopClosure" />
                        skip loop closure <span className="dim">(faster; drift where the robot came back stays)</span>
                    </label>
                    <label className="row">
                        <input type="checkbox" checked={!options.rayTracing} disabled={building} onChange={(event) => set({ rayTracing: !event.target.checked })} data-option="skipRayTracing" />
                        skip ray tracing <span className="dim">(faster; what moved through the scene stays in the map)</span>
                    </label>
                    {(["every", "maxRange", "tfTolerance", "worldFrame"] as const).map((name) => (
                        <Field key={name} name={name} value={options[name]} disabled={building} onChange={(value) => set({ [name]: value })} />
                    ))}
                    <h3>Ray tracer</h3>
                    {Object.keys(defaults.ray).map((name) => (
                        <Field key={name} name={name} value={options.ray[name]} disabled={building || !options.rayTracing} onChange={(value) => group("ray", { [name]: value })} />
                    ))}
                    <h3>Loop closure (pose graph)</h3>
                    {Object.keys(defaults.pgo).map((name) => (
                        <Field key={name} name={name} value={options.pgo[name]} disabled={building || !options.loopClosure} onChange={(value) => group("pgo", { [name]: value })} />
                    ))}
                    <div className="row">
                        <button type="button" className="button" disabled={building} onClick={() => setOptions({ ...defaults, cloudStream: options.cloudStream })}>
                            Reset to defaults
                        </button>
                    </div>
                </details>
                {confirming && (
                    <div className="warning" data-regen-warning>
                        <strong>Regenerate over the current map?</strong> The new map replaces this one: every voxel edit on it (cleanup, erase, draw, straightened walls) and the undo history are gone, and the exported occupancy grids are dropped.
                        {annotationCount > 0 && ` Your ${annotationCount} annotation${annotationCount === 1 ? "" : "s"} stay where they are, but may no longer line up with a map built differently.`}
                        <div className="row">
                            <button type="button" className="button danger armed" onClick={generate} data-action="confirm-regenerate">
                                Yes, regenerate
                            </button>
                            <button type="button" className="button" onClick={() => setConfirming(false)}>
                                Keep the current map
                            </button>
                        </div>
                    </div>
                )}
                {!confirming && (
                    <div className="row">
                        <button type="button" className="button primary" disabled={building} data-action="build" onClick={generate}>
                            {built ? "Regenerate…" : "Generate"}
                        </button>
                        {built && session.build && <span className="hint">current: {session.totalVoxels.toLocaleString()} voxels at {(session.build.voxelSize * 100).toFixed(0)} cm, built in {duration(session.build.seconds)}</span>}
                    </div>
                )}
                {job && <JobCard job={job} onCancel={() => run(api.cancel(session.id), "Cancelling…")} />}
            </div>
        </div>
    )
}
