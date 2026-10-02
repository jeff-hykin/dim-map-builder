// Client for this app's backend (server/src/api.rs, at ./api) and Desktop's shared recordings (../../recordings).
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

export interface Annotations {
    boxes: BoxAnnotation[]
    planes: PlaneAnnotation[]
    points: PointAnnotation[]
    floors: Floor[]
    planPoints: PlanPoint[]
    areas: Area[]
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

export interface Session {
    id: string
    recordingId: string
    recordingPath: string
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

export const api = {
    state: () => call<{ active: string | null; session: Session | null; recordingsDir: string }>("api/state"),
    open: (recording: { id: string; path: string; name: string }) => call<Session>("api/open", json(recording)),
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
    build: (id: string, options: Partial<BuildOptions>) => call<{ job: Job }>(`api/sessions/${id}/build`, json(options)),
    cancel: (id: string) => call(`api/sessions/${id}/job`, { method: "DELETE" }),
    op: (id: string, op: string, region: Region, params: Record<string, unknown> = {}, preview = false) =>
        call<OpResult>(`api/sessions/${id}/op`, json({ op, region, params, preview })),
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
    deliverCapture: (request: number, dataUrl: string) => fetch(here(`api/captures/${request}`), { method: "POST", body: dataUrl }),
}

/** Server events: job progress, session changes, capture / camera requests from the agent. */
export function events(onEvent: (event: Record<string, any>) => void): () => void {
    let source: EventSource | null = null
    let closed = false
    const connect = () => {
        source = new EventSource(here("api/events"))
        source.onmessage = (message) => {
            try {
                onEvent(JSON.parse(message.data))
            } catch {
                // a malformed event is dropped
            }
        }
        source.onerror = () => {
            source?.close()
            if (!closed) {
                setTimeout(connect, 1500)
            }
            onEvent({ type: "disconnected" })
        }
        source.onopen = () => onEvent({ type: "connected" })
    }
    connect()
    return () => {
        closed = true
        source?.close()
    }
}
