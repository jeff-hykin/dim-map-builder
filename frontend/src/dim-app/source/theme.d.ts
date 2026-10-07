// Types for theme.js
export type Corners = "sharp" | "rounded" | "theme"
export type ThemeDetail = {
    dark: boolean
    theme: "portal" | "research"
    /** Desktop's skin id ("portal", "research", "vibeslop", …) */
    skin: string
    corners: Corners
}
/** Whether this page is one of Desktop's apps (served at /apps/<name>/, where Desktop's /theme.css is) */
export function onDesktop(): boolean
export function desktopSkin(): string
export function corners(): Corners
export function isDark(): boolean
export function themeName(): "portal" | "research"
export const THEME_FONTS: string[]
export function themeFontsReady(): Promise<unknown>
export function initTheme(): "portal" | "research"
export function initInsets(): void
export function insets(): { top: number; bottom: number; left: number; right: number }
export function onThemeChange(
    listener: (detail: ThemeDetail) => void,
): () => boolean
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
