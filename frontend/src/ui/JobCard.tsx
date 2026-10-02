// A background job's progress: stage, a bar over the whole job, done/total, elapsed, ETA, and Cancel.
import type { Job } from "../core/api.ts"

export function duration(seconds: number | null | undefined): string {
    if (seconds == null || !Number.isFinite(seconds)) {
        return "–"
    }
    if (seconds < 60) {
        return `${Math.round(seconds)} s`
    }
    const minutes = Math.floor(seconds / 60)
    return minutes < 60 ? `${minutes} min ${Math.round(seconds % 60)} s` : `${Math.floor(minutes / 60)} h ${minutes % 60} min`
}

const TITLES: Record<string, string> = { build: "Building the global map", preview: "Reading the recording", save: "Saving into the recording" }

export function JobCard({ job, onCancel }: { job: Job; onCancel?: () => void }) {
    const progress = job.progress
    const percent = Math.round(job.fraction * 100)
    const state = job.state
    return (
        <div className={`job ${state}`} data-job-state={state}>
            <div className="stage-line">
                <strong>{TITLES[job.kind] ?? job.kind}</strong>
                <span>{state === "running" ? `${percent}%` : state}</span>
            </div>
            <div className="bar">
                <div style={{ width: `${state === "done" ? 100 : percent}%` }} />
            </div>
            {progress && state === "running" && (
                <div className="stage-line">
                    <span>
                        {progress.stageCount > 1 ? `${progress.stageIndex + 1}/${progress.stageCount} · ` : ""}
                        {progress.stage}
                    </span>
                    <span>{progress.total > 1 ? `${progress.done.toLocaleString()} / ${progress.total.toLocaleString()}` : ""}</span>
                </div>
            )}
            {progress?.note && state === "running" && <div className="hint">{progress.note}</div>}
            <div className="numbers">
                <span>elapsed {duration(job.elapsed)}</span>
                {state === "running" && <span data-eta>ETA {job.etaSeconds != null ? duration(job.etaSeconds) : "estimating…"}</span>}
                {job.error && <span className="problem">{job.error}</span>}
                <span className="spacer" />
                {state === "running" && onCancel && job.kind !== "save" && (
                    <button type="button" className="button danger" onClick={onCancel}>
                        Cancel
                    </button>
                )}
            </div>
        </div>
    )
}
