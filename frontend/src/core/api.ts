// Client for this app's backend (server/src/api.rs, at ./api: the same endpoints Desktop's agent calls), Desktop's shared recordings (../../recordings) and
// Desktop's Dimensional cloud uploads (../../dimos/cloud, ../../dimos/uploads).
import type { FloorModel } from "./slice.ts"
import { appEvents } from "../dim-app/events.js"

export interface DesktopRecording {
    id: string
    name: string
    format: "mcap" | "db"
    size: number
    modified: number
    path: string
    writable: boolean
}

export interface RecordingStream {
    name: string
    type: string
    count: number
    start: number | null
    end: number | null
}

export interface RecordingMetadata extends DesktopRecording {
    start: number | null
    end: number | null
    duration: number | null
    streams: RecordingStream[]
}

export interface Box3 {
    center: [number, number, number]
    size: [number, number, number]
    yaw: number
}

export interface BoxAnnotation {
    id: string
    label: string
    box: Box3
    color?: string | null
    source: string
}

export interface PlaneAnnotation {
    id: string
    label: string
    center: [number, number, number]
    normal: [number, number, number]
    size: [number, number]
    source: string
}

export interface PointAnnotation {
    id: string
    label: string
    position: [number, number, number]
    source: string
}

export interface Floor {
    index: number
    name: string
    z: number
}

export interface PlanPoint {
    id: string
    floor: number
    name: string
    position: [number, number]
}

export interface Area {
    id: string
    floor: number
    name: string
    kind: string
    polygon: [number, number][]
}

/** a polygon drawn on a storey, extruded up from `base` by `height` (a box that follows the room's shape) */
export interface PrismAnnotation {
    id: string
    label: string
    floor: number
    polygon: [number, number][]
    base: number
    height: number
    source: string
}

/** a 2D map: a saved perspective on the 3D map (a storey, a height band, where it looks) */
export interface SavedView {
    id: string
    name: string
    floor: number
    follow: boolean
    zMin: number
    zMax: number
    center: [number, number] | null
    pixelsPerMeter: number | null
}

/** the slicer's view: map-frame z band, the turn that lines the walls up, and the crop in the turned frame */
export interface Slice {
    zMin: number
    zMax: number
    yaw: number
    xMin: number
    xMax: number
    yMin: number
    yMax: number
}

export interface Annotations {
    boxes: BoxAnnotation[]
    planes: PlaneAnnotation[]
    points: PointAnnotation[]
    floors: Floor[]
    planPoints: PlanPoint[]
    areas: Area[]
    prisms: PrismAnnotation[]
    views: SavedView[]
    slice?: Slice | null
}

export interface PlanInfo {
    index: number
    z: number
    resolution: number
    origin: [number, number]
    width: number
    height: number
    free: number
    occupied: number
    unknown: number
}

export interface Progress {
    stage: string
    stageIndex: number
    stageCount: number
    done: number
    total: number
    note: string
}

export interface Job {
    id: number
    session: string
    kind: "build" | "preview" | "save"
    state: "running" | "done" | "failed" | "cancelled"
    progress: Progress | null
    fraction: number
    startedAt: number
    elapsed: number
    etaSeconds: number | null
    error: string | null
}

export interface BuildSummary {
    voxelSize: number
    worldFrame: string
    cloudStream: string
    scansUsed: number
    scansSkipped: number
    loops: number
    notes: string[]
    seconds: number
}

export interface BuildOptions {
    cloudStream: string
    worldFrame: string
    voxelSize: number
    maxRange: number
    loopClosure: boolean
    every: number
    tfTolerance: number
}

/** the recording open before the server last started: offered on the empty page, not loaded */
export interface LastSession {
    id: string
    name: string
    recordingId: string
    path: string
    writable: boolean
    stage: string
}

export interface Session {
    id: string
    recordingId: string
    recordingPath: string
    writable: boolean
    name: string
    stage: "raw" | "map"
    build: BuildSummary | null
    buildOptions: BuildOptions | null
    transform: { translation: [number, number, number]; rotation: [number, number, number, number] }
    annotations: Annotations
    plans: PlanInfo[]
    view: Record<string, unknown> | null
    revision: number
    savedRevision: number
    savedAt: number | null
    history: string[]
    undoLabel: string | null
    redoLabel: string | null
    unsaved: boolean
    voxels: number
    mapVersion: number
    totalVoxels: number
    voxelSize: number | null
    bounds: [[number, number, number], [number, number, number]] | null
    job: Job | null
}

