// Notifications → dimOS Desktop's notification center (POST /api/notifications on the Desktop origin).
//
//     import { notify } from "https://esm.sh/gh/jeff-hykin/dim-app@v0.16.0/notify.js"
//     notify({ title: "Battery low", body: "Go2 at 14%", kind: "warn", sound: "battery" })
//
// Apps are served same-origin under /apps/<name>/, so the path is absolute. Outside Desktop (a page opened on its own
// dev server) it does nothing and resolves to null. Never throws.
//
// Fields (Desktop's API):
//   title, body          strings (required)
//   kind                 "ok" | "warn" | "agent" | "events"            (default "ok")
//   sound                "default" | "urgent" | "battery"              (default "default")
//   icon                 URL of an image; default: this app's icon (/api/apps/<app>/icon)
//   actions              [[label, action], ...]  e.g. [["Open", "open_app:controller"]]
//   details              anything JSON (shown expanded in the panel)
//   app                  default: this app's name (from /apps/<name>/ or <meta name="dim-app">)

function appName() {
    try {
        const meta = document.querySelector('meta[name="dim-app"]')
        if (meta?.content) {
            return meta.content
        }
        const match = location.pathname.match(/^\/apps\/([^/]+)/)
        return match ? decodeURIComponent(match[1]) : null
    } catch {
        return null
    }
}

/** True when this page is served by dimOS Desktop (under /apps/<name>/). */
export function underDesktop() {
    try {
        return /^\/apps\/[^/]+/.test(location.pathname)
    } catch {
        return false
    }
}

/**
 * Posts a notification to Desktop. Resolves to the new notification's id, or null when not under Desktop or it failed.
 * @param {{ title: string, body?: string, kind?: "ok"|"warn"|"agent"|"events", sound?: "default"|"urgent"|"battery",
 *           icon?: string, actions?: Array<[string, string]>, details?: unknown, app?: string }} notification
 * @param {{ origin?: string }} [options] origin: Desktop's base URL (default: this page's origin)
 * @returns {Promise<string | number | null>}
 */
export async function notify(notification, options = {}) {
    try {
        if (!options.origin && !underDesktop()) {
            console.debug("[dim-app] notify: not under dimOS Desktop, skipped:", notification?.title)
            return null
        }
        const app = notification.app ?? appName() ?? undefined
        const payload = {
            title: String(notification.title ?? "").slice(0, 200),
            body: String(notification.body ?? "").slice(0, 2000),
            kind: notification.kind ?? "ok",
            sound: notification.sound ?? "default",
            app,
            icon: notification.icon ?? (app ? `/api/apps/${encodeURIComponent(app)}/icon` : undefined),
            actions: notification.actions,
            details: notification.details,
        }
        const url = new URL("/api/notifications", options.origin ?? location.origin)
        const response = await fetch(url, {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(payload),
        })
        if (!response.ok) {
            return null
        }
        const result = await response.json().catch(() => ({}))
        return result?.id ?? null
    } catch {
        return null
    }
}

/**
 * A notifier for a level that crosses thresholds (battery, temperature): notifies once when `value` drops to `low`
 * or below, and re-arms only after it rises above `low + hysteresis`.
 *     const battery = lowLevelAlert({ low: 20, hysteresis: 5, notification: (v) => ({ title: "Battery low", body: `${v}%`, kind: "warn", sound: "battery" }) })
 *     battery(percent)  // on every reading
 */
export function lowLevelAlert({ low, hysteresis = 5, notification, send = notify }) {
    let armed = true
    return (value) => {
        if (typeof value !== "number" || !Number.isFinite(value)) {
            return false
        }
        if (armed && value <= low) {
            armed = false
            send(notification(value))
            return true
        }
        if (!armed && value > low + hysteresis) {
            armed = true
        }
        return false
    }
}
