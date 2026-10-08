// A page's one connection to Desktop's zenoh-gateway (Desktop's docs/events.md): backend → frontend is always zenoh,
// frontend → backend is plain HTTP. Every subscription on a page shares this one connection (a module singleton, kept on
// globalThis so two copies of dim-app on one page share it too).
//
//     import { getZenoh } from "./dim-app/source/zenoh.js"
//     const zenoh = getZenoh() // app name from /apps/<name>/ in the page's URL
//     const off = zenoh.subscribeFrontend("status", (status) => render(status)) // <ns>/apps/<name>/frontend/status
//     zenoh.subscribeDesktop("apps", () => reloadApps()) // <ns>/desktop/events/apps ("*" = every type)
//     zenoh.onReconnect(() => reloadEverything()) // events published while the link was down are gone
//
// Where things are comes from Desktop's `GET /api/desktop/zenoh?app=<name>` (relative to the app:
// `../../api/desktop/zenoh`). The client is vendor/zenoh-gateway/zenoh_gateway.js (vendored, so nothing loads from the network); an app
// that already has its own copy passes `connect`. Discovery and connecting retry with backoff (0.5 s doubling to 10 s);
// once connected the client reconnects by itself and its subscriptions come back on their own.

import { checkTopic } from "./topic.js"

export { checkTopic }

const SHARED = Symbol.for("dim-app.zenoh")
const RETRY_MIN_MS = 500
const RETRY_MAX_MS = 10_000

/**
 * Desktop's base URL and the app's install name, from a page URL: `<origin><prefix>/apps/<name>/…` → base
 * `<origin><prefix>/`, app `<name>`; a page outside /apps/ → base `<origin>/`, app null.
 * @param {string} href
 * @returns {{ base: string, app: string | null }}
 */
