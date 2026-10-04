// vendored from jeff-hykin/dim-app v0.8.0 (notify.d.ts); edit it there, then copy
// Types for notify.js
export type Notification = {
    title: string
    body?: string
    kind?: "ok" | "warn" | "agent" | "events"
    sound?: "default" | "urgent" | "battery"
    icon?: string
    actions?: Array<[string, string]>
    details?: unknown
    app?: string
}
export function underDesktop(): boolean
export function notify(notification: Notification, options?: { origin?: string }): Promise<string | number | null>
export function lowLevelAlert(options: {
    low: number
    hysteresis?: number
    notification: (value: number) => Notification
    send?: (notification: Notification) => unknown
}): (value: unknown) => boolean
