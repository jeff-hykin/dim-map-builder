// dimos.yaml's `provides:` (an OpenAPI 3.1 document) must be exactly the one the server serves at openapi.json (server/src/api.rs
// builds both the routes and openapi.json from one table), so Desktop sees every endpoint even before the app runs.
// `deno task check-endpoints` (CI runs it on the nix-built binary: `--binary result/bin/dimos-app-server`);
// `--write` rewrites dimos.yaml's provides: from the server's (keeping its x-private:); without it, they must match.
import { parse, stringify } from "@std/yaml"

type OpenApi = { openapi: string; info: Record<string, unknown>; paths: Record<string, Record<string, unknown>> }

const binaryAt = Deno.args.indexOf("--binary")
const command = binaryAt === -1
    ? new Deno.Command("cargo", {
        args: ["run", "-q", "-p", "dimos-app-server", "--", "--openapi-json"],
        stderr: "inherit",
    })
    : new Deno.Command(Deno.args[binaryAt + 1], { args: ["--openapi-json"], stderr: "inherit" })
const output = await command.output()
if (!output.success) {
    console.error("the server couldn't print its endpoints (--openapi-json)")
    Deno.exit(1)
}
const served = JSON.parse(new TextDecoder().decode(output.stdout)) as OpenApi

const file = new URL("../dimos.yaml", import.meta.url)
const text = await Deno.readTextFile(file)
const provides = ((parse(text) as Record<string, unknown>).provides ?? {}) as Partial<OpenApi> & Record<string, unknown>

// keys in a readable order: the ones listed first, the rest after as they come
const ordered = (value: Record<string, unknown>, first: string[]) =>
    Object.fromEntries([
        ...first.filter((k) => k in value).map((k) => [k, value[k]]),
        ...Object.entries(value).filter(([k]) => !first.includes(k)),
    ])
const operation = (op: Record<string, unknown>) => {
    const out = ordered(op, ["summary", "description", "x-when", "x-dimos-role", "parameters", "requestBody"])
    if (Array.isArray(out.parameters)) {
        out.parameters = out.parameters.map((p) => ordered(p, ["name", "in", "required", "schema", "description"]))
    }
    return out
}

if (Deno.args.includes("--write")) {
    // dimos.yaml's path order first, new paths after; x-private: stays as written
    const paths = ordered(served.paths, Object.keys(provides.paths ?? {})) as OpenApi["paths"]
    const privatePaths = provides["x-private"] ?? provides.private
    const want = {
        openapi: served.openapi,
        info: ordered(served.info, ["title", "version", "description", "x-when"]),
        ...(privatePaths ? { "x-private": privatePaths } : {}),
        paths: Object.fromEntries(
            Object.entries(paths).map(([path, item]) => [
                path,
                Object.fromEntries(
                    Object.entries(item).map(([m, op]) => [m, operation(op as Record<string, unknown>)]),
                ),
            ]),
        ),
    }
    // replace only the `provides:` block, so the rest of the file (and its comments) stays as written
    const lines = text.split("\n")
    const block = stringify({ provides: want }, { lineWidth: 116, indent: 4 }).trimEnd().split("\n")
    const start = lines.findIndex((line) => /^provides:/.test(line))
    if (start === -1) {
        lines.splice(lines.length - (lines.at(-1) === "" ? 1 : 0), 0, ...block)
    } else {
        let end = start + 1
        while (end < lines.length && (lines[end] === "" || /^[ #]/.test(lines[end]))) {
            end++
        }
        // trailing blank and comment lines belong to what follows
        while (end - 1 > start && (lines[end - 1].trim() === "" || lines[end - 1].startsWith("#"))) {
            end--
        }
        lines.splice(start, end - start, ...block)
    }
    await Deno.writeTextFile(file, lines.join("\n"))
    console.log(`wrote ${Object.values(served.paths).flatMap(Object.keys).length} operations into dimos.yaml`)
    Deno.exit(0)
}

const operations = (paths: OpenApi["paths"] = {}) =>
    new Set(Object.entries(paths).flatMap(([path, item]) => Object.keys(item).map((m) => `${m.toUpperCase()} ${path}`)))
const have = operations(provides.paths)
const need = operations(served.paths)
const missing = [...need].filter((k) => !have.has(k))
const extra = [...have].filter((k) => !need.has(k))
if (missing.length || extra.length) {
    console.error(
        `dimos.yaml's provides: paths differ from the server's openapi.json:\n  missing: ${
            missing.join(", ") || "-"
        }\n  extra: ${extra.join(", ") || "-"}\nrun: deno task check-endpoints --write`,
    )
    Deno.exit(1)
}
// the table is the source of truth for each operation's when, description and parameters too, not just its method and path
const sorted = (value: unknown): unknown =>
    Array.isArray(value) ? value.map(sorted) : value && typeof value === "object"
        ? Object.fromEntries(
            Object.entries(value).sort(([a], [b]) => a.localeCompare(b)).map(([k, v]) => [k, sorted(v)]),
        )
        : value
const { "x-private": _private, ...declared } = provides
if (JSON.stringify(sorted(declared)) !== JSON.stringify(sorted(served))) {
    console.error(
        "dimos.yaml's provides: info or operation details differ from the server's openapi.json\nrun: deno task check-endpoints --write",
    )
    Deno.exit(1)
}
console.log(`dimos.yaml lists all ${need.size} operations`)