export function appBase(href) {
    const url = new URL(href)
    const match = url.pathname.match(/^(.*?)\/apps\/([^/]+)\//)
    if (!match) {
        return { base: `${url.origin}/`, app: null }
    }
    return { base: `${url.origin}${match[1]}/`, app: decodeURIComponent(match[2]) }
}

/**
 * `options` with `changes` applied as zenoh-gateway's `Subscription.update` applies them: `null` removes an option (back
 * to the gateway's default), `encodeOptions` merges.
 */
export function updatedOptions(options, changes) {
    const merged = { ...options }
    for (const [name, value] of Object.entries(changes ?? {})) {
        if (name === "encodeOptions" && value !== null) {
            merged.encodeOptions = { ...options.encodeOptions, ...value }
        } else if (value === null) {
            delete merged[name]
        } else {
            merged[name] = value
        }
    }
    return merged
}

const decoder = new TextDecoder()

/** A sample's payload: JSON (default; a sample that isn't JSON is skipped), "text" or "bytes". */
function parsePayload(message, parse) {
    if (parse === "bytes") {
        return { ok: true, value: message.bytes }
    }
    const text = decoder.decode(message.bytes)
    if (parse === "text") {
        return { ok: true, value: text }
    }
    try {
        return { ok: true, value: JSON.parse(text) }
    } catch {
        console.debug(`[dim-app] zenoh: ${message.key} isn't JSON; skipped`)
        return { ok: false }
    }
}

class AppZenoh {
    /** @type {any} the discovery answer ({ namespace, desktop, dimos, apps, zenohPrefix?, zenohGatewayUrl, client, up }) */
    info = null
    /** @type {any} the zenoh-gateway client (ZenohGateway) once connected */
    client = null
    /** @type {"connecting" | "connected" | "degraded" | "lost"} */
    state = "connecting"
    /** @type {Map<string, { key: (info: any) => string, options: object, callbacks: Set<(message: any) => void>, handle: any }>} */
    #subscriptions = new Map()
    #stateListeners = new Set()
    #reconnectListeners = new Set()
    #wasLost = false
    #options
    #closed = false

    constructor(options = {}) {
        const href = options.href ?? globalThis.location?.href
        const found = href ? appBase(href) : { base: null, app: null }
        this.#options = options
        /** Desktop's base URL ("<origin>/"), or null outside a browser with no `href` */
        this.base = options.base ?? found.base
        /** the app's install name, or null on a page that isn't an app's */
        this.app = options.app ?? found.app
        /** resolves once connected the first time */
        this.ready = this.#start()
        this.ready.catch(() => {}) // only rejects after close()
    }

    /** `<ns>`, once discovered */
    get namespace() {
        return this.info?.namespace ?? null
    }

    /** `<ns>/apps/<name>`, once discovered (null on a page that isn't an app's) */
    get prefix() {
        return this.info?.zenohPrefix ?? null
    }

    async #retry(what, attempt) {
        let delay = RETRY_MIN_MS
        for (;;) {
            try {
                return await attempt()
            } catch (error) {
                if (this.#closed) {
                    throw error
                }
                console.debug(`[dim-app] zenoh: ${what} failed (${error?.message ?? error}); retrying in ${delay} ms`)
                await new Promise((resolve) => setTimeout(resolve, delay))
                delay = Math.min(delay * 2, RETRY_MAX_MS)
            }
        }
    }

    async #start() {
        const fetchImpl = this.#options.fetch ?? globalThis.fetch
        const discovery = new URL("api/desktop/zenoh", this.base)
        if (this.app) {
            discovery.searchParams.set("app", this.app)
        }
        this.info = await this.#retry("discovery", async () => {
            const response = await fetchImpl(discovery.href)
            if (!response.ok) {
                throw new Error(`GET ${discovery.pathname}: HTTP ${response.status}`)
            }
            return await response.json()
        })
        const connect = this.#options.connect ?? (await import("./vendor/zenoh-gateway/zenoh_gateway.js")).connect
        const gateway = this.#options.zenohGatewayUrl ?? this.info.zenohGatewayUrl ?? "/zenoh-gateway"
        const url = /^[a-z]+:\/\//i.test(gateway) ? gateway : new URL(gateway.replace(/^\/+/, ""), this.base).href
        this.client = await this.#retry(
            "connecting to zenoh-gateway",
            () => connect(url, this.#options.connectOptions ?? {}),
        )
        this.client.onState((state) => this.#setState(state))
        this.#setState(this.client.state ?? "connected")
        for (const entry of this.#subscriptions.values()) {
            this.#open(entry)
        }
        return this
    }

    #setState(state) {
        if (state === this.state) {
            return
        }
        this.state = state
        for (const listener of [...this.#stateListeners]) {
            try {
                listener(state)
            } catch (error) {
                console.error("[dim-app] zenoh: onState listener threw", error)
            }
        }
        if (state === "lost") {
            this.#wasLost = true
        } else if (state === "connected" && this.#wasLost) {
            this.#wasLost = false
            for (const listener of [...this.#reconnectListeners]) {
                try {
                    listener()
                } catch (error) {
                    console.error("[dim-app] zenoh: onReconnect listener threw", error)
                }
            }
        }
    }

    #open(entry) {
        const key = entry.key(this.info)
        entry.handle = this.client.subscribe(key, entry.options, (message) => {
            for (const callback of [...entry.callbacks]) {
                try {
                    callback(message)
                } catch (error) {
                    console.error(`[dim-app] zenoh: subscriber of ${key} threw`, error)
                }
            }
        })
    }

    /**
     * A raw subscription on the shared connection: `callback(message)` with zenoh-gateway's `{ key, bytes, timestamp, … }`.
     * `key` is a key expression, or a function of the discovery answer (for keys under `<ns>`). Subscriptions to the
     * same key with the same options share one zenoh-gateway channel.
     *
     * The returned function unsubscribes; it also has `.unsubscribe()` and `.update(changes)`, which changes the running
     * channel's options in place (zenoh-gateway's `Subscription.update`: maxHz, minQuality, qualityToHzTradeoff,
     * bandwidthPriority, maxBitrate, minResolutionScale, maxResolution, playoutDelay, encodeOptions.quality; `null` puts
     * one back to its default; the gateway refuses anything else). A channel shared with other subscribers isn't
     * changed under them: this subscriber moves to a channel with the new options instead.
     * @returns {(() => void) & { unsubscribe(): void, update(changes: object): Promise<void> }}
     */
    subscribe(key, options, callback) {
        const keyOf = typeof key === "function" ? key : () => key
        const id = typeof key === "function" ? `${key.dimAppKey ?? key}` : key
        const entryIdOf = (options) => `${id}\u0000${JSON.stringify(options ?? {})}`
        const join = (options) => {
            const entryId = entryIdOf(options)
            let entry = this.#subscriptions.get(entryId)
            if (!entry) {
                entry = { id: entryId, key: keyOf, options: options ?? {}, callbacks: new Set(), handle: null }
                this.#subscriptions.set(entryId, entry)
                if (this.client) {
                    this.#open(entry)
                }
            }
            entry.callbacks.add(own)
            return entry
        }
        const leave = (entry) => {
            entry.callbacks.delete(own)
            if (entry.callbacks.size === 0 && this.#subscriptions.get(entry.id) === entry) {
                this.#subscriptions.delete(entry.id)
                entry.handle?.close()
            }
        }
        const own = (message) => callback(message)
        let entry = join(options)
        let closed = false
        const unsubscribe = () => {
            closed = true
            leave(entry)
        }
        unsubscribe.unsubscribe = unsubscribe
        unsubscribe.update = async (changes) => {
            if (closed) {
                throw new Error(`[dim-app] zenoh: update on a closed subscription to ${id}`)
            }
            const next = updatedOptions(entry.options, changes)
            const nextId = entryIdOf(next)
            if (nextId === entry.id) {
                return
            }
            if (entry.callbacks.size > 1 || this.#subscriptions.has(nextId)) {
                // shared (or another channel already has these options): move this subscriber, the others keep theirs
                const from = entry
                entry = join(next)
                leave(from)
                return
            }
            // alone on its channel: the same channel and track take the new options (put back if the gateway refuses)
            const rekey = (target, id, options) => {
                this.#subscriptions.delete(target.id)
                Object.assign(target, { id, options })
                this.#subscriptions.set(id, target)
            }
            const [target, previousId, previousOptions] = [entry, entry.id, entry.options]
            rekey(target, nextId, next)
            try {
                await target.handle?.update(changes)
            } catch (error) {
                if (target.id === nextId && !this.#subscriptions.has(previousId)) {
                    rekey(target, previousId, previousOptions)
                }
                throw error
            }
        }
        return unsubscribe
    }

    #subscribeUnder(describe, keyOf, callback, { parse = "json", delivery = "reliable", ...rest } = {}) {
        keyOf.dimAppKey = describe
        return this.subscribe(keyOf, { delivery, ...rest }, (message) => {
            const payload = parsePayload(message, parse)
            if (payload.ok) {
                callback(payload.value, message)
            }
        })
    }

    /**
     * This app's frontend topic `<ns>/apps/<name>/frontend/<topic>` (`*`, `**` allowed): `callback(payload, message)`,
     * payload parsed as JSON by default (`parse: "text" | "bytes"` otherwise). Reliable delivery by default; pass
     * `delivery: "latest"` for state that replaces itself (frames, poses).
     * @returns {() => void} unsubscribe
     */
    subscribeFrontend(topic, callback, options = {}) {
        checkTopic(topic, { wildcards: true })
        if (!this.app && !this.#options.app) {
            console.warn(
                "[dim-app] zenoh: subscribeFrontend on a page that isn't under /apps/<name>/; pass getZenoh({ app })",
            )
        }
        return this.#subscribeUnder(
            `frontend:${topic}`,
            (info) => `${info.zenohPrefix ?? `${info.apps}/${this.app}`}/frontend/${topic}`,
            callback,
            options,
        )
    }

    /** Desktop's events `<ns>/desktop/events/<type>` ("*" = every type): `callback(event)`. */
    subscribeDesktop(type, callback, options = {}) {
        const chunk = type === "*" ? "**" : checkTopic(type)
        return this.#subscribeUnder(
            `desktop:${chunk}`,
            (info) => `${info.desktop ?? `${info.namespace}/desktop`}/events/${chunk}`,
            callback,
            options,
        )
    }

    /** The dimos server's events `<ns>/dimos/events/<type>` ("*" = every type): `callback(event)`. */
    subscribeDimos(type, callback, options = {}) {
        const chunk = type === "*" ? "**" : checkTopic(type)
        return this.#subscribeUnder(
            `dimos:${chunk}`,
            (info) => `${info.dimos ?? `${info.namespace}/dimos`}/events/${chunk}`,
            callback,
            options,
        )
    }

    /** A Desktop job's output `<ns>/desktop/jobs/<jobId>`: `{type:"line", n, line}` … `{type:"done", ok, error, lines}`. */
    subscribeJob(jobId, callback, options = {}) {
        checkTopic(String(jobId))
        return this.#subscribeUnder(
            `job:${jobId}`,
            (info) => `${info.desktop ?? `${info.namespace}/desktop`}/jobs/${jobId}`,
            callback,
            options,
        )
    }

    /** `listener(state)` on every connection state change ("connecting" | "connected" | "degraded" | "lost"). */
    onState(listener) {
        this.#stateListeners.add(listener)
        return () => this.#stateListeners.delete(listener)
    }

    /** `listener()` when the connection is back after being lost: re-GET what you show (snapshot + live). */
    onReconnect(listener) {
        this.#reconnectListeners.add(listener)
        return () => this.#reconnectListeners.delete(listener)
    }

    /** Close the connection (tests; a page never needs to). The next getZenoh() makes a new one. */
    close() {
        this.#closed = true
        for (const entry of this.#subscriptions.values()) {
            entry.handle?.close()
        }
        this.#subscriptions.clear()
        this.client?.close?.()
        if (globalThis[SHARED] === this) {
            delete globalThis[SHARED]
        }
    }
}

/**
 * The page's one zenoh-gateway connection (created on the first call; later calls' options are ignored).
 * @param {{ app?: string, href?: string, base?: string, connect?: (url: string, options: object) => Promise<any>,
 *   connectOptions?: object, zenohGatewayUrl?: string, fetch?: typeof fetch }} [options]
 *   `app`: the install name (default: from /apps/<name>/ in the URL); `connect`: a zenoh-gateway client's connect
 *   (default: the vendored vendor/zenoh-gateway/zenoh_gateway.js); `connectOptions`: passed to it (e.g. `{ heartbeatHz: 10 }` for deadman publishers)
 * @returns {AppZenoh}
 */
export function getZenoh(options = {}) {
    globalThis[SHARED] ??= new AppZenoh(options)
    return globalThis[SHARED]
}
