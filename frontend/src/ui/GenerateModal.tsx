// Generate: the corrected map, or any one of the steps that make it, each written into the recording in place.
// 1. fiducial markers from images, AprilTag or ArUco (raw_april_tags, april_tags), 2. a corrected tf tree (loop closure, with the tags, written
// as the edge above the odometry: world → odom), 3. the global map (through the corrected tf, ray traced, decimated:
// global_map_aggregated + a .pc2.lcm beside the recording, optionally an .rrd). A step whose input the recording
// doesn't have yet runs the earlier step first. The defaults work for most recordings: press "Generate corrected
// map". A raw lite_record recording (no odometry) is post-processed and built the classic way.
import { useEffect, useMemo, useRef, useState } from "react"
import { api, type CloudChoice, type FrameNode, type GenerateStatus } from "../core/api.ts"
import { ReadOnlyAlert } from "./ReadOnlyAlert.tsx"
import { Alert, Badge, Button, Card, Checkbox, Chip, Column, Field, Heading, Modal, NumberInput, Panel, Row, Segmented, Select, Slider, Text, TextInput } from "dimos-react"
import { JobCard, duration } from "./JobCard.tsx"
import type { Context } from "./context.ts"

type Options = Record<string, any>

const LITE_RECORD_VOXEL = 0.04

const LABELS: Record<string, [string, string]> = {
    every: ["use every Nth", "1 = all; higher is faster and sparser"],
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
    closureSpacing: ["stitch every (m of path)", "at most one ICP stitch per this many meters of path (0 = every candidate)"],
    icpRadiusM: ["revisit radius", "m; keyframes this close are a stitch candidate"],
    icpMinDtS: ["revisit time gap", "s; only keyframes at least this far apart in time"],
    icpMaxCorrM: ["ICP correspondence", "m"],
    icpVoxelM: ["submap voxel", "m"],
    icpFitnessMin: ["min fitness", "inlier share a stitch needs"],
    icpRmseMaxM: ["max inlier RMSE", "m"],
    icpHuberDelta: ["Huber k", "robust loss on the stitches"],
    icpRotVar: ["stitch rotation variance", ""],
    icpTransVar: ["stitch translation variance", ""],
    submapHalfS: ["submap half width", "s of scans either side of a keyframe"],
    lmMaxIterations: ["solver iterations", ""],
    scanVoxel: ["scan thinning", "m; the scans handed to icp_stitch are thinned to this first"],
    rollPitchVar: ["roll/pitch variance", "odometry between keyframes"],
    yawVar: ["yaw variance", ""],
    xyVar: ["xy variance", ""],
    zVar: ["z variance", ""],
    keyframeTranslationM: ["keyframe every (m)", ""],
    keyframeRotationDeg: ["keyframe every (°)", ""],
    rateHz: ["edge rate", "Hz; how often the corrected edge is written"],
    minSharpness: ["min sharpness", "Laplacian variance over the tag; lower = blurrier"],
    maxReprojPx: ["max reprojection", "px"],
    minTagPx: ["min tag size", "px"],
    maxDistanceM: ["max distance", "m"],
    maxViewAngleDeg: ["max view angle", "° off head-on"],
    maxLinearSpeedMps: ["max camera speed", "m/s"],
    maxAngularSpeedDps: ["max camera turn rate", "°/s"],
    clusterGapSec: ["visit gap", "s; a marker seen again after this is a new group"],
    minObservations: ["min glimpses per group", ""],
    huberDeltaM: ["Huber delta", "m"],
    rotationWeightMPerRad: ["rotation weight", "m per radian"],
    neighbors: ["outlier neighbors", ""],
    stdRatio: ["outlier std ratio", ""],
}

function OptionField({ name, value, onChange, disabled }: { name: string; value: unknown; onChange: (value: unknown) => void; disabled?: boolean }) {
    const [label, about] = LABELS[name] ?? [name, ""]
    return (
        <Field inline label={label} title={about}>
            {typeof value === "number"
                ? <NumberInput size="sm" step={0.01} value={value} disabled={disabled} onChange={onChange} data-option={name} style={{ width: 110 }} />
                : <TextInput size="sm" value={String(value ?? "")} disabled={disabled} onChange={onChange} data-option={name} />}
        </Field>
    )
}

