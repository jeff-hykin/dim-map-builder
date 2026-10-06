// The repeating height palettes: three colors blended c1 → c2 → c3 → c1 over every `period` meters of height, so a tall
// or large map shows its floors apart without one ramp stretched thin over the whole range. Each cloud on screen gets its
// own palette (the map one, the raw scans the next), so two clouds read apart. No three.js: Deno's tests import it.

export type Rgb = [number, number, number]

/** Softened neon: about 70% saturation, 90% value, bright enough to glow on the dark scene without stinging. */
export const PALETTES: Record<string, { label: string; colors: [string, string, string] }> = {
    neon: { label: "Neon (cyan, blue, violet)", colors: ["#44dcdc", "#5080e6", "#a066e6"] },
    ember: { label: "Ember (amber, coral, magenta)", colors: ["#eeb050", "#ec7468", "#d660c4"] },
    lime: { label: "Lime (lime, green, teal)", colors: ["#b4e05c", "#56d88c", "#36bcb4"] },
}

export const PALETTE_NAMES = Object.keys(PALETTES)
export const DEFAULT_PALETTE = "neon"
/** meters of height one c1 → c2 → c3 → c1 cycle spans */
export const DEFAULT_PERIOD = 3

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

/** The palette's color at a phase (0..1, wrapping) as 0..255 rgb: linear in sRGB between the three evenly spaced colors. */
export function paletteAt(name: string, phase: number): Rgb {
    const colors = (PALETTES[name] ?? PALETTES[DEFAULT_PALETTE]).colors.map(hexToRgb)
    const position = (phase - Math.floor(phase)) * colors.length
    const index = Math.floor(position) % colors.length
    const mix = position - Math.floor(position)
    const [a, b] = [colors[index], colors[(index + 1) % colors.length]]
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
