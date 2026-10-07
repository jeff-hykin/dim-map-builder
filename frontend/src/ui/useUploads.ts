// Uploads to Dimensional cloud: Desktop's queue (../../dimos/uploads), read once and again whenever the dimos server says
// it changed (its zenoh events <ns>/dimos/events/upload, uploads, upload-removed, on the page's one zenoh-gateway
// connection; also after that connection comes back), so uploads the agent starts show up too; plus the login flow.
import { useCallback, useEffect, useRef, useState } from "react"
import { cloud, uploads as uploadsApi, type CloudAccount, type Upload } from "../core/api.ts"
import { getZenoh } from "../dim-app/source/zenoh.js"

export interface Uploads {
    list: Upload[]
    waitingForLogin: boolean
    /** Desktop can't be asked (too old, or down) */
    problem: string | null
    account: CloudAccount | null
    panelOpen: boolean
    setPanelOpen: (open: boolean) => void
    loginOpen: boolean
    /** opens the login dialog; `then` = what to queue once logged in */
    openLogin: (then?: Queue) => void
    closeLogin: () => void
    /** the login was approved: queue what was waiting and show the panel */
    loggedIn: () => Promise<void>
    /** checks the login, then queues the upload (or asks to log in first) */
    upload: (queue: Queue) => Promise<void>
    refresh: () => Promise<void>
    cancel: (id: string) => Promise<void>
    retry: (id: string) => Promise<void>
    clearFinished: () => Promise<void>
    logout: () => Promise<void>
    refreshAccount: () => Promise<void>
}

/** queues an upload through the backend (POST api/sessions/{id}/upload); says what it did */
export type Queue = () => Promise<string>

export const isActive = (upload: Upload) => upload.state === "queued" || upload.state === "uploading"

export function useUploads(say: (text: string, error?: boolean) => void): Uploads {
    const [list, setList] = useState<Upload[]>([])
    const [waitingForLogin, setWaiting] = useState(false)
    const [problem, setProblem] = useState<string | null>(null)
    const [account, setAccount] = useState<CloudAccount | null>(null)
    const [panelOpen, setPanelOpen] = useState(false)
    const [loginOpen, setLoginOpen] = useState(false)
    const afterLogin = useRef<Queue | null>(null)

    const refresh = useCallback(async () => {
        try {
            const body = await uploadsApi.list()
            setList(body.uploads ?? []) // outside Desktop (no /dimos/uploads) the answer has no list
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
                setLoginOpen(true)
                return
            }
            await enqueue(queue)
        },
        [say, enqueue],
    )

    const loggedIn = useCallback(async () => {
        setLoginOpen(false)
        await refreshAccount()
        const queue = afterLogin.current
        afterLogin.current = null
        if (queue) {
            await enqueue(queue)
        } else {
            setPanelOpen(true)
            await refresh()
        }
    }, [refreshAccount, enqueue, refresh])

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
        loginOpen,
        openLogin: (then?: Queue) => {
            afterLogin.current = then ?? null
            setLoginOpen(true)
        },
        closeLogin: () => {
            afterLogin.current = null
            setLoginOpen(false)
        },
        loggedIn,
        upload,
        refresh,
        cancel: (id) => act(uploadsApi.remove(id)),
        retry: (id) => act(uploadsApi.retry(id)),
        clearFinished: () => act(uploadsApi.clearFinished()),
        logout: async () => {
            await act(cloud.logout().then(setAccount))
        },
        refreshAccount,
    }
}
