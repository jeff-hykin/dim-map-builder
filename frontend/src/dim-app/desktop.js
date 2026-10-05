// Opening other apps from an app, and first-run / empty-state messages that send the user there.
//
//     import { appInstalled, emptyState, openApp } from "./dim-app/desktop.js"
//     await openApp("launcher", { kind: "blueprint", stream: "cmd_vel" }) // the Launcher, on blueprints that drive
//     await openApp("dim-controller", { path: "#record" })                 // another app (its install name)
//     if (!(await appInstalled("dim-controller"))) { ... }                 // built-ins are always installed
//
//     const card = emptyState({
//         title: "You need to launch a blueprint with a cmd_vel topic before you can control a robot",
//         body: "The Controller drives whatever robot a running blueprint connects to.",
//         actions: [{ label: "Open the Launcher", app: "launcher", params: { kind: "blueprint", stream: "cmd_vel" } }],
//     })
//     container.replaceChildren(card)
//
// Inside Desktop's shell an app opens in the same window (a `{dimosShell: 1, type: "open_app"}` postMessage, Desktop's
// docs/apps.md); a page opened on its own under Desktop opens it in a new tab (`/?app=<id>`). Outside Desktop nothing
// can be opened: `openApp` resolves to false and `emptyState` leaves app links out.

/** Desktop's built-in views: always "installed". */
export const BUILTIN_APPS = Object.freeze({
    launcher: "Launcher",
    appstore: "App Store",
    settings: "Settings",
    desktop: "Desktop",
})

/** The Launcher's filters `openApp("launcher", params)` sets (the rest are cleared, so an old search can't hide them). */
const LAUNCHER_FIELDS = ["query", "kind", "robot", "selected", "stream"]

/** True when this page is served by dimOS Desktop (under /apps/<name>/). */
export function underDesktop() {
    try {
        return /^\/apps\/[^/]+/.test(location.pathname)
    } catch {
        return false
    }
}

/** True when this page is in an iframe of Desktop's shell (same origin, so it can hear the shell's messages). */
export function inDesktopShell() {
    try {
        return underDesktop() && parent !== globalThis && parent.location.origin === location.origin
    } catch {
        return false
    }
}

let appsCache = null
let appsCacheAt = 0

/**
 * Desktop's installed apps (`GET /api/apps`), cached for 2 s. `[]` outside Desktop or when it fails.
 * @returns {Promise<Array<{ name: string, title: string, url: string, stopped?: boolean }>>}
 */
export async function listApps({ fresh = false } = {}) {
    if (!underDesktop()) {
        return []
    }
    if (!fresh && appsCache && Date.now() - appsCacheAt < 2000) {
        return appsCache
    }
    try {
        const response = await fetch("/api/apps")
        const body = await response.json()
        const list = Array.isArray(body) ? body : body.apps ?? []
        appsCache = list
        appsCacheAt = Date.now()
        return list
    } catch {
        return []
    }
}

/** The installed app `id` names: its install name, or its title (any case). */
export async function findApp(id, options) {
    const wanted = String(id).toLowerCase()
    const apps = await listApps(options)
    return apps.find((app) => app.name?.toLowerCase() === wanted || app.id?.toLowerCase() === wanted) ??
        apps.find((app) => app.title?.toLowerCase() === wanted) ?? null
}

/** Whether `id` (an install name, a title, or a built-in like "launcher") can be opened. False outside Desktop. */
export async function appInstalled(id, options) {
    if (!underDesktop()) {
        return false
    }
    if (String(id).toLowerCase() in BUILTIN_APPS) {
        return true
    }
    return (await findApp(id, options)) !== null
}

/**
 * Opens `id` in Desktop: an installed app (install name or title) or a built-in (`launcher`, `appstore`,
 * `settings`, `desktop`). `params.path` opens the app at that path (e.g. "#record", "?recording=x"). For the
 * Launcher, `params` may set its filters: `query`, `kind` ("blueprint" | "module" | "skill"), `robot`, `stream`
 * (e.g. "cmd_vel": only what has a module with that input or output), `selected`.
 * Resolves to true when Desktop was asked to open it, false when it can't be (outside Desktop, not installed).
 * @param {string} id
 * @param {{ path?: string, query?: string, kind?: string, robot?: string, stream?: string, selected?: string }} [params]
 */
