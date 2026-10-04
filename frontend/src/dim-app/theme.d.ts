// Types for theme.js
export type ThemeChoice = "dark" | "light" | "auto"
export type ThemeDetail = { dark: boolean; theme: "portal" | "research"; choice: ThemeChoice }
export function themeChoice(): ThemeChoice
export function isDark(): boolean
export function themeName(): "portal" | "research"
export function initTheme(): "portal" | "research"
export function setThemeChoice(choice: ThemeChoice): void
export function toggleTheme(): void
export function onThemeChange(listener: (detail: ThemeDetail) => void): () => boolean
export function themeColors(): {
    bg: string
    card: string
    fg: string
    mutedFg: string
    border: string
    primary: string
    ok: string
    warn: string
    danger: string
    info: string
    sceneBg: string
    sceneGrid: string
    sceneGridMajor: string
    cat: string[]
    mono: string
    sans: string
}
export function mountThemeToggle(container: Element | null | undefined): HTMLButtonElement
