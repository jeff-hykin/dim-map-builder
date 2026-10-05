// The 2D view's raster: a top-down slice of the current voxels between z-start and z-end, either over the local
// floor (the server's per-cell floor heights, so stairs and ramps stay in the slice) or as absolute heights.
import { sampleGradient } from "../render/gradients.ts"
import type { Slice as MapSlice } from "./api.ts"

export interface FloorStorey {
    level: number
    band: [number, number]
    heights: (number | null)[]
    measured: number[]
}

export interface FloorModel {
    mapVersion: number
    cell: number
    origin: [number, number]
    width: number
    height: number
    storeys: FloorStorey[]
}

export interface SliceRange {
    /** z-start / z-end over the local floor (true) or absolute map-frame z (false) */
    follow: boolean
    z0: number
    z1: number
}

export const AUTO_RANGE: SliceRange = { follow: true, z0: 0.1, z1: 1.8 }

/** A storey's floor at (x, y): bilinear between cell centers over the known ones (floor.rs's height_at). */
export function floorAt(model: FloorModel, storey: number, x: number, y: number): number | null {
    const heights = model.storeys[storey]?.heights
    if (!heights) {
        return null
    }
    const fx = (x - model.origin[0]) / model.cell - 0.5
    const fy = (y - model.origin[1]) / model.cell - 0.5
    const c0 = Math.floor(fx)
    const r0 = Math.floor(fy)
    const tx = fx - c0
    const ty = fy - r0
    let sum = 0
    let weights = 0
    for (const [dc, dr, weight] of [[0, 0, (1 - tx) * (1 - ty)], [1, 0, tx * (1 - ty)], [0, 1, (1 - tx) * ty], [1, 1, tx * ty]]) {
        const c = c0 + dc
        const r = r0 + dr
        if (c < 0 || r < 0 || c >= model.width || r >= model.height || weight <= 0) {
            continue
        }
        const value = heights[r * model.width + c]
        if (value !== null) {
            sum += value * weight
            weights += weight
        }
    }
    return weights > 1e-6 ? sum / weights : null
}

export interface Slice {
    origin: [number, number]
    resolution: number
    width: number
    height: number
    /** row-major from the origin (row 0 = lowest y): 0 unknown, 1 floor, 2 something in the slice */
    cells: Uint8Array
    /** what made it, so a redraw with the same inputs is skipped */
    key: string
    milliseconds: number
}

/** Rasterizes `points` (xyz f32) at `resolution`: a cell is "in the slice" when a voxel there is within the range,
 * "floor" when a voxel there is on the storey's local floor. */
/** Each 5 cm column's own walkable floor on a storey: the highest horizontal surface (a voxel with most of its 3 x 3
 * neighbours at the same height and little just above) within 0.3 m of the floor model there, so a stair tread or a step the 25 cm model
 * smooths over counts as floor. NaN where the column has none (the model's floor is used). Computed once per map and
 * storey; the slice reads it per voxel. */
export function columnFloors(points: Float32Array, model: FloorModel, storey: number, resolution = 0.05): Float32Array {
    const origin = model.origin
    const width = Math.ceil((model.width * model.cell) / resolution)
    const height = Math.ceil((model.height * model.cell) / resolution)
    const floors = new Float32Array(width * height).fill(NaN)
    // occupancy near the floor only (what the test below reads): per column, a 32-layer bit mask starting a little
    // under the floor model's height at the column's center (typed arrays: no hash set of every voxel)
    const base = new Int32Array(width * height)
    const known = new Uint8Array(width * height)
    for (let row = 0; row < height; row++) {
        for (let column = 0; column < width; column++) {
            const grid = floorAt(model, storey, origin[0] + (column + 0.5) * resolution, origin[1] + (row + 0.5) * resolution)
            if (grid !== null) {
                base[row * width + column] = Math.floor(grid / resolution) - BELOW
                known[row * width + column] = 1
            }
        }
    }
    const bits = new Uint32Array(width * height)
    for (let index = 0; index < points.length; index += 3) {
        const column = Math.floor((points[index] - origin[0]) / resolution)
        const row = Math.floor((points[index + 1] - origin[1]) / resolution)
        if (column >= 0 && row >= 0 && column < width && row < height) {
            const pixel = row * width + column
            const offset = Math.floor(points[index + 2] / resolution) - base[pixel]
            if (known[pixel] && offset >= 0 && offset < 32) {
                bits[pixel] |= 1 << offset
            }
        }
    }
    const count = (column: number, row: number, layer: number) => {
        let n = 0
        for (let dy = -1; dy <= 1; dy++) {
            for (let dx = -1; dx <= 1; dx++) {
                const pixel = (row + dy) * width + column + dx
                const offset = layer - base[pixel]
                if (known[pixel] && offset >= 0 && offset < 32 && bits[pixel] & (1 << offset)) {
                    n++
                }
            }
        }
        return n
    }
    for (let index = 0; index < points.length; index += 3) {
        const [x, y, z] = [points[index], points[index + 1], points[index + 2]]
        const column = Math.floor((x - origin[0]) / resolution)
        const row = Math.floor((y - origin[1]) / resolution)
        if (column < 1 || row < 1 || column >= width - 1 || row >= height - 1) {
            continue
        }
        const pixel = row * width + column
        if (floors[pixel] >= z || !known[pixel]) {
            continue
        }
        const grid = floorAt(model, storey, x, y)
        if (grid === null || Math.abs(z - grid) > 0.3) {
            continue
        }
        const layer = Math.floor(z / resolution)
        // a top surface: 7 of the 3 x 3 here, little just above (a wall isn't one)
        if (count(column, row, layer) >= 7 && count(column, row, layer + 1) <= 3) {
            floors[pixel] = z
        }
    }
    return floors
}

