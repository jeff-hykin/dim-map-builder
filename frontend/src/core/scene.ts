// The 3D view: the map (GPU point sprites, the Live Viewer's styles), the paths, the annotations with an edit gizmo,
// a selection box the cleanup tools act in, click-to-pick on the map, and screenshots (with a labelled grid) for the
// agent. No React here; the UI drives it through methods and listens through callbacks.
import * as THREE from "three"
import { TransformControls } from "three/examples/jsm/controls/TransformControls.js"
import { Viewer } from "../render/viewer.ts"
import { applyLook, makePointMaterial, type PointLook, type PointStyle } from "../render/pointMaterial.ts"
import { FatLines, pushBox } from "../render/lines.ts"
import { LabelPool } from "../render/labels.ts"
import type { Annotations, Box3 } from "./api.ts"

export type Selectable = { kind: "box" | "plane" | "point"; id: string } | { kind: "region"; id: "region" }
export type GizmoMode = "translate" | "rotate" | "scale"

export interface MapLook {
    style: PointStyle
    gradient: string
    /** voxel edge multiplier: 1 = the map's voxel size */
    scale: number
}

const ACCENT = new THREE.Color("#7af0a8")
const AGENT = new THREE.Color("#7fc8f8")
const SELECTED = new THREE.Color("#ffd166")
const PREVIEW = new THREE.Color("#ff5f6d")

function percentileRange(values: Float32Array, axis: number, low = 0.05, high = 0.95): [number, number] {
    const count = values.length / 3
    if (!count) {
        return [0, 1]
    }
    const step = Math.max(1, Math.floor(count / 20000))
    const sample: number[] = []
    for (let index = 0; index < count; index += step) {
        sample.push(values[index * 3 + axis])
    }
    sample.sort((a, b) => a - b)
    const lo = sample[Math.floor(low * (sample.length - 1))]
    const hi = sample[Math.floor(high * (sample.length - 1))]
    return [lo, hi > lo ? hi : lo + 1]
}

class PointLayer {
    readonly points: THREE.Points
    readonly geometry = new THREE.BufferGeometry()
    readonly material: THREE.ShaderMaterial
    positions: Float32Array<ArrayBufferLike> = new Float32Array(0)
    range: [number, number] = [0, 1]

    constructor(viewer: Viewer) {
        this.material = makePointMaterial(viewer.pixelsPerMeter)
        this.points = new THREE.Points(this.geometry, this.material)
        this.points.frustumCulled = false
        this.points.renderOrder = 1
    }

    set(positions: Float32Array) {
        this.positions = positions
        const count = positions.length / 3
        this.geometry.setAttribute("position", new THREE.BufferAttribute(positions, 3))
        this.geometry.setAttribute("aTime", new THREE.BufferAttribute(new Float32Array(count), 1))
        this.geometry.setAttribute("aIntensity", new THREE.BufferAttribute(new Uint8Array(count), 1, true))
        this.geometry.setDrawRange(0, count)
        this.geometry.computeBoundingSphere()
        this.range = percentileRange(positions, 2)
    }

    look(look: PointLook) {
        applyLook(this.material, look, this.range)
    }
}

export class MapScene {
    readonly viewer: Viewer
    readonly map: PointLayer
    readonly raw: PointLayer
    readonly preview: PointLayer
    voxelSize = 0.05
    look: MapLook = { style: "voxel", gradient: "memworld", scale: 1 }
    #paths: FatLines
    #rawPath: FatLines
    #loops: FatLines
    #boxes: FatLines
    #labels = new LabelPool()
    #annotationGroup = new THREE.Group()
    #annotations: Annotations | null = null
    #pointMeshes = new Map<string, THREE.Mesh>()
    #planeMeshes = new Map<string, THREE.Mesh>()
    #gizmo: TransformControls
    #proxy = new THREE.Object3D()
    #selected: Selectable | null = null
    #region: Box3 | null = null
    #regionLines: FatLines
    #dragging = false
    /** the UI's callbacks */
    onSelect: (selection: Selectable | null) => void = () => {}
    onEdit: (selection: Selectable, change: { center: [number, number, number]; size: [number, number, number]; yaw: number; normal?: [number, number, number] }) => void = () => {}
    onPick: ((point: THREE.Vector3, event: PointerEvent) => void) | null = null
    onViewChange: () => void = () => {}

