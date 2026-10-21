// Uploads to Dimensional cloud: Desktop's queue (../../dimos-gateway/cloud/uploads), read once and again whenever the dimos server says
// it changed (its zenoh events <ns>/dimos/events/upload, uploads, upload-removed, on the page's one zenoh-gateway
// connection; also after that connection comes back), so uploads the agent starts show up too. Logging in isn't this
// app's: it is Desktop's Settings → Dimensional account (one login for every app), which comes back here once done.
import { useCallback, useEffect, useRef, useState } from "react"
import { cloud, uploads as uploadsApi, type CloudAccount, type Upload } from "../core/api.ts"
// @deno-types="../dim-app/source/desktop.d.ts"
import { openApp } from "../dim-app/source/desktop.js"
// @deno-types="../dim-app/source/zenoh.d.ts"
import { getZenoh } from "../dim-app/source/zenoh.js"

export interface Uploads {
    list: Upload[]
    waitingForLogin: boolean
    /** Desktop can't be asked (too old, or down) */
    problem: string | null
    account: CloudAccount | null
    panelOpen: boolean
    setPanelOpen: (open: boolean) => void
    /** opens Desktop's Settings → Dimensional account; `then` = what to queue once logged in */
    openLogin: (then?: Queue) => void
    /** checks the login, then queues the upload (or asks to log in first) */
    upload: (queue: Queue) => Promise<void>
    refresh: () => Promise<void>
    cancel: (id: string) => Promise<void>
    retry: (id: string) => Promise<void>
    clearFinished: () => Promise<void>
    refreshAccount: () => Promise<void>
}

/** queues an upload through the backend (POST api/sessions/{id}/upload); says what it did */
export type Queue = () => Promise<string>

/** Desktop's Settings → Dimensional account: every app's one cloud login; signed in, the shell comes back here. */
export function openCloudLogin() {
    openApp("settings", { path: "settings-cloud-account" })
}

export const isActive = (upload: Upload) => upload.state === "queued" || upload.state === "uploading"

export function useUploads(say: (text: string, error?: boolean) => void): Uploads {
    const [list, setList] = useState<Upload[]>([])
    const [waitingForLogin, setWaiting] = useState(false)
    const [problem, setProblem] = useState<string | null>(null)
    const [account, setAccount] = useState<CloudAccount | null>(null)
    const [panelOpen, setPanelOpen] = useState(false)
    const afterLogin = useRef<Queue | null>(null)

    const refresh = useCallback(async () => {
        try {
            const body = await uploadsApi.list()
            setList(body.uploads ?? []) // outside Desktop (no /dimos-gateway/cloud/uploads) the answer has no list
            setWaiting(Boolean(body.waitingForLogin))
            setProblem(null)
        } catch (error) {
            setProblem((error as Error).message)
        }
    }, [])

    useEffect(() => {
        refresh()
        const zenoh = getZenoh()
        // an `upload` event carries the whole upload (progress included): apply it; the others say "re-read"
        let timer: number | undefined
        const reread = () => {
            window.clearTimeout(timer)
            timer = window.setTimeout(refresh, 100)
        }
        const offs = [
            zenoh.subscribeDimos<{ upload?: Upload }>("upload", (event) => {
                const upload = event.upload
                if (!upload?.id) {
                    return reread()
                }
                setList((list) => (list.some((u) => u.id === upload.id) ? list.map((u) => (u.id === upload.id ? upload : u)) : [...list, upload]))
            }),
            zenoh.subscribeDimos("uploads", reread),
            zenoh.subscribeDimos("upload-removed", reread),
            zenoh.onReconnect(reread),
        ]
        return () => {
            window.clearTimeout(timer)
            offs.forEach((off) => off())
        }
    }, [refresh])

    const refreshAccount = useCallback(async () => {
        try {
            setAccount(await cloud.account())
        } catch (error) {
            setProblem((error as Error).message)
        }
    }, [])
    useEffect(() => {
        if (panelOpen) {
            refreshAccount()
        }
    }, [panelOpen, refreshAccount])

    const enqueue = useCallback(
        async (queue: Queue) => {
            try {
                say(await queue())
                setPanelOpen(true)
                await refresh()
            } catch (error) {
                say(`Couldn't queue the upload: ${(error as Error).message}`, true)
            }
        },
        [say, refresh],
    )

    const upload = useCallback(
        async (queue: Queue) => {
            let current: CloudAccount
            try {
                current = await cloud.account()
            } catch (error) {
                say((error as Error).message, true)
                return
            }
            setAccount(current)
            if (!current.loggedIn) {
                afterLogin.current = queue
                setWaitingToQueue(true)
                openCloudLogin()
                return
            }
            await enqueue(queue)
        },
        [say, enqueue],
    )

    // an upload held for the login: queue it once Desktop says it's done (the dimos server's cloud-login event), or a
    // look every 3 s finds it logged in
    const [waitingToQueue, setWaitingToQueue] = useState(false)
    useEffect(() => {
        if (!waitingToQueue) {
            return
        }
        const check = async () => {
            const current = await cloud.account().catch(() => null)
            const queue = afterLogin.current
            if (current?.loggedIn && queue) {
                afterLogin.current = null
                setAccount(current)
                setWaitingToQueue(false)
                await enqueue(queue)
            }
        }
        const off = getZenoh().subscribeDimos("cloud-login", check)
        const timer = window.setInterval(check, 3000)
        return () => {
            off()
            window.clearInterval(timer)
        }
    }, [waitingToQueue, enqueue])

    const act = useCallback(
        async (action: Promise<unknown>) => {
            try {
                await action
            } catch (error) {
                say((error as Error).message, true)
            }
            await refresh()
        },
        [say, refresh],
    )

    return {
        list,
        waitingForLogin,
        problem,
        account,
        panelOpen,
        setPanelOpen,
        openLogin: (then?: Queue) => {
            afterLogin.current = then ?? null
            setWaitingToQueue(!!then)
            openCloudLogin()
        },
        upload,
        refresh,
        cancel: (id) => act(uploadsApi.remove(id)),
        retry: (id) => act(uploadsApi.retry(id)),
        clearFinished: () => act(uploadsApi.clearFinished()),
        refreshAccount,
    }
}
