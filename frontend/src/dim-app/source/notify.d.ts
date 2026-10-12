// Types for notify.js
/** A modal a `modal:<key>` action opens (Desktop 0.2.137+). */
export type NotificationModal = {
    /** default: the notification's title */
    title?: string
    /** markdown */
    body?: string
    /** preformatted: a log, a traceback (opens scrolled to its end) */
    pre?: string
    /** a Copy button: true copies body + pre, a string copies that */
    copy?: boolean | string
    /** the modal's buttons: the same action kinds, plus close */
    actions?: Array<[string, string]>
}
export type Notification = {
    title: string
    body?: string
    kind?: "ok" | "warn" | "agent" | "events"
    sound?: "default" | "urgent" | "battery"
    icon?: string
    actions?: Array<[string, string]>
    details?: unknown
    app?: string
    modals?: Record<string, NotificationModal>
}
export function underDesktop(): boolean
export function notify(notification: Notification, options?: { origin?: string }): Promise<string | number | null>
export function lowLevelAlert(options: {
    low: number
    hysteresis?: number
    notification: (value: number) => Notification
    send?: (notification: Notification) => unknown
}): (value: unknown) => boolean
