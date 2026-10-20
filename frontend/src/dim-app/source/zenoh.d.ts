// Types for zenoh.js
export type ConnectionState = "connecting" | "connected" | "degraded" | "lost"
export interface ZenohMessage {
    key: string
    bytes: Uint8Array
    timestamp: number
    seq: number
    [field: string]: unknown
}
export interface ZenohInfo {
    namespace: string
    desktop: string
    dimos: string
    apps: string
    zenohPrefix?: string
    zenohGatewayUrl: string
    client: string
    up: boolean
}
export interface SubscribeOptions {
    delivery?: "latest" | "reliable"
    priority?: number
    [option: string]: unknown
}
export interface PayloadOptions extends SubscribeOptions {
    /** default "json" (a sample that isn't JSON is skipped) */
    parse?: "json" | "text" | "bytes"
}
/** Calling it unsubscribes; `update` changes the running channel's options in place (zenoh-gateway's Subscription.update). */
export interface ZenohSubscription {
    (): void
    unsubscribe(): void
    update(changes: Record<string, unknown>): Promise<void>
}
export interface LeaseOptions {
    /** a server-defined group, or a name for this one; unset = unnamed */
    group?: string
    /** keys only the holder publishes on (the gateway drops other pages' puts there) */
    publish?: string[]
    /** keys only the holder subscribes to (other pages' subscriptions there end, "rejected", and are refused) */
    subscribe?: string[]
    /** a newer overlapping lease ends this one ("takenOver") instead of being refused */
    allowTakeover?: boolean
    maxSeconds?: number
    /** heartbeat silence (ms) that ends it, instead of the connection's deadline (zenoh-gateway ≥ 0.5.17; older ignores it) */
    timeoutMs?: number
    /** "takenOver" | "heartbeat" | "maxSeconds" | "disconnected" | "force-expired by peer N" | "released" | "renewed" */
    onLost?: (reason: string) => void
}
export interface Lease {
    readonly group: string
    /** the publish keys */
    readonly keys: string[]
    readonly subscribe: string[]
    readonly expiresInMs: number | null
    /** the `timeoutMs` the gateway applied; null: the connection's heartbeat deadline (or a gateway < 0.5.17) */
    readonly timeoutMs: number | null
    /** why it ended, null while held */
    lost: string | null
    onLost(listener: (reason: string) => void): () => void
    release(): Promise<void>
}
export function updatedOptions<T extends Record<string, unknown>>(options: T, changes: Record<string, unknown>): T
export interface AppZenoh {
    readonly base: string | null
    readonly app: string | null
    readonly ready: Promise<AppZenoh>
    readonly info: ZenohInfo | null
    readonly client: unknown
    readonly state: ConnectionState
    readonly namespace: string | null
    readonly prefix: string | null
    subscribe(
        key: string | ((info: ZenohInfo) => string),
        options: SubscribeOptions,
        callback: (message: ZenohMessage) => void,
    ): ZenohSubscription
    subscribeFrontend<T = unknown>(
        topic: string,
        callback: (payload: T, message: ZenohMessage) => void,
        options?: PayloadOptions,
    ): ZenohSubscription
    subscribeDesktop<T = unknown>(
        type: string,
        callback: (event: T, message: ZenohMessage) => void,
        options?: PayloadOptions,
    ): ZenohSubscription
    subscribeDimos<T = unknown>(
        type: string,
        callback: (event: T, message: ZenohMessage) => void,
        options?: PayloadOptions,
    ): ZenohSubscription
    subscribeJob<T = unknown>(
        jobId: string,
        callback: (event: T, message: ZenohMessage) => void,
        options?: PayloadOptions,
    ): ZenohSubscription
    /** zenoh-gateway >= 0.5.16: exclusive publish/subscribe keys among the gateway's pages (take it before subscribing). */
    lease(options: LeaseOptions): Promise<Lease>
    onState(listener: (state: ConnectionState) => void): () => void
    onReconnect(listener: () => void): () => void
    close(): void
}
export interface GetZenohOptions {
    app?: string
    href?: string
    base?: string
    connect?: (url: string, options: Record<string, unknown>) => Promise<unknown>
    connectOptions?: Record<string, unknown>
    zenohGatewayUrl?: string
    fetch?: typeof fetch
}
export function appBase(href: string): { base: string; app: string | null }
export { checkTopic } from "./topic.js"
export function getZenoh(options?: GetZenohOptions): AppZenoh
