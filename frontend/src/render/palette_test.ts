// deno test frontend/src/render: the repeating height palettes.
import { deepStrictEqual, ok, strictEqual } from "node:assert"
import { cyclePhase, DEFAULT_PERIOD, heightColor, otherPalette, PALETTE_NAMES, PALETTES, paletteAt } from "./palette.ts"

const distance = (a: number[], b: number[]) => Math.hypot(a[0] - b[0], a[1] - b[1], a[2] - b[2])
const hex = (color: string) => [1, 3, 5].map((at) => parseInt(color.slice(at, at + 2), 16))

Deno.test("the cycle repeats every period, below zero too", () => {
    strictEqual(DEFAULT_PERIOD, 3)
    for (const name of PALETTE_NAMES) {
        for (const height of [-7.3, -0.4, 0, 0.9, 1.5, 2.99, 4.2, 31.7]) {
            const color = heightColor(name, height)
            ok(distance(color, heightColor(name, height + 3)) < 1e-6, `${name} at ${height} vs +3 m`)
            ok(distance(color, heightColor(name, height - 30)) < 1e-6, `${name} at ${height} vs -30 m`)
        }
    }
    strictEqual(cyclePhase(-0.75), 0.75)
    strictEqual(cyclePhase(4.5), 0.5)
    strictEqual(cyclePhase(1, 2), 0.5)
})

Deno.test("each period runs c1 → c2 → c3 at even thirds", () => {
    for (const name of PALETTE_NAMES) {
        const [c1, c2, c3] = PALETTES[name].colors.map(hex)
        deepStrictEqual(heightColor(name, 0), c1)
        deepStrictEqual(heightColor(name, 1), c2)
        deepStrictEqual(heightColor(name, 2), c3)
        deepStrictEqual(heightColor(name, 3), c1)
    }
})

Deno.test("no seam: the color moves smoothly everywhere, across the wrap as much as inside a period", () => {
    for (const name of PALETTE_NAMES) {
        const step = 0.001
        let largest = 0
        for (let phase = -0.5; phase < 1.5; phase += step) {
            largest = Math.max(largest, distance(paletteAt(name, phase), paletteAt(name, phase + step)))
        }
        // at most the steepest segment's slope (≤ 3 · 256 · √3 per turn) times the step: no jump anywhere
        ok(largest < 3 * 256 * Math.sqrt(3) * step, `${name} jumps by ${largest}`)
        ok(distance(paletteAt(name, 1 - 1e-9), paletteAt(name, 0)) < 1e-3, `${name} wraps without a seam`)
    }
})

Deno.test("the palettes are clearly apart, and a second cloud gets a different one", () => {
    for (const a of PALETTE_NAMES) {
        for (const b of PALETTE_NAMES) {
            if (a === b) {
                continue
            }
            // the closest pair of colors along the two cycles, sampled together, stays well apart
            let closest = Infinity
            for (let phase = 0; phase < 1; phase += 0.01) {
                closest = Math.min(closest, distance(paletteAt(a, phase), paletteAt(b, phase)))
            }
            ok(closest > 60, `${a} and ${b} come within ${closest.toFixed(1)} of each other`)
        }
        ok(otherPalette(a) !== a)
        ok(PALETTE_NAMES.includes(otherPalette(a)))
    }
    ok(PALETTE_NAMES.length >= 3)
    ok(PALETTE_NAMES.includes(otherPalette("turbo")))
})
