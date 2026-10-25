import { readFileSync } from "node:fs"
import process from "node:process"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"
import { mockUploads } from "./dev/mockUploads.ts"
import { esmFetch } from "./tools/esm_fetch.ts"

// served at /apps/<name>/, so every asset URL is relative; the dev server forwards this app's API to its server
// through a running Desktop (DESKTOP_URL, default 7077) and Desktop's /gateways (recordings, dimos gateway, error feed) to Desktop.
// MOCK_UPLOADS=1 answers /gateways/dimos/cloud/* and /gateways/desktop/ui/errors from a dev-only mock (dev/mockUploads.ts)
const desktop = process.env.DESKTOP_URL ?? "http://127.0.0.1:7077"
const app = process.env.APP_NAME ?? "dim-map-builder"
const imports: Record<string, string> = JSON.parse(readFileSync(new URL("./deno.json", import.meta.url), "utf8")).imports
// dimos-react: its TypeScript source at its tag on GitHub (deno.json's URLs, the same files deno reads), fetched and
// compiled into the page; its react, react-dom and three resolve to this page's own
export const dimosReact = Object.fromEntries(Object.entries(imports).filter(([name]) => name.startsWith("dimos-react")))

export default defineConfig({
    base: "./",
    // dimos-react's source is fetched at build (tools/esm_fetch.ts)
    plugins: [esmFetch(dimosReact), react(), ...(process.env.MOCK_UPLOADS ? [mockUploads()] : [])],
    server: {
        proxy: {
            "/api": { target: `${desktop}/apps/${app}`, changeOrigin: true },
            "/gateways": { target: desktop, changeOrigin: true },
        },
    },
    build: { target: "es2022", chunkSizeWarningLimit: 1500 },
})
