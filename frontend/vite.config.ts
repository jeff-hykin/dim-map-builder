import { readFileSync } from "node:fs"
import process from "node:process"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"
import { mockUploads } from "./dev/mockUploads.ts"
import { esmFetch } from "./tools/esm_fetch.ts"

// served at /apps/<name>/, so every asset URL is relative; the dev server forwards this app's API to its server
// through a running Desktop (DESKTOP_URL, default 7077) and Desktop's /recordings and /dimos-gateway to Desktop.
// MOCK_UPLOADS=1 answers /dimos-gateway/cloud/* and /api/errors from a dev-only mock (dev/mockUploads.ts)
const desktop = process.env.DESKTOP_URL ?? "http://127.0.0.1:7077"
const app = process.env.APP_NAME ?? "dim-map-builder"
const imports: Record<string, string> = JSON.parse(readFileSync(new URL("./deno.json", import.meta.url), "utf8")).imports
// deno (tests, deno check) reads dimos-react's TypeScript source at its tag; the page bundles esm.sh's build of that
// same tag, with react, react-dom and three left to this page's own
export const dimosReact = Object.fromEntries(
    Object.entries(imports)
        .filter(([name]) => name.startsWith("dimos-react"))
        .map(([name, url]) => [
            name,
            url.replace(/^https:\/\/raw\.githubusercontent\.com\/jeff-hykin\/dimos-react\/([^/]+)\//, "https://esm.sh/gh/jeff-hykin/dimos-react@$1/") +
            "?external=react,react-dom,three",
        ]),
)

export default defineConfig({
    base: "./",
    // dimos-react comes from esm.sh (deno.json's URLs), fetched and bundled at build (tools/esm_fetch.ts)
    plugins: [esmFetch(dimosReact), react(), ...(process.env.MOCK_UPLOADS ? [mockUploads()] : [])],
    server: {
        proxy: {
            "/api": { target: `${desktop}/apps/${app}`, changeOrigin: true },
            "/recordings": { target: desktop, changeOrigin: true },
            "/dimos-gateway": { target: desktop, changeOrigin: true },
        },
    },
    build: { target: "es2022", chunkSizeWarningLimit: 1500 },
})
