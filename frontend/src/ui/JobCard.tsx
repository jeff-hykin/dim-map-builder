// A background job's progress: stage, a bar over the whole job, done/total, elapsed, ETA, and Cancel.
import type { CSSProperties } from "react"
import { Button, Card, Progress, Row, Spacer, Text } from "dimos-react"
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

const TITLES: Record<string, string> = {
    build: "Building the global map",
    preview: "Reading the recording",
    save: "Saving into the recording",
    tags: "Finding fiducial markers",
    tf: "Correcting the tf tree",
    globalMap: "Generating the global map",
}

export function JobCard({ job, onCancel, style }: { job: Job; onCancel?: () => void; style?: CSSProperties }) {
    const progress = job.progress
    const percent = Math.round(job.fraction * 100)
    const state = job.state
    return (
        <Card padded data-job-state={state} style={{ gap: 6, ...style }}>
            <Row justify="space-between">
                <Text as="strong">{TITLES[job.kind] ?? job.kind}</Text>
                <Text small mono>{state === "running" ? `${percent}%` : state}</Text>
            </Row>
            <Progress value={state === "done" ? 1 : job.fraction} tone={state === "failed" ? "danger" : "primary"} />
            {progress && state === "running" && (
                <Row justify="space-between">
                    <Text small>
                        {progress.stageCount > 1 ? `${progress.stageIndex + 1}/${progress.stageCount} · ` : ""}
                        {progress.stage}
                    </Text>
                    <Text small mono>{progress.total > 1 ? `${progress.done.toLocaleString()} / ${progress.total.toLocaleString()}` : ""}</Text>
                </Row>
            )}
            {progress?.note && state === "running" && <Text small muted>{progress.note}</Text>}
            <Row wrap gap={12}>
                <Text small mono muted>elapsed {duration(job.elapsed)}</Text>
                {state === "running" && <Text small mono muted data-eta>ETA {job.etaSeconds != null ? duration(job.etaSeconds) : "estimating…"}</Text>}
                {job.error && <Text small tone="danger">{job.error}</Text>}
                <Spacer />
                {state === "running" && onCancel && job.kind !== "save" && (
                    <Button size="sm" variant="danger" onClick={onCancel}>
                        Cancel
                    </Button>
                )}
            </Row>
        </Card>
    )
}
