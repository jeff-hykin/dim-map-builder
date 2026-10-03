import process from "node:process"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"
import { mockUploads } from "./dev/mockUploads.ts"

// served at /apps/<name>/, so every asset URL is relative; the dev server forwards this app's API to its server
// through a running Desktop (DESKTOP_URL, default 7077) and Desktop's /recordings and /dimos to Desktop.
// MOCK_UPLOADS=1 answers /dimos/cloud/*, /dimos/uploads* and /api/errors from a dev-only mock (dev/mockUploads.ts)
const desktop = process.env.DESKTOP_URL ?? "http://127.0.0.1:7077"
const app = process.env.APP_NAME ?? "dim-map-builder"

export default defineConfig({
    base: "./",
    plugins: [react(), ...(process.env.MOCK_UPLOADS ? [mockUploads()] : [])],
    server: {
        proxy: {
            "/api": { target: `${desktop}/apps/${app}`, changeOrigin: true },
            "/recordings": { target: desktop, changeOrigin: true },
            "/dimos": { target: desktop, changeOrigin: true },
        },
    },
    build: { target: "es2022", chunkSizeWarningLimit: 1500 },
})
