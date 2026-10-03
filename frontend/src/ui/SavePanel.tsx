// Save: write everything into the opened recording (new map/* streams beside its data), see what's written, start over.
import { useState } from "react"
import { api } from "../core/api.ts"
import { JobCard } from "./JobCard.tsx"
import { Icon } from "./Icon.tsx"
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
        <div>
            <div className="panel-head">
                <h2 className="dim-h2">Save into the recording</h2>
                <p>Written as new {db ? "streams" : "channels"} in {session.name}, beside its own data (which isn't touched). Saving again replaces them. Your working copy is always kept, saved or not.</p>
            </div>
            {!session.writable && <div className="hint">This recording is in a read-only folder: saving first copies it into Desktop's recordings folder (map-builder/) and writes there.</div>}
            <div className="dim-mono streams">
                <span>{name("voxels")} · PointCloud2 · {session.voxels.toLocaleString()} voxels</span>
                <span>{name("path")} · Path · the loop-closed path</span>
                {session.plans.map((plan) => (
                    <span key={plan.index}>
                        {name(`floor_${plan.index}`)} · OccupancyGrid · {plan.width}×{plan.height}
                    </span>
                ))}
                <span>
                    {name("annotations")} · String (JSON) · {a.boxes.length} boxes, {a.planes.length} planes, {a.points.length} points, {a.planPoints.length} named spots, {a.areas.length} areas ({a.areas.filter((x) => x.kind === "no-go").length} no-go), {a.prisms?.length ?? 0} polygons
                </span>
                <span>{name("views")} · String (JSON) · {a.views?.length ?? 0} saved 2D views</span>
            </div>
            <div className="row">
                <button type="button" className="dim-btn sm primary" data-action="save" disabled={session.job?.state === "running"} onClick={() => run(api.save(session.id))}>
                    Save into recording <kbd>⌘S</kbd>
                </button>
                <span className={`dim-badge ${session.unsaved ? "warn" : "ok"}`}>{session.unsaved ? "unsaved changes" : "saved"}</span>
            </div>
            {job && <JobCard job={job} />}
            <h3 className="dim-label">Upload to Dimensional cloud</h3>
            <div className="dim-panel tool-card" data-upload-section>
                <div className="about">
                    Sends {session.name} (with the map saved in it) to your Dimensional account, from Desktop: it keeps going if you close this page. Uploads queue up; follow them in the uploads panel.
                </div>
                {session.unsaved ? (
                    <>
                        <div className="hint">You have changes that aren't saved into the recording yet.</div>
                        <div className="row">
                            <button type="button" className="dim-btn sm primary" disabled={session.job?.state === "running"} onClick={() => uploadRecording(true)} data-action="save-and-upload">
                                <Icon name="upload" /> Save, then upload
                            </button>
                            <button type="button" className="dim-btn sm" onClick={() => uploadRecording(false)} data-action="upload-as-saved">
                                Upload as last saved
                            </button>
                        </div>
                    </>
                ) : (
                    <div className="row">
                        <button type="button" className="dim-btn sm primary" onClick={() => uploadRecording(false)} data-action="upload-recording">
                            <Icon name="upload" /> Upload
                        </button>
                        <button type="button" className="dim-btn sm ghost" onClick={() => uploads.setPanelOpen(true)} data-action="show-uploads">
                            Show uploads
                        </button>
                    </div>
                )}
            </div>
            <h3 className="dim-label">Start over</h3>
            <div className="dim-panel tool-card">
                <div className="about">Forget this working copy (the edits not saved into the recording are lost). The recording itself is untouched.</div>
                {confirmDiscard ? (
                    <div className="row">
                        <button type="button" className="dim-btn sm danger" onClick={() => run(api.discard(session.id), "Working copy discarded")}>
                            Yes, discard
                        </button>
                        <button type="button" className="dim-btn sm" onClick={() => setConfirmDiscard(false)}>
                            Keep it
                        </button>
                    </div>
                ) : (
                    <button type="button" className="dim-btn sm danger" onClick={() => setConfirmDiscard(true)}>
                        Discard working copy…
                    </button>
                )}
            </div>
        </div>
    )
}
