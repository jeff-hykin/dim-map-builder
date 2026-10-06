// The shared map's voxel encoding (Share → as HTML): the voxels quantized to the voxel grid, sorted, each one stored
// as the gap to the previous one's grid index (a LEB128 varint: a run along x is a row of 0x01 bytes), gzipped.
// About a byte a voxel before gzip. No three.js and no DOM beyond (De)CompressionStream: Deno's tests import it.

/** The parts of the editor's slice the export applies (core/api.ts Slice). */
export interface CropBox {
    zMin: number
    zMax: number
    yaw: number
    xMin: number
    xMax: number
    yMin: number
    yMax: number
}

const MAGIC = 0x31564d44 // "DMV1", little-endian
const HEADER_BYTES = 48

/** The voxels the slice shows: z within its band and x, y within its crop in the frame turned by its yaw (the
 * test the point shader makes on the turned map, `world = Rz(yaw) · p`). Null keeps them all. */
export function cropToSlice(positions: Float32Array, slice: CropBox | null): Float32Array {
    if (!slice) {
        return positions
    }
    const cos = Math.cos(slice.yaw)
    const sin = Math.sin(slice.yaw)
    const kept = new Float32Array(positions.length)
    let length = 0
    for (let index = 0; index < positions.length; index += 3) {
        const x = positions[index]
        const y = positions[index + 1]
        const z = positions[index + 2]
        const turnedX = cos * x - sin * y
        const turnedY = sin * x + cos * y
        if (z < slice.zMin || z > slice.zMax || turnedX < slice.xMin || turnedX > slice.xMax || turnedY < slice.yMin || turnedY > slice.yMax) {
            continue
        }
        kept[length++] = x
        kept[length++] = y
        kept[length++] = z
    }
    return kept.slice(0, length)
}

async function pipe(bytes: Uint8Array, stream: CompressionStream | DecompressionStream): Promise<Uint8Array> {
    const piped = new Blob([bytes as BlobPart]).stream().pipeThrough(stream)
    return new Uint8Array(await new Response(piped).arrayBuffer())
}

export const gzip = (bytes: Uint8Array) => pipe(bytes, new CompressionStream("gzip"))
export const gunzip = (bytes: Uint8Array) => pipe(bytes, new DecompressionStream("gzip"))

/** Packs xyz voxels (f32) at grid `step` (the voxel size): positions come back within step / 2, exactly for voxels
 * on the grid (the map's voxel centers). Their order isn't kept. */
export async function packVoxels(positions: Float32Array, step: number): Promise<Uint8Array> {
    const count = positions.length / 3
    const origin = [Infinity, Infinity, Infinity]
    for (let index = 0; index < positions.length; index++) {
        origin[index % 3] = Math.min(origin[index % 3], positions[index])
    }
    if (!count) {
        origin.fill(0)
    }
    const cells = new Uint32Array(positions.length)
    const size = [1, 1, 1]
    for (let index = 0; index < positions.length; index++) {
        const axis = index % 3
        const cell = Math.round((positions[index] - origin[axis]) / step)
        cells[index] = cell
        size[axis] = Math.max(size[axis], cell + 1)
    }
    // the grid index z, y, x (row-major in x): far below 2^53, so exact as a double
    const keys = new Float64Array(count)
    for (let index = 0; index < count; index++) {
        keys[index] = (cells[index * 3 + 2] * size[1] + cells[index * 3 + 1]) * size[0] + cells[index * 3]
    }
    keys.sort()
    const bytes = new Uint8Array(HEADER_BYTES + count * 8)
    const header = new DataView(bytes.buffer)
    header.setUint32(0, MAGIC, true)
    header.setUint32(4, count, true)
    header.setFloat64(8, step, true)
    origin.forEach((value, axis) => header.setFloat64(16 + axis * 8, value, true))
    header.setUint32(40, size[0], true)
    header.setUint32(44, size[1], true)
    let at = HEADER_BYTES
    let previous = 0
    for (const key of keys) {
        let gap = key - previous
        previous = key
        while (gap >= 128) {
            bytes[at++] = (gap % 128) | 128
            gap = Math.floor(gap / 128)
        }
        bytes[at++] = gap
    }
    return gzip(bytes.subarray(0, at))
}

