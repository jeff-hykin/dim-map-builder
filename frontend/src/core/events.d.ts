// Types for the vendored dim-app events.js (v0.6.0).
export declare const EVENTS_PATH: string
export declare function appEvents(
    onEvent: (event: any) => void,
    options?: { path?: string; query?: Record<string, string>; onOpen?: () => void; onClose?: () => void },
): () => void
