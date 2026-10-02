// Upper bound for the box tasks: fit_box on each ground-truth box itself (grown 30%), scored the same way.
// What the map lets anyone (agent or person) reach with this voxel map.
const server = Deno.args[0] ?? "http://127.0.0.1:7190"
const tasks = JSON.parse(await Deno.readTextFile("eval/tasks.json"))
const { boxIou } = await import("./iou.ts")
for (const task of tasks.filter((t: any) => t.kind !== "area")) {
    const ious = []
    for (const gt of task.truth) {
        const region = { center: gt.center, size: gt.size.map((v: number) => v * 1.3), yaw: 0 }
        const fit = await (await fetch(`${server}/api/sessions/${task.session}/fit-box`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ box: region }) })).json()
        ious.push(fit.box ? boxIou(fit.box, gt) : 0)
    }
    console.log(task.id, (ious.reduce((a, b) => a + b, 0) / ious.length).toFixed(3))
}
