// dim-app's first-run / empty / error message (desktop.js emptyState: what's wrong + next-step buttons, themed).
import { useEffect, useRef } from "react"
import { emptyState, type EmptyStateOptions } from "../dim-app/source/desktop.js"

export function EmptyState(props: EmptyStateOptions & { layer?: boolean }) {
    const ref = useRef<HTMLDivElement>(null)
    useEffect(() => {
        ref.current?.replaceChildren(emptyState(props))
    })
    return <div ref={ref} className={props.layer ? "dim-empty-layer" : "dim-empty-host"} />
}
