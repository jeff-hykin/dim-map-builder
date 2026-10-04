// dim-app theme: picks the app's palette and keeps it current.
//
//     import "./theme.css"   // (or <link rel="stylesheet" href=".../theme.css">)
//     import { initTheme, mountThemeToggle, onThemeChange, themeColors } from "https://esm.sh/gh/jeff-hykin/dim-app@v0.9.0/theme.js"
//     initTheme()                                   // body.science [+ .dark], html[data-dim-theme]
//     mountThemeToggle(document.querySelector("header"))   // optional in-app Portal / Research toggle
//     onThemeChange(({ dark }) => renderer.setClearColor(themeColors().sceneBg))
//
// Two palettes (theme.css): Portal (dark) and Research (light). The default follows the OS/browser
// `prefers-color-scheme`; an app may save its own choice, per app, in localStorage "dim-app.theme:<app>"
// ("dark" | "light"; absent = follow the OS). Apps keep their own theme, separate from the Desktop's.

const PREFIX = "dim-app.theme:"
const listeners = new Set()
let installed = false

function appName() {
    try {
        const meta = document.querySelector('meta[name="dim-app"]')
        if (meta?.content) {
            return meta.content
        }
        const match = location.pathname.match(/^\/apps\/([^/]+)/)
        return match ? decodeURIComponent(match[1]) : "app"
    } catch {
        return "app"
    }
}

function storageKey() {
    return PREFIX + appName()
}

function osPrefersDark() {
    try {
        return matchMedia("(prefers-color-scheme: dark)").matches
    } catch {
        return true
    }
}

/** The saved choice for this app: "dark", "light", or "auto" (follow the OS). */
export function themeChoice() {
    try {
        const saved = localStorage.getItem(storageKey())
        return saved === "dark" || saved === "light" ? saved : "auto"
    } catch {
        return "auto"
    }
}

/** True when the Portal (dark) palette is showing. */
export function isDark() {
    const choice = themeChoice()
    return choice === "auto" ? osPrefersDark() : choice === "dark"
}

/** "portal" or "research" */
export function themeName() {
    return isDark() ? "portal" : "research"
}

function apply() {
    const dark = isDark()
    const root = document.documentElement
    root.style.colorScheme = dark ? "dark" : "light"
    root.dataset.dimTheme = dark ? "portal" : "research"
    if (document.body) {
        document.body.classList.add("science")
        document.body.classList.toggle("dark", dark)
    }
    const detail = { dark, theme: dark ? "portal" : "research", choice: themeChoice() }
    for (const listener of listeners) {
        try {
            listener(detail)
        } catch (error) {
            console.error(error)
        }
    }
    dispatchEvent(new CustomEvent("dim-theme", { detail }))
}

/** Applies the theme now and keeps it in sync with the OS setting and other tabs. Safe to call more than once. */
export function initTheme() {
    if (!installed) {
        installed = true
        try {
            matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
                if (themeChoice() === "auto") {
                    apply()
                }
            })
        } catch {
            // no matchMedia: whatever the saved choice says
        }
        addEventListener("storage", (event) => {
            if (event.key === null || event.key === storageKey()) {
                apply()
            }
        })
        if (!document.body) {
            document.addEventListener("DOMContentLoaded", apply, { once: true })
        }
    }
    apply()
    return themeName()
}

/** Saves this app's choice ("dark" | "light" | "auto") and applies it. */
export function setThemeChoice(choice) {
    try {
        if (choice === "dark" || choice === "light") {
            localStorage.setItem(storageKey(), choice)
        } else {
            localStorage.removeItem(storageKey())
        }
    } catch {
        // storage blocked: nothing to save
    }
    apply()
}

/** Flips between Portal and Research; picking the OS's own scheme goes back to "auto". */
export function toggleTheme() {
    const wantDark = !isDark()
    setThemeChoice(wantDark === osPrefersDark() ? "auto" : wantDark ? "dark" : "light")
}

/** Calls `listener({ dark, theme, choice })` on every change. Returns an unsubscribe function. */
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

/** Adds a small "Portal" / "Research" toggle button to `container` (end of it). Returns the button. */
export function mountThemeToggle(container) {
    const button = document.createElement("button")
    button.type = "button"
    button.className = "dim-theme-toggle"
    const label = () => {
        button.textContent = isDark() ? "Portal" : "Research"
        button.title = `Theme: ${button.textContent}${
            themeChoice() === "auto" ? " (follows the system)" : ""
        } — click to switch`
    }
    button.addEventListener("click", toggleTheme)
    onThemeChange(label)
    label()
    container?.append(button)
    return button
}
