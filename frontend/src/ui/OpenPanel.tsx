// Open: pick a recording from Desktop's shared recordings folder (newest first), see its streams, open it; or browse to any .db / .mcap on this machine.
import { useEffect, useState } from "react"
import { Button, Card, Column, EmptyState, IconButton, List, ListRow, Row, SearchInput, Segmented, Text, TextInput } from "dimos-react"
import { files, recordings, type BrowseEntry, type DesktopRecording, type RecordingMetadata } from "../core/api.ts"
import type { Context } from "./context.ts"
import { AppButton } from "./AppButton.tsx"
import { getZenoh } from "../dim-app/source/zenoh.js"

function size(bytes: number) {
    return bytes > 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(0.1, bytes / 1e6).toFixed(1)} MB`
}

function age(seconds: number) {
    const ago = Date.now() / 1000 - seconds
    if (ago < 3600) {
        return `${Math.max(1, Math.round(ago / 60))} min ago`
    }
    if (ago < 86400 * 2) {
        return `${Math.round(ago / 3600)} h ago`
    }
    return new Date(seconds * 1000).toLocaleDateString()
}

export function OpenPanel({ context }: { context: Context }) {
    const [list, setList] = useState<DesktopRecording[] | null>(null)
    const [dir, setDir] = useState("")
    const [problem, setProblem] = useState<string | null>(null)
    const [filter, setFilter] = useState("")
    const [details, setDetails] = useState<RecordingMetadata | null>(null)
    const [opening, setOpening] = useState<string | null>(null)
    const [source, setSource] = useState<"desktop" | "files">("desktop")

    const load = () =>
        recordings
            .list()
            .then((body) => {
                setList(body.recordings)
                setDir(body.dir)
                setProblem(null)
            })
            .catch((error) => setProblem(`Couldn't list Desktop's recordings: ${error.message}`))
    useEffect(() => {
        load()
        // live: a recording added, renamed or deleted anywhere (the Controller, the Recordings app) shows up here
        const zenoh = getZenoh()
        const offs = [zenoh.subscribeDesktop("recordings", () => load()), zenoh.onReconnect(() => load())]
        return () => offs.forEach((off) => off())
    }, [])

    const shown = (list ?? []).filter((r) => r.name.toLowerCase().includes(filter.toLowerCase()))
    const hasLidar = (meta: RecordingMetadata) => meta.streams.some((stream) => /PointCloud2$/.test(stream.type))
    return (
        <Column gap={10}>
            {context.session && (
                <Card title={`Continue: ${context.session.name}`} description={`${context.session.stage === "map" ? `${context.session.voxels.toLocaleString()} voxels` : "not built yet"} · your work is kept automatically`}>
                    <Row>
                        <Button size="sm" variant="primary" onClick={() => context.setModal(context.session?.stage === "map" ? null : "generate")}>
                            Continue
                        </Button>
                    </Row>
                </Card>
            )}
            <Segmented size="sm" value={source} onChange={setSource} items={[["desktop", "Desktop's recordings"], ["files", "Browse files…"]]} data-open-source={source} />
            {source === "files" && <FileBrowser context={context} />}
            {source === "desktop" && <>
                <Text muted small>
                    Maps are built from a robot recording with lidar point clouds. These are Desktop's shared recordings{dir ? ` (${dir})` : ""}; the Controller app records new ones.
                </Text>
                <Row>
                    <SearchInput placeholder="filter…" value={filter} onChange={setFilter} style={{ flex: 1 }} />
                    <IconButton icon="refresh" size="sm" variant="default" label="Reload the list" onClick={load} />
                </Row>
                {problem && (
                    <EmptyState
                        icon="warn"
                        tone="warn"
                        title="Couldn't list Desktop's recordings"
                        message={`${problem}. Desktop may be restarting; try again in a moment.`}
                        actions={<Button variant="primary" onClick={load}>Try again</Button>}
                        data-testid="onboard-recordings-error"
                    />
                )}
                {list && !list.length && (
                    <EmptyState
                        icon="record"
                        title="You don't have any recordings yet"
                        message="You can record a robot using the Controller app: run a blueprint (or a replay), open the Controller's Record tab, and press Record. New recordings show up here by themselves."
                        actions={
                            <>
                                <AppButton app="dim-controller" title="the Controller" variant="primary">Open the Controller</AppButton>
                                <AppButton app="dim-recordings" title="the Recordings app">Open Recordings</AppButton>
                            </>
                        }
                        data-testid="onboard-no-recordings"
                    />
                )}
                {list && list.length > 0 && !shown.length && <Text small muted>No recordings match “{filter}”.</Text>}
                <List>
                    {shown.map((recording) => (
                        <div key={recording.id}>
                            <ListRow
                                icon="file"
                                title={recording.name}
                                subtitle={`${recording.format} · ${size(recording.size)} · ${age(recording.modified)}${recording.id.includes("/") ? ` · ${recording.id.split("/")[0]}` : ""}${!recording.writable ? " · read-only (can't be saved into)" : ""}`}
                                selected={context.session?.recordingPath === recording.path || details?.id === recording.id}
                                onClick={() => recordings.metadata(recording.id).then(setDetails).catch((error) => setProblem(error.message))}
                                data-recording={recording.id}
                            />
                            {details?.id === recording.id && (
                                <Card padded style={{ margin: "4px 0 8px" }}>
                                    <Column gap={1}>
                                        {details.duration != null && <Text small mono muted>{Math.round(details.duration)} s recorded</Text>}
                                        {details.streams.slice(0, 14).map((stream) => (
                                            <Text small mono muted key={stream.name}>
                                                {stream.name} · {stream.type.split(/[./]/).pop()} · {stream.count.toLocaleString()}
                                            </Text>
                                        ))}
                                        {details.streams.length > 14 && <Text small mono muted>…and {details.streams.length - 14} more</Text>}
                                    </Column>
                                    {!hasLidar(details) && (
                                        <EmptyState
                                            icon="warn"
                                            tone="warn"
                                            title="This recording has no lidar point clouds, so no map can be built from it"
                                            message="Pick another recording, or record one with the Controller while a blueprint publishes a point cloud (lidar)."
                                            actions={<AppButton app="dim-controller" title="the Controller">Open the Controller</AppButton>}
                                            data-testid="onboard-no-pointclouds"
                                        />
                                    )}
                                    <Row>
                                        <Button
                                            size="sm"
                                            variant="primary"
                                            loading={opening === recording.id}
                                            disabled={opening !== null || !hasLidar(details)}
                                            data-open={recording.id}
                                            onClick={async () => {
                                                setOpening(recording.id)
                                                await context.openRecording({ id: recording.id, path: recording.path, name: recording.name, writable: recording.writable })
                                                setOpening(null)
                                            }}
                                        >
                                            {opening === recording.id ? "Opening…" : "Open"}
                                        </Button>
                                    </Row>
                                </Card>
                            )}
                        </div>
                    ))}
                </List>
            </>}
        </Column>
    )
}

const LAST_DIR = "map-editor.browse-dir"

function rememberedDir() {
    try {
        return localStorage.getItem(LAST_DIR) ?? ""
    } catch {
        return ""
    }
}

/** any folder on this machine, starting at $HOME (or where it was last): folders and .db / .mcap files, or a path typed in */
function FileBrowser({ context }: { context: Context }) {
    const [listing, setListing] = useState<Awaited<ReturnType<typeof files.browse>> | null>(null)
    const [typed, setTyped] = useState("")
    const [problem, setProblem] = useState<string | null>(null)
    const [picked, setPicked] = useState<BrowseEntry | null>(null)
    const [opening, setOpening] = useState(false)

    const show = (body: Awaited<ReturnType<typeof files.browse>>) => {
        setListing(body)
        setTyped(body.dir)
        setProblem(null)
        setPicked(body.entries.find((entry) => !entry.isDir && entry.name === body.selected) ?? null)
        try {
            localStorage.setItem(LAST_DIR, body.dir)
        } catch {}
    }
    const go = (dir: string) => files.browse(dir).then(show).catch((error) => setProblem(error.message))
    useEffect(() => {
        // a remembered folder that's gone falls back to $HOME
        files.browse(rememberedDir()).catch(() => files.browse("")).then(show).catch((error) => setProblem(error.message))
    }, [])
    const open = async (entry: BrowseEntry) => {
        setOpening(true)
        // the path is the id: the session (and "Continue" after a restart) is keyed by the path
        await context.openRecording({ id: entry.path, path: entry.path, name: entry.name, writable: entry.writable })
        setOpening(false)
    }

    return (
        <Column gap={8}>
            <Row>
                <IconButton icon="arrow-up" size="sm" variant="default" label="Up a folder" disabled={!listing?.parent} onClick={() => listing?.parent && go(listing.parent)} />
                <IconButton icon="home" size="sm" variant="default" label="Home folder" onClick={() => go("")} />
                <TextInput size="sm" mono value={typed} onChange={setTyped} onEnter={go} placeholder="a folder or a recording's path" style={{ flex: 1 }} data-browse-path />
                <IconButton icon="refresh" size="sm" variant="default" label="Reload the folder" onClick={() => go(listing?.dir ?? "")} />
            </Row>
            {problem && <Text small tone="warn">{problem}</Text>}
            {listing && !listing.entries.length && <Text small muted>No folders or recordings (.db / .mcap) here.</Text>}
            <List>
                {listing?.entries.map((entry) => (
                    <div key={entry.path}>
                        <ListRow
                            icon={entry.isDir ? "folder" : "file"}
                            title={entry.name}
                            subtitle={entry.isDir ? undefined : `${size(entry.size)} · ${age(entry.modified)}${!entry.writable ? " · read-only (can't be saved into)" : ""}`}
                            selected={picked?.path === entry.path || context.session?.recordingPath === entry.path}
                            onClick={() => (entry.isDir ? go(entry.path) : setPicked(entry))}
                            data-browse-entry={entry.name}
                        />
                        {picked?.path === entry.path && (
                            <Card padded style={{ margin: "4px 0 8px" }}>
                                <Text small mono muted>{entry.path}</Text>
                                {!entry.writable && <Text small tone="warn">It (or its folder) is read-only, and the Generate steps and Save write into the recording: move it somewhere writable first.</Text>}
                                <Row>
                                    <Button size="sm" variant="primary" loading={opening} disabled={opening} data-browse-open={entry.name} onClick={() => open(entry)}>
                                        {opening ? "Opening…" : "Open"}
                                    </Button>
                                </Row>
                            </Card>
                        )}
                    </div>
                ))}
            </List>
        </Column>
    )
}
