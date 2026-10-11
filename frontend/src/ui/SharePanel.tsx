// Share: a menu in the top bar ("as HTML" for now) and the dialog that makes the file. The page is one self-contained
// .html (share/exportHtml.ts) that anyone can open offline: the map as the 3D view shows it, its floors, its look.
import { useEffect, useState } from "react"
import { Alert, Badge, Button, Column, Menu, Row, Spinner, Text } from "dimos-react"
import { exportHtml, formatBytes, type ShareExport } from "../share/exportHtml.ts"
import type { Context } from "./context.ts"

/** The top bar's Share button and its menu. */
export function ShareMenu({ disabled, onHtml }: { disabled: boolean; onHtml: () => void }) {
    return (
        <Menu
            placement="bottom-end"
            trigger={
                <Button size="sm" icon="link" iconRight="chevron-down" disabled={disabled} title="Share the map" data-action="share-menu">
                    Share
                </Button>
            }
            items={[{ label: "as HTML", icon: "file", shortcut: "one file, opens offline", onSelect: onHtml }]}
        />
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
        <Column gap={10} data-share-panel>
            <Text small muted>
                One .html file with the map, the viewer and its shaders inside: it opens in any browser, offline, with no Desktop. It shows what the 3D view shows now: the current voxels, cropped to the slice, in this look and from this camera{floor && floor.storeys.length > 1 ? ", with the floor selector" : ""}.
            </Text>
            {error && <Alert tone="danger" data-share-error>{error}</Alert>}
            {!made && !error && (
                <Row data-share-busy>
                    <Spinner size={14} /> <Text small muted>Packing the map…</Text>
                </Row>
            )}
            {made && (
                <>
                    <Column gap={1} data-share-summary>
                        <Text small mono muted>
                            {made.voxels.toLocaleString()} voxels{made.sliced ? ` (of ${made.mapVoxels.toLocaleString()}, cropped to the slice)` : ""} at {(scene!.voxelSize * 100).toFixed(0)} cm
                        </Text>
                        <Text small mono muted>{made.floors > 1 ? `${made.floors} floors, with the floor selector` : "one floor"}</Text>
                        <Text small mono muted>
                            voxels {formatBytes(made.voxelBytes)} (from {formatBytes(made.rawBytes)}) · viewer {formatBytes(made.viewerBytes)}
                        </Text>
                    </Column>
                    <Row>
                        <Button size="sm" variant="primary" icon="download" href={url ?? undefined} download={made.fileName} data-action="share-download" data-size={made.html.size}>
                            Download {made.fileName}
                        </Button>
                        <Badge tone="ok" data-share-size>{formatBytes(made.html.size)}</Badge>
                    </Row>
                </>
            )}
        </Column>
    )
}
