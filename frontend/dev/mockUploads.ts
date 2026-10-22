// DEV ONLY: a tiny in-memory stand-in for Desktop's cloud account + upload queue (/gateways/dimos/cloud/account, /gateways/dimos/cloud/uploads) and
// error feed (/gateways/desktop/ui/errors), so the upload UI can be worked on without Desktop or a Dimensional account. On with
// `MOCK_UPLOADS=1 deno task dev`; never part of the build. Login (Desktop's Settings, not here): logged in 6 s after the
// page first finds it logged out. Uploads: one at a time,
// 4 MB/s; a path containing "fail" fails with a network error. The real endpoints: docs/api.md (the dimos gateway's API 4.0).
import type { Plugin } from "vite"

type Upload = Record<string, any>

export function mockUploads(): Plugin {
    let loggedIn = false
    let loginStarted = 0
    let waitingForLogin = false
    let list: Upload[] = []
    let next = 1
    const account = () => ({ loggedIn, email: loggedIn ? "dev@example.com" : null, scopes: loggedIn ? ["data"] : null, source: loggedIn ? "stored" : null, cloudUrl: "https://mock.invalid", error: null })
    const tick = () => {
        if (!loggedIn && loginStarted && Date.now() - loginStarted > 6000) {
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
                if (!path.startsWith("/gateways/dimos/") && path !== "/gateways/desktop/ui/errors") {
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
                const queue = () => ({ uploads: list, waitingForLogin, byPath: {} })
                if (route === "POST /gateways/desktop/ui/errors") {
                    console.log("[mock] error reported:", body)
                    return send(200, { id: 1, count: 1 })
                } else if (route === "GET /gateways/dimos/cloud/account") {
                    loginStarted ||= Date.now()
                    tick()
                    return send(200, account())
                } else if (route === "GET /gateways/dimos/cloud/uploads") {
                    return send(200, queue())
                } else if (route === "POST /gateways/dimos/cloud/uploads" && body.retry !== undefined) {
                    const item = list.find((u) => u.id === body.retry)
                    if (!item) {
                        return send(404, { error: `no upload ${body.retry}` })
                    }
                    Object.assign(item, { state: "queued", error: null, errorCode: null, log: null, bytesDone: 0, path: item.path.replace("fail", "ok") })
                    list = [...list.filter((u) => u !== item), item]
                    return send(200, item)
                } else if (route === "POST /gateways/dimos/cloud/uploads") {
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
                } else if (route === "DELETE /gateways/dimos/cloud/uploads" && url.searchParams.has("id")) {
                    const item = list.find((u) => u.id === url.searchParams.get("id"))
                    if (!item) {
                        return send(404, { error: `no upload ${url.searchParams.get("id")}` })
                    }
                    if (item.state === "queued" || item.state === "uploading") {
                        Object.assign(item, { state: "cancelled", phase: null, rateBps: null, etaSeconds: null, finishedAt: Date.now() })
                    } else {
                        list = list.filter((u) => u !== item)
                    }
                    return send(200, queue())
                } else if (route === "DELETE /gateways/dimos/cloud/uploads") {
                    list = list.filter((u) => u.state === "queued" || u.state === "uploading")
                    return send(200, queue())
                }
                return send(404, { error: `mock: no route ${route}` })
            })
        },
    }
}
