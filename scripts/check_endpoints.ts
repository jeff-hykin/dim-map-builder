// dimos.yaml's `agent:` must list exactly the endpoints the server serves at agent.json (server/src/api.rs builds both
// the routes and agent.json from one table), so Desktop sees every endpoint even before the app runs.
// `deno task check-endpoints` (CI runs it on the nix-built binary: `--binary result/bin/dimos-app-server`);
// `--write` rewrites dimos.yaml's agent section.
import { parse, stringify } from "@std/yaml"

type Endpoint = { method: string; path: string }

const binaryAt = Deno.args.indexOf("--binary")
const command = binaryAt === -1
    ? new Deno.Command("cargo", {
        args: ["run", "-q", "-p", "dimos-app-server", "--", "--agent-json"],
        stderr: "inherit",
    })
    : new Deno.Command(Deno.args[binaryAt + 1], { args: ["--agent-json"], stderr: "inherit" })
const output = await command.output()
if (!output.success) {
    console.error("the server couldn't print its endpoints (--agent-json)")
    Deno.exit(1)
}
const served = JSON.parse(new TextDecoder().decode(output.stdout)) as {
    description: string
    endpoints: (Endpoint & Record<string, unknown>)[]
}
// method and path first, so dimos.yaml reads well
const want = {
    description: served.description,
    endpoints: served.endpoints.map(({ method, path, description, params, role }) => ({
        method,
        path,
        description,
        ...(params ? { params } : {}),
        ...(role ? { role } : {}),
    })),
}

const file = new URL("../dimos.yaml", import.meta.url)
const yaml = parse(await Deno.readTextFile(file)) as Record<string, unknown>
if (Deno.args.includes("--write")) {
    // replace only the top-level `agent:` block, so the rest of the file (and its comments) stays as written
    const lines = (await Deno.readTextFile(file)).split("\n")
    const start = lines.findIndex((line) => /^agent:/.test(line))
    let end = start + 1
    while (start !== -1 && end < lines.length && !/^[A-Za-z]/.test(lines[end])) {
        end++
    }
    const block = stringify({ agent: want }, { lineWidth: 120, indent: 4 }).trimEnd().split("\n")
    if (start === -1) {
        lines.splice(lines.length - (lines.at(-1) === "" ? 1 : 0), 0, block.join("\n"))
    } else {
        lines.splice(start, end - start, ...block)
    }
    await Deno.writeTextFile(file, lines.join("\n"))
    console.log(`wrote ${want.endpoints.length} endpoints into dimos.yaml`)
    Deno.exit(0)
}
const key = (e: Endpoint) => `${e.method} ${e.path}`
const have = new Set(((yaml.agent as { endpoints?: Endpoint[] })?.endpoints ?? []).map(key))
const need = new Set(want.endpoints.map(key))
const missing = [...need].filter((k) => !have.has(k))
const extra = [...have].filter((k) => !need.has(k))
if (missing.length || extra.length) {
    console.error(
        `dimos.yaml's agent endpoints differ from the server's agent.json:\n  missing: ${
            missing.join(", ") || "-"
        }\n  extra: ${extra.join(", ") || "-"}\nrun: deno task check-endpoints --write`,
    )
    Deno.exit(1)
}
console.log(`dimos.yaml lists all ${need.size} endpoints`)
