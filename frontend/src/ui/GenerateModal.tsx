// Generate the map: the build settings in a modal. The defaults are good (just press Generate); voxel size and the
// lidar stream are up front, everything else (skip loop closure / ray tracing, the ray tracer's and the pose graph's
// tunables) under Advanced. Regenerating over a map asks first: it replaces the map and the edits made on it.
import { useEffect, useState } from "react"
import { api, recordings, type RecordingMetadata } from "../core/api.ts"
import { Alert, Button, Checkbox, Column, Field, Heading, Modal, NumberInput, Panel, Row, Select, Slider, Text, TextInput } from "dimos-react"
import { JobCard, duration } from "./JobCard.tsx"
import type { Context } from "./context.ts"

type Options = Record<string, any>

const LITE_RECORD_VOXEL = 0.04

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

function OptionField({ name, value, onChange, disabled }: { name: string; value: unknown; onChange: (value: unknown) => void; disabled: boolean }) {
    const [label, about] = LABELS[name] ?? [name, ""]
    return (
        <Field inline label={label} title={about}>
            {typeof value === "number"
                ? <NumberInput size="sm" step={0.01} value={value} disabled={disabled} onChange={onChange} data-option={name} style={{ width: 110 }} />
                : <TextInput size="sm" value={String(value ?? "")} disabled={disabled} onChange={onChange} data-option={name} />}
        </Field>
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
    // lite_record post-processes it first, and its own map is 4 cm voxels: this one starts at the same size
    const liteRecord = !!session?.postProcess
    useEffect(() => {
        if (liteRecord && !session?.buildOptions && defaults) {
            setOptions((current) => current && { ...current, voxelSize: LITE_RECORD_VOXEL })
        }
    }, [liteRecord, defaults, session?.buildOptions])
    if (!session || !options || !defaults) {
        return null
    }
    const job = session.job?.kind === "build" || session.job?.kind === "preview" ? session.job : null
    const building = session.job?.state === "running" && session.job.kind === "build"
    const built = session.stage === "map"
    const set = (patch: Options) => setOptions({ ...options, ...patch })
    const group = (key: "ray" | "pgo", patch: Options) => setOptions({ ...options, [key]: { ...options[key], ...patch } })
    const clouds = (meta?.streams ?? []).filter((s) => /PointCloud2$/.test(s.type))
    const changed = JSON.stringify({ ...options, cloudStream: "", voxelSize: 0 }) !== JSON.stringify({ ...defaults, cloudStream: "", voxelSize: 0 })
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
        <Modal
            open
            size="lg"
            title={built ? "Regenerate the map" : "Generate the map"}
            onClose={onClose}
            dismissable={!building}
            aria-label="Generate the map"
            data-modal="generate"
            footer={
                confirming
                    ? null
                    : (
                        <Row grow justify="space-between" wrap>
                            <Text small muted>
                                {built && session.build ? `current: ${session.totalVoxels.toLocaleString()} voxels at ${(session.build.voxelSize * 100).toFixed(0)} cm, built in ${duration(session.build.seconds)}` : ""}
                            </Text>
                            <Button variant="primary" disabled={building} data-action="build" onClick={generate}>
                                {built ? "Regenerate…" : "Generate"}
                            </Button>
                        </Row>
                    )
            }
        >
            <Column gap={10}>
                <Text small muted>Every scan placed by the recording's tf, loops closed (ICP + pose graph), then ray traced so free space clears what moved. The defaults work for most recordings.</Text>
                {session.postProcess && (
                    <Alert tone="info" title="Not post-processed yet." data-post-process>
                        Generate first runs lite_record's post-processing on it (its Post process button): odometry, motion-compensated scans, the frame tree and its 4 cm voxel map, written into the recording in place. Then the map is built from the corrected scans.
                        {!session.writable && " This recording is in a read-only folder, so it can't be: move it into the recordings folder first."}
                    </Alert>
                )}
                <Field inline label="voxel size">
                    <Row>
                        <Slider min={0.02} max={0.2} step={0.01} value={options.voxelSize} disabled={building} showValue={false} onChange={(voxelSize: number) => set({ voxelSize })} style={{ width: 160 }} />
                        <NumberInput size="sm" min={0.01} max={0.5} step={0.01} unit="m" value={options.voxelSize} disabled={building} onChange={(voxelSize: number) => set({ voxelSize })} data-option="voxelSize" style={{ width: 90 }} />
                    </Row>
                </Field>
                <Field inline label="lidar stream">
                    <Select
                        size="sm"
                        value={options.cloudStream ?? ""}
                        disabled={building}
                        onChange={(cloudStream: string) => set({ cloudStream })}
                        options={[["", "auto (the biggest)"], ...clouds.map((stream) => [stream.name, `${stream.name} (${stream.count})`])]}
                    />
                </Field>
                <Panel collapsible defaultOpen={false} title={`Advanced · ${changed ? "changed" : "loop closure, ray tracing, tunables"}`} data-advanced>
                    <Checkbox value={!options.loopClosure} disabled={building} onChange={(skip: boolean) => set({ loopClosure: !skip })} data-option="skipLoopClosure" label="skip loop closure (faster; drift where the robot came back stays)" />
                    <Checkbox value={!options.rayTracing} disabled={building} onChange={(skip: boolean) => set({ rayTracing: !skip })} data-option="skipRayTracing" label="skip ray tracing (faster; what moved through the scene stays in the map)" />
                    {(["every", "maxRange", "tfTolerance", "worldFrame"] as const).map((name) => (
                        <OptionField key={name} name={name} value={options[name]} disabled={building} onChange={(value) => set({ [name]: value })} />
                    ))}
                    <Heading level={3}>Ray tracer</Heading>
                    {Object.keys(defaults.ray).map((name) => (
                        <OptionField key={name} name={name} value={options.ray[name]} disabled={building || !options.rayTracing} onChange={(value) => group("ray", { [name]: value })} />
                    ))}
                    <Heading level={3}>Loop closure (pose graph)</Heading>
                    {Object.keys(defaults.pgo).map((name) => (
                        <OptionField key={name} name={name} value={options.pgo[name]} disabled={building || !options.loopClosure} onChange={(value) => group("pgo", { [name]: value })} />
                    ))}
                    <Row>
                        <Button size="sm" disabled={building} onClick={() => setOptions({ ...defaults, cloudStream: options.cloudStream })}>
                            Reset to defaults
                        </Button>
                    </Row>
                </Panel>
                {confirming && (
                    <Alert
                        tone="warn"
                        title="Regenerate over the current map?"
                        data-regen-warning
                        actions={
                            <>
                                <Button size="sm" variant="danger" onClick={generate} data-action="confirm-regenerate">
                                    Yes, regenerate
                                </Button>
                                <Button size="sm" onClick={() => setConfirming(false)}>
                                    Keep the current map
                                </Button>
                            </>
                        }
                    >
                        The new map replaces this one: every voxel edit on it (cleanup, erase, draw, straightened walls) and the undo history are gone, and the exported occupancy grids are dropped.
                        {annotationCount > 0 && ` Your ${annotationCount} annotation${annotationCount === 1 ? "" : "s"} stay where they are, but may no longer line up with a map built differently.`}
                    </Alert>
                )}
                {job && <JobCard job={job} onCancel={() => run(api.cancel(session.id), "Cancelling…")} />}
            </Column>
        </Modal>
    )
}
