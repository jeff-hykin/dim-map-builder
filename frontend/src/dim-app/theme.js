// dim-app theme: the app looks like dimOS Desktop around it (Settings → Appearance), and keeps following it.
//
//     import "./theme.css"   // (or <link rel="stylesheet" href=".../theme.css">)
//     import { initTheme, onThemeChange, themeColors } from "https://esm.sh/gh/jeff-hykin/dim-app@v0.16.0/theme.js"
//     initTheme()                                   // Desktop's /theme.css + html[data-skin], body.science [+ .dark]
//     onThemeChange(({ dark }) => renderer.setClearColor(themeColors().sceneBg))
//     .drive-bar { bottom: calc(12px + var(--dim-inset-bottom)) }   // initTheme() also keeps --dim-inset-* current
//
// All theme values live in dimOS Desktop: it serves every skin's tokens as /theme.css (an app at /apps/<name>/ reaches
// it as ../../theme.css), and theme.css here holds only components written against those tokens. initTheme() links
// Desktop's stylesheet and sets html[data-skin] and html[data-corners] from what Desktop saved (localStorage
// "portal.theme" / "portal.corners", Desktop's origin), and a change reaches every open app at once (the storage
// event). The skin's color-scheme picks theme.css's light or dark structural rules (body.science.dark). Off Desktop:
// theme.css's bundled Portal tokens.

const SKIN_KEY = "portal.theme"
const CORNERS_KEY = "portal.corners"
const DESKTOP_THEME = "/theme.css"
const listeners = new Set()
let installed = false

function stored(key) {
    try {
        return localStorage.getItem(key)
    } catch {
        return null
    }
}

/** Whether this page is one of Desktop's apps (served at /apps/<name>/, so Desktop's /theme.css is there). */
export function onDesktop() {
    return location.pathname.startsWith("/apps/")
}

/** Desktop's current skin id ("portal", "research", "vibeslop", …); "portal" off Desktop. */
export function desktopSkin() {
    return (onDesktop() && stored(SKIN_KEY)) || "portal"
}

/** True when the page is dark: the skin's color-scheme (its tokens) is dark. */
export function isDark() {
    return getComputedStyle(document.documentElement).colorScheme.trim() !== "light"
}

/** "portal" (dark) or "research" (light): which of theme.css's structural rule sets applies. */
export function themeName() {
    return isDark() ? "portal" : "research"
}

/** Desktop's corners setting: "sharp", "rounded", or "theme" (the skin's own). */
export function corners() {
    const value = onDesktop() && stored(CORNERS_KEY)
    return value === "sharp" || value === "rounded" ? value : "theme"
}

/** Links Desktop's theme stylesheet (once, first in <head>, so the app's own CSS still wins ties). */
function linkDesktopTheme() {
    if (!onDesktop() || document.querySelector("link[data-dim-desktop-theme]")) {
        return
    }
    const link = document.createElement("link")
    link.rel = "stylesheet"
    link.href = DESKTOP_THEME
    link.dataset.dimDesktopTheme = ""
    link.addEventListener("load", apply)
    document.head.prepend(link)
}

function apply() {
    const root = document.documentElement
    root.dataset.skin = desktopSkin()
    const corner = corners()
    if (corner === "theme") {
        delete root.dataset.corners
        root.style.removeProperty("--dim-corner-radius")
    } else {
        root.dataset.corners = corner
        root.style.setProperty("--dim-corner-radius", corner === "rounded" ? "10px" : "0px")
    }
    const style = getComputedStyle(root)
    const dark = style.colorScheme.trim() !== "light"
    root.dataset.dimTheme = dark ? "portal" : "research"
    root.toggleAttribute("data-dim-square", /^0(px)?$/.test(style.getPropertyValue("--radius-lg").trim()))
    if (document.body) {
        document.body.classList.add("science")
        document.body.classList.toggle("dark", dark)
    }
    const detail = { dark, theme: dark ? "portal" : "research", skin: desktopSkin(), corners: corner }
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
        linkDesktopTheme()
        // Desktop saving a new skin or corners (in its own page or another tab) is a storage event here
        addEventListener("storage", (event) => {
            if (event.key === null || event.key === SKIN_KEY || event.key === CORNERS_KEY) {
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