export async function openApp(id, params = {}) {
    if (!underDesktop()) {
        return false
    }
    let app = String(id)
    if (app.toLowerCase() in BUILTIN_APPS) {
        app = app.toLowerCase()
    } else {
        const found = await findApp(app)
        if (!found) {
            return false
        }
        app = found.name ?? found.id
    }
    if (app === "launcher") {
        const state = Object.fromEntries(LAUNCHER_FIELDS.map((field) => [field, params[field] ?? ""]))
        try {
            await fetch("/api/launcher/state", {
                method: "PUT",
                headers: { "content-type": "application/json" },
                body: JSON.stringify(state),
            })
        } catch {
            // the Launcher still opens, unfiltered
        }
    }
    const path = params.path ?? null
    if (inDesktopShell()) {
        parent.postMessage({ dimosShell: 1, type: "open_app", app, path }, location.origin)
        return true
    }
    const url = new URL("/", location.href)
    url.searchParams.set("app", app)
    open(url.href, "_blank")
    return true
}

function button(label, onClick, primary) {
    const element = document.createElement("button")
    element.type = "button"
    element.className = primary ? "dim-btn primary" : "dim-btn"
    element.textContent = label
    element.addEventListener("click", onClick)
    return element
}

/**
 * A first-run / empty / error message: what's wrong, and buttons for the next step. Styled by theme.css (`.dim-empty`,
 * Portal and Research). Wrap it in `.dim-empty-layer` to center it over a canvas, above the dock.
 *
 * An action is `{ label, onClick }` (something in this app), `{ label, href }` (a link), or `{ label, app, params,
 * appTitle }` (another app, `openApp(app, params)`). An app that isn't installed becomes "Install <appTitle> from the
 * App Store" (opens the App Store); outside Desktop app actions are left out. The first action is the primary one
 * unless one says `primary: false`.
 * @param {{ title: string, body?: string | Node, label?: string, tone?: "info" | "warn" | "ok", busy?: boolean,
 *           actions?: Array<{ label: string, onClick?: () => void, href?: string, app?: string, params?: object,
 *                             appTitle?: string, primary?: boolean }>, testId?: string }} options
 * @returns {HTMLElement}
 */
export function emptyState({ title, body, label, tone = "info", busy = false, actions = [], testId } = {}) {
    const card = document.createElement("div")
    card.className = `dim-empty ${tone}${busy ? " busy" : ""}`
    card.setAttribute("role", "status")
    if (testId) {
        card.dataset.testid = testId
    }
    if (label) {
        const kicker = document.createElement("div")
        kicker.className = "dim-empty-label"
        kicker.textContent = label
        card.append(kicker)
    }
    const heading = document.createElement("div")
    heading.className = "dim-empty-title"
    heading.textContent = title
    card.append(heading)
    if (body) {
        const text = document.createElement("div")
        text.className = "dim-empty-body"
        text.append(body)
        card.append(text)
    }
    const row = document.createElement("div")
    row.className = "dim-empty-actions"
    actions.forEach((action, index) => {
        const primary = action.primary ?? index === 0
        if (action.href) {
            const link = document.createElement("a")
            link.className = primary ? "dim-btn primary" : "dim-btn"
            link.href = action.href
            link.target = action.target ?? "_blank"
            link.rel = "noreferrer"
            link.textContent = action.label
            row.append(link)
        } else if (action.app) {
            if (!underDesktop()) {
                return
            }
            const element = button(action.label, () => openApp(action.app, action.params), primary)
            element.dataset.app = action.app
            row.append(element)
            appInstalled(action.app).then((installed) => {
                if (!installed) {
                    const name = action.appTitle ?? action.app
                    const install = button(`Install ${name} from the App Store`, () => openApp("appstore"), primary)
                    install.dataset.app = "appstore"
                    element.replaceWith(install)
                }
            })
        } else if (action.onClick) {
            row.append(button(action.label, action.onClick, primary))
        }
    })
    if (actions.length) {
        card.append(row)
    }
    return card
}
