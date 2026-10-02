// Copied from dim-live-viewer frontend/src/core/render (1ba5a3c) — the Map Builder draws with the same point styles.
// One shader for every point-like thing, a port of MemWorld's sprite shader
// (memory_world/web/static/voxel_sprites.js): same view-space light normalize(2, 4, 3), same sphere (0.45 + 0.75·n·L)
// and cube-face (0.42 + 0.72·n·L) lighting, sprites sized to the projected diameter. Points are GL point sprites,
// never meshes: "disc" shades each sprite as a small sphere, "square" is flat, "voxel" snaps the point to a world grid
// and ray-casts an axis-aligned cube inside the sprite, "splat" is a soft blended gaussian with distance fog. No style
// writes gl_FragDepth (as MemWorld): that would turn off early depth rejection and cost ~4x at 10M cubes.
// Coloring (gradient lookup by height / intensity / range) happens on the GPU too, so restyling costs nothing.
import * as THREE from "three"
import { gradientTexture } from "./gradients.ts"

/**
 * The point styles. Each is a shader path selected by `uStyle` (and a `#define` when its shading differs). Adding a style (EDL, splats, AO, ...) = one entry here + its branch in the shaders below; the settings
 * editor lists whatever is here.
 */
export const POINT_STYLES = {
    splat: { id: 3, label: "Glow", define: "SPLAT", about: "soft gaussian splats fading into the background with distance" },
    voxel: { id: 2, label: "Cubes", define: "VOXEL", about: "MemWorld's lit cubes on the voxel grid" },
    disc: { id: 0, label: "Spheres", define: null, about: "MemWorld's lit spheres" },
    square: { id: 1, label: "Squares", define: null, about: "flat squares, the cheapest" },
} as const
export type PointStyle = keyof typeof POINT_STYLES
export type ColorMode = "height" | "intensity" | "range" | "solid"

export interface PointLook {
    /** "default" follows Settings → Rendering */
    style: PointStyle | "default"
    /** meters: the point's diameter, or the voxel's edge */
    size: number
    colorMode: ColorMode
    gradient: string
    /** for "height": which axis of the fixed frame */
    axis: 0 | 1 | 2
    /** null = auto from the data */
    rangeMin: number | null
    rangeMax: number | null
    solid: string
    opacity: number
}

const COLOR = { height: 0, intensity: 1, range: 2, solid: 3 }

const VIEW_LIGHT = new THREE.Vector3(2, 4, 3).normalize()

const vertexShader = /* glsl */ `
uniform float uSize;
uniform float uPxPerMeter;
uniform float uMinPx;
uniform int uStyle;
uniform int uColorMode;
uniform int uAxis;
uniform vec2 uRange;
uniform vec3 uSolid;
uniform vec3 uSensor;
uniform float uNow;
uniform float uWindow;
uniform sampler2D uGradient;
uniform float uKeep;
attribute float aTime;
attribute float aIntensity;
varying vec3 vColor;
varying vec3 vCenter;
varying float vHalf;
varying float vViewDepth;

void main() {
    // attributes are only read when used, so an idle one isn't fetched for every point (10M points are vertex-bound)
#ifdef USE_WINDOW
    if (aTime < uNow - uWindow) {
        gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
        gl_PointSize = 0.0;
        return;
    }
#endif
    vec3 world = (modelMatrix * vec4(position, 1.0)).xyz;
    vec3 center = uStyle == 2 ? (floor(world / uSize) + 0.5) * uSize : world;
#ifdef USE_INTENSITY
    float value = aIntensity;
#else
    float value = uColorMode == 2 ? distance(world, uSensor) : center[uAxis];
#endif
    float t = clamp((value - uRange.x) / max(1e-6, uRange.y - uRange.x), 0.0, 1.0);
    vColor = uColorMode == 3 ? uSolid : texture2D(uGradient, vec2(t, 0.5)).rgb;
    vec4 mv = viewMatrix * vec4(center, 1.0);
    float depth = max(1e-3, -mv.z);
    // a cube's silhouette can reach sqrt(3)/2 of its edge from the center; a splat's soft edge needs room too
    float px = uSize * uPxPerMeter / depth * (uStyle == 2 ? 1.8 : uStyle == 3 ? 1.8 : 1.0);
#ifdef SPLAT
    // over the splat budget, a stable random subset is drawn: blended splats are fill-bound, and where a map is dense
    // enough to exceed the budget its splats already overlap so much that the dropped ones don't change the picture
    if (fract(sin(float(gl_VertexID) * 12.9898) * 43758.5453) > uKeep) {
        gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
        gl_PointSize = 0.0;
        return;
    }
    vViewDepth = depth;
#endif
    gl_Position = projectionMatrix * mv;
    gl_PointSize = clamp(px, uMinPx, 512.0);
    vCenter = center;
    vHalf = 0.5 * gl_PointSize / uPxPerMeter * depth;
}
`

