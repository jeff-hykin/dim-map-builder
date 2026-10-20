// A read-only recording: why the steps and Save can't write into it, and the command that fixes it (the server's writeFix).
import { useEffect, useState } from "react"
import { Alert, Button, Row } from "dimos-react"
import type { Session } from "../core/api.ts"

export function ReadOnlyAlert({ session, doing, refresh }: { session: Session; doing: string; refresh: () => Promise<void> }) {
    const [copied, setCopied] = useState(false)
    // the server asks the file each time: re-read the session every 2 s so the fix shows up without reopening
    useEffect(() => {
        const timer = setInterval(() => refresh().catch(() => {}), 2000)
        return () => clearInterval(timer)
    }, [refresh])
    const copy = () => {
        navigator.clipboard?.writeText(session.writeFix ?? "").then(() => setCopied(true)).catch(() => setCopied(false))
    }
    return (
        <Alert tone="warn" title="This recording is read-only." data-read-only>
            {doing} writes into the recording itself, and this user can't write to {session.recordingPath}. Run this in a terminal (this updates by itself once it's done):
            {session.writeFix && (
                <Row gap={8} align="center" style={{ marginTop: 8 }}>
                    <code data-write-fix style={{ flex: 1, padding: "6px 8px", borderRadius: 4, background: "var(--surface-2, rgba(127,127,127,.12))", fontFamily: "ui-monospace, monospace", fontSize: 12, overflowWrap: "anywhere", userSelect: "all" }}>{session.writeFix}</code>
                    <Button size="sm" onClick={copy} data-action="copy-write-fix">{copied ? "Copied" : "Copy"}</Button>
                </Row>
            )}
        </Alert>
    )
}
