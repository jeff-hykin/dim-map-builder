import { defineConfig } from "vite"
import { esmFetch } from "./tools/esm_fetch.ts"
import { dimosReact } from "./vite.config.ts"

// the shared map's viewer (Share → as HTML): one self-contained script, dist/share-viewer.js, that the editor gzips
// into every exported page (src/share/viewer.tsx). Built after the page (deno task build), so dist is kept.
export default defineConfig({
    // dimos-react's source from GitHub, as the page bundles it (vite.config.ts)
    plugins: [esmFetch(dimosReact)],
    // a library build leaves process.env.NODE_ENV to its user; this one runs as is
    define: { "process.env.NODE_ENV": JSON.stringify("production") },
    build: {
        target: "es2022",
        emptyOutDir: false,
        copyPublicDir: false,
        chunkSizeWarningLimit: 1500,
        lib: { entry: "src/share/viewer.tsx", formats: ["iife"], name: "dimShareViewer", fileName: () => "share-viewer.js" },
    },
})
