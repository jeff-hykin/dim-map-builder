import "./dim-theme.js"
import { createRoot } from "react-dom/client"
import "./theme.css"
import "./styles.css"
import { App } from "./App.tsx"

createRoot(document.getElementById("root")!).render(<App />)
