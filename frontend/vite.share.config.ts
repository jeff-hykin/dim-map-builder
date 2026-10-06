import { defineConfig } from "vite"

// the shared map's viewer (Share → as HTML): one self-contained script, dist/share-viewer.js, that the editor gzips
// into every exported page (src/share/viewer.ts). Built after the page (npm run build), so dist is kept.
export default defineConfig({
    // a library build leaves process.env.NODE_ENV to its user; this one runs as is
    define: { "process.env.NODE_ENV": JSON.stringify("production") },
    build: {
        target: "es2022",
        emptyOutDir: false,
        copyPublicDir: false,
        chunkSizeWarningLimit: 1500,
        lib: { entry: "src/share/viewer.ts", formats: ["iife"], name: "dimShareViewer", fileName: () => "share-viewer.js" },
    },
})
