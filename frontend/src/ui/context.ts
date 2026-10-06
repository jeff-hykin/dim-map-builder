// What every panel gets: the open session, the 3D scene, the local floor, and helpers that run an action with feedback.
import type { MapScene } from "../core/scene.ts"
import type { Session } from "../core/api.ts"
import { AUTO_RANGE, type FloorModel, type SliceRange } from "../core/slice.ts"
import type { CubeShade } from "../render/pointMaterial.ts"
import { DEFAULT_GRADIENT, GRADIENTS } from "../render/gradients.ts"
import { DEFAULT_PERIOD } from "../render/palette.ts"
import type { IconName } from "./Icon.tsx"
import type { Uploads } from "./useUploads.ts"

export type ViewMode = "3d" | "split" | "2d"

/** The edit orb's palette: one tool at a time, each with its own options card. "select" just looks around. */
export type ToolId = "select" | "clean" | "annotate" | "polygon" | "places" | "erase" | "brush" | "line" | "straighten" | "views"

export const TOOLS: { id: ToolId; icon: IconName; label: string; key: string; view: "2d" | "3d" | "any" }[] = [
    { id: "select", icon: "pointer", label: "Select / move around", key: "Escape", view: "any" },
    { id: "erase", icon: "eraser", label: "Erase", key: "x", view: "2d" },
    { id: "brush", icon: "edit", label: "Draw", key: "d", view: "2d" },
    { id: "line", icon: "line", label: "Draw a line", key: "l", view: "2d" },
    { id: "straighten", icon: "ruler", label: "Straighten a wall", key: "w", view: "2d" },
    { id: "polygon", icon: "polygon", label: "Polygon (an area with a height)", key: "p", view: "2d" },
    { id: "places", icon: "pin", label: "Named points and areas", key: "n", view: "2d" },
    { id: "annotate", icon: "cube", label: "Boxes, planes and points in 3D", key: "b", view: "3d" },
    { id: "clean", icon: "sparkle", label: "Clean up (floating specks, outliers, floor, walls, crop, level)", key: "c", view: "3d" },
    { id: "views", icon: "eye", label: "Saved 2D views", key: "", view: "any" },
]

/** the 2D view's camera: the world point at its center and its zoom */
export interface View2d {
    cx: number
    cy: number
    pixelsPerMeter: number
}

/** UI state the server keeps with the session, so a refresh puts everything back */
export interface UiState {
    mode: ViewMode
    /** the 2D pane's share of the width in split mode */
    splitShare: number
    paletteOpen: boolean
    tool: ToolId
    /** the storey the 2D view and its tools work on */
    planFloor: number
    /** the 3D view shows only planFloor's voxels (its band in the floor model); false = every floor */
    floorOnly: boolean
    slice: SliceRange
    view2d: View2d | null
    floorOverlay: boolean
    showPaths: { corrected: boolean; raw: boolean; loops: boolean }
    /** `period`: meters of height per color cycle when `gradient` is a repeating palette */
    look: { style: "voxel" | "disc" | "square" | "splat"; gradient: string; period?: number; scale: number; shade?: CubeShade }
    region: { center: [number, number, number]; size: [number, number, number]; yaw: number } | null
    selected: { kind: string; id: string } | null
    scope: "view" | "region" | "all"
}

export const DEFAULT_UI: UiState = {
    mode: "3d",
    splitShare: 0.38,
    paletteOpen: false,
    tool: "select",
    planFloor: 0,
    floorOnly: false,
    slice: AUTO_RANGE,
    view2d: null,
    floorOverlay: false,
    showPaths: { corrected: true, raw: false, loops: true },
    look: { style: "voxel", gradient: DEFAULT_GRADIENT, period: DEFAULT_PERIOD, scale: 1, shade: "soft" },
    region: null,
    selected: null,
    scope: "view",
}

/** A session's saved panels over the defaults. A look saved before the repeating palettes, on the old default ramp
 * (memworld, no period), or on colors that no longer exist, moves to the default colors; a ramp picked on purpose stays. */
export function restoreUi(saved: Partial<UiState>): UiState {
    const restored: UiState = { ...DEFAULT_UI, ...saved }
    const look = saved.look
    if (look && ((look.period === undefined && look.gradient === "memworld") || !GRADIENTS.includes(look.gradient))) {
        restored.look = { ...look, gradient: DEFAULT_UI.look.gradient, period: DEFAULT_UI.look.period }
    }
    return restored
}

/** the modals over the page */
export type Modal = "open" | "generate" | "save" | "share" | null

export interface Context {
    session: Session | null
    scene: MapScene | null
    /** the local floor of the current map (null until fetched, or with no map) */
    floor: FloorModel | null
    ui: UiState
    setUi: (patch: Partial<UiState>) => void
    /** picks a palette tool (switching to a view it works in) */
    pickTool: (tool: ToolId) => void
    setModal: (modal: Modal) => void
    /** runs `action`, shows its error (or `done` message) as a toast */
    run: <T>(action: Promise<T>, done?: string | ((value: T) => string)) => Promise<T | undefined>
    refresh: () => Promise<void>
    openRecording: (recording: { id: string; path: string; name: string; writable?: boolean }) => Promise<void>
    /** Desktop's cloud upload queue and login */
    uploads: Uploads
    /** uploads the open recording; `saveFirst` saves the edits into it first */
    uploadRecording: (saveFirst: boolean) => Promise<void>
}
