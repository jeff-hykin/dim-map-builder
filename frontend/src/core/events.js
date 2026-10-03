// The standard way an app's backend pushes to its page: a websocket at the app-relative `api/events/ws`, one JSON event
// per text message. One socket per page (not SSE): every app shares Desktop's origin, and an SSE stream holds one of
// the browser's 6 HTTP/1.1 connections per host, so a few open apps starve the rest. Websockets don't count there.
//
//     import { appEvents } from "https://esm.sh/gh/jeff-hykin/dim-app@v0.6.0/events.js"
//     const stop = appEvents((event) => { ... }, { query: { page: id }, onOpen, onClose })
//
// Reconnects forever with backoff (0.5s doubling to 10s, reset after a connection that lived 5s). Returns unsubscribe.

export const EVENTS_PATH = "api/events/ws"

/**
 * @param {(event: any) => void} onEvent called with each parsed JSON event
 * @param {{ path?: string, query?: Record<string, string>, onOpen?: () => void, onClose?: () => void }} [options]
 * @returns {() => void} closes the socket and stops reconnecting
 */
export function appEvents(onEvent, options = {}) {
    const url = new URL(options.path ?? EVENTS_PATH, location.href)
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:"
    for (const [key, value] of Object.entries(options.query ?? {})) {
        url.searchParams.set(key, value)
    }
    let socket = null
    let closed = false
    let delay = 500
    let timer = null
    const connect = () => {
        const openedAt = Date.now()
        let wasOpen = false
        socket = new WebSocket(url)
        socket.onopen = () => {
            wasOpen = true
            options.onOpen?.()
        }
        socket.onmessage = ({ data }) => {
            if (typeof data !== "string") {
                return
            }
            let event
            try {
                event = JSON.parse(data)
            } catch {
                return // a malformed event is dropped
            }
            onEvent(event)
        }
        socket.onclose = () => {
            if (wasOpen) {
                options.onClose?.()
            }
            if (closed) {
                return
            }
            if (Date.now() - openedAt > 5000) {
                delay = 500
            }
            timer = setTimeout(connect, delay)
            delay = Math.min(delay * 2, 10000)
        }
    }
    connect()
    return () => {
        closed = true
        clearTimeout(timer)
        socket?.close()
    }
}
