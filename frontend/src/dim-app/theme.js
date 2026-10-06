// dim-app theme: the app follows dimOS Desktop's theme (Settings → Appearance), and keeps following it.
//
//     import "./theme.css"   // (or <link rel="stylesheet" href=".../theme.css">)
//     import { initTheme, onThemeChange, themeColors } from "https://esm.sh/gh/jeff-hykin/dim-app@v0.14.1/theme.js"
//     initTheme()                                   // body.science [+ .dark], html[data-dim-theme], html[data-corners]
//     onThemeChange(({ dark }) => renderer.setClearColor(themeColors().sceneBg))
//     .drive-bar { bottom: calc(12px + var(--dim-inset-bottom)) }   // initTheme() also keeps --dim-inset-* current
//
// Two palettes (theme.css): Portal (dark) and Research (light). Desktop's theme (its skin) picks one: a light skin is
// Research, every other one Portal. Apps are served from Desktop's origin (/apps/<name>/), so Desktop's saved skin and
// corners are in localStorage "portal.theme" / "portal.corners", and a change there reaches every open app at once (the
// storage event); GET /api/ui-settings/themes gives Desktop's current one (and which skins are light) when the browser
// has no saved copy. Off Desktop's origin, with neither: Portal.

const SKIN_KEY = "portal.theme"
const CORNERS_KEY = "portal.corners"
// Desktop's light skins (its ui/src/skin.ts), until /api/ui-settings/themes says which ones are
const lightSkins = new Set(["research", "vibeslop-light", "solarpunk"])
const listeners = new Set()
let installed = false
let serverSkin = null
let serverCorners = null

function stored(key) {
    try {
        return localStorage.getItem(key)
    } catch {
        return null
    }
}

/** Desktop's current skin id ("portal", "research", "vibeslop", …); "portal" when Desktop can't be asked. */
export function desktopSkin() {
    return stored(SKIN_KEY) || serverSkin || "portal"
}

/** True when the Portal (dark) palette is showing: Desktop's skin is a dark one. */
export function isDark() {
    return !lightSkins.has(desktopSkin())
}

/** "portal" or "research" */
export function themeName() {
    return isDark() ? "portal" : "research"
}

/** Desktop's corners setting: "sharp", "rounded", or "theme" (each palette's own). */
export function corners() {
    const value = stored(CORNERS_KEY) || serverCorners
    return value === "sharp" || value === "rounded" ? value : "theme"
}

function apply() {
    const dark = isDark()
    const root = document.documentElement
    root.style.colorScheme = dark ? "dark" : "light"
    root.dataset.dimTheme = dark ? "portal" : "research"
    const corner = corners()
    if (corner === "theme") {
        delete root.dataset.corners
        root.style.removeProperty("--dim-corner-radius")
    } else {
        root.dataset.corners = corner
        root.style.setProperty("--dim-corner-radius", corner === "rounded" ? "10px" : "0px")
    }
    if (document.body) {
        document.body.classList.add("science")
        document.body.classList.toggle("dark", dark)
    }
    const detail = {
        dark,
        theme: dark ? "portal" : "research",
        skin: desktopSkin(),
        corners: corner,
    }
    for (const listener of listeners) {
        try {
            listener(detail)
        } catch (error) {
            console.error(error)
        }
    }
    dispatchEvent(new CustomEvent("dim-theme", { detail }))
}

/** Asks Desktop (same origin) for its current theme and which skins are light; nothing to ask off Desktop's origin. */
async function askDesktop() {
    if (!location.pathname.startsWith("/apps/")) {
        return
    }
    try {
        const response = await fetch(new URL("/api/ui-settings/themes", location.origin))
        if (!response.ok) {
            return
        }
        const { themes, current, corners } = await response.json()
        for (const theme of themes ?? []) {
            if (theme.light) {
                lightSkins.add(theme.id)
            } else {
                lightSkins.delete(theme.id)
            }
        }
        serverSkin = typeof current === "string" ? current : null
        serverCorners = typeof corners === "string" ? corners : null
        apply()
    } catch {
        // Desktop not reachable: the saved copy, or Portal
    }
}

/** The theme's faces (theme.css @font-face); loading starts in initTheme, so no view shows a fallback first. */
export const THEME_FONTS = [
    '400 14px "Inter"',
    '500 14px "Inter"',
    '600 14px "Inter"',
    '400 14px "IBM Plex Mono"',
    '500 14px "IBM Plex Mono"',
    '400 14px "Instrument Serif"',
    'italic 400 14px "Instrument Serif"',
    '400 14px "Michroma"',
]

