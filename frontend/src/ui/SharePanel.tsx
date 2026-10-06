// Share: a menu in the top bar ("as HTML" for now) and the dialog that makes the file. The page is one self-contained
// .html (share/exportHtml.ts) that anyone can open offline: the map as the 3D view shows it, its floors, its look.
import { useEffect, useRef, useState } from "react"
import { exportHtml, formatBytes, type ShareExport } from "../share/exportHtml.ts"
import { Icon } from "./Icon.tsx"
import type { Context } from "./context.ts"

/** The top bar's Share button and its menu. */
export function ShareMenu({ disabled, onHtml }: { disabled: boolean; onHtml: () => void }) {
    const [open, setOpen] = useState(false)
    const box = useRef<HTMLDivElement>(null)
    useEffect(() => {
        if (!open) {
            return
        }
        const close = (event: Event) => {
            if (!box.current?.contains(event.target as Node)) {
                setOpen(false)
            }
        }
        const escape = (event: KeyboardEvent) => event.key === "Escape" && setOpen(false)
        addEventListener("pointerdown", close)
        addEventListener("keydown", escape)
        return () => {
            removeEventListener("pointerdown", close)
            removeEventListener("keydown", escape)
        }
    }, [open])
    return (
        <div className="share-menu" ref={box}>
            <button type="button" className={`dim-btn sm icon ${open ? "on" : ""}`} disabled={disabled} onClick={() => setOpen(!open)} title="Share the map" aria-haspopup="menu" aria-expanded={open} data-action="share-menu">
                <Icon name="link" /> Share <Icon name="chevron-down" />
            </button>
            {open && (
                <div className="dim-menu share-dropdown" role="menu">
                    <button
                        type="button"
                        role="menuitem"
                        className="dim-menu-item"
                        onClick={() => {
                            setOpen(false)
                            onHtml()
                        }}
                        data-action="share-html"
                    >
                        <Icon name="file" /> as HTML <span className="dim">one file, opens offline</span>
                    </button>
                </div>
            )}
        </div>
    )
}

/** The "as HTML" dialog: packs the map, shows what's in it and its size, and downloads it. */
export function SharePanel({ context }: { context: Context }) {
    const { session, scene, floor, ui } = context
    const [made, setMade] = useState<ShareExport | null>(null)
    const [error, setError] = useState<string | null>(null)
    const [url, setUrl] = useState<string | null>(null)
    useEffect(() => {
        if (!session || !scene) {
            return
        }
        let cancelled = false
        exportHtml(scene, session, floor, ui).then(
            (result) => !cancelled && setMade(result),
            (failure) => !cancelled && setError(String(failure?.message ?? failure)),
        )
        return () => {
            cancelled = true
        }
    }, [])
    useEffect(() => {
        if (!made) {
            return
        }
        const href = URL.createObjectURL(made.html)
        setUrl(href)
        return () => URL.revokeObjectURL(href)
    }, [made])
    return (
        <div data-share-panel>
            <div className="panel-head">
                <h2 className="dim-h2">Share as HTML</h2>
                <p>One .html file with the map, the viewer and its shaders inside: it opens in any browser, offline, with no Desktop. It shows what the 3D view shows now: the current voxels, cropped to the slice, in this look and from this camera{floor && floor.storeys.length > 1 ? ", with the floor selector" : ""}.</p>
            </div>
            {error && <div className="dim-badge danger" data-share-error>{error}</div>}
            {!made && !error && <div className="hint" data-share-busy>Packing the map…</div>}
            {made && (
                <>
                    <div className="dim-mono streams" data-share-summary>
                        <span>
                            {made.voxels.toLocaleString()} voxels{made.sliced ? ` (of ${made.mapVoxels.toLocaleString()}, cropped to the slice)` : ""} at {(scene!.voxelSize * 100).toFixed(0)} cm
                        </span>
                        <span>{made.floors > 1 ? `${made.floors} floors, with the floor selector` : "one floor"}</span>
                        <span>
                            voxels {formatBytes(made.voxelBytes)} (from {formatBytes(made.rawBytes)}) · viewer {formatBytes(made.viewerBytes)}
                        </span>
                    </div>
                    <div className="row">
                        <a className="dim-btn sm primary" href={url ?? undefined} download={made.fileName} data-action="share-download" data-size={made.html.size}>
                            <Icon name="download" /> Download {made.fileName}
                        </a>
                        <span className="dim-badge ok" data-share-size>{formatBytes(made.html.size)}</span>
                    </div>
                </>
            )}
        </div>
    )
}
