// The standard way an app's backend pushes events to its page (Desktop's docs/events.md): the backend publishes each
// JSON event on its frontend topic `events` (`publishFrontend("events", event)`, or DimAppBackend's publishEvent), and
// the page hears it on its one zenoh-gateway connection, in order (reliable delivery, one key).
//
//     import { appEvents } from "./dim-app/events.js"
//     const stop = appEvents((event) => { ... }, { onOpen, onClose })
//
// `onOpen()` runs when the connection is up (first time and after every reconnect: re-GET then, events sent while it
// was down are gone); `onClose({ wasOpen })` when it's lost. Returns unsubscribe.

import { getZenoh } from "./zenoh.js"

export const EVENTS_TOPIC = "events"

/**
 * @param {(event: any) => void} onEvent called with each parsed JSON event
 * @param {{ topic?: string, onOpen?: () => void, onClose?: (info: { wasOpen: boolean }) => void, zenoh?: any }} [options]
 * @returns {() => void}
 */
export function appEvents(onEvent, options = {}) {
    const zenoh = options.zenoh ?? getZenoh()
    const off = zenoh.subscribeFrontend(options.topic ?? EVENTS_TOPIC, (event) => onEvent(event))
    let wasOpen = zenoh.state === "connected"
    if (wasOpen) {
        queueMicrotask(() => options.onOpen?.())
    }
    const offState = zenoh.onState((state) => {
        if (state === "connected" && !wasOpen) {
            wasOpen = true
            options.onOpen?.()
        } else if (state === "lost") {
            options.onClose?.({ wasOpen })
            wasOpen = false
        }
    })
    return () => {
        off()
        offState()
    }
}