export type Region = { kind: "all" } | { kind: "view"; matrix: number[] } | ({ kind: "box" } & Box3)

export interface OpResult {
    label: string
    changed: number
    remaining: number
    preview?: [number, number, number][]
}

const here = (path: string) => new URL(path, location.href).href

async function call<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await fetch(here(path), { headers: { "content-type": "application/json" }, ...init })
    const body = await response.json().catch(() => ({}))
    if (!response.ok) {
        throw new Error(body.error ?? `${response.status} ${response.statusText}`)
    }
    return body as T
}

const json = (body: unknown): RequestInit => ({ method: "POST", body: JSON.stringify(body) })

/** Desktop's shared recordings folder (docs/api.md in dimos-desktop). */
export const recordings = {
    list: () => call<{ dir: string; extraDirs: string[]; recordings: DesktopRecording[] }>("../../recordings"),
    metadata: (id: string) => call<RecordingMetadata>(`../../recordings/${id.split("/").map(encodeURIComponent).join("/")}`),
}

/** Dimensional cloud login (Desktop's dimos server, docs/api.md in dimos-desktop) */
export interface CloudAccount {
    loggedIn: boolean
    email: string | null
    scopes: string[] | null
    source: "env" | "stored" | null
    cloudUrl: string
    error: string | null
}

export interface LoginState {
    state: "idle" | "starting" | "pending" | "approved" | "denied" | "expired" | "failed"
    url: string | null
    urlComplete: string | null
    code: string | null
    expiresAt: number | null
    email: string | null
    error: string | null
}

export type UploadState = "queued" | "uploading" | "done" | "failed" | "cancelled"

export interface Upload {
    id: string
    path: string
    name: string
    size: number
    robotId: string | null
    kind: string | null
    state: UploadState
    phase: string | null
    bytesDone: number
    bytesTotal: number
    rateBps: number | null
    etaSeconds: number | null
    uploadId: string | null
    skipped: boolean
    notice: string | null
    error: string | null
    errorCode: null | "not_logged_in" | "network" | "quota" | "file_missing" | "failed"
    log: string | null
    createdAt: number
    startedAt: number | null
    finishedAt: number | null
}

export const TOO_OLD_DESKTOP = "Uploading needs a newer dimOS Desktop (one with cloud uploads: the jeff/desktop_uploads branch or later)."

/** Desktop's own endpoints; a 404 / 405 means a Desktop from before they existed */
async function desktop<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await fetch(here(path), { headers: { "content-type": "application/json" }, ...init })
    const body = await response.json().catch(() => ({}))
    if (response.status === 404 || response.status === 405) {
        throw new Error(TOO_OLD_DESKTOP)
    }
    if (!response.ok) {
        throw new Error(body.error ?? `${response.status} ${response.statusText}`)
    }
    return body as T
}

export const cloud = {
    account: () => desktop<CloudAccount>("../../dimos/cloud/account"),
    login: () => desktop<LoginState>("../../dimos/cloud/login", { method: "POST" }),
    loginState: () => desktop<LoginState>("../../dimos/cloud/login"),
    cancelLogin: () => desktop<LoginState>("../../dimos/cloud/login", { method: "DELETE" }),
    logout: () => desktop<CloudAccount>("../../dimos/cloud/logout", { method: "POST" }),
}

export const uploads = {
    list: () => desktop<{ uploads: Upload[]; waitingForLogin: boolean }>("../../dimos/uploads"),
    /** cancels a queued / running one, removes a finished one */
    remove: (id: string) => desktop<{ ok: true }>(`../../dimos/uploads/${encodeURIComponent(id)}`, { method: "DELETE" }),
    retry: (id: string) => desktop<Upload>(`../../dimos/uploads/${encodeURIComponent(id)}/retry`, { method: "POST" }),
    clearFinished: () => desktop<{ uploads: Upload[] }>("../../dimos/uploads", { method: "DELETE" }),
}