/** how many layers under the floor model's height a column's occupancy mask starts (0.3 m and a voxel of slack) */
const BELOW = 9

export function computeSlice(points: Float32Array, model: FloorModel, storey: number, range: SliceRange, resolution = 0.05, clip: MapSlice | null = null, ownFloors: Float32Array | null = null): Slice {
    const [cos, sin] = [Math.cos(clip?.yaw ?? 0), Math.sin(clip?.yaw ?? 0)]
    const started = performance.now()
    const origin: [number, number] = [model.origin[0], model.origin[1]]
    const scale = model.cell / resolution
    const width = Math.ceil(model.width * scale)
    const height = Math.ceil(model.height * scale)
    const cells = new Uint8Array(width * height)
    for (let index = 0; index < points.length; index += 3) {
        const x = points[index]
        const y = points[index + 1]
        const z = points[index + 2]
        const column = Math.floor((x - origin[0]) / resolution)
        const row = Math.floor((y - origin[1]) / resolution)
        if (column < 0 || row < 0 || column >= width || row >= height) {
            continue
        }
        // the slicer's box, in its turned frame
        if (clip) {
            const tx = cos * x - sin * y
            const ty = sin * x + cos * y
            if (z < clip.zMin || z > clip.zMax || tx < clip.xMin || tx > clip.xMax || ty < clip.yMin || ty > clip.yMax) {
                continue
            }
        }
        const pixel = row * width + column
        // the column's own walkable surface when it has one, else the model's floor
        const own = ownFloors ? ownFloors[pixel] : NaN
        const floor = own === own ? own : floorAt(model, storey, x, y)
        if (floor !== null && Math.abs(z - floor) <= 0.1) {
            if (cells[pixel] === 0) {
                cells[pixel] = 1
            }
        }
        const value = range.follow ? (floor === null ? NaN : z - floor) : z
        if (value >= range.z0 && value <= range.z1) {
            cells[pixel] = 2
        }
    }
    return { origin, resolution, width, height, cells, key: "", milliseconds: performance.now() - started }
}

/** The slice as two layers in the floor-plan look: a floor, and walls as a rim around a body. Portal (dark) is the glowing
 * blueprint; Research (light) is ink-blue walls on paper. `setPlanTheme` switches `PLAN_STYLE` in place. */
const PLAN_STYLES = {
    dark: {
        floor: [21, 33, 50],
        wallRim: [130, 212, 255],
        wallBody: [38, 78, 122],
        /** the walls' glow: their silhouette blurred (in raster cells) and tinted, drawn under them */
        glow: { color: "rgba(80, 170, 255, 0.45)", cells: 2 } as { color: string; cells: number } | null,
        grid: "rgba(140, 180, 230, 0.07)",
        gridText: "rgba(140, 180, 230, 0.5)",
    },
    light: {
        floor: [234, 233, 226],
        wallRim: [41, 60, 228],
        wallBody: [168, 178, 238],
        glow: null,
        grid: "rgba(32, 33, 31, 0.08)",
        gridText: "rgba(32, 33, 31, 0.5)",
    },
}
export const PLAN_STYLE = { ...PLAN_STYLES.dark }

export function setPlanTheme(dark: boolean) {
    Object.assign(PLAN_STYLE, dark ? PLAN_STYLES.dark : PLAN_STYLES.light)
}

