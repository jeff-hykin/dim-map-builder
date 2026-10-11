// The uploads drawer: Desktop's upload queue (this app's and any other's), with progress, speed, ETA, cancel, retry,
// readable errors (a login prompt when that's the problem), and the Dimensional account.
import { Alert, Badge, Button, Card, Column, FloatingPanel, IconButton, Progress, Row, Spacer, Text } from "dimos-react"
import type { Upload } from "../core/api.ts"
import { isActive, type Uploads } from "./useUploads.ts"

export function bytes(n: number): string {
    if (!Number.isFinite(n) || n <= 0) {
        return "0 B"
    }
    const units = ["B", "KB", "MB", "GB", "TB"]
    const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)))
    return `${(n / 1024 ** i).toFixed(i === 0 ? 0 : n / 1024 ** i < 10 ? 1 : 0)} ${units[i]}`
}

export function timeLeft(seconds: number | null): string {
    if (seconds == null || !Number.isFinite(seconds)) {
        return "estimating…"
    }
    if (seconds < 10) {
        return "a few seconds left"
    }
    if (seconds < 60) {
        return `about ${Math.round(seconds / 5) * 5} s left`
    }
    const minutes = Math.round(seconds / 60)
    return minutes < 60 ? `about ${minutes} min left` : `about ${Math.floor(minutes / 60)} h ${minutes % 60} min left`
}

const PHASES: Record<string, string> = { preparing: "Preparing", compress: "Compressing", upload: "Uploading", finishing: "Finishing" }
const BADGE: Record<Upload["state"], ["default" | "info" | "ok" | "danger" | "warn", string]> = {
    queued: ["default", "queued"],
    uploading: ["info", "uploading"],
    done: ["ok", "done"],
    failed: ["danger", "failed"],
    cancelled: ["warn", "cancelled"],
}

/** the overall fraction of the active uploads, for the topbar button's ring */
export function overallFraction(list: Upload[]): number | null {
    const running = list.find((u) => u.state === "uploading")
    if (!running) {
        return null
    }
    return running.bytesTotal > 0 && running.phase === "upload" ? running.bytesDone / running.bytesTotal : null
}

function Item({ upload, uploads }: { upload: Upload; uploads: Uploads }) {
    const [badge, label] = BADGE[upload.state]
    const measured = upload.state === "uploading" && upload.phase === "upload" && upload.bytesTotal > 0
    const percent = measured ? Math.floor((upload.bytesDone / upload.bytesTotal) * 100) : upload.state === "done" ? 100 : 0
    return (
        <Card padded data-upload={upload.id} data-upload-state={upload.state} style={{ gap: 6, opacity: upload.state === "done" ? 0.85 : 1 }}>
            <Row>
                <Text as="strong" title={upload.path} style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {upload.name}
                </Text>
                <Text small mono muted>{bytes(upload.size)}</Text>
                <Badge tone={badge}>{label}</Badge>
            </Row>
            {upload.state === "uploading" && (
                <>
                    <Progress value={measured ? upload.bytesDone / upload.bytesTotal : undefined} />
                    <Row wrap gap={10}>
                        <Text small mono muted>{PHASES[upload.phase ?? ""] ?? upload.phase ?? "Starting"}{measured ? ` · ${percent}%` : "…"}</Text>
                        {measured && <Text small mono muted>{bytes(upload.bytesDone)} / {bytes(upload.bytesTotal)}</Text>}
                        {measured && upload.rateBps != null && <Text small mono muted>{bytes(upload.rateBps)}/s</Text>}
                        <Spacer />
                        {measured && <Text small mono muted data-eta>{timeLeft(upload.etaSeconds)}</Text>}
                    </Row>
                </>
            )}
            {upload.state === "queued" && upload.errorCode !== "not_logged_in" && <Text small muted>Waiting for the uploads ahead of it.</Text>}
            {upload.state === "done" && (
                <Text small muted>
                    {upload.skipped ? "Already in the cloud: nothing to send." : "Uploaded."}
                    {upload.uploadId && <Text mono> id {upload.uploadId.slice(0, 12)}</Text>}
                </Text>
            )}
            {upload.notice && <Alert tone="warn">{upload.notice}</Alert>}
            {upload.error && upload.state !== "done" && (
                <Text small tone="danger" data-upload-error={upload.errorCode ?? "failed"}>
                    {upload.error}
                    {upload.log && <Text as="div" small mono muted style={{ wordBreak: "break-all" }}>details: {upload.log}</Text>}
                </Text>
            )}
            <Row justify="flex-end">
                {upload.errorCode === "not_logged_in" && upload.state === "failed" && (
                    <Button size="sm" variant="primary" onClick={() => uploads.openLogin()} data-action="upload-login">
                        Log in
                    </Button>
                )}
                {(upload.state === "failed" || upload.state === "cancelled") && (
                    <Button size="sm" icon="refresh" variant={upload.errorCode === "not_logged_in" ? "default" : "primary"} onClick={() => uploads.retry(upload.id)} data-action="upload-retry">
                        Retry
                    </Button>
                )}
                {isActive(upload)
                    ? (
                        <Button size="sm" variant="danger" onClick={() => uploads.cancel(upload.id)} data-action="upload-cancel">
                            Cancel
                        </Button>
                    )
                    : <IconButton icon="close" size="sm" onClick={() => uploads.cancel(upload.id)} data-action="upload-remove" label="Remove from the list" />}
            </Row>
        </Card>
    )
}

