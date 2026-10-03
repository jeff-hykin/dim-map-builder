// The uploads drawer: Desktop's upload queue (this app's and any other's), with progress, speed, ETA, cancel, retry,
// readable errors (a login prompt when that's the problem), and the Dimensional account.
import type { Upload } from "../core/api.ts"
import { Icon } from "./Icon.tsx"
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
const BADGE: Record<Upload["state"], [string, string]> = {
    queued: ["", "queued"],
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
        <li className={`dim-panel upload ${upload.state}`} data-upload={upload.id} data-upload-state={upload.state}>
            <div className="upload-head">
                <span className="upload-name" title={upload.path}>
                    {upload.name}
                </span>
                <span className="dim-mono dim">{bytes(upload.size)}</span>
                <span className={`dim-badge ${badge}`}>{label}</span>
            </div>
            {upload.state === "uploading" && (
                <>
                    <div className={`bar ${measured ? "" : "indeterminate"}`}>
                        <div style={{ width: measured ? `${percent}%` : undefined }} />
                    </div>
                    <div className="dim-mono upload-numbers">
                        <span>{PHASES[upload.phase ?? ""] ?? upload.phase ?? "Starting"}{measured ? ` · ${percent}%` : "…"}</span>
                        {measured && (
                            <span>
                                {bytes(upload.bytesDone)} / {bytes(upload.bytesTotal)}
                            </span>
                        )}
                        {measured && upload.rateBps != null && <span>{bytes(upload.rateBps)}/s</span>}
                        {measured && <span data-eta>{timeLeft(upload.etaSeconds)}</span>}
                    </div>
                </>
            )}
            {upload.state === "queued" && <div className="hint">Waiting for the uploads ahead of it.</div>}
            {upload.state === "done" && (
                <div className="hint">
                    {upload.skipped ? "Already in the cloud: nothing to send." : "Uploaded."}
                    {upload.uploadId && <span className="dim-mono"> id {upload.uploadId.slice(0, 12)}</span>}
                </div>
            )}
            {upload.notice && <div className="dim-alert warn upload-notice">{upload.notice}</div>}
            {upload.error && upload.state !== "done" && (
                <div className="problem" data-upload-error={upload.errorCode ?? "failed"}>
                    {upload.error}
                    {upload.log && <div className="dim-mono dim upload-log">details: {upload.log}</div>}
                </div>
            )}
            <div className="row end">
                {upload.errorCode === "not_logged_in" && upload.state === "failed" && (
                    <button type="button" className="dim-btn sm primary" onClick={() => uploads.openLogin()} data-action="upload-login">
                        Log in
                    </button>
                )}
                {(upload.state === "failed" || upload.state === "cancelled") && (
                    <button type="button" className={`dim-btn sm ${upload.errorCode === "not_logged_in" ? "" : "primary"}`} onClick={() => uploads.retry(upload.id)} data-action="upload-retry">
                        <Icon name="refresh" /> Retry
                    </button>
                )}
                {isActive(upload) ? (
                    <button type="button" className="dim-btn sm danger" onClick={() => uploads.cancel(upload.id)} data-action="upload-cancel">
                        Cancel
                    </button>
                ) : (
                    <button type="button" className="dim-btn sm ghost" onClick={() => uploads.cancel(upload.id)} data-action="upload-remove" title="Remove from the list">
                        <Icon name="close" />
                    </button>
                )}
            </div>
        </li>
    )
}

export function UploadsPanel({ uploads }: { uploads: Uploads }) {
    const { list, account } = uploads
    const finished = list.filter((u) => !isActive(u)).length
    return (
        <div className="dim-panel glass uploads-panel" data-uploads-panel>
            <div className="tool-panel-head">
                <span>
                    <Icon name="upload" /> Uploads to Dimensional cloud
                </span>
                <button type="button" className="dim-btn sm icon" onClick={() => uploads.setPanelOpen(false)} title="Close">
                    <Icon name="close" />
                </button>
            </div>
            <div className="upload-account">
                {account?.loggedIn ? (
                    <>
                        <span className="dim">
                            Logged in as <strong>{account.email ?? "(unknown account)"}</strong>
                            {account.source === "env" ? " (DIMOS_API_KEY)" : ""}
                        </span>
                        {account.source !== "env" && (
                            <button type="button" className="dim-btn sm ghost" onClick={uploads.logout} data-action="cloud-logout">
                                Log out
                            </button>
                        )}
                    </>
                ) : account ? (
                    <>
                        <span className="dim">Not logged in</span>
                        <button type="button" className="dim-btn sm" onClick={() => uploads.openLogin()} data-action="cloud-login">
                            Log in
                        </button>
                    </>
                ) : (
                    <span className="dim">Checking your account…</span>
                )}
            </div>
            {account?.error && <div className="hint">{account.error}</div>}
            {uploads.problem && <div className="dim-alert danger">{uploads.problem}</div>}
            {uploads.waitingForLogin && (
                <div className="dim-alert warn" data-waiting-for-login>
                    Uploads are waiting for you to log in.
                    <div className="row">
                        <button type="button" className="dim-btn sm primary" onClick={() => uploads.openLogin()} data-action="waiting-login">
                            Log in
                        </button>
                    </div>
                </div>
            )}
            {!list.length && !uploads.problem && <div className="hint">Nothing uploaded yet. Upload a recording from the Save menu, or with the Upload button.</div>}
            <ul className="uploads">
                {list.map((upload) => (
                    <Item key={upload.id} upload={upload} uploads={uploads} />
                ))}
            </ul>
            {finished > 0 && (
                <div className="row end">
                    <button type="button" className="dim-btn sm ghost" onClick={uploads.clearFinished} data-action="uploads-clear">
                        Clear finished ({finished})
                    </button>
                </div>
            )}
        </div>
    )
}