/** Canvases from per-cell kinds (0 unknown, 1 floor, 2 wall; row 0 at the top = highest y): a floor, walls as a rim
 * around a body, and (Portal) the walls' glow, baked once here instead of a canvas filter on every frame drawn (a
 * full-screen blur per pan step was most of the 2D view's frame time). Bytes are written straight into the images. */
function paintLayers(kinds: Uint8Array, width: number, height: number, withGlow: boolean) {
    const floor = document.createElement("canvas")
    const walls = document.createElement("canvas")
    for (const canvas of [floor, walls]) {
        canvas.width = width
        canvas.height = height
    }
    const floorImage = floor.getContext("2d")!.createImageData(width, height)
    const wallImage = walls.getContext("2d")!.createImageData(width, height)
    const floorBytes = floorImage.data
    const wallBytes = wallImage.data
    const [fr, fg, fb] = PLAN_STYLE.floor
    const [rr, rg, rb] = PLAN_STYLE.wallRim
    const [br, bg, bb] = PLAN_STYLE.wallBody
    const isWall = (column: number, row: number) => column >= 0 && row >= 0 && column < width && row < height && kinds[row * width + column] === 2
    for (let row = 0; row < height; row++) {
        for (let column = 0; column < width; column++) {
            const kind = kinds[row * width + column]
            if (kind === 0) {
                continue
            }
            const offset = (row * width + column) * 4
            if (kind === 2) {
                const rim = !isWall(column - 1, row) || !isWall(column + 1, row) || !isWall(column, row - 1) || !isWall(column, row + 1)
                wallBytes[offset] = rim ? rr : br
                wallBytes[offset + 1] = rim ? rg : bg
                wallBytes[offset + 2] = rim ? rb : bb
                wallBytes[offset + 3] = 255
            }
            floorBytes[offset] = fr
            floorBytes[offset + 1] = fg
            floorBytes[offset + 2] = fb
            floorBytes[offset + 3] = 255
        }
    }
    floor.getContext("2d")!.putImageData(floorImage, 0, 0)
    walls.getContext("2d")!.putImageData(wallImage, 0, 0)
    let glow: HTMLCanvasElement | null = null
    if (PLAN_STYLE.glow && withGlow) {
        glow = document.createElement("canvas")
        glow.width = width
        glow.height = height
        const g = glow.getContext("2d")!
        g.filter = `blur(${PLAN_STYLE.glow.cells}px)`
        g.drawImage(walls, 0, 0)
        g.filter = "none"
        g.globalCompositeOperation = "source-in"
        g.fillStyle = PLAN_STYLE.glow.color
        g.fillRect(0, 0, width, height)
    }
    return { floor, walls, glow }
}

/** `withGlow`: bake the walls' glow too (the main 2D view; the minimap has none) */
export function sliceLayers(slice: Slice, withGlow: boolean) {
    const { width, height, cells } = slice
    // cells' row 0 is the lowest y; the images' row 0 is the top
    const kinds = new Uint8Array(width * height)
    for (let row = 0; row < height; row++) {
        kinds.set(cells.subarray((height - 1 - row) * width, (height - row) * width), row * width)
    }
    return paintLayers(kinds, width, height, withGlow)
}

/** The storey's floor heights as a colored image (top row = highest y): measured cells solid, filled ones faded. */
export function floorHeightImage(model: FloorModel, storey: number): { canvas: HTMLCanvasElement; range: [number, number] } | null {
    const data = model.storeys[storey]
    if (!data) {
        return null
    }
    const known = data.heights.filter((h): h is number => h !== null).sort((a, b) => a - b)
    if (!known.length) {
        return null
    }
    const range: [number, number] = [known[Math.floor(known.length * 0.02)], known[Math.floor((known.length - 1) * 0.98)]]
    const span = Math.max(0.2, range[1] - range[0])
    const canvas = document.createElement("canvas")
    canvas.width = model.width
    canvas.height = model.height
    const context = canvas.getContext("2d")!
    const image = context.createImageData(model.width, model.height)
    for (let row = 0; row < model.height; row++) {
        for (let column = 0; column < model.width; column++) {
            const value = data.heights[row * model.width + column]
            if (value === null) {
                continue
            }
            const [r, g, b] = sampleGradient("turbo", Math.min(1, Math.max(0, (value - range[0]) / span)))
            image.data.set([r, g, b, data.measured[row * model.width + column] ? 235 : 110], ((model.height - 1 - row) * model.width + column) * 4)
        }
    }
    context.putImageData(image, 0, 0)
    return { canvas, range }
}
