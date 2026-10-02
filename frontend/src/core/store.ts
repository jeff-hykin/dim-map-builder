// A tiny observable value: the core (no React) writes it, the UI reads it with useStore.
import { useSyncExternalStore } from "react"

export class Store<T extends object> {
    #value: T
    #listeners = new Set<() => void>()
    constructor(value: T) {
        this.#value = value
    }
    get(): T {
        return this.#value
    }
    set(value: T) {
        this.#value = value
        for (const listener of this.#listeners) {
            listener()
        }
    }
    update(patch: Partial<T>) {
        this.set({ ...this.#value, ...patch })
    }
    subscribe = (listener: () => void): (() => void) => {
        this.#listeners.add(listener)
        return () => this.#listeners.delete(listener)
    }
}

export function useStore<T extends object>(store: Store<T>): T {
    return useSyncExternalStore(store.subscribe, () => store.get())
}

/** A store whose value survives reloads (localStorage); a broken or missing entry falls back to the defaults. */
export function persistentStore<T extends object>(key: string, defaults: T): Store<T> {
    let saved: Partial<T> = {}
    try {
        saved = JSON.parse(localStorage.getItem(key) ?? "{}") ?? {}
    } catch {
        // storage blocked or corrupt: run on defaults
    }
    const store = new Store<T>({ ...defaults, ...saved })
    store.subscribe(() => {
        try {
            localStorage.setItem(key, JSON.stringify(store.get()))
        } catch {
            // storage full or blocked: keep running
        }
    })
    return store
}
