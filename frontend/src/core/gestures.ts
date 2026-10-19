// The 2D view's pointer gestures, as plain math and state (no DOM), so they're tested on their own (gestures_test.ts):
// a mouse's left button and one finger use the picked tool, a right / middle drag pans, the wheel and a two-finger
// pinch zoom about the point under them (two fingers also pan), and a double tap closes a polygon like a double-click.

/** the view's camera: the (turned-frame) world point at its center and its zoom */
export interface ViewCamera {
    cx: number
    cy: number
    pixelsPerMeter: number
}

export type Point = readonly [number, number]

export const MIN_ZOOM = 2
export const MAX_ZOOM = 2000

const clampZoom = (pixelsPerMeter: number) => Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, pixelsPerMeter))

/** the world point under screen point `at` (y down on screen, up in the world) */
export function screenToWorld(view: ViewCamera, at: Point, width: number, height: number): [number, number] {
    return [view.cx + (at[0] - width / 2) / view.pixelsPerMeter, view.cy - (at[1] - height / 2) / view.pixelsPerMeter]
}

/** the camera zoomed by `factor` about screen point `at` (the world point there stays there) */
export function zoomAt(view: ViewCamera, at: Point, factor: number, width: number, height: number): ViewCamera {
    const [wx, wy] = screenToWorld(view, at, width, height)
    const pixelsPerMeter = clampZoom(view.pixelsPerMeter * factor)
    return { cx: wx - (at[0] - width / 2) / pixelsPerMeter, cy: wy + (at[1] - height / 2) / pixelsPerMeter, pixelsPerMeter }
}

/** The camera after two fingers moved from `a0`, `b0` (with the camera `start`) to `a1`, `b1`: the world point under
 * their first midpoint is under their midpoint now (two-finger pan), scaled by how far apart they are (pinch). */
export function pinchView(start: ViewCamera, a0: Point, b0: Point, a1: Point, b1: Point, width: number, height: number): ViewCamera {
    const mid0: Point = [(a0[0] + b0[0]) / 2, (a0[1] + b0[1]) / 2]
    const mid1: Point = [(a1[0] + b1[0]) / 2, (a1[1] + b1[1]) / 2]
    const [wx, wy] = screenToWorld(start, mid0, width, height)
    const before = Math.hypot(b0[0] - a0[0], b0[1] - a0[1])
    const after = Math.hypot(b1[0] - a1[0], b1[1] - a1[1])
    const pixelsPerMeter = clampZoom(before > 1 ? start.pixelsPerMeter * (after / before) : start.pixelsPerMeter)
    return { cx: wx - (mid1[0] - width / 2) / pixelsPerMeter, cy: wy + (mid1[1] - height / 2) / pixelsPerMeter, pixelsPerMeter }
}

/** what a pointer going down does: use the picked tool, pan, start a pinch (a 2nd finger), or nothing (a 3rd finger,
 * another mouse button while one is held) */
export type PressKind = "tool" | "pan" | "pinch" | "ignore"

/** `touches`: fingers already down before this one */
export function classifyPress(pointerType: string, button: number, touches: number): PressKind {
    if (pointerType === "touch") {
        return touches === 0 ? "tool" : touches === 1 ? "pinch" : "ignore"
    }
    return button === 0 ? "tool" : button === 1 || button === 2 ? "pan" : "ignore"
}

/** The fingers on the view, in order of touching down; a pinch uses the first two. */
export class Touches {
    readonly #points = new Map<number, [number, number]>()

    down(id: number, at: Point) {
        this.#points.set(id, [at[0], at[1]])
    }

    /** false when `id` isn't a finger on the view */
    move(id: number, at: Point): boolean {
        if (!this.#points.has(id)) {
            return false
        }
        this.#points.set(id, [at[0], at[1]])
        return true
    }

    up(id: number) {
        this.#points.delete(id)
    }

    has(id: number) {
        return this.#points.has(id)
    }

    get count() {
        return this.#points.size
    }

    /** the first two fingers, or null with fewer */
    pair(): [[number, number], [number, number]] | null {
        const [a, b] = [...this.#points.values()]
        return a && b ? [[...a], [...b]] : null
    }
}

/** A second tap within `ms` and `radius` px of the first is a double tap (touch screens send no reliable dblclick). */
export class DoubleTap {
    #last: { at: Point; time: number } | null = null

    constructor(readonly ms = 350, readonly radius = 24) {}

    /** a tap at `at`, `time` ms; true when it completes a double tap (which then starts over) */
    tap(at: Point, time: number): boolean {
        const last = this.#last
        if (last && time - last.time <= this.ms && Math.hypot(at[0] - last.at[0], at[1] - last.at[1]) <= this.radius) {
            this.#last = null
            return true
        }
        this.#last = { at, time }
        return false
    }
}
