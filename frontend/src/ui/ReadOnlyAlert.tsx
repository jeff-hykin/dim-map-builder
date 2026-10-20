// A read-only recording: why the steps and Save can't write into it, and the command that fixes it (the server's writeFix).
import { useState } from "react"
import { Alert, Button, Row } from "dimos-react"
import type { Session } from "../core/api.ts"

export function ReadOnlyAlert({ session, doing }: { session: Session; doing: string }) {
    const [copied, setCopied] = useState(false)
    const copy = () => {
        navigator.clipboard?.writeText(session.writeFix ?? "").then(() => setCopied(true)).catch(() => setCopied(false))
    }
    return (
        <Alert tone="warn" title="This recording is read-only." data-read-only>
            {doing} writes into the recording itself, and this user can't write to {session.recordingPath}. Run this in a terminal, then reopen the recording:
            {session.writeFix && (
                <Row gap={8} align="center" style={{ marginTop: 8 }}>
                    <code data-write-fix style={{ flex: 1, padding: "6px 8px", borderRadius: 4, background: "var(--surface-2, rgba(127,127,127,.12))", fontFamily: "ui-monospace, monospace", fontSize: 12, overflowWrap: "anywhere", userSelect: "all" }}>{session.writeFix}</code>
                    <Button size="sm" onClick={copy} data-action="copy-write-fix">{copied ? "Copied" : "Copy"}</Button>
                </Row>
            )}
        </Alert>
    )
}
