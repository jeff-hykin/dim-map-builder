// dim-app theme: the app looks like dimOS Desktop around it (Settings → Appearance), and keeps following it.
//
//     import "./theme.css"   // (or <link rel="stylesheet" href=".../theme.css">)
//     import { initTheme, onThemeChange, themeColors } from "https://esm.sh/gh/jeff-hykin/dim-app@v0.15.0/theme.js"
//     initTheme()                                   // body.science [+ .dark] + Desktop's tokens, html[data-dim-theme]
//     onThemeChange(({ dark }) => renderer.setClearColor(themeColors().sceneBg))
//     .drive-bar { bottom: calc(12px + var(--dim-inset-bottom)) }   // initTheme() also keeps --dim-inset-* current
//
// Apps are served from Desktop's origin (/apps/<name>/), and Desktop's shell publishes its active skin's resolved theme
// tokens in localStorage "portal.themeTokens" ({skin, light, tokens}: every variable of the theme contract, the names
// theme.css uses). initTheme() puts those exact values inline on <body>, so the app shows the same colors, fonts, radii
// and shadows as Desktop in every skin, and a change reaches every open app at once (the storage event). The skin's
// light/dark picks theme.css's structural rules (body.science.dark). With nothing published (off Desktop's origin, or
// opened before Desktop ever ran): theme.css's bundled Portal.

const SKIN_KEY = "portal.theme"
const CORNERS_KEY = "portal.corners"
const TOKENS_KEY = "portal.themeTokens"
const listeners = new Set()
let installed = false
let appliedTokens = []

function stored(key) {
    try {
        return localStorage.getItem(key)
    } catch {
        return null
    }
}

/** The tokens Desktop published for its active skin: `{ skin, light, tokens }`, or null when there are none. */
export function desktopTheme() {
    try {
        const theme = JSON.parse(stored(TOKENS_KEY) ?? "null")
        if (theme && typeof theme.skin === "string" && theme.tokens && typeof theme.tokens === "object") {
            return { skin: theme.skin, light: !!theme.light, tokens: theme.tokens }
        }
    } catch {
        // unreadable: as if none
    }
    return null
}

/** Desktop's current skin id ("portal", "research", "vibeslop", …); "portal" when Desktop can't be asked. */
export function desktopSkin() {
    return desktopTheme()?.skin || stored(SKIN_KEY) || "portal"
}

/** True when the page is dark: Desktop's skin is a dark one (the bundled Portal when nothing is published). */
export function isDark() {
    const theme = desktopTheme()
    return theme ? !theme.light : true
}

/** "portal" (dark) or "research" (light): which of theme.css's structural rule sets applies. */
export function themeName() {
    return isDark() ? "portal" : "research"
}

/** Desktop's corners setting: "sharp", "rounded", or "theme" (the skin's own). */
export function corners() {
    const value = stored(CORNERS_KEY)
    return value === "sharp" || value === "rounded" ? value : "theme"
}

const isZero = (value) => !value || /^0(px)?$/.test(String(value).trim())

function apply() {
    const theme = desktopTheme()
    const dark = theme ? !theme.light : true
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
    if (theme) {
        root.dataset.dimTokens = theme.skin
        root.toggleAttribute("data-dim-square", isZero(theme.tokens["--radius-lg"]))
        if (theme.tokens["--bg"]) {
            root.style.setProperty("background", theme.tokens["--bg"])
        }
    } else {
        delete root.dataset.dimTokens
        root.removeAttribute("data-dim-square")
        root.style.removeProperty("background")
    }
    if (document.body) {
        const body = document.body
        body.classList.add("science")
        body.classList.toggle("dark", dark)
        for (const name of appliedTokens) {
            body.style.removeProperty(name)
        }
        appliedTokens = []
        for (const [name, value] of Object.entries(theme?.tokens ?? {})) {
            if (name.startsWith("--") && typeof value === "string") {
                body.style.setProperty(name, value)
                appliedTokens.push(name)
            }
        }
    }
    const detail = {
        dark,
        theme: dark ? "portal" : "research",
        skin: desktopSkin(),
        corners: corner,
        tokens: theme?.tokens ?? null,
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
        // Desktop publishing a new skin's tokens or corners (in its own page or another tab) is a storage event here
        addEventListener("storage", (event) => {
            if (event.key === null || event.key === TOKENS_KEY || event.key === SKIN_KEY || event.key === CORNERS_KEY) {
                apply()
            }
        })
        if (!document.body) {
            document.addEventListener("DOMContentLoaded", apply, { once: true })
        }
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

/** Calls `listener({ dark, theme, skin, corners, tokens })` on every change. Returns an unsubscribe function. */
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
