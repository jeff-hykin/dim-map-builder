// Log in to Dimensional cloud: Desktop starts dimos's device login, we show the page to open and the code to enter
// there (from any signed-in browser: this laptop, a phone), and wait until it's approved: the dimos server says so on
// its zenoh event <ns>/dimos/events/cloud-login (the page's one zenoh-gateway connection; re-read after a reconnect).
import { useCallback, useEffect, useRef, useState } from "react"
import { cloud, type LoginState } from "../core/api.ts"
import { getZenoh } from "../dim-app/source/zenoh.js"
import { Icon } from "./Icon.tsx"

function Copy({ text, action, label }: { text: string; action: string; label: string }) {
    const [copied, setCopied] = useState(false)
    return (
        <button
            type="button"
            className="dim-btn sm icon"
            data-action={action}
            title={label}
            onClick={() => {
                navigator.clipboard
                    ?.writeText(text)
                    .then(() => {
                        setCopied(true)
                        window.setTimeout(() => setCopied(false), 1500)
                    })
                    .catch(() => {})
            }}
        >
            <Icon name={copied ? "check" : "copy"} /> {copied ? "Copied" : "Copy"}
        </button>
    )
}

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
        <div className="modal-backdrop" onMouseDown={(event) => event.target === event.currentTarget && cancel()}>
            <div className="dim-panel modal login" data-modal="login" data-login-state={state ?? "starting"}>
                <div className="modal-head">
                    <h2 className="dim-h2">Log in to Dimensional cloud</h2>
                    <button type="button" className="dim-btn sm icon" onClick={cancel} title="Cancel">
                        <Icon name="close" />
                    </button>
                </div>
                <p className="hint">{reason ?? "Uploads go to your Dimensional account."} Approve this machine from any browser where you're signed in: this one, or your phone.</p>
                {problem && (
                    <div className="dim-alert danger">
                        {problem}
                        <div className="row">
                            <button type="button" className="dim-btn sm" onClick={start} data-action="login-retry">
                                Try again
                            </button>
                        </div>
                    </div>
                )}
                {!problem && (!login || state === "starting") && <div className="login-wait"><span className="spinner" /> Asking Dimensional cloud for a code…</div>}
                {!problem && login && state === "pending" && (
                    <>
                        <div className="login-step">
                            <span className="dim-label">1 · Open</span>
                            <div className="row">
                                <a className="dim-mono login-url" href={open ?? "#"} target="_blank" rel="noreferrer" data-action="login-open">
                                    {login.url} <Icon name="link" />
                                </a>
                                {login.url && <Copy text={open ?? login.url} action="login-copy-url" label="Copy the link" />}
                            </div>
                        </div>
                        <div className="login-step">
                            <span className="dim-label">2 · Enter the code</span>
                            <div className="row">
                                <span className="login-code dim-mono" data-login-code>
                                    {login.code}
                                </span>
                                {login.code && <Copy text={login.code} action="login-copy-code" label="Copy the code" />}
                            </div>
                        </div>
                        <div className="login-wait">
                            <span className="spinner" /> Waiting for you to approve…
                            {login.expiresAt && <span className="dim-mono dim"> code expires in {left(login.expiresAt, now)}</span>}
                        </div>
                    </>
                )}
                {!problem && state === "approved" && (
                    <div className="dim-alert info" data-login-approved>
                        <Icon name="check" /> Logged in{login?.email ? ` as ${login.email}` : ""}.
                    </div>
                )}
                {!problem && (state === "denied" || state === "expired" || state === "failed" || state === "idle") && (
                    <div className="dim-alert danger">
                        {state === "denied" ? "The login was denied." : state === "expired" ? "The code expired before it was approved." : state === "idle" ? "The login was cancelled." : `The login failed: ${login?.error ?? "unknown error"}`}
                        <div className="row">
                            <button type="button" className="dim-btn sm primary" onClick={start} data-action="login-retry">
                                Try again
                            </button>
                        </div>
                    </div>
                )}
                <div className="row end">
                    <button type="button" className="dim-btn sm" onClick={cancel} data-action="login-cancel">
                        Cancel
                    </button>
                </div>
            </div>
        </div>
    )
}
