// Copied from dim-live-viewer frontend/src/core/render (4a0c5aa) — the Map Editor draws with the same point styles.
// Rendering choices that apply to every point layer: the default point style (a layer can override it) and the
// automatic fallback that swaps splats for cubes when they can't keep up.
import { persistentStore, Store } from "../core/store.ts"
import type { PointStyle } from "./pointMaterial.ts"

export const rendering = persistentStore<{ pointStyle: PointStyle }>("lv.rendering", { pointStyle: "splat" })

/** Set while splats are being drawn as cubes because frames took longer than FRAME_BUDGET_MS for a while. */
export const splatFallback = new Store<{ active: boolean; frameMs: number }>({ active: false, frameMs: 0 })

/** The style a layer draws with: its own choice, else the global default, with the fallback applied. */
export function resolveStyle(style: PointStyle | "default"): PointStyle {
    const chosen = style === "default" ? rendering.get().pointStyle : style
    return chosen === "splat" && splatFallback.get().active ? "voxel" : chosen
}

export const FRAME_BUDGET_MS = 16