const fragmentShader = /* glsl */ `
uniform float uSize;
uniform int uStyle;
uniform float uOpacity;
uniform vec3 uLightWorld;
uniform vec2 uFog;
uniform vec3 uBackground;
// view-space light direction, MemWorld's normalize(2, 4, 3)
const vec3 LIGHT = vec3(0.3713907, 0.7427814, 0.5570860);
varying vec3 vColor;
varying vec3 vCenter;
varying float vHalf;
varying float vViewDepth;

void main() {
    vec2 p = gl_PointCoord * 2.0 - 1.0;
    p.y = -p.y;
#if defined(SPLAT)
    float r2 = dot(p, p);
    if (r2 > 1.0) discard;
    float alpha = exp(-r2 * 3.2) * 0.85 * uOpacity;
    float fog = smoothstep(uFog.x, uFog.y, vViewDepth);
    vec3 color = mix(vColor * 1.15, uBackground, fog * 0.85);
    gl_FragColor = vec4(color * alpha, alpha);
#elif !defined(VOXEL)
    // disc / square never write depth themselves, so the GPU's early depth test keeps working for them
    if (uStyle == 1) {
        gl_FragColor = vec4(vColor, uOpacity);
        return;
    }
    // a ball: the sphere normal the disc implies, lit like MemWorld's voxel_sprites.js
    float r2 = dot(p, p);
    if (r2 > 1.0) discard;
    vec3 n = vec3(p, sqrt(1.0 - r2));
    gl_FragColor = vec4(vColor * (0.45 + 0.75 * max(dot(n, LIGHT), 0.0)), uOpacity);
#else
    // voxel: cast this fragment's camera ray at the cube around vCenter
    vec3 right = vec3(viewMatrix[0][0], viewMatrix[1][0], viewMatrix[2][0]);
    vec3 up = vec3(viewMatrix[0][1], viewMatrix[1][1], viewMatrix[2][1]);
    vec3 onSprite = vCenter + right * p.x * vHalf + up * p.y * vHalf;
    vec3 dir = normalize(onSprite - cameraPosition);
    vec3 h = vec3(0.5 * uSize);
    vec3 inv = 1.0 / dir;
    vec3 t0 = (vCenter - h - cameraPosition) * inv;
    vec3 t1 = (vCenter + h - cameraPosition) * inv;
    vec3 tMin = min(t0, t1);
    vec3 tMax = max(t0, t1);
    float tNear = max(max(tMin.x, tMin.y), tMin.z);
    float tFar = min(min(tMax.x, tMax.y), tMax.z);
    if (tNear > tFar || tFar < 0.0) discard;
    vec3 hit = cameraPosition + dir * tNear;
    vec3 local = (hit - vCenter) / h;
    vec3 a = abs(local);
    vec3 normal;
    vec2 face;
    if (a.x >= a.y && a.x >= a.z) { normal = vec3(sign(local.x), 0.0, 0.0); face = local.yz; }
    else if (a.y >= a.z) { normal = vec3(0.0, sign(local.y), 0.0); face = local.xz; }
    else { normal = vec3(0.0, 0.0, sign(local.z)); face = local.xy; }
    // per-face light (MemWorld's cube faces): the view-space light turned into world space once per draw, on the CPU
    float light = 0.42 + 0.72 * max(dot(normal, uLightWorld), 0.0);
    float edge = smoothstep(0.86, 0.99, max(abs(face.x), abs(face.y)));
    gl_FragColor = vec4(vColor * light * (1.0 - 0.18 * edge), uOpacity);
#endif
}
`

