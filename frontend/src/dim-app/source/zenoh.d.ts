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
