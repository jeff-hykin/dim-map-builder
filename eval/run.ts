// Agent evaluation of the Map Builder's agent endpoints: real agents (`claude -p`, given ONLY Desktop's MCP server,
// which reaches the Map Builder through its agent.json endpoints) do boxing / area tasks on real maps; each result is
// scored against a hand-placed ground truth. The app runs inside Desktop, so --server is its URL under Desktop.
//
//   deno run -A eval/run.ts --server http://127.0.0.1:7077/apps/dim-map-builder --tasks eval/tasks.json --out eval/results.json
//
// (results-basic.json / results-full*.json are from before the move to Desktop's MCP, when this app served its own
// /mcp with a `basic` and a `full` toolset.)
//
// For each task: the session is opened (the page must be open on the server so `get_view` screenshots work), the
// camera put where the task says, the agent runs, the annotations it added are scored (3D IoU for boxes, 2D IoU for
// areas, best match per ground-truth box), then removed so the next task starts clean.
import { parseArgs } from "jsr:@std/cli@1/parse-args"

const args = parseArgs(Deno.args, { string: ["server", "tasks", "out", "model", "only"], default: { model: "" } })
const server = args.server ?? "http://127.0.0.1:7077/apps/dim-map-builder"
const desktop = new URL(server).origin
const tasks: Task[] = JSON.parse(await Deno.readTextFile(args.tasks ?? "eval/tasks.json"))

type Box = { center: [number, number, number]; size: [number, number, number]; yaw?: number }
interface Task {
    id: string
    session: string
    prompt: string
    view?: { target: [number, number, number]; distance?: number; topDown?: boolean }
    /** what's scored: boxes (each ground truth matched to its best new box) or an area polygon */
    kind: "box" | "boxes" | "area"
    truth: Box[] | [number, number][]
    floor?: number
}

async function api(path: string, init?: RequestInit) {
    const response = await fetch(`${server}${path}`, { headers: { "content-type": "application/json" }, ...init })
    return response.json()
}

import { boxIou } from "./iou.ts"

function polygonIou(a: [number, number][], b: [number, number][]): number {
    const contains = (poly: [number, number][], x: number, y: number) => {
        let inside = false
        for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
            const [xi, yi] = poly[i], [xj, yj] = poly[j]
            if ((yi > y) !== (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi) inside = !inside
        }
        return inside
    }
    const all = [...a, ...b]
    const [x0, x1] = [Math.min(...all.map((p) => p[0])), Math.max(...all.map((p) => p[0]))]
    const [y0, y1] = [Math.min(...all.map((p) => p[1])), Math.max(...all.map((p) => p[1]))]
    const n = 200
    let both = 0, either = 0
    for (let i = 0; i < n; i++) for (let j = 0; j < n; j++) {
        const x = x0 + (x1 - x0) * (i + 0.5) / n, y = y0 + (y1 - y0) * (j + 0.5) / n
        const inA = contains(a, x, y), inB = contains(b, x, y)
        if (inA && inB) both++
        if (inA || inB) either++
    }
    return either ? both / either : 0
}

const config = await Deno.makeTempFile({ suffix: ".json" })
await Deno.writeTextFile(config, JSON.stringify({ mcpServers: { desktop: { type: "http", url: `${desktop}/mcp` } } }))

