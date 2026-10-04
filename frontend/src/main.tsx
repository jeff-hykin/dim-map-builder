import { createRoot } from "react-dom/client"
import { captureErrors } from "./core/errors.js"
import { initTheme } from "./dim-app/theme.js"
import "./dim-app/theme.css"
import "./styles.css"
import { App } from "./App.tsx"

// uncaught errors reach Desktop's error feed, where its agent sees them
captureErrors()
initTheme()

createRoot(document.getElementById("root")!).render(<App />)
