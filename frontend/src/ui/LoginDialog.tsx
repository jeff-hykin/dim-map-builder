// Log in to Dimensional cloud: Desktop starts dimos's device login, we show the page to open and the code to enter
// there (from any signed-in browser: this laptop, a phone), and wait until it's approved: the dimos server says so on
// its zenoh event <ns>/dimos/events/cloud-login (the page's one zenoh-gateway connection; re-read after a reconnect).
import { useCallback, useEffect, useRef, useState } from "react"
import { cloud, type LoginState } from "../core/api.ts"
import { getZenoh } from "../dim-app/source/zenoh.js"
import { Alert, Button, Column, CopyButton, Icon, Label, Link, Modal, Row, Spinner, Text } from "dimos-react"

const left = (expiresAt: number | null, now: number) => {
    if (!expiresAt) {
        return ""
    }
    const seconds = Math.max(0, Math.round((expiresAt - now) / 1000))
    return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`
}

export function LoginDialog({ reason, onApproved, onClose }: { reason: string | null; onApproved: () => void; onClose: () => void }) {
    const [login, setLogin] = useState<LoginState | null>(null)
    const [problem, setProblem] = useState<string | null>(null)
    const [now, setNow] = useState(Date.now())
    const done = useRef(false)

    const start = useCallback(async () => {
        setProblem(null)
        setLogin(null)
        try {
            setLogin(await cloud.login())
        } catch (error) {
            setProblem((error as Error).message)
        }
    }, [])

    // reuse a login that's already waiting (another window started it), else start one
    useEffect(() => {
        cloud
            .loginState()
            .then((state) => (state.state === "pending" && (!state.expiresAt || state.expiresAt > Date.now()) ? setLogin(state) : start()))
            .catch(() => start())
    }, [start])

    const waiting = !login || login.state === "starting" || login.state === "pending"
    useEffect(() => {
        if (!waiting || problem) {
            return
        }
        const reread = () =>
            cloud
                .loginState()
                .then(setLogin)
                .catch((error) => setProblem((error as Error).message))
        const zenoh = getZenoh()
        const offs = [
            zenoh.subscribeDimos<{ login?: LoginState }>("cloud-login", (event) => (event.login ? setLogin(event.login) : reread())),
            zenoh.onReconnect(reread),
        ]
        reread() // anything that happened before the subscription was up
        // the countdown to the code's expiry (a clock, not a poll)
        const clock = window.setInterval(() => setNow(Date.now()), 1000)
        return () => {
            offs.forEach((off) => off())
            window.clearInterval(clock)
        }
    }, [waiting, problem])

    useEffect(() => {
        if (login?.state === "approved" && !done.current) {
            done.current = true
            window.setTimeout(onApproved, 900)
        }
    }, [login, onApproved])

    const cancel = () => {
        if (login?.state === "pending" || login?.state === "starting") {
            cloud.cancelLogin().catch(() => {})
        }
        onClose()
    }

    const state = login?.state
    const open = login?.urlComplete ?? login?.url ?? null
    return (
        <Modal
            open
            onClose={cancel}
            title="Log in to Dimensional cloud"
            data-modal="login"
            data-login-state={state ?? "starting"}
            footer={
                <Button size="sm" onClick={cancel} data-action="login-cancel">
                    Cancel
                </Button>
            }
        >
            <Column gap={12}>
                <Text small muted>{reason ?? "Uploads go to your Dimensional account."} Approve this machine from any browser where you're signed in: this one, or your phone.</Text>
                {problem && (
                    <Alert
                        tone="danger"
                        actions={
                            <Button size="sm" onClick={start} data-action="login-retry">
                                Try again
                            </Button>
                        }
                    >
                        {problem}
                    </Alert>
                )}
                {!problem && (!login || state === "starting") && (
                    <Row>
                        <Spinner size={14} /> <Text small muted>Asking Dimensional cloud for a code…</Text>
                    </Row>
                )}
                {!problem && login && state === "pending" && (
                    <>
                        <Column gap={4}>
                            <Label>1 · Open</Label>
                            <Row wrap>
                                <Link className="dr-mono" href={open ?? "#"} target="_blank" rel="noreferrer" data-action="login-open" style={{ wordBreak: "break-all" }}>
                                    {login.url} <Icon name="external" size={13} />
                                </Link>
                                {login.url && <CopyButton text={open ?? login.url} label="Copy the link" data-action="login-copy-url" />}
                            </Row>
                        </Column>
                        <Column gap={4}>
                            <Label>2 · Enter the code</Label>
                            <Row>
                                <Text mono data-login-code style={{ fontSize: 28, fontWeight: 700, letterSpacing: "0.18em", userSelect: "all" }}>
                                    {login.code}
                                </Text>
                                {login.code && <CopyButton text={login.code} label="Copy the code" data-action="login-copy-code" />}
                            </Row>
                        </Column>
                        <Row wrap>
                            <Spinner size={14} /> <Text small muted>Waiting for you to approve…</Text>
                            {login.expiresAt && <Text small mono muted>code expires in {left(login.expiresAt, now)}</Text>}
                        </Row>
                    </>
                )}
                {!problem && state === "approved" && (
                    <Alert tone="ok" data-login-approved>
                        Logged in{login?.email ? ` as ${login.email}` : ""}.
                    </Alert>
                )}
                {!problem && (state === "denied" || state === "expired" || state === "failed" || state === "idle") && (
                    <Alert
                        tone="danger"
                        actions={
                            <Button size="sm" variant="primary" onClick={start} data-action="login-retry">
                                Try again
                            </Button>
                        }
                    >
                        {state === "denied" ? "The login was denied." : state === "expired" ? "The code expired before it was approved." : state === "idle" ? "The login was cancelled." : `The login failed: ${login?.error ?? "unknown error"}`}
                    </Alert>
                )}
            </Column>
        </Modal>
    )
}
