// deno test frontend/src/core/gestures_test.ts: the 2D view's touch and mouse gestures (pinch, pan, zoom, taps).
import { deepStrictEqual, ok, strictEqual } from "node:assert"
import { classifyPress, DoubleTap, MAX_ZOOM, MIN_ZOOM, pinchView, screenToWorld, Touches, type ViewCamera, zoomAt } from "./gestures.ts"

const [W, H] = [400, 300]
const near = (a: number, b: number, what: string) => ok(Math.abs(a - b) < 1e-9, `${what}: ${a} vs ${b}`)
const nearPoint = (a: number[], b: number[], what: string) => a.forEach((v, i) => near(v, b[i], `${what}[${i}]`))

Deno.test("one finger uses the tool, a second starts a pinch, a third is ignored; the mouse keeps left = tool, right / middle = pan", () => {
    strictEqual(classifyPress("touch", 0, 0), "tool")
    strictEqual(classifyPress("touch", 0, 1), "pinch")
    strictEqual(classifyPress("touch", 0, 2), "ignore")
    strictEqual(classifyPress("pen", 0, 0), "tool")
    strictEqual(classifyPress("mouse", 0, 0), "tool")
    strictEqual(classifyPress("mouse", 2, 0), "pan")
    strictEqual(classifyPress("mouse", 1, 0), "pan")
    strictEqual(classifyPress("mouse", 3, 0), "ignore")
})

Deno.test("pinching apart zooms in about the fingers' midpoint: the world point under it stays put", () => {
    const start: ViewCamera = { cx: 3, cy: -2, pixelsPerMeter: 50 }
    const [a0, b0] = [[150, 100], [250, 200]] as const
    const [a1, b1] = [[100, 50], [300, 250]] as const // twice as far apart, same midpoint
    const view = pinchView(start, a0, b0, a1, b1, W, H)
    near(view.pixelsPerMeter, 100, "zoom doubles")
    nearPoint(screenToWorld(view, [200, 150], W, H), screenToWorld(start, [200, 150], W, H), "the midpoint's world point")
})

Deno.test("two fingers moving together pan: the world point under them follows them", () => {
    const start: ViewCamera = { cx: 0, cy: 0, pixelsPerMeter: 20 }
    const [a0, b0] = [[100, 100], [140, 100]] as const
    const [a1, b1] = [[160, 130], [200, 130]] as const // both +60 px right, +30 px down
    const view = pinchView(start, a0, b0, a1, b1, W, H)
    near(view.pixelsPerMeter, 20, "no zoom")
    nearPoint(screenToWorld(view, [180, 130], W, H), screenToWorld(start, [120, 100], W, H), "what was under the fingers")
    near(view.cx, -3, "x moves left by 60 px / 20")
    near(view.cy, 1.5, "y moves up (screen down) by 30 px / 20")
})

Deno.test("a pinch is clamped to the zoom limits, and fingers on one spot don't divide by zero", () => {
    const start: ViewCamera = { cx: 0, cy: 0, pixelsPerMeter: 1000 }
    strictEqual(pinchView(start, [0, 0], [10, 0], [0, 0], [1000, 0], W, H).pixelsPerMeter, MAX_ZOOM)
    strictEqual(pinchView(start, [0, 0], [1000, 0], [0, 0], [1, 0], W, H).pixelsPerMeter, MIN_ZOOM)
    const same = pinchView(start, [5, 5], [5, 5], [9, 9], [9, 9], W, H)
    strictEqual(same.pixelsPerMeter, 1000)
    ok(Number.isFinite(same.cx) && Number.isFinite(same.cy))
})

Deno.test("the wheel zooms about the cursor", () => {
    const start: ViewCamera = { cx: 1, cy: 1, pixelsPerMeter: 40 }
    const view = zoomAt(start, [30, 250], 1.5, W, H)
    near(view.pixelsPerMeter, 60, "zoom")
    nearPoint(screenToWorld(view, [30, 250], W, H), screenToWorld(start, [30, 250], W, H), "the point under the cursor")
})

Deno.test("Touches: the first two fingers make the pinch; a lifted finger leaves it", () => {
    const touches = new Touches()
    touches.down(7, [1, 2])
    strictEqual(touches.pair(), null)
    touches.down(9, [3, 4])
    deepStrictEqual(touches.pair(), [[1, 2], [3, 4]])
    ok(touches.move(9, [5, 6]))
    ok(!touches.move(42, [0, 0]), "a pointer that isn't down")
    deepStrictEqual(touches.pair(), [[1, 2], [5, 6]])
    touches.up(7)
    strictEqual(touches.count, 1)
    strictEqual(touches.pair(), null)
})

Deno.test("DoubleTap: two taps close in time and place, not three, not far apart, not slow", () => {
    const taps = new DoubleTap(350, 24)
    ok(!taps.tap([100, 100], 0))
    ok(taps.tap([110, 105], 200), "close and quick")
    ok(!taps.tap([110, 105], 300), "the next tap starts over")
    ok(!taps.tap([200, 200], 400), "far from the last")
    ok(!taps.tap([200, 200], 1000), "too slow")
    ok(taps.tap([201, 199], 1100))
})
