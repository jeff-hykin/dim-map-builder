// DEV ONLY: a tiny in-memory stand-in for Desktop's cloud login + upload queue (/dimos/cloud/*, /dimos/uploads*) and
// error feed (/api/errors), so the upload UI can be worked on without Desktop or a Dimensional account. On with
// `MOCK_UPLOADS=1 npm run dev`; never part of the build. Login: approved 6 s after it starts. Uploads: one at a time,
// 4 MB/s; a path containing "fail" fails with a network error. The real endpoints: dimos-desktop docs/api.md.
import type { Plugin } from "vite"

type Upload = Record<string, any>

export function mockUploads(): Plugin {
    let loggedIn = false
    let login: Record<string, any> = { state: "idle", url: null, urlComplete: null, code: null, expiresAt: null, email: null, error: null }
    let loginStarted = 0
    let waitingForLogin = false
    let list: Upload[] = []
    let next = 1
    const account = () => ({ loggedIn, email: loggedIn ? "dev@example.com" : null, scopes: loggedIn ? ["data"] : null, source: loggedIn ? "stored" : null, cloudUrl: "https://mock.invalid", error: null })
    const tick = () => {
        if (login.state === "pending" && Date.now() - loginStarted > 6000) {
            login = { ...login, state: "approved", email: "dev@example.com" }
            loggedIn = true
            waitingForLogin = false
        }
        const head = list.find((u) => u.state === "uploading") ?? list.find((u) => u.state === "queued")
        if (!head) {
            return
        }
        if (!loggedIn) {
            waitingForLogin = true
            return
        }
        const now = Date.now()
        if (head.state === "queued") {
            Object.assign(head, { state: "uploading", phase: "preparing", startedAt: now, bytesDone: 0, bytesTotal: head.size })
        } else if (head.phase === "preparing" && now - head.startedAt > 1500) {
            head.phase = "upload"
        } else if (head.phase === "upload") {
            const rate = 4 * 2 ** 20
            head.bytesDone = Math.min(head.bytesTotal, head.bytesDone + rate / 4)
            head.rateBps = rate
            head.etaSeconds = (head.bytesTotal - head.bytesDone) / rate
            if (head.path.includes("fail") && head.bytesDone > head.bytesTotal / 3) {
                Object.assign(head, { state: "failed", phase: null, error: "Couldn't reach Dimensional cloud (network error). Check the connection and retry.", errorCode: "network", log: "/tmp/mock-uploads.log", finishedAt: now, rateBps: null, etaSeconds: null })
            } else if (head.bytesDone >= head.bytesTotal) {
                Object.assign(head, { state: "done", phase: null, uploadId: `mock${head.id}abcdef0123`, finishedAt: now, rateBps: null, etaSeconds: null })
            }
        }
    }
    return {
        name: "mock-uploads",
        apply: "serve",
        configureServer(server) {
            setInterval(tick, 250)
            server.middlewares.use(async (req, res, nextMiddleware) => {
                const url = new URL(req.url ?? "/", "http://x")
                const path = url.pathname
                if (!path.startsWith("/dimos/") && path !== "/api/errors") {
                    return nextMiddleware()
                }
                let body: any = {}
                if (req.method === "POST") {
                    const chunks: Buffer[] = []
                    for await (const chunk of req) {
                        chunks.push(chunk as Buffer)
                    }
                    body = chunks.length ? JSON.parse(Buffer.concat(chunks).toString() || "{}") : {}
                }
                const send = (status: number, value: unknown) => {
                    res.statusCode = status
                    res.setHeader("content-type", "application/json")
                    res.end(JSON.stringify(value))
                }
                const route = `${req.method} ${path}`
                const item = list.find((u) => path.startsWith(`/dimos/uploads/${u.id}`))
                if (route === "POST /api/errors") {
                    console.log("[mock] error reported:", body)
                    return send(200, { id: 1, count: 1 })
                } else if (route === "GET /dimos/cloud/account") {
                    return send(200, account())
                } else if (route === "POST /dimos/cloud/login") {
                    loginStarted = Date.now()
                    login = { state: "pending", url: "https://mock.invalid/device", urlComplete: "https://mock.invalid/device?code=WDJB-MJHT", code: "WDJB-MJHT", expiresAt: Date.now() + 600_000, email: null, error: null }
                    return send(200, login)
                } else if (route === "GET /dimos/cloud/login") {
                    return send(200, login)
                } else if (route === "DELETE /dimos/cloud/login") {
                    login = { ...login, state: "idle" }
                    return send(200, login)
                } else if (route === "POST /dimos/cloud/logout") {
                    loggedIn = false
                    return send(200, account())
                } else if (route === "GET /dimos/uploads") {
                    return send(200, { uploads: list, waitingForLogin })
                } else if (route === "POST /dimos/uploads") {
                    if (!/\.(mcap|db)$/.test(body.path ?? "")) {
                        return send(400, { error: `not a recording (.mcap or .db): ${body.path}` })
                    }
                    const existing = list.find((u) => u.path === body.path && (u.state === "queued" || u.state === "uploading"))
                    if (existing) {
                        return send(200, existing)
                    }
                    const upload = { id: String(next++), path: body.path, name: body.path.split("/").pop(), size: 48 * 2 ** 20, robotId: null, kind: null, state: "queued", phase: null, bytesDone: 0, bytesTotal: 0, rateBps: null, etaSeconds: null, uploadId: null, skipped: false, notice: null, error: null, errorCode: null, log: null, createdAt: Date.now(), startedAt: null, finishedAt: null }
                    list.push(upload)
                    return send(200, upload)
                } else if (route === "DELETE /dimos/uploads") {
                    list = list.filter((u) => u.state === "queued" || u.state === "uploading")
                    return send(200, { uploads: list })
                } else if (item && req.method === "DELETE") {
                    if (item.state === "queued" || item.state === "uploading") {
                        Object.assign(item, { state: "cancelled", phase: null, rateBps: null, etaSeconds: null, finishedAt: Date.now() })
                    } else {
                        list = list.filter((u) => u !== item)
                    }
                    return send(200, { ok: true })
                } else if (item && route.endsWith("/retry")) {
                    Object.assign(item, { state: "queued", error: null, errorCode: null, log: null, bytesDone: 0, path: item.path.replace("fail", "ok") })
                    list = [...list.filter((u) => u !== item), item]
                    return send(200, item)
                }
                return send(404, { error: `mock: no route ${route}` })
            })
        },
    }
}
