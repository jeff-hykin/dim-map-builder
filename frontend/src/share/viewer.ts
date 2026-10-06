// The shared map's page (Share → as HTML): built on its own (vite.share.config.ts → dist/share-viewer.js) and gzipped
// into the exported file, it draws the packed voxels with the editor's renderer (render/viewer.ts and its point
// shader, the same look, slice turn and clip), the floor selector bottom right for a multi-floor map, Frame / Top.
import * as THREE from "three"
import themeCss from "../dim-app/theme.css?raw"
import { initTheme } from "../dim-app/theme.js"
import { dimIcon } from "../dim-icons.js"
import { applyLook, makePointMaterial, type CubeShade } from "../render/pointMaterial.ts"
import { Viewer } from "../render/viewer.ts"
import { fromBase64, SHARE_IDS, type SharedMap, unpackVoxels } from "./pack.ts"

const PAGE_CSS = `
#dim-share { position: fixed; inset: 0; background: radial-gradient(120% 90% at 50% 0%, var(--card), var(--bg) 60%); color: var(--fg); font-family: var(--sans, system-ui, sans-serif); }
.scene { position: absolute; inset: 0; touch-action: none; }
.scene canvas { display: block; }
.label-layer { position: absolute; inset: 0; pointer-events: none; }
.share-title { position: absolute; left: 12px; top: 12px; z-index: 2; padding: 6px 10px; display: flex; flex-direction: column; gap: 2px; max-width: calc(100vw - 140px); }
.share-title .dim-title { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.share-title .dim-mono { font-size: 11px; opacity: 0.75; }
.view-tools { position: absolute; right: 12px; top: 12px; display: flex; flex-direction: column; gap: 6px; z-index: 2; }
.floor-picker { position: absolute; right: 12px; bottom: 12px; z-index: 3; display: flex; flex-direction: column; align-items: stretch; gap: 4px; padding: 6px; }
.floor-picker .dim-label { text-align: center; margin: 0; }
.floor-picker .dim-tab { justify-content: space-between; gap: 8px; }
.share-error { position: absolute; inset: 0; display: grid; place-items: center; padding: 16px; text-align: center; }
`

function element<K extends keyof HTMLElementTagNameMap>(tag: K, className: string, html = ""): HTMLElementTagNameMap[K] {
    const made = document.createElement(tag)
    made.className = className
    made.innerHTML = html
    return made
}

function percentile(values: Float32Array, axis: number, low: number, high: number): [number, number] {
    const count = values.length / 3
    const step = Math.max(1, Math.floor(count / 20000))
    const sample: number[] = []
    for (let index = 0; index < count; index += step) {
        sample.push(values[index * 3 + axis])
    }
    sample.sort((a, b) => a - b)
    return [sample[Math.floor(low * (sample.length - 1))] ?? 0, sample[Math.floor(high * (sample.length - 1))] ?? 1]
}