/** every numeric field of an options group, as inputs */
function Fields({ group, onChange, disabled, only }: { group: Options; onChange: (patch: Options) => void; disabled?: boolean; only?: string[] }) {
    return (
        <>
            {Object.keys(group).filter((name) => (only ? only.includes(name) : typeof group[name] === "number")).map((name) => (
                <OptionField key={name} name={name} value={group[name]} disabled={disabled} onChange={(value) => onChange({ [name]: value })} />
            ))}
        </>
    )
}

// f32 settings arrive as 0.05000000074505806: show them as typed
const tidy = (value: Options) => JSON.parse(JSON.stringify(value), (_key, v) => (typeof v === "number" ? +v.toPrecision(6) : v))

const when = (seconds: number | undefined) => (seconds ? new Date(seconds * 1000).toLocaleString() : "")

/** The edge a correction of `sensor` would write (the server's correct::plan, for the page's live preview). */
function edgeFor(frames: FrameNode[], sensor: string, grandparentName: string): { parent: string; grandparent: string; exists: boolean } | null {
    const byName = new Map(frames.map((f) => [f.name, f]))
    let at = byName.get(sensor)
    for (let guard = 0; at && guard < frames.length; guard++) {
        if (!at.parent) {
            return null
        }
        if (at.dynamic) {
            const parent = byName.get(at.parent)
            if (!parent) {
                return null
            }
            return parent.parent ? { parent: parent.name, grandparent: parent.parent, exists: true } : { parent: parent.name, grandparent: grandparentName || "world", exists: false }
        }
        at = byName.get(at.parent)
    }
    return null
}

/** DICT_APRILTAG_36h11 → "AprilTag 36h11", DICT_4X4_50 → "ArUco 4x4 (50)", DICT_ARUCO_ORIGINAL → "ArUco original" */
function dictionaryLabel(name: string) {
    const april = name.match(/^DICT_APRILTAG_(\w+)$/)
    if (april) {
        return `AprilTag ${april[1]}`
    }
    const aruco = name.match(/^DICT_(\d)X\d_(\d+)$/)
    if (aruco) {
        return `ArUco ${aruco[1]}x${aruco[1]} (${aruco[2]})`
    }
    return `ArUco ${name.replace(/^DICT_ARUCO_/, "").replace("_", " ").toLowerCase()}`
}

function cloudLabel(cloud: CloudChoice) {
    const where = !cloud.inTf ? "not in tf" : cloud.isRoot ? `${cloud.frame}, the root` : `${cloud.frame}${cloud.dynamic ? ", moving" : ""}`
    return `${cloud.stream} (${where}, ${cloud.count})`
}

function StepCard({ number, title, status, children, run, running, disabled, runLabel, step }: { number: number; title: string; status: React.ReactNode; children: React.ReactNode; run: () => void; running: boolean; disabled?: boolean; runLabel?: string; step: string }) {
    return (
        <Card padded data-step={step} title={`${number}. ${title}`} description={status} actions={
            <Button size="sm" disabled={running || disabled} onClick={run} data-action={`run-${step}`}>
                {runLabel ?? "Run this step"}
            </Button>
        }>
            <Column gap={8}>{children}</Column>
        </Card>
    )
}

/** the stage the preview and "read" jobs read the tf in (server/src/app.rs READING_TF) */
const READING_TF = "Reading the tf tree"

