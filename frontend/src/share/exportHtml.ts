// Share → as HTML: the map as the 3D view shows it now (the current voxels, cropped to the saved slice, at the map's
// voxel size, in the editor's look, the floors and the camera) packed into one self-contained page with the viewer.
import type { Session } from "../core/api.ts"
import type { MapScene } from "../core/scene.ts"
import type { FloorModel } from "../core/slice.ts"
import type { UiState } from "../ui/context.ts"
import { cropToSlice, gzip, packVoxels, shareHtml, type SharedMap } from "./pack.ts"

export interface ShareExport {
    html: Blob
    fileName: string
    /** the voxels in the map / in the file */
    mapVoxels: number
    voxels: number
    /** the voxels as the editor holds them (xyz f32) */
    rawBytes: number
    /** the packed voxels and the gzipped viewer, before base64 */
    voxelBytes: number
    viewerBytes: number
    floors: number
    sliced: boolean
}

let viewerCode: Promise<Uint8Array> | null = null

/** dist/share-viewer.js (vite.share.config.ts), gzipped once per page. */
function gzippedViewer(): Promise<Uint8Array> {
    viewerCode ??= fetch(new URL("share-viewer.js", document.baseURI))
        .then((response) => {
            if (!response.ok) {
                throw new Error(`the share viewer (share-viewer.js) is missing: ${response.status}. Build it with npm run build`)
            }
            return response.arrayBuffer()
        })
        .then((code) => gzip(new Uint8Array(code)))
    viewerCode.catch(() => (viewerCode = null))
    return viewerCode
}

export async function exportHtml(scene: MapScene, session: Session, floor: FloorModel | null, ui: UiState): Promise<ShareExport> {
    const all = scene.map.positions as Float32Array
    const slice = session.annotations.slice ?? null
    const positions = cropToSlice(all, slice)
    const storeys = floor?.storeys ?? []
    const camera = scene.viewer.camera.position
    const target = scene.viewer.controls.target
    const meta: SharedMap = {
        name: session.name.replace(/\.(db|mcap)$/, ""),
        exported: new Date().toISOString(),
        voxelSize: scene.voxelSize,
        count: positions.length / 3,
        look: { ...scene.look },
        range: [...scene.map.range],
        slice,
        floors: storeys.map((storey) => ({ level: storey.level, band: [storey.band[0], storey.band[1]] })),
        floor: storeys.length > 1 && ui.floorOnly ? Math.min(ui.planFloor, storeys.length - 1) : null,
        camera: { position: [camera.x, camera.y, camera.z], target: [target.x, target.y, target.z] },
    }
    const [voxels, viewer] = await Promise.all([packVoxels(positions, scene.voxelSize), gzippedViewer()])
    return {
        html: new Blob([shareHtml(meta, voxels, viewer)], { type: "text/html" }),
        fileName: `${meta.name}_map.html`,
        mapVoxels: all.length / 3,
        voxels: meta.count,
        rawBytes: all.byteLength,
        voxelBytes: voxels.length,
        viewerBytes: viewer.length,
        floors: storeys.length,
        sliced: !!slice,
    }
}

export function formatBytes(bytes: number): string {
    return bytes < 1024 ? `${bytes} B` : bytes < 1 << 20 ? `${(bytes / 1024).toFixed(0)} KB` : `${(bytes / (1 << 20)).toFixed(1)} MB`
}
