// Uploads to Dimensional cloud: Desktop's queue (../../dimos/uploads), polled every second while anything is queued or
// uploading or the panel is open (every 10 s otherwise, so uploads the agent starts show up too), plus the login flow.
import { useCallback, useEffect, useRef, useState } from "react"
import { cloud, uploads as uploadsApi, type CloudAccount, type Upload } from "../core/api.ts"

export interface Uploads {
    list: Upload[]
    waitingForLogin: boolean
    /** Desktop can't be asked (too old, or down) */
    problem: string | null
    account: CloudAccount | null
    panelOpen: boolean
    setPanelOpen: (open: boolean) => void
    loginOpen: boolean
    /** opens the login dialog; `then` = a path to queue once logged in */
    openLogin: (then?: string) => void
    closeLogin: () => void
    /** the login was approved: queue what was waiting and show the panel */
    loggedIn: () => Promise<void>
    /** checks the login, then queues `path` (or asks to log in first) */
    upload: (path: string) => Promise<void>
    cancel: (id: string) => Promise<void>
    retry: (id: string) => Promise<void>
    clearFinished: () => Promise<void>
    logout: () => Promise<void>
    refreshAccount: () => Promise<void>
}

export const isActive = (upload: Upload) => upload.state === "queued" || upload.state === "uploading"

export function useUploads(say: (text: string, error?: boolean) => void): Uploads {
    const [list, setList] = useState<Upload[]>([])
    const [waitingForLogin, setWaiting] = useState(false)
    const [problem, setProblem] = useState<string | null>(null)
    const [account, setAccount] = useState<CloudAccount | null>(null)
    const [panelOpen, setPanelOpen] = useState(false)
    const [loginOpen, setLoginOpen] = useState(false)
    const afterLogin = useRef<string | null>(null)

    const refresh = useCallback(async () => {
        try {
            const body = await uploadsApi.list()
            setList(body.uploads)
            setWaiting(body.waitingForLogin)
            setProblem(null)
        } catch (error) {
            setProblem((error as Error).message)
        }
    }, [])

    const active = list.some(isActive)
    useEffect(() => {
        refresh()
        const timer = window.setInterval(refresh, active || panelOpen ? 1000 : 10000)
        return () => window.clearInterval(timer)
    }, [refresh, active, panelOpen])

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
        async (path: string) => {
            try {
                const added = await uploadsApi.add(path)
                say(`Queued ${added.name} for upload`)
                setPanelOpen(true)
                await refresh()
            } catch (error) {
                say(`Couldn't queue the upload: ${(error as Error).message}`, true)
            }
        },
        [say, refresh],
    )

    const upload = useCallback(
        async (path: string) => {
            let current: CloudAccount
            try {
                current = await cloud.account()
            } catch (error) {
                say((error as Error).message, true)
                return
            }
            setAccount(current)
            if (!current.loggedIn) {
                afterLogin.current = path
                setLoginOpen(true)
                return
            }
            await enqueue(path)
        },
        [say, enqueue],
    )

    const loggedIn = useCallback(async () => {
        setLoginOpen(false)
        await refreshAccount()
        const path = afterLogin.current
        afterLogin.current = null
        if (path) {
            await enqueue(path)
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
        openLogin: (then?: string) => {
            afterLogin.current = then ?? null
            setLoginOpen(true)
        },
        closeLogin: () => {
            afterLogin.current = null
            setLoginOpen(false)
        },
        loggedIn,
        upload,
        cancel: (id) => act(uploadsApi.remove(id)),
        retry: (id) => act(uploadsApi.retry(id)),
        clearFinished: () => act(uploadsApi.clearFinished()),
        logout: async () => {
            await act(cloud.logout().then(setAccount))
        },
        refreshAccount,
    }
}
