// The in-app Portal / Research switch (dim-app theme.js); the choice is saved per app, absent = follow the OS.
import { useEffect, useState } from "react"
import { isDark, onThemeChange, themeChoice, toggleTheme } from "./dim-app/theme.js"

export function ThemeToggle({ className = "" }: { className?: string }) {
    const [dark, setDark] = useState(isDark())
    useEffect(() => {
        const off = onThemeChange((detail) => setDark(detail.dark))
        return () => {
            off()
        }
    }, [])
    const name = dark ? "Portal" : "Research"
    return (
        <button
            type="button"
            className={`dim-theme-toggle ${className}`.trim()}
            title={`Theme: ${name}${themeChoice() === "auto" ? " (follows the system)" : ""}. Click to switch.`}
            onClick={(event) => {
                event.stopPropagation()
                toggleTheme()
            }}
        >
            {name}
        </button>
    )
}
