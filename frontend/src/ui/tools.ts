// The 2D view's tool state, shared by the panels that pick a tool and the view that uses it.
import { Store } from "../core/store.ts"

export type PlanTool = "point" | "area"

export const planTool = new Store<{ tool: PlanTool; draft: [number, number][]; name: string; kind: string; hover: [number, number] | null }>({
    tool: "point",
    draft: [],
    name: "",
    kind: "no-go",
    hover: null,
})

export const modifyTool = new Store<{
    /** erase brush radius, m */
    radius: number
    /** erase the whole column (to the next storey) instead of up to the slice's z-end */
    fullColumn: boolean
    /** draw brush / line width, m */
    width: number
    /** drawn voxels' height over the floor, m */
    height: number
    /** the straighten band's width (how far from the dragged line wall voxels are taken), m */
    band: number
    busy: boolean
}>({ radius: 0.3, fullColumn: false, width: 0.1, height: 1.0, band: 0.4, busy: false })

/** a polygon being drawn in 2D (it becomes a prism annotation) */
export const polygonTool = new Store<{ draft: [number, number][]; hover: [number, number] | null; label: string; height: number }>({
    draft: [],
    hover: null,
    label: "",
    height: 1.0,
})