const results = []
for (const task of tasks.filter((t) => !args.only || args.only.split(",").includes(t.id))) {
    const session = await api(`/api/sessions/${task.session}`)
    // make it the active session (the page follows the open session)
    await api("/api/open", { method: "POST", body: JSON.stringify({ id: session.recordingId, path: session.recordingPath, name: session.name }) })
    await new Promise((r) => setTimeout(r, 2500))
    if (task.view) {
        // ask the page to look there
        await fetch(`${server}/api/camera`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(task.view) })
        await new Promise((r) => setTimeout(r, 1500))
    }
    const before = await api(`/api/sessions/${task.session}`)
    const known = new Set([...before.annotations.boxes, ...before.annotations.areas].map((a: { id: string }) => a.id))
    const started = performance.now()
    const command = new Deno.Command("claude", {
        args: [
            "-p", `${task.prompt}\n\n(You are working in the Map Builder through its tools only. When done, reply with one line saying what you added.)`,
            "--mcp-config", config, "--strict-mcp-config",
            "--allowedTools", "mcp__desktop",
            "--disallowedTools", "Bash,Read,Write,Edit,Glob,Grep,WebFetch,WebSearch,Task,NotebookEdit,TodoWrite",
            "--output-format", "stream-json", "--verbose",
            ...(args.model ? ["--model", args.model] : []),
        ],
        stdout: "piped",
        stderr: "piped",
        env: { CBG_PASSTHROUGH: "1" },
    })
    const output = await command.output()
    const seconds = (performance.now() - started) / 1000
    const lines = new TextDecoder().decode(output.stdout).split("\n").filter(Boolean).map((line) => { try { return JSON.parse(line) } catch { return null } }).filter(Boolean)
    const calls: string[] = []
    let final = ""
    let failedCalls = 0
    for (const event of lines) {
        for (const block of event.message?.content ?? []) {
            // call_endpoint's target is what counts (map builder actions are `dim-map-builder:POST api/sessions/{id}/fit-box`...)
            if (block.type === "tool_use") calls.push(String(block.input?.id ?? block.name).replace(/^mcp__desktop__/, "").replace(/^dim-map-builder:/, ""))
            if (block.type === "tool_result" && block.is_error) failedCalls++
        }
        if (event.type === "result") final = event.result ?? ""
    }
    const after = await api(`/api/sessions/${task.session}`)
    const newBoxes = after.annotations.boxes.filter((b: { id: string }) => !known.has(b.id))
    const newAreas = after.annotations.areas.filter((a: { id: string }) => !known.has(a.id))
    let score: Record<string, unknown> = {}
    if (task.kind === "area") {
        const truth = task.truth as [number, number][]
        const best = Math.max(0, ...newAreas.map((a: { polygon: [number, number][] }) => polygonIou(a.polygon, truth)))
        score = { iou: +best.toFixed(3), added: newAreas.length, kinds: newAreas.map((a: { kind: string }) => a.kind) }
    } else {
        const truth = task.truth as Box[]
        const ious = truth.map((gt) => Math.max(0, ...newBoxes.map((b: { box: Box }) => boxIou(b.box, gt))))
        score = {
            meanIou: +(ious.reduce((s, v) => s + v, 0) / ious.length).toFixed(3),
            found: ious.filter((v) => v >= 0.3).length,
            of: truth.length,
            added: newBoxes.length,
            ious: ious.map((v) => +v.toFixed(2)),
        }
    }
    const result = { task: task.id, seconds: Math.round(seconds), toolCalls: calls.length, failedCalls, calls, ...score, answer: final.slice(0, 300), exit: output.code }
    console.log(JSON.stringify(result))
    results.push(result)
    // clean up what the agent added (boxes, areas, anything else new)
    for (const annotation of [...after.annotations.boxes, ...after.annotations.planes, ...after.annotations.points, ...after.annotations.areas, ...after.annotations.planPoints]) {
        if (!known.has(annotation.id) && ![...before.annotations.planes, ...before.annotations.points, ...before.annotations.planPoints].some((a: { id: string }) => a.id === annotation.id)) {
            await fetch(`${server}/api/sessions/${task.session}/annotations/${annotation.id}`, { method: "DELETE" })
        }
    }
    // undo any map edits the agent made (cleanups), back to the starting revision's voxel count
    for (let guard = 0; guard < 20; guard++) {
        const now = await api(`/api/sessions/${task.session}`)
        if (now.voxels === before.voxels) break
        await fetch(`${server}/api/sessions/${task.session}/undo`, { method: "POST" })
    }
}
if (args.out) {
    await Deno.writeTextFile(args.out, JSON.stringify(results, null, 2))
}