export function UploadsPanel({ uploads }: { uploads: Uploads }) {
    const { list, account } = uploads
    const finished = list.filter((u) => !isActive(u)).length
    return (
        <FloatingPanel
            title="Uploads to Dimensional cloud"
            icon="upload"
            width="min(420px, calc(100vw - 24px))"
            defaultPosition={{ x: -12, y: 60 }}
            onClose={() => uploads.setPanelOpen(false)}
            data-uploads-panel
        >
            <Row>
                {account?.loggedIn
                    ? (
                        <>
                            <Text small muted style={{ flex: 1 }}>
                                Logged in as <strong>{account.email ?? "(unknown account)"}</strong>
                                {account.source === "env" ? " (DIMOS_API_KEY)" : ""}
                            </Text>
                            {account.source !== "env" && (
                                <Button size="sm" variant="ghost" onClick={uploads.logout} data-action="cloud-logout">
                                    Log out
                                </Button>
                            )}
                        </>
                    )
                    : account
                    ? (
                        <>
                            <Text small muted style={{ flex: 1 }}>Not logged in</Text>
                            <Button size="sm" onClick={() => uploads.openLogin()} data-action="cloud-login">
                                Log in
                            </Button>
                        </>
                    )
                    : <Text small muted>Checking your account…</Text>}
            </Row>
            {account?.error && <Text small muted>{account.error}</Text>}
            {uploads.problem && <Alert tone="danger">{uploads.problem}</Alert>}
            {uploads.waitingForLogin && (
                <Alert
                    tone="warn"
                    data-waiting-for-login
                    actions={
                        <Button size="sm" variant="primary" onClick={() => uploads.openLogin()} data-action="waiting-login">
                            Log in
                        </Button>
                    }
                >
                    Uploads are waiting for you to log in.
                </Alert>
            )}
            {!list.length && !uploads.problem && <Text small muted>Nothing uploaded yet. Upload a recording from the Save menu, or with the Upload button.</Text>}
            <Column gap={6}>
                {list.map((upload) => (
                    <Item key={upload.id} upload={upload} uploads={uploads} />
                ))}
            </Column>
            {finished > 0 && (
                <Row justify="flex-end">
                    <Button size="sm" variant="ghost" onClick={uploads.clearFinished} data-action="uploads-clear">
                        Clear finished ({finished})
                    </Button>
                </Row>
            )}
        </FloatingPanel>
    )
}
