// The repeating height palettes: the colors run up and back down over every `period` meters of height (a ping-pong:
// first → last over half a period, then last → first), so a tall or large map shows its floors apart without one ramp
// stretched thin over the whole range, and the repeat has no hue jump. Each cloud on screen gets its own palette (the
// map one, the raw scans the next), so two clouds read apart. No three.js: Deno's tests import it.

export type Rgb = [number, number, number]

/** Soft neon: bright enough to glow on the dark scene without stinging. Linear in sRGB between evenly spaced stops. */
export const PALETTES: Record<string, { label: string; colors: string[] }> = {
    aurora: { label: "Aurora (purple → blue → cyan → green)", colors: ["#a05cf0", "#5a86f2", "#3ed4e0", "#6ee68c"] },
    ember: { label: "Ember (gold → orange → red)", colors: ["#f2c64e", "#f08c4c", "#e8565e"] },
    rose: { label: "Rose (pink → blush → white)", colors: ["#f264b0", "#f2a6cc", "#f6e8ee"] },
}

export const PALETTE_NAMES = Object.keys(PALETTES)
export const DEFAULT_PALETTE = "aurora"
/** meters of height one full up-and-back cycle spans */
export const DEFAULT_PERIOD = 9

export function isPalette(name: string): boolean {
    return name in PALETTES
}

function hexToRgb(hex: string): Rgb {
    const value = parseInt(hex.slice(1), 16)
    return [(value >> 16) & 255, (value >> 8) & 255, value & 255]
}

/** Where in its cycle a height is, 0 (inclusive) to 1 (exclusive); negative heights wrap the same way. */
export function cyclePhase(height: number, period = DEFAULT_PERIOD): number {
    const turns = height / Math.max(1e-6, period)
    return turns - Math.floor(turns)
}

/** The palette's color at a phase (0..1, wrapping) as 0..255 rgb: first color at 0, last at 0.5, first again at 1. */
export function paletteAt(name: string, phase: number): Rgb {
    const colors = (PALETTES[name] ?? PALETTES[DEFAULT_PALETTE]).colors.map(hexToRgb)
    const wrapped = phase - Math.floor(phase)
    const along = (1 - Math.abs(2 * wrapped - 1)) * (colors.length - 1)
    const index = Math.min(colors.length - 2, Math.floor(along))
    const mix = along - index
    const [a, b] = [colors[index], colors[index + 1]]
    return [a[0] + (b[0] - a[0]) * mix, a[1] + (b[1] - a[1]) * mix, a[2] + (b[2] - a[2]) * mix]
}

/** The color of a point at this height. */
export function heightColor(name: string, height: number, period = DEFAULT_PERIOD): Rgb {
    return paletteAt(name, cyclePhase(height, period))
}

/** The palette for another cloud beside one drawn in `taken`: the next one, so the two never match. */
export function otherPalette(taken: string): string {
    const index = PALETTE_NAMES.indexOf(taken)
    return PALETTE_NAMES[(index + 1) % PALETTE_NAMES.length]
}
