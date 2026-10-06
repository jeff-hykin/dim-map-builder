// deno test frontend/src/render: the repeating height palettes.
import { deepStrictEqual, ok, strictEqual } from "node:assert"
import { cyclePhase, DEFAULT_PERIOD, heightColor, otherPalette, PALETTE_NAMES, PALETTES, paletteAt } from "./palette.ts"

const distance = (a: number[], b: number[]) => Math.hypot(a[0] - b[0], a[1] - b[1], a[2] - b[2])
const hex = (color: string) => [1, 3, 5].map((at) => parseInt(color.slice(at, at + 2), 16))

Deno.test("the cycle repeats every period, below zero too", () => {
    strictEqual(DEFAULT_PERIOD, 6)
    for (const name of PALETTE_NAMES) {
        for (const height of [-7.3, -0.4, 0, 0.9, 1.5, 2.99, 4.2, 31.7]) {
            const color = heightColor(name, height)
            ok(distance(color, heightColor(name, height + 6)) < 1e-6, `${name} at ${height} vs +6 m`)
            ok(distance(color, heightColor(name, height - 30)) < 1e-6, `${name} at ${height} vs -30 m`)
        }
    }
    strictEqual(cyclePhase(-1.5), 0.75)
    strictEqual(cyclePhase(9), 0.5)
    strictEqual(cyclePhase(1, 2), 0.5)
})

Deno.test("each period runs up the stops and back down: first at 0, last at half, first again at the period", () => {
    for (const name of PALETTE_NAMES) {
        const stops = PALETTES[name].colors.map(hex)
        const step = 3 / (stops.length - 1)
        stops.forEach((stop, index) => {
            ok(distance(heightColor(name, index * step), stop) < 1e-6, `${name} up, stop ${index}`)
            ok(distance(heightColor(name, 6 - index * step), stop) < 1e-6, `${name} down, stop ${index}`)
        })
        ok(distance(heightColor(name, 6), stops[0]) < 1e-6)
    }
    deepStrictEqual(PALETTES.aurora.colors.length >= 3, true)
})

Deno.test("no seam: the color moves smoothly everywhere, across the wrap as much as inside a period", () => {
    for (const name of PALETTE_NAMES) {
        const step = 0.001
        let largest = 0
        for (let phase = -0.5; phase < 1.5; phase += step) {
            largest = Math.max(largest, distance(paletteAt(name, phase), paletteAt(name, phase + step)))
        }
        // at most the steepest segment's slope (≤ 2 · (stops - 1) · 256 · √3 per turn) times the step: no jump anywhere
        ok(largest < 2 * (PALETTES[name].colors.length - 1) * 256 * Math.sqrt(3) * step, `${name} jumps by ${largest}`)
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
