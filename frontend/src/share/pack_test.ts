// deno test frontend/src/share: the shared map's voxel packing and page round trips.
import { deepStrictEqual, ok, strictEqual, throws } from "node:assert"
import { cropToSlice, packVoxels, readShareHtml, shareHtml, type SharedMap, unpackVoxels } from "./pack.ts"

/** voxel centers on a `step` grid, in random order: a few walls, a floor and some far specks */
function gridVoxels(step: number, seed = 7): Float32Array {
    let state = seed
    const random = () => (state = (state * 1103515245 + 12345) % 2147483648) / 2147483648
    const cells = new Set<string>()
    for (let x = 0; x < 120; x++) {
        for (let y = 0; y < 80; y++) {
            cells.add(`${x},${y},0`)
        }
        for (let z = 1; z < 40; z++) {
            cells.add(`${x},0,${z}`)
            cells.add(`${x},79,${z}`)
        }
    }
    for (let index = 0; index < 500; index++) {
        cells.add(`${Math.floor(random() * 4000) - 2000},${Math.floor(random() * 4000)},${Math.floor(random() * 300) - 100}`)
    }
    const keys = [...cells].sort(() => random() - 0.5)
    const positions = new Float32Array(keys.length * 3)
    keys.forEach((key, index) => key.split(",").forEach((value, axis) => (positions[index * 3 + axis] = (Number(value) + 0.5) * step - 3.2)))
    return positions
}

/** the points as sorted "x,y,z" strings rounded to `digits` */
function asSet(positions: Float32Array, digits = 4): string[] {
    const out: string[] = []
    for (let index = 0; index < positions.length; index += 3) {
        out.push([0, 1, 2].map((axis) => positions[index + axis].toFixed(digits)).join(","))
    }
    return out.sort()
}

Deno.test("voxels on the grid round-trip exactly (as a set), at about a byte each or less", async () => {
    const step = 0.05
    const positions = gridVoxels(step)
    const packed = await packVoxels(positions, step)
    const { positions: back, step: stepBack } = await unpackVoxels(packed)
    strictEqual(stepBack, step)
    strictEqual(back.length, positions.length)
    deepStrictEqual(asSet(back), asSet(positions))
    const count = positions.length / 3
    ok(packed.length < count, `${packed.length} bytes for ${count} voxels`)
})

Deno.test("off-grid points come back within half a step", async () => {
    const positions = new Float32Array([0.013, -4.2, 1.777, 10.5, 3.3, -0.02, 0.06, -4.21, 1.8])
    const step = 0.1
    const { positions: back } = await unpackVoxels(await packVoxels(positions, step))
    const sorted = (values: Float32Array) => asSet(values, 1).length
    strictEqual(sorted(back), 3)
    for (let index = 0; index < positions.length; index += 3) {
        const near = [0, 3, 6].some((other) => [0, 1, 2].every((axis) => Math.abs(back[other + axis] - positions[index + axis]) <= step / 2 + 1e-5))
        ok(near, `point ${index / 3} has no match within ${step / 2}`)
    }
})

Deno.test("no voxels and one voxel", async () => {
    strictEqual((await unpackVoxels(await packVoxels(new Float32Array(0), 0.05))).positions.length, 0)
    const one = await unpackVoxels(await packVoxels(new Float32Array([1.025, 2.075, -0.475]), 0.05))
    deepStrictEqual(asSet(one.positions), ["1.0250,2.0750,-0.4750"])
})

Deno.test("a damaged pack is refused", async () => {
    const packed = await packVoxels(gridVoxels(0.05), 0.05)
    await unpackVoxels(packed.slice(0, packed.length - 40)).then(
        () => ok(false, "a truncated pack was read"),
        () => {},
    )
})

Deno.test("the crop keeps what the slice shows: its z band and its turned x, y box", () => {
    const positions = new Float32Array([
        1, 0, 0.5, // turned 90°: (0, 1), inside
        0, 1, 0.5, // turned: (-1, 0), outside x
        1, 0, 3, // above the band
        0.5, 0.25, 0.0, // turned: (-0.25, 0.5), inside, on the band's bottom
    ])
    const slice = { zMin: 0, zMax: 2, yaw: Math.PI / 2, xMin: -0.5, xMax: 0.5, yMin: -2, yMax: 2 }
    deepStrictEqual([...cropToSlice(positions, slice)], [1, 0, 0.5, 0.5, 0.25, 0])
    strictEqual(cropToSlice(positions, null), positions)
})

Deno.test("the page holds the meta, voxels and viewer, and reads back", async () => {
    const meta: SharedMap = {
        name: "office </script> <b>& co</b>",
        exported: "2026-10-06T21:43:00.000Z",
        voxelSize: 0.05,
        count: 1,
        look: { style: "voxel", gradient: "memworld", scale: 1, shade: "soft" },
        range: [0, 2],
        slice: null,
        floors: [{ level: 0, band: [-0.3, 2.4] }, { level: 3, band: [2.4, 5.6] }],
        floor: null,
        camera: { position: [1, 2, 3], target: [0, 0, 0] },
    }
    const voxels = await packVoxels(new Float32Array([1.025, 2.075, -0.475]), 0.05)
    const code = new TextEncoder().encode("console.log('</script>')")
    const html = shareHtml(meta, voxels, code)
    strictEqual(html.match(/<\/script>/g)?.length, 4)
    ok(html.includes("<title>office &lt;/script&gt; &lt;b&gt;&amp; co&lt;/b&gt; · map</title>"))
    const back = readShareHtml(html)
    deepStrictEqual(back.meta, meta)
    deepStrictEqual(back.voxels, voxels)
    deepStrictEqual(back.code, code)
    throws(() => readShareHtml("<html></html>"))
})