    constructor(host: HTMLElement) {
        this.viewer = new Viewer(host, () => Date.now())
        this.viewer.setTheme(true)
        this.map = new PointLayer(this.viewer)
        this.raw = new PointLayer(this.viewer)
        this.preview = new PointLayer(this.viewer)
        this.preview.points.renderOrder = 3
        const scene = this.viewer.scene
        scene.add(this.map.points, this.raw.points, this.preview.points)
        const resolution = this.viewer.resolution
        this.#paths = new FatLines(resolution, { width: 3, color: 0xff3df2 })
        this.#rawPath = new FatLines(resolution, { width: 1.5, color: 0x8a94a6, opacity: 0.7 })
        this.#loops = new FatLines(resolution, { width: 2, color: 0xffb454 })
        this.#boxes = new FatLines(resolution, { width: 2.5, vertexColors: true })
        this.#regionLines = new FatLines(resolution, { width: 2, color: 0xffd166, dashed: false })
        for (const lines of [this.#paths, this.#rawPath, this.#loops]) {
            lines.material.depthTest = false
            lines.object.renderOrder = 5
        }
        scene.add(this.#paths.object, this.#rawPath.object, this.#loops.object, this.#boxes.object, this.#regionLines.object, this.#labels.group, this.#annotationGroup)

        this.#gizmo = new TransformControls(this.viewer.camera, this.viewer.renderer.domElement)
        this.#gizmo.setSize(0.9)
        scene.add(this.#proxy)
        scene.add(this.#gizmo.getHelper())
        this.#gizmo.addEventListener("dragging-changed", (event) => {
            this.#dragging = !!event.value
            this.viewer.controls.enabled = !event.value
            if (!event.value) {
                this.#commitGizmo()
            }
        })
        this.#gizmo.addEventListener("objectChange", () => {
            this.#previewGizmo()
            this.viewer.requestRender()
        })
        this.#gizmo.addEventListener("change", () => this.viewer.requestRender())

        let downAt: { x: number; y: number } | null = null
        const canvas = this.viewer.renderer.domElement
        canvas.addEventListener("pointerdown", (event) => {
            downAt = { x: event.clientX, y: event.clientY }
        })
        canvas.addEventListener("pointerup", (event) => {
            if (!downAt || this.#dragging || Math.hypot(event.clientX - downAt.x, event.clientY - downAt.y) > 4) {
                return
            }
            this.#click(event)
        })
        let viewTimer = 0
        this.viewer.controls.addEventListener("change", () => {
            clearTimeout(viewTimer)
            viewTimer = window.setTimeout(() => this.onViewChange(), 350)
        })
    }

    // ---- data ----

    setMap(positions: Float32Array, voxelSize: number) {
        this.voxelSize = voxelSize
        this.map.set(positions)
        this.map.points.visible = positions.length > 0
        this.applyLook()
        this.viewer.requestRender()
    }

    setRaw(positions: Float32Array | null, path: Float32Array | null) {
        this.raw.set(positions ?? new Float32Array(0))
        this.raw.points.visible = !!positions?.length
        this.raw.look({ style: "disc", size: 0.08, colorMode: "height", gradient: this.look.gradient, axis: 2, rangeMin: null, rangeMax: null, solid: "#ffffff", opacity: 1 })
        this.#rawPath.clear()
        if (path) {
            for (let index = 3; index < path.length; index += 3) {
                this.#rawPath.push(path[index - 3], path[index - 2], path[index - 1], path[index], path[index + 1], path[index + 2])
            }
        }
        this.#rawPath.commit()
        this.viewer.requestRender()
    }

