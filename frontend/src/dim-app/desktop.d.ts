// Types for desktop.js
export const BUILTIN_APPS: Readonly<Record<"launcher" | "appstore" | "settings" | "desktop", string>>
export type DesktopApp = { name: string; id?: string; title: string; url: string; stopped?: boolean }
export type OpenAppParams = {
    path?: string
    query?: string
    kind?: "" | "blueprint" | "module" | "skill"
    robot?: string
    stream?: string
    selected?: string
}
export function underDesktop(): boolean
export function inDesktopShell(): boolean
export function listApps(options?: { fresh?: boolean }): Promise<DesktopApp[]>
export function findApp(id: string, options?: { fresh?: boolean }): Promise<DesktopApp | null>
export function appInstalled(id: string, options?: { fresh?: boolean }): Promise<boolean>
export function openApp(id: string, params?: OpenAppParams): Promise<boolean>
export type EmptyStateAction = {
    label: string
    onClick?: () => void
    href?: string
    target?: string
    app?: string
    params?: OpenAppParams
    appTitle?: string
    primary?: boolean
}
export type EmptyStateOptions = {
    title: string
    body?: string | Node
    label?: string
    tone?: "info" | "warn" | "ok"
    busy?: boolean
    actions?: EmptyStateAction[]
    testId?: string
}
export function emptyState(options: EmptyStateOptions): HTMLElement