/** packVoxels's inverse: xyz f32, sorted by grid index. */
export async function unpackVoxels(packed: Uint8Array): Promise<{ positions: Float32Array; step: number }> {
    const bytes = await gunzip(packed)
    const header = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
    if (header.getUint32(0, true) !== MAGIC) {
        throw new Error("not a packed map (bad magic)")
    }
    const count = header.getUint32(4, true)
    const step = header.getFloat64(8, true)
    const origin = [0, 1, 2].map((axis) => header.getFloat64(16 + axis * 8, true))
    const sizeX = header.getUint32(40, true)
    const sizeY = header.getUint32(44, true)
    const positions = new Float32Array(count * 3)
    let at = HEADER_BYTES
    let key = 0
    for (let index = 0; index < count; index++) {
        let gap = 0
        let scale = 1
        let byte: number
        do {
            byte = bytes[at++]
            gap += (byte & 127) * scale
            scale *= 128
        } while (byte & 128)
        key += gap
        const x = key % sizeX
        const rest = (key - x) / sizeX
        const y = rest % sizeY
        const z = (rest - y) / sizeY
        positions[index * 3] = origin[0] + x * step
        positions[index * 3 + 1] = origin[1] + y * step
        positions[index * 3 + 2] = origin[2] + z * step
    }
    if (at !== bytes.length) {
        throw new Error(`packed map: ${bytes.length - at} bytes left over`)
    }
    return { positions, step }
}

/** Base64 of bytes, in chunks (String.fromCharCode(...) of megabytes would overflow the stack). */
export function toBase64(bytes: Uint8Array): string {
    let text = ""
    for (let index = 0; index < bytes.length; index += 0x8000) {
        text += String.fromCharCode(...bytes.subarray(index, index + 0x8000))
    }
    return btoa(text)
}

export function fromBase64(text: string): Uint8Array {
    const binary = atob(text.trim())
    const bytes = new Uint8Array(binary.length)
    for (let index = 0; index < binary.length; index++) {
        bytes[index] = binary.charCodeAt(index)
    }
    return bytes
}

/** What the shared page shows besides the voxels: the editor's look, slice and floors when it was exported. */
export interface SharedMap {
    name: string
    /** ISO time of the export */
    exported: string
    voxelSize: number
    count: number
    look: { style: "voxel" | "disc" | "square" | "splat"; gradient: string; scale: number; shade?: string }
    /** the height color ramp's [low, high] in the editor (of the whole map, so colors match it) */
    range: [number, number]
    /** the slice: its yaw turns the map, its box clips it (the voxels are already cropped to it) */
    slice: CropBox | null
    /** the floor model's storeys, bottom up: each shows alone as its z band */
    floors: { level: number; band: [number, number] }[]
    /** the floor shown first, or null for every floor */
    floor: number | null
    /** the editor's camera (world frame, after the slice's turn), or null to frame the map */
    camera: { position: [number, number, number]; target: [number, number, number] } | null
}

const escapeHtml = (text: string) => text.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!)

/** The ids of the shared page's data blocks (the viewer reads them). */
export const SHARE_IDS = { meta: "dim-map-meta", voxels: "dim-map-voxels", code: "dim-map-viewer" } as const

/** The one-file page: the meta as JSON, the packed voxels and the gzipped viewer bundle as base64, and a few lines that
 * unzip the viewer and run it. Opens from disk with no network. */
export function shareHtml(meta: SharedMap, packedVoxels: Uint8Array, gzippedViewer: Uint8Array): string {
    // "<" escaped, so no "</script>" in the JSON ends its block
    const json = JSON.stringify(meta).replace(/</g, "\\u003c")
    return `<!doctype html>
<html lang="en" data-skin="portal">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<meta name="generator" content="dimOS Map Editor">
<title>${escapeHtml(meta.name)} · map</title>
<style>html, body { margin: 0; height: 100%; background: #05070d; color-scheme: dark; overflow: hidden; }</style>
</head>
<body>
<div id="dim-share"></div>
<script type="application/json" id="${SHARE_IDS.meta}">${json}</script>
<script type="application/octet-stream" id="${SHARE_IDS.voxels}">${toBase64(packedVoxels)}</script>
<script type="application/octet-stream" id="${SHARE_IDS.code}">${toBase64(gzippedViewer)}</script>
<script>
(async () => {
    const binary = atob(document.getElementById("${SHARE_IDS.code}").textContent.trim())
    const bytes = new Uint8Array(binary.length)
    for (let index = 0; index < binary.length; index++) {
        bytes[index] = binary.charCodeAt(index)
    }
    const code = await new Response(new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"))).text()
    const script = document.createElement("script")
    script.src = URL.createObjectURL(new Blob([code], { type: "text/javascript" }))
    document.body.append(script)
})()
</script>
</body>
</html>
`
}

/** Reads a shared page's parts back (the viewer, and the tests). */
export function readShareHtml(html: string): { meta: SharedMap; voxels: Uint8Array; code: Uint8Array } {
    const block = (id: string) => {
        const match = html.match(new RegExp(`<script [^>]*id="${id}">([^<]*)</script>`))
        if (!match) {
            throw new Error(`shared map: no ${id} block`)
        }
        return match[1]
    }
    return { meta: JSON.parse(block(SHARE_IDS.meta)), voxels: fromBase64(block(SHARE_IDS.voxels)), code: fromBase64(block(SHARE_IDS.code)) }
}