async function main() {
    // the Portal look, baked in (theme.css's bundled tokens; its web fonts stay out, the system ones stand in)
    const style = document.createElement("style")
    style.textContent = themeCss.replace(/@font-face\s*{[^}]*}/g, "") + PAGE_CSS
    document.head.append(style)
    initTheme()

    const meta: SharedMap = JSON.parse(document.getElementById(SHARE_IDS.meta)!.textContent!)
    const { positions } = await unpackVoxels(fromBase64(document.getElementById(SHARE_IDS.voxels)!.textContent!))

    const page = document.getElementById("dim-share")!
    const host = element("div", "scene")
    page.append(host)
    const viewer = new Viewer(host, () => Date.now())
    viewer.setTheme(document.body.classList.contains("dark"))

    // the map frame hangs under a root the slice's yaw turns, as in the editor (core/scene.ts)
    const root = new THREE.Group()
    root.rotation.z = meta.slice?.yaw ?? 0
    viewer.scene.add(root)
    const geometry = new THREE.BufferGeometry()
    const count = positions.length / 3
    geometry.setAttribute("position", new THREE.BufferAttribute(positions, 3))
    geometry.setAttribute("aTime", new THREE.BufferAttribute(new Float32Array(count), 1))
    geometry.setAttribute("aIntensity", new THREE.BufferAttribute(new Uint8Array(count), 1, true))
    const material = makePointMaterial(viewer.pixelsPerMeter)
    const points = new THREE.Points(geometry, material)
    points.frustumCulled = false
    points.renderOrder = 1
    root.add(points)
    applyLook(material, {
        style: meta.look.style,
        size: meta.voxelSize * meta.look.scale,
        colorMode: "height",
        gradient: meta.look.gradient,
        axis: 2,
        rangeMin: null,
        rangeMax: null,
        solid: "#ffffff",
        opacity: 1,
        shade: (meta.look.shade ?? "soft") as CubeShade,
    }, meta.range)
    root.updateMatrixWorld()

    // the clip: the slice's box (x, y in the turned frame) and the shown floor's band, intersected (scene.ts #applyClip)
    const big = 1e9
    const showFloor = (floor: number | null) => {
        const slice = meta.slice
        const band = floor === null ? null : meta.floors[floor]?.band ?? null
        material.uniforms.uClipMin.value.set(slice ? slice.xMin : -big, slice ? slice.yMin : -big, Math.max(slice ? slice.zMin : -big, band ? band[0] : -big))
        material.uniforms.uClipMax.value.set(slice ? slice.xMax : big, slice ? slice.yMax : big, Math.min(slice ? slice.zMax : big, band ? band[1] : big))
        viewer.requestRender()
    }

    const frame = () => {
        const ranges = [0, 1, 2].map((axis) => percentile(positions, axis, 0.02, 0.98))
        const center = new THREE.Vector3(...ranges.map(([lo, hi]) => (lo + hi) / 2)).applyMatrix4(root.matrixWorld)
        viewer.frame(center, Math.max(3, Math.hypot(...ranges.map(([lo, hi]) => hi - lo)) * 0.75))
    }
    if (meta.camera) {
        viewer.controls.target.set(...meta.camera.target)
        viewer.camera.position.set(...meta.camera.position)
        viewer.controls.update()
        viewer.requestRender()
    } else {
        frame()
    }

    const title = element("div", "dim-panel glass share-title")
    const titleName = element("span", "dim-title")
    titleName.textContent = meta.name
    const titleInfo = element("span", "dim-mono")
    titleInfo.textContent = `${count.toLocaleString()} voxels · ${(meta.voxelSize * 100).toFixed(0)} cm · ${meta.exported.slice(0, 10)}`
    title.append(titleName, titleInfo)
    page.append(title)

    const tools = element("div", "view-tools")
    const frameButton = element("button", "dim-btn sm icon", `${dimIcon("fullscreen")} Frame`)
    frameButton.title = "Frame the map (F)"
    frameButton.onclick = frame
    const topButton = element("button", "dim-btn sm icon", `${dimIcon("top")} Top`)
    topButton.title = "Top-down view (T)"
    const top = () => {
        const target = viewer.controls.target.clone()
        viewer.topDown(target, Math.max(4, viewer.camera.position.distanceTo(target)))
    }
    topButton.onclick = top
    tools.append(frameButton, topButton)
    page.append(tools)
    addEventListener("keydown", (event) => {
        if (event.metaKey || event.ctrlKey || event.altKey) {
            return
        }
        if (event.key === "f" || event.key === "F") {
            frame()
        } else if (event.key === "t" || event.key === "T") {
            top()
        }
    })

    // the floor selector (ui/FloorPicker.tsx's markup): a lift's buttons, the top floor on top, then All
    let shown = meta.floors.length > 1 ? meta.floor : null
    showFloor(shown)
    if (meta.floors.length > 1) {
        const picker = element("div", "dim-panel glass floor-picker", `<span class="dim-label">floor</span>`)
        picker.setAttribute("role", "radiogroup")
        picker.setAttribute("aria-label", "floor")
        picker.dataset.floorPicker = ""
        const tabs = element("div", "dim-tabs seg vertical")
        picker.append(tabs)
        const buttons: [number | null, HTMLButtonElement][] = []
        const choices: (number | null)[] = [...meta.floors.keys()].reverse()
        choices.push(null)
        for (const choice of choices) {
            const storey = choice === null ? null : meta.floors[choice]
            const button = element("button", "dim-tab", storey ? `F${choice! + 1} <span class="dim">${storey.level.toFixed(1)}</span>` : "All")
            button.type = "button"
            button.setAttribute("role", "radio")
            button.title = storey ? `floor ${choice! + 1}: ${storey.band[0].toFixed(2)} to ${storey.band[1].toFixed(2)} m (floor at ${storey.level.toFixed(2)} m)` : "every floor"
            button.dataset[choice === null ? "floor" : "storey"] = choice === null ? "all" : String(choice)
            button.onclick = () => {
                shown = choice
                showFloor(shown)
                update()
            }
            buttons.push([choice, button])
            tabs.append(button)
        }
        const update = () => {
            for (const [choice, button] of buttons) {
                button.classList.toggle("on", choice === shown)
                button.setAttribute("aria-checked", String(choice === shown))
            }
        }
        update()
        page.append(picker)
    }
    document.documentElement.dataset.ready = "1"
}

main().catch((error) => {
    console.error(error)
    const page = document.getElementById("dim-share") ?? document.body
    const message = element("div", "share-error")
    message.textContent = `This map couldn't be shown: ${error?.message ?? error}`
    page.append(message)
})
