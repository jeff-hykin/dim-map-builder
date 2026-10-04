// Copied from dim-live-viewer frontend/src/core/render (4a0c5aa) — the Map Editor draws with the same point styles.
// Text in the 3D scene as HTML labels (crisp at any zoom, no font atlas), pooled so a message that relabels
// everything every frame reuses the same elements.
import * as THREE from "three"
import { CSS2DObject } from "three/examples/jsm/renderers/CSS2DRenderer.js"

export class LabelPool {
    readonly group = new THREE.Group()
    #labels: CSS2DObject[] = []
    #used = 0

    constructor(readonly className = "scene-label") {}

    /** Call before placing a message's labels; `end()` hides the ones not reused. */
    begin() {
        this.#used = 0
    }

    place(text: string, position: THREE.Vector3, color?: string): CSS2DObject {
        let label = this.#labels[this.#used]
        if (!label) {
            const element = document.createElement("div")
            element.className = this.className
            label = new CSS2DObject(element)
            this.#labels.push(label)
            this.group.add(label)
        }
        this.#used++
        if (label.element.textContent !== text) {
            label.element.textContent = text
        }
        label.element.style.setProperty("--label-color", color ?? "")
        label.position.copy(position)
        label.visible = true
        return label
    }

    end() {
        for (let index = this.#used; index < this.#labels.length; index++) {
            this.#labels[index].visible = false
        }
    }

    dispose() {
        for (const label of this.#labels) {
            label.element.remove()
            label.removeFromParent()
        }
        this.#labels = []
    }
}
