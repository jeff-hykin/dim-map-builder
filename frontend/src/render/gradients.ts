// Copied from dim-live-viewer frontend/src/core/render (4a0c5aa) — the Map Editor draws with the same point styles.
// Color ramps for point clouds and other scalar coloring, as 256×1 textures the shaders sample. The repeating height
// palettes (palette.ts) are textures too, wrapping (RepeatWrapping), so the cycle's repeat has no seam.
import * as THREE from "three"
import { DEFAULT_PALETTE, isPalette, paletteAt, PALETTE_NAMES, PALETTES } from "./palette.ts"

/** Stops as [position 0..1, "#rrggbb"]; piecewise-linear in sRGB. */
const RAMPS: Record<string, [number, string][]> = {
    // MemWorld's height ramp (memory_world/replay_serving.py HEIGHT_COLOR_STOPS): purple, blue, cyan, mint
    memworld: [[0, "#6e1eaa"], [1 / 3, "#285aeb"], [2 / 3, "#28c8e6"], [1, "#96f096"]],
    // Turbo (Google's improved rainbow): the best general height ramp; every band reads apart
    // the floor is usually the lowest band, so the ramp starts at turbo's blue rather than its near-black
    turbo: [[0, "#3e5fd8"], [0.2, "#4675ed"], [0.3, "#39a2fc"], [0.4, "#1bcfd4"], [0.5, "#24eca6"], [0.6, "#61fc6c"], [0.7, "#a4fc3b"], [0.8, "#d1e834"], [0.9, "#f3c63a"], [1, "#fe9b2d"]],
    viridis: [[0, "#440154"], [0.25, "#3b528b"], [0.5, "#21918c"], [0.75, "#5ec962"], [1, "#fde725"]],
    magma: [[0, "#000004"], [0.25, "#51127c"], [0.5, "#b73779"], [0.75, "#fc8961"], [1, "#fcfdbf"]],
    plasma: [[0, "#0d0887"], [0.25, "#7e03a8"], [0.5, "#cc4778"], [0.75, "#f89540"], [1, "#f0f921"]],
    ice: [[0, "#0b1d3a"], [0.35, "#1f5f9e"], [0.7, "#7fc8f8"], [1, "#f4fbff"]],
    sunset: [[0, "#1a1046"], [0.3, "#7b2d7a"], [0.6, "#e0565b"], [0.85, "#f6a04d"], [1, "#fde39b"]],
    grayscale: [[0, "#1a1a1a"], [1, "#f2f2f2"]],
}

/** the repeating palettes first (the default), then the ramps stretched over the map's height range */
export const GRADIENTS = [...PALETTE_NAMES, ...Object.keys(RAMPS)]
export const DEFAULT_GRADIENT = DEFAULT_PALETTE

function hexToRgb(hex: string): [number, number, number] {
    const value = parseInt(hex.slice(1), 16)
    return [(value >> 16) & 255, (value >> 8) & 255, value & 255]
}

/** The ramp's color at t (0..1) as 0..255 rgb. */
export function sampleGradient(name: string, t: number): [number, number, number] {
    if (isPalette(name) || !RAMPS[name]) {
        return paletteAt(isPalette(name) ? name : DEFAULT_PALETTE, t)
    }
    const stops = RAMPS[name]
    const clamped = Math.min(1, Math.max(0, t))
    for (let index = 1; index < stops.length; index++) {
        const [position, hex] = stops[index]
        if (clamped <= position) {
            const [lastPosition, lastHex] = stops[index - 1]
            const mix = (clamped - lastPosition) / (position - lastPosition || 1)
            const a = hexToRgb(lastHex)
            const b = hexToRgb(hex)
            return [a[0] + (b[0] - a[0]) * mix, a[1] + (b[1] - a[1]) * mix, a[2] + (b[2] - a[2]) * mix]
        }
    }
    return hexToRgb(stops[stops.length - 1][1])
}

const textures = new Map<string, THREE.DataTexture>()

export function gradientTexture(name: string): THREE.DataTexture {
    let texture = textures.get(name)
    if (!texture) {
        const data = new Uint8Array(256 * 4)
        // a palette's texels sit at their centers in one wrapping cycle; a ramp's run end to end
        const cyclic = isPalette(name)
        for (let index = 0; index < 256; index++) {
            const [r, g, b] = sampleGradient(name, cyclic ? (index + 0.5) / 256 : index / 255)
            data.set([r, g, b, 255], index * 4)
        }
        texture = new THREE.DataTexture(data, 256, 1)
        if (cyclic) {
            texture.wrapS = THREE.RepeatWrapping
        }
        texture.colorSpace = THREE.SRGBColorSpace
        texture.magFilter = THREE.LinearFilter
        texture.minFilter = THREE.LinearFilter
        texture.needsUpdate = true
        textures.set(name, texture)
    }
    return texture
}

/** CSS linear-gradient for a ramp (settings swatches). */
export function gradientCss(name: string): string {
    if (!RAMPS[name]) {
        const colors = (PALETTES[name] ?? PALETTES[DEFAULT_PALETTE]).colors
        return `linear-gradient(90deg, ${[...colors, ...colors.slice(0, -1).reverse()].join(", ")})`
    }
    const stops = RAMPS[name]
    return `linear-gradient(90deg, ${stops.map(([position, hex]) => `${hex} ${position * 100}%`).join(", ")})`
}
