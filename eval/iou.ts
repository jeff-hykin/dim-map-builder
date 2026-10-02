// IoU of two oriented boxes, by sampling a grid over their joint bounds (shared by run.ts and oracle.ts).
type Box = { center: [number, number, number]; size: [number, number, number]; yaw?: number }
export function boxIou(a: Box, b: Box): number {
    const inside = (box: Box, p: number[]) => {
        const yaw = box.yaw ?? 0
        const dx = p[0] - box.center[0], dy = p[1] - box.center[1]
        const local = [Math.cos(yaw) * dx + Math.sin(yaw) * dy, -Math.sin(yaw) * dx + Math.cos(yaw) * dy, p[2] - box.center[2]]
        return local.every((v, i) => Math.abs(v) <= box.size[i] / 2)
    }
    const radius = (box: Box) => Math.hypot(box.size[0], box.size[1]) / 2
    const lo = [0, 1].map((i) => Math.min(a.center[i] - radius(a), b.center[i] - radius(b))).concat(Math.min(a.center[2] - a.size[2] / 2, b.center[2] - b.size[2] / 2))
    const hi = [0, 1].map((i) => Math.max(a.center[i] + radius(a), b.center[i] + radius(b))).concat(Math.max(a.center[2] + a.size[2] / 2, b.center[2] + b.size[2] / 2))
    const n = 40
    let both = 0, either = 0
    for (let i = 0; i < n; i++) for (let j = 0; j < n; j++) for (let k = 0; k < n; k++) {
        const p = [lo[0] + (hi[0] - lo[0]) * (i + 0.5) / n, lo[1] + (hi[1] - lo[1]) * (j + 0.5) / n, lo[2] + (hi[2] - lo[2]) * (k + 0.5) / n]
        const inA = inside(a, p), inB = inside(b, p)
        if (inA && inB) both++
        if (inA || inB) either++
    }
    return either ? both / either : 0
}