export const api = {
    state: () => call<{ active: string | null; session: Session | null; last: LastSession | null; recordingsDir: string }>("api/state"),
    open: (recording: { id: string; path: string; name: string; writable?: boolean }) => call<Session>("api/open", json(recording)),
    session: (id: string) => call<Session>(`api/sessions/${id}`),
    discard: (id: string) => call(`api/sessions/${id}`, { method: "DELETE" }),
    points: async (id: string): Promise<Float32Array> => {
        const response = await fetch(here(`api/sessions/${id}/points.bin`))
        if (!response.ok) {
            throw new Error(`map: ${response.status}`)
        }
        return new Float32Array(await response.arrayBuffer())
    },
    paths: (id: string) => call<{ raw?: number[][]; corrected?: number[][]; loops?: number[][][] }>(`api/sessions/${id}/paths`),
    /** null while the preview is still being made (a job is running for it) */
    preview: async (id: string): Promise<{ points: Float32Array; path: Float32Array } | null> => {
        const response = await fetch(here(`api/sessions/${id}/preview.bin`))
        if (response.status === 202) {
            return null
        }
        if (!response.ok) {
            throw new Error((await response.json().catch(() => ({}))).error ?? `preview: ${response.status}`)
        }
        const buffer = await response.arrayBuffer()
        const header = new Uint32Array(buffer, 0, 2)
        return { points: new Float32Array(buffer, 8, header[0] * 3), path: new Float32Array(buffer, 8 + header[0] * 12, header[1] * 3) }
    },
    buildDefaults: () => call<Record<string, any>>("api/build-defaults"),
    build: (id: string, options: Partial<BuildOptions>) => call<{ job: Job }>(`api/sessions/${id}/build`, json(options)),
    cancel: (id: string) => call(`api/sessions/${id}/job`, { method: "DELETE" }),
    op: (id: string, op: string, region: Region, params: Record<string, unknown> = {}, preview = false) =>
        call<OpResult>(`api/sessions/${id}/op`, json({ op, region, params, preview })),
    modify: (id: string, body: Record<string, unknown>) => call<OpResult>(`api/sessions/${id}/modify`, json(body)),
    setSlice: (id: string, slice: Slice | null) => call(`api/sessions/${id}/slice`, { method: "PUT", body: JSON.stringify(slice) }),
    alignment: (id: string) => call<{ yaw: number | null }>(`api/sessions/${id}/alignment`),
    floor: (id: string) => call<FloorModel>(`api/sessions/${id}/floor`),
    rotate: (id: string, degrees: number) => call(`api/sessions/${id}/transform`, json({ kind: "rotate", degrees })),
    level: (id: string) => call(`api/sessions/${id}/transform`, json({ kind: "level" })),
    add: (id: string, annotation: Record<string, unknown>) => call<{ id: string }>(`api/sessions/${id}/annotations`, json(annotation)),
    patch: (id: string, annotation: string, patch: Record<string, unknown>) =>
        call(`api/sessions/${id}/annotations/${annotation}`, { method: "PATCH", body: JSON.stringify(patch) }),
    remove: (id: string, annotation: string) => call(`api/sessions/${id}/annotations/${annotation}`, { method: "DELETE" }),
    fitBox: (id: string, box: Box3) => call<{ box: Box3; voxels: number }>(`api/sessions/${id}/fit-box`, json({ box })),
    plans: (id: string, multiFloor: boolean) => call<{ floors: Floor[] }>(`api/sessions/${id}/plans`, json({ multiFloor })),
    planUrl: (id: string, index: number, revision: number) => here(`api/sessions/${id}/plans/${index}.png?r=${revision}`),
    undo: (id: string) => call<{ undone: string | null }>(`api/sessions/${id}/undo`, { method: "POST" }),
    redo: (id: string) => call<{ redone: string | null }>(`api/sessions/${id}/redo`, { method: "POST" }),
    view: (id: string, view: Record<string, unknown>) => call(`api/sessions/${id}/view`, { method: "PUT", body: JSON.stringify(view) }),
    save: (id: string) => call<{ job: Job }>(`api/sessions/${id}/save`, { method: "POST" }),
    /** queues the recording in Desktop's upload queue; with saveFirst and unsaved edits, after a save job */
    upload: (id: string, saveFirst: boolean) => call<{ upload?: Upload; job?: Job }>(`api/sessions/${id}/upload`, json({ saveFirst })),
    deliverCapture: (request: number, dataUrl: string) => fetch(here(`api/captures/${request}`), { method: "POST", body: dataUrl }),
}

/** Server events: job progress, session changes, capture / camera requests from the agent. */
export function events(onEvent: (event: Record<string, any>) => void): () => void {
    return appEvents(onEvent, {
        onOpen: () => onEvent({ type: "connected" }),
        onClose: () => onEvent({ type: "disconnected" }),
    })
}