export function GenerateModal({ context, onClose }: { context: Context; onClose: () => void }) {
    const { session, run } = context
    const [status, setStatus] = useState<GenerateStatus | null>(null)
    const [problem, setProblem] = useState<string | null>(null)
    const [buildDefaults, setBuildDefaults] = useState<Options | null>(null)
    const [tagOptions, setTagOptions] = useState<Options | null>(null)
    const [tfOptions, setTfOptions] = useState<Options | null>(null)
    const [globalOptions, setGlobalOptions] = useState<Options | null>(null)
    const [classic, setClassic] = useState<Options | null>(null)
    /** a press that would replace the current map, waiting for a yes: Generate's, or the recorded map's */
    const [confirming, setConfirming] = useState<"generate" | "recorded" | null>(null)
    /** with a recorded global_map the steps aren't read until Details opens (it is the map) */
    const [showSteps, setShowSteps] = useState(!session?.recordedMap)
    /** the steps and their settings: closed, since Generate's defaults are the usual path */
    const [detailsOpen, setDetailsOpen] = useState(false)
    const job = session?.job && session.job.kind !== "save" ? session.job : null
    const running = job?.state === "running"
    // the preview reads the tf first: past that, the step list can be read
    const readingTf = job?.state === "running" && job.progress?.stage === READING_TF
    const jobState = `${job?.id}:${job?.state}:${readingTf}`

    // the options start from each step's defaults with what it ran with last on top (once per session: the first
    // status there is, which waits for an "open" job reading a recording with no index)
    const optionsFor = useRef<string | null>(null)
    const initOptions = (fresh: GenerateStatus) => {
        if (!session || optionsFor.current === session.id) {
            return
        }
        optionsFor.current = session.id
        setTagOptions(tidy({ ...fresh.tags.defaults, ...(fresh.tags.record?.options ?? {}) }))
        setTfOptions(tidy({ ...fresh.tf.defaults, ...(fresh.tf.record?.options ?? {}), cloudStream: fresh.tf.record?.options?.cloudStream || fresh.tf.analysis.defaultCloud || "" }))
        const global = tidy({ ...fresh.globalMap.defaults })
        global.rrd = { ...global.rrd, streams: fresh.globalMap.rrdStreams }
        if (session.buildOptions) {
            global.build = { ...global.build, voxelSize: session.buildOptions.voxelSize }
        }
        setGlobalOptions(global)
    }
    const load = () =>
        session &&
        api.generateStatus(session.id)
            .then((fresh) => {
                if (fresh) {
                    setStatus(fresh)
                    setProblem(null)
                    initOptions(fresh)
                }
                return fresh
            })
            .catch((error) => setProblem(String(error.message ?? error)))
    // the steps' status is read once they show (a big .mcap's tf is read in a job first: not while the recorded map
    // is the offer, so pressing it isn't held up)
    const stepsWanted = showSteps || !session?.recordedMap
    useEffect(() => {
        if (!session || !stepsWanted) {
            return
        }
        api.buildDefaults().then((base) => {
            setBuildDefaults(tidy(base))
            setClassic(tidy({ ...base, ...(session.buildOptions ?? {}), ...(session.postProcess && !session.buildOptions ? { voxelSize: LITE_RECORD_VOXEL } : {}) }))
        })
        load()
    }, [session?.id, stepsWanted])
    // a step finished (here or from the agent): what the recording has now
    useEffect(() => {
        if (stepsWanted && job && (job.state !== "running" || (!status && !readingTf))) {
            load()
        }
    }, [jobState])

    const frames = status?.tf.analysis.frames ?? []
    const clouds = useMemo(() => [...(status?.tf.analysis.clouds ?? [])].sort((a, b) => Number(a.mapLike) - Number(b.mapLike)), [status])
    if (!session) {
        return null
    }
    const built = session.stage === "map"
    const lite = !!session.postProcess
    const ready = status && tagOptions && tfOptions && globalOptions && buildDefaults
    const setTags = (patch: Options) => setTagOptions({ ...tagOptions, ...patch })
    const setTf = (patch: Options) => setTfOptions({ ...tfOptions, ...patch })
    const setGlobal = (patch: Options) => setGlobalOptions({ ...globalOptions, ...patch })
    const group = (set: (patch: Options) => void, current: Options | null, key: string) => (patch: Options) => set({ [key]: { ...(current?.[key] ?? {}), ...patch } })

    const cloud = clouds.find((c) => c.stream === tfOptions?.cloudStream)
    const sensor = cloud?.isRoot ? tfOptions?.sensorFrame ?? "" : cloud?.frame ?? ""
    const sensorChoices = cloud?.isRoot ? frames.filter((f) => f.root === cloud.frame && f.parent).sort((a, b) => a.depth - b.depth || Number(b.dynamic) - Number(a.dynamic)) : []
    const edge = sensor ? edgeFor(frames, sensor, tfOptions?.grandparent ?? "") : null
    const markers = status?.tags.markers ?? []

    const runTags = () => run(api.generateTags(session.id, tagOptions ?? {}))
    // the correction gates the tags the way step 1 does
    const runTf = () => run(api.generateTf(session.id, { ...tfOptions, gates: tagOptions?.gates, sensorFrame: cloud?.isRoot ? tfOptions?.sensorFrame : "", tags: tagOptions }))
    const runGlobal = () => {
        if (built && confirming !== "generate") {
            setConfirming("generate")
            return
        }
        setConfirming(null)
        if (lite) {
            run(api.build(session.id, classic ?? {}))
        } else {
            run(api.generateGlobal(session.id, { ...globalOptions, tf: { ...tfOptions, gates: tagOptions?.gates, sensorFrame: cloud?.isRoot ? tfOptions?.sensorFrame : "" }, tags: tagOptions }))
        }
    }
    const recorded = session.recordedMap
    const useRecorded = () => {
        if (built && confirming !== "recorded") {
            setConfirming("recorded")
            return
        }
        setConfirming(null)
        run(api.useRecordedMap(session.id))
    }
    const stepsShown = stepsWanted
    const a = session.annotations
    const annotationCount = a.boxes.length + a.planes.length + a.points.length + a.planPoints.length + a.areas.length + (a.prisms?.length ?? 0)

    const tagStatus = status && (status.tags.raw > 0 || status.tags.grouped > 0)
        ? <>{`${status.tags.raw} raw, ${status.tags.grouped} grouped · markers ${markers.map((m) => m.id).join(", ") || "none"}`}{status.tags.record ? ` · ${when(status.tags.record.at)}` : ""}</>
        : "not in the recording yet"
    const report = status?.tf.record?.report
    const tfStatus = status?.tf.corrected
        ? <>
            {`${status.tf.record?.plan.grandparent} → ${status.tf.record?.plan.parent} written · ${report?.keyframes ?? 0} keyframes, ${report?.loops ?? 0} loops, ${report?.tagFactors ?? 0} tag factors`}
            {report?.tagSpreadRaw != null && report?.tagSpreadCorrected != null ? ` · tag spread ${report.tagSpreadRaw.toFixed(3)} → ${report.tagSpreadCorrected.toFixed(3)} m` : ""}
            {status.tf.stale ? <> <Badge tone="warn">the tags changed since</Badge></> : null}
        </>
        : "not in the recording yet"
    const globalStatus = status?.globalMap.present
        ? <>
            {`global_map_aggregated: ${status.globalMap.record?.points?.toLocaleString() ?? "?"} points in ${status.globalMap.record?.worldFrame ?? "?"}${status.globalMap.lcmExists ? " · .pc2.lcm beside it" : ""}${status.globalMap.rrdExists ? " · .rrd beside it" : ""}`}
            {status.globalMap.stale ? <> <Badge tone="warn">the tf changed since</Badge></> : null}
        </>
        : "not in the recording yet"

    return (
        <Modal
            open
            size="lg"
            title={built ? "Regenerate the map" : "Generate the map"}
            onClose={onClose}
            dismissable={!running}
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
                            {stepsShown && (
                                <Button variant={recorded ? undefined : "primary"} disabled={running || !ready || !session.writable} data-action="build" onClick={runGlobal}>
                                    {lite ? (built ? "Regenerate…" : "Generate") : built ? "Regenerate corrected map…" : "Generate corrected map"}
                                </Button>
                            )}
                        </Row>
                    )
            }
        >
            <Column gap={10}>
                {recorded && (
                    <Card
                        padded
                        data-recorded-map={recorded}
                        title={`This recording already has a map: ${recorded}`}
                        description={`Its last message is used as the aggregated scan, so there's nothing to generate (and nothing is written into the recording). A voxel map is kept as it is.`}
                        style={{ borderColor: "color-mix(in srgb, var(--ok) 55%, transparent)" }}
                        actions={
                            <Button variant="primary" disabled={running} onClick={useRecorded} data-action="use-recorded-map">
                                {`Use ${recorded}${built ? "…" : ""}`}
                            </Button>
                        }
                    />
                )}
                {problem && <Alert tone="danger" title="Couldn't read the recording.">{problem}</Alert>}
                {/* it only matters for generating: with a recorded map, inside Details */}
                {!session.writable && !recorded && <ReadOnlyAlert session={session} doing={lite ? "Post-processing" : "Every step"} />}
                {lite && (
                    <Alert tone="info" title="Not post-processed yet." data-post-process>
                        Generate first runs lite_record's post-processing on it (its Post process button): odometry, motion-compensated scans, the frame tree and its 4 cm voxel map, written into the recording in place. Then the map is built from the corrected scans; the steps below work once it has its frame tree.
                    </Alert>
                )}
                <Panel collapsible open={detailsOpen} onOpenChange={(open: boolean) => {
                    setDetailsOpen(open)
                    if (open) {
                        setShowSteps(true)
                    }
                }} title={recorded ? "Details · generate from the scans instead" : "Details · steps and settings"} data-details>
                <Column gap={10}>
                {!session.writable && recorded && <ReadOnlyAlert session={session} doing={lite ? "Post-processing" : "Every step"} />}
                <Text small muted>
                    Everything is written into the recording itself ({status?.format ?? "…"}): run the whole thing, or any one step below. A step that needs an earlier one the recording doesn't have yet runs it first.
                </Text>
                {stepsShown && !ready && !problem && <Text small muted>Reading the recording…</Text>}
                {stepsShown && ready && !lite && (
                    <>
                        <StepCard number={1} step="tags" title="Fiducial markers (AprilTag / ArUco)" status={tagStatus} running={running} disabled={!!status.tags.problem || (!tagOptions.raw && !tagOptions.grouped)} run={runTags}>
                            {status.tags.problem && <Alert tone="warn" title="Can't find tags here.">{status.tags.problem}</Alert>}
                            <Row wrap gap={16}>
                                <Checkbox value={tagOptions.raw} onChange={(raw: boolean) => setTags({ raw })} data-option="tagsRaw" label="raw tags (every glimpse: raw_april_tags)" />
                                <Checkbox value={tagOptions.grouped} onChange={(grouped: boolean) => setTags({ grouped })} data-option="tagsGrouped" label="grouped tags (gated, one per visit: april_tags)" />
                            </Row>
                            <Row wrap gap={16}>
                                <Field inline label="camera">
                                    <Select size="sm" value={tagOptions.cameraStream ?? ""} onChange={(cameraStream: string) => setTags({ cameraStream })} options={[["", `auto (${status.tags.camera ?? "none"})`], ...status.tags.images.map((i) => [i.name, `${i.name} (${i.count})`])]} />
                                </Field>
                                <Field inline label="markers">
                                    <Select size="sm" value={tagOptions.dictionary} onChange={(dictionary: string) => setTags({ dictionary })} data-option="tagsDictionary"
                                        options={(status.tags.dictionaries ?? ["DICT_APRILTAG_36h11"]).map((name) => [name, dictionaryLabel(name)])} />
                                </Field>
                                <Field inline label="tag size">
                                    <NumberInput size="sm" step={0.01} unit="m" value={tagOptions.markerLength} onChange={(markerLength: number) => setTags({ markerLength })} style={{ width: 90 }} />
                                </Field>
                            </Row>
                            <Panel collapsible defaultOpen={false} title="Advanced · gates, grouping" data-advanced="tags">
                                <OptionField name="every" value={tagOptions.every} onChange={(every) => setTags({ every })} />
                                <Field inline label="leave out of the grouped tags" title="ids of tags on things that move">
                                    <TextInput size="sm" value={(tagOptions.exclude ?? []).join(", ")} onChange={(text: string) => setTags({ exclude: text.split(/[ ,]+/).filter(Boolean).map(Number).filter((n) => Number.isFinite(n)) })} data-option="tagsExclude" />
                                </Field>
                                <Heading level={3}>Quality gates (step 2 uses them too)</Heading>
                                <Fields group={tagOptions.gates} onChange={group(setTags, tagOptions, "gates")} />
                                <Heading level={3}>Grouping</Heading>
                                <Fields group={tagOptions.grouping} onChange={group(setTags, tagOptions, "grouping")} />
                            </Panel>
                        </StepCard>

                        <StepCard number={2} step="tf" title="A corrected tf tree (loop closure)" status={tfStatus} running={running} disabled={!cloud || (cloud.isRoot && !tfOptions.sensorFrame) || !edge} run={runTf}>
                            {status.tf.planProblem && !cloud && <Alert tone="warn" title="Nothing to correct yet.">{status.tf.planProblem}</Alert>}
                            <Checkbox value={tfOptions.useTags} onChange={(useTags: boolean) => setTf({ useTags })} disabled={!!status.tags.problem && status.tags.raw === 0} data-option="useTags"
                                label={status.tags.raw > 0 ? "use the AprilTags" : status.tags.problem ? "use AprilTags (none here: no camera intrinsics)" : "use AprilTags (step 1 runs first: the recording has none yet)"} />
                            <Field inline label="target cloud">
                                <Select size="sm" value={tfOptions.cloudStream} onChange={(cloudStream: string) => setTf({ cloudStream, sensorFrame: "" })} data-option="cloudStream"
                                    options={clouds.map((c) => ({ value: c.stream, label: `${cloudLabel(c)}${c.stream === status.tf.analysis.defaultCloud ? " · default" : ""}`, disabled: !c.inTf }))} />
                            </Field>
                            {cloud?.isRoot && (
                                <Alert tone="warn" title={`${cloud.stream} is in ${cloud.frame}, the root of its tree.`} data-root-warning>
                                    Its points are already placed, so the correction needs the sensor's own frame to move them back into first. Which frame is the sensor?
                                    <Row style={{ marginTop: 6 }}>
                                        <Select size="sm" value={tfOptions.sensorFrame ?? ""} onChange={(sensorFrame: string) => setTf({ sensorFrame })} data-option="sensorFrame"
                                            options={[["", "pick the sensor frame…"], ...sensorChoices.map((f) => [f.name, `${f.name}${f.dynamic ? " (moving)" : ""}`])]} />
                                    </Row>
                                </Alert>
                            )}
                            {edge
                                ? (
                                    <Row wrap gap={8}>
                                        <Text small>writes <b>{edge.grandparent} → {edge.parent}</b>{edge.exists ? " (replacing the one there)" : " (a new root above it)"}</Text>
                                        {!edge.exists && <TextInput size="sm" value={tfOptions.grandparent || "world"} onChange={(grandparent: string) => setTf({ grandparent })} data-option="grandparent" style={{ width: 120 }} />}
                                    </Row>
                                )
                                : sensor && <Text small tone="danger">Nothing moves {sensor}: every edge above it is static.</Text>}
                            {tfOptions.useTags && markers.length > 0 && (
                                <Field label="tags to leave out (on things that move)">
                                    <Row wrap gap={6}>
                                        {markers.map((m) => (
                                            <Chip key={m.id} selected={(tfOptions.excludeTags ?? []).includes(m.id)} tone={(tfOptions.excludeTags ?? []).includes(m.id) ? "warn" : undefined} data-tag={m.id}
                                                onChange={() => setTf({ excludeTags: (tfOptions.excludeTags ?? []).includes(m.id) ? tfOptions.excludeTags.filter((id: number) => id !== m.id) : [...(tfOptions.excludeTags ?? []), m.id] })}>
                                                {`tag ${m.id} (${m.sightings})`}
                                            </Chip>
                                        ))}
                                    </Row>
                                </Field>
                            )}
                            <Field inline label="quality">
                                <Segmented size="sm" value={tfOptions.quality} onChange={(quality: string) => setTf({ quality })} data-option="quality"
                                    items={[{ value: "fast", label: "Faster (loop closure)" }, { value: "high", label: "Higher quality (ICP stitching)", disabled: !status.tf.highQuality }]} />
                            </Field>
                            <Panel collapsible defaultOpen={false} title={`Advanced · ${tfOptions.quality === "high" ? "ICP stitching" : "loop closure"}, odometry noise`} data-advanced="tf">
                                {tfOptions.quality === "high"
                                    ? <>
                                        <Heading level={3}>ICP stitching (icp_stitch)</Heading>
                                        <Fields group={tfOptions.stitch} onChange={group(setTf, tfOptions, "stitch")} />
                                    </>
                                    : <>
                                        <Heading level={3}>Loop closure</Heading>
                                        <Fields group={tfOptions.pgo} onChange={group(setTf, tfOptions, "pgo")} only={["loop_search_radius", "loop_time_thresh", "loop_score_thresh", "loop_submap_half_range", "submap_resolution", "min_loop_detect_duration", "max_icp_iterations", "max_icp_correspondence_dist", "loop_rot_var"]} />
                                    </>}
                                <Heading level={3}>Pose graph</Heading>
                                <Fields group={tfOptions} onChange={setTf} only={["keyframeTranslationM", "keyframeRotationDeg", "every", "tfTolerance", "rateHz"]} />
                                <Fields group={tfOptions.odometry} onChange={group(setTf, tfOptions, "odometry")} />
                            </Panel>
                        </StepCard>

                        <StepCard number={3} step="globalMap" title="Global map" status={globalStatus} running={running} run={runGlobal} runLabel={built ? "Run this step…" : "Run this step"}>
                            <Checkbox value={globalOptions.useCorrectedTf} onChange={(useCorrectedTf: boolean) => setGlobal({ useCorrectedTf })} data-option="useCorrectedTf"
                                label={status.tf.corrected ? "use the corrected tf" : "use the corrected tf (step 2 runs first: the recording has none yet)"} />
                            <Checkbox value={globalOptions.build.rayTracing} onChange={(rayTracing: boolean) => setGlobal({ build: { ...globalOptions.build, rayTracing } })} data-option="rayTracing" label="ray tracing (free space clears what moved)" />
                            <Field inline label="voxel size">
                                <Row>
                                    <Slider min={0.02} max={0.2} step={0.01} value={globalOptions.build.voxelSize} showValue={false} onChange={(voxelSize: number) => setGlobal({ build: { ...globalOptions.build, voxelSize } })} style={{ width: 160 }} />
                                    <NumberInput size="sm" min={0.01} max={0.5} step={0.01} unit="m" value={globalOptions.build.voxelSize} onChange={(voxelSize: number) => setGlobal({ build: { ...globalOptions.build, voxelSize } })} data-option="voxelSize" style={{ width: 90 }} />
                                </Row>
                            </Field>
                            <Row wrap gap={16}>
                                <Field inline label="final decimation" title="m; the saved map is thinned to one point per voxel this size (0 = the voxel size)">
                                    <NumberInput size="sm" step={0.01} unit="m" value={globalOptions.decimation.voxel} onChange={(voxel: number) => setGlobal({ decimation: { ...globalOptions.decimation, voxel } })} data-option="decimationVoxel" style={{ width: 90 }} />
                                </Field>
                                <Checkbox value={globalOptions.decimation.removeOutliers} onChange={(removeOutliers: boolean) => setGlobal({ decimation: { ...globalOptions.decimation, removeOutliers } })} data-option="removeOutliers" label="remove stray points" />
                            </Row>
                            <Checkbox value={globalOptions.rrd.enabled} onChange={(enabled: boolean) => setGlobal({ rrd: { ...globalOptions.rrd, enabled } })} data-option="rrd" label="also write an .rrd (rerun) beside the recording" />
                            {globalOptions.rrd.enabled && (
                                <Column gap={4} data-rrd-streams>
                                    <Row wrap gap={16}>
                                        <Checkbox value={globalOptions.rrd.map} onChange={(map: boolean) => setGlobal({ rrd: { ...globalOptions.rrd, map } })} label="the map" />
                                        <Checkbox value={globalOptions.rrd.tags} onChange={(tags: boolean) => setGlobal({ rrd: { ...globalOptions.rrd, tags } })} label="AprilTags" />
                                        <Checkbox value={globalOptions.rrd.trajectory} onChange={(trajectory: boolean) => setGlobal({ rrd: { ...globalOptions.rrd, trajectory } })} label="paths" />
                                    </Row>
                                    {(globalOptions.rrd.streams as { name: string; enabled: boolean; hz: number }[]).map((stream, index) => (
                                        <Row key={stream.name} gap={8}>
                                            <Checkbox value={stream.enabled} data-rrd-stream={stream.name} label={stream.name}
                                                onChange={(enabled: boolean) => setGlobal({ rrd: { ...globalOptions.rrd, streams: globalOptions.rrd.streams.map((s: Options, i: number) => (i === index ? { ...s, enabled } : s)) } })} />
                                            <NumberInput size="sm" step={0.5} unit="Hz" value={stream.hz} disabled={!stream.enabled} style={{ width: 90 }}
                                                onChange={(hz: number) => setGlobal({ rrd: { ...globalOptions.rrd, streams: globalOptions.rrd.streams.map((s: Options, i: number) => (i === index ? { ...s, hz } : s)) } })} />
                                        </Row>
                                    ))}
                                </Column>
                            )}
                            <Panel collapsible defaultOpen={false} title="Advanced · ray tracer, decimation, build" data-advanced="globalMap">
                                <Heading level={3}>Ray tracer</Heading>
                                <Fields group={globalOptions.build.ray} disabled={!globalOptions.build.rayTracing} onChange={(patch) => setGlobal({ build: { ...globalOptions.build, ray: { ...globalOptions.build.ray, ...patch } } })} />
                                <Heading level={3}>Stray points</Heading>
                                <Fields group={globalOptions.decimation} only={["neighbors", "stdRatio"]} disabled={!globalOptions.decimation.removeOutliers} onChange={(patch) => setGlobal({ decimation: { ...globalOptions.decimation, ...patch } })} />
                                <Heading level={3}>Build</Heading>
                                <Fields group={globalOptions.build} only={["every", "maxRange", "tfTolerance"]} onChange={(patch) => setGlobal({ build: { ...globalOptions.build, ...patch } })} />
                                {!globalOptions.useCorrectedTf && (
                                    <Checkbox value={globalOptions.build.loopClosure} onChange={(loopClosure: boolean) => setGlobal({ build: { ...globalOptions.build, loopClosure } })} label="without the corrected tf: close loops while building (the classic Generate)" />
                                )}
                            </Panel>
                        </StepCard>
                    </>
                )}
                {stepsShown && ready && lite && classic && (
                    <Field inline label="voxel size">
                        <NumberInput size="sm" min={0.01} max={0.5} step={0.01} unit="m" value={classic.voxelSize} onChange={(voxelSize: number) => setClassic({ ...classic, voxelSize })} data-option="voxelSize" style={{ width: 90 }} />
                    </Field>
                )}
                </Column>
                </Panel>
                {confirming && (
                    <Alert
                        tone="warn"
                        title={confirming === "recorded" ? `Replace the current map with ${recorded}?` : "Regenerate over the current map?"}
                        data-regen-warning
                        actions={
                            <>
                                <Button size="sm" variant="danger" onClick={confirming === "recorded" ? useRecorded : runGlobal} data-action="confirm-regenerate">
                                    {confirming === "recorded" ? "Yes, replace it" : "Yes, regenerate"}
                                </Button>
                                <Button size="sm" onClick={() => setConfirming(null)}>
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
