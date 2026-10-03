import "./dim-theme.js"
import { createRoot } from "react-dom/client"
import { captureErrors } from "./core/errors.js"
import "./theme.css"
import "./styles.css"
import { App } from "./App.tsx"

// uncaught errors reach Desktop's error feed, where its agent sees them
captureErrors()

createRoot(document.getElementById("root")!).render(<App />)
