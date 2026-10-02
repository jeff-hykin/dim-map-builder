// What every panel gets: the open session, the 3D scene, and helpers that run an action with feedback.
import type { MapScene } from "../core/scene.ts"
import type { Session } from "../core/api.ts"

export type Stage = "open" | "build" | "clean" | "annotate" | "plans" | "save"

export const STAGES: { id: Stage; label: string; key: string }[] = [
    { id: "open", label: "Open", key: "1" },
    { id: "build", label: "Build map", key: "2" },
    { id: "clean", label: "Clean", key: "3" },
    { id: "annotate", label: "Annotate", key: "4" },
    { id: "plans", label: "Floor plans", key: "5" },
    { id: "save", label: "Save", key: "6" },
]

/** UI state the server keeps with the session, so a refresh puts everything back */
export interface UiState {
    stage: Stage
    planFloor: number
    showPaths: { corrected: boolean; raw: boolean; loops: boolean }
    look: { style: "voxel" | "disc" | "square" | "splat"; gradient: string; scale: number }
    region: { center: [number, number, number]; size: [number, number, number]; yaw: number } | null
    selected: { kind: string; id: string } | null
    scope: "view" | "region" | "all"
}

export const DEFAULT_UI: UiState = {
    stage: "open",
    planFloor: 0,
    showPaths: { corrected: true, raw: false, loops: true },
    look: { style: "voxel", gradient: "memworld", scale: 1 },
    region: null,
    selected: null,
    scope: "view",
}

export interface Context {
    session: Session | null
    scene: MapScene | null
    ui: UiState
    setUi: (patch: Partial<UiState>) => void
    /** runs `action`, shows its error (or `done` message) as a toast */
    run: <T>(action: Promise<T>, done?: string | ((value: T) => string)) => Promise<T | undefined>
    refresh: () => Promise<void>
    openRecording: (recording: { id: string; path: string; name: string; writable?: boolean }) => Promise<void>
}