/** Resolves when every face of the theme has loaded (or failed). */
export function themeFontsReady() {
    try {
        return Promise.allSettled(
            THEME_FONTS.map((font) => document.fonts.load(font)),
        ).then(() => document.fonts.ready)
    } catch {
        return Promise.resolve()
    }
}

/** Applies Desktop's theme now and keeps following it. Safe to call more than once. */
export function initTheme() {
    if (!installed) {
        installed = true
        initInsets()
        themeFontsReady()
        // Desktop saving a new theme or corners (in its own page or another tab) is a storage event here
        addEventListener("storage", (event) => {
            if (event.key === null || event.key === SKIN_KEY || event.key === CORNERS_KEY) {
                apply()
            }
        })
        if (!document.body) {
            document.addEventListener("DOMContentLoaded", apply, { once: true })
        }
        askDesktop()
    }
    apply()
    signalReady()
    return themeName()
}

let readySent = false
/** Tells Desktop (the page around an app's frame) that the app has painted in its theme, so the shell fades the frame
 * in now instead of waiting for the frame's load event: `{type: "dimos-ready"}`, once, two frames after initTheme(). */
function signalReady() {
    if (readySent || globalThis.parent === globalThis.self) {
        return
    }
    readySent = true
    requestAnimationFrame(() =>
        requestAnimationFrame(() => {
            try {
                parent.postMessage({ type: "dimos-ready" }, location.origin)
            } catch {
                // not Desktop's origin: nothing to tell
            }
        })
    )
}

const INSET_SIDES = ["top", "bottom", "left", "right"]
let insetsInstalled = false

/**
 * How much of the page Desktop's shell covers (its floating dock over the bottom edge), as `--dim-inset-top/bottom/
 * left/right` on :root, in px; 0 when not inside Desktop. The shell posts `{type: "dimos-inset", top, bottom, left,
 * right}` on load and on every change; this asks for it once too, in case the page started listening late.
 * `initTheme()` calls it. Keep controls, panels and the ends of scrolling lists above `var(--dim-inset-bottom)`.
 */
export function initInsets() {
    if (insetsInstalled) {
        return
    }
    insetsInstalled = true
    const root = document.documentElement
    for (const side of INSET_SIDES) {
        if (!root.style.getPropertyValue(`--dim-inset-${side}`)) {
            root.style.setProperty(`--dim-inset-${side}`, "0px")
        }
    }
    addEventListener("message", (event) => {
        const data = event.data
        if (event.origin !== location.origin || data?.type !== "dimos-inset" || event.source !== parent) {
            return
        }
        for (const side of INSET_SIDES) {
            const value = Number(data[side])
            root.style.setProperty(`--dim-inset-${side}`, `${Number.isFinite(value) && value > 0 ? value : 0}px`)
        }
    })
    try {
        if (parent !== globalThis) {
            parent.postMessage({ type: "dimos-inset-request" }, location.origin)
        }
    } catch {
        // no parent to ask
    }
}

/** The current insets in px, `{ top, bottom, left, right }` (all 0 outside Desktop). */
export function insets() {
    const style = document.documentElement.style
    return Object.fromEntries(
        INSET_SIDES.map((side) => [side, parseFloat(style.getPropertyValue(`--dim-inset-${side}`)) || 0]),
    )
}

/** Calls `listener({ dark, theme, skin, corners })` on every change. Returns an unsubscribe function. */
export function onThemeChange(listener) {
    listeners.add(listener)
    return () => listeners.delete(listener)
}

/** The current palette's colors, for canvases, charts and 3D scenes (read from theme.css's variables). */
export function themeColors() {
    const style = getComputedStyle(document.body ?? document.documentElement)
    const read = (name) => style.getPropertyValue(name).trim()
    return {
        bg: read("--bg"),
        card: read("--card"),
        fg: read("--fg"),
        mutedFg: read("--muted-fg"),
        border: read("--border"),
        primary: read("--primary"),
        ok: read("--ok"),
        warn: read("--warn"),
        danger: read("--danger"),
        info: read("--info"),
        sceneBg: read("--scene-bg"),
        sceneGrid: read("--scene-grid"),
        sceneGridMajor: read("--scene-grid-major"),
        cat: [read("--cat-1"), read("--cat-2"), read("--cat-3"), read("--cat-4")],
        mono: read("--mono"),
        sans: read("--sans"),
    }
}