/** The camera's distance to what it orbits; the viewer keeps it current (splat fog is scaled by it). */
export const focusDistance = { value: 10 }

export function makePointMaterial(pixelsPerMeter: { value: number }): THREE.ShaderMaterial {
    const material = new THREE.ShaderMaterial({
        vertexShader,
        fragmentShader,
        uniforms: {
            uSize: { value: 0.05 },
            uPxPerMeter: pixelsPerMeter,
            uMinPx: { value: 1.5 },
            uStyle: { value: 0 },
            uColorMode: { value: 0 },
            uAxis: { value: 2 },
            uRange: { value: new THREE.Vector2(0, 2) },
            uSolid: { value: new THREE.Color(0xffffff) },
            uSensor: { value: new THREE.Vector3() },
            uNow: { value: 0 },
            uWindow: { value: -1 },
            uOpacity: { value: 1 },
            uGradient: { value: gradientTexture("memworld") },
            uLightWorld: { value: new THREE.Vector3() },
            uKeep: { value: 1 },
            uFog: { value: new THREE.Vector2(10, 40) },
            uBackground: { value: new THREE.Color(0x06090f) },
        },
        defines: {},
    })
    const light = material.uniforms.uLightWorld.value as THREE.Vector3
    material.onBeforeRender = (_renderer, _scene, camera) => {
        light.copy(VIEW_LIGHT).transformDirection(camera.matrixWorld)
        // splats fade into the background with distance, relative to how far the view is from what it orbits
        const focus = focusDistance.value
        material.uniforms.uFog.value.set(focus * 0.6, focus * 2.2)
    }
    // three compiles this as GLSL 3 (WebGL2) and maps gl_FragColor / gl_FragDepthEXT / texture2D onto it
    return material
}

/** Pushes a look into the material's uniforms; `range` is the resolved [min, max] when the look's is auto. */
export function applyLook(material: THREE.ShaderMaterial, look: PointLook, range: [number, number]) {
    const uniforms = material.uniforms
    uniforms.uSize.value = Math.max(0.001, look.size)
    const style = POINT_STYLES[look.style as PointStyle] ?? POINT_STYLES.disc
    uniforms.uStyle.value = style.id
    // only the defines this look needs; a change recompiles once
    const wanted = new Set<string>()
    if (style.define) {
        wanted.add(style.define)
    }
    if (look.colorMode === "intensity") {
        wanted.add("USE_INTENSITY")
    }
    if (uniforms.uWindow.value >= 0) {
        wanted.add("USE_WINDOW")
    }
    const defines = material.defines as Record<string, number>
    const current = Object.keys(defines)
    if (current.length !== wanted.size || current.some((name) => !wanted.has(name))) {
        for (const name of current) {
            delete defines[name]
        }
        for (const name of wanted) {
            defines[name] = 1
        }
        material.needsUpdate = true
    }
    uniforms.uColorMode.value = COLOR[look.colorMode]
    uniforms.uAxis.value = look.axis
    uniforms.uRange.value.set(look.rangeMin ?? range[0], look.rangeMax ?? range[1])
    uniforms.uSolid.value.set(look.solid)
    uniforms.uOpacity.value = look.opacity
    uniforms.uGradient.value = gradientTexture(look.gradient)
    const splat = look.style === "splat"
    material.transparent = splat || look.opacity < 1
    material.depthWrite = !splat && look.opacity >= 1
    // splats: premultiplied alpha over what's behind
    material.blending = splat ? THREE.CustomBlending : THREE.NormalBlending
    material.blendSrc = THREE.OneFactor
    material.blendDst = THREE.OneMinusSrcAlphaFactor
}

