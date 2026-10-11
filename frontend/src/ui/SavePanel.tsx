// Save: write everything into the opened recording (new map/* streams beside its data), see what's written, start over.
import { useState } from "react"
import { api } from "../core/api.ts"
import { JobCard } from "./JobCard.tsx"
import { Badge, Button, Card, Column, Heading, Kbd, Row, Text } from "dimos-react"
import type { Context } from "./context.ts"

export function SavePanel({ context }: { context: Context }) {
    const { session, run, uploads, uploadRecording } = context
    const [confirmDiscard, setConfirmDiscard] = useState(false)
    if (!session) {
        return null
    }
    const a = session.annotations
    const db = session.recordingPath.endsWith(".db")
    const name = (suffix: string) => `map/${suffix}`
    const job = session.job?.kind === "save" ? session.job : null
    return (
        <Column gap={10}>
            <Text small muted>
                Written as new {db ? "streams" : "channels"} in {session.name}, beside its own data (which isn't touched). Saving again {db ? "replaces them" : "appends newer ones in place (the newest is what opens)"}. Your working session is always kept, saved or not.
            </Text>
            {!session.writable && <Text small tone="warn">This recording is in a read-only folder, and the Map Editor never copies recordings: move it into the recordings folder to save into it.</Text>}
            <Column gap={1}>
                <Text small mono muted>{name("voxels")} · PointCloud2 · {session.voxels.toLocaleString()} voxels</Text>
                <Text small mono muted>{name("path")} · Path · the loop-closed path</Text>
                {session.plans.map((plan) => (
                    <Text small mono muted key={plan.index}>
                        {name(`floor_${plan.index}`)} · OccupancyGrid · {plan.width}×{plan.height}
                    </Text>
                ))}
                <Text small mono muted>
                    {name("annotations")} · String (JSON) · {a.boxes.length} boxes, {a.planes.length} planes, {a.points.length} points, {a.planPoints.length} named spots, {a.areas.length} areas ({a.areas.filter((x) => x.kind === "no-go").length} no-go), {a.prisms?.length ?? 0} polygons
                </Text>
                <Text small mono muted>{name("views")} · String (JSON) · {a.views?.length ?? 0} saved 2D views</Text>
            </Column>
            <Row>
                <Button size="sm" variant="primary" data-action="save" disabled={session.job?.state === "running"} onClick={() => run(api.save(session.id))}>
                    Save into recording <Kbd>⌘S</Kbd>
                </Button>
                <Badge tone={session.unsaved ? "warn" : "ok"}>{session.unsaved ? "unsaved changes" : "saved"}</Badge>
            </Row>
            {job && <JobCard job={job} />}
            <Heading level={3}>Upload to Dimensional cloud</Heading>
            <Card description={`Sends ${session.name} (with the map saved in it) to your Dimensional account, from Desktop: it keeps going if you close this page. Uploads queue up; follow them in the uploads panel.`} data-upload-section>
                {session.unsaved
                    ? (
                        <>
                            <Text small muted>You have changes that aren't saved into the recording yet.</Text>
                            <Row wrap>
                                <Button size="sm" variant="primary" icon="upload" disabled={session.job?.state === "running"} onClick={() => uploadRecording(true)} data-action="save-and-upload">
                                    Save, then upload
                                </Button>
                                <Button size="sm" onClick={() => uploadRecording(false)} data-action="upload-as-saved">
                                    Upload as last saved
                                </Button>
                            </Row>
                        </>
                    )
                    : (
                        <Row>
                            <Button size="sm" variant="primary" icon="upload" onClick={() => uploadRecording(false)} data-action="upload-recording">
                                Upload
                            </Button>
                            <Button size="sm" variant="ghost" onClick={() => uploads.setPanelOpen(true)} data-action="show-uploads">
                                Show uploads
                            </Button>
                        </Row>
                    )}
            </Card>
            <Heading level={3}>Start over</Heading>
            <Card description="Forget this working copy (the edits not saved into the recording are lost). The recording itself is untouched.">
                {confirmDiscard
                    ? (
                        <Row>
                            <Button size="sm" variant="danger" onClick={() => run(api.discard(session.id), "Working copy discarded")}>
                                Yes, discard
                            </Button>
                            <Button size="sm" onClick={() => setConfirmDiscard(false)}>
                                Keep it
                            </Button>
                        </Row>
                    )
                    : (
                        <Row>
                            <Button size="sm" variant="danger" onClick={() => setConfirmDiscard(true)}>
                                Discard working copy…
                            </Button>
                        </Row>
                    )}
            </Card>
        </Column>
    )
}
