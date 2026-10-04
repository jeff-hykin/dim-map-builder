// Copied from dim-live-viewer frontend/src/core/render (4a0c5aa) — the Map Editor draws with the same point styles.
// Screen-space thick lines (three's LineSegments2) over a preallocated buffer, so trails and paths can grow or be
// replaced every message without allocating: write segments, bump the count.
import * as THREE from "three"
import { LineMaterial } from "three/examples/jsm/lines/LineMaterial.js"
import { LineSegments2 } from "three/examples/jsm/lines/LineSegments2.js"
import { LineSegmentsGeometry } from "three/examples/jsm/lines/LineSegmentsGeometry.js"

export class FatLines {
    readonly object: LineSegments2
    readonly material: LineMaterial
    #geometry = new LineSegmentsGeometry()
    #positions!: Float32Array
    #colors: Float32Array | null = null
    #positionBuffer!: THREE.InstancedInterleavedBuffer
    #colorBuffer: THREE.InstancedInterleavedBuffer | null = null
    #capacity = 0
    /** first segment changed since the last commit */
    #dirtyFrom = 0
    count = 0

    constructor(resolution: THREE.Vector2, options: { width: number; color?: THREE.ColorRepresentation; vertexColors?: boolean; capacity?: number; opacity?: number; dashed?: boolean; worldUnits?: boolean }) {
        this.material = new LineMaterial({
            linewidth: options.width,
            color: options.vertexColors ? 0xffffff : options.color ?? 0xffffff,
            vertexColors: !!options.vertexColors,
            transparent: (options.opacity ?? 1) < 1,
            opacity: options.opacity ?? 1,
            dashed: !!options.dashed,
            // worldUnits: width in meters (marker lines); otherwise CSS pixels
            worldUnits: !!options.worldUnits,
        })
        this.material.uniforms.resolution.value = resolution
        this.object = new LineSegments2(this.#geometry, this.material)
        this.object.frustumCulled = false
        this.#allocate(options.capacity ?? 64, !!options.vertexColors)
    }

    #allocate(capacity: number, withColors: boolean) {
        const positions = new Float32Array(capacity * 6)
        if (this.#positions) {
            positions.set(this.#positions.subarray(0, this.count * 6))
        }
        this.#positions = positions
        this.#dirtyFrom = 0
        this.#positionBuffer = new THREE.InstancedInterleavedBuffer(positions, 6, 1).setUsage(THREE.DynamicDrawUsage)
        this.#geometry.setAttribute("instanceStart", new THREE.InterleavedBufferAttribute(this.#positionBuffer, 3, 0))
        this.#geometry.setAttribute("instanceEnd", new THREE.InterleavedBufferAttribute(this.#positionBuffer, 3, 3))
        if (withColors) {
            const colors = new Float32Array(capacity * 6)
            if (this.#colors) {
                colors.set(this.#colors.subarray(0, this.count * 6))
            }
            this.#colors = colors
            this.#colorBuffer = new THREE.InstancedInterleavedBuffer(colors, 6, 1).setUsage(THREE.DynamicDrawUsage)
            this.#geometry.setAttribute("instanceColorStart", new THREE.InterleavedBufferAttribute(this.#colorBuffer, 3, 0))
            this.#geometry.setAttribute("instanceColorEnd", new THREE.InterleavedBufferAttribute(this.#colorBuffer, 3, 3))
        }
        this.#capacity = capacity
    }

    #reserve(count: number) {
        if (count > this.#capacity) {
            this.#allocate(Math.max(count, this.#capacity * 2), this.#colors !== null)
        }
    }

    clear() {
        this.count = 0
        this.#dirtyFrom = 0
        this.#geometry.instanceCount = 0
    }

    /** Appends one segment (and its color when the lines have vertex colors). */
    push(ax: number, ay: number, az: number, bx: number, by: number, bz: number, color?: THREE.Color) {
        this.#reserve(this.count + 1)
        this.#dirtyFrom = Math.min(this.#dirtyFrom, this.count)
        const at = this.count * 6
        const positions = this.#positions
        positions[at] = ax
        positions[at + 1] = ay
        positions[at + 2] = az
        positions[at + 3] = bx
        positions[at + 4] = by
        positions[at + 5] = bz
        const colors = this.#colors
        if (colors && color) {
            colors[at] = colors[at + 3] = color.r
            colors[at + 1] = colors[at + 4] = color.g
            colors[at + 2] = colors[at + 5] = color.b
        }
        this.count++
    }

    /** Drops the oldest `count` segments (a trail over its limit). */
    shift(count: number) {
        count = Math.min(count, this.count)
        this.#positions.copyWithin(0, count * 6, this.count * 6)
        this.#colors?.copyWithin(0, count * 6, this.count * 6)
        this.count -= count
        this.#dirtyFrom = 0
    }

    /** Uploads what changed since the last commit: just the new segments, or everything after a shift. */
    commit() {
        const from = Math.min(this.#dirtyFrom, this.count) * 6
        const length = this.count * 6 - from
        for (const buffer of [this.#positionBuffer, this.#colorBuffer]) {
            if (buffer && length > 0) {
                buffer.clearUpdateRanges()
                buffer.addUpdateRange(from, length)
                buffer.needsUpdate = true
            }
        }
        this.#dirtyFrom = this.count
        this.#geometry.instanceCount = this.count
    }

    dispose() {
        this.#geometry.dispose()
        this.material.dispose()
    }
}

/** Twelve edges of a box centered at the origin with full sizes (x, y, z), pushed into `lines` through `matrix`. */
export function pushBox(lines: FatLines, matrix: THREE.Matrix4, size: { x: number; y: number; z: number }, color?: THREE.Color) {
    const hx = size.x / 2, hy = size.y / 2, hz = size.z / 2
    const corners = [
        [-hx, -hy, -hz], [hx, -hy, -hz], [hx, hy, -hz], [-hx, hy, -hz],
        [-hx, -hy, hz], [hx, -hy, hz], [hx, hy, hz], [-hx, hy, hz],
    ].map(([x, y, z]) => new THREE.Vector3(x, y, z).applyMatrix4(matrix))
    const edges = [[0, 1], [1, 2], [2, 3], [3, 0], [4, 5], [5, 6], [6, 7], [7, 4], [0, 4], [1, 5], [2, 6], [3, 7]]
    for (const [a, b] of edges) {
        lines.push(corners[a].x, corners[a].y, corners[a].z, corners[b].x, corners[b].y, corners[b].z, color)
    }
}