    setPaths(paths: { raw?: number[][]; corrected?: number[][]; loops?: number[][][] }) {
        const line = (lines: FatLines, points: number[][] | undefined) => {
            lines.clear()
            for (let index = 1; index < (points?.length ?? 0); index++) {
                const [a, b] = [points![index - 1], points![index]]
                lines.push(a[0], a[1], a[2], b[0], b[1], b[2])
            }
            lines.commit()
        }
        line(this.#paths, paths.corrected)
        this.#loops.clear()
        for (const [a, b] of paths.loops ?? []) {
            this.#loops.push(a[0], a[1], a[2], b[0], b[1], b[2])
        }
        this.#loops.commit()
        if (paths.raw && !this.raw.points.visible) {
            line(this.#rawPath, paths.raw)
        }
        this.viewer.requestRender()
    }

    showPaths(show: { corrected: boolean; raw: boolean; loops: boolean }) {
        this.#paths.object.visible = show.corrected
        this.#rawPath.object.visible = show.raw
        this.#loops.object.visible = show.loops
        this.viewer.requestRender()
    }

    setPreview(points: [number, number, number][] | null) {
        const flat = new Float32Array((points?.length ?? 0) * 3)
        points?.forEach((p, index) => flat.set(p, index * 3))
        this.preview.set(flat)
        this.preview.points.visible = flat.length > 0
        this.preview.look({ style: "square", size: this.voxelSize * 1.15, colorMode: "solid", gradient: "memworld", axis: 2, rangeMin: null, rangeMax: null, solid: `#${PREVIEW.getHexString()}`, opacity: 1 })
        this.viewer.requestRender()
    }

    applyLook(look: Partial<MapLook> = {}) {
        this.look = { ...this.look, ...look }
        this.map.look({
            style: this.look.style,
            size: this.voxelSize * this.look.scale,
            colorMode: "height",
            gradient: this.look.gradient,
            axis: 2,
            rangeMin: null,
            rangeMax: null,
            solid: "#ffffff",
            opacity: 1,
        })
        this.viewer.requestRender()
    }

    // ---- annotations ----

    setAnnotations(annotations: Annotations) {
        this.#annotations = annotations
        this.#drawAnnotations()
        // keep the gizmo on its annotation (it may have moved, e.g. undo)
        if (this.#selected && this.#selected.kind !== "region" && !this.#dragging) {
            this.#attach(this.#selected)
        }
    }

    #drawAnnotations() {
        const annotations = this.#annotations
        this.#boxes.clear()
        this.#labels.begin()
        const keepPoints = new Set<string>()
        const keepPlanes = new Set<string>()
        if (annotations) {
            for (const item of annotations.boxes) {
                const selected = this.#selected?.kind === "box" && this.#selected.id === item.id
                const color = selected ? SELECTED : item.source === "agent" ? AGENT : ACCENT
                const box = selected && this.#dragging ? this.#proxyBox() : item.box
                const matrix = new THREE.Matrix4().compose(new THREE.Vector3(...box.center), new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(0, 0, 1), box.yaw ?? 0), new THREE.Vector3(1, 1, 1))
                pushBox(this.#boxes, matrix, { x: box.size[0], y: box.size[1], z: box.size[2] }, color)
                this.#labels.place(item.label || "box", new THREE.Vector3(box.center[0], box.center[1], box.center[2] + box.size[2] / 2 + 0.08), `#${color.getHexString()}`)
            }
            for (const item of annotations.points) {
                keepPoints.add(item.id)
                let mesh = this.#pointMeshes.get(item.id)
                if (!mesh) {
                    mesh = new THREE.Mesh(new THREE.SphereGeometry(0.06, 16, 12), new THREE.MeshBasicMaterial({ color: ACCENT, depthTest: false }))
                    mesh.renderOrder = 6
                    mesh.userData = { kind: "point", id: item.id }
                    this.#pointMeshes.set(item.id, mesh)
                    this.#annotationGroup.add(mesh)
                }
                const selected = this.#selected?.kind === "point" && this.#selected.id === item.id
                ;(mesh.material as THREE.MeshBasicMaterial).color.copy(selected ? SELECTED : item.source === "agent" ? AGENT : ACCENT)
                if (!(selected && this.#dragging)) {
                    mesh.position.set(...item.position)
                }
                this.#labels.place(item.label || "point", mesh.position.clone().add(new THREE.Vector3(0, 0, 0.18)))
            }
            for (const item of annotations.planes) {
                keepPlanes.add(item.id)
                let mesh = this.#planeMeshes.get(item.id)
                if (!mesh) {
                    mesh = new THREE.Mesh(new THREE.PlaneGeometry(1, 1), new THREE.MeshBasicMaterial({ color: ACCENT, transparent: true, opacity: 0.28, side: THREE.DoubleSide, depthWrite: false }))
                    mesh.userData = { kind: "plane", id: item.id }
                    this.#planeMeshes.set(item.id, mesh)
                    this.#annotationGroup.add(mesh)
                }
                const selected = this.#selected?.kind === "plane" && this.#selected.id === item.id
                ;(mesh.material as THREE.MeshBasicMaterial).color.copy(selected ? SELECTED : item.source === "agent" ? AGENT : ACCENT)
                if (!(selected && this.#dragging)) {
                    mesh.position.set(...item.center)
                    mesh.quaternion.setFromUnitVectors(new THREE.Vector3(0, 0, 1), new THREE.Vector3(...item.normal).normalize())
                    mesh.scale.set(item.size[0], item.size[1], 1)
                }
                this.#labels.place(item.label || "plane", mesh.position.clone().add(new THREE.Vector3(0, 0, item.size[1] / 2 + 0.1)))
            }
        }
        for (const [id, mesh] of this.#pointMeshes) {
            if (!keepPoints.has(id)) {
                mesh.removeFromParent()
                this.#pointMeshes.delete(id)
            }
        }
        for (const [id, mesh] of this.#planeMeshes) {
            if (!keepPlanes.has(id)) {
                mesh.removeFromParent()
                this.#planeMeshes.delete(id)
            }
        }
        this.#boxes.commit()
        this.#labels.end()
        this.viewer.requestRender()
    }

    // ---- selection + gizmo ----

    get selected() {
        return this.#selected
    }

    select(selection: Selectable | null) {
        this.#selected = selection
        if (selection) {
            this.#attach(selection)
        } else {
            this.#gizmo.detach()
        }
        this.#drawAnnotations()
        this.#drawRegion()
        this.onSelect(selection)
    }

    setGizmoMode(mode: GizmoMode) {
        this.#gizmo.setMode(mode)
        // boxes and the region only turn about z
        const flat = this.#selected?.kind === "box" || this.#selected?.kind === "region"
        this.#gizmo.showX = !(mode === "rotate" && flat)
        this.#gizmo.showY = !(mode === "rotate" && flat)
        this.viewer.requestRender()
    }

    get gizmoMode(): GizmoMode {
        return this.#gizmo.mode as GizmoMode
    }

    #attach(selection: Selectable) {
        const proxy = this.#proxy
        const annotations = this.#annotations
        let box: Box3 | null = null
        if (selection.kind === "region") {
            box = this.#region
        } else if (selection.kind === "box") {
            box = annotations?.boxes.find((b) => b.id === selection.id)?.box ?? null
        }
        if (box) {
            proxy.position.set(...box.center)
            proxy.quaternion.setFromAxisAngle(new THREE.Vector3(0, 0, 1), box.yaw ?? 0)
            proxy.scale.set(...box.size)
        } else if (selection.kind === "point") {
            const point = annotations?.points.find((p) => p.id === selection.id)
            if (!point) {
                return this.#gizmo.detach()
            }
            proxy.position.set(...point.position)
            proxy.quaternion.identity()
            proxy.scale.set(1, 1, 1)
        } else if (selection.kind === "plane") {
            const plane = annotations?.planes.find((p) => p.id === selection.id)
            if (!plane) {
                return this.#gizmo.detach()
            }
            proxy.position.set(...plane.center)
            proxy.quaternion.setFromUnitVectors(new THREE.Vector3(0, 0, 1), new THREE.Vector3(...plane.normal).normalize())
            proxy.scale.set(plane.size[0], plane.size[1], 1)
        } else {
            return this.#gizmo.detach()
        }
        proxy.updateMatrixWorld()
        this.#gizmo.attach(proxy)
        this.setGizmoMode(this.gizmoMode)
    }

    #proxyBox(): Box3 {
        const proxy = this.#proxy
        const euler = new THREE.Euler().setFromQuaternion(proxy.quaternion, "ZYX")
        return {
            center: [proxy.position.x, proxy.position.y, proxy.position.z],
            size: [Math.abs(proxy.scale.x), Math.abs(proxy.scale.y), Math.abs(proxy.scale.z)],
            yaw: euler.z,
        }
    }

    #previewGizmo() {
        const selection = this.#selected
        if (!selection) {
            return
        }
        if (selection.kind === "region") {
            this.#region = this.#proxyBox()
            this.#drawRegion()
        } else if (selection.kind === "point") {
            this.#pointMeshes.get(selection.id)?.position.copy(this.#proxy.position)
        } else if (selection.kind === "plane") {
            const mesh = this.#planeMeshes.get(selection.id)
            mesh?.position.copy(this.#proxy.position)
            mesh?.quaternion.copy(this.#proxy.quaternion)
            mesh?.scale.set(this.#proxy.scale.x, this.#proxy.scale.y, 1)
        } else {
            this.#drawAnnotations()
        }
    }

    #commitGizmo() {
        const selection = this.#selected
        if (!selection) {
            return
        }
        const box = this.#proxyBox()
        const normal = new THREE.Vector3(0, 0, 1).applyQuaternion(this.#proxy.quaternion)
        if (selection.kind === "region") {
            this.#region = box
            this.#drawRegion()
        }
        this.onEdit(selection, { ...box, normal: [normal.x, normal.y, normal.z] })
    }

    // ---- the selection region (a box the cleanup tools act in) ----

    get region(): Box3 | null {
        return this.#region
    }

    setRegion(region: Box3 | null) {
        this.#region = region
        this.#drawRegion()
        if (region && this.#selected?.kind === "region") {
            this.#attach(this.#selected)
        }
        if (!region && this.#selected?.kind === "region") {
            this.select(null)
        }
    }

    #drawRegion() {
        this.#regionLines.clear()
        const region = this.#region
        if (region) {
            const matrix = new THREE.Matrix4().compose(new THREE.Vector3(...region.center), new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(0, 0, 1), region.yaw), new THREE.Vector3(1, 1, 1))
            pushBox(this.#regionLines, matrix, { x: region.size[0], y: region.size[1], z: region.size[2] })
        }
        this.#regionLines.commit()
        this.viewer.requestRender()
    }

    /** A box around what's in view now, a starting point for the region. */
    regionFromView(): Box3 {
        const target = this.viewer.controls.target
        const distance = this.viewer.camera.position.distanceTo(target)
        const half = Math.max(0.5, distance * 0.35)
        const z = this.map.range
        return { center: [target.x, target.y, (z[0] + z[1]) / 2], size: [half * 2, half * 2, Math.max(1, z[1] - z[0] + 0.6)], yaw: 0 }
    }

    // ---- picking ----

    #click(event: PointerEvent) {
        const rect = this.viewer.renderer.domElement.getBoundingClientRect()
        const ndc = new THREE.Vector2(((event.clientX - rect.left) / rect.width) * 2 - 1, -((event.clientY - rect.top) / rect.height) * 2 + 1)
        const ray = new THREE.Raycaster()
        ray.setFromCamera(ndc, this.viewer.camera)
        // annotations first
        const hits = ray.intersectObjects([...this.#pointMeshes.values(), ...this.#planeMeshes.values()], false)
        if (hits.length && !this.onPick) {
            const data = hits[0].object.userData as { kind: "point" | "plane"; id: string }
            this.select({ kind: data.kind, id: data.id })
            return
        }
        if (!this.onPick && this.#annotations) {
            const box = this.#annotations.boxes.find((item) => {
                const b = item.box
                const inverse = new THREE.Matrix4().compose(new THREE.Vector3(...b.center), new THREE.Quaternion().setFromAxisAngle(new THREE.Vector3(0, 0, 1), b.yaw ?? 0), new THREE.Vector3(...b.size)).invert()
                const local = ray.ray.clone().applyMatrix4(inverse)
                return local.intersectsBox(new THREE.Box3(new THREE.Vector3(-0.5, -0.5, -0.5), new THREE.Vector3(0.5, 0.5, 0.5)))
            })
            if (box) {
                this.select({ kind: "box", id: box.id })
                return
            }
        }
        const point = this.pickMap(ray.ray)
        if (this.onPick) {
            if (point) {
                this.onPick(point, event)
            }
            return
        }
        this.select(null)
    }

    /** The map voxel nearest the camera along `ray` (within a few voxels of it). */
    pickMap(ray: THREE.Ray): THREE.Vector3 | null {
        const positions = this.map.points.visible ? this.map.positions : this.raw.positions
        const tolerance = Math.max(this.voxelSize * 1.5, 0.05)
        let best: THREE.Vector3 | null = null
        let bestDistance = Infinity
        const p = new THREE.Vector3()
        const origin = ray.origin
        const direction = ray.direction
        for (let index = 0; index < positions.length; index += 3) {
            p.set(positions[index], positions[index + 1], positions[index + 2])
            const along = (p.x - origin.x) * direction.x + (p.y - origin.y) * direction.y + (p.z - origin.z) * direction.z
            if (along <= 0 || along >= bestDistance) {
                continue
            }
            const dx = origin.x + direction.x * along - p.x
            const dy = origin.y + direction.y * along - p.y
            const dz = origin.z + direction.z * along - p.z
            const off = Math.sqrt(dx * dx + dy * dy + dz * dz)
            if (off < tolerance + along * 0.004) {
                bestDistance = along
                best = p.clone()
            }
        }
        return best
    }

    // ---- camera ----

    frameMap() {
        const positions = this.map.points.visible ? this.map.positions : this.raw.positions
        if (!positions.length) {
            return
        }
        const box = new THREE.Box3().setFromArray(positions)
        const center = box.getCenter(new THREE.Vector3())
        const size = box.getSize(new THREE.Vector3()).length()
        this.viewer.frame(center, Math.max(3, size * 0.8))
    }

    topDown() {
        const target = this.viewer.controls.target.clone()
        this.viewer.topDown(target, Math.max(4, this.viewer.camera.position.distanceTo(target)))
    }

    lookAt(target: [number, number, number], distance?: number, topDown = false) {
        const point = new THREE.Vector3(...target)
        const far = distance ?? Math.max(3, this.viewer.camera.position.distanceTo(this.viewer.controls.target))
        if (topDown) {
            this.viewer.topDown(point, far)
        } else {
            this.viewer.frame(point, far)
        }
    }

    /** What's reported to the server (and the agent): the camera, its view-projection matrix, the visible bounds. */
    viewState(): Record<string, unknown> {
        const camera = this.viewer.camera
        camera.updateMatrixWorld()
        const viewProjection = new THREE.Matrix4().multiplyMatrices(camera.projectionMatrix, camera.matrixWorldInverse)
        const frustum = new THREE.Frustum().setFromProjectionMatrix(viewProjection)
        const positions = this.map.points.visible ? this.map.positions : this.raw.positions
        const step = Math.max(1, Math.floor(positions.length / 3 / 60000)) * 3
        const min = [Infinity, Infinity, Infinity]
        const max = [-Infinity, -Infinity, -Infinity]
        const p = new THREE.Vector3()
        let seen = 0
        for (let index = 0; index < positions.length; index += step) {
            p.set(positions[index], positions[index + 1], positions[index + 2])
            if (frustum.containsPoint(p)) {
                seen++
                for (let axis = 0; axis < 3; axis++) {
                    min[axis] = Math.min(min[axis], positions[index + axis])
                    max[axis] = Math.max(max[axis], positions[index + axis])
                }
            }
        }
        const round = (v: number) => Math.round(v * 100) / 100
        const target = this.viewer.controls.target
        return {
            camera: {
                position: camera.position.toArray().map(round),
                target: target.toArray().map(round),
                fov: camera.fov,
                aspect: round(camera.aspect),
            },
            viewProjection: viewProjection.toArray(),
            visibleBounds: seen ? { min: min.map(round), max: max.map(round) } : null,
        }
    }

    // ---- screenshots for the agent ----

    /** The 3D view as a PNG data URL; `overlay` draws a labelled 1 m grid, axes and annotation labels on it. */
    capture(options: { overlay?: boolean; topDown?: boolean } = {}): string {
        const viewer = this.viewer
        const camera = viewer.camera
        const saved = { position: camera.position.clone(), target: viewer.controls.target.clone() }
        if (options.topDown) {
            const target = viewer.controls.target.clone()
            camera.position.set(target.x, target.y - 0.001, target.z + Math.max(6, saved.position.distanceTo(saved.target)))
            camera.lookAt(target)
        }
        camera.updateMatrixWorld()
        viewer.renderer.render(viewer.scene, camera)
        const source = viewer.renderer.domElement
        const canvas = document.createElement("canvas")
        canvas.width = source.width
        canvas.height = source.height
        const context = canvas.getContext("2d")!
        context.fillStyle = "#06090f"
        context.fillRect(0, 0, canvas.width, canvas.height)
        context.drawImage(source, 0, 0)
        if (options.overlay) {
            this.#drawOverlay(context, canvas.width, canvas.height)
        }
        if (options.topDown) {
            camera.position.copy(saved.position)
            camera.lookAt(saved.target)
            viewer.controls.target.copy(saved.target)
            viewer.controls.update()
            viewer.requestRender()
        }
        return canvas.toDataURL("image/png")
    }

    #drawOverlay(context: CanvasRenderingContext2D, width: number, height: number) {
        const camera = this.viewer.camera
        const project = (x: number, y: number, z: number) => {
            const v = new THREE.Vector3(x, y, z).project(camera)
            return { x: (v.x + 1) / 2 * width, y: (1 - v.y) / 2 * height, visible: v.z > -1 && v.z < 1 }
        }
        const target = this.viewer.controls.target
        const span = Math.min(30, Math.ceil(camera.position.distanceTo(target) * 1.2))
        const floorZ = this.map.range[0]
        const scale = width / 1200
        context.lineWidth = Math.max(1, scale)
        context.font = `${Math.round(12 * Math.max(1, scale))}px ui-monospace, monospace`
        context.textBaseline = "middle"
        const cx = Math.round(target.x)
        const cy = Math.round(target.y)
        // grid lines every meter on the floor plane, labelled with their coordinate
        for (let offset = -span; offset <= span; offset++) {
            for (const axis of ["x", "y"] as const) {
                const fixed = axis === "x" ? cx + offset : cy + offset
                const a = axis === "x" ? project(fixed, cy - span, floorZ) : project(cx - span, fixed, floorZ)
                const b = axis === "x" ? project(fixed, cy + span, floorZ) : project(cx + span, fixed, floorZ)
                if (!a.visible || !b.visible) {
                    continue
                }
                context.strokeStyle = fixed === 0 ? "rgba(255, 95, 109, 0.85)" : offset % 5 === 0 ? "rgba(122, 240, 168, 0.45)" : "rgba(122, 240, 168, 0.18)"
                context.beginPath()
                context.moveTo(a.x, a.y)
                context.lineTo(b.x, b.y)
                context.stroke()
                if (Math.abs(offset) % 2 === 0) {
                    const label = axis === "x" ? project(fixed, cy, floorZ) : project(cx, fixed, floorZ)
                    if (label.visible && label.x > 0 && label.x < width && label.y > 0 && label.y < height) {
                        context.fillStyle = "rgba(216, 230, 244, 0.85)"
                        context.fillText(`${axis}=${fixed}`, label.x + 3, label.y - 7)
                    }
                }
            }
        }
        // axes at the origin
        const origin = project(0, 0, floorZ)
        for (const [dx, dy, dz, color, name] of [[1, 0, 0, "#ff5f6d", "+x"], [0, 1, 0, "#7af0a8", "+y"], [0, 0, 1, "#7fc8f8", "+z"]] as const) {
            const tip = project(dx, dy, floorZ + dz)
            if (origin.visible && tip.visible) {
                context.strokeStyle = color
                context.lineWidth = 3 * Math.max(1, scale)
                context.beginPath()
                context.moveTo(origin.x, origin.y)
                context.lineTo(tip.x, tip.y)
                context.stroke()
                context.fillStyle = color
                context.fillText(name, tip.x + 4, tip.y)
            }
        }
        // annotation labels with their ids
        const annotations = this.#annotations
        context.fillStyle = "#ffd166"
        for (const item of annotations?.boxes ?? []) {
            const at = project(item.box.center[0], item.box.center[1], item.box.center[2] + item.box.size[2] / 2)
            if (at.visible) {
                context.fillText(`[${item.label}]`, at.x, at.y - 10)
            }
        }
        for (const item of annotations?.points ?? []) {
            const at = project(...item.position)
            if (at.visible) {
                context.fillText(`• ${item.label}`, at.x, at.y)
            }
        }
        context.fillStyle = "rgba(216, 230, 244, 0.9)"
        context.fillText(`camera (${camera.position.toArray().map((v) => v.toFixed(1)).join(", ")}) → target (${target.toArray().map((v) => v.toFixed(1)).join(", ")}); grid 1 m on z=${floorZ.toFixed(2)}`, 10, height - 14)
    }
}
